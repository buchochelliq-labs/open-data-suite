//! Producing-code contract regression tests.
use ods_provider_dbt::{DbtProducingCode, Manifest};
use ods_sdk::contracts::producing_code::{AssetKey, CodeGap, ProducingCodeSource};
use serde_json::json;

#[test]
fn source_metadata_is_a_declared_mapping_and_does_not_need_a_catalog() {
    let manifest = Manifest::parse(std::path::Path::new("manifest.json"), &json!({
        "metadata":{"dbt_schema_version":"https://schemas.getdbt.com/dbt/manifest/v12.json"},
        "sources":{"source.p.raw":{"unique_id":"source.p.raw","resource_type":"source",
        "meta":{"ods":{"producing_code":[{"repository":"ingestion","path":"jobs/load.py","revision":{"kind":"unknown"}}]}}}}
    }).to_string()).unwrap();
    let source = DbtProducingCode::from_manifest(&manifest, "project", "prod").unwrap();
    let key = AssetKey::new("project", "prod", "source.p.raw");
    let result = source.producing_code(&key, None).unwrap();
    assert_eq!(result.references.len(), 1);
    assert!(result.gaps.is_empty());
    assert_eq!(
        source
            .producing_code(&AssetKey::new("other", "prod", "source.p.raw"), None)
            .unwrap()
            .gaps,
        vec![CodeGap::Missing]
    );
    ods_sdk::conformance::producing_code::check(&source, &key);
}

#[test]
fn config_metadata_is_supported_and_conflicting_forms_fail() {
    let location =
        json!([{"repository":"ingestion","path":"load.py","revision":{"kind":"unknown"}}]);
    let mut document = json!({"metadata":{"dbt_schema_version":"https://schemas.getdbt.com/dbt/manifest/v12.json"},
        "sources":{"source.p.raw":{"unique_id":"source.p.raw","resource_type":"source","config":{"meta":{"ods":{"producing_code":location}}}}}});
    let parse = |value: &serde_json::Value| {
        Manifest::parse(std::path::Path::new("manifest.json"), &value.to_string()).unwrap()
    };
    let source = DbtProducingCode::from_manifest(&parse(&document), "p", "prod").unwrap();
    assert_eq!(
        source
            .producing_code(&AssetKey::new("p", "prod", "source.p.raw"), None)
            .unwrap()
            .references
            .len(),
        1
    );
    document["sources"]["source.p.raw"]["meta"] = json!({"ods":{"producing_code":[]}});
    assert!(DbtProducingCode::from_manifest(&parse(&document), "p", "prod").is_err());
}
