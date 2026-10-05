//! The scripted observed-lineage source passes the `ObservedLineageSource` conformance
//! suite (#99). It reads what it was given, so it has no malformed input to fail on.

use std::sync::Arc;

use ods_core::{ColumnRef, RelationName};
use ods_provider_fake::FakeObservedLineageSource;
use ods_sdk::conformance::observed_lineage::{ObservedLineageHarness, run};
use ods_sdk::contracts::observed_lineage::{ObservedLineage, ObservedLineageSource};

struct Harness;

impl ObservedLineageHarness for Harness {
    fn source(&self) -> Arc<dyn ObservedLineageSource> {
        let col = |relation: &str, column: &str| {
            ColumnRef::new(RelationName::new(relation.split('.')).unwrap(), column)
        };
        let mut observed = ObservedLineage::default();
        observed.add_column_edge(col("c.s.stg_orders", "id"), col("c.s.orders", "id"));
        observed.add_row_input(
            col("c.s.stg_orders", "status"),
            col("c.s.orders", "id").relation,
        );
        observed.records = 3;
        observed.skipped = 1;
        Arc::new(FakeObservedLineageSource(observed))
    }
}

#[test]
fn conforms() {
    let report = run(&Harness);
    assert_eq!(report.passed.len(), 2, "{report:?}");
    assert_eq!(
        report
            .skipped
            .iter()
            .map(|(case, _)| *case)
            .collect::<Vec<_>>(),
        ["malformed_input_is_an_error"],
        "only the case a scripted source can't take part in"
    );
}
