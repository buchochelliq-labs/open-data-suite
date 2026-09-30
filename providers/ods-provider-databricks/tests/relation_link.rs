//! Catalog Explorer links pass the `RelationLinker` conformance suite.

use std::sync::Arc;

use ods_provider_databricks::CatalogExplorer;
use ods_sdk::conformance::relation_link::{RelationLinkHarness, run};
use ods_sdk::contracts::relation_link::RelationLinker;

struct Harness;

impl RelationLinkHarness for Harness {
    fn linker(&self) -> Arc<dyn RelationLinker> {
        Arc::new(CatalogExplorer::new(
            "uc",
            Some("https://dbc-0123.cloud.databricks.com/"),
        ))
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
}

#[test]
fn conforms() {
    let report = run(&Harness);
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(report.passed.len(), 5, "{report:?}");
}

#[test]
fn links_as_the_issue_specifies() {
    let link = Harness.linker().link("`catalog`.`schema`.`table`").unwrap();
    assert_eq!(
        link.url,
        "https://dbc-0123.cloud.databricks.com/explore/data/catalog/schema/table"
    );
    assert_eq!(link.label, "Open in Catalog Explorer");
}
