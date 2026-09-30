//! In-memory [`RelationLinker`] (#329): links to `https://<host>/relations/<a>/<b>/<c>`
//! for relations named `a.b.c`, with `"`-quoted names as ANSI SQL quotes them.

use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLink, RelationLinker};
use ods_sdk::{Provider, ProviderInfo};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::KIND;

/// Kept as they are in a path segment; everything else is percent-encoded.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

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

/// `a."b.c".d` → `["a", "b.c", "d"]`; `None` if a quote isn't closed or a part is empty.
fn parts(relation: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = relation.trim().chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                current.push('"');
            }
            '"' => quoted = !quoted,
            '.' if !quoted => out.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    out.push(current);
    (!quoted && out.iter().all(|p| !p.is_empty())).then_some(out)
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
        let parts = parts(relation).unwrap_or_default();
        if parts.len() != self.parts {
            return Err(NoRelationLink::NotQualified {
                relation: relation.to_owned(),
                parts: parts.len(),
                needed: self.parts,
            });
        }
        let path: Vec<String> = parts
            .iter()
            .map(|p| utf8_percent_encode(p, SEGMENT).to_string())
            .collect();
        Ok(RelationLink::new(
            format!("https://{host}/relations/{}", path.join("/")),
            "Open in the fake warehouse",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_names() {
        assert_eq!(parts(r#"a."b.c"."d""e""#).unwrap(), ["a", "b.c", r#"d"e"#]);
        assert_eq!(parts(r#"a."b"#), None);
        assert_eq!(parts("a..b"), None);
    }
}
