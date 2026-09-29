//! How a run's per-node stats read (#322, ADR-0024): the words, times and row counts
//! `ods state run` and `ods state history --run` show, as view nodes. A stat that
//! wasn't reported reads `—`, never `0`; a rows total that misses some nodes reads
//! "at least".

use std::path::Path;

use ods_sdk::contracts::executor::ExecutionMode;
use ods_sdk::contracts::run_events::{
    NodeRunStats, NodeRunStatus, RunOutcome, RunSummary, RunTotals,
};
use serde::Serialize;

use super::state_plan::display_name;
use crate::present::{Line, Span, Tone, ViewNode};

/// What a missing stat reads as.
pub(super) const MISSING: &str = "—";

/// A run in brief, for a list of runs: how it ended, how long it took, and its totals.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct RunBrief {
    /// How it ended; `None` if its journal doesn't say.
    pub(super) outcome: Option<RunOutcome>,
    /// From start to finish.
    pub(super) duration_ms: Option<u64>,
    /// Whether it was reported as it ran.
    pub(super) live: bool,
    /// Its totals.
    pub(super) totals: RunTotals,
}

impl RunBrief {
    /// The run `run_id` beside `state_db`, if it has a journal that reads.
    pub(super) fn of_run(state_db: &Path, run_id: &str) -> Option<Self> {
        let path = super::run_journal::path_for(state_db, run_id)?;
        let journal = super::run_journal::read(&path).ok()??;
        let run = RunSummary::from_events(&journal.events);
        Some(Self {
            outcome: run.outcome,
            duration_ms: run.totals.duration_ms,
            live: run.live,
            totals: run.totals,
        })
    }
}

/// A duration, for people: `850ms`, `4.2s`, `2m 05s`.
pub(super) fn duration(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..60_000 => {
            let tenths = (ms + 50) / 100;
            format!("{}.{}s", tenths / 10, tenths % 10)
        }
        _ => {
            let seconds = (ms + 500) / 1_000;
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        }
    }
}

/// A node's time taken, or `—`.
pub(super) fn took(stats: &NodeRunStats) -> String {
    stats.took_ms().map_or_else(|| MISSING.to_owned(), duration)
}

/// A node's rows affected, or `—`.
pub(super) fn rows(stats: &NodeRunStats) -> String {
    stats
        .rows_affected
        .map_or_else(|| MISSING.to_owned(), |r| r.to_string())
}

/// What a success is called: a test run builds nothing, it tests.
fn success_word(mode: Option<ExecutionMode>) -> &'static str {
    if mode == Some(ExecutionMode::Test) {
        "tested"
    } else {
        "built"
    }
}

/// A status as the run shows it, toned; `mode` is the run's.
pub(super) fn status(stats: &NodeRunStats, mode: Option<ExecutionMode>) -> Span {
    match stats.status {
        NodeRunStatus::Success => Span::toned(success_word(mode), Tone::Success),
        NodeRunStatus::Error => Span::toned("failed", Tone::Error),
        NodeRunStatus::Skipped => Span::toned("skipped", Tone::Warning),
        NodeRunStatus::Queued => Span::toned("queued", Tone::Muted),
        NodeRunStatus::Running => Span::toned("running", Tone::Emphasis),
        _ => Span::toned("unknown", Tone::Warning),
    }
}

/// What else to say about a node: why it failed, what stopped it, its tests.
pub(super) fn detail(stats: &NodeRunStats) -> String {
    let mut parts = Vec::new();
    if let Some(error) = &stats.error {
        parts.push(error.message().to_owned());
    }
    if !stats.blocked_by.is_empty() {
        let names: Vec<String> = stats.blocked_by.iter().map(|n| display_name(n)).collect();
        parts.push(format!("upstream {} failed", names.join(", ")));
    }
    if let Some(tests) = stats.tests {
        let mut counts = vec![format!("{} passed", tests.passed)];
        if tests.failed > 0 {
            counts.push(format!("{} failed", tests.failed));
        }
        if tests.warned > 0 {
            counts.push(format!("{} warned", tests.warned));
        }
        if tests.skipped > 0 {
            counts.push(format!("{} skipped", tests.skipped));
        }
        parts.push(format!("tests: {}", counts.join(", ")));
    }
    parts.join("; ")
}

/// The run's totals, e.g. `13.3s · 5 built · 1 failed · 1 skipped`.
pub(super) fn totals_line(run: &RunSummary) -> String {
    let totals = &run.totals;
    let mut parts = vec![
        totals
            .duration_ms
            .map_or_else(|| format!("time {MISSING}"), duration),
    ];
    for (status, word) in [
        (NodeRunStatus::Success, success_word(run.mode)),
        (NodeRunStatus::Error, "failed"),
        (NodeRunStatus::Skipped, "skipped"),
        (NodeRunStatus::Running, "running"),
        (NodeRunStatus::Queued, "queued"),
        (NodeRunStatus::Unknown, "unknown"),
    ] {
        let n = totals.count(status);
        if n > 0 || status == NodeRunStatus::Success {
            parts.push(format!("{n} {word}"));
        }
    }
    parts.join(" · ")
}

/// Rows affected in total: `298`, `at least 298 (2 didn't report rows)`, or `—` when
/// no node that could have written rows reported any: never a made-up 0.
pub(super) fn rows_line(totals: &RunTotals) -> String {
    if totals.rows_is_lower_bound() && totals.rows_affected == 0 {
        let n = totals.rows_unreported;
        return format!(
            "{MISSING} (not reported: {n} {} didn't report rows)",
            if n == 1 { "node" } else { "nodes" }
        );
    }
    if totals.rows_is_lower_bound() {
        let n = totals.rows_unreported;
        format!(
            "at least {} ({n} {} didn't report rows)",
            totals.rows_affected,
            if n == 1 { "node" } else { "nodes" }
        )
    } else {
        totals.rows_affected.to_string()
    }
}

/// Whether the run's stats were reported as it ran, for people.
pub(super) fn source_line(run: &RunSummary) -> &'static str {
    if run.live {
        "reported as it ran"
    } else {
        "rebuilt from the final results: no live timing"
    }
}

/// A table of the run's nodes: status, time taken, rows, and more.
pub(super) fn nodes_table(run: &RunSummary) -> ViewNode {
    ViewNode::Table {
        title: None,
        columns: vec![
            "node".into(),
            "result".into(),
            "took".into(),
            "rows".into(),
            "thread".into(),
            "detail".into(),
        ],
        rows: run
            .nodes
            .iter()
            .map(|n| {
                vec![
                    vec![Span::toned(display_name(&n.node), Tone::Code)],
                    vec![status(&n.stats, run.mode)],
                    vec![Span::plain(took(&n.stats))],
                    vec![Span::plain(rows(&n.stats))],
                    vec![Span::plain(
                        n.stats.thread.as_deref().unwrap_or(MISSING).to_owned(),
                    )],
                    vec![Span::plain(detail(&n.stats))],
                ]
            })
            .collect(),
    }
}

/// The run's totals as key-value lines.
pub(super) fn totals(run: &RunSummary) -> Vec<(String, Line)> {
    vec![
        ("totals".into(), vec![Span::plain(totals_line(run))]),
        ("rows".into(), vec![Span::plain(rows_line(&run.totals))]),
        (
            "stats".into(),
            vec![Span::toned(source_line(run), Tone::Muted)],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_as_people_say_them() {
        assert_eq!(duration(0), "0ms");
        assert_eq!(duration(850), "850ms");
        assert_eq!(duration(1_900), "1.9s");
        assert_eq!(duration(4_249), "4.2s");
        assert_eq!(duration(59_990), "60.0s");
        assert_eq!(duration(125_000), "2m 05s");
    }

    #[test]
    fn a_rows_total_says_when_it_is_a_lower_bound() {
        let mut totals = RunTotals::default();
        totals.by_status.insert(NodeRunStatus::Success, 3);
        totals.rows_affected = 298;
        assert_eq!(rows_line(&totals), "298");
        totals.rows_unreported = 2;
        totals.rows_at_least = true;
        assert_eq!(
            rows_line(&totals),
            "at least 298 (2 nodes didn't report rows)"
        );
        totals.rows_unreported = 3;
        totals.rows_affected = 0;
        assert_eq!(
            rows_line(&totals),
            "— (not reported: 3 nodes didn't report rows)"
        );
        // Nothing could have written rows (everything skipped): an exact 0.
        assert_eq!(rows_line(&RunTotals::default()), "0");
    }
}
