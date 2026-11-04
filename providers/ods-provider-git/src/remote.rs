//! Credential-free HTTPS fetches into a disposable local object store.
use super::{LocalGitCode, command};
use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_sdk::contracts::{
    code_reader::{CodeDocument, CodeReadError, CodeReader},
    producing_code::{CodeLocation, Revision},
};
use ods_sdk::{Provider, ProviderInfo};
use std::{collections::BTreeMap, time::Duration};

/// Read-only public HTTPS source reader. Repository URLs are host allowlists, never
/// taken from project metadata. This provider has no credentials or push operation.
/// Private repositories require a separate read-only credentialed provider; it must
/// not reuse a developer's ambient SSH agent, token or credential helper.
pub struct RemoteGitCode {
    repositories: BTreeMap<String, String>,
    max_bytes: usize,
    timeout: Duration,
}
impl RemoteGitCode {
    /// Approves explicit credential-free HTTPS repositories and resource limits.
    /// # Errors
    /// Rejects non-HTTPS URLs, user info, query strings, fragments and invalid limits.
    pub fn new(
        repositories: BTreeMap<String, String>,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Self, CodeReadError> {
        LocalGitCode::new(BTreeMap::new(), max_bytes, timeout)?;
        for value in repositories.values() {
            let url = url::Url::parse(value).map_err(|_| CodeReadError::InvalidRequest)?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(CodeReadError::InvalidRequest);
            }
        }
        Ok(Self {
            repositories,
            max_bytes,
            timeout,
        })
    }
    async fn fetch(
        &self,
        location: &CodeLocation,
        run: &str,
        commit: &str,
    ) -> Result<CodeDocument, CodeReadError> {
        let url = self
            .repositories
            .get(&location.repository)
            .ok_or(CodeReadError::RepositoryNotAllowed)?;
        let checkout = tempfile::tempdir().map_err(|_| CodeReadError::Unavailable)?;
        let git_dir = checkout.path().join(".git");
        initialize(&git_dir, commit).await?;
        // Fetch one immutable commit; no remote is configured and no working tree
        // checked out. Git cannot discover/use a push URL from the repository.
        command(
            &git_dir,
            &[
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "--depth=1",
                "--",
                url,
                commit,
            ],
            4096,
        )
        .await?;
        let local = LocalGitCode::new(
            BTreeMap::from([(location.repository.clone(), checkout.path().to_path_buf())]),
            self.max_bytes,
            self.timeout,
        )?;
        local.read_code(location, Some(run)).await
    }
}
impl Provider for RemoteGitCode {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            "git",
            "remote_code",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}
#[async_trait]
impl CodeReader for RemoteGitCode {
    async fn verify_read_access(
        &self,
        _location: &CodeLocation,
    ) -> Result<ods_sdk::contracts::code_reader::CodeReadAccess, CodeReadError> {
        Ok(ods_sdk::contracts::code_reader::CodeReadAccess::Unauthenticated)
    }

    async fn read_code(
        &self,
        location: &CodeLocation,
        producing_run: Option<&str>,
    ) -> Result<CodeDocument, CodeReadError> {
        location
            .validate()
            .map_err(|_| CodeReadError::InvalidRequest)?;
        let Revision::Exact { commit, run } = &location.revision else {
            return Err(CodeReadError::RevisionRequired);
        };
        if Some(run.as_str()) != producing_run {
            return Err(CodeReadError::RevisionMismatch);
        }
        tokio::time::timeout(self.timeout, self.fetch(location, run, commit))
            .await
            .map_err(|_| CodeReadError::TimedOut)?
    }
}

async fn initialize(git_dir: &std::path::Path, commit: &str) -> Result<(), CodeReadError> {
    let format = if commit.len() == 64 {
        "--object-format=sha256"
    } else {
        "--object-format=sha1"
    };
    command(git_dir, &["init", "--bare", "--template=", format], 4096).await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn remote_store_matches_revision_hash_format() {
        let root = tempfile::tempdir().unwrap();
        let git_dir = root.path().join(".git");
        initialize(&git_dir, &"a".repeat(64)).await.unwrap();
        let format = command(&git_dir, &["rev-parse", "--show-object-format"], 128)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(format).unwrap().trim(), "sha256");
    }
}
