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

use crate::dashboard::state::{
    ChainLine, LastRunView, PlanRow, PlanView, RunFilter, RunOutcome, RunPageView, RunRow,
    RunsView, TimelineRow, WhyView, count, date_and_time,
};
use crate::dashboard::{CommandHint, EmptyState, ShellView, StateStatus};
use crate::home::{Frame, framed};
use crate::server::Shared;

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
const RECORDED_ICON: &str = r#"<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-dasharray="3 3" aria-hidden="true"><circle cx="12" cy="12" r="9"></circle></svg>"#;

/// What a recorded run's outcome does and doesn't say (as on Home).
const RECORDED_NOTE: &str = "Recorded: its successful builds are the new state. Whether other nodes failed isn't stored for this run.";
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
        RunOutcome::Succeeded => "succeeded",
        RunOutcome::Failed => "failed",
        RunOutcome::Recorded => "recorded",
    }
}

/// A Copy button for `value`; `what` names it for screen readers ("Copy run id").
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
fn outcome_label(outcome: RunOutcome, from_last_run: bool) -> String {
    match (outcome, from_last_run) {
        (RunOutcome::Recorded, _) => "recorded: outcome not stored".to_owned(),
        (o, true) => format!("{} (from the last run's record)", outcome_word(o)),
        (o, false) => outcome_word(o).to_owned(),
    }
}

fn glyph(outcome: RunOutcome, label: &str) -> String {
    let (class, mark) = match outcome {
        RunOutcome::Succeeded => ("ok", "✓"),
        RunOutcome::Failed => ("bad", "✕"),
        RunOutcome::Recorded => ("rec", ""),
    };
    format!(
        r#"<span class="st-glyph {class}" aria-hidden="true" title="{}">{mark}</span>"#,
        attr(label),
    )
}

fn num_pill(n: Option<usize>, class: &str) -> String {
    match n {
        Some(n) => format!(r#"<span class="st-num {class}">{n}</span>"#),
        None => NOT_RECORDED.to_owned(),
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

const RUN_COLUMNS: &str = r#"<thead><tr><th scope="col"><span class="sr-only">Outcome</span></th><th scope="col">Run</th><th scope="col">Command</th><th scope="col">Target</th><th scope="col">Snapshot</th><th scope="col">Built</th><th scope="col" title="Kept earlier build: reused, not selected, or failed">Kept</th><th scope="col" title="Failed, or ran but not recorded">Failed</th><th scope="col">Skipped</th><th scope="col">Time</th><th scope="col">Duration</th><th scope="col">Triggered by</th></tr></thead>"#;

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
        r#"<div class="st-legend"><span><span class="st-num built">n</span>built</span><span title="{kept}"><span class="st-num kept">n</span>kept earlier build: reused, not selected, or failed</span><span><span class="st-num failed">n</span>failed or not recorded</span><span><span class="st-num skipped">n</span>skipped: waited on a failed node</span><span><span class="st-glyph rec" aria-hidden="true"></span>recorded: outcome not stored</span><span>{NOT_RECORDED} not recorded</span><span class="st-right">Duration and user are not recorded yet.</span></div>"#,
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
    if last.failed.is_empty() {
        String::new()
    } else {
        text(&format!(
            "; {} failed or weren't recorded",
            last.failed
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .into_owned()
    }
}

/// The recorded runs, one table body per day.
fn run_rows(b: &mut String, view: &RunsView) {
    let mut day = String::new();
    for row in &view.runs {
        let (date, time) = date_and_time(row.recorded_at);
        if date != day {
            if !day.is_empty() {
                b.push_str("</tbody>");
            }
            let _ = write!(
                b,
                r#"<tbody><tr class="st-rgroup"><th scope="rowgroup" colspan="12">{} · UTC</th></tr>"#,
                text(&date)
            );
            day = date;
        }
        let selected = view.selected.as_deref() == Some(row.run_id.as_str());
        let label = outcome_label(row.outcome, row.from_last_run);
        let _ = write!(
            b,
            r#"<tr class="{sel}{bad}"{current} data-run="{id}"><td><a href="{select}" aria-label="{select_label}">{glyph}</a></td><td><a class="mono st-runid" href="runs/{href}" title="run {id}">{short}</a></td><td>{command}</td><td class="st-dim">{target}</td><td class="st-snap">{snap}</td><td>{built}</td><td title="{kept_note}">{kept}</td><td>{failed}</td><td>{skipped}</td><td class="mono st-ts" title="When its snapshot was recorded">{time}</td><td class="st-dim">[duration]</td><td class="st-dim">[user]</td></tr>"#,
            sel = if selected { "selected" } else { "" },
            bad = if row.outcome == RunOutcome::Failed {
                " failed"
            } else {
                ""
            },
            current = if selected {
                r#" aria-current="true""#
            } else {
                ""
            },
            id = attr(&row.run_id),
            select = attr(&runs_query(view, Some(&row.run_id))),
            select_label = attr(&format!(
                "Show run {} in the side panel, {label}",
                row.short_run_id
            )),
            glyph = glyph(row.outcome, &label),
            href = attr(&enc(&row.run_id)),
            short = text(&row.short_run_id),
            command = row.command.as_deref().map_or_else(
                || r#"<span class="st-na" aria-label="not recorded" title="Snapshots don't record their command yet">—</span>"#.to_owned(),
                |c| format!(r#"<code title="{}">{}</code>"#, attr(LAST_RUN_NOTE), text(c)),
            ),
            target = text(&target_text(row)),
            snap = row.snapshot,
            built = num_pill(Some(row.built), "built"),
            kept_note = attr(KEPT_NOTE),
            kept = num_pill(Some(row.kept), "kept"),
            failed = num_pill(row.failed, "failed"),
            skipped = num_pill(row.skipped, "skipped"),
            time = text(&time),
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
    let failed = !last.failed.is_empty() || !last.skipped.is_empty();
    let (date, time) = date_and_time(last.started_at);
    let selected = view.selected.as_deref() == Some("last");
    let outcome = if failed {
        RunOutcome::Failed
    } else {
        RunOutcome::Succeeded
    };
    let label = format!(
        "{}; recorded nothing (inferred)",
        outcome_label(outcome, true)
    );
    let _ = write!(
        b,
        r#"<tbody><tr class="st-rgroup"><th scope="rowgroup" colspan="12">Last run · started {date} · UTC</th></tr><tr class="{sel}{bad}"{current} data-run="last"><td><a href="{select}" aria-label="Show the last run in the side panel, {label}">{glyph}</a></td><td class="st-dim">last run</td><td><code title="{full}">{command}</code></td><td class="st-dim">—</td><td class="st-snap{kept_class}">{snap} {chip}</td><td>{built}</td><td>{kept}</td><td>{nfailed}</td><td>{nskipped}</td><td class="mono st-ts" title="When it started">started {time}</td><td class="st-dim">[duration]</td><td class="st-dim">[user]</td></tr></tbody>"#,
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
        nfailed = num_pill(Some(last.failed.len()), "failed"),
        nskipped = num_pill(Some(last.skipped.len()), "skipped"),
        time = text(&time),
    );
}

fn runs_side(b: &mut String, view: &RunsView) {
    let row = view
        .selected
        .as_deref()
        .and_then(|id| view.runs.iter().find(|r| r.run_id == id));
    let last = view.last_run.as_ref();
    b.push_str(r#"<aside class="st-side w320" aria-label="Selected run">"#);
    if let Some(row) = row {
        let (_, time) = date_and_time(row.recorded_at);
        let label = outcome_label(row.outcome, row.from_last_run);
        let _ = write!(
            b,
            r#"<div class="st-side-head"><div class="st-head-line">{glyph}<h2 class="mono">run {short}</h2><span class="st-outcome {outcome}">{outcome_upper}</span></div>{source}<span class="muted">{command} · {target} · <span class="mono st-ts">{time}</span></span><span>{counts}</span></div>"#,
            glyph = glyph(row.outcome, &label),
            short = text(&row.short_run_id),
            outcome = outcome_word(row.outcome),
            outcome_upper = outcome_word(row.outcome).to_uppercase(),
            source = if row.from_last_run {
                format!(
                    r#"<span class="st-small" title="{}">from the last run's record</span>"#,
                    attr(LAST_RUN_NOTE)
                )
            } else {
                String::new()
            },
            command = row.command.as_deref().map_or_else(
                || r#"<span title="Snapshots don't record their command yet">command not recorded</span>"#.to_owned(),
                |c| format!("<code>{}</code>", text(c))
            ),
            target = text(&target_text(row)),
            time = text(&time),
            counts = text(&run_counts(row)),
        );
        match last.filter(|l| l.snapshot == Some(row.snapshot)) {
            Some(last) => last_run_panels(b, last, Some(row.snapshot), "../"),
            None => {
                let _ = write!(
                    b,
                    r#"<div class="st-side-sec"><h3 class="st-label">State</h3><span class="st-state">{SHIELD}Snapshot {snap}</span><span class="st-small">{note}</span></div>"#,
                    snap = row.snapshot,
                    note = text(&format!(
                        "{RECORDED_NOTE} Failures are only kept for the last run, for its target."
                    )),
                );
            }
        }
        let _ = write!(
            b,
            r#"<div class="st-side-foot"><a class="st-btn" href="runs/{href}">Open run</a><button type="button" class="st-btn" data-copy-url="../api/state/runs/{href}" aria-label="Copy run {short} as JSON">Copy as JSON</button></div>"#,
            href = attr(&enc(&row.run_id)),
            short = attr(&row.short_run_id),
        );
    } else if let Some(last) = last.filter(|l| l.recorded_nothing_inferred && l.outcome_known) {
        let failed = !last.failed.is_empty() || !last.skipped.is_empty();
        let outcome = if failed {
            RunOutcome::Failed
        } else {
            RunOutcome::Succeeded
        };
        let _ = write!(
            b,
            r#"<div class="st-side-head"><div class="st-head-line">{glyph}<h2>Last run</h2><span class="st-outcome {o}">{upper}</span></div><span class="st-small" title="{source}">from the last run's record</span><span class="muted"><code>{command}</code> · started <span class="mono st-ts">{at}</span></span><span>{counts} {chip}</span>{full}</div>"#,
            glyph = glyph(outcome, &outcome_label(outcome, true)),
            o = outcome_word(outcome),
            upper = outcome_word(outcome).to_uppercase(),
            source = attr(LAST_RUN_NOTE),
            command = text(&last.command_name),
            at = text(&last.started_at.to_string()),
            counts = text(&format!(
                "recorded nothing · {} failed or not recorded · {} skipped",
                last.failed.len(),
                last.skipped.len()
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
        last_run_panels(b, last, None, "../");
    } else {
        b.push_str(r#"<p class="st-pad muted">No run selected.</p>"#);
    }
    b.push_str("</aside>");
}

fn run_counts(row: &RunRow) -> String {
    let mut parts = vec![
        format!("{} built", row.built),
        format!("{} kept earlier build", row.kept),
    ];
    if let Some(n) = row.failed {
        parts.push(format!("{n} failed or not recorded"));
    }
    if let Some(n) = row.skipped {
        parts.push(format!("{n} skipped"));
    }
    parts.join(" · ")
}

/// The failed nodes, the state kept, and what to run next, from the last run's record.
fn last_run_panels(b: &mut String, last: &LastRunView, snapshot: Option<u64>, root: &str) {
    if !last.failed.is_empty() {
        b.push_str(r#"<div class="st-side-sec"><h3 class="st-label">Failed or not recorded</h3>"#);
        for node in &last.failed {
            let _ = write!(
                b,
                r#"<div class="st-failed-node"><span class="st-bar" aria-hidden="true"></span>{}</div>"#,
                node_name(&node.node, &node.name, root),
            );
        }
        b.push_str(r#"<pre class="st-error" title="Error output isn't recorded yet">[error excerpt]<span class="chip">Planned</span></pre><span class="st-small">It failed, or ran but its build couldn't be recorded. The run log isn't recorded yet.</span></div>"#);
    }
    if !last.skipped.is_empty() {
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
            format!(
                "Snapshot {good} kept: this run seems to have recorded nothing, and a failed run never replaces the last good state."
            ),
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
        r##"<div class="st-run-title"><h1 class="mono">run {short}</h1>{command}<span class="st-chip-target">{target}</span><span class="st-right st-actions">{copy}<a class="st-btn" href="#built">Why these decisions</a></span></div>"##,
        short = text(&run.short_run_id),
        command = run.command.as_deref().map_or_else(String::new, |c| format!(
            r#"<code class="st-chip-cmd" title="{}">{}</code>"#,
            attr(LAST_RUN_NOTE),
            text(c)
        )),
        target = text(&target_text(run)),
        copy = copy_button(
            &run.run_id,
            "Copy run id",
            &format!("Copy run id {}", run.run_id)
        ),
    );
    let (icon, class) = match run.outcome {
        RunOutcome::Succeeded => (OK_ICON, "ok"),
        RunOutcome::Failed => (FAIL_ICON, "bad"),
        RunOutcome::Recorded => (RECORDED_ICON, "rec"),
    };
    let _ = write!(
        b,
        r#"<div class="st-tiles4"><div class="st-tile" data-tile="outcome" title="{onote}"><span class="st-label">Outcome</span><span class="st-outcome-value {class}">{icon}{outcome}</span>{source}</div><div class="st-tile" data-tile="built"><span class="st-label">Built</span><span class="st-value build">{built}</span></div><div class="st-tile" data-tile="kept" title="{knote}"><span class="st-label">Kept earlier build</span><span class="st-value">{kept}</span></div><div class="st-tile" data-tile="wall_clock"><span class="st-label">Wall clock</span><span class="st-value st-dim" title="Durations aren't recorded yet">[wall clock]</span></div></div>"#,
        onote = attr(if run.from_last_run {
            LAST_RUN_NOTE
        } else {
            RECORDED_NOTE
        }),
        outcome = outcome_word(run.outcome),
        source = if run.from_last_run {
            r#"<span class="st-note">from the last run's record</span>"#
        } else {
            r#"<span class="st-note">outcome not stored</span>"#
        },
        built = run.built,
        knote = attr(KEPT_NOTE),
        kept = run.kept,
    );
    let _ = write!(
        b,
        r#"<div class="st-notice">{INFO}<span><strong>State rule:</strong> if a node fails, it keeps its last good state. {}</span></div>"#,
        text(&view.state_rule)
    );
    let href = enc(&run.run_id);
    let _ = write!(
        b,
        r#"<nav class="st-tabs" aria-label="Run views"><a class="st-tab" href="{href}"{t}>Timeline</a><a class="st-tab" href="{href}?tab=nodes"{n}>Nodes</a><span class="st-tab disabled" aria-disabled="true" title="Run logs aren't recorded yet">Log<span class="chip">Planned</span></span><span class="st-tab disabled" aria-disabled="true" title="Planned">Graph<span class="chip">Planned</span></span></nav>"#,
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
    // The dot says the outcome only when the last run's record gives it.
    let dot = match (run.outcome, run.from_last_run) {
        (RunOutcome::Succeeded, true) => "dot",
        (RunOutcome::Failed, true) => "dot bad",
        _ => "dot none",
    };
    let status = format!(
        r#"<span class="pill-snap"><span class="{dot}"></span>snapshot {} · recorded {}</span>"#,
        run.snapshot,
        text(&run.recorded_at.to_string())
    );
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

fn timeline(b: &mut String, view: &RunPageView) {
    const ROW: usize = 28;
    const TOP: usize = 30;
    const START: usize = 200;
    const BUILD_FROM: usize = 272;
    const RIGHT: usize = 776;
    let rows = &view.timeline;
    let height = TOP + ROW * rows.len() + 6;
    let lanes = rows
        .iter()
        .filter(|r| r.built)
        .map(|r| r.lane)
        .max()
        .map_or(1, |l| l + 1);
    let width = ((RIGHT - BUILD_FROM - 90) / lanes).clamp(24, 260);
    let _ = write!(
        b,
        r#"<section class="st-timeline" aria-label="Timeline"><svg width="100%" viewBox="0 0 796 {height}" role="img" aria-label="{label}">"#,
        label = attr(&format!(
            "Timeline of run {}: {} kept an earlier build, {} built. Durations not recorded yet. The Nodes tab lists them as a table.",
            view.run.short_run_id,
            count(view.run.kept, "node"),
            view.run.built
        )),
    );
    for (i, r) in rows.iter().enumerate() {
        let y = TOP + ROW * i;
        if r.built {
            let _ = write!(
                b,
                r#"<rect class="tl-sel" x="0" y="{y}" width="796" height="{ROW}"></rect>"#
            );
        }
        let _ = write!(
            b,
            r#"<path class="tl-line" d="M0 {y2}H796"></path>"#,
            y2 = y + ROW
        );
    }
    let _ = write!(
        b,
        r#"<g class="tl-head"><text x="0" y="18">Node</text><text x="{START}" y="18">start</text><text x="{RIGHT}" y="18" text-anchor="end">[wall clock]</text></g><g class="tl-guide"><path d="M{START} 24V{height}"></path><path d="M{RIGHT} 24V{height}"></path></g>"#
    );
    for (i, r) in rows.iter().enumerate() {
        timeline_row(b, TOP + ROW * i, r, width);
    }
    b.push_str("</svg>");
    let _ = write!(
        b,
        r#"<div class="st-legend"><span><span class="st-sw built"></span>Built</span><span title="{KEPT_NOTE}"><span class="st-sw tick"></span>Kept earlier build: reused, not selected, or failed</span><span><span class="st-sw wait"></span>waits on upstream</span><span class="st-right">Order and waits come from lineage (inferred); lengths are placeholders.</span></div></section>"#
    );
}

/// One node of the timeline, at `y`: its kind, its name, and its bar or kept mark.
fn timeline_row(b: &mut String, y: usize, r: &TimelineRow, width: usize) {
    const START: usize = 200;
    const BUILD_FROM: usize = 272;
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
        bold = if r.built { " built" } else { "" },
        id = text(&r.node),
        name = text(&r.name),
    );
    if r.built {
        let x = BUILD_FROM + r.lane * width;
        if r.lane > 0 {
            let _ = write!(
                b,
                r#"<path class="tl-wait" d="M{BUILD_FROM} {my}H{x}"></path>"#,
                my = y + 14
            );
        }
        let _ = write!(
            b,
            r#"<rect class="tl-bar" x="{x}" y="{by}" width="{width}" height="14" rx="3"></rect><text class="tl-dur" x="{tx}" y="{ty}">[duration]</text>"#,
            by = y + 7,
            tx = x + width + 8,
            ty = y + 18,
        );
    } else {
        let _ = write!(
            b,
            r#"<rect class="tl-kept" x="{START}" y="{ky}" width="3" height="14" rx="1"></rect><text class="tl-kept-text" x="{tx}" y="{ty}"><title>{title}</title>kept</text>"#,
            ky = y + 7,
            tx = START + 10,
            ty = y + 18,
            title = text(&r.kept_from.as_deref().map_or_else(
                || "kept an earlier build".to_owned(),
                |f| format!("kept the build of run {f}")
            )),
        );
    }
}

fn nodes_table(b: &mut String, view: &RunPageView) {
    b.push_str(r#"<section class="st-timeline" aria-label="Nodes"><table class="st-nodes"><thead><tr><th scope="col">Node</th><th scope="col">Type</th><th scope="col">This run</th><th scope="col">Build kept from</th></tr></thead><tbody>"#);
    for r in &view.timeline {
        let _ = write!(
            b,
            r#"<tr><th scope="row">{name}</th><td class="st-dim">{kind}</td><td>{what}</td><td class="mono st-dim">{from}</td></tr>"#,
            name = node_name(&r.node, &r.name, "../../"),
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
    b.push_str("</tbody></table></section>");
}

fn run_side(b: &mut String, view: &RunPageView) {
    let run = &view.run;
    let _ = write!(
        b,
        r#"<aside class="st-side w340" aria-label="Run details"><div class="st-side-sec"><h2>Run details</h2><dl class="st-dl"><dt>Command</dt><dd>{command}</dd><dt>Target</dt><dd>{target}</dd><dt>Snapshot</dt><dd>{snap}</dd><dt>Compared with</dt><dd>{compared}</dd><dt>Recorded</dt><dd class="mono st-ts">{recorded}</dd><dt>Started</dt><dd class="st-dim">[start time]</dd><dt>Triggered by</dt><dd class="st-dim">[user]</dd></dl></div>"#,
        command = run.command.as_deref().map_or_else(
            || r#"<span class="st-dim" title="Snapshots don't record their command yet">not recorded</span>"#.to_owned(),
            |c| format!(
                r#"<code>{}</code> <span class="st-small" title="{}">from the last run's record</span>"#,
                text(c),
                attr(LAST_RUN_NOTE)
            ),
        ),
        target = text(&target_text(run)),
        snap = text(&run.replaces.map_or_else(
            || format!("{} (the first)", run.snapshot),
            |p| format!("{} (replaces {p})", run.snapshot)
        )),
        compared = view.compared_with.as_ref().map_or_else(
            || "nothing: first recorded run".to_owned(),
            |c| format!(
                r#"snapshot {} · run <a class="mono" href="{}" title="{}">{}</a>"#,
                c.snapshot,
                attr(&enc(&c.run_id)),
                attr(&c.run_id),
                text(&c.short_run_id)
            ),
        ),
        recorded = text(&run.recorded_at.to_string()),
    );
    b.push_str(r#"<div class="st-side-sec" id="built" tabindex="-1"><h2>Built in this run</h2>"#);
    if view.built.is_empty() {
        b.push_str(r#"<span class="st-small">Nothing: every recorded node kept an earlier build (e.g. a run that only recorded tests).</span>"#);
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
        last_run_panels(b, last, Some(run.snapshot), "../../");
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
        b.push_str(r#"<span class="st-small">None: this is the first recorded run.</span>"#);
    }
    for e in &view.earlier {
        let (_, time) = date_and_time(e.recorded_at);
        let _ = write!(
            b,
            r#"<a class="st-earlier" href="{href}"><span class="st-earlier-top"><span>snapshot {snap} · <span class="mono">{short}</span></span><span class="st-o {o}">{o}</span></span><span class="muted">{counts} · <span class="mono st-ts">{time}</span></span></a>"#,
            href = attr(&enc(&e.run_id)),
            snap = e.snapshot,
            short = text(&e.short_run_id),
            o = outcome_word(e.outcome),
            counts = text(&format!("{} built, {} kept", e.built, e.kept)),
            time = text(&time),
        );
    }
    let _ = write!(
        b,
        r#"</div><div class="st-side-foot"><button type="button" class="st-btn" data-copy-url="../../api/state/runs/{href}" aria-label="Copy run {short} as JSON">Copy as JSON</button><span class="st-btn disabled" aria-disabled="true" title="Run logs aren't recorded yet">Download log<span class="chip">Planned</span></span></div></aside>"#,
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
