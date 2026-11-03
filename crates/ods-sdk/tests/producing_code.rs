//! Producing-code contract regression tests.
use ods_sdk::contracts::producing_code::*;
use serde_json::json;

fn mappings() -> DeclaredMappings {
    serde_json::from_value(json!({"schema_version":{"major":0,"minor":1},"entries":[
        {"asset":{"project":"p","environment":"prod","node":"source.p.raw"},
         "code":{"repository":"ingestion","path":"jobs/load.py","revision":{"kind":"current","reference":"main"}}}
    ]})).unwrap()
}
fn asset(environment: &str) -> AssetKey {
    AssetKey::new("p", environment, "source.p.raw")
}

#[test]
fn mappings_are_scoped_and_never_claim_current_is_exact() {
    let found = mappings().resolve(&asset("prod"), Some("run-1")).unwrap();
    assert_eq!(found.references.len(), 1);
    assert_eq!(found.references[0].provenance, Provenance::Declared);
    assert!(matches!(
        found.references[0].code.revision,
        Revision::Current { .. }
    ));
    assert_eq!(
        mappings().resolve(&asset("dev"), None).unwrap().gaps,
        vec![CodeGap::Missing]
    );
}

#[test]
fn rejects_bad_paths_and_future_schema() {
    for path in [
        "../secret",
        "/tmp/secret",
        "a/../../secret",
        "C:\\secret",
        "a\\..\\secret",
        "",
        "a//b",
    ] {
        let mut mappings = mappings();
        mappings.entries[0].code.path = path.into();
        assert!(mappings.resolve(&asset("prod"), None).is_err(), "{path}");
    }
    let mut mappings = mappings();
    mappings.schema_version.major = 99;
    assert!(mappings.resolve(&asset("prod"), None).is_err());
}

#[test]
fn exact_revision_is_only_for_its_recorded_run() {
    let mut mappings = mappings();
    mappings.entries[0].code.revision = Revision::Exact {
        commit: "a".repeat(40),
        run: "r1".into(),
    };
    assert_eq!(
        mappings.resolve(&asset("prod"), Some("r1")).unwrap().gaps,
        Vec::new()
    );
    let wrong = mappings.resolve(&asset("prod"), Some("r2")).unwrap();
    assert_eq!(wrong.gaps, vec![CodeGap::RevisionMismatch]);
    assert_eq!(wrong.references, Vec::new());
}

#[test]
fn multiple_producers_are_preserved_without_guessing() {
    let mut mappings = mappings();
    let mut other = mappings.entries[0].clone();
    other.code.repository = "other".into();
    mappings.entries.push(other);
    let result = mappings.resolve(&asset("prod"), None).unwrap();
    assert_eq!(result.references.len(), 2);
    assert_eq!(result.gaps, vec![CodeGap::Ambiguous]);
}

#[test]
fn external_providers_can_build_references_without_serialization() {
    let location = CodeLocation::new("repo", "job.py", Revision::Unknown);
    location.validate().unwrap();
    let reference = CodeReference::new(location, Provenance::Observed);
    assert_eq!(reference.provenance, Provenance::Observed);
}
