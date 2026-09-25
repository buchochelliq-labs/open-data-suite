//! Reading the committed `jaffle-ods` fixture artifacts (#100).

use std::path::{Path, PathBuf};

use ods_provider_dbt::{Artifacts, DbtError, Manifest, ResourceType};

fn target() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

#[test]
fn reads_the_fixture_manifest_and_catalog() {
    let artifacts = Artifacts::load(&target()).unwrap();
    let manifest = &artifacts.manifest;
    assert_eq!(manifest.schema_version, 12);
    assert_eq!(manifest.adapter_type.as_deref(), Some("duckdb"));
    let orders = manifest
        .nodes
        .iter()
        .find(|n| n.unique_id == "model.jaffle_ods.orders")
        .unwrap();
    assert_eq!(orders.resource_type, ResourceType::Model);
    assert_eq!(
        orders.relation_name.as_deref(),
        Some(r#""jaffle_ods"."main"."orders""#)
    );
    assert!(
        orders
            .compiled_code
            .as_deref()
            .unwrap()
            .contains("left join payments")
    );
    assert_eq!(orders.materialized.as_deref(), Some("table"));
    let models = manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == ResourceType::Model)
        .count();
    let seeds = manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == ResourceType::Seed)
        .count();
    assert_eq!((models, seeds), (8, 3));

    let catalog = artifacts.catalog.unwrap();
    assert_eq!(
        catalog.columns["seed.jaffle_ods.raw_orders"],
        ["id", "user_id", "order_date", "status"],
        "columns come in warehouse order"
    );
}

#[test]
fn unsupported_versions_and_invalid_json_are_clear_errors() {
    let path = Path::new("manifest.json");
    let old = r#"{"metadata": {"dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v4.json"}}"#;
    let err = Manifest::parse(path, old).unwrap_err();
    assert!(matches!(err, DbtError::UnsupportedVersion { found: 4, .. }));
    assert_eq!(
        err.to_string(),
        "`manifest.json` is dbt manifest schema v4; supported versions: v11, v12"
    );
    assert!(matches!(
        Manifest::parse(path, "{").unwrap_err(),
        DbtError::Invalid { .. }
    ));
}
