//! dbt-databricks's own error messages (#323, ADR-0025, ADR-0031 §3a): its compute (a
//! cluster or SQL warehouse) that can't be started, asked for its state or connected
//! to, a command or Python model run that timed out, and credentials its profile is
//! missing. Every phrase is from dbt-databricks's source (Apache-2.0, 1.12).
//!
//! The Databricks plugin offers this catalogue; dbt's is consulted after it, and adds
//! dbt's steps to what this one recognises. Spark's and Delta's error conditions,
//! which other engines report too, stay in dbt's catalogue.

use ods_core::failure::{ErrorCategory, Symptom};
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::error_catalogue::{
    CatalogueInfo, Classification, ErrorCatalogue, PatternMatch,
};
use ods_sdk::contracts::run_events::ErrorSummary;
use ods_sdk::{Provider, ProviderInfo};

use crate::KIND;

/// The catalogue's version: bumped whenever a pattern is added, changed or removed. 1:
/// the patterns dbt's catalogue held for Databricks until its version 7.
pub const CATALOGUE_VERSION: &str = "1";

/// A pattern: its id, the symptom, and phrases a lowercased message must all hold.
pub struct Pattern {
    /// Its stable id.
    pub id: &'static str,
    /// What the error means.
    pub symptom: Symptom,
    /// Phrases the message holds, all of them.
    pub all: &'static [&'static str],
}

const fn p(id: &'static str, symptom: Symptom, all: &'static [&'static str]) -> Pattern {
    Pattern { id, symptom, all }
}

/// The patterns. Messages are lowercase and already redacted: quoted names read
/// `[value removed]`, and so do numbers.
pub const PATTERNS: &[Pattern] = &[
    p(
        "databricks-cluster-start",
        Symptom::WarehouseUnavailable,
        &["error starting cluster"],
    ),
    p(
        "databricks-cluster-status",
        Symptom::WarehouseUnavailable,
        &["error getting status of cluster"],
    ),
    p(
        "databricks-connection",
        Symptom::WarehouseUnavailable,
        &["failed to create connection"],
    ),
    p(
        "databricks-command-timeout",
        Symptom::QueryTimeout,
        &["command execution timed out"],
    ),
    p(
        "databricks-python-timeout",
        Symptom::QueryTimeout,
        &["python model run timed out"],
    ),
    p(
        "databricks-oauth-required",
        Symptom::CredentialsMissing,
        &["is required when not using access token"],
    ),
    // `The config 'client_id' is required to connect to Databricks when
    // 'client_secret' is present`, its names removed.
    p(
        "databricks-client-id-required",
        Symptom::CredentialsMissing,
        &["is required to connect to databricks when", "is present"],
    ),
];

/// Databricks' error catalogue (see the module docs).
#[derive(Debug, Clone, Copy, Default)]
pub struct DatabricksErrors;

impl DatabricksErrors {
    /// The catalogue.
    pub fn new() -> Self {
        Self
    }
}

impl Provider for DatabricksErrors {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "databricks-error-catalogue",
            CATALOGUE_VERSION,
            CapabilitySet::from_iter([Capability::ErrorExplain]),
        )
    }
}

impl ErrorCatalogue for DatabricksErrors {
    fn catalogue(&self) -> CatalogueInfo {
        CatalogueInfo::new(KIND, CATALOGUE_VERSION, "dbt-databricks")
    }

    fn classify(&self, error: &ErrorSummary) -> Classification {
        let message = error.message().to_ascii_lowercase();
        match PATTERNS
            .iter()
            .find(|p| p.all.iter().all(|phrase| message.contains(phrase)))
        {
            Some(pattern) => {
                Classification::Recognised(PatternMatch::new(pattern.id, pattern.symptom))
            }
            None => Classification::NotRecognised {
                category: ErrorCategory::Unknown,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use ods_sdk::conformance::error_catalogue::{ErrorCatalogueHarness, Sample, run};

    use super::*;

    /// A message from dbt-databricks's source for each pattern, as dbt reports it.
    const WRITTEN: &[(&str, &str)] = &[
        (
            "databricks-cluster-start",
            "Runtime Error: Error starting cluster: Cluster 0123-456789-abcdefgh is terminated",
        ),
        (
            "databricks-cluster-status",
            "Runtime Error: Error getting status of cluster: Cluster 0123-456789-abcdefgh does not exist",
        ),
        (
            "databricks-connection",
            "Database Error: Failed to create connection",
        ),
        (
            "databricks-command-timeout",
            "Runtime Error: Command execution timed out",
        ),
        (
            "databricks-python-timeout",
            "Runtime Error: Python model run timed out",
        ),
        (
            "databricks-oauth-required",
            "Runtime Error: The config `auth_type: oauth` is required when not using access token",
        ),
        (
            "databricks-client-id-required",
            "Runtime Error: The config 'client_id' is required to connect to Databricks when 'client_secret' is present",
        ),
    ];

    fn summary(message: &str) -> ErrorSummary {
        ErrorSummary::from_message(message).unwrap()
    }

    #[test]
    fn every_pattern_is_reached_by_its_written_message() {
        assert_eq!(WRITTEN.len(), PATTERNS.len());
        for (id, message) in WRITTEN {
            match DatabricksErrors.classify(&summary(message)) {
                Classification::Recognised(m) => assert_eq!(m.id, *id, "{message}"),
                other => panic!("{message}: {other:?}"),
            }
        }
    }

    #[test]
    fn another_engines_error_is_not_recognised() {
        for message in [
            "Binder Error: Referenced column \"x\" not found in FROM clause!",
            "[UNRESOLVED_COLUMN.WITH_SUGGESTION] A column cannot be resolved.",
        ] {
            assert!(matches!(
                DatabricksErrors.classify(&summary(message)),
                Classification::NotRecognised { .. }
            ));
        }
    }

    /// The reference's table of this catalogue's patterns.
    fn reference_table() -> String {
        use std::fmt::Write as _;
        let mut out = String::from("| Pattern | Symptom | The message holds |\n|---|---|---|\n");
        for p in PATTERNS {
            let symptom = serde_json::to_value(p.symptom).unwrap();
            let phrases = p
                .all
                .iter()
                .map(|ph| format!("`{ph}`"))
                .collect::<Vec<_>>()
                .join(" and ");
            let _ = writeln!(
                out,
                "| `{}` | `{}` | {phrases} |",
                p.id,
                symptom.as_str().unwrap()
            );
        }
        out
    }

    /// `docs/reference/error-patterns.md` lists these patterns as the code has them:
    /// `ODS_UPDATE_DOCS=1 cargo test -p ods-provider-databricks reference` rewrites them.
    #[test]
    fn the_error_pattern_reference_matches_the_catalogue() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/reference/error-patterns.md");
        let page = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        let (begin, end) = (
            "<!-- databricks-patterns:begin -->\n",
            "<!-- databricks-patterns:end -->",
        );
        let start = page.find(begin).expect("the begin marker") + begin.len();
        let stop = page.find(end).expect("the end marker");
        let table = reference_table();
        if std::env::var_os("ODS_UPDATE_DOCS").is_some() {
            std::fs::write(&path, format!("{}{table}{}", &page[..start], &page[stop..])).unwrap();
            return;
        }
        assert_eq!(
            &page[start..stop],
            table,
            "the reference is out of date: ODS_UPDATE_DOCS=1 cargo test -p ods-provider-databricks reference"
        );
        assert!(
            page.contains(&format!(
                "at catalogue version **{CATALOGUE_VERSION}**, holds dbt-databricks"
            )),
            "the reference names the catalogue's version"
        );
    }

    struct Harness;

    impl ErrorCatalogueHarness for Harness {
        fn catalogue(&self) -> &dyn ErrorCatalogue {
            &DatabricksErrors
        }

        fn samples(&self) -> Vec<Sample> {
            let mut samples: Vec<Sample> = WRITTEN
                .iter()
                .zip(PATTERNS)
                .map(|((id, message), pattern)| Sample {
                    name: id,
                    summary: summary(message),
                    expected: Some(pattern.symptom),
                    sentinel: Some("0123-456789-abcdefgh").filter(|s| message.contains(s)),
                })
                .collect();
            samples.push(Sample {
                name: "duckdb-binder",
                summary: summary("Binder Error: Referenced column \"x\" not found"),
                expected: None,
                sentinel: None,
            });
            samples
        }
    }

    #[test]
    fn conforms() {
        let report = run(&Harness);
        assert!(report.skipped.is_empty(), "{report:?}");
    }
}
