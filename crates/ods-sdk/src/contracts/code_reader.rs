//! Opt-in source retrieval, separate from locating producing-code references.
use super::producing_code::CodeLocation;
use crate::{Contract, Provider};
use async_trait::async_trait;
use ods_core::SchemaVersion;

/// Version of the source-retrieval contract.
pub const CODE_READER: Contract = Contract {
    name: "code_reader",
    version: SchemaVersion::new(0, 1),
};

/// Expected retrieval failures, without paths, credentials, source or stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CodeReadError {
    /// Credential authority reports write capability on the repository.
    #[error("write-capable authentication is forbidden for source readers")]
    WriteCapableAuth,
    /// Read-only permission could not be established authoritatively.
    #[error("read-only authentication could not be verified")]
    AuthUnverified,
    /// Invalid location or resource limit.
    #[error("invalid source retrieval request")]
    InvalidRequest,
    /// Alias is not in the host's approved repositories.
    #[error("repository is not approved")]
    RepositoryNotAllowed,
    /// Only an immutable revision associated with a producing run is accepted.
    #[error("an exact producing revision is required")]
    RevisionRequired,
    /// The requested run does not match the declared producing run.
    #[error("producing run does not match revision evidence")]
    RevisionMismatch,
    /// Commit is not available locally.
    #[error("revision is not available locally")]
    RevisionMissing,
    /// No such path at this commit.
    #[error("path is absent at the requested revision")]
    PathMissing,
    /// Directory, symbolic link, submodule or unsupported repository layout.
    #[error("repository entry or layout is unsupported")]
    UnsupportedEntry,
    /// Content or metadata exceeds the host's resource limit.
    #[error("source exceeds the size limit")]
    TooLarge,
    /// Source is not UTF-8 text.
    #[error("source is not UTF-8 text")]
    NotText,
    /// Local operating-system access was denied.
    #[error("source access was denied")]
    PermissionDenied,
    /// Local I/O or Git execution failed.
    #[error("source retrieval is unavailable")]
    Unavailable,
    /// Retrieval exceeded the host's total time limit.
    #[error("source retrieval timed out")]
    TimedOut,
}

/// Source text is intentionally not serializable, and Debug omits its contents.
/// It is code, not a data sample, but may itself contain sensitive literals. Hosts
/// must explicitly authorize any disclosure; generic investigation reports should
/// contain only location/revision metadata.
#[derive(Clone)]
pub struct CodeDocument {
    location: CodeLocation,
    commit: String,
    text: String,
}
impl CodeDocument {
    /// Constructs a provider result. Hosts must validate identity and size before
    /// consuming results from arbitrary plugins.
    #[must_use]
    pub fn new(location: CodeLocation, commit: String, text: String) -> Self {
        Self {
            location,
            commit,
            text,
        }
    }
    /// Requested code location, including declared revision evidence.
    #[must_use]
    pub fn location(&self) -> &CodeLocation {
        &self.location
    }
    /// Immutable commit actually read.
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }
    /// Explicit access to source text. This is not safe for automatic disclosure.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}
impl std::fmt::Debug for CodeDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeDocument")
            .field("commit", &self.commit)
            .field("bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}
/// Result of a trusted provider's authoritative credential inspection. This is
/// not a user-configurable promise. Credentialed adapters must inspect all grants
/// applicable to the target repository before returning `VerifiedReadOnly`; absence
/// of a successful push attempt is never proof of read-only access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodeReadAccess {
    /// No authentication is supplied or inherited by the reader process.
    Unauthenticated,
    /// The remote authority verified this credential can read but cannot write.
    VerifiedReadOnly,
    /// At least one effective grant permits a repository write.
    WriteCapable,
    /// Permissions are unavailable, incomplete, stale or unverifiable.
    Unverified,
}

/// Refuses write-capable or unverified authentication before retrieval.
/// # Errors
/// Any verification failure is fatal; no fallback to using the credential.
pub async fn require_read_only(
    reader: &dyn CodeReader,
    location: &CodeLocation,
) -> Result<(), CodeReadError> {
    match reader.verify_read_access(location).await? {
        CodeReadAccess::Unauthenticated | CodeReadAccess::VerifiedReadOnly => Ok(()),
        CodeReadAccess::WriteCapable => Err(CodeReadError::WriteCapableAuth),
        CodeReadAccess::Unverified => Err(CodeReadError::AuthUnverified),
    }
}

/// Trusted provider plugin retrieving source without executing it.
#[async_trait]
pub trait CodeReader: Provider {
    /// Establishes effective read-only access for this repository before any fetch.
    /// The default denies access. Authenticated adapters must query the authority,
    /// reject write grants and fail closed when verification is unavailable. A
    /// caller-supplied flag or advertised plugin capability is not verification.
    /// # Errors
    /// Verification failures must not expose credentials or allow a fetch.
    async fn verify_read_access(
        &self,
        _location: &CodeLocation,
    ) -> Result<CodeReadAccess, CodeReadError> {
        Ok(CodeReadAccess::Unverified)
    }

    /// Reads an approved location at an exact producing revision. Implementations
    /// must enforce their own repository allowlist, byte bound and total deadline.
    /// # Errors
    /// Returns a typed gap; never an empty substitute for missing code.
    async fn read_code(
        &self,
        location: &CodeLocation,
        producing_run: Option<&str>,
    ) -> Result<CodeDocument, CodeReadError>;
}
