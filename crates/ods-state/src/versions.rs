//! Where each source's data version comes from (ADR-0022 §2).
//!
//! The composition root collects what it could read about sources' data, each reading
//! with the capabilities of what read it, and [`choose_source_versions`] picks one per
//! source with [`ods_core::choose`], in this order:
//! 1. `relation_versions`: the relation's own version, which moves on every commit;
//! 2. `source_freshness`: the newest load time a freshness check measured;
//! 3. no version, so the source counts as changed (AGENTS.md rule 3).
//!
//! The planner decides this, not the composition root, so every caller gets the same
//! order and explanation. Which strategy won, and why the ones before it didn't, is
//! recorded as evidence on the source.

use std::collections::BTreeMap;

use ods_core::state::{DataVersion, Evidence, Exactness, Timestamp};
use ods_core::{Capability, CapabilitySet, Strategy, choose};

use crate::Source;

/// What a reading says about one source.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VersionAnswer {
    /// Its current data version.
    Version(DataVersion),
    /// It couldn't be read, and why.
    Unknown(String),
}

/// Sources' data versions as one reader (e.g. a change provider, or a freshness
/// check) reported them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VersionReading {
    /// What the reader can do, e.g. `relation_versions`: which strategy it serves.
    pub capabilities: CapabilitySet,
    /// When the versions were read, no later than the reading started. A version only
    /// says something about data a node hasn't seen if it was read after the node was
    /// built.
    pub observed_at: Option<Timestamp>,
    /// Source id → answer. A source that isn't here wasn't reported.
    pub answers: BTreeMap<String, VersionAnswer>,
}

impl VersionReading {
    /// A reading.
    pub fn new(
        capabilities: CapabilitySet,
        observed_at: Option<Timestamp>,
        answers: BTreeMap<String, VersionAnswer>,
    ) -> Self {
        Self {
            capabilities,
            observed_at,
            answers,
        }
    }
}

/// The strategies, most preferred first; each serves the capability it requires.
fn strategies() -> [Strategy<Option<Capability>>; 3] {
    [
        Strategy::new(
            "relation_versions",
            [Capability::RelationVersions],
            Some(Capability::RelationVersions),
        ),
        Strategy::new(
            "source_freshness",
            [Capability::SourceFreshness],
            Some(Capability::SourceFreshness),
        ),
        Strategy::fallback("no_version", None),
    ]
}

/// The first reading that serves `capability` and has a version of `source`.
fn version_from<'r>(
    readings: &'r [VersionReading],
    capability: &Capability,
    source: &str,
) -> Option<(&'r DataVersion, &'r VersionReading)> {
    readings
        .iter()
        .filter(|r| r.capabilities.contains(capability))
        .find_map(|r| match r.answers.get(source) {
            Some(VersionAnswer::Version(v)) => Some((v, r)),
            _ => None,
        })
}

/// Why no reading gave `source` a version through `capability`.
fn why_not(readings: &[VersionReading], capability: &Capability, source: &str) -> String {
    let mut serving = readings
        .iter()
        .filter(|r| r.capabilities.contains(capability))
        .peekable();
    if serving.peek().is_none() {
        return format!("nothing read {capability}");
    }
    serving
        .find_map(|r| match r.answers.get(source) {
            Some(VersionAnswer::Unknown(why)) => Some(why.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "not reported".to_owned())
}

/// Sets each source's data version, when it was observed, and the evidence of where
/// it came from, from `readings`: a relation version over a freshness measurement,
/// and no version when neither has one.
///
/// Whatever the source had before is replaced. Versions compare equal only when value,
/// exactness and origin are equal, so a source whose version comes from another
/// strategy than last time counts as changed once.
pub fn choose_source_versions(sources: &mut [Source], readings: &[VersionReading]) {
    let strategies = strategies();
    for source in sources {
        let offered: CapabilitySet = strategies
            .iter()
            .filter_map(|s| s.value.clone())
            .filter(|c| version_from(readings, c, &source.id).is_some())
            .collect();
        let chosen = choose(&offered, &strategies).ok();
        let picked = chosen
            .as_ref()
            .and_then(|c| c.chosen.value.as_ref())
            .and_then(|capability| version_from(readings, capability, &source.id));
        source.version = picked.map(|(v, _)| v.clone());
        source.observed_at = picked.and_then(|(_, r)| r.observed_at);
        let mut evidence = vec![Evidence::new(
            "source_version_strategy",
            source.id.clone(),
            Some(
                chosen
                    .as_ref()
                    .map_or("no_version", |c| c.chosen.id)
                    .to_owned(),
            ),
            source
                .version
                .as_ref()
                .map_or(Exactness::None, |v| v.exactness),
        )];
        for skipped in chosen.iter().flat_map(|c| &c.skipped) {
            let why = skipped
                .missing
                .first()
                .map_or_else(String::new, |c| why_not(readings, c, &source.id));
            evidence.push(Evidence::new(
                "source_version_skipped",
                source.id.clone(),
                Some(format!("{}: {why}", skipped.id)),
                Exactness::None,
            ));
        }
        source.version_evidence = evidence;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ods_core::FreshnessPolicy;
    use ods_core::state::{Fingerprint, PlanAction, ReasonCode, StateSnapshot};

    use super::*;
    use crate::{Node, Project, RunResult, plan, record};

    const ID: &str = "source.p.raw.orders";

    fn table(value: &str) -> DataVersion {
        DataVersion::new(value, Exactness::Exact, "table_history")
    }

    fn loaded(value: &str) -> DataVersion {
        DataVersion::new(value, Exactness::Semantic, "max_loaded_at")
    }

    fn reading(capability: Capability, at: i64, answer: Option<VersionAnswer>) -> VersionReading {
        VersionReading::new(
            CapabilitySet::from([capability]),
            Some(Timestamp::from_unix(at)),
            answer.map(|a| (ID.to_owned(), a)).into_iter().collect(),
        )
    }

    fn chosen(readings: &[VersionReading]) -> Source {
        let mut sources = [Source::new(ID, "raw.orders", None)];
        choose_source_versions(&mut sources, readings);
        let [source] = sources;
        source
    }

    fn values(source: &Source) -> Vec<(String, String)> {
        source
            .version_evidence
            .iter()
            .map(|e| (e.kind.clone(), e.value.clone().unwrap_or_default()))
            .collect()
    }

    fn pair(kind: &str, value: &str) -> (String, String) {
        (kind.to_owned(), value.to_owned())
    }

    #[test]
    fn a_table_version_wins_over_a_load_time() {
        let source = chosen(&[
            reading(
                Capability::SourceFreshness,
                10,
                Some(VersionAnswer::Version(loaded("2026-01-01T00:00:00Z"))),
            ),
            reading(
                Capability::RelationVersions,
                20,
                Some(VersionAnswer::Version(table("t1/7"))),
            ),
        ]);
        assert_eq!(source.version, Some(table("t1/7")));
        assert_eq!(source.observed_at, Some(Timestamp::from_unix(20)));
        assert_eq!(
            values(&source),
            [pair("source_version_strategy", "relation_versions")]
        );
        assert_eq!(source.version_evidence[0].exactness, Exactness::Exact);
    }

    #[test]
    fn an_unknown_table_version_falls_back_to_the_load_time_and_says_why() {
        let source = chosen(&[
            reading(
                Capability::RelationVersions,
                20,
                Some(VersionAnswer::Unknown("not a versioned table".to_owned())),
            ),
            reading(
                Capability::SourceFreshness,
                10,
                Some(VersionAnswer::Version(loaded("2026-01-01T00:00:00Z"))),
            ),
        ]);
        assert_eq!(source.version, Some(loaded("2026-01-01T00:00:00Z")));
        assert_eq!(source.observed_at, Some(Timestamp::from_unix(10)));
        assert_eq!(
            values(&source),
            [
                pair("source_version_strategy", "source_freshness"),
                pair(
                    "source_version_skipped",
                    "relation_versions: not a versioned table"
                ),
            ]
        );
    }

    #[test]
    fn neither_gives_no_version_and_says_why_for_each() {
        let source = chosen(&[reading(Capability::RelationVersions, 20, None)]);
        assert_eq!(source.version, None);
        assert_eq!(source.observed_at, None);
        assert_eq!(
            values(&source),
            [
                pair("source_version_strategy", "no_version"),
                pair("source_version_skipped", "relation_versions: not reported"),
                pair(
                    "source_version_skipped",
                    "source_freshness: nothing read source_freshness"
                ),
            ]
        );
        // And with nothing read at all.
        assert_eq!(chosen(&[]).version, None);
    }

    #[test]
    fn a_reading_serves_only_the_strategy_it_advertises() {
        // A version from something that can't read relation versions isn't one.
        let source = chosen(&[reading(
            Capability::QueryHistory,
            20,
            Some(VersionAnswer::Version(table("t1/7"))),
        )]);
        assert_eq!(source.version, None);
    }

    fn fingerprint() -> Fingerprint {
        Fingerprint::from_content([("file", "v1"), ("config", "{}")])
    }

    fn project(source: Source) -> Project {
        let node = Node::new(
            "model.p.orders",
            "orders",
            "model",
            vec![ID.to_owned()],
            Ok(fingerprint()),
            FreshnessPolicy::conservative(),
        );
        Project::new(vec![node], vec![source])
    }

    fn all() -> BTreeSet<String> {
        BTreeSet::from(["model.p.orders".to_owned()])
    }

    /// The state after `orders` built at 100 from `source`.
    fn built(source: Source) -> StateSnapshot {
        record(
            &project(source),
            None,
            &[RunResult::new(
                "model.p.orders",
                crate::Outcome::Success,
                Some(Timestamp::from_unix(100)),
            )],
            "run-1",
            Timestamp::from_unix(100),
            true,
        )
        .snapshot
    }

    fn decision(source: Source, before: &StateSnapshot) -> (PlanAction, ReasonCode) {
        let plan = plan(
            &project(source),
            Some((ods_core::state::SnapshotId(1), before)),
            &all(),
            Timestamp::from_unix(300),
        )
        .unwrap();
        let entry = &plan.entries[0];
        (entry.action, entry.reasons[0].code)
    }

    #[test]
    fn the_planner_reuses_on_the_same_table_version_and_builds_on_any_change() {
        let at = |t: i64, v: Option<VersionAnswer>| reading(Capability::RelationVersions, t, v);
        let before = built(chosen(&[at(
            50,
            Some(VersionAnswer::Version(table("t1/7"))),
        )]));
        // Same version, read after the build.
        assert_eq!(
            decision(
                chosen(&[at(200, Some(VersionAnswer::Version(table("t1/7"))))]),
                &before
            ),
            (PlanAction::Reuse, ReasonCode::Unchanged)
        );
        // A new commit.
        assert_eq!(
            decision(
                chosen(&[at(200, Some(VersionAnswer::Version(table("t1/8"))))]),
                &before
            ),
            (PlanAction::Build, ReasonCode::NewUpstreamData)
        );
        // Unknown now, and nothing else: counts as changed.
        assert_eq!(
            decision(
                chosen(&[at(200, Some(VersionAnswer::Unknown("no".to_owned())))]),
                &before
            ),
            (PlanAction::Build, ReasonCode::MissingDataEvidence)
        );
        // The version now comes from the load time instead: a change, built once.
        let freshness = reading(
            Capability::SourceFreshness,
            200,
            Some(VersionAnswer::Version(loaded("2026-01-01T00:00:00Z"))),
        );
        assert_eq!(
            decision(chosen(std::slice::from_ref(&freshness)), &before),
            (PlanAction::Build, ReasonCode::NewUpstreamData)
        );
        // And the other way round.
        let before = built(chosen(&[reading(
            Capability::SourceFreshness,
            50,
            Some(VersionAnswer::Version(loaded("2026-01-01T00:00:00Z"))),
        )]));
        assert_eq!(
            decision(chosen(std::slice::from_ref(&freshness)), &before),
            (PlanAction::Reuse, ReasonCode::Unchanged)
        );
        assert_eq!(
            decision(
                chosen(&[
                    freshness,
                    at(200, Some(VersionAnswer::Version(table("t1/7"))))
                ]),
                &before
            ),
            (PlanAction::Build, ReasonCode::NewUpstreamData)
        );
    }

    #[test]
    fn the_plan_shows_where_each_version_came_from() {
        let before = built(chosen(&[reading(
            Capability::RelationVersions,
            50,
            Some(VersionAnswer::Version(table("t1/7"))),
        )]));
        let source = chosen(&[reading(
            Capability::RelationVersions,
            200,
            Some(VersionAnswer::Version(table("t1/7"))),
        )]);
        let plan = plan(
            &project(source),
            Some((ods_core::state::SnapshotId(1), &before)),
            &all(),
            Timestamp::from_unix(300),
        )
        .unwrap();
        assert!(
            plan.entries[0]
                .evidence
                .iter()
                .any(|e| e.kind == "source_version_strategy"
                    && e.value.as_deref() == Some("relation_versions")),
            "{:?}",
            plan.entries[0].evidence
        );
    }
}
