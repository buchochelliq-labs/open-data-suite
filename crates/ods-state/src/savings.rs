//! What reusing builds saved in a run, estimated (#210, ADR-0029 §2).
//!
//! The estimate is the serial build time of the nodes a plan reuses: each one's last
//! measured build ([`NodeState::build_ms`](ods_core::state::NodeState::build_ms)),
//! from the snapshot the plan read. Nodes without a timing are counted, never guessed
//! (AGENTS.md rule 3), and the estimate names the runs whose timings it used.

use std::collections::BTreeSet;

use ods_core::state::{ExecutionPlan, PlanAction, RunAction, RunEntry, StateSnapshot};
use serde::Serialize;

/// The build time a run avoided by reusing builds: an estimate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Savings {
    /// Nodes the plan reused.
    pub reused: usize,
    /// Nodes the run was to build, however that went.
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

    /// Adds `other`'s counts and time to these: totals over several runs.
    pub fn add(&mut self, other: &Savings) {
        self.reused += other.reused;
        self.built += other.built;
        self.timed += other.timed;
        self.untimed += other.untimed;
        self.avoided_ms = self.avoided_ms.saturating_add(other.avoided_ms);
        let runs: BTreeSet<String> = self
            .timed_by
            .iter()
            .chain(&other.timed_by)
            .cloned()
            .collect();
        self.timed_by = runs.into_iter().collect();
    }
}

/// The savings of a run, from its entry in the run ledger: the timings frozen when
/// it was recorded (ADR-0029 §3). `built` counts the nodes it was to build, however
/// that went, as the run's own summary does.
pub fn run_savings(entry: &RunEntry) -> Savings {
    let mut estimate = Savings {
        built: entry
            .nodes
            .values()
            .filter(|n| n.action != RunAction::Reused)
            .count(),
        ..Savings::default()
    };
    let mut runs = BTreeSet::new();
    for node in entry
        .nodes
        .values()
        .filter(|n| n.action == RunAction::Reused)
    {
        estimate.reused += 1;
        match (node.build_ms, &node.timed_by) {
            (Some(ms), by) => {
                estimate.timed += 1;
                estimate.avoided_ms = estimate.avoided_ms.saturating_add(ms);
                runs.extend(by.clone());
            }
            (None, _) => estimate.untimed += 1,
        }
    }
    estimate.timed_by = runs.into_iter().collect();
    estimate
}

/// The savings of a run that reuses `reused` and builds `built` nodes, timed from
/// `before`, the snapshot its plan read. `reused` is what the run would otherwise have
/// built: a command that builds only seeds saves nothing by reusing models.
pub fn savings<'a>(
    reused: impl IntoIterator<Item = &'a str>,
    built: usize,
    before: Option<&StateSnapshot>,
) -> Savings {
    let mut estimate = Savings {
        built,
        ..Savings::default()
    };
    let mut runs = BTreeSet::new();
    for node in reused {
        estimate.reused += 1;
        let state = before.and_then(|s| s.nodes.get(node));
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

/// The savings of reusing what `plan` reuses, all of it: what a plan of every kind
/// of node would save.
pub fn plan_savings(plan: &ExecutionPlan, built: usize, before: Option<&StateSnapshot>) -> Savings {
    savings(
        plan.with_action(PlanAction::Reuse).map(|e| e.node.as_str()),
        built,
        before,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ods_core::state::{RunAction, RunEntry, RunEntryOutcome, RunNode, Timestamp};

    use super::{Savings, run_savings};

    fn entry(run: &str, nodes: &[(&str, RunNode)]) -> RunEntry {
        RunEntry::new(
            run,
            Timestamp::from_unix(10),
            RunEntryOutcome::Succeeded,
            nodes
                .iter()
                .map(|(id, n)| ((*id).to_owned(), n.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    /// A run's savings come from the timings frozen in its entry: untimed reused nodes
    /// are counted, not estimated, and every node it was to build counts as built.
    #[test]
    fn a_runs_savings_count_untimed_nodes_and_name_their_timings() {
        let saved = run_savings(&entry(
            "run-3",
            &[
                (
                    "a",
                    RunNode::new(RunAction::Reused).timed(Some(1_000), "run-1"),
                ),
                (
                    "b",
                    RunNode::new(RunAction::Reused).timed(Some(500), "run-2"),
                ),
                ("c", RunNode::new(RunAction::Reused)),
                (
                    "d",
                    RunNode::new(RunAction::Built).timed(Some(9_000), "run-3"),
                ),
                ("e", RunNode::new(RunAction::Failed)),
                ("f", RunNode::new(RunAction::Skipped)),
            ],
        ));
        assert_eq!(
            (saved.reused, saved.built, saved.timed, saved.untimed),
            (3, 3, 2, 1)
        );
        assert_eq!(saved.avoided_ms, 1_500, "a built node saves nothing");
        assert!(saved.is_lower_bound());
        assert_eq!(saved.timed_by, ["run-1", "run-2"]);
    }

    /// Totals add counts and times, merge the timing runs, and never overflow.
    #[test]
    fn totals_add_up_without_overflowing() {
        let mut total = Savings::default();
        let one = Savings {
            reused: 2,
            built: 1,
            timed: 2,
            untimed: 0,
            avoided_ms: u64::MAX - 1,
            timed_by: vec!["run-1".into()],
        };
        let two = Savings {
            reused: 1,
            built: 0,
            timed: 0,
            untimed: 1,
            avoided_ms: 10,
            timed_by: vec!["run-0".into(), "run-1".into()],
        };
        total.add(&one);
        total.add(&two);
        assert_eq!((total.reused, total.built, total.untimed), (3, 1, 1));
        assert_eq!(total.avoided_ms, u64::MAX);
        assert_eq!(total.timed_by, ["run-0", "run-1"]);
    }
}
