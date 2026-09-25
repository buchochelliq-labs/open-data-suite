//! Reading the synthetic `system.access.column_lineage` export for `jaffle-ods`.

use std::path::{Path, PathBuf};

use ods_core::{ColumnRef, RelationName};
use ods_provider_databricks::{ExportFormat, UcColumnLineage};
use ods_sdk::contracts::observed_lineage::ObservedLineageSource;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/databricks/uc-lineage")
        .join(name)
}

fn col(relation: &str, column: &str) -> ColumnRef {
    ColumnRef::new(RelationName::new(relation.split('.')).unwrap(), column)
}

#[test]
fn reads_column_edges_and_skips_reads_and_paths() {
    let observed = UcColumnLineage::from_path(fixture("column_lineage.csv"))
        .unwrap()
        .observed_lineage()
        .unwrap();
    assert_eq!(observed.records, 50);
    assert_eq!(
        observed.skipped, 2,
        "the plain read and the file-path source"
    );
    assert!(observed.column_edges.contains(&(
        col("jaffle_ods.main.stg_payments", "amount"),
        col("jaffle_ods.main.orders", "amount")
    )));
    assert!(
        observed.column_edges.contains(&(
            col("JAFFLE_ODS.Main.Orders", "Amount"),
            col("jaffle_ods.main.customer_order_rank", "AMOUNT")
        )),
        "names are kept as written; callers normalize"
    );
    assert!(observed.row_inputs.is_empty());
    let (first, last) = observed.observed_between.unwrap();
    assert!(first < last, "{first} .. {last}");
}

#[test]
fn csv_and_json_exports_read_the_same() {
    let csv = UcColumnLineage::from_path(fixture("column_lineage.csv"))
        .unwrap()
        .observed_lineage()
        .unwrap();
    let json = UcColumnLineage::from_path(fixture("column_lineage.json"))
        .unwrap()
        .observed_lineage()
        .unwrap();
    assert_eq!(csv, json);
}

#[test]
fn newline_delimited_json_and_row_inputs() {
    let dir = std::env::temp_dir().join(format!("ods-uc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lineage.ndjson");
    std::fs::write(
        &path,
        concat!(
            r#"{"source_table_full_name":"c.s.a","source_column_name":"status","target_table_full_name":"c.s.b","target_column_name":null}"#,
            "\n",
            r#"{"source_table_full_name":"c.s.a","source_column_name":"id","target_table_full_name":"c.s.b","target_column_name":"id","event_time":"2026-09-01T00:00:00Z"}"#,
            "\n"
        ),
    )
    .unwrap();
    let observed = UcColumnLineage::new(&path, ExportFormat::Json)
        .observed_lineage()
        .unwrap();
    assert_eq!(observed.column_edges.len(), 1);
    assert_eq!(
        observed.row_inputs.iter().next().unwrap(),
        &(
            col("c.s.a", "status"),
            RelationName::new(["c", "s", "b"]).unwrap()
        )
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_file_that_is_not_a_column_lineage_export_is_an_error() {
    let dir = std::env::temp_dir().join(format!("ods-uc-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("table_lineage.csv");
    std::fs::write(
        &path,
        "source_table_full_name,target_table_full_name\na.b.c,a.b.d\n",
    )
    .unwrap();
    let err = UcColumnLineage::from_path(&path)
        .unwrap()
        .observed_lineage()
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("missing source_column_name, target_column_name"),
        "{err}"
    );
    assert!(UcColumnLineage::from_path(dir.join("x.parquet")).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}
