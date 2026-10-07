//! The built-in `DuckDB` plugin (ADR-0031 §3, §3a): `DuckDB`'s error messages, as
//! dbt-duckdb reports them (ADR-0025), and the `DuckDB` dialect. The only place the CLI
//! names `DuckDB`'s provider.
//!
//! `DuckDB` has no table version to read, so the plugin offers no source versions: `ods
//! state` uses `sources.json` alone, as before. Nor does it offer a login check, links
//! or observed lineage: a local database file has none of them.

use std::sync::Arc;

use ods_provider_duckdb::DuckdbErrors;
use ods_sdk::contracts::error_catalogue::ErrorCatalogue;

use super::{Origin, WarehousePlugin};

/// The `DuckDB` plugin.
pub(super) struct Duckdb;

impl WarehousePlugin for Duckdb {
    fn origin(&self) -> Origin {
        Origin {
            name: "ods-provider-duckdb",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn warehouse(&self) -> &str {
        ods_provider_duckdb::KIND
    }

    fn errors(&self) -> Option<Arc<dyn ErrorCatalogue>> {
        Some(Arc::new(DuckdbErrors))
    }

    fn dialect(&self) -> Option<&str> {
        Some("duckdb")
    }
}
