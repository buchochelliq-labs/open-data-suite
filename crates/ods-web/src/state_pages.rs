//! The State pages (#311): Plan with its Why panel, Runs, and one Run, rendered on the
//! server from the view models in [`crate::dashboard::state`], and their JSON API.
//!
//! Every route is `GET`: actions are commands to copy, never requests to the server.

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
    ChainLine, LastRunView, Names, PlanRow, PlanView, RunFilter, RunOutcome, RunPageView, RunRow,
    RunsView, WhyView, count, date_and_time,
};
use crate::dashboard::{CommandHint, EmptyState, ShellView, StateStatus};
use crate::home::{Frame, framed};
use crate::server::{Shared, Snapshot};

const CSS: &str = include_str!("../assets/state.css");
const JS: &str = include_str!("../assets/state.js");

/// The State pages' routes, prefixed by `at` (the base path).
pub(crate) fn routes(app: Router<Shared>, at: &dyn Fn(&str) -> String) -> Router<Shared> {
    app.route(
        &at("/state"),
        get(|| async { Redirect::temporary("state/plan") }),
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

/// Node ids to names, from the lineage graph: it names sources too.
fn names(snapshot: &Snapshot) -> Names {
    snapshot
        .document
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.name.clone()))
        .collect()
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

async fn plan_page(State(state): State<Shared>, Query(query): Query<PlanQuery>) -> Html<String> {
    let snapshot = state.current();
    let dashboard = snapshot.dashboard();
    let view = dashboard.plan_view(
        state.details,
        Timestamp::now(),
        query.action.as_deref(),
        query.node.as_deref(),
        &names(&snapshot),
    );
    Html(plan_html(
        &dashboard.shell("state"),
        &view,
        query.view.as_deref() == Some("json"),
        state.generation.load(Ordering::SeqCst),
    ))
}

async fn plan_api(State(state): State<Shared>, Query(query): Query<PlanQuery>) -> Json<PlanView> {
    let snapshot = state.current();
    Json(snapshot.dashboard().plan_view(
        state.details,
        Timestamp::now(),
        query.action.as_deref(),
        query.node.as_deref(),
        &names(&snapshot),
    ))
}

async fn why_api(State(state): State<Shared>, Path(node): Path<String>) -> Response {
    let snapshot = state.current();
    match snapshot
        .dashboard()
        .why_view(Timestamp::now(), &node, &names(&snapshot))
    {
        Some(why) => Json(why).into_response(),
        None => not_found("no such planned node, or no plan: see /api/state/plan"),
    }
}

async fn runs_page(State(state): State<Shared>, Query(query): Query<RunsQuery>) -> Html<String> {
    let snapshot = state.current();
    let dashboard = snapshot.dashboard();
    let view = dashboard.runs_view(
        state.details,
        Timestamp::now(),
        &query.filter(),
        &names(&snapshot),
    );
    Html(runs_html(
        &dashboard.shell("state"),
        &view,
        state.generation.load(Ordering::SeqCst),
    ))
}

async fn runs_api(State(state): State<Shared>, Query(query): Query<RunsQuery>) -> Json<RunsView> {
    let snapshot = state.current();
    Json(snapshot.dashboard().runs_view(
        state.details,
        Timestamp::now(),
        &query.filter(),
        &names(&snapshot),
    ))
}

async fn run_page(
    State(state): State<Shared>,
    Path(run): Path<String>,
    Query(query): Query<RunQuery>,
) -> Response {
    let snapshot = state.current();
    let dashboard = snapshot.dashboard();
    let shell = dashboard.shell("state");
    let generation = state.generation.load(Ordering::SeqCst);
    match dashboard.run_view(state.details, &run, &names(&snapshot)) {
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
}

async fn run_api(State(state): State<Shared>, Path(run): Path<String>) -> Response {
    let snapshot = state.current();
    match snapshot
        .dashboard()
        .run_view(state.details, &run, &names(&snapshot))
    {
        Some(view) => Json(view).into_response(),
        None => not_found("no such run among the listed ones: see /api/state/runs"),
    }
}

// ------------------------------------------------------------------------ helpers

/// Percent-encodes a URL path segment or query value (node and run ids).
fn enc(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// `?a=1&b=2` from the pairs whose value isn't empty; empty when none.
fn query(pairs: &[(&str, &str)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    if parts.is_empty() {
        "?".to_owned()
    } else {
        format!("?{}", parts.join("&"))
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
const INFERRED_NOTE: &str = "Inferred: the command and outcome are the last run's, kept beside the store for `ods state retry`, and this is the only snapshot recorded since it started.";

fn outcome_word(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Succeeded => "succeeded",
        RunOutcome::Failed => "failed",
        RunOutcome::Recorded => "recorded",
    }
}

fn copy_button(value: &str, label: &str) -> String {
    format!(
        r#"<button type="button" class="st-btn" data-copy="{}">{}</button>"#,
        attr(value),
        text(label)
    )
}

fn command_box(b: &mut String, command: &CommandHint) {
    let _ = write!(
        b,
        r#"<div class="st-cmd"><code>{c}</code>{copy}</div><p class="st-small">{d}</p>"#,
        c = text(&command.command),
        copy = copy_button(&command.command, "Copy"),
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
            r#"<div class="cmd"><code>{c}</code><span>{d}</span></div>"#,
            c = text(&command.command),
            d = text(&command.does),
        );
    }
    b.push_str("</div></section>");
}

fn crumbs(parts: &[(&str, Option<&str>)], mono_last: bool) -> String {
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

// --------------------------------------------------------------------------- plan

fn plan_html(shell: &ShellView, view: &PlanView, json: bool, generation: u64) -> String {
    let mut b = String::with_capacity(32 * 1024);
    b.push_str(r#"<div class="st-split"><section class="st-main">"#);
    if let Some(empty) = &view.empty {
        empty_card(&mut b, view.state, empty);
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
                r#"<aside class="st-side" aria-label="Why"><p class="st-pad muted">Nothing is planned.</p></aside>"#
            } else {
                r#"<aside class="st-side" aria-label="Why"><p class="st-pad muted">No planned node by that name: pick one in the table to see why it builds or is reused.</p></aside>"#
            });
        }
        None => {}
    }
    b.push_str("</div>");
    let status = format!(
        r#"<span class="st-status">{}</span>"#,
        text(&view.based_on.map_or_else(
            || "no recorded state · dry run, nothing built".to_owned(),
            |s| format!("against snapshot {s} · dry run, nothing built"),
        ))
    );
    let title = format!("Plan for {}", view.environment);
    let frame = Frame {
        title: &title,
        crumbs: Some(crumbs(&[("State", None), (&title, None)], false)),
        root: "../",
        status: Some(status),
        css: CSS,
        js: JS,
    };
    framed(shell, &frame, &b, generation)
}

fn plan_main(b: &mut String, view: &PlanView) {
    let c = view.counts;
    let _ = write!(
        b,
        r#"<div class="st-tiles3"><div class="st-tile" data-tile="build"><span class="st-label">To build</span><span class="st-value build">{build}</span></div><div class="st-tile" data-tile="reuse"><span class="st-label">To reuse</span><span class="st-value reuse">{reuse}</span></div><div class="st-tile" data-tile="relations" title="Reused nodes whose table was found in the warehouse. This plan is made offline, so none are checked; `ods state build --dry-run` checks."><span class="st-label">Relations checked</span><span class="st-value">{checked} / {reuse}</span>{note}</div></div>"#,
        build = c.build,
        reuse = c.reuse,
        checked = c.relations_checked,
        note = if c.reuse > 0 && c.relations_checked < c.reuse {
            r#"<span class="st-note">not checked: planned offline</span>"#
        } else {
            ""
        },
    );
    if let Some(command) = view.commands.first() {
        let _ = write!(
            b,
            r#"<div class="st-cmdbar"><code>{c}</code><span class="st-cmd-does">{d}</span>{copy}</div>"#,
            c = text(&command.command),
            d = text(&command.does),
            copy = copy_button(&command.command, "Copy"),
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
            r#" aria-current="true""#
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
    b.push_str(r#"<section class="st-table" aria-label="Decisions"><div class="st-thead"><span>Node</span><span>Type</span><span>Action</span><span>Why</span></div>"#);
    if view.rows.is_empty() {
        b.push_str(r#"<p class="st-pad muted">No node has this action.</p>"#);
    }
    for row in &view.rows {
        plan_row(b, row, view.filter);
    }
    b.push_str("</section>");
}

fn action_pill(action: PlanAction) -> &'static str {
    if action == PlanAction::Build {
        r#"<span class="st-pill build">BUILD</span>"#
    } else {
        r#"<span class="st-pill reuse">REUSE</span>"#
    }
}

fn plan_row(b: &mut String, row: &PlanRow, filter: &str) {
    let _ = write!(
        b,
        r#"<div class="st-tr{sel}" data-node="{id}"><a class="st-node" href="{href}"{current}>{name}</a><span class="st-kind">{kind}</span><span>{pill}</span><span class="st-why-cell">{why}{unknown}</span></div>"#,
        sel = if row.selected { " selected" } else { "" },
        id = attr(&row.node),
        href = attr(&query(&[
            ("action", if filter == "all" { "" } else { filter }),
            ("node", &row.node)
        ])),
        current = if row.selected {
            r#" aria-current="true""#
        } else {
            ""
        },
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
        r#"<aside class="st-side st-why" aria-label="Why {name} {verb}"><div class="st-side-head"><span class="st-label">{eyebrow}</span><span class="st-why-name mono">{name}</span><div class="st-toggle"><a href="{explain}"{p1}>Explanation</a><a href="{json_href}"{p2}>JSON</a></div></div>"#,
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
        p1 = if json { "" } else { r#" aria-pressed="true""# },
        p2 = if json { r#" aria-pressed="true""# } else { "" },
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
    // 1. The recorded build.
    match &why.last_build {
        Some(last) => {
            let _ = write!(
                b,
                r#"<li><strong>Recorded build found</strong><span class="st-sub">run <span class="mono" title="{run}">{short}</span>, snapshot {snap}, built <span class="mono">{at}</span>{tested}</span></li>"#,
                run = attr(&last.run_id),
                short = text(&last.short_run_id),
                snap = last.snapshot,
                at = text(&last.built_at.to_string()),
                tested = if last.tested_in.is_some() {
                    ", tests passed"
                } else {
                    ""
                },
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
    // 2. The fingerprint.
    let fp = &why.fingerprint;
    let _ = write!(b, r"<li><strong>Fingerprint {}</strong>", text(&fp.summary));
    if fp.compared {
        b.push_str(r#"<div class="st-grid2">"#);
        for c in &fp.components {
            let _ = write!(
                b,
                r#"<span class="muted">{}</span>{}"#,
                text(&c.name),
                if c.changed {
                    r#"<span class="st-changed">changed</span>"#
                } else {
                    "<span>unchanged</span>"
                }
            );
        }
        b.push_str("</div>");
        if let (Some(before), Some(after)) = (&fp.before, &fp.after)
            && before != after
        {
            let _ = write!(
                b,
                r#"<pre class="st-diff"><span class="del">- {}</span>
<span class="add">+ {}</span></pre><span class="st-sub">Snapshots keep digests, not code: the change itself isn't shown.</span>"#,
                text(before),
                text(after)
            );
        }
    }
    b.push_str("</li>");
}

/// The planned parents and the sources it reads, with each source's evidence.
fn why_reads(b: &mut String, why: &WhyView, filter: &str) {
    // 3. What it reads.
    if !why.parents.is_empty() || !why.sources.is_empty() {
        b.push_str(r#"<li><strong>What it reads</strong><ul class="st-reads">"#);
        for p in &why.parents {
            let _ = write!(
                b,
                r#"<li><a class="mono" href="{href}">{name}</a> <span class="{class}">{decision}</span></li>"#,
                href = attr(&query(&[("action", filter), ("node", &p.node)])),
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
        b.push_str("</ul></li>");
    }
}

/// The relation check and the decision, with the readers that rebuild with it.
fn why_decision(b: &mut String, why: &WhyView, filter: &str) {
    let builds = why.action == PlanAction::Build;
    // 4. The relation.
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
    // 5. The decision.
    let _ = write!(
        b,
        r#"<li><strong>Decision: {}</strong><ul class="st-reasons">"#,
        if builds { "BUILD" } else { "REUSE" }
    );
    for r in &why.reasons {
        let _ = write!(
            b,
            r#"<li>{msg} <span class="st-code" title="The planner's reason code">{label}</span></li>"#,
            msg = text(&r.message),
            label = text(&r.label),
        );
    }
    b.push_str("</ul>");
    if !why.readers.is_empty() {
        let names: Vec<String> = why
            .readers
            .iter()
            .map(|r| {
                format!(
                    r#"<a class="mono" href="{}">{}</a>"#,
                    attr(&query(&[("action", filter), ("node", &r.node)])),
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

/// The reason chain, every piece of evidence, the command, and links out.
fn why_chain(b: &mut String, why: &WhyView) {
    // The chain, as `ods state explain` prints it.
    let _ = write!(
        b,
        r#"<section class="st-pad st-chain" aria-label="Reason chain"><h3 class="st-label">Reason chain · as <code>ods state explain</code></h3><p class="st-verdict">{}</p><ul class="st-tree">"#,
        text(&why.verdict)
    );
    chain(b, &why.chain);
    b.push_str("</ul></section>");
    if !why.evidence.is_empty() {
        b.push_str(r#"<details class="st-pad st-evidence"><summary>All evidence</summary><ul>"#);
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
    b.push_str(r#"<div class="st-pad">"#);
    for command in &why.commands {
        command_box(b, command);
    }
    let _ = write!(
        b,
        r#"<p class="st-links"><a href="../lineage?node={id}">Lineage</a> · <a href="../catalog/{id}">Model page</a></p></div></aside>"#,
        id = attr(&enc(&why.node)),
    );
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

fn glyph(row_outcome: RunOutcome, inferred: bool) -> String {
    let (class, mark, label) = match row_outcome {
        RunOutcome::Succeeded => ("ok", "✓", "succeeded"),
        RunOutcome::Failed => ("bad", "✕", "failed"),
        RunOutcome::Recorded => ("rec", "", "recorded"),
    };
    let title = match (row_outcome, inferred) {
        (RunOutcome::Recorded, _) => RECORDED_NOTE.to_owned(),
        (_, true) => format!("{label}. {INFERRED_NOTE}"),
        (_, false) => label.to_owned(),
    };
    format!(
        r#"<span class="st-glyph {class}{inf}" role="img" aria-label="{label}" title="{title}">{mark}</span>"#,
        inf = if inferred { " inferred" } else { "" },
        title = attr(&title),
    )
}

fn num_pill(n: Option<usize>, class: &str, unknown: &str) -> String {
    match n {
        Some(0) => r#"<span class="st-num zero">–</span>"#.to_owned(),
        Some(n) => format!(r#"<span class="st-num {class}">{n}</span>"#),
        None => format!(
            r#"<span class="st-num zero" title="{}">·</span>"#,
            attr(unknown)
        ),
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

fn runs_html(shell: &ShellView, view: &RunsView, generation: u64) -> String {
    let mut b = String::with_capacity(32 * 1024);
    b.push_str(r#"<div class="st-split"><section class="st-main">"#);
    let _ = write!(
        b,
        r#"<div class="st-title"><h1>Runs</h1><span class="muted">{} · every run recorded in this state store, newest first</span></div>"#,
        text(&view.project)
    );
    let _ = write!(
        b,
        r#"<div class="st-notice">{INFO}<span><strong>ODS doesn't schedule anything.</strong> A run appears here after <code>ods state build</code> or <code>ods state run</code> records it in this state store, from your terminal or a CI job.</span></div>"#
    );
    let listed = view.runs.len();
    let _ = write!(
        b,
        r#"<div class="st-tabs" role="tablist"><span role="tab" aria-selected="true" class="st-tab">Local runs<span class="st-count">{total}</span></span><span role="tab" aria-selected="false" aria-disabled="true" class="st-tab disabled" title="CI runs need server mode, which is planned">{ci}<span class="chip">Planned</span></span></div>"#,
        total = view.total,
        ci = text(view.ci),
    );
    if let Some(empty) = &view.empty {
        empty_card(&mut b, view.state, empty);
        b.push_str("</section></div>");
        return runs_frame(shell, view, &b, generation);
    }
    facets(&mut b, view);
    b.push_str(r#"<section class="st-runs" role="table" aria-label="Local runs"><div class="st-rrow st-rhead" role="row"><span role="columnheader" aria-label="Outcome"></span><span role="columnheader">Run</span><span role="columnheader">Command</span><span role="columnheader">Target</span><span role="columnheader">Snapshot</span><span role="columnheader">Built</span><span role="columnheader" title="Kept earlier build: reused, not selected, or failed">Kept</span><span role="columnheader">Failed</span><span role="columnheader">Skipped</span><span role="columnheader" title="When the run's snapshot was recorded">Recorded</span><span role="columnheader">Duration</span><span role="columnheader">Triggered by</span></div>"#);
    let unrecorded = view.last_run.as_ref().filter(|_| view.last_run_listed);
    if let Some(last) = unrecorded {
        last_run_row(&mut b, last, view);
    }
    if view.runs.is_empty() && unrecorded.is_none() {
        b.push_str(r#"<p class="st-pad muted">No run matches these filters.</p>"#);
    }
    run_rows(&mut b, view);
    b.push_str("</section>");
    let _ = write!(
        b,
        r#"<div class="st-legend"><span><span class="st-num built">n</span>built</span><span title="{kept}"><span class="st-num kept">n</span>kept earlier build: reused, not selected, or failed</span><span><span class="st-num failed">n</span>failed</span><span><span class="st-num skipped">n</span>skipped: waited on a failed node</span><span><span class="st-num zero">·</span>not recorded</span><span class="st-right">Duration and user are not recorded yet.</span></div>"#,
        kept = attr(KEPT_NOTE),
    );
    if let Some(last) = view
        .last_run
        .as_ref()
        .filter(|l| l.snapshot.is_none() && !l.recorded_nothing)
    {
        let _ = write!(
            b,
            r#"<div class="st-notice">{INFO}<span><strong>The last run can't be tied to a run here.</strong> <code>{command}</code> started at <span class="mono">{at}</span>{failed}, but which snapshot it recorded, if any, can't be told from the times kept (to the second). <code>ods state history</code> lists the snapshots.</span></div>"#,
            command = text(&last.command_name),
            at = text(&last.started_at.to_string()),
            failed = if last.failed.is_empty() {
                String::new()
            } else {
                text(&format!(
                    "; {} failed",
                    last.failed
                        .iter()
                        .map(|n| n.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .into_owned()
            },
        );
    }
    b.push_str(
        r#"<details class="st-recorded"><summary>What the state store records</summary><ul>"#,
    );
    for line in &view.recorded {
        let _ = write!(b, "<li>{}</li>", text(line));
    }
    if view.total > listed && view.total > view.limit {
        let _ = write!(
            b,
            "<li>Only the newest {} runs are listed; <code>ods state history</code> lists them all.</li>",
            view.limit
        );
    }
    b.push_str("</ul></details></section>");
    runs_side(&mut b, view);
    b.push_str("</div>");
    runs_frame(shell, view, &b, generation)
}

/// The recorded runs, grouped by day.
fn run_rows(b: &mut String, view: &RunsView) {
    let mut day = String::new();
    for row in &view.runs {
        let (date, time) = date_and_time(row.recorded_at);
        if date != day {
            let _ = write!(
                b,
                r#"<div class="st-rgroup" role="row">{} · UTC</div>"#,
                text(&date)
            );
            day = date;
        }
        let selected = view.selected.as_deref() == Some(row.run_id.as_str());
        let _ = write!(
            b,
            r#"<div class="st-rrow{sel}{bad}" role="row" aria-selected="{selected}" data-run="{id}"><span role="cell"><a href="{select}" title="Show in the side panel">{glyph}</a></span><span role="cell"><a class="mono st-runid" href="runs/{href}" title="run {id}">{short}</a></span><span role="cell">{command}</span><span role="cell" class="st-dim">{target}</span><span role="cell" class="st-snap">{snap}</span><span role="cell">{built}</span><span role="cell" title="{kept_note}">{kept}</span><span role="cell">{failed}</span><span role="cell">{skipped}</span><span role="cell" class="mono">{time}</span><span role="cell" class="st-dim">[duration]</span><span role="cell" class="st-dim">[user]</span></div>"#,
            sel = if selected { " selected" } else { "" },
            bad = if row.outcome == RunOutcome::Failed {
                " failed"
            } else {
                ""
            },
            id = attr(&row.run_id),
            select = attr(&runs_query(view, Some(&row.run_id))),
            glyph = glyph(row.outcome, row.inferred),
            href = attr(&enc(&row.run_id)),
            short = text(&row.short_run_id),
            command = row.command.as_deref().map_or_else(
                || r#"<span class="st-dim" title="Snapshots don't record their command yet">—</span>"#.to_owned(),
                |c| format!(r#"<code title="{}">{}</code>"#, attr(INFERRED_NOTE), text(c)),
            ),
            target = text(&target_text(row)),
            snap = row.snapshot,
            built = num_pill(Some(row.built), "built", ""),
            kept_note = attr(KEPT_NOTE),
            kept = num_pill(Some(row.kept), "kept", ""),
            failed = num_pill(row.failed, "failed", "Not recorded for this run"),
            skipped = num_pill(row.skipped, "skipped", "Not recorded for this run"),
            time = text(&time),
        );
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
        status: Some(status),
        css: CSS,
        js: JS,
    };
    framed(shell, &frame, body, generation)
}

/// `?outcome=…&target=…&date=…&run=…`, keeping the filters applied.
fn runs_query(view: &RunsView, run: Option<&str>) -> String {
    let applied = |key: &str| {
        view.facets
            .iter()
            .find(|f| f.key == key)
            .and_then(|f| f.options.iter().find(|o| o.selected))
            .map_or("", |o| o.value.as_str())
    };
    query(&[
        ("outcome", applied("outcome")),
        ("target", applied("target")),
        ("date", applied("date")),
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
            r#"<details class="st-facet"><summary><span class="muted">{label}:</span>{current}<span class="muted">▾</span></summary><div class="st-menu">"#,
            label = text(facet.label),
            current = text(current),
        );
        for option in &facet.options {
            let mut pairs: Vec<(&str, String)> = ["outcome", "target", "date"]
                .iter()
                .map(|key| {
                    let value = if *key == facet.key {
                        option.value.clone()
                    } else {
                        view.facets
                            .iter()
                            .find(|f| f.key == *key)
                            .and_then(|f| f.options.iter().find(|o| o.selected))
                            .map_or_else(String::new, |o| o.value.clone())
                    };
                    (*key, value)
                })
                .collect();
            pairs.push(("run", String::new()));
            let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let _ = write!(
                b,
                r#"<a href="{href}"{current}>{label}<span class="st-count">{count}</span></a>"#,
                href = attr(&query(&refs)),
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
    let _ = write!(
        b,
        r#"<span class="st-right muted">{}{}</span></div>"#,
        text(&count(view.listed, "run")),
        if view.failed > 0 {
            format!(" · {} failed", view.failed)
        } else {
            String::new()
        }
    );
}

/// The last run, when it recorded no snapshot: its row, before the recorded ones.
fn last_run_row(b: &mut String, last: &LastRunView, view: &RunsView) {
    let failed = !last.failed.is_empty() || !last.skipped.is_empty();
    let (date, time) = date_and_time(last.started_at);
    let selected = view.selected.as_deref() == Some("last");
    let _ = write!(
        b,
        r#"<div class="st-rgroup" role="row">Last run · {date} · UTC</div><div class="st-rrow{sel}{bad}" role="row" data-run="last"><span role="cell"><a href="{select}" title="Show in the side panel">{glyph}</a></span><span role="cell" class="st-dim" title="It recorded no snapshot, so it has no run id here">last run</span><span role="cell"><code title="{full}">{command}</code></span><span role="cell" class="st-dim">—</span><span role="cell" class="st-snap{kept_class}" title="It recorded no snapshot: the last good state was kept">{snap}</span><span role="cell">{built}</span><span role="cell">{kept}</span><span role="cell">{nfailed}</span><span role="cell">{nskipped}</span><span role="cell" class="mono" title="When it started">{time}</span><span role="cell" class="st-dim">[duration]</span><span role="cell" class="st-dim">[user]</span></div>"#,
        date = text(&date),
        select = attr(&runs_query(view, Some("last"))),
        sel = if selected { " selected" } else { "" },
        bad = if failed { " failed" } else { "" },
        glyph = glyph(
            if failed {
                RunOutcome::Failed
            } else {
                RunOutcome::Succeeded
            },
            false
        ),
        command = text(&last.command_name),
        full = attr(&last.command),
        kept_class = if failed { " bad" } else { "" },
        snap = last
            .last_good
            .map_or_else(|| "none".to_owned(), |s| format!("kept {s}")),
        built = num_pill(Some(0), "built", ""),
        kept = num_pill(None, "kept", "It recorded nothing"),
        nfailed = num_pill(Some(last.failed.len()), "failed", ""),
        nskipped = num_pill(Some(last.skipped.len()), "skipped", ""),
        time = text(&time),
    );
}

fn runs_side(b: &mut String, view: &RunsView) {
    let row = view
        .selected
        .as_deref()
        .and_then(|id| view.runs.iter().find(|r| r.run_id == id));
    let last = view.last_run.as_ref();
    b.push_str(r#"<aside class="st-side" aria-label="Selected run">"#);
    match row {
        Some(row) => {
            let (_, time) = date_and_time(row.recorded_at);
            let _ = write!(
                b,
                r#"<div class="st-side-head"><div class="st-head-line">{glyph}<h2 class="mono">run {short}</h2><span class="st-outcome {outcome}">{outcome_upper}</span></div><span class="muted">{command} · {target} · <span class="mono">{time}</span></span><span>{counts}</span></div>"#,
                glyph = glyph(row.outcome, row.inferred),
                short = text(&row.short_run_id),
                outcome = outcome_word(row.outcome),
                outcome_upper = outcome_word(row.outcome).to_uppercase(),
                command = row.command.as_deref().map_or_else(
                    || r#"<span title="Snapshots don't record their command yet">command not recorded</span>"#.to_owned(),
                    |c| format!("<code>{}</code>", text(c))
                ),
                target = text(&target_text(row)),
                time = text(&time),
                counts = text(&run_counts(row)),
            );
            let linked = last.filter(|l| l.snapshot == Some(row.snapshot));
            match linked {
                Some(last) => last_run_panels(b, last, Some(row.snapshot)),
                None => {
                    let _ = write!(
                        b,
                        r#"<div class="st-side-sec"><h3 class="st-label">State</h3><span class="st-state">{SHIELD}Snapshot {snap}{latest}</span><span class="st-small">{note}</span></div>"#,
                        snap = row.snapshot,
                        latest = if view.runs.first().map(|r| r.snapshot) == Some(row.snapshot)
                            && view
                                .facets
                                .iter()
                                .all(|f| f.options.first().is_some_and(|o| o.selected))
                        {
                            " · the last good state"
                        } else {
                            ""
                        },
                        note = text(&format!(
                            "{RECORDED_NOTE} Failures are only kept for the last run started on this machine."
                        )),
                    );
                }
            }
            let _ = write!(
                b,
                r#"<div class="st-side-foot"><a class="st-btn" href="runs/{href}">Open run</a><button type="button" class="st-btn" data-copy-url="../api/state/runs/{href}">Copy as JSON</button></div>"#,
                href = attr(&enc(&row.run_id)),
            );
        }
        None => match last.filter(|l| l.recorded_nothing && l.outcome_known) {
            Some(last) => {
                let failed = !last.failed.is_empty() || !last.skipped.is_empty();
                let _ = write!(
                    b,
                    r#"<div class="st-side-head"><div class="st-head-line">{glyph}<h2>Last run</h2><span class="st-outcome {o}">{O}</span></div><span class="muted"><code>{command}</code> · started <span class="mono">{at}</span></span><span>{counts}</span>{full}</div>"#,
                    glyph = glyph(
                        if failed {
                            RunOutcome::Failed
                        } else {
                            RunOutcome::Succeeded
                        },
                        false
                    ),
                    o = if failed { "failed" } else { "succeeded" },
                    O = if failed { "FAILED" } else { "SUCCEEDED" },
                    command = text(&last.command_name),
                    full = if last.command == last.command_name {
                        String::new()
                    } else {
                        format!(
                            r#"<details class="st-full"><summary>Full command</summary><code>{}</code></details>"#,
                            text(&last.command)
                        )
                    },
                    at = text(&last.started_at.to_string()),
                    counts = text(&format!(
                        "recorded nothing · {} failed · {} skipped",
                        last.failed.len(),
                        last.skipped.len()
                    )),
                );
                last_run_panels(b, last, None);
            }
            None => b.push_str(r#"<p class="st-pad muted">No run selected.</p>"#),
        },
    }
    b.push_str("</aside>");
}

fn run_counts(row: &RunRow) -> String {
    let mut parts = vec![
        format!("{} built", row.built),
        format!("{} kept earlier build", row.kept),
    ];
    if let Some(n) = row.failed {
        parts.push(format!("{n} failed"));
    }
    if let Some(n) = row.skipped {
        parts.push(format!("{n} skipped"));
    }
    parts.join(" · ")
}

/// The failed nodes, the state kept, and what to run next, from the last run.
fn last_run_panels(b: &mut String, last: &LastRunView, snapshot: Option<u64>) {
    if !last.failed.is_empty() {
        let _ = write!(
            b,
            r#"<div class="st-side-sec"><h3 class="st-label">{}</h3>"#,
            if last.failed.len() == 1 {
                "Failed node"
            } else {
                "Failed nodes"
            }
        );
        for node in &last.failed {
            let _ = write!(
                b,
                r#"<div class="st-failed-node"><span class="st-bar"></span><a class="mono" href="../catalog/{href}">{name}</a></div>"#,
                href = attr(&enc(&node.node)),
                name = text(&node.name),
            );
        }
        b.push_str(r#"<pre class="st-error" title="Error output isn't recorded yet">[error excerpt]</pre><span class="st-dim st-small">The run log isn't recorded yet.</span></div>"#);
    }
    if !last.skipped.is_empty() {
        let names: Vec<String> = last
            .skipped
            .iter()
            .map(|n| text(&n.name).into_owned())
            .collect();
        let _ = write!(
            b,
            r#"<div class="st-side-sec"><h3 class="st-label">Skipped</h3><span class="st-small">Waited on a failed node: <span class="mono">{}</span></span></div>"#,
            names.join(", ")
        );
    }
    let state = match (snapshot, last.last_good) {
        (Some(s), _) => format!(
            "Snapshot {s} recorded this run's successful builds only; the failed nodes keep their last good build. This run is tied to snapshot {s} by time (the only one recorded since it started): inferred."
        ),
        (None, Some(good)) => format!(
            "Snapshot {good} kept: no snapshot was recorded since this run started, and a failed run never replaces the last good state."
        ),
        (None, None) => "No snapshot was recorded: there is no good state yet.".to_owned(),
    };
    let good = snapshot.or(last.last_good);
    let _ = write!(
        b,
        r#"<div class="st-side-sec"><h3 class="st-label">State</h3><span class="st-state">{SHIELD}{title}</span><span class="st-small">{state}</span></div>"#,
        title = text(&good.map_or_else(
            || "No good state yet".to_owned(),
            |g| format!("Last good state: snapshot {g}")
        )),
        state = text(&state),
    );
    if !last.next.is_empty() {
        b.push_str(r#"<div class="st-side-sec"><h3 class="st-label">Suggested next step</h3>"#);
        for command in &last.next {
            command_box(b, command);
        }
        b.push_str(r#"<p class="st-small st-dim">Run it in your terminal; this dashboard is read-only.</p></div>"#);
    }
}

// ---------------------------------------------------------------------------- run

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
            attr(INFERRED_NOTE),
            text(c)
        )),
        target = text(&target_text(run)),
        copy = copy_button(&run.run_id, "Copy run id"),
    );
    let (icon, class) = match run.outcome {
        RunOutcome::Succeeded => (OK_ICON, "ok"),
        RunOutcome::Failed => (FAIL_ICON, "bad"),
        RunOutcome::Recorded => (RECORDED_ICON, "rec"),
    };
    let _ = write!(
        b,
        r#"<div class="st-tiles4"><div class="st-tile" data-tile="outcome" title="{onote}"><span class="st-label">Outcome</span><span class="st-outcome-value {class}">{icon}{outcome}{inferred}</span></div><div class="st-tile" data-tile="built"><span class="st-label">Built</span><span class="st-value build">{built}</span></div><div class="st-tile" data-tile="kept" title="{knote}"><span class="st-label">Kept earlier build</span><span class="st-value reuse">{kept}</span></div><div class="st-tile" data-tile="wall_clock"><span class="st-label">Wall clock</span><span class="st-value st-dim" title="Durations aren't recorded yet">[wall clock]</span></div></div>"#,
        onote = attr(if run.inferred {
            INFERRED_NOTE
        } else {
            RECORDED_NOTE
        }),
        outcome = outcome_word(run.outcome),
        inferred = if run.inferred {
            r#"<span class="st-grade inferred">inferred</span>"#
        } else {
            ""
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
        r#"<div class="st-tabs" role="tablist"><a role="tab" class="st-tab" href="{href}" aria-selected="{t}">Timeline</a><a role="tab" class="st-tab" href="{href}?tab=nodes" aria-selected="{n}">Nodes</a><span role="tab" class="st-tab disabled" aria-disabled="true" title="Run logs aren't recorded yet">Log<span class="chip">Planned</span></span><span role="tab" class="st-tab disabled" aria-disabled="true" title="Planned">Graph<span class="chip">Planned</span></span></div>"#,
        href = attr(&href),
        t = !nodes_tab,
        n = nodes_tab,
    );
    if nodes_tab {
        nodes_table(&mut b, view);
    } else {
        timeline(&mut b, view);
    }
    b.push_str("</section>");
    run_side(&mut b, view);
    b.push_str("</div>");
    let status = format!(
        r#"<span class="pill-snap"><span class="dot"></span>snapshot {} · recorded {}</span>"#,
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
            "Timeline of run {}: {} kept an earlier build, {} built. Durations not recorded yet.",
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
        let y = TOP + ROW * i;
        let kind = r.kind.as_deref().unwrap_or("");
        let _ = write!(
            b,
            r#"<rect class="tl-kind {kclass}" x="0" y="{ky}" width="4" height="14"></rect><text class="tl-name{bold}" x="12" y="{ty}"><title>{id}</title>{name}</text>"#,
            kclass = if kind == "seed" { "seed" } else { "other" },
            ky = y + 7,
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
                r#"<rect class="tl-kept" x="{START}" y="{ky}" width="3" height="14" rx="1"></rect><text class="tl-kept-text" x="{tx}" y="{ty}">kept{from}</text>"#,
                ky = y + 7,
                tx = START + 10,
                ty = y + 18,
                from = r.kept_from.as_deref().map_or_else(String::new, |f| format!(
                    " · build of run {}",
                    text(&f.chars().take(8).collect::<String>())
                )),
            );
        }
    }
    b.push_str("</svg>");
    let _ = write!(
        b,
        r#"<div class="st-legend"><span><span class="st-sw built"></span>Built</span><span title="{KEPT_NOTE}"><span class="st-sw tick"></span>Kept earlier build: reused, not selected, or failed</span><span><span class="st-sw wait"></span>waits on upstream</span><span class="st-right">Bar lengths are placeholders until durations are recorded.</span></div></section>"#
    );
}

fn nodes_table(b: &mut String, view: &RunPageView) {
    b.push_str(r#"<section class="st-timeline" aria-label="Nodes"><table class="st-nodes"><thead><tr><th>Node</th><th>Type</th><th>This run</th><th>Build kept from</th></tr></thead><tbody>"#);
    for r in &view.timeline {
        let _ = write!(
            b,
            r#"<tr><td><a class="mono" href="../../catalog/{href}">{name}</a></td><td class="st-dim">{kind}</td><td>{what}</td><td class="mono st-dim">{from}</td></tr>"#,
            href = attr(&enc(&r.node)),
            name = text(&r.name),
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
        r#"<aside class="st-side" aria-label="Run details"><div class="st-side-sec"><h2>Run details</h2><dl class="st-dl"><dt>Command</dt><dd>{command}</dd><dt>Target</dt><dd>{target}</dd><dt>Snapshot</dt><dd>{snap}</dd><dt>Compared with</dt><dd>{compared}</dd><dt>Recorded</dt><dd class="mono">{recorded}</dd><dt>Started</dt><dd class="st-dim">[start time]</dd><dt>Triggered by</dt><dd class="st-dim">[user]</dd></dl></div>"#,
        command = run.command.as_deref().map_or_else(
            || r#"<span class="st-dim" title="Snapshots don't record their command yet">not recorded</span>"#.to_owned(),
            |c| format!(
                r#"<code>{}</code> <span class="st-grade inferred" title="{}">inferred</span>"#,
                text(c),
                attr(INFERRED_NOTE)
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
    b.push_str(r#"<div class="st-side-sec" id="built"><h2>Built in this run</h2>"#);
    if view.built.is_empty() {
        b.push_str(r#"<span class="st-small muted">Nothing: every recorded node kept an earlier build (e.g. a run that only recorded tests).</span>"#);
    }
    for w in &view.built {
        let _ = write!(
            b,
            r#"<div class="st-built"><a class="mono" href="../../catalog/{href}">{name}</a><span class="muted">{why}</span></div>"#,
            href = attr(&enc(&w.node)),
            name = text(&w.name),
            why = text(&w.why),
        );
    }
    b.push_str("</div>");
    if let Some(last) = &view.last_run {
        last_run_panels(b, last, Some(run.snapshot));
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
        b.push_str(r#"<span class="st-small muted">None: this is the first recorded run.</span>"#);
    }
    for e in &view.earlier {
        let (_, time) = date_and_time(e.recorded_at);
        let _ = write!(
            b,
            r#"<a class="st-earlier" href="{href}"><span class="st-earlier-top"><span>snapshot {snap} · <span class="mono">{short}</span></span><span class="st-o {o}">{o}</span></span><span class="muted">{counts} · <span class="mono">{time}</span></span></a>"#,
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
        r#"</div><div class="st-side-foot"><button type="button" class="st-btn" data-copy-url="../../api/state/runs/{href}">Copy as JSON</button><span class="st-btn disabled" aria-disabled="true" title="Run logs aren't recorded yet">Download log<span class="chip">Planned</span></span></div></aside>"#,
        href = attr(&enc(&run.run_id)),
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
        status: None,
        css: CSS,
        js: JS,
    };
    framed(shell, &frame, &body, generation)
}
