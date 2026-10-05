//! The sqlparser analyzer passes the `SqlLineageAnalyzer` conformance suite in every
//! dialect it supports (#99).

use std::sync::Arc;

use ods_provider_sqlparser::{SqlDialect, SqlparserAnalyzer};
use ods_sdk::conformance::sql_lineage::{SqlLineageHarness, run};
use ods_sdk::contracts::sql_lineage::SqlLineageAnalyzer;

struct Harness(SqlDialect);

impl SqlLineageHarness for Harness {
    fn analyzer(&self) -> Arc<dyn SqlLineageAnalyzer> {
        Arc::new(SqlparserAnalyzer::new(self.0))
    }
}

#[test]
fn conforms_in_every_dialect() {
    for dialect in SqlDialect::ALL {
        let report = run(&Harness(dialect));
        assert!(report.skipped.is_empty(), "{dialect:?}: {report:?}");
        assert_eq!(report.passed.len(), 5, "{dialect:?}: {report:?}");
    }
}
