//! `DuckDB`'s error kinds and messages (MIT), as dbt-duckdb reports them (#323, ADR-0025,
//! ADR-0031 §3a). The ones a run can reach are recorded from real runs of dbt 1.10, 1.11
//! and 1.12 with dbt-duckdb, in `fixtures/dbt/jaffle-ods/artifacts/dbt-<version>-errors`;
//! the rest are from `DuckDB`'s documented error kinds, and a test reaches each with a
//! written message.
//!
//! The `DuckDB` plugin offers this catalogue; dbt's is consulted after it, and adds dbt's
//! steps to what this one recognises. Until dbt's catalogue version 8, these patterns
//! were dbt's.

use ods_core::failure::{ErrorCategory, Symptom};
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::error_catalogue::{
    CatalogueInfo, Classification, ErrorCatalogue, PatternMatch,
};
use ods_sdk::contracts::run_events::ErrorSummary;
use ods_sdk::{Provider, ProviderInfo};

use crate::KIND;

/// The catalogue's version: bumped whenever a pattern is added, changed or removed. 1:
/// the patterns dbt's catalogue held for `DuckDB` until its version 8.
pub const CATALOGUE_VERSION: &str = "1";

/// A pattern: its id, the symptom, the error's kind it needs, and phrases a lowercased
/// message must all hold.
pub struct Pattern {
    /// Its stable id.
    pub id: &'static str,
    /// What the error means.
    pub symptom: Symptom,
    /// The summary's kind, lowercased, if the pattern needs one: the message's own
    /// (`binder error`), or the one dbt's header gave around it.
    pub kind: Option<&'static str>,
    /// Phrases the message holds, all of them.
    pub all: &'static [&'static str],
}

const fn p(
    id: &'static str,
    symptom: Symptom,
    kind: Option<&'static str>,
    all: &'static [&'static str],
) -> Pattern {
    Pattern {
        id,
        symptom,
        kind,
        all,
    }
}

/// The patterns. Messages are lowercase and already redacted: quoted names read
/// `[value removed]`, and so do numbers.
pub const PATTERNS: &[Pattern] = &[
    p(
        "duckdb-values-list-column",
        Symptom::MissingColumn,
        Some("binder error"),
        &["does not have a column named"],
    ),
    p(
        "duckdb-referenced-column",
        Symptom::MissingColumn,
        Some("binder error"),
        &["referenced column", "not found"],
    ),
    p(
        "duckdb-table-missing",
        Symptom::MissingRelation,
        Some("catalog error"),
        &["table with name", "does not exist"],
    ),
    p(
        "duckdb-view-missing",
        Symptom::MissingRelation,
        Some("catalog error"),
        &["view with name", "does not exist"],
    ),
    p(
        "duckdb-schema-missing",
        Symptom::MissingSchema,
        Some("catalog error"),
        &["schema with name", "does not exist"],
    ),
    // Recorded in every `dbt-<version>-errors` (`missing-function`).
    p(
        "duckdb-function-missing",
        Symptom::MissingFunction,
        Some("catalog error"),
        &["function with name", "does not exist"],
    ),
    p(
        "duckdb-conversion",
        Symptom::TypeMismatch,
        Some("conversion error"),
        &[],
    ),
    p(
        "duckdb-constraint",
        Symptom::ConstraintViolation,
        Some("constraint error"),
        &[],
    ),
    p(
        "duckdb-dependent-entries",
        Symptom::DependentObjects,
        Some("dependency error"),
        &["because there are entries that depend on it"],
    ),
    p(
        "duckdb-write-conflict",
        Symptom::LockConflict,
        Some("transactioncontext error"),
        &["conflict"],
    ),
    p(
        "duckdb-file-lock",
        Symptom::LockConflict,
        None,
        &["could not set lock on file"],
    ),
    p(
        "duckdb-permission",
        Symptom::PermissionDenied,
        Some("permission error"),
        &[],
    ),
    p(
        "duckdb-interrupted",
        Symptom::QueryTimeout,
        Some("interrupt error"),
        &[],
    ),
];

/// `DuckDB`'s error catalogue (see the module docs).
#[derive(Debug, Clone, Copy, Default)]
pub struct DuckdbErrors;

impl DuckdbErrors {
    /// The catalogue.
    pub fn new() -> Self {
        Self
    }
}

impl Provider for DuckdbErrors {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "duckdb-error-catalogue",
            CATALOGUE_VERSION,
            CapabilitySet::from_iter([Capability::ErrorExplain]),
        )
    }
}

impl ErrorCatalogue for DuckdbErrors {
    fn catalogue(&self) -> CatalogueInfo {
        CatalogueInfo::new(KIND, CATALOGUE_VERSION, "dbt-duckdb")
    }

    fn classify(&self, error: &ErrorSummary) -> Classification {
        let kind = error.kind().map(str::to_ascii_lowercase);
        let outer = error.outer_kind().map(str::to_ascii_lowercase);
        let message = error.message().to_ascii_lowercase();
        match PATTERNS.iter().find(|pattern| {
            pattern
                .kind
                .is_none_or(|k| kind.as_deref() == Some(k) || outer.as_deref() == Some(k))
                && pattern.all.iter().all(|phrase| message.contains(phrase))
        }) {
            Some(pattern) => {
                Classification::Recognised(PatternMatch::new(pattern.id, pattern.symptom))
            }
            // dbt's catalogue, asked next, gives the category the error's kind implies.
            None => Classification::NotRecognised {
                category: ErrorCategory::Unknown,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use ods_provider_dbt::events::{error_summary, project_failure};
    use ods_sdk::conformance::error_catalogue::{ErrorCatalogueHarness, Sample, run};

    use super::*;

    const SENTINEL: &str = "sk_live_SENTINEL_42";

    /// A message for each pattern, as dbt-duckdb reports it: the recorded ones as
    /// recorded, the others from `DuckDB`'s documented error kinds.
    const WRITTEN: &[(&str, &str)] = &[
        (
            "duckdb-values-list-column",
            "Binder Error: Values list \"o\" does not have a column named \"x\"",
        ),
        (
            "duckdb-referenced-column",
            "Binder Error: Referenced column \"x\" not found in FROM clause!",
        ),
        (
            "duckdb-table-missing",
            "Catalog Error: Table with name orders does not exist!",
        ),
        (
            "duckdb-view-missing",
            "Catalog Error: View with name orders does not exist!",
        ),
        (
            "duckdb-schema-missing",
            "Catalog Error: Schema with name staging does not exist!",
        ),
        (
            "duckdb-function-missing",
            "Catalog Error: Scalar Function with name no_such_function does not exist!",
        ),
        (
            "duckdb-conversion",
            "Conversion Error: Could not convert string 'sk_live_SENTINEL_42' to INT32",
        ),
        (
            "duckdb-constraint",
            "Constraint Error: Duplicate key \"id: 1\" violates primary key constraint.",
        ),
        (
            "duckdb-dependent-entries",
            "Dependency Error: Cannot alter entry \"orders\" because there are entries that depend on it.",
        ),
        (
            "duckdb-write-conflict",
            "TransactionContext Error: Catalog write-write conflict on create with \"orders\"",
        ),
        (
            "duckdb-file-lock",
            "IO Error: Could not set lock on file \"jaffle.duckdb\": Conflicting lock is held",
        ),
        (
            "duckdb-permission",
            "Permission Error: File system LocalFileSystem has been disabled by configuration",
        ),
        ("duckdb-interrupted", "INTERRUPT Error: Interrupted!"),
    ];

    /// A node's failure, as dbt reports it around `message`.
    fn node_failure(message: &str) -> ErrorSummary {
        error_summary(&format!(
            "Runtime Error in model customers (models/customers.sql)\n  {message}"
        ))
        .unwrap()
    }

    /// The runs recorded with real dbt and dbt-duckdb (`capture-errors.sh`).
    const VERSIONS: [&str; 3] = ["1.10", "1.11", "1.12"];

    /// The messages a recorded run gave: each scenario's name, the failed node (none
    /// when the whole project failed) and the message.
    fn recorded(version: &str) -> Vec<(String, Option<String>, String)> {
        let path = format!(
            "{}/../../fixtures/dbt/jaffle-ods/artifacts/dbt-{version}-errors/errors.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        rows.into_iter()
            .map(|r| {
                (
                    r["name"].as_str().unwrap().to_owned(),
                    r["node"].as_str().map(str::to_owned),
                    r["message"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// A recorded message's summary, as ODS reads a real run's.
    fn summary_of(node: Option<&String>, message: &str) -> ErrorSummary {
        match node {
            Some(_) => error_summary(message).unwrap(),
            None => project_failure(message).unwrap().summary,
        }
    }

    #[test]
    fn every_pattern_is_reached_by_its_written_message() {
        assert_eq!(WRITTEN.len(), PATTERNS.len());
        for ((id, message), pattern) in WRITTEN.iter().zip(PATTERNS) {
            assert_eq!(*id, pattern.id, "WRITTEN is in pattern order");
            let summary = node_failure(message);
            let got = DuckdbErrors.classify(&summary);
            let Classification::Recognised(m) = &got else {
                panic!("{id}: {summary:?} gave {got:?}")
            };
            assert_eq!((m.id.as_str(), m.symptom), (*id, pattern.symptom));
            let json = serde_json::to_string(&(&summary, &got)).unwrap();
            assert!(!json.contains(SENTINEL), "{id}: {json}");
        }
    }

    #[test]
    fn every_recorded_duckdb_error_classifies_as_expected() {
        // Each scenario, and the pattern it must reach: none for an error that isn't
        // `DuckDB`'s own (dbt's catalogue reads those).
        let expected = [
            ("missing-column", Some("duckdb-values-list-column")),
            ("unqualified-column", Some("duckdb-referenced-column")),
            ("missing-relation", Some("duckdb-table-missing")),
            ("type-mismatch", Some("duckdb-conversion")),
            ("missing-function", Some("duckdb-function-missing")),
            ("dependent-objects", Some("duckdb-dependent-entries")),
            ("unknown-macro", None),
            ("missing-ref", None),
            ("template-syntax", None),
            ("packages-missing", None),
            ("profile-missing", None),
            ("python-exception", None),
            ("test-failure", None),
            ("accepted-values", None),
        ];
        for version in VERSIONS {
            let recorded = recorded(version);
            assert_eq!(recorded.len(), expected.len(), "{version}: every scenario");
            for (name, node, message) in &recorded {
                let (_, want) = expected
                    .iter()
                    .find(|(n, _)| n == name)
                    .unwrap_or_else(|| panic!("{version} {name} isn't expected"));
                let summary = summary_of(node.as_ref(), message);
                let got = DuckdbErrors.classify(&summary);
                match (&got, want) {
                    (Classification::Recognised(m), Some(id)) => {
                        assert_eq!(m.id, *id, "{version} {name}");
                    }
                    (Classification::NotRecognised { .. }, None) => {}
                    _ => panic!("{version} {name}: {summary:?} gave {got:?}"),
                }
                let json = serde_json::to_string(&(&summary, &got)).unwrap();
                assert!(!json.contains(SENTINEL), "{version} {name}: {json}");
            }
        }
    }

    #[test]
    fn another_engines_error_is_not_recognised() {
        for (kind, message) in [
            ("Database Error", "column \"first_name\" does not exist"),
            (
                "Database Error",
                "[TABLE_OR_VIEW_NOT_FOUND] The table or view `main`.`orders` cannot be found.",
            ),
            ("Runtime Error", "Error starting cluster: terminated"),
        ] {
            let summary = error_summary(&format!(
                "{kind} in model customers (models/customers.sql)\n  {message}"
            ))
            .unwrap();
            assert!(
                matches!(
                    DuckdbErrors.classify(&summary),
                    Classification::NotRecognised { .. }
                ),
                "{message}"
            );
        }
    }

    /// The pattern ids a recorded run reaches.
    fn recorded_ids() -> std::collections::BTreeSet<String> {
        let mut seen = std::collections::BTreeSet::new();
        for version in VERSIONS {
            for (_, node, message) in recorded(version) {
                if let Classification::Recognised(m) =
                    DuckdbErrors.classify(&summary_of(node.as_ref(), &message))
                {
                    seen.insert(m.id);
                }
            }
        }
        seen
    }

    /// The reference's table of these patterns: what the docs page holds between its
    /// markers.
    fn reference_table() -> String {
        let seen = recorded_ids();
        let mut out = String::from(
            "| Pattern | Symptom | Kind | The message holds | Recorded from real dbt |\n|---|---|---|---|---|\n",
        );
        for p in PATTERNS {
            let symptom = serde_json::to_value(p.symptom).unwrap();
            let phrases = if p.all.is_empty() {
                "(any message)".to_owned()
            } else {
                p.all
                    .iter()
                    .map(|ph| format!("`{ph}`"))
                    .collect::<Vec<_>>()
                    .join(" and ")
            };
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!(
                    "| `{}` | `{}` | {} | {} | {} |\n",
                    p.id,
                    symptom.as_str().unwrap(),
                    p.kind
                        .map_or_else(|| "any".to_owned(), |k| format!("`{k}`")),
                    phrases,
                    if seen.contains(p.id) {
                        "yes"
                    } else {
                        "no: from `DuckDB`'s error kinds"
                    },
                ),
            );
        }
        out
    }

    /// `docs/reference/error-patterns.md` lists these patterns as the code has them:
    /// `ODS_UPDATE_DOCS=1 cargo test -p ods-provider-duckdb reference` rewrites them.
    #[test]
    fn the_error_pattern_reference_matches_the_catalogue() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/reference/error-patterns.md");
        let page = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        let (begin, end) = (
            "<!-- duckdb-patterns:begin -->\n",
            "<!-- duckdb-patterns:end -->",
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
            "the reference is out of date: ODS_UPDATE_DOCS=1 cargo test -p ods-provider-duckdb reference"
        );
        assert!(
            page.contains(&format!(
                "at catalogue version **{CATALOGUE_VERSION}**, holds `DuckDB`'s"
            )),
            "the reference names the catalogue's version"
        );
    }

    struct Harness;

    impl ErrorCatalogueHarness for Harness {
        fn catalogue(&self) -> &dyn ErrorCatalogue {
            &DuckdbErrors
        }

        fn samples(&self) -> Vec<Sample> {
            let mut samples: Vec<Sample> = WRITTEN
                .iter()
                .zip(PATTERNS)
                .map(|((id, message), pattern)| Sample {
                    name: id,
                    summary: node_failure(message),
                    expected: Some(pattern.symptom),
                    sentinel: Some(SENTINEL).filter(|s| message.contains(s)),
                })
                .collect();
            samples.push(Sample {
                name: "a DuckDB error kind no pattern names",
                summary: node_failure(
                    "Out of Range Error: Overflow in multiplication of INT32 ('sk_live_SENTINEL_42')",
                ),
                expected: None,
                sentinel: Some(SENTINEL),
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
