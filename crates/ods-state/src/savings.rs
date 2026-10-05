//! What reusing builds saved in a run, estimated (#210, ADR-0029 §2).
//!
//! The estimate is the serial build time of the nodes a plan reuses: each one's last
//! measured build ([`NodeState::build_ms`](ods_core::state::NodeState::build_ms)),
//! from the snapshot the plan read. Nodes without a timing are counted, never guessed
//! (AGENTS.md rule 3), and the estimate names the runs whose timings it used.

use std::collections::BTreeSet;

use ods_core::state::{ExecutionPlan, PlanAction, StateSnapshot};
use serde::Serialize;

/// The build time a run avoided by reusing builds: an estimate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Savings {
    /// Nodes the plan reused.
    pub reused: usize,
    /// Nodes the run builds.
    pub built: usize,
    /// Reused nodes with a measured build time.
    pub timed: usize,
    /// Reused nodes without one: counted, not estimated.
    pub untimed: usize,
    /// The timed nodes' build times, summed: serial build time, so more than the
    /// wall-clock time a parallel build would have taken. A lower bound when some
    /// nodes are untimed.
    pub avoided_ms: u64,
    /// The runs whose timings the estimate used, sorted.
    pub timed_by: Vec<String>,
}

impl Savings {
    /// Whether the estimate is a lower bound: some reused nodes have no timing.
    pub fn is_lower_bound(&self) -> bool {
        self.untimed > 0
    }
}

/// The savings of a run that reuses what `plan` reuses and builds `built` nodes,
/// timed from `before`, the snapshot the plan read.
pub fn savings(plan: &ExecutionPlan, built: usize, before: Option<&StateSnapshot>) -> Savings {
    let mut estimate = Savings {
        built,
        ..Savings::default()
    };
    let mut runs = BTreeSet::new();
    for entry in plan.with_action(PlanAction::Reuse) {
        estimate.reused += 1;
        let state = before.and_then(|s| s.nodes.get(&entry.node));
        match state.and_then(|s| s.build_ms.map(|ms| (ms, &s.run_id))) {
            Some((ms, run)) => {
                estimate.timed += 1;
                estimate.avoided_ms = estimate.avoided_ms.saturating_add(ms);
                runs.insert(run.clone());
            }
            None => estimate.untimed += 1,
        }
    }
    estimate.timed_by = runs.into_iter().collect();
    estimate
}
