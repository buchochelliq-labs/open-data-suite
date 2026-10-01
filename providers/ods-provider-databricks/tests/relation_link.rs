//! Catalog Explorer links pass the `RelationLinker` conformance suite.

use std::sync::Arc;

use ods_provider_databricks::CatalogExplorer;
use ods_sdk::conformance::relation_link::{RelationLinkHarness, run};
use ods_sdk::contracts::relation_link::RelationLinker;

struct Harness;

impl RelationLinkHarness for Harness {
    fn linker(&self) -> Arc<dyn RelationLinker> {
        // With a workspace id, so the suite sees the `?o=` query too.
        Arc::new(
            CatalogExplorer::new("uc", Some("https://dbc-0123.cloud.databricks.com/"))
                .with_workspace_id(Some("1234567890")),
        )
    }

    fn unconfigured(&self) -> Option<Arc<dyn RelationLinker>> {
        Some(Arc::new(CatalogExplorer::new("uc", None)))
    }

    fn qualified(&self) -> String {
        "`main`.`jaffle`.`orders`".to_owned()
    }

    fn under_qualified(&self) -> String {
        "`jaffle`.`orders`".to_owned()
    }

    fn awkward(&self) -> String {
        "`my main`.`a?b#c`.`d/e%f`".to_owned()
    }

    fn malformed(&self) -> Vec<String> {
        [
            "`main`.`.`.`orders`",
            "`main`.`..`.`orders`",
            "`main`.``.`orders`",
            "`main`x.`jaffle`.`orders`",
            "my main.jaffle.orders",
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
fn links_as_the_issue_specifies() {
    let link = Harness.linker().link("`catalog`.`schema`.`table`").unwrap();
    assert_eq!(
        link.url,
        "https://dbc-0123.cloud.databricks.com/explore/data/catalog/schema/table?o=1234567890"
    );
    assert_eq!(link.label, "Open in Catalog Explorer");
}

#[test]
fn passes_without_a_workspace_id_too() {
    struct NoId;
    impl RelationLinkHarness for NoId {
        fn linker(&self) -> Arc<dyn RelationLinker> {
            Arc::new(CatalogExplorer::new(
                "uc",
                Some("dbc-0123.cloud.databricks.com"),
            ))
        }
        fn unconfigured(&self) -> Option<Arc<dyn RelationLinker>> {
            Harness.unconfigured()
        }
        fn qualified(&self) -> String {
            Harness.qualified()
        }
        fn under_qualified(&self) -> String {
            Harness.under_qualified()
        }
        fn awkward(&self) -> String {
            Harness.awkward()
        }
        fn malformed(&self) -> Vec<String> {
            Harness.malformed()
        }
    }
    let report = run(&NoId);
    assert_eq!(report.passed.len(), 6, "{report:?}");
}
