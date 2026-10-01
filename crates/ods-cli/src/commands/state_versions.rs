//! Sources' data versions from the warehouse (#17, ADR-0022).
//!
//! The CLI only wires: it maps the dbt adapter to a change provider, built over the dbt
//! executor's relation probe, asks it about every source of the project, and hands the
//! answers to the planner, which picks each source's version (`ods_state::
//! choose_source_versions`). Only this module names a warehouse.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::state::Timestamp;
use ods_provider_databricks::DeltaVersions;
use ods_provider_dbt::executor::DbtExecutor;
use ods_sdk::contracts::changes::{ChangeProvider, RequestedSource, SourceVersion};
use ods_state::{VersionAnswer, VersionReading};

use super::state_plan::{Workspace, block_on};
use crate::exit::CliError;

/// Whether the dbt adapter `adapter_type` has a change provider.
pub(super) fn has_change_provider(adapter_type: Option<&str>) -> bool {
    // Delta table versions, read through dbt's own connection (ADR-0022 §1).
    adapter_type == Some("databricks")
}

/// The change provider for the dbt adapter `adapter_type`, if it has one.
pub(super) fn change_provider(
    adapter_type: Option<&str>,
    executor: &DbtExecutor,
) -> Option<DeltaVersions<DbtExecutor>> {
    has_change_provider(adapter_type).then(|| DeltaVersions::new(executor.clone()))
}

/// Whether the commands that run dbt read table versions for this project, so
/// `ods state plan`, which doesn't, says it didn't.
pub(super) fn reads_table_versions(ws: &Workspace) -> bool {
    has_change_provider(ws.manifest.adapter_type.as_deref()) && !ws.project.sources.is_empty()
}

/// Reads the table version of every source of the project, when the adapter has a
/// change provider, and has the planner pick each source's version again. Returns the
/// reading, for recording, and nothing for other adapters or a project without
/// sources.
pub(super) fn read_table_versions(
    ws: &mut Workspace,
    executor: &DbtExecutor,
    warnings: &mut Vec<String>,
) -> Result<Option<VersionReading>, CliError> {
    let Some(provider) = change_provider(ws.manifest.adapter_type.as_deref(), executor) else {
        return Ok(None);
    };
    if ws.project.sources.is_empty() {
        return Ok(None);
    }
    // Every source, not only those that could change a decision: a source without a
    // version makes its readers build, so it would never be asked about and never get
    // a baseline.
    let sources: Vec<RequestedSource> = ws
        .project
        .sources
        .iter()
        .map(|s| RequestedSource::new(s.id.clone(), s.name.clone()))
        .collect();
    let reading = versions(&provider, &sources, warnings)?;
    ws.add_reading(reading.clone());
    Ok(Some(reading))
}

/// Asks `provider` for the versions of `sources`, in one call, as a reading taken when
/// the call started: a version read later can't show data older than that. A failed
/// call leaves every source unknown, with a warning; a source answered twice, or not
/// at all, is unknown too (AGENTS.md rule 3).
pub(super) fn versions<C: ChangeProvider + ?Sized>(
    provider: &C,
    sources: &[RequestedSource],
    warnings: &mut Vec<String>,
) -> Result<VersionReading, CliError> {
    let observed_at = Timestamp::now();
    let mut answers: BTreeMap<String, VersionAnswer> = BTreeMap::new();
    match block_on(provider.versions(sources))? {
        Ok(report) => {
            let mut seen = BTreeSet::new();
            for (id, version) in report.sources {
                let answer = match version {
                    SourceVersion::Version(v) => VersionAnswer::Version(v),
                    SourceVersion::Unknown(why) => VersionAnswer::Unknown(why),
                    _ => VersionAnswer::Unknown("an answer ODS doesn't understand".to_owned()),
                };
                if !seen.insert(id.clone()) {
                    answers.insert(id, VersionAnswer::Unknown("answered twice".to_owned()));
                    continue;
                }
                answers.insert(id, answer);
            }
        }
        Err(e) => {
            warnings.push(format!(
                "couldn't read the sources' table versions, so they are unknown and the nodes reading them are built, unless source freshness vouches for them: {e}"
            ));
            // dbt's error goes in the warning only: evidence stays short and fixed.
            let why = "the table-version probe failed; see the warning".to_owned();
            answers.extend(
                sources
                    .iter()
                    .map(|s| (s.id.clone(), VersionAnswer::Unknown(why.clone()))),
            );
        }
    }
    // Only what was asked about.
    let asked: BTreeSet<&str> = sources.iter().map(|s| s.id.as_str()).collect();
    answers.retain(|id, _| asked.contains(id.as_str()));
    for source in sources {
        answers.entry(source.id.clone()).or_insert_with(|| {
            VersionAnswer::Unknown("the table version probe didn't report on it".to_owned())
        });
    }
    Ok(VersionReading::new(
        provider.info().capabilities,
        Some(observed_at),
        answers,
    ))
}

#[cfg(test)]
mod tests {
    use ods_core::Capability;
    use ods_provider_fake::FakeChangeProvider;

    use super::*;

    fn sources(ids: &[&str]) -> Vec<RequestedSource> {
        ids.iter()
            .map(|id| RequestedSource::new(*id, *id))
            .collect()
    }

    #[test]
    fn each_answer_is_kept_and_nothing_else() {
        let provider = FakeChangeProvider::new()
            .with_source("a")
            .unreadable("b", "a view")
            .with_source("not asked");
        let mut warnings = Vec::new();
        let reading = versions(&provider, &sources(&["a", "b", "c"]), &mut warnings).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(reading.capabilities.contains(&Capability::RelationVersions));
        assert!(reading.observed_at.is_some());
        assert!(matches!(reading.answers["a"], VersionAnswer::Version(_)));
        assert_eq!(
            reading.answers["b"],
            VersionAnswer::Unknown("a view".to_owned())
        );
        assert!(matches!(reading.answers["c"], VersionAnswer::Unknown(_)));
        assert_eq!(reading.answers.len(), 3);
    }

    #[test]
    fn a_failed_probe_leaves_every_source_unknown_and_warns() {
        let provider = FakeChangeProvider::new().with_source("a").failing();
        let mut warnings = Vec::new();
        let reading = versions(&provider, &sources(&["a", "b"]), &mut warnings).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("table versions"), "{warnings:?}");
        assert!(warnings[0].contains("can't be reached"), "{warnings:?}");
        let fixed =
            VersionAnswer::Unknown("the table-version probe failed; see the warning".to_owned());
        assert!(reading.answers.values().all(|a| *a == fixed), "{reading:?}");
        assert_eq!(reading.answers.len(), 2);
    }

    #[test]
    fn only_the_databricks_adapter_has_a_change_provider() {
        let executor = DbtExecutor::new("dbt", "target");
        assert!(change_provider(Some("databricks"), &executor).is_some());
        for other in [Some("duckdb"), Some("snowflake"), None] {
            assert!(change_provider(other, &executor).is_none());
        }
    }
}
