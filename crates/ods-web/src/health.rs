//! Health badges and coverage (#354) on the dashboard. Badges come from the health
//! engine (`ods-health`, ADR-0030) as `[health]` configures it; this module turns the
//! Catalog's facts into the engine's and computes coverage.
//!
//! Nothing here is a score or a trend: those are #117's.

use ods_health::{BuildFacts, CheckScope, LastFailures, NodeFacts};
pub use ods_health::{HEALTHS, Health, HealthBadge};
use serde::Serialize;

use crate::catalog::{CatalogInput, CatalogNode, NodeLink, node_href};
use crate::dashboard::state::LastRun;
use crate::freshness::FreshnessInput;

/// The engine's facts about `node`: the project's, and its last build from the state
/// store.
pub(crate) fn facts(input: &CatalogInput, node: &CatalogNode) -> NodeFacts {
    let mut facts = NodeFacts::new(&node.id, &node.name, &node.resource_type);
    facts.path.clone_from(&node.file);
    facts.tags.clone_from(&node.tags);
    facts.tests = node.tests.len();
    facts.test_types = node.tests.iter().map(|t| t.name.clone()).collect();
    facts.described = node
        .description
        .as_deref()
        .is_some_and(|d| !d.trim().is_empty());
    facts.constraints = node.columns.iter().map(|c| c.constraints.len()).sum();
    facts.build = input.last_builds.get(&node.id).map(|b| {
        let build = BuildFacts::new(&b.run_id, b.built_at);
        match &b.tested {
            Some((run, at)) => build.tested(run, *at, b.checks_current),
            None => build,
        }
    });
    facts
}

/// What `ods health check` runs the engine on: every node of the Catalog, as the
/// dashboard badges them, and `last_run`'s failures, if its record says.
pub fn check_scope(input: &CatalogInput, last_run: Option<LastFailures>) -> CheckScope {
    CheckScope::new(
        input.nodes.iter().map(|node| facts(input, node)).collect(),
        last_run,
    )
}

/// What the last run's record says failed; `None` when it doesn't say.
pub(crate) fn failures_of(last: Option<&LastRun>) -> Option<LastFailures> {
    let last = last?;
    let outcome = last.outcome.as_ref()?;
    Some(LastFailures::new(
        last.started_at,
        &last.command_name,
        outcome.failed.iter().cloned(),
        outcome.skipped.iter().cloned(),
    ))
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
    /// Its target's verdict, when `[health.coverage]` sets one (ADR-0030 §3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ods_health::CoverageFinding>,
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
        target: None,
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
        target: None,
    });
    rows
}

/// What the engine judges coverage targets on: each measure's count.
pub fn measured(rows: &[Coverage]) -> Vec<ods_health::Measured> {
    rows.iter()
        .map(|r| ods_health::Measured::new(r.key, r.count, r.total))
        .collect()
}

/// The project's coverage, measured as the dashboard shows it, for `ods health check`.
pub fn project_coverage(
    input: &CatalogInput,
    freshness: &FreshnessInput,
) -> Vec<ods_health::Measured> {
    measured(&coverage(input, freshness))
}

/// The project's coverage, each measure with its target's verdict under `settings`.
pub(crate) fn judged(
    input: &CatalogInput,
    freshness: &FreshnessInput,
    settings: &ods_health::HealthSettings,
) -> Vec<Coverage> {
    let mut rows = coverage(input, freshness);
    let verdicts = settings.coverage(&measured(&rows));
    for row in &mut rows {
        row.target = verdicts.iter().find(|v| v.measure == row.key).cloned();
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every measure the dashboard shows can have a target, and every measure a target
    /// can be set for is one the dashboard measures (ADR-0030 §3).
    #[test]
    fn the_measures_are_the_engines() {
        let shown: Vec<&str> = coverage(&CatalogInput::default(), &FreshnessInput::default())
            .iter()
            .map(|r| r.key)
            .collect();
        let targets: Vec<&str> = ods_health::COVERAGE_MEASURES
            .iter()
            .map(|(k, _)| *k)
            .collect();
        assert_eq!(shown, targets);
    }
    use crate::catalog::{CatalogColumn, CatalogTest, LastBuild, TestKind};
    use crate::dashboard::state::LastOutcome;
    use crate::freshness::SourceInput;
    use ods_core::state::Timestamp;
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

    #[test]
    fn the_engine_sees_the_catalogs_facts() {
        let mut node = model("model.a", true);
        node.file = Some("models/marts/a.sql".into());
        node.tags = vec!["core".into()];
        let build = LastBuild::new(Some(1), "run-1", at("2026-09-28T09:00:00Z")).with_tested(
            "run-1",
            at("2026-09-28T09:05:00Z"),
            None,
            true,
        );
        let facts = facts(&input(vec![node.clone()], vec![("model.a", build)]), &node);
        assert_eq!(facts.path.as_deref(), Some("models/marts/a.sql"));
        assert_eq!(facts.tags, ["core"]);
        assert_eq!(facts.tests, 1);
        let build = facts.build.unwrap();
        assert_eq!(build.tested.unwrap().0, "run-1");
        assert!(build.checks_current);
    }

    #[test]
    fn failures_are_only_known_from_an_outcome() {
        let last = LastRun::new(
            "ods state build",
            "ods state build",
            at("2026-09-29T09:00:00Z"),
            crate::dashboard::StoreLocation::from(".ods/last_run.json"),
        );
        assert_eq!(failures_of(Some(&last)), None);
        let with = last.with_outcome(Some(LastOutcome::new(
            vec!["model.a".into()],
            vec![],
            vec![],
        )));
        assert!(failures_of(Some(&with)).unwrap().failed.contains("model.a"));
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
