//! Health badges and coverage (#354): each from a signal ODS really has, with how it
//! was worked out, or said to be not measured (AGENTS rules 3 and 4).
//!
//! - A node's badge comes from the last run's record (what failed or was skipped), its
//!   last successful build, and whether its tests passed on that build, as they are now.
//! - Coverage counts what the project declares: tests, descriptions, column constraints,
//!   and sources whose new data can be measured.
//!
//! Nothing here is a score or a trend: those are #117's.

use std::collections::BTreeSet;

use ods_core::state::Timestamp;
use serde::Serialize;

use crate::catalog::{CatalogInput, CatalogNode, NodeLink, node_href};
use crate::dashboard::state::LastRun;
use crate::freshness::FreshnessInput;

/// A node's health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Health {
    /// Built, and its tests passed on that build as they are now.
    Healthy,
    /// Built, but something isn't vouched for: no tests, tests not passed on this build
    /// or changed since, or skipped in the last run.
    Warning,
    /// It failed in the last run, and hasn't been built since.
    Failing,
    /// ODS has nothing to judge it on: it was never built.
    Unknown,
}

/// Every health, in the order Home and the facet list them.
pub const HEALTHS: [Health; 4] = [
    Health::Healthy,
    Health::Warning,
    Health::Failing,
    Health::Unknown,
];

impl Health {
    /// Its key, also its query value.
    pub fn key(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Warning => "warning",
            Self::Failing => "failing",
            Self::Unknown => "unknown",
        }
    }

    /// Its label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Healthy => "Healthy",
            Self::Warning => "Warning",
            Self::Failing => "Failing",
            Self::Unknown => "Unknown",
        }
    }
    /// What it means, for people.
    pub fn how(self) -> &'static str {
        match self {
            Self::Healthy => {
                "Built, and its tests passed on that build as they are now (a seed needs none)."
            }
            Self::Warning => {
                "Built, but without tests (models and snapshots), with tests not recorded \
                 passing on this build or changed since, or skipped in the last run."
            }
            Self::Failing => "Failed in the last run, and not built since.",
            Self::Unknown => "Never built by ODS: nothing to judge it on.",
        }
    }
}

/// A node's health, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct HealthBadge {
    /// The health.
    pub health: Health,
    /// Why, most important first.
    pub reasons: Vec<String>,
}

/// What the last run says failed, when its outcome is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastFailures {
    started_at: Timestamp,
    command: String,
    failed: BTreeSet<String>,
    skipped: BTreeSet<String>,
}

impl LastFailures {
    /// From the last run's record; `None` when it doesn't say what failed.
    pub(crate) fn of(last: Option<&LastRun>) -> Option<Self> {
        let last = last?;
        let outcome = last.outcome.as_ref()?;
        Some(Self {
            started_at: last.started_at,
            command: last.command_name.clone(),
            failed: outcome.failed.iter().cloned().collect(),
            skipped: outcome.skipped.iter().cloned().collect(),
        })
    }
}

/// Kinds of node that are expected to have tests.
fn tests_expected(node: &CatalogNode) -> bool {
    matches!(node.resource_type.as_str(), "model" | "snapshot")
}

fn short(run: &str) -> String {
    run.chars().take(8).collect()
}

/// A node's health, from the last run's failures (when known), its last build, and its
/// tests.
pub(crate) fn node_health(
    input: &CatalogInput,
    failures: Option<&LastFailures>,
    node: &CatalogNode,
) -> HealthBadge {
    let build = input.last_builds.get(&node.id);
    // A failure counts until a later build replaces it (e.g. recorded elsewhere).
    let since = |f: &LastFailures| build.is_none_or(|b| b.built_at < f.started_at);
    if let Some(f) = failures.filter(|f| f.failed.contains(&node.id) && since(f)) {
        return HealthBadge {
            health: Health::Failing,
            reasons: vec![format!(
                "failed in the last run ({}, started {}), and hasn't been built since",
                f.command, f.started_at
            )],
        };
    }
    let Some(build) = build else {
        return HealthBadge {
            health: Health::Unknown,
            reasons: vec!["never built by ODS: nothing to judge it on".to_owned()],
        };
    };
    let mut warnings = Vec::new();
    if let Some(f) = failures.filter(|f| f.skipped.contains(&node.id) && since(f)) {
        warnings.push(format!(
            "skipped in the last run ({}) because something upstream failed",
            f.command
        ));
    }
    if node.tests.is_empty() {
        if tests_expected(node) {
            warnings.push("no tests: nothing checks its data".to_owned());
        }
    } else {
        match (&build.tested, build.checks_current) {
            (None, _) => warnings
                .push("its tests haven't been recorded passing on its current build".to_owned()),
            (Some((run, _)), false) => warnings.push(format!(
                "its tests changed since they passed in run {}: the new ones haven't run",
                short(run)
            )),
            (Some(_), true) => {}
        }
    }
    if !warnings.is_empty() {
        return HealthBadge {
            health: Health::Warning,
            reasons: warnings,
        };
    }
    let mut reasons = vec![format!(
        "built in run {} at {}",
        short(&build.run_id),
        build.built_at
    )];
    if let Some((run, at)) = &build.tested {
        reasons.push(format!("its tests passed in run {} at {at}", short(run)));
    } else {
        reasons.push(format!("a {} has no tests to run", node.resource_type));
    }
    HealthBadge {
        health: Health::Healthy,
        reasons,
    }
}

/// How the badges are worked out, for people.
pub(crate) fn badges_how(failures_known: bool) -> String {
    let mut how = "Failing: failed in the last run and not built since. Warning: built, but \
        without tests (models and snapshots), with tests not recorded passing on this build \
        or changed since, or skipped in the last run. Healthy: built, and its tests passed \
        on this build as they are now. Unknown: never built by ODS."
        .to_owned();
    if !failures_known {
        how.push_str(
            " The last run's record doesn't say what failed, so failures aren't measured.",
        );
    }
    how
}

/// A coverage measure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Coverage {
    /// Stable key.
    pub key: &'static str,
    /// Its label.
    pub label: &'static str,
    /// How many are covered; `None` when it isn't measured.
    pub count: Option<usize>,
    /// Out of how many.
    pub total: usize,
    /// How it was worked out, for people.
    pub how: String,
    /// What isn't covered, by name.
    pub uncovered: Vec<NodeLink>,
}

fn link(node: &CatalogNode) -> NodeLink {
    NodeLink {
        id: node.id.clone(),
        name: node.name.clone(),
        href: Some(node_href(&node.id)),
    }
}

/// One measure over the project's models.
fn over_models(
    input: &CatalogInput,
    key: &'static str,
    label: &'static str,
    how: &str,
    covered: impl Fn(&CatalogNode) -> bool,
) -> Coverage {
    let models: Vec<&CatalogNode> = input
        .nodes
        .iter()
        .filter(|n| n.resource_type == "model")
        .collect();
    let mut uncovered: Vec<NodeLink> = models
        .iter()
        .filter(|n| !covered(n))
        .map(|n| link(n))
        .collect();
    uncovered.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    Coverage {
        key,
        label,
        // Without models (e.g. the project couldn't be read) there is nothing to count.
        count: (!models.is_empty()).then(|| models.len() - uncovered.len()),
        total: models.len(),
        how: if models.is_empty() {
            "No models to measure.".to_owned()
        } else {
            how.to_owned()
        },
        uncovered,
    }
}

/// The project's coverage: tests, descriptions, constraints, and source freshness.
pub(crate) fn coverage(input: &CatalogInput, freshness: &FreshnessInput) -> Vec<Coverage> {
    let mut rows = vec![
        over_models(
            input,
            "tests",
            "Models with tests",
            "Models with at least one data or unit test in the manifest.",
            |n| !n.tests.is_empty(),
        ),
        over_models(
            input,
            "descriptions",
            "Models with descriptions",
            "Models whose own description is documented.",
            |n| {
                n.description
                    .as_deref()
                    .is_some_and(|d| !d.trim().is_empty())
            },
        ),
        over_models(
            input,
            "constraints",
            "Models with constraints",
            "Models with at least one column constraint (a contract's constraints, e.g. not_null).",
            |n| n.columns.iter().any(|c| !c.constraints.is_empty()),
        ),
    ];
    let sources = &freshness.sources;
    let mut uncovered: Vec<NodeLink> = sources
        .iter()
        .filter(|s| s.measured_with.is_none() && s.read_by_runs.is_none() && s.version.is_none())
        .map(|s| NodeLink {
            id: s.id.clone(),
            name: s.name.clone(),
            href: Some("catalog/sources".to_owned()),
        })
        .collect();
    uncovered.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    rows.push(Coverage {
        key: "source_freshness",
        label: "Sources with freshness",
        count: (!sources.is_empty()).then(|| sources.len() - uncovered.len()),
        total: sources.len(),
        how: if sources.is_empty() {
            "No sources declared: nothing to measure.".to_owned()
        } else {
            "Sources whose new data ODS can measure: a loaded_at_field or query, a \
             measured version, or a table version runs read. The rest are unknown, and \
             what reads them always builds."
                .to_owned()
        },
        uncovered,
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CatalogColumn, CatalogTest, LastBuild, TestKind};
    use crate::dashboard::state::LastOutcome;
    use crate::freshness::SourceInput;
    use std::collections::BTreeMap;

    fn at(t: &str) -> Timestamp {
        Timestamp::parse(t).unwrap()
    }

    fn model(id: &str, tested: bool) -> CatalogNode {
        let mut n = CatalogNode::new(id, id.rsplit('.').next().unwrap(), "model");
        if tested {
            n.tests = vec![CatalogTest::new("test.x", "unique", None, TestKind::Data)];
        }
        n
    }

    fn input(nodes: Vec<CatalogNode>, builds: Vec<(&str, LastBuild)>) -> CatalogInput {
        CatalogInput::new(nodes).with_last_builds(
            builds
                .into_iter()
                .map(|(id, b)| (id.to_owned(), b))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn built() -> LastBuild {
        LastBuild::new(Some(1), "run-1", at("2026-09-28T09:00:00Z"))
    }

    fn failures(failed: &[&str], skipped: &[&str]) -> LastFailures {
        LastFailures {
            started_at: at("2026-09-29T09:00:00Z"),
            command: "ods state build".into(),
            failed: failed.iter().map(|s| (*s).to_owned()).collect(),
            skipped: skipped.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn a_tested_build_is_healthy_and_says_why() {
        let node = model("model.a", true);
        let build = built().with_tested("run-1", at("2026-09-28T09:05:00Z"), None, true);
        let input = input(vec![node.clone()], vec![("model.a", build)]);
        let badge = node_health(&input, None, &node);
        assert_eq!(badge.health, Health::Healthy);
        assert!(badge.reasons[1].starts_with("its tests passed in run run-1"));
    }

    #[test]
    fn never_built_is_unknown_never_healthy() {
        let node = model("model.a", true);
        let badge = node_health(&input(vec![node.clone()], vec![]), None, &node);
        assert_eq!(badge.health, Health::Unknown);
    }

    #[test]
    fn untested_unpassed_or_changed_tests_warn() {
        let untested = model("model.a", false);
        let input_a = input(vec![untested.clone()], vec![("model.a", built())]);
        assert_eq!(
            node_health(&input_a, None, &untested).health,
            Health::Warning
        );

        let tested = model("model.b", true);
        let input_b = input(vec![tested.clone()], vec![("model.b", built())]);
        let badge = node_health(&input_b, None, &tested);
        assert_eq!(badge.health, Health::Warning);
        assert!(badge.reasons[0].contains("haven't been recorded passing"));

        let changed = built().with_tested("run-1", at("2026-09-28T09:05:00Z"), None, false);
        let input_c = input(vec![tested.clone()], vec![("model.b", changed)]);
        assert!(node_health(&input_c, None, &tested).reasons[0].contains("changed since"));
    }

    #[test]
    fn a_seed_without_tests_is_healthy_once_built() {
        let seed = CatalogNode::new("seed.s", "s", "seed");
        let input = input(vec![seed.clone()], vec![("seed.s", built())]);
        assert_eq!(node_health(&input, None, &seed).health, Health::Healthy);
    }

    #[test]
    fn a_failure_counts_until_a_later_build_replaces_it() {
        let node = model("model.a", true);
        let tested = built().with_tested("run-1", at("2026-09-28T09:05:00Z"), None, true);
        let older = input(vec![node.clone()], vec![("model.a", tested)]);
        let f = failures(&["model.a"], &[]);
        assert_eq!(node_health(&older, Some(&f), &node).health, Health::Failing);
        // Never built and failed: failing, not unknown.
        let never = input(vec![node.clone()], vec![]);
        assert_eq!(node_health(&never, Some(&f), &node).health, Health::Failing);
        // Built after the failed run started: that build stands.
        let later = LastBuild::new(Some(2), "run-2", at("2026-09-29T10:00:00Z")).with_tested(
            "run-2",
            at("2026-09-29T10:05:00Z"),
            None,
            true,
        );
        let newer = input(vec![node.clone()], vec![("model.a", later)]);
        assert_eq!(node_health(&newer, Some(&f), &node).health, Health::Healthy);
        // Skipped: a warning.
        let tested = built().with_tested("run-1", at("2026-09-28T09:05:00Z"), None, true);
        let skipped = input(vec![node.clone()], vec![("model.a", tested)]);
        let s = failures(&[], &["model.a"]);
        let badge = node_health(&skipped, Some(&s), &node);
        assert_eq!(badge.health, Health::Warning);
        assert!(badge.reasons[0].starts_with("skipped in the last run"));
    }

    #[test]
    fn failures_are_only_known_from_an_outcome() {
        let last = LastRun::new(
            "ods state build",
            "ods state build",
            at("2026-09-29T09:00:00Z"),
            crate::dashboard::StoreLocation::from(".ods/last_run.json"),
        );
        assert_eq!(LastFailures::of(Some(&last)), None);
        let with = last.with_outcome(Some(LastOutcome::new(
            vec!["model.a".into()],
            vec![],
            vec![],
        )));
        assert!(
            LastFailures::of(Some(&with))
                .unwrap()
                .failed
                .contains("model.a")
        );
        assert!(badges_how(false).contains("aren't measured"));
    }

    #[test]
    fn coverage_counts_models_and_lists_what_is_missing() {
        let mut a = model("model.a", true);
        a.description = Some("A.".into());
        let mut c = CatalogColumn::new("id");
        c.constraints = vec!["not_null".into()];
        a.columns = vec![c];
        let mut b = model("model.b", false);
        b.description = Some("  ".into());
        let seed = CatalogNode::new("seed.s", "s", "seed");
        let input = input(vec![a, b, seed], vec![]);
        let fresh = FreshnessInput::new(vec![
            SourceInput::new("source.x", "x").measured_with(Some("max(_at)".into())),
            SourceInput::new("source.y", "y"),
        ]);
        let rows = coverage(&input, &fresh);
        let got: Vec<(&str, Option<usize>, usize, Vec<&str>)> = rows
            .iter()
            .map(|r| {
                (
                    r.key,
                    r.count,
                    r.total,
                    r.uncovered.iter().map(|n| n.name.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("tests", Some(1), 2, vec!["b"]),
                ("descriptions", Some(1), 2, vec!["b"]),
                ("constraints", Some(1), 2, vec!["b"]),
                ("source_freshness", Some(1), 2, vec!["y"]),
            ]
        );
    }

    #[test]
    fn nothing_to_count_is_not_measured_never_zero() {
        let rows = coverage(&CatalogInput::default(), &FreshnessInput::default());
        assert!(rows.iter().all(|r| r.count.is_none()), "{rows:?}");
        assert!(rows.iter().all(|r| !r.how.is_empty()));
    }
}
