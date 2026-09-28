//! Retrying only what failed (#292): which of a plan's nodes a retry builds.
//!
//! A retry of the nodes that failed, or were skipped because of a failure, in the last
//! run still goes through the planner: each of them keeps the plan's decision and its
//! reasons. The retry only narrows what builds, never widens it, and never builds a
//! node on a parent that needs building but isn't built with it (AGENTS.md rule 3).

use std::collections::{BTreeMap, BTreeSet};

use ods_core::state::{ExecutionPlan, PlanAction};
use serde::Serialize;

/// A node a retry would build, but doesn't, because a parent it reads needs building
/// and isn't built in this retry: it would run on stale input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct HeldBack {
    /// The node.
    pub node: String,
    /// The first parent, in plan order, that needs building and isn't built.
    pub parent: String,
}

/// How a retry splits a plan (#292). Every list is in plan order: upstream first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RetrySplit {
    /// Nodes to retry that the plan builds: the retry builds exactly these.
    pub retried: Vec<String>,
    /// Nodes to retry that the plan now reuses: they stay reused, with the plan's
    /// reasons.
    pub reused: Vec<String>,
    /// Nodes to retry that the plan builds, but whose parent needs building and isn't
    /// built here, so they aren't built either.
    pub held_back: Vec<HeldBack>,
    /// Nodes the plan builds that weren't in the run retried (they changed since): not
    /// built.
    pub changed_since: Vec<String>,
    /// Nodes to retry that aren't in the plan at all, e.g. removed from the project.
    pub not_planned: Vec<String>,
}

/// Splits `plan` for a retry of `retry`, the nodes that failed or were skipped because
/// of a failure. `buildable` is what the command would build of the plan's BUILD set
/// after its own narrowing (`--exclude`, resource types): a node outside it isn't
/// built by the retry either, and isn't listed here.
///
/// A node to retry is held back, with its parent, when a parent it reads needs
/// building but isn't built by the retry: a node changed since, one left out, or one
/// held back itself. Plans list parents before children, so holding back cascades.
#[must_use]
pub fn split_retry(
    plan: &ExecutionPlan,
    retry: &BTreeSet<String>,
    buildable: &BTreeSet<String>,
) -> RetrySplit {
    let actions: BTreeMap<&str, PlanAction> = plan
        .entries
        .iter()
        .map(|e| (e.node.as_str(), e.action))
        .collect();
    let mut split = RetrySplit::default();
    let mut built: BTreeSet<&str> = BTreeSet::new();
    for entry in &plan.entries {
        let id = entry.node.as_str();
        match (entry.action, retry.contains(id)) {
            (PlanAction::Build, true) if buildable.contains(id) => {
                let stale = entry.depends_on.iter().find(|p| {
                    actions.get(p.as_str()) == Some(&PlanAction::Build)
                        && !built.contains(p.as_str())
                });
                if let Some(parent) = stale {
                    split.held_back.push(HeldBack {
                        node: entry.node.clone(),
                        parent: parent.clone(),
                    });
                } else {
                    built.insert(id);
                    split.retried.push(entry.node.clone());
                }
            }
            (PlanAction::Build, false) if buildable.contains(id) => {
                split.changed_since.push(entry.node.clone());
            }
            (PlanAction::Reuse, true) => split.reused.push(entry.node.clone()),
            // Left out by the command's own narrowing, or not the retry's.
            _ => {}
        }
    }
    let planned: BTreeSet<&str> = actions.keys().copied().collect();
    split.not_planned = retry
        .iter()
        .filter(|id| !planned.contains(id.as_str()))
        .cloned()
        .collect();
    split
}

#[cfg(test)]
mod tests {
    use ods_core::FreshnessPolicy;
    use ods_core::state::{PlanEntry, Reason, ReasonCode, Timestamp};

    use super::*;

    fn entry(node: &str, action: PlanAction, parents: &[&str]) -> PlanEntry {
        let mut e = PlanEntry::new(
            node,
            node,
            "model",
            action,
            vec![Reason::new(ReasonCode::CodeChanged, "why")],
            FreshnessPolicy::conservative(),
            0,
        );
        e.depends_on = parents.iter().map(|p| (*p).to_owned()).collect();
        e
    }

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn builds_only_what_failed_and_reports_the_rest() {
        use PlanAction::{Build, Reuse};
        let plan = ExecutionPlan::new(
            None,
            Timestamp::from_unix(0),
            vec![
                entry("a", Build, &[]),
                entry("fixed", Reuse, &[]),
                entry("other", Build, &[]),
                entry("b", Build, &["a", "source.raw"]),
                entry("left", Build, &[]),
            ],
        );
        let split = split_retry(
            &plan,
            &set(&["a", "b", "fixed", "left", "gone"]),
            &set(&["a", "b", "other"]),
        );
        assert_eq!(split.retried, ["a", "b"]);
        assert_eq!(split.reused, ["fixed"]);
        assert_eq!(split.changed_since, ["other"]);
        assert!(split.held_back.is_empty());
        assert_eq!(split.not_planned, ["gone"]);
    }

    #[test]
    fn never_builds_on_a_parent_that_is_not_built() {
        use PlanAction::Build;
        let plan = ExecutionPlan::new(
            None,
            Timestamp::from_unix(0),
            vec![
                entry("changed", Build, &[]),
                entry("left", Build, &[]),
                entry("x", Build, &["changed"]),
                entry("y", Build, &["x"]),
                entry("z", Build, &["left"]),
            ],
        );
        let split = split_retry(
            &plan,
            &set(&["x", "y", "z", "left"]),
            &set(&["changed", "x", "y", "z"]),
        );
        assert!(split.retried.is_empty(), "{split:?}");
        let held: Vec<(&str, &str)> = split
            .held_back
            .iter()
            .map(|h| (h.node.as_str(), h.parent.as_str()))
            .collect();
        // `y` waits on `x`, held back itself; `z` on `left`, which isn't buildable.
        assert_eq!(held, [("x", "changed"), ("y", "x"), ("z", "left")]);
        assert_eq!(split.changed_since, ["changed"]);
    }
}
