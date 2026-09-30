//! The State pages (#311): Plan with its Why panel, Runs, and one Run, rendered on the
//! server from the view models in [`crate::dashboard::state`], and their JSON API.
//!
//! Every route is `GET`: actions are commands to copy, never requests to the server.
//! Pages are built on a blocking thread, as they may plan (see `Dashboard::plan_at`).

use std::fmt::Write as _;
use std::sync::atomic::Ordering;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::get;
use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};
use ods_core::state::{PlanAction, Timestamp};
use serde::Deserialize;

use crate::dashboard::journal::{MISSING, NodeStatsView};
use crate::dashboard::state::{
    ChainLine, LastRunView, NO_JOURNAL, OutcomeFrom, PlanRow, PlanView, RunFilter, RunOutcome,
    RunPageView, RunRow, RunsView, TimelineRow, WhyView, count, date_and_time,
};
use crate::dashboard::{CommandHint, EmptyState, ShellView, StateStatus};
use crate::home::{Frame, framed};
use crate::model_page::{no_link_reason, warehouse_link, with_code};
use crate::server::Shared;
use ods_sdk::contracts::run_events::NodeRunStatus;

const CSS: &str = include_str!("../assets/state.css");
const JS: &str = include_str!("../assets/state.js");

/// Whether node names link to their Model page (`catalog/<id>`): on since the Catalog
/// pages exist (#313); kept as one switch for pages that may lack them.
const CATALOG_PAGES: bool = true;

/// The State pages' routes, prefixed by `at` (the base path).
pub(crate) fn routes(app: Router<Shared>, at: &dyn Fn(&str) -> String) -> Router<Shared> {
    app.route(
        &at("/state"),
        get(|| async { Redirect::temporary("state/plan") }),
    )
    .route(
        &at("/state/"),
        get(|| async { Redirect::temporary("plan") }),
    )
    .route(&at("/state/plan"), get(plan_page))
    .route(&at("/state/runs"), get(runs_page))
    .route(&at("/state/runs/{run}"), get(run_page))
    .route(&at("/api/state/plan"), get(plan_api))
    .route(&at("/api/state/plan/{node}"), get(why_api))
    .route(&at("/api/state/runs"), get(runs_api))
    .route(&at("/api/state/runs/{run}"), get(run_api))
}

// ----------------------------------------------------------------------- handlers

#[derive(Debug, Default, Deserialize)]
struct PlanQuery {
    /// The node whose Why panel is open: an id, or a name only one node has.
    node: Option<String>,
    /// `build` or `reuse`; anything else shows all.
    action: Option<String>,
    /// `json` shows the Why panel's data instead of its explanation.
    view: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RunsQuery {
    outcome: Option<String>,
    target: Option<String>,
    date: Option<String>,
    run: Option<String>,
}

impl RunsQuery {
    fn filter(self) -> RunFilter {
        let some = |v: Option<String>| v.filter(|v| !v.is_empty());
        RunFilter {
            outcome: some(self.outcome),
            target: some(self.target),
            date: some(self.date),
            run: some(self.run),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RunQuery {
    /// `nodes` shows the table instead of the timeline.
    tab: Option<String>,
}

/// Runs `page` on a blocking thread: building a page may plan, which reads files.
pub(crate) async fn blocking(page: impl FnOnce() -> Response + Send + 'static) -> Response {
    tokio::task::spawn_blocking(page)
        .await
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

async fn plan_page(State(state): State<Shared>, Query(query): Query<PlanQuery>) -> Response {
    blocking(move || {
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let view = dashboard.plan_view(
            state.details,
            Timestamp::now(),
            query.action.as_deref(),
            query.node.as_deref(),
            &snapshot.names(),
        );
        Html(plan_html(
            &dashboard.shell("state"),
            &view,
            query.view.as_deref() == Some("json"),
            state.generation.load(Ordering::SeqCst),
        ))
        .into_response()
    })
    .await
}

async fn plan_api(State(state): State<Shared>, Query(query): Query<PlanQuery>) -> Response {
    blocking(move || {
        let snapshot = state.current();
        Json(snapshot.dashboard().plan_view(
            state.details,
            Timestamp::now(),
            query.action.as_deref(),
            query.node.as_deref(),
            &snapshot.names(),
        ))
        .into_response()
    })
    .await
}

async fn why_api(State(state): State<Shared>, Path(node): Path<String>) -> Response {
    blocking(move || {
        let snapshot = state.current();
        match snapshot
            .dashboard()
            .why_view(Timestamp::now(), &node, &snapshot.names())
        {
            Some(why) => Json(why).into_response(),
            None => not_found("no such planned node, or no plan: see /api/state/plan"),
        }
    })
    .await
}

async fn runs_page(State(state): State<Shared>, Query(query): Query<RunsQuery>) -> Response {
    blocking(move || {
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let view = dashboard.runs_view(
            state.details,
            Timestamp::now(),
            &query.filter(),
            &snapshot.names(),
        );
        Html(runs_html(
            &dashboard.shell("state"),
            &view,
            state.generation.load(Ordering::SeqCst),
        ))
        .into_response()
    })
    .await
}

async fn runs_api(State(state): State<Shared>, Query(query): Query<RunsQuery>) -> Response {
    blocking(move || {
        let snapshot = state.current();
        Json(snapshot.dashboard().runs_view(
            state.details,
            Timestamp::now(),
            &query.filter(),
            &snapshot.names(),
        ))
        .into_response()
    })
    .await
}

async fn run_page(
    State(state): State<Shared>,
    Path(run): Path<String>,
    Query(query): Query<RunQuery>,
) -> Response {
    blocking(move || {
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let shell = dashboard.shell("state");
        let generation = state.generation.load(Ordering::SeqCst);
        match dashboard.run_view(state.details, &run, &snapshot.names()) {
            Some(view) => Html(run_html(
                &shell,
                &view,
                query.tab.as_deref() == Some("nodes"),
                generation,
            ))
            .into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Html(missing_run_html(&shell, &run, generation)),
            )
                .into_response(),
        }
    })
    .await
}

async fn run_api(State(state): State<Shared>, Path(run): Path<String>) -> Response {
    blocking(move || {
        let snapshot = state.current();
        match snapshot
            .dashboard()
            .run_view(state.details, &run, &snapshot.names())
        {
            Some(view) => Json(view).into_response(),
            None => not_found("no such run among the listed ones: see /api/state/runs"),
        }
    })
    .await
}

// ------------------------------------------------------------------------ helpers

/// Percent-encodes a URL path segment or query value (node and run ids).
fn enc(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// `?a=1&b=2` from the pairs whose value isn't empty; `?` when none.
fn query(pairs: &[(&str, &str)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    format!("?{}", parts.join("&"))
}

/// A node's name: a link to its Model page once those exist ([`CATALOG_PAGES`]), else
/// plain text. `root` leads from the page to the dashboard's root.
fn node_name(node: &str, name: &str, root: &str) -> String {
    if CATALOG_PAGES {
        format!(
            r#"<a class="mono" href="{root}catalog/{}">{}</a>"#,
            attr(&enc(node)),
            text(name)
        )
    } else {
        format!(
            r#"<span class="mono" title="{}">{}</span>"#,
            attr(node),
            text(name)
        )
    }
}

const INFO: &str = r#"<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="9"></circle><path d="M12 11v5M12 8h.01"></path></svg>"#;
const DB: &str = r#"<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><ellipse cx="12" cy="5" rx="8" ry="3"></ellipse><path d="M4 5v14c0 1.7 3.6 3 8 3s8-1.3 8-3V5M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"></path></svg>"#;
const SHIELD: &str = r#"<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3 5 6v6c0 4 3 7 7 9 4-2 7-5 7-9V6z"></path><path d="m9 12 2 2 4-4"></path></svg>"#;
const OK_ICON: &str = r#"<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9"></circle><path d="m8 12 3 3 5-6"></path></svg>"#;
const FAIL_ICON: &str = r#"<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="9"></circle><path d="m9 9 6 6M15 9l-6 6"></path></svg>"#;
const PARTIAL_ICON: &str = r#"<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="9"></circle><path d="M12 7v6M12 16.5h.01"></path></svg>"#;
const RECORDED_ICON: &str = r#"<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-dasharray="3 3" aria-hidden="true"><circle cx="12" cy="12" r="9"></circle></svg>"#;

/// What a recorded run's outcome does and doesn't say (as on Home).
const RECORDED_NOTE: &str = "Recorded: its successful builds are the new state. Whether other nodes failed isn't stored for this run.";
/// The row tint of an outcome: red when it failed, amber when it partly did.
fn row_tint(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Failed => " failed",
        RunOutcome::Partial => " partial",
        _ => "",
    }
}

/// What "kept earlier build" counts when the run's journal says which nodes it ran.
const KEPT_JOURNAL_NOTE: &str = "Not in this run: reused or not selected. Its failed and skipped nodes are counted apart; they keep their last good build too.";
/// What "kept earlier build" covers (as on Home).
const KEPT_NOTE: &str = "Not rebuilt by this run: reused, not selected, or failed. The snapshot keeps the last good build either way.";
/// Where a run's command and outcome come from when known.
const LAST_RUN_NOTE: &str = "From the last run's record, kept beside the store for `ods state retry`: this snapshot records its run id.";
/// Why "recorded nothing" is inferred.
const NOTHING_NOTE: &str = "Inferred: no listed snapshot records this run's id. A clock step or a later `ods state record` could make this wrong.";
/// A count ODS doesn't record.
const NOT_RECORDED: &str =
    r#"<span class="st-na" aria-label="not recorded" title="Not recorded for this run">—</span>"#;

fn inferred_chip(title: &str) -> String {
    format!(
        r#"<span class="st-grade inferred" title="{}">inferred</span>"#,
        attr(title)
    )
}

fn outcome_word(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Unfinished => "running or stopped",
        o => o.word(),
    }
}

/// The CSS class of an outcome: `ok`, `bad`, `warn` or `rec` (dashed: not known).
fn outcome_class(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Succeeded => "ok",
        RunOutcome::Failed => "bad",
        RunOutcome::Partial => "warn",
        _ => "rec",
    }
}

fn copy_button(value: &str, label: &str, what: &str) -> String {
    format!(
        r#"<button type="button" class="st-btn" data-copy="{}" aria-label="{}">{}</button>"#,
        attr(value),
        attr(what),
        text(label)
    )
}

fn command_box(b: &mut String, command: &CommandHint) {
    let _ = write!(
        b,
        r#"<div class="st-cmd"><code>{c}</code>{copy}</div><p class="st-small">{d}</p>"#,
        c = text(&command.command),
        copy = copy_button(
            &command.command,
            "Copy",
            &format!("Copy the command {}", command.command)
        ),
        d = text(&command.does),
    );
}

fn empty_card(b: &mut String, state: StateStatus, empty: &EmptyState) {
    let _ = write!(
        b,
        r#"<section class="card empty" data-state="{state}"><h2>{title}</h2><p>{message}</p><div class="cmds">"#,
        state = serde_json::to_value(state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        title = text(&empty.title),
        message = text(&empty.message),
    );
    for command in &empty.commands {
        let _ = write!(
            b,
            r#"<div class="cmd"><code>{c}</code>{copy}<span>{d}</span></div>"#,
            c = text(&command.command),
            copy = copy_button(
                &command.command,
                "Copy",
                &format!("Copy the command {}", command.command)
            ),
            d = text(&command.does),
        );
    }
    b.push_str("</div></section>");
}

/// The breadcrumb: every part but the last links, if it has an href.
pub(crate) fn crumbs(parts: &[(&str, Option<&str>)], mono_last: bool) -> String {
    let mut out = String::new();
    for (i, (label, href)) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str(r#"<span class="crumb-sep">/</span>"#);
        }
        if let Some(href) = href {
            let _ = write!(
                out,
                r#"<a class="crumb" href="{}">{}</a>"#,
                attr(href),
                text(label)
            );
        } else {
            let class = match (i == parts.len() - 1, mono_last) {
                (true, true) => "crumb-here mono",
                (true, false) => "crumb-here",
                (false, _) => "crumb",
            };
            let _ = write!(out, r#"<span class="{class}">{}</span>"#, text(label));
        }
    }
    out
}

fn warnings(b: &mut String, warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }
    b.push_str(r#"<ul class="st-warnings" aria-label="Warnings">"#);
    for w in warnings {
        let _ = write!(b, "<li>{}</li>", text(w));
    }
    b.push_str("</ul>");
}

/// Where "Copied" is announced to screen readers.
const LIVE: &str = r#"<div id="st-live" class="sr-only" aria-live="polite"></div>"#;

// --------------------------------------------------------------------------- plan

fn plan_html(shell: &ShellView, view: &PlanView, json: bool, generation: u64) -> String {
    let mut b = String::with_capacity(32 * 1024);
    b.push_str(r#"<div class="st-split"><section class="st-main">"#);
    if let Some(empty) = &view.empty {
        if view.state == StateStatus::NoStore {
            // The plan's own words: without a store, everything would build.
            let empty = EmptyState {
                title: "No plan to show yet".to_owned(),
                message: "There is no state store yet, so nothing is recorded to reuse: every node would build. Record a first run in a terminal; this page then shows what the next run builds and reuses, and why.".to_owned(),
                commands: empty.commands.clone(),
            };
            empty_card(&mut b, view.state, &empty);
        } else {
            empty_card(&mut b, view.state, empty);
        }
    } else if let Some(error) = &view.error {
        let _ = write!(
            b,
            r#"<section class="card empty" data-state="plan_error"><h2>The plan couldn't be made</h2><p>{}</p><div class="cmds"><div class="cmd"><code>ods state plan</code><span>says the same in a terminal</span></div></div></section>"#,
            text(error)
        );
    } else {
        plan_main(&mut b, view);
    }
    warnings(&mut b, &view.warnings);
    b.push_str("</section>");
    match &view.selected {
        Some(why) => why_panel(&mut b, why, view.filter, json),
        None if view.empty.is_none() && view.error.is_none() => {
            b.push_str(if view.counts.total == 0 {
                r#"<aside class="st-side w380" id="why" aria-label="Why"><p class="st-pad muted">Nothing is planned.</p></aside>"#
            } else {
                r#"<aside class="st-side w380" id="why" aria-label="Why"><p class="st-pad muted">No planned node by that name: pick one in the table to see why it builds or is reused.</p></aside>"#
            });
        }
        None => {}
    }
    b.push_str("</div>");
    b.push_str(LIVE);
    let status = format!(
        r#"<span class="st-status">{}</span>"#,
        text(&view.based_on.map_or_else(
            || "no recorded state · dry run, nothing built".to_owned(),
            |s| format!("against snapshot {s} · dry run, nothing built"),
        ))
    );
    let title = format!("Plan for {}", view.target);
    let frame = Frame {
        title: &title,
        crumbs: Some(crumbs(&[("State", Some("plan")), (&title, None)], false)),
        root: "../",
        sub: Some("plan"),
        search: true,
        status: Some(status),
        css: CSS,
        js: JS,
    };
    framed(shell, &frame, &b, generation)
}

fn plan_main(b: &mut String, view: &PlanView) {
    let c = view.counts;
    let relations = if c.reuse == 0 {
        r#"<span class="st-value st-dim" aria-label="none to check">—</span><span class="st-note">nothing is reused</span>"#.to_owned()
    } else if c.relations_checked == 0 {
        format!(
            r#"<span class="st-value st-dim small">not checked</span><span class="st-note">0 of {} reused relations · planned offline · <code>ods state build --dry-run</code> checks them</span>"#,
            c.reuse
        )
    } else {
        format!(
            r#"<span class="st-value">{} / {}</span><span class="st-note">reused relations found in the warehouse</span>"#,
            c.relations_checked, c.reuse
        )
    };
    let _ = write!(
        b,
        r#"<div class="st-tiles3"><div class="st-tile" data-tile="build"><span class="st-label">To build</span><span class="st-value build">{build}</span></div><div class="st-tile" data-tile="reuse"><span class="st-label">To reuse</span><span class="st-value reuse">{reuse}</span></div><div class="st-tile" data-tile="relations"><span class="st-label">Relations checked</span>{relations}</div></div>"#,
        build = c.build,
        reuse = c.reuse,
    );
    if c.build == 0 && c.total > 0 {
        let _ = write!(
            b,
            r#"<div class="st-notice" data-state="nothing_to_build">{INFO}<span><strong>Nothing to build:</strong> every node reuses its recorded build. <code>ods state build --dry-run</code> also checks that each reused relation is still in the warehouse.</span></div>"#
        );
    }
    if let Some(command) = view.commands.first() {
        let _ = write!(
            b,
            r#"<div class="st-cmdbar"><code>{c}</code><span class="st-cmd-does">{d}</span>{copy}</div>"#,
            c = text(&command.command),
            d = text(&command.does),
            copy = copy_button(&command.command, "Copy", "Copy the command ods state build"),
        );
    }
    let selected = view.selected.as_ref().map_or("", |w| w.node.as_str());
    b.push_str(r#"<nav class="st-pills" aria-label="Filter by action">"#);
    for (key, label, n) in [
        ("all", "All", c.total),
        ("build", "Build", c.build),
        ("reuse", "Reuse", c.reuse),
    ] {
        let current = if view.filter == key {
            r#" aria-current="page""#
        } else {
            ""
        };
        let _ = write!(
            b,
            r#"<a href="{href}"{current}>{label} <span class="st-count">{n}</span></a>"#,
            href = attr(&query(&[
                ("action", if key == "all" { "" } else { key }),
                ("node", selected)
            ])),
        );
    }
    b.push_str("</nav>");
    if view.rows.is_empty() {
        b.push_str(r#"<p class="st-small">No node has this action.</p>"#);
        return;
    }
    b.push_str(r#"<table class="st-table" aria-label="Decisions, builds first"><thead><tr><th scope="col">Node</th><th scope="col">Type</th><th scope="col">Action</th><th scope="col">Why</th></tr></thead><tbody>"#);
    for (i, row) in view.rows.iter().enumerate() {
        plan_row(b, i, row, view.filter);
    }
    b.push_str("</tbody></table>");
}

fn action_pill(action: PlanAction) -> &'static str {
    if action == PlanAction::Build {
        r#"<span class="st-pill build">BUILD</span>"#
    } else {
        r#"<span class="st-pill reuse">REUSE</span>"#
    }
}

fn plan_row(b: &mut String, i: usize, row: &PlanRow, filter: &str) {
    // The link returns to this row, so the page keeps its place after selecting.
    let _ = write!(
        b,
        r#"<tr id="r{i}"{sel} data-node="{id}"><th scope="row"><a class="st-node" href="{href}#r{i}">{name}</a></th><td class="st-kind">{kind}</td><td>{pill}</td><td class="st-why-cell">{why}{unknown}</td></tr>"#,
        sel = if row.selected {
            r#" class="selected" aria-current="true""#
        } else {
            ""
        },
        id = attr(&row.node),
        href = attr(&query(&[
            ("action", if filter == "all" { "" } else { filter }),
            ("node", &row.node)
        ])),
        name = text(&row.name),
        kind = text(&row.kind),
        pill = action_pill(row.action),
        why = text(&row.why),
        unknown = if row.unknown_evidence {
            r#" <span class="st-grade unknown" title="It builds because evidence is missing, not because something changed">unknown evidence</span>"#
        } else {
            ""
        },
    );
}

fn grade_chip(grade: &str) -> String {
    let class = match grade {
        "exact" | "semantic" => "fact",
        "unknown" => "unknown",
        _ => "inferred",
    };
    format!(
        r#"<span class="st-grade {class}" title="How exact the evidence is: only semantic or exact evidence is shown as fact">{}</span>"#,
        text(grade)
    )
}

fn why_panel(b: &mut String, why: &WhyView, filter: &str, json: bool) {
    let builds = why.action == PlanAction::Build;
    let filter = if filter == "all" { "" } else { filter };
    let _ = write!(
        b,
        r#"<aside class="st-side w380 st-why" id="why" aria-label="Why {name} {verb}"><div class="st-side-head"><span class="st-label">{eyebrow}</span><span class="st-why-name mono">{name}</span><div class="st-head-links"><nav class="st-toggle" aria-label="Show as"><a href="{explain}"{p1}>Explanation</a><a href="{json_href}"{p2}>JSON</a></nav><span class="st-links"><a href="../lineage?node={id}">Lineage</a>{model}</span></div></div>"#,
        name = text(&why.name),
        verb = if builds { "builds" } else { "is reused" },
        eyebrow = if builds {
            "Why it builds"
        } else {
            "Why it's reused"
        },
        explain = attr(&query(&[("action", filter), ("node", &why.node)])),
        json_href = attr(&query(&[
            ("action", filter),
            ("node", &why.node),
            ("view", "json")
        ])),
        p1 = if json { "" } else { r#" aria-current="page""# },
        p2 = if json { r#" aria-current="page""# } else { "" },
        id = attr(&enc(&why.node)),
        model = if CATALOG_PAGES {
            format!(
                r#" · <a href="../catalog/{}">Model page</a>"#,
                attr(&enc(&why.node))
            )
        } else {
            String::new()
        },
    );
    if json {
        let data = serde_json::to_string_pretty(why).unwrap_or_default();
        let _ = write!(
            b,
            r#"<div class="st-pad st-json-wrap"><p class="st-small">The same as <a href="../api/state/plan/{id}"><code>api/state/plan/…</code></a>.</p><pre class="st-json">{data}</pre></div></aside>"#,
            id = attr(&enc(&why.node)),
            data = text(&data),
        );
        return;
    }
    b.push_str(r#"<ol class="st-steps">"#);
    why_recorded(b, why);
    why_reads(b, why, filter);
    why_decision(b, why, filter);
    why_chain(b, why);
}

/// The recorded build and the fingerprint compared with it.
fn why_recorded(b: &mut String, why: &WhyView) {
    match &why.last_build {
        Some(last) => {
            let source = match last.built_in {
                Some(built) if built != last.snapshot => format!(
                    "built by run {run} (snapshot {built}), still current in snapshot {now}",
                    run = short_span(&last.run_id, &last.short_run_id),
                    now = last.snapshot
                ),
                Some(_) => format!(
                    "built by run {run} in snapshot {now}",
                    run = short_span(&last.run_id, &last.short_run_id),
                    now = last.snapshot
                ),
                None => format!(
                    "built by run {run}, still current in snapshot {now}",
                    run = short_span(&last.run_id, &last.short_run_id),
                    now = last.snapshot
                ),
            };
            let _ = write!(
                b,
                r#"<li><strong>Recorded build found</strong><span class="st-sub">{source}, at <span class="mono st-ts">{at}</span></span></li>"#,
                at = text(&last.built_at.to_string()),
            );
        }
        None => {
            let _ = write!(
                b,
                r#"<li><strong>No recorded build</strong><span class="st-sub">{}</span></li>"#,
                text(&why.based_on.map_or_else(
                    || "nothing is recorded yet".to_owned(),
                    |s| format!("snapshot {s} has none for it"),
                ))
            );
        }
    }
    let fp = &why.fingerprint;
    let _ = write!(b, "<li><strong>Fingerprint {}</strong>", text(&fp.summary));
    if fp.compared {
        let (changed, same): (Vec<_>, Vec<_>) = fp.components.iter().partition(|c| c.changed);
        b.push_str(r#"<div class="st-grid2">"#);
        for c in &changed {
            let _ = write!(
                b,
                r#"<span class="muted">{}</span><span class="st-changed">changed</span>"#,
                text(&c.name)
            );
        }
        b.push_str("</div>");
        if !same.is_empty() {
            let _ = write!(
                b,
                r#"<details class="st-more"><summary>{} unchanged</summary><div class="st-grid2">"#,
                same.len()
            );
            for c in &same {
                let _ = write!(
                    b,
                    r#"<span class="muted">{}</span><span>unchanged</span>"#,
                    text(&c.name)
                );
            }
            b.push_str("</div></details>");
        }
        if let (Some(before), Some(after)) = (&fp.before, &fp.after)
            && before != after
        {
            let _ = write!(
                b,
                r#"<span class="st-sub">digest <span class="mono">{}</span> → <span class="mono">{}</span>. Snapshots keep digests, not code, so the change itself isn't shown.</span>"#,
                text(before),
                text(after)
            );
        }
    }
    b.push_str("</li>");
}

/// A run id, short, with the full id as its title.
fn short_span(run_id: &str, short: &str) -> String {
    format!(
        r#"<span class="mono" title="{}">{}</span>"#,
        attr(run_id),
        text(short)
    )
}

/// The planned parents and the sources it reads, with each source's evidence.
fn why_reads(b: &mut String, why: &WhyView, filter: &str) {
    let entries = why.parents.len() + why.sources.len();
    if entries == 0 {
        return;
    }
    // Many inputs would push the decision out of view: collapsed beyond three.
    if entries > 3 {
        let _ = write!(
            b,
            r#"<li><details class="st-more"><summary><strong>What it reads</strong> ({entries})</summary><ul class="st-reads">"#
        );
    } else {
        b.push_str(r#"<li><strong>What it reads</strong><ul class="st-reads">"#);
    }
    for p in &why.parents {
        let _ = write!(
            b,
            r#"<li><a class="mono" href="{href}">{name}</a> <span class="{class}">{decision}</span></li>"#,
            href = attr(&format!(
                "{}#why",
                query(&[("action", filter), ("node", &p.node)])
            )),
            name = text(&p.name),
            class = if p.builds { "st-changed" } else { "muted" },
            decision = text(&p.decision.replace('_', " ")),
        );
    }
    for s in &why.sources {
        let _ = write!(
            b,
            r#"<li><span class="mono">{name}</span> {grade} <span class="muted">{version}</span>"#,
            name = text(&s.name),
            grade = grade_chip(s.grade),
            version = text(&match (&s.version, s.usable) {
                (Some(v), true) => format!("version {v}"),
                (Some(v), false) => format!("version {v} (not usable for reuse)"),
                (None, _) => "no usable version: its readers build".to_owned(),
            }),
        );
        let mut details = Vec::new();
        if let Some(strategy) = &s.strategy {
            details.push(format!("strategy {strategy}"));
        }
        if let Some(origin) = &s.origin {
            details.push(format!("from {origin}"));
        }
        for skipped in &s.skipped {
            details.push(format!("passed over {skipped}"));
        }
        if !details.is_empty() {
            let _ = write!(
                b,
                r#"<span class="st-sub">{}</span>"#,
                text(&details.join(" · "))
            );
        }
        b.push_str("</li>");
    }
    b.push_str(if entries > 3 {
        "</ul></details></li>"
    } else {
        "</ul></li>"
    });
}

/// The relation check and the decision, with the readers that rebuild with it.
fn why_decision(b: &mut String, why: &WhyView, filter: &str) {
    let builds = why.action == PlanAction::Build;
    let _ = write!(
        b,
        r#"<li><strong>Relation check</strong> {grade}<span class="st-sub">{note}</span></li>"#,
        grade = if why.relation.status == "not_needed" {
            String::new()
        } else {
            grade_chip(why.relation.grade)
        },
        note = text(&why.relation.note),
    );
    let _ = write!(
        b,
        r#"<li><strong>Decision: {}</strong><ul class="st-reasons">"#,
        if builds { "BUILD" } else { "REUSE" }
    );
    for r in &why.reasons {
        let _ = write!(b, "<li>{}</li>", text(&r.message));
    }
    b.push_str("</ul>");
    if !why.readers.is_empty() {
        let names: Vec<String> = why
            .readers
            .iter()
            .map(|r| {
                format!(
                    r#"<a class="mono" href="{}">{}</a>"#,
                    attr(&format!(
                        "{}#why",
                        query(&[("action", filter), ("node", &r.node)])
                    )),
                    text(&r.name)
                )
            })
            .collect();
        let _ = write!(
            b,
            r#"<span class="st-sub">{} with it: {}.</span>"#,
            if why.readers.len() == 1 {
                "One reader rebuilds".to_owned()
            } else {
                format!("{} readers rebuild", why.readers.len())
            },
            names.join(", ")
        );
    }
    b.push_str("</li></ol>");
}

/// The reason chain, every piece of evidence and the command, collapsed.
fn why_chain(b: &mut String, why: &WhyView) {
    let _ = write!(
        b,
        r#"<details class="st-pad st-chain"><summary>Reason chain · as <code>ods state explain</code></summary><p class="st-verdict">{}</p><ul class="st-tree">"#,
        text(&why.verdict)
    );
    chain(b, &why.chain);
    b.push_str("</ul></details>");
    if !why.evidence.is_empty() {
        let _ = write!(
            b,
            r#"<details class="st-pad st-evidence"><summary>All evidence ({})</summary><ul>"#,
            why.evidence.len()
        );
        for e in &why.evidence {
            let _ = write!(
                b,
                r#"<li><span class="mono">{kind}</span> of {subject}{value} {grade}</li>"#,
                kind = text(&e.kind),
                subject = text(&e.subject_name),
                value = e
                    .value
                    .as_ref()
                    .map(|v| format!(" = {}", text(v)))
                    .unwrap_or_default(),
                grade = grade_chip(e.grade),
            );
        }
        b.push_str("</ul></details>");
    }
    b.push_str(r#"<details class="st-pad st-evidence"><summary>In a terminal</summary>"#);
    for command in &why.commands {
        command_box(b, command);
    }
    b.push_str("</details></aside>");
}

/// The reason chain as nested lists, as `ods state explain` prints its tree.
fn chain(b: &mut String, lines: &[ChainLine]) {
    let mut open = 0;
    for (i, line) in lines.iter().enumerate() {
        let _ = write!(
            b,
            r#"<li><span class="mono">{}</span>: {}"#,
            text(&line.name),
            if line.action == PlanAction::Build {
                r#"<span class="st-changed">build</span>"#
            } else {
                r#"<span class="st-reused">reuse</span>"#
            }
        );
        if line.repeated {
            b.push_str(r#" <span class="muted">(see above)</span>"#);
        }
        b.push_str("<ul>");
        for reason in &line.reasons {
            let _ = write!(b, "<li>{}</li>", text(reason));
        }
        if !line.changed.is_empty() {
            let _ = write!(
                b,
                r#"<li><span class="muted">changed:</span> {}</li>"#,
                text(&line.changed.join(", "))
            );
        }
        open += 1;
        // Close this node, and the ones above it, down to the next line's depth.
        let next = lines.get(i + 1).map_or(0, |n| n.depth);
        let close = line.depth + 1 - next.min(line.depth + 1);
        for _ in 0..close {
            b.push_str("</ul></li>");
            open -= 1;
        }
    }
    for _ in 0..open {
        b.push_str("</ul></li>");
    }
}

// --------------------------------------------------------------------------- runs

/// The outcome's words for people and screen readers, with where it comes from.
fn outcome_label(row: &RunRow) -> String {
    match (row.outcome, row.outcome_from) {
        (RunOutcome::Recorded, _) => "recorded: outcome not stored, no run journal".to_owned(),
        (RunOutcome::Unfinished, _) => "running or stopped without finishing".to_owned(),
        (_, _) if row.outcome_inferred => {
            "unknown: probably stopped (inferred), no event for over 10 minutes".to_owned()
        }
        (o, OutcomeFrom::LastRun) => format!("{} (from the last run's record)", outcome_word(o)),
        (o, _) => outcome_word(o).to_owned(),
    }
}

fn glyph(outcome: RunOutcome, label: &str) -> String {
    let mark = match outcome {
        RunOutcome::Succeeded => "✓",
        RunOutcome::Failed => "✕",
        RunOutcome::Partial => "!",
        RunOutcome::Unknown => "?",
        _ => "",
    };
    format!(
        r#"<span class="st-glyph {class}{run}" aria-hidden="true" title="{}">{mark}</span>"#,
        attr(label),
        class = outcome_class(outcome),
        run = if outcome == RunOutcome::Unfinished {
            " run"
        } else {
            ""
        },
    )
}

/// A count as a pill; zero as a plain `0` (it is known to be none), and an unknown
/// count as `—`.
fn num_pill(n: Option<usize>, class: &str) -> String {
    match n {
        Some(0) => r#"<span class="st-num zero">0</span>"#.to_owned(),
        Some(n) => format!(r#"<span class="st-num {class}">{n}</span>"#),
        None => NOT_RECORDED.to_owned(),
    }
}

/// A count as a pill, or `—` with why it isn't known.
fn num_pill_or(n: Option<usize>, class: &str, why: &str) -> String {
    n.map_or_else(|| missing(why), |n| num_pill(Some(n), class))
}

/// A missing stat with its reason written beside it, where there is room.
fn missing_said(why: &str) -> String {
    format!(
        r#"{}<span class="st-why">{}</span>"#,
        missing(why),
        text(why)
    )
}

/// A command line, token by token: lines break between tokens, never inside one
/// (`--profiles-dir` stays whole).
fn command_tokens(command: &str) -> String {
    command
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(|t| format!(r#"<span class="tok">{}</span>"#, text(t)))
        .collect::<Vec<_>>()
        .join(" <wbr>")
}

/// A missing stat: `—`, with why.
fn missing(why: &str) -> String {
    format!(
        r#"<span class="st-na" aria-label="not recorded: {why}" title="{why}">—</span>"#,
        why = attr(why)
    )
}

/// A run's duration, or `—` with why it's missing.
fn duration_cell(row: &RunRow) -> String {
    match (&row.duration, &row.stats, &row.no_journal) {
        (Some(d), _, _) => format!(r#"<span class="mono">{}</span>"#, text(d)),
        (None, Some(_), _) if row.outcome == RunOutcome::Unfinished => {
            missing("still running, or stopped without finishing")
        }
        (None, Some(stats), _) if !stats.live => {
            missing("not timed: this run's stats came from its final results only")
        }
        (None, Some(_), _) => missing("its journal doesn't say when it finished"),
        (None, None, Some(why)) => missing(why),
        (None, None, None) => missing("not recorded"),
    }
}

/// A run's rows total: `298`, `≥ 298` (some nodes didn't report), or `—`.
fn rows_cell(row: &RunRow) -> String {
    let Some(stats) = &row.stats else {
        return missing(row.no_journal.as_deref().unwrap_or("not recorded"));
    };
    match stats.rows_affected {
        None => missing(&format!(
            "not reported: {} didn't report rows",
            count(stats.rows_unreported, "node")
        )),
        Some(n) if stats.rows_at_least => format!(
            r#"<span class="mono" title="{}">≥&#8201;{n}</span>"#,
            attr(&format!(
                "at least {n} rows ({} didn't report)",
                count(stats.rows_unreported, "node")
            ))
        ),
        Some(n) => format!(r#"<span class="mono">{n}</span>"#),
    }
}

fn target_text(row: &RunRow) -> String {
    row.target.as_ref().map_or_else(
        || "—".to_owned(),
        |t| match &t.kind {
            Some(kind) => format!("{} · {kind}", t.name),
            None => t.name.clone(),
        },
    )
}

const RUN_COLUMNS: &str = r#"<thead><tr><th scope="col"><span class="sr-only">Outcome</span></th><th scope="col">Run</th><th scope="col">Command</th><th scope="col">Target</th><th scope="col">Snapshot</th><th scope="col">Built</th><th scope="col" title="Kept earlier build: reused, not selected, or failed">Kept</th><th scope="col" title="Nodes that failed, from the run's journal; without one, from the last run's record">Failed</th><th scope="col">Skipped</th><th scope="col" title="Rows affected, as the adapter reported them">Rows</th><th scope="col">Started</th><th scope="col">Duration</th><th scope="col">Triggered by</th></tr></thead>"#;

fn runs_html(shell: &ShellView, view: &RunsView, generation: u64) -> String {
    let mut b = String::with_capacity(32 * 1024);
    b.push_str(r#"<div class="st-split"><section class="st-main">"#);
    let _ = write!(
        b,
        r#"<div class="st-title"><h1>Runs</h1><span class="muted">{} · every run recorded in this state store, newest first</span></div><div class="st-notice">{INFO}<span><strong>ODS doesn't schedule anything.</strong> A run appears here after <code>ods state build</code> or <code>ods state run</code> records it in this state store, from your terminal or a CI job.</span></div>"#,
        text(&view.project)
    );
    let more = view.total_capped || view.total > view.limit;
    let _ = write!(
        b,
        r#"<nav class="st-tabs" aria-label="Runs"><a class="st-tab" href="runs" aria-current="page">Local runs<span class="st-count">{badge}</span></a><span class="st-tab disabled" aria-disabled="true" title="CI runs need server mode, which is planned">{ci}<span class="chip">Planned</span></span></nav>"#,
        badge = if more {
            format!("{}+", view.limit)
        } else {
            view.unfiltered.to_string()
        },
        ci = text(view.ci),
    );
    if let Some(empty) = &view.empty {
        empty_card(&mut b, view.state, empty);
        b.push_str("</section></div>");
        b.push_str(LIVE);
        return runs_frame(shell, view, &b, generation);
    }
    facets(&mut b, view);
    let unrecorded = view.last_run.as_ref().filter(|_| view.last_run_listed);
    if view.runs.is_empty() && unrecorded.is_none() {
        b.push_str(r#"<p class="st-small">No run matches these filters.</p>"#);
    } else {
        b.push_str(r#"<table class="st-runs" aria-label="Local runs, newest first">"#);
        b.push_str(RUN_COLUMNS);
        if let Some(last) = unrecorded {
            last_run_row(&mut b, last, view);
        }
        run_rows(&mut b, view);
        b.push_str("</table>");
    }
    let _ = write!(
        b,
        r#"<div class="st-legend"><span><span class="st-num built">n</span>built</span><span title="{kept}"><span class="st-num kept">n</span>kept earlier build: not in the run (reused or not selected); without a journal, also its failed nodes</span><span><span class="st-num failed">n</span>failed: kept its last good build</span><span><span class="st-num skipped">n</span>skipped: waited on a failed node</span><span><span class="st-glyph warn" aria-hidden="true">!</span>partial: some nodes failed, others succeeded</span><span><span class="st-glyph rec" aria-hidden="true"></span>no journal: outcome not stored</span><span>{NOT_RECORDED} not recorded or not reported</span><span class="st-right">Who ran each run isn't recorded yet.</span></div>"#,
        kept = attr(KEPT_NOTE),
    );
    if more {
        let _ = write!(
            b,
            r#"<div class="st-notice" data-state="truncated">{INFO}<span>Only the newest {} runs are listed. <code>ods state history</code> lists them all.</span></div>"#,
            view.limit
        );
    }
    last_run_notices(&mut b, view);
    b.push_str(
        r#"<details class="st-recorded"><summary>What the state store records</summary><ul>"#,
    );
    for line in &view.recorded {
        let _ = write!(b, "<li>{}</li>", text(line));
    }
    b.push_str("</ul></details></section>");
    runs_side(&mut b, view);
    b.push_str("</div>");
    b.push_str(LIVE);
    runs_frame(shell, view, &b, generation)
}

/// The last run, when it can't be shown with the runs: it names no target, or which
/// snapshot it recorded can't be told.
fn last_run_notices(b: &mut String, view: &RunsView) {
    if let Some(last) = &view.unscoped_last_run {
        let _ = write!(
            b,
            r#"<div class="st-notice" data-state="unscoped_last_run">{INFO}<span><strong>The last run kept on this machine may belong to another target.</strong> <code>{command}</code> started at <span class="mono st-ts">{at}</span>{failed}. It was kept by an older ODS or stopped before it built, so it doesn't say which target it ran for, and it isn't shown with these runs.</span></div>"#,
            command = text(&last.command_name),
            at = text(&last.started_at.to_string()),
            failed = failed_names(last),
        );
    }
    if let Some(last) = view
        .last_run
        .as_ref()
        .filter(|l| l.snapshot.is_none() && !l.recorded_nothing_inferred)
    {
        let _ = write!(
            b,
            r#"<div class="st-notice" data-state="untied_last_run">{INFO}<span><strong>The last run can't be tied to a run here.</strong> <code>{command}</code> started at <span class="mono st-ts">{at}</span>{failed}, but it has no run id, and snapshots were recorded since, so which one it recorded, if any, can't be told. <code>ods state history</code> lists the snapshots.</span></div>"#,
            command = text(&last.command_name),
            at = text(&last.started_at.to_string()),
            failed = failed_names(last),
        );
    }
}

fn failed_names(last: &LastRunView) -> String {
    let names = |nodes: &[crate::dashboard::state::NodeRef]| {
        nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    if !last.failed.is_empty() {
        parts.push(format!(
            "; {} failed or weren't recorded",
            names(&last.failed)
        ));
    }
    if !last.failed_source_tests.is_empty() {
        parts.push(format!(
            "; the tests of {} failed",
            names(&last.failed_source_tests)
        ));
    }
    text(&parts.concat()).into_owned()
}

/// The runs, one table body per day.
fn run_rows(b: &mut String, view: &RunsView) {
    let mut day = String::new();
    for row in &view.runs {
        let (date, time) = row
            .at
            .map_or_else(|| ("Date unknown".to_owned(), String::new()), date_and_time);
        if date != day {
            if !day.is_empty() {
                b.push_str("</tbody>");
            }
            let _ = write!(
                b,
                r#"<tbody><tr class="st-rgroup"><th scope="rowgroup" colspan="13">{}</th></tr>"#,
                text(&if row.at.is_some() {
                    format!("{date} · UTC")
                } else {
                    date.clone()
                })
            );
            day = date;
        }
        let selected = view.selected.as_deref() == Some(row.run_id.as_str());
        let label = outcome_label(row);
        let snap = match row.snapshot {
            Some(id) => id.to_string(),
            None => format!(
                "{} {}",
                text(
                    &row.kept_state
                        .map_or_else(|| "none".to_owned(), |s| format!("kept {s}"))
                ),
                inferred_chip(NOTHING_NOTE)
            ),
        };
        let kept_note = if row.stats.is_some() {
            KEPT_JOURNAL_NOTE
        } else {
            KEPT_NOTE
        };
        let _ = write!(
            b,
            r#"<tr class="{sel}{bad}"{current} data-run="{id}" data-outcome="{outcome}"><td><a href="{select}" aria-label="{select_label}">{glyph}</a></td><td><a class="mono st-runid" href="runs/{href}" title="run {id}">{short}</a></td><td>{command}</td><td class="st-dim">{target}</td><td class="st-snap{snap_class}">{snap}</td><td>{built}</td><td title="{kept_note}">{kept}</td><td>{failed}</td><td>{skipped}</td><td>{rows}</td><td class="mono st-ts" title="{time_note}">{time}</td><td class="st-dim">{duration}</td><td class="st-dim">[user]</td></tr>"#,
            sel = if selected { "selected" } else { "" },
            bad = row_tint(row.outcome),
            current = if selected {
                r#" aria-current="true""#
            } else {
                ""
            },
            id = attr(&row.run_id),
            outcome = row.outcome.word(),
            select = attr(&runs_query(view, Some(&row.run_id))),
            select_label = attr(&format!(
                "Show run {} in the side panel, {label}",
                row.short_run_id
            )),
            glyph = glyph(row.outcome, &label),
            href = attr(&enc(&row.run_id)),
            short = text(&row.short_run_id),
            command = row.command.as_deref().map_or_else(
                || r#"<span class="st-na" aria-label="not recorded" title="Runs don't record their command yet">—</span>"#.to_owned(),
                |c| format!(
                    r#"<code title="{}">{}</code>"#,
                    attr(&format!("{c}\n\n{LAST_RUN_NOTE}")),
                    command_tokens(c)
                ),
            ),
            target = text(&target_text(row)),
            snap_class = if row.snapshot.is_none() { " bad" } else { "" },
            built = num_pill(Some(row.built), "built"),
            kept_note = attr(kept_note),
            kept = num_pill_or(row.kept, "kept", "not known: when it started isn't recorded"),
            failed = num_pill_or(row.failed, "failed", row.no_journal.as_deref().unwrap_or("not recorded")),
            skipped = num_pill_or(row.skipped, "skipped", row.no_journal.as_deref().unwrap_or("not recorded")),
            rows = rows_cell(row),
            time_note = if row.stats.as_ref().is_some_and(|s| s.started_at.is_some()) {
                "When it started, from its journal"
            } else if row.at.is_some() {
                "When its snapshot was recorded: it has no journal to say when it started"
            } else {
                "Not recorded: its journal never said when it started"
            },
            time = if row.at.is_some() { text(&time).into_owned() } else { missing("not recorded: its journal never said when it started") },
            duration = duration_cell(row),
        );
    }
    if !day.is_empty() {
        b.push_str("</tbody>");
    }
}

fn runs_frame(shell: &ShellView, view: &RunsView, body: &str, generation: u64) -> String {
    let status = match &view.store {
        Some(store) => format!(
            r#"<span class="st-status pill">{DB}Local runs · from <code title="{full}">{shown}</code> on this machine</span>"#,
            full = attr(&store.full),
            shown = text(&store.shown),
        ),
        None => {
            format!(r#"<span class="st-status pill">{DB}Local runs · from the state store</span>"#)
        }
    };
    let frame = Frame {
        title: "Runs",
        crumbs: Some(crumbs(&[("State", Some("plan")), ("Runs", None)], false)),
        root: "../",
        sub: Some("runs"),
        search: true,
        status: Some(status),
        css: CSS,
        js: JS,
    };
    framed(shell, &frame, body, generation)
}

/// The value of the applied choice of filter `key`, or `""`.
fn applied<'a>(view: &'a RunsView, key: &str) -> &'a str {
    view.facets
        .iter()
        .find(|f| f.key == key)
        .and_then(|f| f.options.iter().find(|o| o.selected))
        .map_or("", |o| o.value.as_str())
}

/// `?outcome=…&target=…&date=…&run=…`, keeping the filters applied.
fn runs_query(view: &RunsView, run: Option<&str>) -> String {
    query(&[
        ("outcome", applied(view, "outcome")),
        ("target", applied(view, "target")),
        ("date", applied(view, "date")),
        ("run", run.unwrap_or("")),
    ])
}

fn facets(b: &mut String, view: &RunsView) {
    b.push_str(r#"<div class="st-facets">"#);
    for facet in &view.facets {
        let current = facet
            .options
            .iter()
            .find(|o| o.selected)
            .map_or("All", |o| o.label.as_str());
        if let Some(why) = facet.unavailable {
            let _ = write!(
                b,
                r#"<span class="st-facet disabled" title="{why}" aria-disabled="true"><span class="muted">{label}:</span>{current}<span class="muted">▾</span></span>"#,
                why = attr(why),
                label = text(facet.label),
                current = text(current),
            );
            continue;
        }
        let _ = write!(
            b,
            r#"<details class="st-facet"><summary><span class="muted">{label}:</span>{current}<span class="muted" aria-hidden="true">▾</span></summary><div class="st-menu">"#,
            label = text(facet.label),
            current = text(current),
        );
        for option in &facet.options {
            let pairs: Vec<(&str, &str)> = ["outcome", "target", "date"]
                .iter()
                .map(|key| {
                    let value = if *key == facet.key {
                        option.value.as_str()
                    } else {
                        applied(view, key)
                    };
                    (*key, value)
                })
                .collect();
            let _ = write!(
                b,
                r#"<a href="{href}"{current}>{label}<span class="st-count">{count}</span></a>"#,
                href = attr(&query(&pairs)),
                current = if option.selected {
                    r#" aria-current="true""#
                } else {
                    ""
                },
                label = text(&option.label),
                count = option.count,
            );
        }
        b.push_str("</div></details>");
    }
    let summary = if view.filtered {
        format!(
            "Showing {} of {}",
            view.listed,
            count(view.unfiltered, "run")
        )
    } else {
        let mut parts = vec![
            count(view.listed, "run"),
            format!("{} recorded a snapshot", view.recorded_snapshots),
        ];
        if view.failed > 0 {
            parts.push(format!("{} failed", view.failed));
        }
        if view.partial > 0 {
            parts.push(format!("{} partial", view.partial));
        }
        parts.join(" · ")
    };
    let _ = write!(
        b,
        r#"<span class="st-right muted" role="status">{}</span></div>"#,
        text(&summary)
    );
}

/// The last run, when it probably recorded no snapshot: its own row, first.
fn last_run_row(b: &mut String, last: &LastRunView, view: &RunsView) {
    let failed = last.has_failures();
    let (date, time) = date_and_time(last.started_at);
    let selected = view.selected.as_deref() == Some("last");
    let outcome = if failed {
        RunOutcome::Failed
    } else {
        RunOutcome::Succeeded
    };
    let label = format!(
        "{} (from the last run's record); recorded nothing (inferred)",
        outcome_word(outcome)
    );
    let _ = write!(
        b,
        r#"<tbody><tr class="st-rgroup"><th scope="rowgroup" colspan="13">Last run · started {date} · UTC</th></tr><tr class="{sel}{bad}"{current} data-run="last"><td><a href="{select}" aria-label="Show the last run in the side panel, {label}">{glyph}</a></td><td class="st-dim">last run</td><td><code title="{full}">{command}</code></td><td class="st-dim">—</td><td class="st-snap{kept_class}">{snap} {chip}</td><td>{built}</td><td>{kept}</td><td>{nfailed}</td><td>{nskipped}</td><td>{nrows}</td><td class="mono st-ts" title="When it started">{time}</td><td class="st-dim">{duration}</td><td class="st-dim">[user]</td></tr></tbody>"#,
        date = text(&date),
        select = attr(&runs_query(view, Some("last"))),
        label = attr(&label),
        sel = if selected { "selected" } else { "" },
        current = if selected {
            r#" aria-current="true""#
        } else {
            ""
        },
        bad = if failed { " failed" } else { "" },
        glyph = glyph(outcome, &label),
        command = text(&last.command_name),
        full = attr(&last.command),
        kept_class = if failed { " bad" } else { "" },
        snap = text(
            &last
                .last_good
                .map_or_else(|| "none".to_owned(), |s| format!("kept {s}"))
        ),
        chip = inferred_chip(NOTHING_NOTE),
        built = num_pill(Some(0), "built"),
        kept = NOT_RECORDED,
        nfailed = num_pill(Some(last.failures()), "failed"),
        nskipped = num_pill(Some(last.skipped.len()), "skipped"),
        nrows = missing(NO_JOURNAL),
        duration = missing(NO_JOURNAL),
        time = text(&time),
    );
}

#[allow(clippy::too_many_lines, reason = "one panel, built top to bottom")]
fn runs_side(b: &mut String, view: &RunsView) {
    let row = view
        .selected
        .as_deref()
        .and_then(|id| view.runs.iter().find(|r| r.run_id == id));
    let last = view.last_run.as_ref();
    b.push_str(r#"<aside class="st-side w320" aria-label="Selected run">"#);
    if let Some(row) = row {
        let time = row
            .at
            .map_or_else(|| "start not recorded".to_owned(), |at| date_and_time(at).1);
        let label = outcome_label(row);
        let _ = write!(
            b,
            r#"<div class="st-side-head"><div class="st-head-line">{glyph}<h2 class="mono">run {short}</h2><span class="st-outcome {outcome}" title="{note}">{outcome_upper}</span></div>{source}{inferred}<span class="muted">{command} · {target} · <span class="mono st-ts">{time}</span></span><span>{counts}</span>{totals}</div>"#,
            glyph = glyph(row.outcome, &label),
            short = text(&row.short_run_id),
            outcome = row.outcome.word(),
            note = attr(&row.outcome_note),
            outcome_upper = outcome_word(row.outcome).to_uppercase(),
            source = outcome_source(row),
            inferred = if row.outcome_inferred {
                format!(
                    r#"<span class="st-small">probably stopped: no event for over 10 minutes {}</span>"#,
                    inferred_chip(&row.outcome_note)
                )
            } else {
                String::new()
            },
            command = row.command.as_deref().map_or_else(
                || r#"<span title="Runs don't record their command yet">command not recorded</span>"#.to_owned(),
                |c| format!("<code>{}</code>", command_tokens(c))
            ),
            target = text(&target_text(row)),
            time = text(&time),
            counts = text(&run_counts(row)),
            totals = run_totals(row),
        );
        failed_nodes(b, &view.selected_nodes, "../");
        let ours = last.filter(|l| match row.snapshot {
            Some(id) => l.snapshot == Some(id),
            None => view.selected.as_deref() == Some(row.run_id.as_str()) && row.from_last_run,
        });
        match ours {
            // A run listed from its journal: what its journal says, then what to run.
            Some(last) if row.snapshot.is_none() && row.stats.is_some() => {
                state_panel(b, row);
                next_steps(b, last);
            }
            Some(last) => last_run_panels(
                b,
                last,
                row.snapshot,
                !view.selected_nodes.is_empty(),
                "../",
            ),
            None => state_panel(b, row),
        }
        let _ = write!(
            b,
            r#"<div class="st-side-foot"><a class="st-btn" href="runs/{href}">Open run</a><button type="button" class="st-btn" data-copy-url="../api/state/runs/{href}" aria-label="Copy run {short} as JSON">Copy as JSON</button></div>"#,
            href = attr(&enc(&row.run_id)),
            short = attr(&row.short_run_id),
        );
    } else if let Some(last) = last.filter(|l| l.recorded_nothing_inferred && l.outcome_known) {
        let failed = last.has_failures();
        let outcome = if failed {
            RunOutcome::Failed
        } else {
            RunOutcome::Succeeded
        };
        let _ = write!(
            b,
            r#"<div class="st-side-head"><div class="st-head-line">{glyph}<h2>Last run</h2><span class="st-outcome {o}">{upper}</span></div><span class="st-small" title="{source}">from the last run's record</span><span class="muted"><code>{command}</code> · started <span class="mono st-ts">{at}</span></span><span>{counts} {chip}</span>{full}</div>"#,
            glyph = glyph(
                outcome,
                &format!("{} (from the last run's record)", outcome_word(outcome))
            ),
            o = outcome.word(),
            upper = outcome_word(outcome).to_uppercase(),
            source = attr(LAST_RUN_NOTE),
            command = text(&last.command_name),
            at = text(&last.started_at.to_string()),
            counts = text(&format!(
                "recorded nothing · {} failed or not recorded · {} skipped · {} source tests failed",
                last.failed.len(),
                last.skipped.len(),
                last.failed_source_tests.len()
            )),
            chip = inferred_chip(NOTHING_NOTE),
            full = if last.command == last.command_name {
                String::new()
            } else {
                format!(
                    r#"<details class="st-full"><summary>Full command (values redacted)</summary><code>{}</code></details>"#,
                    text(&last.command)
                )
            },
        );
        last_run_panels(b, last, None, false, "../");
    } else {
        b.push_str(r#"<p class="st-pad muted">No run selected.</p>"#);
    }
    b.push_str("</aside>");
}

/// Where a run's outcome comes from, as a small line.
fn outcome_source(row: &RunRow) -> String {
    let (words, title) = match (row.outcome_from, &row.no_journal) {
        (OutcomeFrom::Journal, _) => ("from its run journal", row.outcome_note.as_str()),
        (OutcomeFrom::LastRun, _) => ("from the last run's record", LAST_RUN_NOTE),
        (_, Some(why)) => ("no run journal", why.as_str()),
        _ => ("outcome not stored", RECORDED_NOTE),
    };
    format!(
        r#"<span class="st-small" title="{}">{}</span>"#,
        attr(title),
        text(words)
    )
}

/// A run's time and rows, from its journal: `4.2s · 298 rows`, `at least 298 rows`.
fn run_totals(row: &RunRow) -> String {
    let Some(stats) = &row.stats else {
        return String::new();
    };
    let took = stats.duration.clone().unwrap_or_else(|| {
        if row.outcome == RunOutcome::Unfinished {
            "time — (still running, or stopped)".to_owned()
        } else {
            "time — (not recorded)".to_owned()
        }
    });
    let rows = match stats.rows_affected {
        None => format!(
            "rows — (not reported by {})",
            count(stats.rows_unreported, "node")
        ),
        Some(n) if stats.rows_at_least => format!(
            "at least {n} rows ({} didn't report)",
            count(stats.rows_unreported, "node")
        ),
        Some(n) => format!("{n} rows"),
    };
    format!(
        r#"<span class="muted">{}</span>"#,
        text(&format!("{took} · {rows}"))
    )
}

fn run_counts(row: &RunRow) -> String {
    let mut parts = vec![format!("{} built", row.built)];
    if let Some(kept) = row.kept {
        parts.push(format!("{kept} kept earlier build"));
    }
    if let Some(n) = row.failed {
        parts.push(if row.stats.is_some() {
            format!("{n} failed")
        } else {
            format!("{n} failed or not recorded")
        });
    }
    if let Some(n) = row.skipped {
        parts.push(format!("{n} skipped"));
    }
    if let Some(stats) = &row.stats {
        for (status, word) in [
            (NodeRunStatus::Unknown, "unknown"),
            (NodeRunStatus::Running, "running"),
            (NodeRunStatus::Queued, "queued"),
        ] {
            let n = stats.count(status);
            if n > 0 {
                parts.push(format!("{n} {word}"));
            }
        }
    }
    parts.join(" · ")
}

/// A run's state, when the last run's record doesn't say more.
fn state_panel(b: &mut String, row: &RunRow) {
    let (title, note, chip) = match (row.snapshot, row.kept_state) {
        (Some(s), _) => (
            format!("Snapshot {s}"),
            if row.stats.is_some() {
                "It records this run's successful builds; every other node keeps its last good build.".to_owned()
            } else {
                format!(
                    "{RECORDED_NOTE} Without a journal, failures are only kept for the last run, for its target."
                )
            },
            String::new(),
        ),
        (None, Some(good)) => (
            format!("Last good state when it ran: snapshot {good}"),
            format!(
                "This run recorded no snapshot. {}",
                no_snapshot_why(row.outcome)
            ),
            inferred_chip(NOTHING_NOTE),
        ),
        (None, None) => (
            "No good state before it".to_owned(),
            format!(
                "This run recorded no snapshot, and none was recorded before it. {}",
                no_snapshot_why(row.outcome)
            ),
            inferred_chip(NOTHING_NOTE),
        ),
    };
    let _ = write!(
        b,
        r#"<div class="st-side-sec"><h3 class="st-label">State</h3><span class="st-state">{SHIELD}{title}</span><span class="st-small">{note} {chip}</span></div>"#,
        title = text(&title),
        note = text(&note),
    );
}

/// Why a run recorded no snapshot, as far as its outcome says.
fn no_snapshot_why(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Failed | RunOutcome::Partial => {
            "A failed run never replaces the last good state."
        }
        RunOutcome::Unfinished => {
            "It may still be running: a run records its snapshot only when it ends."
        }
        RunOutcome::Succeeded => {
            "Its journal says it succeeded, but no listed snapshot has its id: its builds weren't recorded."
        }
        _ => "How it ended isn't known.",
    }
}

/// Failed and skipped nodes from a run's journal: each failed node's error as the
/// journal keeps it (redacted), and what stopped each skipped one.
fn failed_nodes(b: &mut String, nodes: &[NodeStatsView], root: &str) {
    let failed: Vec<&NodeStatsView> = nodes
        .iter()
        .filter(|n| n.status == NodeRunStatus::Error)
        .collect();
    if !failed.is_empty() {
        let _ = write!(
            b,
            r#"<div class="st-side-sec"><h3 class="st-label">{}</h3>"#,
            if failed.len() == 1 {
                "Failed node"
            } else {
                "Failed nodes"
            }
        );
        for node in failed {
            let _ = write!(
                b,
                r#"<div class="st-failed-node"><span class="st-bar" aria-hidden="true"></span>{name}<span class="st-small">{kind}{took}</span></div>"#,
                name = node_name(&node.node, &node.name, root),
                kind = text(node.kind.as_deref().unwrap_or("")),
                took = text(
                    &node
                        .took
                        .as_deref()
                        .map_or_else(String::new, |t| format!(" · after {t}"))
                ),
            );
            explanation_card(b, node, nodes);
        }
        b.push_str("</div>");
    }
    let skipped: Vec<&NodeStatsView> = nodes
        .iter()
        .filter(|n| n.status == NodeRunStatus::Skipped)
        .collect();
    if !skipped.is_empty() {
        b.push_str(r#"<div class="st-side-sec"><h3 class="st-label">Skipped</h3>"#);
        for node in skipped {
            let blocked: Vec<String> = node
                .blocked_by
                .iter()
                .map(|n| node_name(&n.node, &n.name, root))
                .collect();
            let _ = write!(
                b,
                r#"<span class="st-small">{} · {}</span>"#,
                node_name(&node.node, &node.name, root),
                if blocked.is_empty() {
                    "waited on a failed node".to_owned()
                } else {
                    format!("waited on {}", blocked.join(", "))
                }
            );
        }
        b.push_str("</div>");
    }
}

/// Explanation text, with its code spans as `<code>`: every part escaped.
fn explained_text(t: &ods_core::failure::Text) -> String {
    t.parts()
        .into_iter()
        .map(|(part, code)| {
            if code {
                format!("<code>{}</code>", text(part))
            } else {
                text(part).into_owned()
            }
        })
        .collect()
}

/// A failed node, explained (#323, board 9): what went wrong, how sure ODS is, why it
/// thinks so (or what it knows), where, what to try with commands to copy, the impact,
/// and the engine's own redacted message one click away. An unrecognised error gets no
/// cause, only facts.
fn explanation_card(b: &mut String, node: &NodeStatsView, nodes: &[NodeStatsView]) {
    use ods_core::failure::Confidence;
    let Some(e) = &node.explanation else {
        return error_box(b, node);
    };
    let recognised = e.confidence() != Confidence::NotRecognised;
    let (confidence, class) = match e.confidence() {
        Confidence::KnownPatternWithEvidence => ("known_pattern_with_evidence", "evidence"),
        Confidence::KnownPattern => ("known_pattern", "known"),
        _ => ("not_recognised", "unknown"),
    };
    let _ = write!(
        b,
        r#"<div class="st-explain" data-confidence="{confidence}"><div class="st-explain-chips"><span class="st-chip-kind">{chip}</span><span class="st-conf {class}" title="How sure ODS is">{label}</span></div><h3 class="st-explain-head">{head}</h3>"#,
        chip = text(&e.chip()),
        label = text(e.confidence().label()),
        head = explained_text(e.headline()),
    );
    if let Some(detail) = e.detail() {
        let _ = write!(
            b,
            r#"<p class="st-explain-detail">{}</p>"#,
            explained_text(detail)
        );
    }
    explanation_evidence(b, e, recognised);
    explanation_where(b, e);
    explanation_steps(b, e, recognised);
    explanation_impact(b, e, nodes);
    explanation_said(b, e, node, recognised);
    b.push_str("</div>");
}

/// Why ODS thinks so, or, for an unrecognised error, what it knows.
fn explanation_evidence(b: &mut String, e: &ods_core::failure::ErrorExplanation, recognised: bool) {
    if e.evidence().is_empty() {
        return;
    }
    let _ = write!(
        b,
        r#"<div class="st-explain-sec"><h4 class="st-label">{}</h4><ul>"#,
        if recognised {
            "Why ODS thinks so"
        } else {
            "What ODS knows"
        }
    );
    for item in e.evidence() {
        let _ = write!(
            b,
            r#"<li title="From {}">{}</li>"#,
            attr(item.source.label()),
            explained_text(&item.text)
        );
    }
    b.push_str("</ul></div>");
}

/// Where: the source file (and line, where ODS knows it), and the line the engine
/// reported in the code it ran.
fn explanation_where(b: &mut String, e: &ods_core::failure::ErrorExplanation) {
    let Some(at) = e.location() else {
        return;
    };
    b.push_str(r#"<div class="st-explain-sec"><h4 class="st-label">Where</h4>"#);
    if let Some(file) = &at.file {
        let _ = write!(
            b,
            r#"<div class="mono">{}</div>"#,
            text(
                &at.line
                    .map_or_else(|| file.clone(), |l| format!("{file}:{l}"))
            )
        );
    }
    let reported = match (at.reported_line, &at.compiled_file) {
        (Some(l), Some(f)) => Some(format!("reported at line {l} of the code it ran · {f}")),
        (Some(l), None) => Some(format!("reported at line {l} of the code it ran")),
        (None, Some(f)) => Some(format!("compiled: {f}")),
        (None, None) => None,
    };
    if let Some(reported) = reported {
        let _ = write!(b, r#"<span class="st-small">{}</span>"#, text(&reported));
    }
    b.push_str("</div>");
}

/// What to try, with a Copy button per command; an unrecognised error also shows the
/// planned agent.
fn explanation_steps(b: &mut String, e: &ods_core::failure::ErrorExplanation, recognised: bool) {
    if e.suggestions().is_empty() && recognised {
        return;
    }
    b.push_str(r#"<div class="st-explain-sec"><h4 class="st-label">What to try</h4><ol>"#);
    for s in e.suggestions() {
        let _ = write!(b, "<li>{}</li>", explained_text(&s.text));
    }
    b.push_str("</ol>");
    for command in e.suggestions().iter().flat_map(|s| &s.commands) {
        let _ = write!(
            b,
            r#"<div class="st-explain-cmd"><code>{}</code>{}</div>"#,
            text(command),
            copy_button(command, "Copy", &format!("Copy {command}"))
        );
    }
    if !recognised {
        b.push_str(r#"<button type="button" class="st-btn st-agent" disabled>Ask the ODS agent to investigate<span class="chip">Planned</span></button>"#);
    }
    b.push_str("</div>");
}

/// The nodes it blocked, and whether their last good builds are kept.
fn explanation_impact(
    b: &mut String,
    e: &ods_core::failure::ErrorExplanation,
    nodes: &[NodeStatsView],
) {
    let Some(impact) = e.impact() else {
        return;
    };
    let name = |id: &String| {
        nodes.iter().find(|n| &n.node == id).map_or_else(
            || id.rsplit('.').next().unwrap_or(id).to_owned(),
            |n| n.name.clone(),
        )
    };
    let blocked: Vec<String> = impact.blocked.iter().map(name).collect();
    let n = blocked.len();
    let kept = match impact.kept.len() {
        0 => String::new(),
        k if k == n && n == 1 => " Its last good build is kept.".to_owned(),
        k if k == n => " Their last good builds are kept.".to_owned(),
        _ => format!(
            " Last good builds are kept for {}.",
            impact.kept.iter().map(name).collect::<Vec<_>>().join(", ")
        ),
    };
    let _ = write!(
        b,
        r#"<div class="st-explain-impact"><strong>Impact:</strong> blocks {n} downstream node{s} ({names} {verb} skipped).{kept}</div>"#,
        s = if n == 1 { "" } else { "s" },
        names = text(&blocked.join(", ")),
        verb = if n == 1 { "was" } else { "were" },
        kept = text(&kept),
    );
}

/// The engine's own words, redacted, in a disclosure: open when ODS doesn't recognise
/// the error.
fn explanation_said(
    b: &mut String,
    e: &ods_core::failure::ErrorExplanation,
    node: &NodeStatsView,
    recognised: bool,
) {
    let Some(said) = e.engine_message() else {
        return;
    };
    let message = said
        .kind
        .as_deref()
        .and_then(|k| said.message.strip_prefix(k))
        .and_then(|rest| rest.strip_prefix(": "))
        .unwrap_or(&said.message);
    let _ = write!(
        b,
        r#"<details class="st-explain-said"{open}><summary>What {engine} said</summary><pre class="st-error real">{kind}{message}</pre><span class="st-small">Literal values and SQL removed.{at}</span></details>"#,
        open = if recognised { "" } else { " open" },
        engine = text(&said.engine),
        kind = said
            .kind
            .as_deref()
            .map_or_else(String::new, |k| format!("<strong>{}</strong>\n", text(k))),
        message = text(message),
        at = node
            .error
            .as_ref()
            .and_then(|e| e.details_at.as_deref())
            .map_or_else(String::new, |at| format!(
                " Full text: <code>{}</code>",
                text(at)
            )),
    );
}

/// A failed node's error, as the journal keeps it: never more.
fn error_box(b: &mut String, node: &NodeStatsView) {
    match &node.error {
        Some(error) => {
            let _ = write!(
                b,
                r#"<pre class="st-error real">{kind}{message}</pre><span class="st-small">Quoted values, numbers and SQL removed.{at}</span>"#,
                kind = error
                    .kind
                    .as_deref()
                    .map_or_else(String::new, |k| format!("<strong>{}</strong>\n", text(k))),
                message = text(&error.message),
                at = error
                    .details_at
                    .as_deref()
                    .map_or_else(String::new, |at| format!(
                        " Full message: <code>{}</code>",
                        text(at)
                    )),
            );
        }
        None => b.push_str(r#"<span class="st-small">Its error wasn't reported.</span>"#),
    }
}

/// The failed nodes, the state kept, and what to run next, from the last run's record.
/// `journaled`: the run's journal already lists its failed nodes with their errors.
fn last_run_panels(
    b: &mut String,
    last: &LastRunView,
    snapshot: Option<u64>,
    journaled: bool,
    root: &str,
) {
    if !last.failed.is_empty() && !journaled {
        b.push_str(r#"<div class="st-side-sec"><h3 class="st-label">Failed or not recorded</h3>"#);
        for node in &last.failed {
            let _ = write!(
                b,
                r#"<div class="st-failed-node"><span class="st-bar" aria-hidden="true"></span>{}</div>"#,
                node_name(&node.node, &node.name, root),
            );
        }
        b.push_str(r#"<span class="st-small">It failed, or ran but its build couldn't be recorded. Its error isn't known here: the run has no journal, which keeps each error's summary.</span></div>"#);
    }
    if !last.failed_source_tests.is_empty() {
        let names: Vec<String> = last
            .failed_source_tests
            .iter()
            .map(|n| node_name(&n.node, &n.name, root))
            .collect();
        let _ = write!(
            b,
            r#"<div class="st-side-sec"><h3 class="st-label">Source tests failed</h3><span class="st-small">{}: their readers build again once the tests pass.</span></div>"#,
            names.join(", ")
        );
    }
    if !last.skipped.is_empty() && !journaled {
        let names: Vec<String> = last
            .skipped
            .iter()
            .map(|n| node_name(&n.node, &n.name, root))
            .collect();
        let _ = write!(
            b,
            r#"<div class="st-side-sec"><h3 class="st-label">Skipped</h3><span class="st-small">Waited on a failed node: {}</span></div>"#,
            names.join(", ")
        );
    }
    let (title, state, chip) = match (snapshot, last.last_good) {
        (Some(s), _) => (
            format!("Snapshot {s}"),
            format!(
                "Snapshot {s} recorded this run's successful builds only: the nodes that failed or weren't recorded keep their last good build. It records this run's id."
            ),
            String::new(),
        ),
        (None, Some(good)) => (
            format!("Last good state: snapshot {good}"),
            if last.has_failures() {
                format!(
                    "Snapshot {good} kept: this run seems to have recorded nothing, and a failed run never replaces the last good state."
                )
            } else {
                format!("Snapshot {good} kept: this run seems to have recorded nothing.")
            },
            inferred_chip(NOTHING_NOTE),
        ),
        (None, None) => (
            "No good state yet".to_owned(),
            "No snapshot is recorded yet.".to_owned(),
            String::new(),
        ),
    };
    let _ = write!(
        b,
        r#"<div class="st-side-sec"><h3 class="st-label">State</h3><span class="st-state">{SHIELD}{title}</span><span class="st-small">{state} {chip}</span></div>"#,
        title = text(&title),
        state = text(&state),
    );
    next_steps(b, last);
}

/// What to run next, from the last run's record.
fn next_steps(b: &mut String, last: &LastRunView) {
    if !last.next.is_empty() {
        b.push_str(r#"<div class="st-side-sec"><h3 class="st-label">Suggested next step</h3>"#);
        for command in &last.next {
            command_box(b, command);
        }
        b.push_str(r#"<p class="st-small">Run it in your terminal; this dashboard is read-only.</p></div>"#);
    }
}

// ---------------------------------------------------------------------------- run

#[allow(clippy::too_many_lines, reason = "one page, built top to bottom")]
fn run_html(shell: &ShellView, view: &RunPageView, nodes_tab: bool, generation: u64) -> String {
    let run = &view.run;
    let mut b = String::with_capacity(32 * 1024);
    b.push_str(r#"<div class="st-split"><section class="st-main">"#);
    let _ = write!(
        b,
        r#"<div class="st-run-title"><h1 class="mono">run {short}</h1>{command}{target}<span class="st-right st-actions">{copy}{why}</span></div>"#,
        short = text(&run.short_run_id),
        // Long commands are cut to one line; the whole command is in the title.
        command = run.command.as_deref().map_or_else(String::new, |c| format!(
            r#"<code class="st-chip-cmd" title="{}">{}</code>"#,
            attr(&format!(
                "{c}\n\nThis run's command, from the last run's record."
            )),
            text(c)
        )),
        // Why builds were decided is only kept for runs that recorded a snapshot.
        why = if run.snapshot.is_some() {
            r##"<a class="st-btn" href="#built">Why these decisions</a>"##
        } else {
            ""
        },
        target = if run.target.is_some() {
            format!(
                r#"<span class="st-chip-target">{}</span>"#,
                text(&target_text(run))
            )
        } else {
            String::new()
        },
        copy = copy_button(
            &run.run_id,
            "Copy run id",
            &format!("Copy run id {}", run.run_id)
        ),
    );
    let icon = match run.outcome {
        RunOutcome::Succeeded => OK_ICON,
        RunOutcome::Failed => FAIL_ICON,
        RunOutcome::Partial => PARTIAL_ICON,
        _ => RECORDED_ICON,
    };
    let source = match (run.outcome_from, run.outcome) {
        (OutcomeFrom::Journal, _) if run.outcome_inferred => {
            format!(
                "probably stopped: no event for over 10 minutes {}",
                inferred_chip(&run.outcome_note)
            )
        }
        (OutcomeFrom::Journal, RunOutcome::Unfinished) => "no end in its journal yet".to_owned(),
        (OutcomeFrom::Journal, _) => "from its run journal".to_owned(),
        (OutcomeFrom::LastRun, _) => "from the last run's record".to_owned(),
        _ => "outcome not stored: no run journal".to_owned(),
    };
    let (wall, wall_note) = match (&run.duration, &run.stats) {
        (Some(d), _) => (
            text(d).into_owned(),
            "From its start to its finish, from the run's journal".to_owned(),
        ),
        (None, Some(_)) if run.outcome == RunOutcome::Unfinished => (
            MISSING.to_owned(),
            "Its journal doesn't say it finished: still running, or stopped".to_owned(),
        ),
        (None, Some(_)) => (
            MISSING.to_owned(),
            "Not recorded: its journal doesn't give both its start and its finish".to_owned(),
        ),
        (None, None) => (
            MISSING.to_owned(),
            "Not recorded: this run has no journal".to_owned(),
        ),
    };
    let _ = write!(
        b,
        r#"<div class="st-tiles4"><div class="st-tile" data-tile="outcome" title="{onote}"><span class="st-label">Outcome</span><span class="st-outcome-value {class}">{icon}{outcome}</span><span class="st-note">{source}</span></div><div class="st-tile" data-tile="built"><span class="st-label">Built</span><span class="st-value build">{built}</span>{built_note}</div><div class="st-tile" data-tile="kept" title="{knote}"><span class="st-label">Kept earlier build</span><span class="st-value">{kept}</span><span class="st-note">{kept_note}</span></div><div class="st-tile" data-tile="wall_clock" title="{wall_note}"><span class="st-label">Wall clock</span><span class="st-value{wall_class}">{wall}</span>{wall_why}</div></div>"#,
        onote = attr(&run.outcome_note),
        class = outcome_class(run.outcome),
        outcome = text(outcome_word(run.outcome)),
        built = run.built,
        built_note = if run.snapshot.is_none() {
            r#"<span class="st-note">no snapshot recorded</span>"#
        } else {
            ""
        },
        knote = attr(if run.stats.is_some() {
            KEPT_JOURNAL_NOTE
        } else {
            KEPT_NOTE
        }),
        kept = run.kept.map_or_else(
            || missing("not known: its journal never said when it started"),
            |k| k.to_string()
        ),
        kept_note = match (run.kept, run.stats.is_some(), run.kept_state) {
            (None, _, _) => "not known: when it started isn't recorded".to_owned(),
            (Some(_), true, Some(k)) if run.snapshot.is_none() => {
                format!("not in this run: kept from snapshot {k}")
            }
            (Some(_), true, _) => "not in this run: reused or not selected".to_owned(),
            (Some(_), false, _) => "reused, not selected, or failed".to_owned(),
        },
        wall_why = if run.duration.is_none() {
            format!(r#"<span class="st-note">{}</span>"#, text(&wall_note))
        } else {
            String::new()
        },
        wall_note = attr(&wall_note),
        wall_class = if run.duration.is_some() {
            " mono"
        } else {
            " st-dim"
        },
    );
    if let Some(stats) = &run.stats {
        run_totals_line(&mut b, view, stats);
    }
    let _ = write!(
        b,
        r#"<div class="st-notice">{INFO}<span><strong>State rule:</strong> if a node fails, it keeps its last good state. {}</span></div>"#,
        text(&view.state_rule)
    );
    let href = enc(&run.run_id);
    let _ = write!(
        b,
        r#"<nav class="st-tabs" aria-label="Run views"><a class="st-tab" href="{href}"{t}>Timeline</a><a class="st-tab" href="{href}?tab=nodes"{n}>Nodes</a><span class="st-tab disabled" aria-disabled="true" title="Run logs aren't kept by ODS: a failed node's error summary says where the full message is">Log<span class="chip">Planned</span></span><span class="st-tab disabled" aria-disabled="true" title="Planned">Graph<span class="chip">Planned</span></span></nav>"#,
        href = attr(&href),
        t = if nodes_tab {
            ""
        } else {
            r#" aria-current="page""#
        },
        n = if nodes_tab {
            r#" aria-current="page""#
        } else {
            ""
        },
    );
    if nodes_tab {
        nodes_table(&mut b, view);
    } else {
        timeline(&mut b, view);
    }
    b.push_str("</section>");
    run_side(&mut b, view);
    b.push_str("</div>");
    b.push_str(LIVE);
    // The dot says the outcome only when the journal or the last run's record gives it.
    let dot = match (run.outcome, run.outcome_from) {
        (_, OutcomeFrom::Snapshot) => "dot none",
        (RunOutcome::Succeeded, _) => "dot",
        (RunOutcome::Failed, _) => "dot bad",
        (RunOutcome::Partial, _) => "dot warn",
        _ => "dot none",
    };
    let status = match (run.snapshot, run.recorded_at) {
        (Some(id), Some(at)) => format!(
            r#"<span class="pill-snap"><span class="{dot}"></span>snapshot {id} · recorded {}</span>"#,
            text(&at.to_string())
        ),
        _ => format!(
            r#"<span class="pill-snap"><span class="{dot}"></span>no snapshot · started {}</span>"#,
            text(
                &run.at
                    .map_or_else(|| "at a time not recorded".to_owned(), |at| at.to_string())
            )
        ),
    };
    let title = format!("run {}", run.short_run_id);
    let frame = Frame {
        title: &title,
        crumbs: Some(crumbs(
            &[
                ("State", Some("../plan")),
                ("Runs", Some("../runs")),
                (&title, None),
            ],
            true,
        )),
        root: "../../",
        sub: Some("runs"),
        search: true,
        status: Some(status),
        css: CSS,
        js: JS,
    };
    framed(shell, &frame, &b, generation)
}

/// `no run journal: …` → `No run journal: …`.
fn sentence_case(line: &str) -> String {
    crate::dashboard::sentence(line)
}

/// The run's totals from its journal: nodes by status, rows, and how they were reported.
fn run_totals_line(
    b: &mut String,
    view: &RunPageView,
    stats: &crate::dashboard::journal::RunStatsView,
) {
    // A success is "tested" in a test run: the nodes say which.
    let word = |status: NodeRunStatus| {
        view.nodes.iter().find(|n| n.status == status).map_or_else(
            || crate::dashboard::journal::status_label(status, None),
            |n| n.status_label,
        )
    };
    let mut parts: Vec<String> = Vec::new();
    for (status, class) in [
        (NodeRunStatus::Success, "built"),
        (NodeRunStatus::Error, "failed"),
        (NodeRunStatus::Skipped, "skipped"),
        (NodeRunStatus::Unknown, "skipped"),
        (NodeRunStatus::Running, "built"),
        (NodeRunStatus::Queued, "skipped"),
    ] {
        let n = stats.count(status);
        if n > 0 || status == NodeRunStatus::Success {
            parts.push(format!(
                r#"<span><span class="st-num {class}">{n}</span>{}</span>"#,
                text(word(status))
            ));
        }
    }
    let rows = match stats.rows_affected {
        None => format!(
            r#"<span>rows {}<span class="muted">not reported by {}</span></span>"#,
            missing(&format!(
                "not reported by {}",
                count(stats.rows_unreported, "node")
            )),
            text(&count(stats.rows_unreported, "node"))
        ),
        Some(n) if stats.rows_at_least => format!(
            r#"<span>at least <strong class="mono">{n}</strong> rows ({} didn't report)</span>"#,
            text(&count(stats.rows_unreported, "node"))
        ),
        Some(n) => format!(r#"<span><strong class="mono">{n}</strong> rows</span>"#),
    };
    parts.push(rows);
    parts.push(format!(
        r#"<span class="st-right muted">{}</span>"#,
        if stats.live {
            "live stats: yes, each node's reported as it ran"
        } else {
            "live stats: no, rebuilt from the final results (no times, rows or threads)"
        }
    ));
    if stats.unreadable_lines > 0 {
        parts.push(format!(
            r#"<span class="muted" title="A newer format, or a last line cut short by a run that stopped mid-write">{} of its journal couldn't be read</span>"#,
            text(&count(stats.unreadable_lines, "line"))
        ));
    }
    let _ = write!(
        b,
        r#"<div class="st-totals" data-state="totals" aria-label="Run totals">{}</div>"#,
        parts.join("")
    );
}

/// The x of a time offset on the timeline.
fn tl_x(offset: u64, span: u64) -> usize {
    const T0: usize = 200;
    // Room is left on the right for every label, so all sit after their bars.
    const T1: usize = 640;
    let span = span.max(1);
    let x = u128::from(offset.min(span)) * (T1 - T0) as u128 / u128::from(span);
    T0 + usize::try_from(x).unwrap_or(0)
}

#[allow(clippy::too_many_lines, reason = "one chart, built top to bottom")]
fn timeline(b: &mut String, view: &RunPageView) {
    const ROW: usize = 28;
    const TOP: usize = 30;
    const START: usize = 200;
    const BUILD_FROM: usize = 272;
    const RIGHT: usize = 776;
    let rows = &view.timeline;
    let height = TOP + ROW * rows.len() + 6;
    // Timed from the journal: bars where each node started and finished.
    let span = rows
        .iter()
        .filter_map(|r| r.end_offset_ms)
        .max()
        .into_iter()
        .chain(view.run.stats.as_ref().and_then(|s| s.duration_ms))
        .max();
    let timed = span.is_some() && rows.iter().any(|r| r.start_offset_ms.is_some());
    let lanes = rows
        .iter()
        .filter(|r| r.built || r.status.is_some())
        .map(|r| r.lane)
        .max()
        .map_or(1, |l| l + 1);
    let width = ((RIGHT - BUILD_FROM - 90) / lanes).clamp(24, 260);
    let ran = rows.iter().filter(|r| r.status.is_some()).count();
    let _ = write!(
        b,
        r#"<section class="st-timeline" aria-label="Timeline"><svg width="100%" viewBox="0 0 796 {height}" role="img" aria-label="{label}">"#,
        label = attr(&if timed {
            format!(
                "Timeline of run {}: {} ran, {} kept an earlier build, over {}. The Nodes tab lists them as a table.",
                view.run.short_run_id,
                count(ran, "node"),
                count(rows.len() - ran, "node"),
                view.run
                    .duration
                    .as_deref()
                    .unwrap_or("a time not recorded"),
            )
        } else {
            format!(
                "Timeline of run {}: {} kept an earlier build, {} built. Times not recorded. The Nodes tab lists them as a table.",
                view.run.short_run_id,
                count(view.run.kept.unwrap_or(0), "node"),
                view.run.built
            )
        }),
    );
    for (i, r) in rows.iter().enumerate() {
        let y = TOP + ROW * i;
        if r.built || r.status.is_some() {
            let _ = write!(
                b,
                r#"<rect class="tl-sel{tint}" x="0" y="{y}" width="796" height="{ROW}"></rect>"#,
                tint = if r.status == Some(NodeRunStatus::Error) {
                    " failed"
                } else {
                    ""
                },
            );
        }
        let _ = write!(
            b,
            r#"<path class="tl-line" d="M0 {y2}H796"></path>"#,
            y2 = y + ROW
        );
    }
    let end_label = match (&view.run.duration, timed) {
        (Some(d), _) => d.clone(),
        (None, true) => format!(
            "{} so far",
            crate::dashboard::journal::duration(span.unwrap_or(0))
        ),
        (None, false) => MISSING.to_owned(),
    };
    let end_x = if timed { 640 } else { RIGHT };
    let _ = write!(
        b,
        r#"<g class="tl-head"><text x="0" y="18">Node</text><text x="{START}" y="18">start</text><text x="{end_x}" y="18" text-anchor="end">{end}</text></g><g class="tl-guide"><path d="M{START} 24V{height}"></path><path d="M{end_x} 24V{height}"></path></g>"#,
        end = text(&end_label),
    );
    for (i, r) in rows.iter().enumerate() {
        let y = TOP + ROW * i;
        // A skipped node never ran, whatever times its engine gave it: no bar.
        let ran = !matches!(
            r.status,
            Some(NodeRunStatus::Skipped | NodeRunStatus::Queued) | None
        );
        match (timed && ran, r.start_offset_ms, r.end_offset_ms.or(span)) {
            (true, Some(start), Some(end)) => timeline_bar(b, y, r, start, end, span.unwrap_or(1)),
            _ => timeline_row(b, y, r, width),
        }
    }
    b.push_str("</svg>");
    let journal_only = view.run.snapshot.is_none() && view.run.stats.is_some();
    let note = if journal_only {
        "Only the nodes this run selected are shown; the others kept their earlier build (counted under Kept earlier build)."
    } else if timed {
        "Bars: when each node started and finished, from the run's journal."
    } else if view.run.stats.is_some() {
        "Its journal has no times: order and waits come from lineage (inferred)."
    } else {
        "No run journal: order and waits come from lineage (inferred); per-node times weren't recorded."
    };
    let _ = write!(
        b,
        r#"<div class="st-legend"><span><span class="st-sw seed" aria-hidden="true"></span>seed</span><span><span class="st-sw kind" aria-hidden="true"></span>model or other node</span><span><span class="st-sw built"></span>Built</span><span><span class="st-sw failed"></span>Failed: kept its last good build</span><span><span class="st-sw skip"></span>Skipped: waited on a failed node</span><span title="{kept_title}"><span class="st-sw tick"></span>{kept_words}</span>{wait}<span class="st-right">{note}</span></div></section>"#,
        kept_title = if view.run.stats.is_some() {
            KEPT_JOURNAL_NOTE
        } else {
            KEPT_NOTE
        },
        kept_words = if view.run.stats.is_some() {
            "Kept earlier build: not in this run"
        } else {
            "Kept earlier build: reused, not selected, or failed"
        },
        wait = if timed {
            ""
        } else {
            r#"<span><span class="st-sw wait"></span>waits on upstream</span>"#
        },
    );
}

/// The name column of a timeline row: its kind mark and name.
fn timeline_name(b: &mut String, y: usize, r: &TimelineRow) {
    let kind = r.kind.as_deref().unwrap_or("not planned now");
    // Seeds are round, other kinds square: the shape says it, not only the colour.
    if kind == "seed" {
        let _ = write!(
            b,
            r#"<circle class="tl-kind seed" cx="3" cy="{cy}" r="3"><title>{kind}</title></circle>"#,
            cy = y + 14,
            kind = text(kind),
        );
    } else {
        let _ = write!(
            b,
            r#"<rect class="tl-kind other" x="0" y="{ky}" width="4" height="14"><title>{kind}</title></rect>"#,
            ky = y + 7,
            kind = text(kind),
        );
    }
    let _ = write!(
        b,
        r#"<text class="tl-name{bold}" x="12" y="{ty}"><title>{id}</title>{name}</text>"#,
        ty = y + 18,
        bold = if r.built || r.status.is_some() {
            " built"
        } else {
            ""
        },
        id = text(&r.node),
        name = text(&clip(&r.name, 24)),
    );
}

/// `a_very_long_name` cut to `n` characters, with `…`.
fn clip(name: &str, n: usize) -> String {
    if name.chars().count() <= n {
        name.to_owned()
    } else {
        let mut cut: String = name.chars().take(n - 1).collect();
        cut.push('…');
        cut
    }
}

/// A node the journal timed: a bar from its start to its finish, coloured by status.
fn timeline_bar(b: &mut String, y: usize, r: &TimelineRow, start: u64, end: u64, span: u64) {
    timeline_name(b, y, r);
    let bar_x = tl_x(start, span);
    let bar_w = tl_x(end, span).saturating_sub(bar_x).max(3);
    let status = r.status.unwrap_or(NodeRunStatus::Unknown);
    let (class, word) = match status {
        NodeRunStatus::Success => ("", "built"),
        NodeRunStatus::Error => (" failed", "failed"),
        NodeRunStatus::Running => (" running", "running"),
        _ => (" unknown", "unknown"),
    };
    let took = r.duration.clone().unwrap_or_else(|| MISSING.to_owned());
    let label = match status {
        NodeRunStatus::Error => format!("failed after {took}"),
        NodeRunStatus::Success => took.clone(),
        _ => format!("{word} · {took}"),
    };
    let _ = write!(
        b,
        r#"<rect class="tl-bar{class}" x="{bar_x}" y="{by}" width="{bar_w}" height="14" rx="3"><title>{title}</title></rect><text class="tl-dur" x="{tx}" y="{ty}">{label}</text>"#,
        by = y + 7,
        tx = bar_x + bar_w + 8,
        ty = y + 18,
        title = text(&format!(
            "{}: {word}, started +{}, took {took}",
            r.name,
            crate::dashboard::journal::duration(start)
        )),
        label = text(&label),
    );
}

/// One node of the timeline, at `y`, without times: its bar or kept mark.
fn timeline_row(b: &mut String, y: usize, r: &TimelineRow, width: usize) {
    const START: usize = 200;
    const BUILD_FROM: usize = 272;
    timeline_name(b, y, r);
    let status = r.status;
    let ran = r.built || matches!(status, Some(NodeRunStatus::Success | NodeRunStatus::Error));
    if ran {
        let bar_x = BUILD_FROM + r.lane * width;
        if r.lane > 0 {
            let _ = write!(
                b,
                r#"<path class="tl-wait" d="M{BUILD_FROM} {my}H{bar_x}"></path>"#,
                my = y + 14
            );
        }
        let failed = status == Some(NodeRunStatus::Error);
        let _ = write!(
            b,
            r#"<rect class="tl-bar{class}" x="{bar_x}" y="{by}" width="{width}" height="14" rx="3"></rect><text class="tl-dur" x="{tx}" y="{ty}"><title>{why}</title>{label}</text>"#,
            class = if failed { " failed" } else { " untimed" },
            by = y + 7,
            tx = bar_x + width + 8,
            ty = y + 18,
            why = text(if r.status.is_some() {
                "Not timed: its journal has no start or finish for it"
            } else {
                "Not recorded: this run has no journal"
            }),
            label = text(&match (failed, &r.duration) {
                (true, Some(took)) => format!("failed after {took}"),
                (true, None) => "failed · —".to_owned(),
                (false, Some(took)) => took.clone(),
                (false, None) => MISSING.to_owned(),
            }),
        );
    } else {
        let (word, title) = match status {
            Some(NodeRunStatus::Skipped) => {
                // Its own mark: a dashed box, not the kept tick.
                let _ = write!(
                    b,
                    r#"<rect class="tl-skip" x="{START}" y="{ky}" width="12" height="12" rx="2"></rect><text class="tl-kept-text" x="{tx}" y="{ty}"><title>skipped: waited on a failed node; kept its last good build</title>skipped</text>"#,
                    ky = y + 8,
                    tx = START + 18,
                    ty = y + 18,
                );
                return;
            }
            Some(s) if s != NodeRunStatus::Success => (
                crate::dashboard::journal::status_label(s, None),
                "its journal doesn't say how it ended".to_owned(),
            ),
            _ => (
                "kept",
                r.kept_from.as_deref().map_or_else(
                    || "kept an earlier build".to_owned(),
                    |f| format!("kept the build of run {f}"),
                ),
            ),
        };
        let _ = write!(
            b,
            r#"<rect class="tl-kept" x="{START}" y="{ky}" width="3" height="14" rx="1"></rect><text class="tl-kept-text" x="{tx}" y="{ty}"><title>{title}</title>{word}</text>"#,
            ky = y + 7,
            tx = START + 10,
            ty = y + 18,
            title = text(&title),
        );
    }
}

/// `2026-09-29T14:02:11.123Z` → `14:02:11`.
fn clock(at: ods_core::state::TimestampMs) -> String {
    let text = at.to_string();
    text.split_once('T')
        .map_or(text.clone(), |(_, t)| t.chars().take(8).collect())
}

/// A node's name on the Run page, with the link to where its relation is expected to
/// be in the warehouse's own UI, when there is one (#329).
fn run_node_name(view: &RunPageView, node: &str, name: &str) -> String {
    let link = view
        .relation_links
        .get(node)
        .map_or_else(String::new, |fields| {
            warehouse_link(fields, "st-wh", &|_| "↗".to_owned())
        });
    let name = node_name(node, name, "../../");
    if link.is_empty() {
        name
    } else {
        format!("{name} {link}")
    }
}

/// Why no node has a warehouse link, once under the table: the reason is the
/// warehouse's or the configuration's, the same for every row.
fn links_note(b: &mut String, view: &RunPageView) {
    if view
        .relation_links
        .values()
        .any(|f| f.relation_url.is_some())
    {
        return;
    }
    let reasons: std::collections::BTreeSet<String> = view
        .relation_links
        .values()
        .map(no_link_reason)
        .filter(|r| !r.is_empty())
        .collect();
    for reason in reasons {
        let _ = write!(
            b,
            r#"<p class="st-small st-links-note" data-state="no_relation_link">{}</p>"#,
            with_code(&reason)
        );
    }
}

#[allow(clippy::too_many_lines, reason = "one table, column by column")]
fn nodes_table(b: &mut String, view: &RunPageView) {
    if view.nodes.is_empty() {
        let _ = write!(
            b,
            r#"<section class="st-timeline" aria-label="Nodes"><div class="st-notice" data-state="no_journal">{INFO}<span><strong>Per-node stats weren't recorded for this run.</strong> {}.</span></div><table class="st-nodes"><thead><tr><th scope="col">Node</th><th scope="col">Type</th><th scope="col">This run</th><th scope="col">Build kept from</th></tr></thead><tbody>"#,
            text(&sentence_case(
                view.run.no_journal.as_deref().unwrap_or(NO_JOURNAL)
            ))
        );
        for r in &view.timeline {
            let _ = write!(
                b,
                r#"<tr><th scope="row">{name}</th><td class="st-dim">{kind}</td><td>{what}</td><td class="mono st-dim">{from}</td></tr>"#,
                name = run_node_name(view, &r.node, &r.name),
                kind = text(r.kind.as_deref().unwrap_or("—")),
                what = if r.built {
                    r#"<span class="st-num built">built</span>"#
                } else {
                    r#"<span class="st-num kept">kept</span>"#
                },
                from = text(
                    &r.kept_from
                        .as_deref()
                        .map_or_else(String::new, |f| f.chars().take(8).collect())
                ),
            );
        }
        b.push_str("</tbody></table>");
        links_note(b, view);
        b.push_str("</section>");
        return;
    }
    let why: std::collections::BTreeMap<&str, &str> = view
        .built
        .iter()
        .map(|w| (w.node.as_str(), w.why.as_str()))
        .collect();
    b.push_str(r#"<section class="st-timeline" aria-label="Nodes"><table class="st-nodes st-stats"><colgroup><col style="width:21%"><col style="width:12%"><col style="width:10%"><col style="width:12%"><col style="width:11%"><col style="width:10%"><col style="width:10%"><col></colgroup><thead><tr><th scope="col">Node</th><th scope="col">Status</th><th scope="col">Started</th><th scope="col">Took</th><th scope="col" title="Rows affected, as the adapter reported them">Rows</th><th scope="col">Thread</th><th scope="col">Tests</th><th scope="col">Why it ran</th></tr></thead><tbody>"#);
    for n in &view.nodes {
        let status_class = match n.status {
            NodeRunStatus::Success => "built",
            NodeRunStatus::Error => "failed",
            _ => "skipped",
        };
        let took = match (&n.took, &n.compile, &n.execute) {
            (Some(t), Some(c), Some(e)) => format!(
                r#"<span class="mono">{}</span><span class="st-small">compile {}</span><span class="st-small">execute {}</span>"#,
                text(t),
                text(c),
                text(e)
            ),
            (Some(t), _, _) => format!(r#"<span class="mono">{}</span>"#, text(t)),
            (None, _, _) if n.status == NodeRunStatus::Running => missing_said("still running"),
            (None, _, _) if matches!(n.status, NodeRunStatus::Skipped | NodeRunStatus::Queued) => {
                missing_said("didn't run")
            }
            (None, _, _) => missing_said("not timed"),
        };
        let rows = match (n.rows_affected, n.rows_missing) {
            (Some(r), _) => format!(r#"<span class="mono">{r}</span>"#),
            (None, why) => {
                let why = why.unwrap_or("not reported");
                // The reason in brief beside the dash, in full on hover.
                let brief = if why.starts_with("not reported") {
                    "not reported"
                } else if why.starts_with("not recorded") {
                    "not recorded"
                } else {
                    why
                };
                format!(
                    r#"{}<span class="st-why" title="{}">{}</span>"#,
                    missing(why),
                    attr(why),
                    text(brief)
                )
            }
        };
        let extras: Vec<String> = n.adapter.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let tests = n.tests.map_or_else(
            || {
                if matches!(n.status, NodeRunStatus::Skipped | NodeRunStatus::Queued) {
                    missing_said("didn't run")
                } else {
                    missing_said("none ran on it")
                }
            },
            |t| {
                let mut parts = vec![format!("{} passed", t.passed)];
                if t.failed > 0 {
                    parts.push(format!("{} failed", t.failed));
                }
                if t.warned > 0 {
                    parts.push(format!("{} warned", t.warned));
                }
                if t.skipped > 0 {
                    parts.push(format!("{} skipped", t.skipped));
                }
                text(&parts.join(" · ")).into_owned()
            },
        );
        let _ = write!(
            b,
            r#"<tr class="{row_class}" data-node="{id}" data-status="{status}"><th scope="row">{name}<span class="st-small">{kind}</span></th><td><span class="st-num {status_class}">{label}</span>{kept_note}</td><td class="mono" title="{started_full}">{started}</td><td>{took}</td><td>{rows}{extras}</td><td>{thread}</td><td>{tests}</td><td><span class="st-why-ran">{why}</span></td></tr>"#,
            row_class = if n.status == NodeRunStatus::Error {
                "failed"
            } else {
                ""
            },
            id = attr(&n.node),
            status = attr(n.status_label),
            name = run_node_name(view, &n.node, &n.name),
            kind = text(n.kind.as_deref().unwrap_or("")),
            label = text(n.status_label),
            // Failed and skipped nodes keep their last good build (AGENTS rule 5).
            kept_note = match n.status {
                NodeRunStatus::Error => {
                    r#"<span class="st-why">kept its last good build</span>"#.to_owned()
                }
                NodeRunStatus::Skipped => {
                    let names: Vec<&str> = n.blocked_by.iter().map(|r| r.name.as_str()).collect();
                    format!(
                        r#"<span class="st-why">{}; kept its last good build</span>"#,
                        text(&if names.is_empty() {
                            "waited on a failed node".to_owned()
                        } else {
                            format!("waited on {}", names.join(", "))
                        })
                    )
                }
                _ => String::new(),
            },
            started_full = attr(
                &n.started_at
                    .map_or_else(|| "not recorded".to_owned(), |t| t.to_string())
            ),
            // A skipped node didn't run, whatever time its engine gave it.
            started = match (n.status, n.started_at) {
                (NodeRunStatus::Skipped | NodeRunStatus::Queued, _) => missing_said("didn't run"),
                (_, Some(t)) => text(&clock(t)).into_owned(),
                (_, None) => missing_said("not recorded"),
            },
            extras = if extras.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<span class="st-small" title="Reported by the adapter">{}</span>"#,
                    text(&extras.join(" · "))
                )
            },
            thread = n.thread.as_deref().map_or_else(
                || if matches!(n.status, NodeRunStatus::Skipped | NodeRunStatus::Queued) {
                    missing_said("didn't run")
                } else {
                    missing_said("not said")
                },
                |t| text(t).into_owned(),
            ),
            // Why it was selected, from the snapshots; its outcome is in Status.
            why = why.get(n.node.as_str()).map_or_else(
                || missing_said("not recorded: snapshots keep why only for the builds they record"),
                |w| text(w).into_owned(),
            ),
        );
        if n.status == NodeRunStatus::Error {
            b.push_str(r#"<tr class="failed st-err-row"><td colspan="8">"#);
            explanation_card(b, n, &view.nodes);
            b.push_str("</td></tr>");
        }
    }
    let ran: std::collections::BTreeSet<&str> =
        view.nodes.iter().map(|n| n.node.as_str()).collect();
    for r in view
        .timeline
        .iter()
        .filter(|r| !ran.contains(r.node.as_str()))
    {
        let _ = write!(
            b,
            r#"<tr data-node="{id}" data-status="kept"><th scope="row">{name}<span class="st-small">{kind}</span></th><td><span class="st-num kept">kept</span></td><td colspan="6" class="st-small">{from}</td></tr>"#,
            id = attr(&r.node),
            name = run_node_name(view, &r.node, &r.name),
            kind = text(r.kind.as_deref().unwrap_or("")),
            from = text(&r.kept_from.as_deref().map_or_else(
                || "not in this run: kept an earlier build".to_owned(),
                |f| format!(
                    "not in this run: kept the build of run {}",
                    f.chars().take(8).collect::<String>()
                )
            )),
        );
    }
    b.push_str("</tbody></table>");
    links_note(b, view);
    b.push_str("</section>");
}

#[allow(clippy::too_many_lines, reason = "one panel, built top to bottom")]
fn run_side(b: &mut String, view: &RunPageView) {
    let run = &view.run;
    let _ = write!(
        b,
        r#"<aside class="st-side w340" aria-label="Run details"><div class="st-side-sec"><h2>Run details</h2><dl class="st-dl"><dt>Command</dt><dd>{command}</dd><dt>Target</dt><dd>{target}</dd><dt>Snapshot</dt><dd>{snap}</dd><dt>Compared with</dt><dd>{compared}</dd><dt>Recorded</dt><dd class="mono st-ts">{recorded}</dd><dt>Started</dt><dd class="mono st-ts">{started}</dd><dt>Finished</dt><dd class="mono st-ts">{finished}</dd><dt>Run journal</dt><dd>{journal}</dd><dt>Triggered by</dt><dd class="st-dim">[user]</dd></dl></div>"#,
        command = run.command.as_deref().map_or_else(
            || r#"<span class="st-dim" title="Runs don't record their command yet">not recorded</span>"#.to_owned(),
            // Only ever the run whose id the last run's record has.
            |c| format!(
                r#"<code>{}</code> <span class="st-small" title="{}">this run's command, from the last run's record</span>"#,
                command_tokens(c),
                attr(LAST_RUN_NOTE)
            ),
        ),
        target = text(&target_text(run)),
        snap = match (run.snapshot, run.replaces) {
            (Some(s), Some(p)) => text(&format!("{s} (replaces {p})")).into_owned(),
            (Some(s), None) => text(&format!("{s} (the first)")).into_owned(),
            (None, _) => format!(
                "none: {} {}",
                text(&run.kept_state.map_or_else(|| "nothing recorded yet".to_owned(), |k| format!("snapshot {k} kept"))),
                inferred_chip(NOTHING_NOTE)
            ),
        },
        compared = view.compared_with.as_ref().map_or_else(
            || if run.snapshot.is_some() {
                "nothing: first recorded run".to_owned()
            } else {
                "nothing: it recorded no snapshot".to_owned()
            },
            |c| format!(
                r#"snapshot {} · run <a class="mono" href="{}" title="{}">{}</a>"#,
                c.snapshot,
                attr(&enc(&c.run_id)),
                attr(&c.run_id),
                text(&c.short_run_id)
            ),
        ),
        recorded = run.recorded_at.map_or_else(|| missing_said("it recorded no snapshot"), |t| text(&t.to_string()).into_owned()),
        // To the second, as Recorded is.
        started = run.stats.as_ref().and_then(|s| s.started_at).map_or_else(
            || missing_said(if run.stats.is_some() { "its journal doesn't say" } else { "no run journal" }),
            |t| text(&t.to_seconds().to_string()).into_owned()
        ),
        finished = run.stats.as_ref().and_then(|s| s.finished_at).map_or_else(
            || missing_said(if run.outcome == RunOutcome::Unfinished {
                "running, or stopped without finishing"
            } else if run.stats.is_some() {
                "its journal doesn't say"
            } else {
                "no run journal"
            }),
            |t| text(&t.to_seconds().to_string()).into_owned()
        ),
        journal = match &run.stats {
            Some(s) if s.live => "kept · live stats: yes".to_owned(),
            Some(_) => "kept · live stats: no (rebuilt from final results)".to_owned(),
            None => format!(r#"<span title="{}">none</span>"#, attr(run.no_journal.as_deref().unwrap_or(NO_JOURNAL))),
        },
    );
    failed_nodes(b, &view.nodes, "../../");
    b.push_str(r#"<div class="st-side-sec" id="built" tabindex="-1"><h2>Built in this run</h2>"#);
    if view.built.is_empty() {
        b.push_str(if run.snapshot.is_some() {
            r#"<span class="st-small">Nothing: every recorded node kept an earlier build (e.g. a run that only recorded tests).</span>"#
        } else {
            r#"<span class="st-small">Nothing recorded: this run recorded no snapshot.</span>"#
        });
    }
    for w in &view.built {
        let _ = write!(
            b,
            r#"<div class="st-built">{name}<span class="muted">{why}</span></div>"#,
            name = node_name(&w.node, &w.name, "../../"),
            why = text(&w.why),
        );
    }
    b.push_str("</div>");
    if let Some(last) = &view.last_run {
        if run.snapshot.is_none() && run.stats.is_some() {
            // The state rule above says what its journal says; only what to run next.
            next_steps(b, last);
        } else {
            let journaled = view
                .nodes
                .iter()
                .any(|n| matches!(n.status, NodeRunStatus::Error | NodeRunStatus::Skipped));
            last_run_panels(b, last, run.snapshot, journaled, "../../");
        }
    }
    let _ = write!(
        b,
        r#"<div class="st-side-sec"><h2>Earlier runs{on}</h2>"#,
        on = run
            .target
            .as_ref()
            .map_or_else(String::new, |t| format!(" on {}", text(&t.name))),
    );
    if view.earlier.is_empty() {
        b.push_str(r#"<span class="st-small">None: this is the first listed run.</span>"#);
    }
    for e in &view.earlier {
        let time =
            e.at.map_or_else(|| "time not recorded".to_owned(), |at| date_and_time(at).1);
        let head = e
            .snapshot
            .map_or_else(|| "no snapshot".to_owned(), |s| format!("snapshot {s}"));
        let mut counts = vec![format!("{} built", e.built)];
        if let Some(k) = e.kept {
            counts.push(format!("{k} kept"));
        }
        if let Some(f) = e.failed.filter(|f| *f > 0) {
            counts.push(format!("{f} failed"));
        }
        if let Some(d) = &e.duration {
            counts.push(d.clone());
        }
        let _ = write!(
            b,
            r#"<a class="st-earlier" href="{href}"><span class="st-earlier-top"><span>{head} · <span class="mono">{short}</span></span><span class="st-o {o}">{word}</span></span><span class="muted">{counts} · <span class="mono st-ts">{time}</span></span></a>"#,
            href = attr(&enc(&e.run_id)),
            head = text(&head),
            short = text(&e.short_run_id),
            o = e.outcome.word(),
            word = text(outcome_word(e.outcome)),
            counts = text(&counts.join(", ")),
            time = text(&time),
        );
    }
    let _ = write!(
        b,
        r#"</div><div class="st-side-foot"><button type="button" class="st-btn" data-copy-url="../../api/state/runs/{href}" aria-label="Copy run {short} as JSON">Copy as JSON</button><span class="st-btn disabled" aria-disabled="true" title="Run logs aren't kept by ODS: a failed node's error summary says where the full message is">Download log<span class="chip">Planned</span></span></div></aside>"#,
        href = attr(&enc(&run.run_id)),
        short = attr(&run.short_run_id),
    );
}

fn missing_run_html(shell: &ShellView, run: &str, generation: u64) -> String {
    let body = format!(
        r#"<div class="content"><section class="card empty" data-state="no_run"><h2>No such run</h2><p>No listed run has the id <code>{}</code>. Only the newest runs are listed; <code>ods state history</code> lists them all.</p><p><a href="../runs">All runs</a></p></section></div>"#,
        text(run)
    );
    let frame = Frame {
        title: "Run not found",
        crumbs: Some(crumbs(
            &[
                ("State", Some("../plan")),
                ("Runs", Some("../runs")),
                ("not found", None),
            ],
            false,
        )),
        root: "../../",
        sub: Some("runs"),
        search: true,
        status: None,
        css: CSS,
        js: JS,
    };
    framed(shell, &frame, &body, generation)
}
