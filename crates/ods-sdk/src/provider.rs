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
    /// version: same major, and no newer minor than this SDK knows about.
    pub const fn accepts(&self, built_against: SchemaVersion) -> bool {
        self.version.can_read(built_against)
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
