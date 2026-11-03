//! Scoped producing-code references. Locating metadata never reads or executes code.
use crate::{Contract, Provider, ProviderError};
use ods_core::SchemaVersion;
use serde::{Deserialize, Serialize};

/// Version of the producing-code contract and mapping document.
pub const PRODUCING_CODE: Contract = Contract {
    name: "producing_code",
    version: SchemaVersion::new(0, 1),
};

/// A logical node in a project and deployment environment. Never a bare table name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct AssetKey {
    /// Project namespace, supplied by the host.
    pub project: String,
    /// Deployment environment, supplied by the host.
    pub environment: String,
    /// Existing project node identity.
    pub node: String,
}
impl AssetKey {
    /// Creates a scoped identity; resolution validates nonempty components.
    pub fn new(
        project: impl Into<String>,
        environment: impl Into<String>,
        node: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            environment: environment.into(),
            node: node.into(),
        }
    }
    /// Rejects incomplete identities.
    /// # Errors
    /// Returns an error for any empty component.
    pub fn validate(&self) -> Result<(), ProviderError> {
        if [&self.project, &self.environment, &self.node]
            .iter()
            .any(|s| s.trim().is_empty())
        {
            return Err(invalid(
                "asset identity must include project, environment and node",
            ));
        }
        Ok(())
    }
}

/// Revision evidence; a branch is never evidence for a historical run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Revision {
    /// An immutable Git commit explicitly associated with a producing run.
    Exact {
        /// Full SHA-1 or SHA-256 hexadecimal commit ID.
        commit: String,
        /// Producing run identity, not a consumer's test invocation.
        run: String,
    },
    /// Current code only, not tied to a producing run.
    Current {
        /// A branch, tag or revision reference.
        reference: String,
    },
    /// No revision evidence is available.
    Unknown,
}
/// A location within an approved repository alias. No URL, credentials or contents.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CodeLocation {
    /// Host-managed alias; retrieval must separately enforce an allowlist.
    pub repository: String,
    /// Slash-separated relative path with no dot, empty or parent components.
    pub path: String,
    /// What is known about the revision.
    pub revision: Revision,
}
impl CodeLocation {
    /// Creates a location; call `validate` before accepting external metadata.
    #[must_use]
    pub fn new(repository: impl Into<String>, path: impl Into<String>, revision: Revision) -> Self {
        Self {
            repository: repository.into(),
            path: path.into(),
            revision,
        }
    }

    /// Validates metadata, not checkout access or the existence of the file.
    /// # Errors
    /// Returns an error for invalid aliases, paths or revision evidence.
    pub fn validate(&self) -> Result<(), ProviderError> {
        // An alias may name a directory when code is retrieved, so `.`, `..` and
        // other dot-led names are refused along with anything but `[A-Za-z0-9_.-]`.
        if self.repository.is_empty()
            || self.repository.starts_with('.')
            || !self
                .repository
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            return Err(invalid("repository must be a host-managed alias"));
        }
        if self.path.contains(['\\', ':'])
            || self.path.chars().any(char::is_control)
            || self
                .path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(invalid("code path must stay relative to its repository"));
        }
        match &self.revision {
            Revision::Exact { commit, run }
                if ![40, 64].contains(&commit.len())
                    || !commit.bytes().all(|b| b.is_ascii_hexdigit())
                    || run.trim().is_empty() =>
            {
                Err(invalid(
                    "exact revision needs a full commit and producing run",
                ))
            }
            Revision::Current { reference } if reference.trim().is_empty() => {
                Err(invalid("current revision reference is empty"))
            }
            _ => Ok(()),
        }
    }
}
/// How the association was obtained; declared exact revisions remain declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Provenance {
    /// Explicitly supplied project metadata.
    Declared,
    /// Recorded by a producing system.
    Observed,
}
/// A code location with its evidence origin.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CodeReference {
    /// Location only; content retrieval is separate.
    pub code: CodeLocation,
    /// Declared or observed association.
    pub provenance: Provenance,
}
impl CodeReference {
    /// Associates a code location with its evidence origin.
    #[must_use]
    pub fn new(code: CodeLocation, provenance: Provenance) -> Self {
        Self { code, provenance }
    }
}
/// Why provenance remains incomplete; safe to disclose without provider messages.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CodeGap {
    /// No mapping or observation matched.
    Missing,
    /// Access was explicitly denied.
    PermissionDenied,
    /// The provider cannot supply this metadata.
    Unsupported,
    /// The provider could not be reached.
    Unavailable,
    /// Multiple candidate producers require a decision.
    Ambiguous,
    /// Exact evidence belongs to another or unspecified producing run.
    RevisionMismatch,
    /// Providers disagree about locations or revisions.
    Conflict,
}
/// A provider's scoped answer. Gaps can coexist with useful references.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CodeResolution {
    /// Candidate locations in deterministic order.
    pub references: Vec<CodeReference>,
    /// Missing evidence, retained even when a fallback supplies references.
    pub gaps: Vec<CodeGap>,
}
/// One explicit association.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CodeMapping {
    /// Scoped asset identity.
    pub asset: AssetKey,
    /// Declared code location.
    pub code: CodeLocation,
}
impl CodeMapping {
    /// Associates a scoped asset with a declared location.
    #[must_use]
    pub fn new(asset: AssetKey, code: CodeLocation) -> Self {
        Self { asset, code }
    }
}
/// Generic configuration document usable without a catalog or ontology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct DeclaredMappings {
    /// Must match this pre-1.0 contract version exactly.
    pub schema_version: SchemaVersion,
    /// Explicit associations; multiple entries for an asset are retained.
    pub entries: Vec<CodeMapping>,
}
impl DeclaredMappings {
    /// Builds a versioned mapping document.
    #[must_use]
    pub fn new(entries: Vec<CodeMapping>) -> Self {
        Self {
            schema_version: PRODUCING_CODE.version,
            entries,
        }
    }
    /// Validates all entries, then resolves an exact scoped identity.
    /// # Errors
    /// Unsupported versions or invalid metadata fail the whole document.
    pub fn resolve(
        &self,
        asset: &AssetKey,
        producing_run: Option<&str>,
    ) -> Result<CodeResolution, ProviderError> {
        asset.validate()?;
        if !PRODUCING_CODE.accepts(self.schema_version) {
            return Err(invalid("unsupported producing-code schema version"));
        }
        let mut result = CodeResolution::default();
        for entry in &self.entries {
            entry.asset.validate()?;
            entry.code.validate()?;
            if &entry.asset != asset {
                continue;
            }
            if let Revision::Exact { run, .. } = &entry.code.revision
                && Some(run.as_str()) != producing_run
            {
                result.gaps.push(CodeGap::RevisionMismatch);
                continue;
            }
            result.references.push(CodeReference {
                code: entry.code.clone(),
                provenance: Provenance::Declared,
            });
        }
        result.references.sort();
        result.references.dedup();
        if result.references.len() > 1 {
            result.gaps.push(CodeGap::Ambiguous);
        }
        if result.references.is_empty() && result.gaps.is_empty() {
            result.gaps.push(CodeGap::Missing);
        }
        result.gaps.sort();
        result.gaps.dedup();
        Ok(result)
    }
}
/// Metadata-only source; provider identity/version comes from `Provider::info`.
pub trait ProducingCodeSource: Provider {
    /// Resolves a scoped asset, optionally for a known producing run.
    /// # Errors
    /// Invalid inputs or unexpected provider failures. Expected gaps are typed.
    fn producing_code(
        &self,
        asset: &AssetKey,
        producing_run: Option<&str>,
    ) -> Result<CodeResolution, ProviderError>;
}
fn invalid(message: &str) -> ProviderError {
    ProviderError::Other(message.into())
}
