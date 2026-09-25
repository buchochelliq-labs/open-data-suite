//! Turns a finished run into the next snapshot (ADR-0013, "Snapshots").

use std::collections::{BTreeMap, BTreeSet};

use ods_core::state::{DataVersion, NodeState, SnapshotId, StateSnapshot, TestRecord, Timestamp};
use serde::Serialize;

use crate::Project;

/// How a node's run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome {
    /// Built successfully.
    Success,
    /// Ran and failed.
    Failed,
    /// Not run (e.g. an upstream failure).
    Skipped,
}

/// One node's result in a run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunResult {
    /// Node id.
    pub node: String,
    /// How it ended.
    pub outcome: Outcome,
    /// When it finished, if known.
    pub completed_at: Option<Timestamp>,
    /// Whether its checks ran with it and all passed (e.g. `dbt build`). A build that
    /// ran without checks leaves the node untested.
    pub tested: bool,
}

impl RunResult {
    /// A result.
    pub fn new(node: impl Into<String>, outcome: Outcome, completed_at: Option<Timestamp>) -> Self {
        Self {
            node: node.into(),
            outcome,
            completed_at,
            tested: false,
        }
    }

    /// Marks a successful build whose checks ran and passed.
    #[must_use]
    pub fn tested(mut self) -> Self {
        self.tested = true;
        self
    }
}

/// The next snapshot, and what happened to each node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Recorded {
    /// The snapshot to commit.
    #[serde(skip)]
    pub snapshot: StateSnapshot,
    /// Nodes whose state advanced to this run.
    pub advanced: Vec<String>,
    /// Nodes in the run that kept their previous state, and why.
    pub kept: BTreeMap<String, String>,
    /// Results for nodes the project doesn't have, ignored.
    pub ignored: Vec<String>,
}

/// The snapshot after `results`: nodes that succeeded advance to their current
/// fingerprint and the source versions known to predate the run; every other node keeps
/// its previous state (AGENTS.md rule 5).
///
/// `sources_predate_run` says whether the project's source versions were observed
/// before the run started. If not, a node could be recorded as having seen data that
/// only arrived after it was built, so its source inputs are recorded as unknown.
pub fn record(
    project: &Project,
    previous: Option<(SnapshotId, &StateSnapshot)>,
    results: &[RunResult],
    run_id: &str,
    finished_at: Timestamp,
    sources_predate_run: bool,
) -> Recorded {
    let mut nodes = previous.map(|(_, s)| s.nodes.clone()).unwrap_or_default();
    let by_id: BTreeMap<&str, &crate::Node> =
        project.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let sources: BTreeMap<&str, Option<&DataVersion>> = project
        .sources
        .iter()
        .map(|s| (s.id.as_str(), s.version.as_ref()))
        .collect();
    let mut advanced = BTreeSet::new();
    let mut kept = BTreeMap::new();
    let mut ignored = BTreeSet::new();
    let succeeded: BTreeSet<&str> = results
        .iter()
        .filter(|r| r.outcome == Outcome::Success)
        .map(|r| r.node.as_str())
        .collect();
    // Which build of each node exists after this run: this run's, or the one before.
    let run_of = |id: &str| -> Option<String> {
        if succeeded.contains(id) && by_id.get(id).is_some_and(|n| n.fingerprint.is_ok()) {
            Some(run_id.to_owned())
        } else {
            previous
                .and_then(|(_, s)| s.nodes.get(id))
                .map(|p| p.run_id.clone())
        }
    };
    for result in results {
        let Some(node) = by_id.get(result.node.as_str()) else {
            ignored.insert(result.node.clone());
            continue;
        };
        match (result.outcome, &node.fingerprint) {
            (Outcome::Success, Ok(fingerprint)) => {
                let inputs = node
                    .parents
                    .iter()
                    .filter_map(|p| {
                        sources.get(p.as_str()).map(|version| {
                            (p.clone(), version.filter(|_| sources_predate_run).cloned())
                        })
                    })
                    .collect();
                let mut state = NodeState::new(
                    fingerprint.clone(),
                    result.completed_at.unwrap_or(finished_at),
                    run_id,
                    inputs,
                );
                // Tested only against the checks it has now; with none, nothing is.
                if let (true, Some(checks)) = (result.tested, &node.checks) {
                    state.tested = Some(TestRecord::new(
                        run_id,
                        result.completed_at.unwrap_or(finished_at),
                        checks.clone(),
                    ));
                }
                state.parents = node
                    .parents
                    .iter()
                    .filter(|p| by_id.contains_key(p.as_str()))
                    .filter_map(|p| Some((p.clone(), run_of(p)?)))
                    .collect();
                nodes.insert(node.id.clone(), state);
                advanced.insert(node.id.clone());
            }
            (Outcome::Success, Err(why)) => {
                kept.insert(
                    node.id.clone(),
                    format!("succeeded, but its code can't be fingerprinted: {why}"),
                );
            }
            (outcome, _) => {
                let word = match outcome {
                    Outcome::Failed => "failed",
                    _ => "was skipped",
                };
                // A failed build (or one whose checks failed) may have replaced what
                // the kept state describes, so its checks no longer vouch for anything.
                if outcome == Outcome::Failed
                    && let Some(state) = nodes.get_mut(&node.id)
                {
                    state.tested = None;
                }
                kept.insert(
                    node.id.clone(),
                    format!("{word}; its last successful state is kept"),
                );
            }
        }
    }
    Recorded {
        snapshot: StateSnapshot::new(previous.map(|(id, _)| id), finished_at, run_id, nodes),
        advanced: advanced.into_iter().collect(),
        kept,
        ignored: ignored.into_iter().collect(),
    }
}

/// One node's checks in a test-only run (#220).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TestResult {
    /// Node id.
    pub node: String,
    /// Whether all its checks passed.
    pub passed: bool,
    /// When they finished, if known.
    pub completed_at: Option<Timestamp>,
}

impl TestResult {
    /// A result.
    pub fn new(node: impl Into<String>, passed: bool, completed_at: Option<Timestamp>) -> Self {
        Self {
            node: node.into(),
            passed,
            completed_at,
        }
    }
}

/// What a test-only run changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RecordedTests {
    /// The snapshot to commit.
    #[serde(skip)]
    pub snapshot: StateSnapshot,
    /// Nodes whose checks passed on their current build.
    pub passed: Vec<String>,
    /// Nodes whose checks failed: they are untested until they pass.
    pub failed: Vec<String>,
    /// Results for nodes ODS has no build of, or whose checks it can't identify,
    /// ignored: they stay untested.
    pub ignored: Vec<String>,
}

/// The snapshot after a test-only run: nodes whose checks passed are marked tested,
/// nodes whose checks failed are marked untested. Builds are unchanged (a test run
/// builds nothing).
pub fn record_tests(
    project: &Project,
    previous: (SnapshotId, &StateSnapshot),
    results: &[TestResult],
    run_id: &str,
    finished_at: Timestamp,
) -> RecordedTests {
    let (id, snapshot) = previous;
    let mut nodes = snapshot.nodes.clone();
    let checks: BTreeMap<&str, Option<&str>> = project
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.checks.as_deref()))
        .collect();
    let (mut passed, mut failed, mut ignored) = (Vec::new(), Vec::new(), Vec::new());
    for result in results {
        let (Some(state), Some(node_checks)) = (
            nodes.get_mut(&result.node),
            checks.get(result.node.as_str()),
        ) else {
            ignored.push(result.node.clone());
            continue;
        };
        match (result.passed, node_checks) {
            (true, Some(c)) => {
                state.tested = Some(TestRecord::new(
                    run_id,
                    result.completed_at.unwrap_or(finished_at),
                    *c,
                ));
                passed.push(result.node.clone());
            }
            // Nothing identifies the checks that passed: they vouch for nothing.
            (true, None) => {
                state.tested = None;
                ignored.push(result.node.clone());
            }
            (false, _) => {
                state.tested = None;
                failed.push(result.node.clone());
            }
        }
    }
    passed.sort();
    failed.sort();
    ignored.sort();
    RecordedTests {
        snapshot: StateSnapshot::new(Some(id), finished_at, run_id, nodes),
        passed,
        failed,
        ignored,
    }
}
