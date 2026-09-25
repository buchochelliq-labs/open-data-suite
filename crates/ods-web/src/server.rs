//! `ods serve`: the explorer over HTTP, with a read-only JSON API and live reload.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use ods_core::ColumnRef;
use ods_lineage::export::GraphNode;
use ods_lineage::{Change, ColumnChangeKind, ColumnGraph, GraphDocument};
use serde::{Deserialize, Serialize};

use crate::search::search;

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
}

impl Snapshot {
    /// A snapshot.
    pub fn new(document: GraphDocument, graph: ColumnGraph, source: impl Into<String>) -> Self {
        Self {
            document,
            graph,
            source: source.into(),
        }
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
}

impl ServeOptions {
    /// Options for `addr`, no base path, no watching.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            watch: None,
            base_path: String::new(),
            allowed_hosts: Vec::new(),
        }
    }

    /// Serves under a URL prefix such as `/lineage` or `/tools/lineage`.
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
        "invalid base path `{0}`: use segments of letters, digits, `-`, `.`, `_` or `~`, e.g. /lineage"
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

struct AppState {
    snapshot: RwLock<Arc<Snapshot>>,
    generation: AtomicU64,
    last_error: Mutex<Option<String>>,
    /// Whether `/api/version` may show local paths and error text. Only on loopback:
    /// beyond it, those go to the server log.
    details: bool,
}

impl AppState {
    fn current(&self) -> Arc<Snapshot> {
        self.snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

type Shared = Arc<AppState>;

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
        details: options.addr.ip().is_loopback(),
    })
}

fn router_with_state(state: Shared, options: &ServeOptions) -> Router {
    let base_path = options.base_path.as_str();
    // Routes are prefixed rather than nested: the page resolves `api/...` against its
    // own URL, so it must be served at `<base>/` (with the slash), and `<base>` alone
    // redirects there.
    let at = |path: &str| format!("{base_path}{path}");
    let mut app = Router::new()
        .route(&at("/"), get(index))
        .route(&at("/index.html"), get(index))
        .route(&at("/healthz"), get(|| async { "ok" }))
        .route(&at("/api/version"), get(version))
        .route(&at("/api/graph"), get(graph))
        .route(&at("/api/search"), get(search_handler))
        .route(&at("/api/node"), get(node))
        .route(&at("/api/impact"), get(impact));
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
             img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CACHE_CONTROL, "no-store"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

async fn index(State(state): State<Shared>) -> Response {
    // The first paint is embedded, with its generation so the page notices any reload
    // after it; the page then polls the API.
    let generation = state.generation.load(Ordering::SeqCst);
    match crate::page::page(Some(&state.current().document), "api", generation) {
        Ok(html) => Html(html).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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
