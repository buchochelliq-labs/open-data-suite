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

/// A node as lineage sees it: id, relation, compiled SQL, dependencies.
type Node = (String, Option<String>, Option<String>, Vec<String>);

fn v2() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-2.0")
}

#[test]
fn reads_dbt_v2_manifest_json() {
    let artifacts =
        Artifacts::load_with(&v2(), ods_provider_dbt::ArtifactPreference::Json).unwrap();
    let manifest = &artifacts.manifest;
    assert_eq!(
        manifest.source,
        ods_provider_dbt::ArtifactSource::ManifestJson
    );
    assert_eq!(manifest.schema_version, 12);
    assert_eq!(manifest.dbt_version.as_deref(), Some("2.0.5"));
    assert!(artifacts.catalog.is_none(), "compile writes no catalog");
}

#[test]
fn reads_the_dbt_v2_information_schema_like_the_manifest() {
    use ods_provider_dbt::{ArtifactPreference, ArtifactSource};
    let json = Artifacts::load_with(&v2(), ArtifactPreference::Json)
        .unwrap()
        .manifest;
    let parquet = Artifacts::load_with(&v2(), ArtifactPreference::InfoSchema)
        .unwrap()
        .manifest;
    assert_eq!(parquet.source, ArtifactSource::InfoSchema);
    assert_eq!(parquet.schema_version, 1);
    assert_eq!(parquet.dbt_version.as_deref(), Some("2.0.5"));
    assert_eq!(parquet.adapter_type.as_deref(), Some("duckdb"));

    let lineage_nodes = |m: &Manifest| -> Vec<Node> {
        let ids: std::collections::BTreeSet<&str> = m
            .nodes
            .iter()
            .filter(|n| n.relation_name.is_some())
            .map(|n| n.unique_id.as_str())
            .collect();
        m.nodes
            .iter()
            .filter(|n| matches!(n.resource_type, ResourceType::Model | ResourceType::Seed))
            .map(|n| {
                let mut deps: Vec<String> = n
                    .depends_on
                    .iter()
                    .filter(|d| ids.contains(d.as_str()))
                    .cloned()
                    .collect();
                deps.sort();
                (
                    n.unique_id.clone(),
                    n.relation_name.clone(),
                    n.compiled_code.as_deref().map(|c| c.trim().to_owned()),
                    deps,
                )
            })
            .collect()
    };
    assert_eq!(lineage_nodes(&parquet), lineage_nodes(&json));

    // Pointing at the versioned directory itself works too.
    let direct = Artifacts::load(&v2().join("info_schema/v1")).unwrap();
    assert_eq!(direct.manifest.source, ArtifactSource::InfoSchema);
}

#[test]
fn asking_for_an_information_schema_that_is_not_there_is_an_error() {
    let err = Artifacts::load_with(&target(), ods_provider_dbt::ArtifactPreference::InfoSchema)
        .unwrap_err();
    assert!(err.to_string().contains("dbt Information Schema"), "{err}");
}

/// (test name, attached node, column, arguments `to`/`field`, `depends_on`) for comparison.
type TestSummary = (String, Option<String>, Option<String>, String, Vec<String>);

fn tests_of(m: &Manifest) -> Vec<TestSummary> {
    let mut tests: Vec<TestSummary> = m
        .nodes
        .iter()
        .filter_map(|n| {
            let t = n.test.as_ref()?;
            let mut deps = n.depends_on.clone();
            deps.sort();
            Some((
                t.name.clone(),
                t.attached_node.clone(),
                t.column_name.clone(),
                format!("{}/{}", t.arguments["to"], t.arguments["field"]),
                deps,
            ))
        })
        .collect();
    tests.sort();
    tests
}

#[test]
fn data_tests_read_the_same_from_every_format() {
    use ods_provider_dbt::ArtifactPreference;
    let v1 = tests_of(&Artifacts::load(&target()).unwrap().manifest);
    let v2_json = tests_of(
        &Artifacts::load_with(&v2(), ArtifactPreference::Json)
            .unwrap()
            .manifest,
    );
    let v2_parquet = tests_of(
        &Artifacts::load_with(&v2(), ArtifactPreference::InfoSchema)
            .unwrap()
            .manifest,
    );
    assert_eq!(v1.len(), 12);
    assert_eq!(v1, v2_json);
    assert_eq!(v1, v2_parquet);
    let relationship = v1
        .iter()
        .find(|t| t.0 == "relationships" && t.1.as_deref() == Some("model.jaffle_ods.orders"))
        .unwrap();
    assert_eq!(relationship.2.as_deref(), Some("customer_id"));
    assert_eq!(relationship.3, r#""ref('customers')"/"customer_id""#);
    assert_eq!(
        relationship.4,
        ["model.jaffle_ods.customers", "model.jaffle_ods.orders"]
    );
}

#[test]
fn catalog_types_are_read() {
    let catalog = Artifacts::load(&target()).unwrap().catalog.unwrap();
    let types = &catalog.types["model.jaffle_ods.orders"];
    assert_eq!(types["order_id"].to_lowercase(), "integer");
    assert_eq!(
        types.len(),
        catalog.columns["model.jaffle_ods.orders"].len()
    );
}

#[test]
fn model_and_column_constraints_are_collected() {
    let json = r#"{
      "metadata": {"dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12.json"},
      "nodes": {"model.p.orders": {
        "unique_id": "model.p.orders", "resource_type": "model",
        "constraints": [{"type": "foreign_key", "columns": ["customer_id"],
                         "to": "ref('customers')", "to_columns": ["id"]}],
        "columns": {"order_id": {"name": "order_id", "data_type": "bigint",
                                 "constraints": [{"type": "primary_key"}, {"type": "not_null"}]}}
      }}
    }"#;
    let manifest = Manifest::parse(Path::new("manifest.json"), json).unwrap();
    let node = &manifest.nodes[0];
    assert_eq!(node.declared_types["order_id"], "bigint");
    let kinds: Vec<(&str, Vec<String>)> = node
        .constraints
        .iter()
        .map(|c| (c.kind.as_str(), c.columns.clone()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("foreign_key", vec!["customer_id".to_owned()]),
            ("primary_key", vec!["order_id".to_owned()]),
            ("not_null", vec!["order_id".to_owned()])
        ]
    );
    assert_eq!(node.constraints[0].to.as_deref(), Some("ref('customers')"));
    assert_eq!(node.constraints[0].to_columns, ["id"]);
}
