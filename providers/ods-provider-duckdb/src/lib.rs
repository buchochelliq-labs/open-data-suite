//! `DuckDB` provider for OpenDataSuite.
//!
//! - [`DuckdbErrors`]: `DuckDB`'s error kinds and messages, as dbt-duckdb reports them,
//!   as an [`ErrorCatalogue`](ods_sdk::contracts::error_catalogue::ErrorCatalogue)
//!   (ADR-0025, ADR-0031 §3a).
//!
//! `DuckDB` has no table version to read, so there is no source-version provider: `ods
//! state` uses `sources.json` alone on `DuckDB`, as it does for any warehouse without one.

pub mod error_catalogue;

pub use error_catalogue::DuckdbErrors;

/// The warehouse kind dbt-duckdb reports (`adapter_type`).
pub const KIND: &str = "duckdb";
