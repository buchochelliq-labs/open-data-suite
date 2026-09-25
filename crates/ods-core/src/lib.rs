//! Provider-neutral semantic graph and domain model for OpenDataSuite.
//!
//! `ods-core` sits at the bottom of the dependency graph (ADR-0001): it depends on no other
//! ODS crate and must never contain warehouse- or runtime-specific logic. It holds the
//! capability vocabulary and strategy choice (#3, ADR-0006), the State domain model
//! (ADR-0013), and the conventions every persisted domain type follows. The semantic graph itself is delivered by #4.

pub mod capability;
pub mod freshness;
pub mod lineage;
pub mod state;
pub mod strategy;

pub use capability::{Capability, CapabilitySet, CustomCapability, UnknownCapability};
pub use freshness::{FreshnessPolicy, LoadedAt, PolicyOrigin, Quorum, UnappliedSetting};
pub use lineage::{ColumnRef, Confidence, DirectKind, EdgeKind, IndirectKind, RelationName};
pub use strategy::{Choice, ChoiceError, Skipped, Strategy, choose};

use serde::{Deserialize, Serialize};

/// Version stamp carried by every persisted or serialized ODS document.
///
/// Readers use it to pick a migration path; an incompatible change bumps `major`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SchemaVersion {
    /// Incremented on backward-incompatible changes.
    pub major: u32,
    /// Incremented on backward-compatible additions.
    pub minor: u32,
}

impl SchemaVersion {
    /// Creates a schema version.
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// Whether a reader that understands `self` can read a document written as `other`.
    ///
    /// Same major and a minor no newer than ours: unknown additions from a newer minor
    /// could carry meaning we would silently drop, so they are rejected (conservative).
    pub const fn can_read(self, other: Self) -> bool {
        self.major == other.major && other.minor <= self.minor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_same_or_older_minor_only() {
        let reader = SchemaVersion::new(1, 2);
        assert!(reader.can_read(SchemaVersion::new(1, 0)));
        assert!(reader.can_read(SchemaVersion::new(1, 2)));
        assert!(!reader.can_read(SchemaVersion::new(1, 3)));
        assert!(!reader.can_read(SchemaVersion::new(2, 0)));
    }

    #[test]
    fn serializes_as_snake_case_object() {
        let json = serde_json::to_string(&SchemaVersion::new(1, 0)).unwrap();
        assert_eq!(json, r#"{"major":1,"minor":0}"#);
    }
}
