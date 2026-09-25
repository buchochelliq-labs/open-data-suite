//! Turns a finished run into the next snapshot (ADR-0013, "Snapshots").

use std::collections::{BTreeMap, BTreeSet};

use ods_core::state::{DataVersion, NodeState, SnapshotId, StateSnapshot, Timestamp};
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
}

impl RunResult {
    /// A result.
    pub fn new(node: impl Into<String>, outcome: Outcome, completed_at: Option<Timestamp>) -> Self {
        Self {
            node: node.into(),
            outcome,
            completed_at,
        }
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
