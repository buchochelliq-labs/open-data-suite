//! The scripted SQL lineage analyzer passes the `SqlLineageAnalyzer` conformance suite
//! (#99), with the suite's identity query scripted.

use std::sync::Arc;

use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::conformance::sql_lineage::{
    IDENTITY_SQL, RELATION, SqlLineageHarness, identity_lineage, run,
};
use ods_sdk::contracts::sql_lineage::SqlLineageAnalyzer;

struct Harness;

impl SqlLineageHarness for Harness {
    fn analyzer(&self) -> Arc<dyn SqlLineageAnalyzer> {
        let fake = FakeSqlLineageAnalyzer::new();
        let relation = fake.relation_name(RELATION).unwrap();
        Arc::new(fake.with(IDENTITY_SQL, identity_lineage(relation, "id")))
    }
}

#[test]
fn conforms() {
    let report = run(&Harness);
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 5, "{report:?}");
}
