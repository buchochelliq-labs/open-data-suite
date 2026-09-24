//! Versioned plugin contracts for OpenDataSuite providers (#2, ADR-0006).
//!
//! Providers (dbt, Databricks, SQLite, …) implement the contract traits in
//! [`contracts`]. Modules and the CLI depend only on these contracts, never on concrete
//! providers (ADR-0001). A provider describes itself with [`ProviderInfo`], including the
//! [`Capability`](ods_core::Capability) set that planners choose strategies from (#3).
//!
//! # Versioning
//! [`SDK_VERSION`] versions the SDK as a whole, and each contract has its own
//! [`Contract::version`]. While the major version is 0, any minor bump may break
//! providers. From 1.0:
//! - removing or changing a method is a major bump;
//! - adding a method with a default implementation, or a new contract, is a minor bump;
//! - documentation and fixes are patch releases.
//!
//! A provider built against contract version `M.p` works with a host at `M.m` when
//! `p <= m` (see [`Contract::accepts`]).

pub mod contracts;
mod error;
mod provider;
mod registry;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use error::ProviderError;
pub use provider::{Contract, Provider, ProviderInfo};
pub use registry::{ProviderFactory, Registry, RegistryError};

use ods_core::SchemaVersion;

/// Version of the plugin contract surface exposed by this crate.
pub const SDK_VERSION: SchemaVersion = SchemaVersion::new(0, 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_is_pre_1_0() {
        assert_eq!(SDK_VERSION.major, 0);
    }
}
