//! `ods serve`: the explorer over HTTP, with a read-only JSON API and live reload.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use ods_core::ColumnRef;
use ods_lineage::export::GraphNode;
use ods_lineage::{Change, ColumnChangeKind, ColumnGraph, GraphDocument};
use serde::{Deserialize, Serialize};

use crate::dashboard::{Dashboard, ShellView};
use crate::search::search;

mod catalog_routes;

/// Everything the server shows, rebuilt by the [`Loader`] when artifacts change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Snapshot {
    /// The graph the page renders.
    pub document: GraphDocument,
    /// The analyzed graph, for impact queries.
    pub graph: ColumnGraph,
    /// Where it came from, e.g. the target directory.
    pub source: String,
    /// The project, its state and plan, for the dashboard. Without one, Home says
    /// nothing is known about the project's state.
    pub dashboard: Option<Dashboard>,
    /// Node ids to names from the graph, built on first use and kept until the next
    /// reload.
    names: OnceLock<Arc<BTreeMap<String, String>>>,
}

impl Snapshot {
    /// A snapshot.
    pub fn new(document: GraphDocument, graph: ColumnGraph, source: impl Into<String>) -> Self {
        Self {
            document,
            graph,
            source: source.into(),
            dashboard: None,
            names: OnceLock::new(),
        }
    }

    /// Adds what the dashboard shows.
    #[must_use]
    pub fn with_dashboard(mut self, dashboard: Dashboard) -> Self {
        self.dashboard = Some(dashboard);
        self
    }

    /// Node ids to names, from the graph (it names sources too).
    pub(crate) fn names(&self) -> Arc<BTreeMap<String, String>> {
        Arc::clone(self.names.get_or_init(|| {
            Arc::new(
                self.document
                    .nodes
                    .iter()
                    .map(|n| (n.id.clone(), n.name.clone()))
                    .collect(),
            )
        }))
    }

    /// The dashboard's facts, or, without any, a project named after nothing with no
    /// state store.
    pub(crate) fn dashboard(&self) -> Dashboard {
        self.dashboard
            .clone()
            .unwrap_or_else(|| Dashboard::new("project", "default"))
    }
}

/// Produces a fresh [`Snapshot`]. Supplied by the binary, which owns the providers.
pub type Loader = Arc<dyn Fn() -> Result<Snapshot, String> + Send + Sync>;

/// How to serve.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ServeOptions {
    /// Address to bind. Defaults to loopback: exposing it is an explicit choice.
    pub addr: SocketAddr,
    /// Files whose change triggers a reload, and how often to check.
    pub watch: Option<(Vec<PathBuf>, Duration)>,
    base_path: String,
    allowed_hosts: Vec<String>,
    streams: crate::live::StreamLimits,
}

impl ServeOptions {
    /// Options for `addr`, no base path, no watching.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            watch: None,
            base_path: String::new(),
            allowed_hosts: Vec::new(),
            streams: crate::live::StreamLimits::default(),
        }
    }

    /// How the live run streams behave (#322): how many may be open at once, how often
    /// a journal is checked, and the heartbeat. The defaults suit a local server.
    #[must_use]
    pub fn with_stream_limits(mut self, limits: crate::live::StreamLimits) -> Self {
        self.streams = limits;
        self
    }

    /// Serves under a URL prefix such as `/ods` or `/tools/ods`.
    ///
    /// # Errors
    /// Returns [`WebError::BasePath`] unless every segment is made of letters, digits,
    /// `-`, `.`, `_` or `~` (no `..`).
    pub fn with_base_path(mut self, base_path: &str) -> Result<Self, WebError> {
        self.base_path = normalize_base(base_path)?;
        Ok(self)
    }

    /// The normalized URL prefix: empty, or `/a/b` without a trailing slash.
    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    /// Also accepts these `Host` names (without port), e.g. the name a reverse proxy
    /// forwards. On loopback, only `localhost`, `127.0.0.1` and `[::1]` are accepted
    /// otherwise, which stops DNS rebinding.
    #[must_use]
    pub fn with_allowed_hosts(mut self, hosts: impl IntoIterator<Item = String>) -> Self {
        self.allowed_hosts
            .extend(hosts.into_iter().map(|h| h.to_ascii_lowercase()));
        self
    }

    /// Reloads when any of `paths` changes, checking every `every`.
    #[must_use]
    pub fn with_watch(mut self, paths: Vec<PathBuf>, every: Duration) -> Self {
        self.watch = Some((paths, every));
        self
    }

    /// `None` accepts any `Host`: bound beyond loopback with no allow-list, which the
    /// caller has been warned about.
    /// Whether pages may show local paths, configuration values and error text: only
    /// when every request comes from this machine, that is, bound to loopback with no
    /// other `Host` allowed. A name allowed with `--allow-host` is a proxy's, forwarding
    /// requests from elsewhere.
    fn details(&self) -> bool {
        self.addr.ip().is_loopback() && self.allowed_hosts.is_empty()
    }

    fn host_policy(&self) -> Option<Vec<String>> {
        let loopback = self.addr.ip().is_loopback();
        if !loopback && self.allowed_hosts.is_empty() {
            return None;
        }
        let mut hosts = self.allowed_hosts.clone();
        if loopback {
            hosts.extend(["localhost", "127.0.0.1", "[::1]"].map(str::to_owned));
        }
        Some(hosts)
    }
}

/// `lineage/`, `/lineage` → `/lineage`; `/` or empty → empty. Only plain path segments
/// are accepted: anything else would be route syntax, need escaping, or escape the
/// prefix.
fn normalize_base(path: &str) -> Result<String, WebError> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let valid = trimmed.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'))
    });
    if valid {
        Ok(format!("/{trimmed}"))
    } else {
        Err(WebError::BasePath(path.to_owned()))
    }
}

/// Why serving failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WebError {
    /// The first snapshot couldn't be built.
    #[error("{0}")]
    Load(String),
    /// The URL prefix isn't a plain path.
    #[error(
        "invalid base path `{0}`: use segments of letters, digits, `-`, `.`, `_` or `~`, e.g. /ods"
    )]
    BasePath(String),
    /// Binding or serving failed.
    #[error("cannot serve on {addr}: {source}")]
    Io {
        /// The address.
        addr: SocketAddr,
        /// The error.
        source: std::io::Error,
    },
}

pub(crate) struct AppState {
    snapshot: RwLock<Arc<Snapshot>>,
    pub(crate) generation: AtomicU64,
    last_error: Mutex<Option<String>>,
    /// Whether pages may show local paths, values and error text: on loopback with no
    /// other host allowed ([`ServeOptions::details`]); else those go to the server log.
    pub(crate) details: bool,
    /// The live run streams open now, and their limits (#322).
    pub(crate) streams: crate::live::Streams,
}

impl AppState {
    pub(crate) fn current(&self) -> Arc<Snapshot> {
        self.snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

pub(crate) type Shared = Arc<AppState>;

/// The router, for embedding or tests. `snapshot` is served until replaced by reloads.
/// `options` supply the base path and `Host` policy.
pub fn router(snapshot: Snapshot, options: &ServeOptions) -> Router {
    router_with_state(new_state(snapshot, options), options)
}

fn new_state(snapshot: Snapshot, options: &ServeOptions) -> Shared {
    Arc::new(AppState {
        snapshot: RwLock::new(Arc::new(snapshot)),
        generation: AtomicU64::new(1),
        last_error: Mutex::new(None),
        details: options.details(),
        streams: crate::live::Streams::new(options.streams),
    })
}

fn router_with_state(state: Shared, options: &ServeOptions) -> Router {
    let base_path = options.base_path.as_str();
    // Routes are prefixed rather than nested: the page resolves `api/...` against its
    // own URL, so it must be served at `<base>/` (with the slash), and `<base>` alone
    // redirects there.
    let at = |path: &str| format!("{base_path}{path}");
    let mut app = Router::new()
        .route(&at("/"), get(home))
        .route(&at("/index.html"), get(home))
        // Relative to `lineage`, the explorer's `api/...` resolves to `<base>/api/...`.
        .route(&at("/lineage"), get(explorer))
        // The Impact simulator (#347).
        .route(&at("/lineage/impact"), get(catalog_routes::impact_page))
        .route(&at("/api/lineage/impact"), get(catalog_routes::impact_api))
        // The ERD page (#64).
        .route(&at("/erd"), get(catalog_routes::erd_page))
        .route(&at("/api/erd"), get(catalog_routes::erd_api))
        // The Lineage page's State overlay (#312).
        .route(&at("/api/lineage/overlay"), get(lineage_overlay))
        .route(&at("/healthz"), get(|| async { "ok" }))
        .route(&at("/api/version"), get(version))
        .route(&at("/api/shell"), get(shell))
        .route(&at("/api/home"), get(home_api))
        .route(&at("/assets/fonts/{file}"), get(font))
        .route(&at("/api/graph"), get(graph))
        .route(&at("/api/search"), get(search_handler))
        .route(&at("/api/node"), get(node))
        .route(&at("/api/impact"), get(impact))
        // The live run view (#322): runs going on now, and a run's events as written.
        .route(&at("/api/runs/live"), get(crate::live::live))
        .route(&at("/api/runs/{run}/events"), get(crate::live::events))
        // The Catalog and the model pages (#313).
        .route(&at("/catalog"), get(catalog_routes::page))
        // Before `/catalog/{id}`: no node id is `sources`, but the route is fixed.
        .route(&at("/catalog/sources"), get(catalog_routes::sources_page))
        .route(
            &at("/api/catalog/sources"),
            get(catalog_routes::sources_api),
        )
        // The Semantic layer (#352), read-only. No node id is `semantic` either.
        .route(&at("/catalog/semantic"), get(catalog_routes::semantic_page))
        .route(
            &at("/api/catalog/semantic"),
            get(catalog_routes::semantic_api),
        )
        .route(&at("/catalog/{id}"), get(catalog_routes::model))
        .route(&at("/catalog/"), {
            let to = at("/catalog");
            get(move || async move { Redirect::permanent(&to) })
        })
        // The About page (ADR-0031 §3c): this `ods` and its plugins.
        .route(&at("/settings/about"), get(about_page))
        .route(&at("/api/settings/about"), get(about_api))
        // Settings (#351): the configuration, read-only.
        .route(&at("/settings"), get(settings_page))
        .route(&at("/api/settings"), get(settings_api))
        .route(&at("/settings/"), {
            let to = at("/settings");
            get(move || async move { Redirect::permanent(&to) })
        })
        .route(&at("/api/catalog"), get(catalog_routes::api))
        .route(&at("/api/catalog/{id}"), get(catalog_routes::model_api));
    // State pages (#311): the plan and its Why panel, the runs, one run.
    app = crate::state_pages::routes(app, &at);
    if !base_path.is_empty() {
        let target = at("/");
        app = app.route(
            base_path,
            get(move || async move { Redirect::permanent(&target) }),
        );
    }
    let hosts = Arc::new(options.host_policy());
    app.with_state(state)
        .layer(axum::middleware::from_fn(move |request, next| {
            check_host(hosts.clone(), request, next)
        }))
        .layer(axum::middleware::map_response(security_headers))
}

/// Refuses requests whose `Host` isn't allowed. On loopback this stops DNS rebinding:
/// a page on `evil.example` that re-resolves to 127.0.0.1 still sends `Host:
/// evil.example`.
async fn check_host(allowed: Arc<Option<Vec<String>>>, request: Request, next: Next) -> Response {
    let Some(allowed) = allowed.as_ref() else {
        return next.run(request).await;
    };
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(host_name);
    match host {
        Some(host) if allowed.contains(&host) => next.run(request).await,
        _ => (
            StatusCode::MISDIRECTED_REQUEST,
            "host not allowed; see `ods serve --allow-host`",
        )
            .into_response(),
    }
}

/// `Host` without the port, lowercased: `LocalHost:8765` → `localhost`, `[::1]:80` →
/// `[::1]`.
fn host_name(host: &str) -> String {
    let host = host.to_ascii_lowercase();
    let end = if host.starts_with('[') {
        host.find(']').map_or(host.len(), |i| i + 1)
    } else {
        host.rfind(':').unwrap_or(host.len())
    };
    host[..end].to_owned()
}

/// A strict policy: the page needs its own inline script and style, and talks only to
/// this server.
async fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    for (name, value) in [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
             img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; \
             base-uri 'none'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    // Data changes with every reload, so nothing is cached, unless a route says
    // otherwise (the vendored fonts).
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}

/// Home: the dashboard's first page. Built on a blocking thread, as it may plan.
async fn home(State(state): State<Shared>) -> Response {
    tokio::task::spawn_blocking(move || {
        let generation = state.generation.load(Ordering::SeqCst);
        let dashboard = state.current().dashboard();
        Html(crate::home::home_page(
            &dashboard.shell("home"),
            &dashboard.home(state.details),
            &dashboard.live_runs(SystemTime::now()),
            generation,
        ))
        .into_response()
    })
    .await
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `/settings`: the configuration, read-only (#351).
async fn settings_page(State(state): State<Shared>) -> Html<String> {
    let generation = state.generation.load(Ordering::SeqCst);
    let snapshot = state.current();
    let dashboard = snapshot.dashboard();
    Html(crate::settings_page::settings_page(
        &dashboard.shell("settings"),
        &dashboard.settings(state.details),
        generation,
    ))
}

/// `/api/settings`: the Settings page's view model.
async fn settings_api(State(state): State<Shared>) -> Json<crate::settings::SettingsView> {
    Json(state.current().dashboard().settings(state.details))
}

/// `/settings/about`: this `ods` and the plugins it runs with (ADR-0031 §3c).
async fn about_page(State(state): State<Shared>) -> Html<String> {
    let generation = state.generation.load(Ordering::SeqCst);
    let snapshot = state.current();
    let dashboard = snapshot.dashboard();
    Html(crate::about_page::about_page(
        &dashboard.shell("settings"),
        &dashboard.about(),
        generation,
    ))
}

/// `/api/settings/about`: the About page's view model.
async fn about_api(State(state): State<Shared>) -> Json<crate::about::AboutView> {
    Json(state.current().dashboard().about())
}

async fn shell(State(state): State<Shared>) -> Json<ShellView> {
    Json(state.current().dashboard().shell("home"))
}

async fn home_api(State(state): State<Shared>) -> Response {
    tokio::task::spawn_blocking(move || Json(state.current().dashboard().home(state.details)))
        .await
        .map_or_else(
            |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            IntoResponse::into_response,
        )
}

/// The dashboard's vendored fonts; nothing else is served from disk or memory by name.
async fn font(Path(file): Path<String>) -> Response {
    match crate::fonts::font(&file) {
        // The files never change within a build of ODS, so browsers may keep them.
        Some(font) => (
            [
                (header::CONTENT_TYPE, "font/woff2"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            font.bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct LineageQuery {
    /// A node to select, by id (or unique name): `/lineage?node=<id>`.
    node: Option<String>,
}

/// The Lineage page (#312): the explorer in the dashboard's shell, with the State
/// overlay from the plan every page shares. Built on a blocking thread, as it may plan.
async fn explorer(State(state): State<Shared>, Query(query): Query<LineageQuery>) -> Response {
    crate::state_pages::blocking(move || {
        // The first paint is embedded, with its generation so the page notices any
        // reload after it.
        let generation = state.generation.load(Ordering::SeqCst);
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let overlay = dashboard.lineage_overlay(&snapshot.document, state.details);
        match crate::lineage::lineage_page(
            &dashboard.shell("lineage"),
            &snapshot.document,
            &overlay,
            query.node.as_deref(),
            generation,
        ) {
            Ok(html) => Html(html).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    })
    .await
}

/// `/api/lineage/overlay`: the plan's decision for each node, as the page colours it.
async fn lineage_overlay(State(state): State<Shared>) -> Response {
    crate::state_pages::blocking(move || {
        let snapshot = state.current();
        Json(
            snapshot
                .dashboard()
                .lineage_overlay(&snapshot.document, state.details),
        )
        .into_response()
    })
    .await
}

#[derive(Serialize)]
struct Version {
    api: u32,
    generation: u64,
    source: Option<String>,
    last_error: Option<String>,
}

async fn version(State(state): State<Shared>) -> Json<Version> {
    let last_error = state
        .last_error
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    Json(Version {
        api: crate::API_VERSION,
        generation: state.generation.load(Ordering::SeqCst),
        source: state.details.then(|| state.current().source.clone()),
        last_error: last_error.map(|e| {
            if state.details {
                e
            } else {
                "reload failed; see the server log".to_owned()
            }
        }),
    })
}

async fn graph(State(state): State<Shared>) -> Json<GraphDocument> {
    Json(state.current().document.clone())
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

async fn search_handler(
    State(state): State<Shared>,
    Query(query): Query<SearchQuery>,
) -> Json<Vec<crate::SearchHit>> {
    Json(search(
        &state.current().document,
        &query.q,
        query.limit.min(200),
    ))
}

#[derive(Deserialize)]
struct NodeQuery {
    id: String,
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// A node by id or unique name.
fn find_node<'a>(snapshot: &'a Snapshot, id: &str) -> Result<&'a GraphNode, (StatusCode, String)> {
    if let Some(node) = snapshot.document.nodes.iter().find(|n| n.id == id) {
        return Ok(node);
    }
    let named: Vec<&GraphNode> = snapshot
        .document
        .nodes
        .iter()
        .filter(|n| n.name == id)
        .collect();
    match named.as_slice() {
        [node] => Ok(node),
        [] => Err((StatusCode::NOT_FOUND, format!("no node `{id}`"))),
        several => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "`{id}` is ambiguous: {}; use the id",
                several
                    .iter()
                    .map(|n| n.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

async fn node(State(state): State<Shared>, Query(query): Query<NodeQuery>) -> Response {
    let snapshot = state.current();
    let node = match find_node(&snapshot, &query.id) {
        Ok(node) => node,
        Err((status, message)) => return error(status, &message),
    };
    let lineage = snapshot
        .graph
        .node(&node.id)
        .and_then(|n| n.lineage.clone());
    Json(serde_json::json!({ "node": node, "lineage": lineage })).into_response()
}

#[derive(Deserialize)]
struct ImpactQuery {
    /// Node id or name.
    node: String,
    /// A column of it; omitted means "its rows may change".
    column: Option<String>,
    /// `modified` (default), `added` or `removed`.
    kind: Option<String>,
}

async fn impact(State(state): State<Shared>, Query(query): Query<ImpactQuery>) -> Response {
    let snapshot = state.current();
    let node = match find_node(&snapshot, &query.node) {
        Ok(node) => node,
        Err((status, message)) => return error(status, &message),
    };
    let Some(lineage_node) = snapshot.graph.node(&node.id) else {
        return error(StatusCode::NOT_FOUND, &format!("no node `{}`", query.node));
    };
    let relation = lineage_node.relation.clone();
    let change = match &query.column {
        None => Change::Rows { relation },
        Some(column) => {
            let kind = match query.kind.as_deref().unwrap_or("modified") {
                "modified" => ColumnChangeKind::Modified,
                "added" => ColumnChangeKind::Added,
                "removed" => ColumnChangeKind::Removed,
                other => {
                    return error(StatusCode::BAD_REQUEST, &format!("unknown kind `{other}`"));
                }
            };
            // A modified or removed column must exist: an unknown name would "affect
            // nothing" and report every reader as safe to skip (AGENTS.md rule 3).
            let column = if kind == ColumnChangeKind::Added {
                column.clone()
            } else {
                match known_column(&lineage_node.columns, column) {
                    Ok(column) => column,
                    Err(message) => return error(StatusCode::BAD_REQUEST, &message),
                }
            };
            Change::Column {
                column: ColumnRef::new(relation, column),
                kind,
            }
        }
    };
    let impact = snapshot.graph.impact(std::slice::from_ref(&change));
    Json(serde_json::json!({ "change": change, "impact": impact })).into_response()
}

/// `wanted` as the node spells it: exact, else the only case-insensitive match.
fn known_column(columns: &[String], wanted: &str) -> Result<String, String> {
    if columns.iter().any(|c| c == wanted) {
        return Ok(wanted.to_owned());
    }
    let folded: Vec<&String> = columns
        .iter()
        .filter(|c| c.eq_ignore_ascii_case(wanted))
        .collect();
    match folded.as_slice() {
        [column] => Ok((*column).clone()),
        _ if columns.is_empty() => Err(format!(
            "the columns of this node are unknown, so `{wanted}` can't be checked; \
             omit `column` to analyze a change to its rows"
        )),
        _ => Err(format!("no column `{wanted}`")),
    }
}

/// Per watched file: modification time and length, or `None` if missing.
fn signature(paths: &[PathBuf]) -> Vec<Option<(SystemTime, u64)>> {
    paths
        .iter()
        .map(|p| {
            p.metadata()
                .ok()
                .and_then(|m| Some((m.modified().ok()?, m.len())))
        })
        .collect()
}

async fn watch(state: Shared, loader: Loader, paths: Vec<PathBuf>, every: Duration) {
    let mut seen = signature(&paths);
    // A change is loaded only once it has held for a whole tick, so a build that is
    // still writing files isn't read half-way.
    let mut pending = None;
    let mut ticker = tokio::time::interval(every);
    loop {
        ticker.tick().await;
        let now = signature(&paths);
        if now == seen {
            pending = None;
            continue;
        }
        if pending.as_ref() != Some(&now) {
            pending = Some(now);
            continue;
        }
        seen = now;
        pending = None;
        let loader = loader.clone();
        let error = match tokio::task::spawn_blocking(move || loader()).await {
            Ok(Ok(snapshot)) => {
                *state
                    .snapshot
                    .write()
                    .unwrap_or_else(PoisonError::into_inner) = Arc::new(snapshot);
                state.generation.fetch_add(1, Ordering::SeqCst);
                tracing::info!("reloaded lineage");
                None
            }
            // Keep serving the last good snapshot and say why it's stale.
            Ok(Err(error)) => Some(error),
            Err(error) => Some(error.to_string()),
        };
        if let Some(error) = &error {
            tracing::warn!(%error, "reload failed; serving the last good lineage");
        }
        // Held only for the assignment: never across I/O such as logging.
        *state
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = error;
    }
}

/// Resolves on Ctrl-C, or SIGTERM where there is one (containers stop with it).
async fn shutdown() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            () = interrupt => {}
            () = terminate => {}
        }
    }
    #[cfg(not(unix))]
    interrupt.await;
}

/// Serves until Ctrl-C or SIGTERM. `ready` is called with the bound address once
/// listening.
///
/// # Errors
/// Returns [`WebError`] if the first snapshot fails or the address can't be bound.
pub async fn serve(
    options: ServeOptions,
    loader: Loader,
    ready: impl FnOnce(SocketAddr),
) -> Result<(), WebError> {
    let first = loader.clone();
    let snapshot = tokio::task::spawn_blocking(move || first())
        .await
        .map_err(|e| WebError::Load(e.to_string()))?
        .map_err(WebError::Load)?;
    let state = new_state(snapshot, &options);
    let app = router_with_state(state.clone(), &options);
    let listener = tokio::net::TcpListener::bind(options.addr)
        .await
        .map_err(|source| WebError::Io {
            addr: options.addr,
            source,
        })?;
    let addr = listener.local_addr().map_err(|source| WebError::Io {
        addr: options.addr,
        source,
    })?;
    if let Some((paths, every)) = options.watch.clone() {
        tokio::spawn(watch(state, loader, paths, every));
    }
    ready(addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|source| WebError::Io { addr, source })
}

/// [`serve`] on a new multi-threaded runtime, for synchronous callers such as the CLI.
///
/// # Errors
/// Returns [`WebError`] as [`serve`] does, or if the runtime can't start.
pub fn serve_blocking(
    options: ServeOptions,
    loader: Loader,
    ready: impl FnOnce(SocketAddr),
) -> Result<(), WebError> {
    let addr = options.addr;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| WebError::Io { addr, source })?
        .block_on(serve(options, loader, ready))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_paths_are_plain_segments() {
        assert_eq!(normalize_base("lineage/").unwrap(), "/lineage");
        assert_eq!(normalize_base("/tools/lineage").unwrap(), "/tools/lineage");
        assert_eq!(normalize_base("/").unwrap(), "");
        for bad in ["/{x", "/a/{id}", "my lineage", "/a/../b", "/a//b", "/*rest"] {
            assert!(normalize_base(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn host_names_drop_the_port() {
        assert_eq!(host_name("LocalHost:8765"), "localhost");
        assert_eq!(host_name("[::1]:80"), "[::1]");
        assert_eq!(host_name("127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn unknown_columns_are_refused_not_ignored() {
        let columns = ["amount".to_owned(), "id".to_owned()];
        assert_eq!(known_column(&columns, "AMOUNT").unwrap(), "amount");
        assert!(known_column(&columns, "amont").is_err());
        assert!(known_column(&[], "amount").is_err());
    }
}
