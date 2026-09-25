//! Databricks provider for OpenDataSuite.
//!
//! Today: reading Unity Catalog's lineage system table,
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

mod column_lineage;

pub use column_lineage::{ExportFormat, UcColumnLineage};

/// The `kind` Databricks providers are registered under in configuration.
pub const KIND: &str = "databricks";
