//! In-memory [`RelationLinker`] (#329): links to `https://<host>/relations/<a>/<b>/<c>`
//! for relations named `a.b.c`, with `"`-quoted names as ANSI SQL quotes them.

use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::relation_link::{
    NoRelationLink, RelationLink, RelationLinker, path_segment, split_relation,
};
use ods_sdk::{Provider, ProviderInfo};

use crate::KIND;

/// A linker for a fake warehouse UI.
#[derive(Debug, Clone)]
pub struct FakeRelationLinker {
    host: Option<String>,
    parts: usize,
}

impl FakeRelationLinker {
    /// Links under `host` (no scheme), for three-part relations.
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: Some(host.into()),
            parts: 3,
        }
    }

    /// A linker whose host isn't configured: every link is refused, saying so.
    pub fn unconfigured() -> Self {
        Self {
            host: None,
            parts: 3,
        }
    }

    /// Needs `parts` parts per relation instead of three.
    #[must_use]
    pub fn with_parts(mut self, parts: usize) -> Self {
        self.parts = parts;
        self
    }
}

impl Provider for FakeRelationLinker {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "fake",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::RelationLink]),
        )
    }
}

impl RelationLinker for FakeRelationLinker {
    fn link(&self, relation: &str) -> Result<RelationLink, NoRelationLink> {
        let host = self.host.as_deref().ok_or(NoRelationLink::NotConfigured {
            setting: "host".to_owned(),
        })?;
        let relation = relation.trim();
        let invalid = |why: String| NoRelationLink::InvalidName {
            relation: relation.to_owned(),
            why,
        };
        let parts = split_relation(relation, '"').map_err(invalid)?;
        if parts.len() != self.parts {
            return Err(NoRelationLink::NotQualified {
                relation: relation.to_owned(),
                parts: parts.len(),
                needed: self.parts,
            });
        }
        let path = parts
            .iter()
            .map(|p| path_segment(p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid)?;
        Ok(RelationLink::new(
            format!("https://{host}/relations/{}", path.join("/")),
            "Open in the fake warehouse",
        ))
    }
}
