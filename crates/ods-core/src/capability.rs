//! Provider capabilities (#3, ADR-0006 §3).
//!
//! Providers advertise what they can do; planners choose strategies from those
//! capabilities. Core code never asks *which* provider it is talking to, only *what* it
//! can do (AGENTS.md rule 1).

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Something a provider can do.
///
/// Well-known capabilities have variants; anything else is [`Capability::Custom`], a
/// namespaced name such as `x-acme.bulk_load`, so third-party providers can extend the
/// vocabulary without changing core. Serialized as its `snake_case` name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Capability {
    /// Relations carry a monotonic content version (e.g. a table version number).
    RelationVersions,
    /// A relation can be copied without copying its data (zero-copy or shallow clone).
    ZeroCopyClone,
    /// A relation can be replaced atomically, with no window where it is missing.
    AtomicReplace,
    /// Changes to a relation since a version or time can be listed.
    ChangeTracking,
    /// Past queries against relations can be read.
    QueryHistory,
    /// Source data freshness (last load time) can be read.
    SourceFreshness,
    /// A relation's schema history is versioned.
    SchemaVersioning,
    /// Column-level usage can be reported.
    ColumnUsage,
    /// Primary key, unique and foreign key constraints can be read.
    ConstraintMetadata,
    /// Locks expire unless renewed (leases).
    LeaseExpiry,
    /// Each lock grant carries a strictly increasing fencing token.
    FencingTokens,
    /// A capability outside the well-known set, as `x-<namespace>.<name>`.
    Custom(String),
}

impl Capability {
    /// Every well-known capability, for documentation and tests.
    pub const WELL_KNOWN: [Capability; 11] = [
        Capability::RelationVersions,
        Capability::ZeroCopyClone,
        Capability::AtomicReplace,
        Capability::ChangeTracking,
        Capability::QueryHistory,
        Capability::SourceFreshness,
        Capability::SchemaVersioning,
        Capability::ColumnUsage,
        Capability::ConstraintMetadata,
        Capability::LeaseExpiry,
        Capability::FencingTokens,
    ];

    /// The capability's stable name.
    pub fn name(&self) -> &str {
        match self {
            Capability::RelationVersions => "relation_versions",
            Capability::ZeroCopyClone => "zero_copy_clone",
            Capability::AtomicReplace => "atomic_replace",
            Capability::ChangeTracking => "change_tracking",
            Capability::QueryHistory => "query_history",
            Capability::SourceFreshness => "source_freshness",
            Capability::SchemaVersioning => "schema_versioning",
            Capability::ColumnUsage => "column_usage",
            Capability::ConstraintMetadata => "constraint_metadata",
            Capability::LeaseExpiry => "lease_expiry",
            Capability::FencingTokens => "fencing_tokens",
            Capability::Custom(name) => name,
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a capability name was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{0}` is not a known capability; custom capabilities are `x-<namespace>.<name>` \
     using lowercase letters, digits, `_` and `-`"
)]
pub struct UnknownCapability(pub String);

impl FromStr for Capability {
    type Err = UnknownCapability;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        if let Some(known) = Capability::WELL_KNOWN.iter().find(|c| c.name() == name) {
            return Ok(known.clone());
        }
        let custom = name
            .strip_prefix("x-")
            .and_then(|rest| rest.split_once('.'));
        let valid = |part: &str| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        };
        match custom {
            Some((namespace, local)) if valid(namespace) && valid(local) => {
                Ok(Capability::Custom(name.to_owned()))
            }
            _ => Err(UnknownCapability(name.to_owned())),
        }
    }
}

impl Serialize for Capability {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

/// A sorted set of capabilities (deterministic in output and hashes).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet(BTreeSet<Capability>);

impl CapabilitySet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `capability` is in the set.
    pub fn contains(&self, capability: &Capability) -> bool {
        self.0.contains(capability)
    }

    /// Adds a capability.
    pub fn insert(&mut self, capability: Capability) {
        self.0.insert(capability);
    }

    /// Removes a capability.
    pub fn remove(&mut self, capability: &Capability) {
        self.0.remove(capability);
    }

    /// Capabilities in `required` that this set lacks, sorted.
    pub fn missing<'a>(&self, required: &'a CapabilitySet) -> Vec<&'a Capability> {
        required.0.iter().filter(|c| !self.0.contains(*c)).collect()
    }

    /// Whether every capability in `required` is present.
    pub fn satisfies(&self, required: &CapabilitySet) -> bool {
        self.missing(required).is_empty()
    }

    /// Iterates in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.0.iter()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<const N: usize> From<[Capability; N]> for CapabilitySet {
    fn from(capabilities: [Capability; N]) -> Self {
        capabilities.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for capability in Capability::WELL_KNOWN {
            assert_eq!(
                capability.name().parse::<Capability>(),
                Ok(capability.clone())
            );
        }
        assert_eq!(
            "x-acme.bulk_load".parse::<Capability>(),
            Ok(Capability::Custom("x-acme.bulk_load".into()))
        );
    }

    #[test]
    fn rejects_unknown_and_malformed_names() {
        for bad in ["teleport", "x-acme", "x-.name", "x-Acme.name", "acme.name"] {
            assert!(bad.parse::<Capability>().is_err(), "{bad}");
        }
    }

    #[test]
    fn serializes_as_sorted_names() {
        let set = CapabilitySet::from([Capability::ZeroCopyClone, Capability::AtomicReplace]);
        let json = serde_json::to_string(&set).unwrap();
        assert_eq!(json, r#"["zero_copy_clone","atomic_replace"]"#);
        assert_eq!(serde_json::from_str::<CapabilitySet>(&json).unwrap(), set);
    }

    #[test]
    fn missing_lists_what_is_lacking() {
        let offered = CapabilitySet::from([Capability::RelationVersions]);
        let required =
            CapabilitySet::from([Capability::RelationVersions, Capability::ZeroCopyClone]);
        assert_eq!(offered.missing(&required), [&Capability::ZeroCopyClone]);
        assert!(!offered.satisfies(&required));
    }
}
