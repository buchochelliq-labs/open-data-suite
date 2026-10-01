//! Source checks (#232): ODS doesn't build sources, but it runs their checks (e.g. dbt
//! source tests) when their data may have changed since the checks last passed.
//!
//! A source is an input with checks. Its checks' last pass is recorded against the
//! data version measured before they ran, as a node's checks are recorded against its
//! build. They run again when:
//! - they haven't passed since ODS started recording them, or failed last time;
//! - they changed (one was added, removed or edited);
//! - the source's data version is unknown now, or was when they passed, or was measured
//!   before they passed (it can't show data that arrived since): AGENTS.md rule 3;
//! - the data version moved: the source has new data.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::state::{
    Evidence, Exactness, Reason, ReasonCode, SourceState, StateSnapshot, TestRecord, Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::{Project, Source, TestResult};

/// Whether a source's checks run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceCheckAction {
    /// Run them.
    Test,
    /// They passed on the data the source has now.
    Skip,
}

/// The decision for one source's checks, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SourceCheck {
    /// Source id.
    pub source: String,
    /// Display name, e.g. `raw.orders`.
    pub name: String,
    /// Run or skip.
    pub action: SourceCheckAction,
    /// Why, most important first.
    pub reasons: Vec<Reason>,
    /// What the decision rests on.
    pub evidence: Vec<Evidence>,
}

/// Decides, for each source in `scope` that has checks, whether they run, in id order.
/// `previous` is the recorded state of the target they would run in; `all` runs every
/// one, saying why it would otherwise have been skipped.
pub fn source_checks(
    project: &Project,
    previous: Option<&StateSnapshot>,
    scope: &BTreeSet<String>,
    all: bool,
) -> Vec<SourceCheck> {
    let mut sources: Vec<&Source> = project
        .sources
        .iter()
        .filter(|s| scope.contains(&s.id) && s.checks.is_some())
        .collect();
    sources.sort_by(|a, b| a.id.cmp(&b.id));
    sources
        .into_iter()
        .map(|source| {
            let mut evidence = Vec::new();
            let (action, mut reasons) = decide(
                source,
                previous.and_then(|s| s.sources.get(&source.id)),
                &mut evidence,
            );
            let action = if all && action == SourceCheckAction::Skip {
                if let Some(reason) = reasons.first_mut() {
                    reason
                        .message
                        .push_str("; run anyway, as every check was asked for");
                }
                SourceCheckAction::Test
            } else {
                action
            };
            SourceCheck {
                source: source.id.clone(),
                name: source.name.clone(),
                action,
                reasons,
                evidence,
            }
        })
        .collect()
}

fn decide(
    source: &Source,
    before: Option<&SourceState>,
    evidence: &mut Vec<Evidence>,
) -> (SourceCheckAction, Vec<Reason>) {
    let test = |code, message: String| (SourceCheckAction::Test, vec![Reason::new(code, message)]);
    let name = &source.name;
    let now = source.version.as_ref();
    evidence.push(Evidence::new(
        "source_data_version",
        source.id.clone(),
        now.map(|v| v.value.clone()),
        now.map_or(Exactness::None, |v| v.exactness),
    ));
    evidence.extend(source.version_evidence.iter().cloned());
    let checks = source.checks.as_deref().unwrap_or_default();
    evidence.push(Evidence::new(
        "checks",
        source.id.clone(),
        Some(checks.to_owned()),
        Exactness::Exact,
    ));
    let Some(before) = before else {
        return test(
            ReasonCode::NotTested,
            format!(
                "`{name}`'s tests haven't passed since ODS started recording them, or failed last time"
            ),
        );
    };
    if before.tested.checks.as_deref() != Some(checks) {
        return test(
            ReasonCode::ChecksChanged,
            format!(
                "`{name}`'s tests changed since they passed in run {}",
                before.tested.run_id
            ),
        );
    }
    let Some(now) = now.filter(|v| v.exactness.allows_reuse()) else {
        return test(
            ReasonCode::MissingDataEvidence,
            format!("`{name}`'s data version is unknown"),
        );
    };
    // A version measured before the tests last passed can't show data that arrived
    // after them.
    if source.observed_at.is_none_or(|at| at <= before.tested.at) {
        return test(
            ReasonCode::MissingDataEvidence,
            format!(
                "`{name}`'s data version was measured before its tests last passed ({}), so it can't show newer data",
                before.tested.at
            ),
        );
    }
    let Some(then) = before
        .version
        .as_ref()
        .filter(|v| v.exactness.allows_reuse())
    else {
        return test(
            ReasonCode::MissingDataEvidence,
            format!(
                "the data `{name}`'s tests passed on in run {} is unknown",
                before.tested.run_id
            ),
        );
    };
    if now != then {
        return test(
            ReasonCode::NewUpstreamData,
            format!(
                "`{name}` has new data ({} {}, was {})",
                now.source, now.value, then.value
            ),
        );
    }
    (
        SourceCheckAction::Skip,
        vec![Reason::new(
            ReasonCode::Unchanged,
            format!(
                "`{name}` has no new data since its tests passed in run {}",
                before.tested.run_id
            ),
        )],
    )
}

/// What recording a run's source checks changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RecordedSources {
    /// Sources whose checks all passed, recorded against their data version.
    pub passed: Vec<String>,
    /// Sources whose checks failed: they run again next time, whatever the data.
    pub failed: Vec<String>,
    /// Results for sources the project doesn't have, or whose checks it can't
    /// identify: nothing is recorded for them, so they run again.
    pub ignored: Vec<String>,
}

impl RecordedSources {
    /// Whether anything was recorded.
    pub fn is_empty(&self) -> bool {
        self.passed.is_empty() && self.failed.is_empty() && self.ignored.is_empty()
    }
}

/// Records source checks' results on `snapshot`, the next snapshot: a pass is recorded
/// against the source's data version, a failure removes the record so the checks run
/// again. Results that don't say whether every check passed (not in `results`) leave
/// the record as it was.
///
/// `sources_predate_run` says whether the project's source versions were measured
/// before the checks ran. If not, the checks may have passed on newer data than the
/// version says, so the pass is recorded without one (and vouches for none).
pub fn record_source_checks(
    snapshot: &mut StateSnapshot,
    project: &Project,
    results: &[TestResult],
    finished_at: Timestamp,
    sources_predate_run: bool,
) -> RecordedSources {
    let by_id: BTreeMap<&str, &Source> =
        project.sources.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut recorded = RecordedSources::default();
    for result in results {
        let checks = by_id
            .get(result.node.as_str())
            .and_then(|s| Some((*s, s.checks.as_deref()?)));
        match (result.passed, checks) {
            (true, Some((source, checks))) => {
                let known = sources_predate_run;
                snapshot.sources.insert(
                    source.id.clone(),
                    SourceState::new(
                        source.version.clone().filter(|_| known),
                        source.observed_at.filter(|_| known),
                        TestRecord::new(
                            snapshot.run_id.clone(),
                            result.completed_at.unwrap_or(finished_at),
                            checks,
                        ),
                    ),
                );
                recorded.passed.push(result.node.clone());
            }
            (true, None) => {
                // Nothing identifies what passed: it vouches for nothing.
                snapshot.sources.remove(&result.node);
                recorded.ignored.push(result.node.clone());
            }
            (false, _) => {
                snapshot.sources.remove(&result.node);
                recorded.failed.push(result.node.clone());
            }
        }
    }
    recorded.passed.sort();
    recorded.failed.sort();
    recorded.ignored.sort();
    recorded
}

#[cfg(test)]
mod tests {

    #[test]
    fn source_checks_round_trip_through_json() {
        let check = SourceCheck {
            source: "source.p.raw.orders".into(),
            name: "raw.orders".into(),
            action: SourceCheckAction::Test,
            reasons: vec![Reason::new(ReasonCode::NewUpstreamData, "new data")],
            evidence: Vec::new(),
        };
        let json = serde_json::to_string(&check).unwrap();
        assert_eq!(serde_json::from_str::<SourceCheck>(&json).unwrap(), check);
        let recorded = RecordedSources {
            passed: vec!["a".into()],
            failed: vec!["b".into()],
            ignored: Vec::new(),
        };
        let json = serde_json::to_string(&recorded).unwrap();
        assert_eq!(
            serde_json::from_str::<RecordedSources>(&json).unwrap(),
            recorded
        );
    }

    use std::collections::BTreeMap;

    use ods_core::state::{DataVersion, SnapshotId};

    use super::*;

    fn version(at: &str) -> DataVersion {
        DataVersion::new(at, Exactness::Semantic, "max_loaded_at")
    }

    fn source(at: Option<&str>, observed: i64) -> Source {
        Source::new("source.p.raw.orders", "raw.orders", at.map(version))
            .observed_at(Some(Timestamp::from_unix(observed)))
            .with_checks(Some("c1".to_owned()))
    }

    fn project(source: Source) -> Project {
        Project::new(Vec::new(), vec![source])
    }

    fn all() -> BTreeSet<String> {
        BTreeSet::from(["source.p.raw.orders".to_owned()])
    }

    /// The snapshot after the source's checks passed at `at` on `seen`.
    fn tested(seen: Source, at: i64) -> StateSnapshot {
        let mut snapshot =
            StateSnapshot::new(None, Timestamp::from_unix(at), "run-1", BTreeMap::new());
        let recorded = record_source_checks(
            &mut snapshot,
            &project(seen),
            &[TestResult::new(
                "source.p.raw.orders",
                true,
                Some(Timestamp::from_unix(at)),
            )],
            Timestamp::from_unix(at),
            true,
        );
        assert_eq!(recorded.passed, ["source.p.raw.orders"]);
        snapshot
    }

    fn decision(
        project: &Project,
        previous: Option<&StateSnapshot>,
    ) -> (SourceCheckAction, ReasonCode) {
        let checks = source_checks(project, previous, &all(), false);
        assert_eq!(checks.len(), 1);
        (checks[0].action, checks[0].reasons[0].code)
    }

    #[test]
    fn checks_run_when_the_data_is_new_or_unknown_and_not_otherwise() {
        use SourceCheckAction::{Skip, Test};
        let then = tested(source(Some("2026-01-01T00:00:00Z"), 10), 20);
        // Never passed.
        assert_eq!(
            decision(&project(source(Some("2026-01-01T00:00:00Z"), 30)), None),
            (Test, ReasonCode::NotTested)
        );
        // Same data, measured after the pass: skipped.
        assert_eq!(
            decision(
                &project(source(Some("2026-01-01T00:00:00Z"), 30)),
                Some(&then)
            ),
            (Skip, ReasonCode::Unchanged)
        );
        // New data.
        let new = source(Some("2026-01-02T00:00:00Z"), 30);
        let checks = source_checks(&project(new), Some(&then), &all(), false);
        assert_eq!(checks[0].action, Test);
        assert_eq!(checks[0].reasons[0].code, ReasonCode::NewUpstreamData);
        assert!(
            checks[0].reasons[0]
                .message
                .starts_with("`raw.orders` has new data"),
            "{:?}",
            checks[0].reasons
        );
        // Unknown now.
        assert_eq!(
            decision(&project(source(None, 30)), Some(&then)),
            (Test, ReasonCode::MissingDataEvidence)
        );
        // Measured before the pass: it says nothing about data since.
        assert_eq!(
            decision(
                &project(source(Some("2026-01-01T00:00:00Z"), 20)),
                Some(&then)
            ),
            (Test, ReasonCode::MissingDataEvidence)
        );
        // Below semantic: not evidence enough.
        let proxy = Source::new(
            "source.p.raw.orders",
            "raw.orders",
            Some(DataVersion::new("x", Exactness::Proxy, "mtime")),
        )
        .observed_at(Some(Timestamp::from_unix(30)))
        .with_checks(Some("c1".to_owned()));
        assert_eq!(
            decision(&project(proxy), Some(&then)),
            (Test, ReasonCode::MissingDataEvidence)
        );
        // The checks changed.
        let edited = source(Some("2026-01-01T00:00:00Z"), 30).with_checks(Some("c2".to_owned()));
        assert_eq!(
            decision(&project(edited), Some(&then)),
            (Test, ReasonCode::ChecksChanged)
        );
        // `all` runs even unchanged ones, saying so.
        let forced = source_checks(
            &project(source(Some("2026-01-01T00:00:00Z"), 30)),
            Some(&then),
            &all(),
            true,
        );
        assert_eq!(forced[0].action, Test);
        assert!(forced[0].reasons[0].message.contains("run anyway"));
        // Sources without checks, or out of scope, have nothing to run.
        let unchecked = source(Some("2026-01-01T00:00:00Z"), 30).with_checks(None);
        let none = source_checks(&project(unchecked), None, &all(), false);
        assert!(none.is_empty(), "{none:?}");
        let checked = project(source(Some("2026-01-01T00:00:00Z"), 30));
        assert!(
            source_checks(&checked, None, &BTreeSet::new(), false).is_empty(),
            "{:?}",
            source_checks(&checked, None, &BTreeSet::new(), false)
        );
    }

    #[test]
    fn a_pass_without_a_prior_measurement_vouches_for_no_version() {
        let seen = source(Some("2026-01-01T00:00:00Z"), 10);
        let mut snapshot = StateSnapshot::new(
            Some(SnapshotId(1)),
            Timestamp::from_unix(20),
            "run-2",
            BTreeMap::new(),
        );
        record_source_checks(
            &mut snapshot,
            &project(seen.clone()),
            &[TestResult::new("source.p.raw.orders", true, None)],
            Timestamp::from_unix(20),
            false,
        );
        let state = &snapshot.sources["source.p.raw.orders"];
        assert_eq!(state.version, None);
        assert_eq!(state.tested.run_id, "run-2");
        assert_eq!(state.tested.at, Timestamp::from_unix(20));
        let later = source(Some("2026-01-01T00:00:00Z"), 30);
        assert_eq!(
            decision(&project(later), Some(&snapshot)),
            (SourceCheckAction::Test, ReasonCode::MissingDataEvidence)
        );
    }

    #[test]
    fn a_failure_forgets_the_last_pass_and_unknown_sources_are_ignored() {
        let mut snapshot = tested(source(Some("2026-01-01T00:00:00Z"), 10), 20);
        let recorded = record_source_checks(
            &mut snapshot,
            &project(source(Some("2026-01-01T00:00:00Z"), 30)),
            &[
                TestResult::new("source.p.raw.orders", false, None),
                TestResult::new("source.p.raw.gone", true, None),
            ],
            Timestamp::from_unix(40),
            true,
        );
        assert_eq!(recorded.failed, ["source.p.raw.orders"]);
        assert_eq!(recorded.ignored, ["source.p.raw.gone"]);
        assert!(snapshot.sources.is_empty());
        assert_eq!(
            decision(
                &project(source(Some("2026-01-01T00:00:00Z"), 50)),
                Some(&snapshot)
            ),
            (SourceCheckAction::Test, ReasonCode::NotTested)
        );
    }
}
