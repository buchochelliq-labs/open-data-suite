//! The built-in `DuckDB` plugin (ADR-0031 §3, §3a): `DuckDB`'s error messages, as
//! dbt-duckdb reports them (ADR-0025), and the `DuckDB` dialect. The only place the CLI
//! names `DuckDB`'s provider.
//!
//! `DuckDB` has no table version to read, so the plugin offers no source versions: `ods
//! state` uses `sources.json` alone, as before. Nor does it offer a login check, links
//! or observed lineage: a local database file has none of them.

use std::sync::Arc;

use ods_provider_duckdb::DuckdbErrors;
use ods_sdk::contracts::error_catalogue::ErrorCatalogue;

use super::{Origin, WarehousePlugin};

/// The `DuckDB` plugin.
pub(super) struct Duckdb;

impl WarehousePlugin for Duckdb {
    fn origin(&self) -> Origin {
        Origin {
            name: "ods-provider-duckdb",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn warehouse(&self) -> &str {
        ods_provider_duckdb::KIND
    }

    fn errors(&self) -> Option<Arc<dyn ErrorCatalogue>> {
        Some(Arc::new(DuckdbErrors))
    }

    fn dialect(&self) -> Option<&str> {
        Some("duckdb")
    }
}

/// `DuckDB`'s patterns against the runs recorded with real dbt-duckdb, read through dbt's
/// provider as a run reads them. Here, not in `ods-provider-duckdb`: providers never
/// depend on each other, even in tests (ADR-0001), and the CLI wires both.
#[cfg(test)]
mod tests {
    use ods_provider_dbt::events::{error_summary, project_failure};
    use ods_provider_duckdb::error_catalogue::{CATALOGUE_VERSION, PATTERNS};
    use ods_sdk::contracts::error_catalogue::Classification;
    use ods_sdk::contracts::run_events::ErrorSummary;

    use super::*;

    const SENTINEL: &str = "sk_live_SENTINEL_42";

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
    /// `ODS_UPDATE_DOCS=1 cargo test -p ods-cli duckdb_reference` rewrites them.
    #[test]
    fn the_duckdb_reference_matches_the_catalogue() {
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
            "the reference is out of date: ODS_UPDATE_DOCS=1 cargo test -p ods-cli duckdb_reference"
        );
        assert!(
            page.contains(&format!(
                "at catalogue version **{CATALOGUE_VERSION}**, holds `DuckDB`'s"
            )),
            "the reference names the catalogue's version"
        );
    }
}
