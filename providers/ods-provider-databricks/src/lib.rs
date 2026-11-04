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
//! - [`DatabricksCredentials`]: signs in to the workspace with a service principal's
//!   OAuth secret (`auth = "m2m"`) or a static token (`auth = "pat"`), as
//!   [`Credentials`](ods_sdk::contracts::credentials::Credentials) (#297, ADR-0021).
//!   Tokens stay in memory and never leave this crate.
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

mod auth;
pub mod catalog_explorer;
mod column_lineage;
mod credentials;
pub mod delta_versions;
pub mod error_catalogue;
pub mod unity_catalog;
mod workspace;

pub use catalog_explorer::CatalogExplorer;
pub use column_lineage::{ExportFormat, UcColumnLineage};
pub use credentials::DatabricksCredentials;
pub use delta_versions::DeltaVersions;
pub use error_catalogue::DatabricksErrors;
pub use unity_catalog::UnityCatalog;

/// The `kind` Databricks providers are registered under in configuration.
pub const KIND: &str = "databricks";

/// The settings a `[providers.<name>]` with `kind = "databricks"` may have, each with
/// what it is for (ADR-0021). Any other key is refused when configuration is loaded.
/// `profile`, `cli_path` and `audience` are ADR-0021's later methods: known, and
/// refused with "isn't supported yet" when signing in.
pub const CONFIG_SETTINGS: [(&str, &str); 10] = [
    (
        "audience",
        "the token audience for `auth = \"federated\"` (not supported yet)",
    ),
    ("auth", "how ODS signs in: `m2m` or `pat`"),
    (
        "cli_path",
        "the `databricks` CLI for `auth = \"cli\"` (not supported yet)",
    ),
    (
        "client_id",
        "the service principal's application id (`auth = \"m2m\"`)",
    ),
    (
        "client_secret",
        "a secret reference to its OAuth secret (`auth = \"m2m\"`)",
    ),
    ("host", "the workspace URL; `DATABRICKS_HOST` comes first"),
    (
        "profile",
        "a Databricks CLI profile for `auth = \"cli\"` (not supported yet)",
    ),
    (
        "scopes",
        "the OAuth scopes to ask for, narrowest first (`auth = \"m2m\"`)",
    ),
    (
        "token",
        "a secret reference to a personal access token (`auth = \"pat\"`)",
    ),
    (
        "workspace_id",
        "the workspace's numeric id, for Catalog Explorer links",
    ),
];
