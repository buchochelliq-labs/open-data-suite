//! A node's stats card in a run, for the Lineage page's live view (#322, the
//! `NodeStats` board): `GET /state/runs/<run_id>/card?node=<id>` answers the card as an HTML
//! fragment the page puts in its side panel.
//!
//! It is the Run page's view model and rendering, not a second one: each stat comes
//! from the run's [`NodeStatsView`] (the journal, read through ods-sdk's reader), a
//! failed node gets the Run page's explanation card (#323), and the relation its
//! warehouse link (#329). A stat the run didn't report reads `—` with the reason, never
//! `0` (AGENTS rule 3).

use std::fmt::Write as _;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};
use ods_core::state::TimestampMs;
use ods_sdk::contracts::executor::ExecutionMode;
use ods_sdk::contracts::run_events::NodeRunStatus;
use serde::Deserialize;

use super::{blocking, explanation_card, missing, node_name};
use crate::catalog::RUN_LINK_TITLE;
use crate::dashboard::journal::NodeStatsView;
use crate::dashboard::state::RunPageView;
use crate::model_page::{no_link_reason, warehouse_link, with_code};
use crate::server::Shared;
use ods_sdk::contracts::relation_link::RelationLinkFields;

#[derive(Debug, Deserialize)]
pub(super) struct CardQuery {
    /// The node, by id.
    node: String,
}

/// The status as the live view's pill says it.
fn pill(status: NodeRunStatus, test_run: bool) -> (&'static str, &'static str) {
    match status {
        NodeRunStatus::Queued => ("queued", "QUEUED"),
        NodeRunStatus::Running => ("running", "RUNNING"),
        NodeRunStatus::Success if test_run => ("success", "TESTED"),
        NodeRunStatus::Success => ("success", "BUILT"),
        NodeRunStatus::Error => ("error", "FAILED"),
        NodeRunStatus::Skipped => ("skipped", "SKIPPED"),
        _ => ("unknown", "UNKNOWN"),
    }
}

/// `14:02:11`, with the whole time for machines and on hover.
fn clock(at: TimestampMs) -> String {
    let full = at.to_string();
    let time = full
        .split_once('T')
        .map_or(full.as_str(), |(_, t)| t)
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches('Z')
        .to_owned();
    format!(
        r#"<time class="mono" datetime="{}" title="{}">{}</time>"#,
        attr(&full),
        attr(&full),
        text(&time)
    )
}

/// A stat: its name, its value (HTML), and a note under it (text, with `code` spans).
fn row(b: &mut String, key: &str, value: &str, sub: Option<&str>) {
    let _ = write!(
        b,
        r#"<div class="lv-row" data-stat="{k}"><dt>{key}</dt><dd><span class="lv-v">{value}</span>{sub}</dd></div>"#,
        k = attr(&key.to_ascii_lowercase().replace(' ', "_")),
        key = text(key),
        sub = sub.map_or_else(String::new, |s| format!(
            r#"<span class="lv-sub">{}</span>"#,
            with_code(s)
        )),
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "one card, stat by stat, in the board's order"
)]
/// The card of `node`: its status, times, rows, adapter extras, thread, relation, why
/// it ran, tests and what blocked it, and for a failed node what went wrong.
pub(crate) fn card(node: &NodeStatsView, nodes: &[NodeStatsView], about: &About<'_>) -> String {
    let About {
        relation,
        link,
        why,
        ..
    } = *about;
    let mut b = String::with_capacity(4 * 1024);
    // A success in a test run tested; it didn't build.
    let (class, word) = pill(node.status, node.status_label == "tested");
    // `python model`, as the Catalog says it, and how it materializes.
    let kind = match (node.kind.as_deref(), about.language) {
        (Some(kind), Some(language)) if language != "sql" => format!("{language} {kind}"),
        (Some(kind), _) => kind.to_owned(),
        (None, _) => String::new(),
    };
    let chip = |t: &str| {
        if t.is_empty() {
            String::new()
        } else {
            format!(r#"<span class="pill plain">{}</span>"#, text(t))
        }
    };
    let _ = write!(
        b,
        r#"<section class="lv-card" data-status="{class}" aria-label="{name}, this run's stats"><div class="lv-card-pills"><span class="lv-pill {class}">{word}</span>{kind}{mat}</div>"#,
        name = attr(&node.name),
        kind = chip(&kind),
        mat = chip(about.materialization.unwrap_or_default()),
    );
    if node.status == NodeRunStatus::Error {
        explanation_card(&mut b, node, nodes);
    }
    b.push_str(r#"<dl class="lv-stats">"#);
    row(&mut b, "Status", node.status_label, None);
    let not_run = matches!(node.status, NodeRunStatus::Queued | NodeRunStatus::Skipped);
    let started = node.started_at.map_or_else(
        || {
            missing(if not_run {
                "didn't run"
            } else {
                "not recorded"
            })
        },
        clock,
    );
    row(&mut b, "Started", &started, None);
    let ended = match (node.finished_at, node.status) {
        (_, NodeRunStatus::Running) => "still running".to_owned(),
        (Some(at), _) => clock(at),
        (None, _) if not_run => missing("didn't run"),
        (None, _) => missing("not recorded"),
    };
    row(&mut b, "Ended", &ended, None);
    let (took, split) = match (&node.took, node.status, node.started_at) {
        (Some(t), _, _) => (
            format!(r#"<span class="mono">{}</span>"#, text(t)),
            match (&node.compile, &node.execute) {
                (Some(c), Some(e)) => Some(format!("compile {c} · execute {e}")),
                _ => None,
            },
        ),
        // Counted by the page as it runs.
        (None, NodeRunStatus::Running, Some(at)) => (
            format!(
                r#"<span class="mono lv-so-far" data-since="{}">so far</span>"#,
                at.unix_millis()
            ),
            None,
        ),
        (None, _, _) if not_run => (missing("didn't run"), None),
        (None, _, _) => (missing("not timed"), None),
    };
    row(&mut b, "Time taken", &took, split.as_deref());
    let built = !matches!(node.status, NodeRunStatus::Error | NodeRunStatus::Skipped);
    match (node.rows_affected, node.rows_missing) {
        (None, _) if !built => row(
            &mut b,
            "Rows affected",
            &missing("the node didn't build"),
            Some("the node didn't build"),
        ),
        (Some(rows), _) => row(
            &mut b,
            "Rows affected",
            &format!(r#"<span class="mono">{rows}</span>"#),
            Some("from the adapter response"),
        ),
        (None, why) => {
            let why = why.unwrap_or("not reported");
            row(&mut b, "Rows affected", &missing(why), Some(why));
        }
    }
    if !node.adapter.is_empty() {
        let extras: Vec<String> = node
            .adapter
            .iter()
            .map(|(k, v)| format!("{} {}", text(k), text(v)))
            .collect();
        row(
            &mut b,
            "Adapter",
            &extras.join("<br>"),
            Some("as the adapter reported it"),
        );
    }
    let thread = node.thread.as_deref().map_or_else(
        || missing(if not_run { "didn't run" } else { "not said" }),
        |t| text(t).into_owned(),
    );
    row(&mut b, "Thread", &thread, None);
    if let Some(relation) = relation {
        let link_html = link.map_or_else(String::new, |fields| {
            warehouse_link(
                fields,
                "wh-link",
                &|label| format!("{label} ↗"),
                RUN_LINK_TITLE,
            )
        });
        let value = if link_html.is_empty() {
            format!(r#"<span class="mono lv-rel">{}</span>"#, dotted(relation))
        } else {
            format!(
                r#"<span class="mono lv-rel">{}</span> {link_html}"#,
                dotted(relation)
            )
        };
        // Where the manifest puts it, never a check that it exists (#329).
        let sub = if link_html.is_empty() {
            link.map(no_link_reason).filter(|r| !r.is_empty())
        } else {
            Some("expected location, not checked".to_owned())
        };
        row(&mut b, "Relation", &value, sub.as_deref());
    }
    if let Some((why, note)) = why {
        row(&mut b, "Why it ran", &text(why), Some(note));
    }
    let (tests, tests_sub) = tests_said(node, about);
    row(&mut b, "Tests", &tests, tests_sub);
    if !node.blocked_by.is_empty() {
        let blocked: Vec<String> = node
            .blocked_by
            .iter()
            .map(|n| node_name(&n.node, &n.name, ""))
            .collect();
        row(&mut b, "Blocked by", &blocked.join(", "), None);
    }
    b.push_str("</dl></section>");
    b
}

/// What the card says beside the node's own stats.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct About<'a> {
    /// Its relation, from the graph.
    pub(crate) relation: Option<&'a str>,
    /// Where the relation is in the warehouse's UI, or why there is no link.
    pub(crate) link: Option<&'a RelationLinkFields>,
    /// Why it ran, and what that reason is.
    pub(crate) why: Option<(&'a str, &'a str)>,
    /// Its language, e.g. `python`.
    pub(crate) language: Option<&'a str>,
    /// How it materializes, e.g. `table`.
    pub(crate) materialization: Option<&'a str>,
    /// Build, run or test.
    pub(crate) mode: Option<ExecutionMode>,
    /// Whether the run ended.
    pub(crate) over: bool,
}

/// A relation that breaks between its parts, never inside a name.
fn dotted(relation: &str) -> String {
    text(relation).replace('.', ".<wbr>")
}

/// The tests on a node in this run, in words: counts when checks reported, else why
/// none did.
fn tests_said(node: &NodeStatsView, about: &About<'_>) -> (String, Option<&'static str>) {
    if let Some(t) = node.tests {
        let mut parts = vec![format!("{} passed", t.passed)];
        if t.failed > 0 {
            parts.push(format!("{} failed", t.failed));
        }
        if t.warned > 0 {
            parts.push(format!("{} warned", t.warned));
        }
        return (text(&parts.join(" · ")).into_owned(), None);
    }
    match (node.status, about.mode) {
        (NodeRunStatus::Error | NodeRunStatus::Skipped, _) => {
            ("not run".to_owned(), Some("the node did not build"))
        }
        (_, Some(ExecutionMode::Run)) => {
            ("not run".to_owned(), Some("`ods state run` runs no tests"))
        }
        (_, _) if about.over => (
            "none ran on it".to_owned(),
            Some("no test covers it, or tests were left out of this run"),
        ),
        _ => (
            "run after this node".to_owned(),
            Some(
                "`ods state build` tests each node once it builds (skip with `--exclude-resource-type test`)",
            ),
        ),
    }
}

/// `GET /state/runs/<run_id>/card?node=<id>`: the card, as an HTML fragment; 404 when
/// the run or the node isn't known. The page falls back to what its events say.
pub(super) async fn handler(
    State(state): State<Shared>,
    Path(run): Path<String>,
    Query(query): Query<CardQuery>,
) -> Response {
    blocking(move || {
        let snapshot = state.current();
        let dashboard = snapshot.dashboard();
        let names = snapshot.names();
        let graph_node = snapshot.document.nodes.iter().find(|n| n.id == query.node);
        let catalog = dashboard.catalog.nodes.iter().find(|n| n.id == query.node);
        let journal = dashboard.journal_source().and_then(|j| j.run_of(&run));
        let base = About {
            relation: graph_node.map(|n| n.relation.as_str()),
            language: catalog.and_then(|n| n.language.as_deref()),
            materialization: catalog.and_then(|n| n.materialization.as_deref()),
            mode: journal.as_ref().and_then(|j| j.summary.mode),
            over: journal
                .as_ref()
                .is_some_and(|j| j.summary.outcome.is_some()),
            ..About::default()
        };
        // The Run page's view, when the run is listed: with explanations and links.
        let view: Option<RunPageView> = dashboard.run_view(state.details, &run, &names);
        let html = if let Some(view) = &view {
            view.nodes
                .iter()
                .find(|n| n.node == query.node)
                .map(|node| {
                    let why = view.built.iter().find(|w| w.node == query.node).map(|w| {
                        (
                            w.why.as_str(),
                            "what changed since its previous recorded build",
                        )
                    });
                    card(
                        node,
                        &view.nodes,
                        &About {
                            link: view.relation_links.get(&query.node),
                            why,
                            ..base
                        },
                    )
                })
        } else {
            None
        };
        // Not listed yet (e.g. a first run, before the store exists): its journal alone.
        let html = html.or_else(|| {
            let run = journal.as_ref()?;
            let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_owned());
            let nodes = run.nodes(&name, &|_| None, state.details);
            let node = nodes.iter().find(|n| n.node == query.node)?;
            Some(card(node, &nodes, &base))
        });
        match html {
            Some(html) => {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
            }
            None => (StatusCode::NOT_FOUND, "no such node in this run").into_response(),
        }
    })
    .await
}
