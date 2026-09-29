//! Delta table versions as sources' data versions (#17, ADR-0022).
//!
//! Every commit to a Delta table moves its version, so the same version means the same
//! data: `exact` evidence. [`DeltaVersions`] reads each source's table id and latest
//! version through any [`RelationProbe`], e.g. the dbt executor's, so ODS handles no
//! warehouse credential (AGENTS.md rule 9):
//! - it asks only about tables the probe confirms are Delta, and checks the format
//!   `DESCRIBE DETAIL` reports as well;
//! - the version is `<table id>/<version>`. The table id makes it unique: a table
//!   dropped and created again gets a new id and starts again at version 0;
//! - anything missing, empty or not Delta makes the source unknown, never a partial
//!   version (AGENTS.md rule 3).

use async_trait::async_trait;
use ods_core::state::{DataVersion, Exactness};
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::changes::{ChangeProvider, RequestedSource, SourceVersion, VersionReport};
use ods_sdk::contracts::probe::{
    InvalidProbe, ProbeAnswer, ProbeFilter, ProbeRequest, ProbeRow, ProbeStatement, RelationProbe,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;

/// Where the versions come from, as their [`DataVersion::source`].
pub const ORIGIN: &str = "delta_history";

/// The table's identity and format.
pub const DETAIL: &str = "DESCRIBE DETAIL {relation}";

/// Its latest commit. History is listed newest first.
pub const HISTORY: &str = "DESCRIBE HISTORY {relation} LIMIT 1";

/// The format a Delta table reports, and the probe confirms.
const DELTA: &str = "delta";

/// Reads sources' Delta table versions through a [`RelationProbe`]. It advertises
/// `relation_versions`.
#[derive(Debug, Clone)]
pub struct DeltaVersions<P> {
    probe: P,
}

impl<P: RelationProbe> DeltaVersions<P> {
    /// Reads versions through `probe`.
    pub fn new(probe: P) -> Self {
        Self { probe }
    }
}

/// The probe request: Delta tables only, their detail, then their latest commit.
///
/// # Errors
/// [`InvalidProbe`] if it were malformed; it is fixed, and a test checks it isn't.
pub fn request() -> Result<ProbeRequest, InvalidProbe> {
    ProbeRequest::new(
        ProbeFilter::kinds(["table"])?.with_format(DELTA)?,
        vec![
            ProbeStatement::new(DETAIL, ["id", "format"])?,
            ProbeStatement::new(HISTORY, ["version", "timestamp"])?,
        ],
    )
}

/// A value the row has and isn't blank.
fn value<'r>(row: Option<&'r ProbeRow>, column: &str) -> Option<&'r str> {
    row.and_then(|r| r.get(column))
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
}

/// One source's version from its probe answer.
fn version(answer: ProbeAnswer) -> SourceVersion {
    let unknown = |why: String| SourceVersion::Unknown(why);
    let rows = match answer {
        ProbeAnswer::Rows(rows) => rows,
        ProbeAnswer::Skipped(why) => return unknown(format!("not a Delta table: {why}")),
        ProbeAnswer::Unknown(why) => return unknown(why),
        _ => return unknown("an answer ODS doesn't understand".to_owned()),
    };
    let (detail, history) = (rows.first(), rows.get(1));
    match value(detail, "format") {
        Some(format) if format.eq_ignore_ascii_case(DELTA) => {}
        Some(format) => return unknown(format!("not a Delta table: its format is {format}")),
        None => return unknown("DESCRIBE DETAIL didn't report its format".to_owned()),
    }
    let Some(id) = value(detail, "id") else {
        return unknown("DESCRIBE DETAIL didn't report its table id".to_owned());
    };
    let Some(number) = value(history, "version") else {
        return unknown("DESCRIBE HISTORY didn't report a version".to_owned());
    };
    // The commit time isn't part of the value: it adds nothing to uniqueness, and
    // times lose precision when normalised.
    tracing::debug!(
        table = id,
        version = number,
        committed = value(history, "timestamp").unwrap_or("unknown"),
        "delta version"
    );
    SourceVersion::Version(DataVersion::new(
        format!("{id}/{number}"),
        Exactness::Exact,
        ORIGIN,
    ))
}

impl<P: RelationProbe> Provider for DeltaVersions<P> {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "delta_versions",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::RelationVersions]),
        )
    }
}

#[async_trait]
impl<P: RelationProbe> ChangeProvider for DeltaVersions<P> {
    async fn versions(&self, sources: &[RequestedSource]) -> Result<VersionReport, ProviderError> {
        let request =
            request().map_err(|e| ProviderError::Other(format!("the Delta version probe: {e}")))?;
        let mut report = self.probe.probe(&request, sources).await?;
        // One answer per requested source, in order, whatever the probe returned.
        Ok(VersionReport::new(
            sources
                .iter()
                .map(|s| {
                    let answer = report
                        .sources
                        .iter()
                        .position(|(id, _)| *id == s.id)
                        .map(|i| report.sources.remove(i).1);
                    let version = answer.map_or_else(
                        || SourceVersion::Unknown("the probe didn't report on it".to_owned()),
                        version,
                    );
                    (s.id.clone(), version)
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(detail: &[(&str, &str)], history: &[(&str, &str)]) -> ProbeAnswer {
        let row = |pairs: &[(&str, &str)]| -> ProbeRow {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        ProbeAnswer::Rows(vec![row(detail), row(history)])
    }

    fn why(answer: ProbeAnswer) -> String {
        match version(answer) {
            SourceVersion::Unknown(why) => why,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_version_is_the_table_id_and_its_latest_version() {
        assert_eq!(
            version(rows(
                &[("id", "3f2a"), ("format", "delta")],
                &[("version", "12"), ("timestamp", "2026-09-29 10:00:00")]
            )),
            SourceVersion::Version(DataVersion::new("3f2a/12", Exactness::Exact, ORIGIN))
        );
    }

    #[test]
    fn anything_missing_or_not_delta_is_unknown() {
        assert!(
            why(rows(
                &[("id", "a"), ("format", "parquet")],
                &[("version", "1")]
            ))
            .contains("parquet")
        );
        assert!(why(rows(&[("id", "a")], &[("version", "1")])).contains("format"));
        assert!(
            why(rows(
                &[("id", " "), ("format", "delta")],
                &[("version", "1")]
            ))
            .contains("table id")
        );
        assert!(why(rows(&[("format", "delta")], &[("version", "1")])).contains("table id"));
        assert!(why(rows(&[("id", "a"), ("format", "delta")], &[])).contains("version"));
        assert!(why(ProbeAnswer::Rows(vec![])).contains("format"));
        assert!(why(ProbeAnswer::Skipped("a view".into())).starts_with("not a Delta table"));
        assert_eq!(why(ProbeAnswer::Unknown("gone".into())), "gone");
    }

    #[test]
    fn the_request_is_valid() {
        let request = request().unwrap();
        assert_eq!(request.filter().relation_kinds(), ["table"]);
        assert_eq!(request.filter().format(), Some("delta"));
        assert_eq!(request.statements().len(), 2);
    }
}
