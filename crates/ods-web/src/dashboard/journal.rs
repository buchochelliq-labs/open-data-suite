//! Runs as their journals tell them (#322, ADR-0024): how each run ended, how long it
//! and each node took, and the rows they wrote, for the Runs and Run pages.
//!
//! The binary names the journals' directory ([`History::with_journals`]); journals are
//! read here, through ods-sdk's reader (the one `ods state history` uses), when a page
//! asks, and read again only once a file changes. Every event is redacted again as it
//! is read, so a page shows no more than the sanitized fields.
//!
//! Nothing here claims more than the journal says (AGENTS rule 3): a stat it doesn't
//! report is `None`, shown as `—` with the reason, never `0`; a run without a
//! `run_finished` never reads as a success.
//!
//! [`History::with_journals`]: super::state::History::with_journals

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use ods_core::state::{Timestamp, TimestampMs};
use ods_sdk::contracts::executor::ExecutionMode;
use ods_sdk::contracts::run_events::{
    NodeRunStats, NodeRunStatus, RunOutcome as JournalOutcome, RunSummary, TestCounts,
};
use ods_sdk::run_journal::{JournalFile, Journals, RECENT};
use serde::Serialize;

/// What a missing stat reads as.
pub const MISSING: &str = "—";

/// A journal file's version: when it changed, and its size.
type FileVersion = (SystemTime, u64);

/// Each run read, by run id, with the version of the file it was read from.
type ReadRuns = BTreeMap<String, (FileVersion, Arc<JournalRun>)>;

/// A run as its journal tells it, as last read.
#[derive(Debug, Clone)]
pub(crate) struct JournalRun {
    /// The fold of its events.
    pub(crate) summary: RunSummary,
    /// Lines that couldn't be read (a newer version, or a last line cut short).
    pub(crate) unreadable: usize,
    /// When the file last changed.
    pub(crate) modified: SystemTime,
}

/// The journals beside the store, and the runs read from them: each read again only
/// when its file changes (size or modification time), so a page lists 50 runs without
/// reading 50 files each time. Shared by every clone of one reload's facts.
#[derive(Debug, Clone, Default)]
pub(crate) struct JournalSource {
    journals: Option<Journals>,
    read: Arc<Mutex<ReadRuns>>,
}

impl JournalSource {
    pub(crate) fn new(journals: Journals) -> Self {
        Self {
            journals: Some(journals),
            read: Arc::default(),
        }
    }

    /// The journals, if the binary named where they are.
    pub(crate) fn journals(&self) -> Option<&Journals> {
        self.journals.as_ref()
    }

    /// Whether the binary named a journals directory at all.
    pub(crate) fn is_set(&self) -> bool {
        self.journals.is_some()
    }

    /// Every journal there, newest first; none if it can't be listed.
    pub(crate) fn list(&self) -> Vec<JournalFile> {
        let Some(journals) = &self.journals else {
            return Vec::new();
        };
        let files = journals.list().unwrap_or_else(|e| {
            tracing::warn!(error = %e, dir = %journals.dir().display(), "dashboard: run journals can't be listed");
            Vec::new()
        });
        // Journals pruned or removed since are forgotten.
        let listed: std::collections::BTreeSet<&str> =
            files.iter().map(|f| f.run_id.as_str()).collect();
        self.read
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|run_id, _| listed.contains(run_id.as_str()));
        files
    }

    /// The run in `file`, from the last read if the file hasn't changed since.
    pub(crate) fn run(&self, file: &JournalFile) -> Option<Arc<JournalRun>> {
        let key = (file.modified, file.len);
        {
            let read = self.read.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((at, run)) = read.get(&file.run_id)
                && *at == key
            {
                return Some(Arc::clone(run));
            }
        }
        // Read without the lock held: other requests needn't wait on this file.
        let journal = match ods_sdk::run_journal::read(&file.path) {
            Ok(Some(journal)) => journal,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(error = %e, run = %file.run_id, "dashboard: a run journal can't be read");
                return None;
            }
        };
        let run = Arc::new(JournalRun {
            summary: RunSummary::from_events(&journal.events),
            unreadable: journal.unreadable,
            modified: file.modified,
        });
        self.read
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(file.run_id.clone(), (key, Arc::clone(&run)));
        Some(run)
    }

    /// The journal of `run_id`, if there is one.
    pub(crate) fn run_of(&self, run_id: &str) -> Option<Arc<JournalRun>> {
        self.run(&self.journals.as_ref()?.file(run_id)?)
    }
}

// -------------------------------------------------------------------- view models

/// A run's totals, from its journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RunStatsView {
    /// Whether its events were reported as it ran; otherwise they were rebuilt from its
    /// final results, with no times, rows or threads.
    pub live: bool,
    /// When it started.
    pub started_at: Option<TimestampMs>,
    /// When it finished; `None` while it runs, or if it stopped without saying.
    pub finished_at: Option<TimestampMs>,
    /// How long it took, start to finish, in milliseconds.
    pub duration_ms: Option<u64>,
    /// The same, for people, e.g. `4.2s`.
    pub duration: Option<String>,
    /// How many nodes it ran, or was asked to.
    pub nodes: usize,
    /// How many nodes ended in each status.
    pub by_status: BTreeMap<NodeRunStatus, usize>,
    /// Rows written by the nodes that reported them; `None` when none did though some
    /// could have written rows.
    pub rows_affected: Option<u64>,
    /// Whether `rows_affected` is only "at least": some nodes that ran didn't report
    /// rows.
    pub rows_at_least: bool,
    /// How many nodes that ran, or may have, didn't report rows.
    pub rows_unreported: usize,
    /// The rows total for people: `298`, `at least 298` or `—`.
    pub rows: String,
    /// Lines of the journal that couldn't be read.
    pub unreadable_lines: usize,
}

impl RunStatsView {
    /// How many nodes ended as `status`.
    pub fn count(&self, status: NodeRunStatus) -> usize {
        self.by_status.get(&status).copied().unwrap_or(0)
    }
}

/// A failed node's error, as the journal keeps it: its kind and a one-line summary with
/// quoted values, numbers and SQL removed. Never more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ErrorView {
    /// E.g. `Database Error`.
    pub kind: Option<String>,
    /// The summary.
    pub message: String,
    /// Where the full message is (a log file); only on loopback.
    pub details_at: Option<String>,
}

/// One node's part in a run, from the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeStatsView {
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
    /// `model`, `seed`, … when planned now.
    pub kind: Option<String>,
    /// How it ended; `unknown` is never read as success.
    pub status: NodeRunStatus,
    /// The status for people: `built` (`tested` in a test run), `failed`, …
    pub status_label: &'static str,
    /// Whether the run was asked to build it.
    pub requested: bool,
    /// When it started.
    pub started_at: Option<TimestampMs>,
    /// When it finished.
    pub finished_at: Option<TimestampMs>,
    /// Milliseconds from the run's start to its start.
    pub start_offset_ms: Option<u64>,
    /// Milliseconds from the run's start to its finish.
    pub end_offset_ms: Option<u64>,
    /// How long it took, in milliseconds.
    pub took_ms: Option<u64>,
    /// The same, for people.
    pub took: Option<String>,
    /// Compile time, for people, when the engine timed it apart.
    pub compile: Option<String>,
    /// Execute time, for people, when the engine timed it apart.
    pub execute: Option<String>,
    /// Rows written, as the engine reported them.
    pub rows_affected: Option<u64>,
    /// Why `rows_affected` is missing, when it is.
    pub rows_missing: Option<&'static str>,
    /// The thread it ran on.
    pub thread: Option<String>,
    /// Anything else the engine reported, by its own key.
    pub adapter: BTreeMap<String, String>,
    /// Why it failed, redacted.
    pub error: Option<ErrorView>,
    /// Its tests, if any ran.
    pub tests: Option<TestCounts>,
    /// The failed nodes that stopped it, when it was skipped.
    pub blocked_by: Vec<super::state::NodeRef>,
    /// Why it failed, explained (#323, ADR-0025), when the binary gave an explainer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<ods_core::failure::ErrorExplanation>,
}

/// A duration, for people: `850ms`, `4.2s`, `2m 05s` (as `ods state run` says it).
pub fn duration(ms: u64) -> String {
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

/// How a run ended, from its journal, for the Runs page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    Succeeded,
    /// Some nodes failed, others succeeded.
    Partial,
    Failed,
    /// Not known how it ended; never read as a success.
    Unknown,
    /// No `run_finished`, and the journal changed recently: running, or stopped.
    Unfinished,
}

/// How a run ended, why that is said, and whether it is only inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub(crate) ended: Ended,
    pub(crate) note: String,
    pub(crate) inferred: bool,
}

impl JournalRun {
    /// How it ended, as of `now`. The executor's own outcome is taken only when the
    /// nodes agree: a "success" with a failed node, a node whose outcome isn't known,
    /// or lines that couldn't be read is not shown as a success (AGENTS rule 3).
    pub(crate) fn ended(&self, now: Timestamp) -> Verdict {
        let run = &self.summary;
        let count = |s| run.totals.count(s);
        let errors = count(NodeRunStatus::Error);
        let successes = count(NodeRunStatus::Success);
        let unsettled = count(NodeRunStatus::Unknown)
            + count(NodeRunStatus::Running)
            + count(NodeRunStatus::Queued);
        let verdict = |ended, note: String| Verdict {
            ended,
            note,
            inferred: false,
        };
        let failed_nodes = |said: &str| {
            if successes > 0 {
                verdict(
                    Ended::Partial,
                    format!("{said} {errors} failed, {successes} succeeded."),
                )
            } else {
                verdict(
                    Ended::Failed,
                    format!("{said} {errors} failed, and no node succeeded."),
                )
            }
        };
        match run.outcome {
            Some(_) if errors > 0 => failed_nodes(match run.outcome {
                Some(JournalOutcome::Succeeded) => {
                    "The executor said the run succeeded, but its journal has failed nodes:"
                }
                _ => "Its journal says the run failed:",
            }),
            Some(JournalOutcome::Succeeded) if unsettled > 0 || self.unreadable > 0 => verdict(
                Ended::Unknown,
                format!(
                    "The executor said the run succeeded, but {}{}{} its journal: not read as a success.",
                    if unsettled > 0 {
                        format!("{unsettled} node(s) have no outcome in")
                    } else {
                        String::new()
                    },
                    if unsettled > 0 && self.unreadable > 0 {
                        " and "
                    } else {
                        ""
                    },
                    if self.unreadable > 0 {
                        format!("{} line(s) couldn't be read from", self.unreadable)
                    } else {
                        String::new()
                    },
                ),
            ),
            Some(JournalOutcome::Succeeded) => verdict(
                Ended::Succeeded,
                "Its journal says the run succeeded, and every node did.".to_owned(),
            ),
            Some(JournalOutcome::Failed) => verdict(
                Ended::Failed,
                format!(
                    "Its journal says the run failed, though no node failed itself: {} skipped, or a test failed.",
                    count(NodeRunStatus::Skipped)
                ),
            ),
            Some(_) => verdict(
                Ended::Unknown,
                "Its journal says the run ended, but not how: it isn't read as a success."
                    .to_owned(),
            ),
            None => {
                let changed = self
                    .modified
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
                let recent = i64::try_from(RECENT.as_secs()).unwrap_or(i64::MAX);
                // A clock that went back counts as recent.
                if now.unix().saturating_sub(changed) < recent {
                    verdict(
                        Ended::Unfinished,
                        "Running or stopped without finishing: its journal doesn't say it finished, and it changed in the last 10 minutes.".to_owned(),
                    )
                } else {
                    // A single long node writes nothing while it runs: only inferred.
                    Verdict {
                        ended: Ended::Unknown,
                        note: "Probably stopped: no event for over 10 minutes, and its journal doesn't say it finished. A node that runs longer than that writes nothing meanwhile, so it may still be running.".to_owned(),
                        inferred: true,
                    }
                }
            }
        }
    }

    /// When it started, to the second, if it said.
    pub(crate) fn started(&self) -> Option<Timestamp> {
        self.summary.started_at.map(TimestampMs::to_seconds)
    }

    /// Its totals.
    pub(crate) fn stats(&self) -> RunStatsView {
        let run = &self.summary;
        let reported: Vec<u64> = run
            .nodes
            .iter()
            .filter_map(|n| n.stats.rows_affected)
            .collect();
        let totals = &run.totals;
        // Nothing ran, or it never said it started: no total, not a made-up 0.
        let rows_affected = if (reported.is_empty() && totals.rows_unreported > 0)
            || run.nodes.is_empty()
            || run.started_at.is_none()
        {
            None
        } else {
            Some(totals.rows_affected)
        };
        let rows = match rows_affected {
            None => MISSING.to_owned(),
            Some(n) if totals.rows_at_least => format!("at least {n}"),
            Some(n) => n.to_string(),
        };
        RunStatsView {
            live: run.live,
            started_at: run.started_at,
            finished_at: run.finished_at,
            duration_ms: totals.duration_ms,
            duration: totals.duration_ms.map(duration),
            nodes: run.nodes.len(),
            by_status: totals.by_status.clone(),
            rows_affected,
            rows_at_least: totals.rows_at_least,
            rows_unreported: totals.rows_unreported,
            rows,
            unreadable_lines: self.unreadable,
        }
    }

    /// Each node's stats, named by `name`, with kinds from `kind`; `details` keeps
    /// where an error's full text is (a local path).
    pub(crate) fn nodes(
        &self,
        name: &dyn Fn(&str) -> String,
        kind: &dyn Fn(&str) -> Option<String>,
        details: bool,
    ) -> Vec<NodeStatsView> {
        let run = &self.summary;
        let start = run.started_at;
        run.nodes
            .iter()
            .map(|n| {
                let s = &n.stats;
                let offset = |at: Option<TimestampMs>| at?.millis_since(start?);
                NodeStatsView {
                    node: n.node.clone(),
                    name: name(&n.node),
                    kind: kind(&n.node),
                    status: s.status,
                    status_label: status_label(s.status, run.mode),
                    requested: n.requested,
                    started_at: s.started_at,
                    finished_at: s.finished_at,
                    start_offset_ms: offset(s.started_at),
                    end_offset_ms: offset(s.finished_at),
                    took_ms: s.took_ms(),
                    took: s.took_ms().map(duration),
                    compile: s.compile_ms.map(duration),
                    execute: s.execute_ms.map(duration),
                    rows_affected: s.rows_affected,
                    rows_missing: rows_missing(s, run.live),
                    thread: s.thread.clone(),
                    adapter: s.adapter.clone(),
                    error: s.error.as_ref().map(|e| ErrorView {
                        kind: e.kind().map(str::to_owned),
                        // The kind is shown apart; its repeat at the start of the
                        // message is dropped, nothing else.
                        message: e
                            .kind()
                            .and_then(|k| e.message().strip_prefix(k))
                            .and_then(|rest| rest.strip_prefix(": "))
                            .filter(|rest| !rest.trim().is_empty())
                            .unwrap_or(e.message())
                            .to_owned(),
                        details_at: details.then(|| e.details_at().map(str::to_owned)).flatten(),
                    }),
                    tests: s.tests,
                    blocked_by: s
                        .blocked_by
                        .iter()
                        .map(|b| super::state::NodeRef {
                            node: b.clone(),
                            name: name(b),
                        })
                        .collect(),
                    explanation: None,
                }
            })
            .collect()
    }
}

/// A status for people; a success in a test run tested, it didn't build.
pub(crate) fn status_label(status: NodeRunStatus, mode: Option<ExecutionMode>) -> &'static str {
    match status {
        NodeRunStatus::Success if mode == Some(ExecutionMode::Test) => "tested",
        NodeRunStatus::Success => "built",
        NodeRunStatus::Error => "failed",
        NodeRunStatus::Skipped => "skipped",
        NodeRunStatus::Queued => "queued",
        NodeRunStatus::Running => "running",
        _ => "unknown",
    }
}

/// Why a node's rows are missing, if they are.
fn rows_missing(stats: &NodeRunStats, live: bool) -> Option<&'static str> {
    if stats.rows_affected.is_some() {
        return None;
    }
    Some(match stats.status {
        NodeRunStatus::Queued => "didn't run",
        // A failed or skipped node built nothing: not a stat the adapter left out.
        NodeRunStatus::Skipped | NodeRunStatus::Error => "the node didn't build",
        _ if !live => "not recorded: this run's stats came from its final results only",
        NodeRunStatus::Running => "still running",
        _ => "not reported by the adapter",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_as_people_say_them() {
        assert_eq!(duration(0), "0ms");
        assert_eq!(duration(850), "850ms");
        assert_eq!(duration(4_249), "4.2s");
        assert_eq!(duration(125_000), "2m 05s");
    }
}
