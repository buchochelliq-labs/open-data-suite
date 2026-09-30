//! The fake linker passes the `RelationLinker` conformance suite.

use std::sync::Arc;

use ods_provider_fake::FakeRelationLinker;
use ods_sdk::conformance::relation_link::{RelationLinkHarness, run};
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLinker};

struct Harness;

impl RelationLinkHarness for Harness {
    fn linker(&self) -> Arc<dyn RelationLinker> {
        Arc::new(FakeRelationLinker::new("warehouse.example"))
    }

    fn unconfigured(&self) -> Option<Arc<dyn RelationLinker>> {
        Some(Arc::new(FakeRelationLinker::unconfigured()))
    }

    fn qualified(&self) -> String {
        r#""shop"."mart"."orders""#.to_owned()
    }

    fn under_qualified(&self) -> String {
        r#""mart"."orders""#.to_owned()
    }

    fn awkward(&self) -> String {
        r#""my shop"."a?b#c"."d/e%f""#.to_owned()
    }

    fn malformed(&self) -> Vec<String> {
        [
            r#""shop"."."."orders""#,
            r#""shop".".."."orders""#,
            r#""shop".""."orders""#,
            r#""shop"x."mart"."orders""#,
            "my shop.mart.orders",
        ]
        .map(str::to_owned)
        .to_vec()
    }
}

#[test]
fn conforms() {
    let report = run(&Harness);
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 6, "{report:?}");
}

#[test]
fn links_each_part_encoded() {
    let link = FakeRelationLinker::new("w.example")
        .link(r#""my shop".mart."d/e""#)
        .unwrap();
    assert_eq!(link.url, "https://w.example/relations/my%20shop/mart/d%2Fe");
    assert!(matches!(
        FakeRelationLinker::new("w.example")
            .with_parts(2)
            .link("a.b.c"),
        Err(NoRelationLink::NotQualified {
            parts: 3,
            needed: 2,
            ..
        })
    ));
}
