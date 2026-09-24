//! Versioned plugin contracts for OpenDataSuite providers.
//!
//! Providers (dbt, Databricks, SQLite, …) implement the traits defined here; modules and
//! the CLI depend only on these contracts, never on concrete providers (ADR-0001).
//! The contracts themselves arrive with #2 (plugin SDK) and #3 (capability negotiation).

use ods_core::SchemaVersion;

/// Version of the plugin contract surface exposed by this crate.
///
/// Providers built against a different major version are refused at load time.
pub const SDK_VERSION: SchemaVersion = SchemaVersion::new(0, 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_is_pre_1_0() {
        assert_eq!(SDK_VERSION.major, 0);
    }
}
