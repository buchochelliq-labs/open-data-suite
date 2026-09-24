//! What every provider is and says about itself (ADR-0006 §2).

use ods_core::{CapabilitySet, SchemaVersion};
use serde::Serialize;

/// A contract: a named, versioned trait that providers implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Contract {
    /// Stable name, e.g. `lock_provider`.
    pub name: &'static str,
    /// The contract version this SDK defines.
    pub version: SchemaVersion,
}

impl Contract {
    /// Whether a provider built against `built_against` works with this contract
    /// version (ADR-0006 §6):
    /// - before 1.0, any minor bump may break providers, so the versions must match
    ///   exactly;
    /// - from 1.0, the major must match and the provider's minor must not be newer
    ///   than this SDK's.
    pub const fn accepts(&self, built_against: SchemaVersion) -> bool {
        if self.version.major == 0 {
            built_against.major == 0 && built_against.minor == self.version.minor
        } else {
            self.version.can_read(built_against)
        }
    }
}

/// How a provider describes itself. Shown by diagnostics and used by planners.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProviderInfo {
    /// The provider kind, as written in configuration (`providers.<name>.kind`).
    pub kind: String,
    /// The configured instance name (`providers.<name>`).
    pub instance: String,
    /// The provider implementation's own version.
    pub version: String,
    /// What this instance can do. Core chooses strategies from this, never from `kind`.
    pub capabilities: CapabilitySet,
}

impl ProviderInfo {
    /// Creates provider information.
    pub fn new(
        kind: impl Into<String>,
        instance: impl Into<String>,
        version: impl Into<String>,
        capabilities: CapabilitySet,
    ) -> Self {
        Self {
            kind: kind.into(),
            instance: instance.into(),
            version: version.into(),
            capabilities,
        }
    }
}

/// Implemented by every provider, whatever contracts it serves.
pub trait Provider: Send + Sync {
    /// Describes this provider instance.
    fn info(&self) -> ProviderInfo;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(major: u32, minor: u32) -> Contract {
        Contract {
            name: "test",
            version: SchemaVersion::new(major, minor),
        }
    }

    #[test]
    fn pre_1_0_contracts_need_an_exact_minor() {
        let host = contract(0, 2);
        assert!(host.accepts(SchemaVersion::new(0, 2)));
        assert!(!host.accepts(SchemaVersion::new(0, 1)));
        assert!(!host.accepts(SchemaVersion::new(0, 3)));
        assert!(!host.accepts(SchemaVersion::new(1, 2)));
    }

    #[test]
    fn stable_contracts_accept_older_minors_of_the_same_major() {
        let host = contract(1, 2);
        assert!(host.accepts(SchemaVersion::new(1, 0)));
        assert!(host.accepts(SchemaVersion::new(1, 2)));
        assert!(!host.accepts(SchemaVersion::new(1, 3)));
        assert!(!host.accepts(SchemaVersion::new(2, 0)));
        assert!(!host.accepts(SchemaVersion::new(0, 2)));
    }
}
