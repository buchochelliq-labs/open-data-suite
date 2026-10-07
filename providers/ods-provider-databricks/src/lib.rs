//! Databricks provider for OpenDataSuite.
//!
//! - [`DeltaVersions`]: each source's Delta table version, read through any
//!   [`RelationProbe`](ods_sdk::contracts::probe::RelationProbe), as a
//!   [`ChangeProvider`](ods_sdk::contracts::changes::ChangeProvider) (#17, ADR-0022).
//! - [`UnityCatalog`]: probes through any
//!   [`RelationProbe`](ods_sdk::contracts::probe::RelationProbe) and reports, from
//!   Unity Catalog's `information_schema`, what its login may do on each relation, as
//!   [`RelationPrivileges`](ods_sdk::contracts::privileges::RelationPrivileges) (#392,
//!   ADR-0030 §4c).
//! - [`CatalogExplorer`]: links to a relation's page in Catalog Explorer, as a
//!   [`RelationLinker`](ods_sdk::contracts::relation_link::RelationLinker) (#329).
//!
//! Reading Unity Catalog's lineage system table,
//! [`system.access.column_lineage`](https://docs.databricks.com/aws/en/admin/system-tables/lineage),
//! from an export, as [`ObservedLineage`](ods_sdk::contracts::observed_lineage::ObservedLineage)
//! ([`UcColumnLineage`]). Unity Catalog records column lineage for queries run by notebooks,
//! jobs, pipelines and SQL warehouses, whatever wrote them, Python included. That makes it
//! both a check on static analysis and a source of lineage for code the analyzer can't read.
//!
//! Exports are read, not queried, so tests need no workspace and no credentials
//! (AGENTS.md rules 9 and "no network in tests"). The export is a CSV or JSON download
//! of a query such as:
//!
//! ```sql
//! SELECT source_table_full_name, source_column_name,
//!        target_table_full_name, target_column_name, event_time
//! FROM system.access.column_lineage
//! WHERE target_table_catalog = 'analytics'
//!   AND event_date >= current_date() - INTERVAL 30 DAYS
//! ```

pub mod catalog_explorer;
mod column_lineage;
pub mod delta_versions;
pub mod error_catalogue;
pub mod unity_catalog;

pub use catalog_explorer::CatalogExplorer;
pub use column_lineage::{ExportFormat, UcColumnLineage};
pub use delta_versions::DeltaVersions;
pub use error_catalogue::DatabricksErrors;
pub use unity_catalog::UnityCatalog;

/// The `kind` Databricks providers are registered under in configuration.
pub const KIND: &str = "databricks";
