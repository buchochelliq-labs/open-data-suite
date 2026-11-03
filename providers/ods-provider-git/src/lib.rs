//! Bounded reads from host-approved local Git object stores; never worktree files.
//! Depends only on ODS contracts and Tokio process I/O. Git 2.48+ is required.
use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_sdk::contracts::code_reader::{CodeDocument, CodeReadError, CodeReader};
use ods_sdk::contracts::producing_code::{CodeLocation, Revision};
use ods_sdk::{Provider, ProviderInfo};
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};
mod remote;
pub use remote::RemoteGitCode;

/// Host-approved checkout roots and resource limits. Roots must be standard local
/// repositories controlled by the host; concurrent mutation of Git internals is
/// outside this trust boundary. Linked worktrees and bare repositories are refused.
pub struct LocalGitCode {
    repositories: BTreeMap<String, PathBuf>,
    max_bytes: usize,
    timeout: Duration,
}
impl LocalGitCode {
    /// Configures approved aliases. No discovery, clone or credential lookup occurs.
    /// # Errors
    /// Rejects zero limits or byte limits above 8 MiB.
    pub fn new(
        repositories: BTreeMap<String, PathBuf>,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Self, CodeReadError> {
        if max_bytes == 0 || max_bytes > 8 * 1024 * 1024 || timeout.is_zero() {
            return Err(CodeReadError::InvalidRequest);
        }
        Ok(Self {
            repositories,
            max_bytes,
            timeout,
        })
    }
    async fn read(
        &self,
        location: &CodeLocation,
        commit: &str,
    ) -> Result<CodeDocument, CodeReadError> {
        let root = self
            .repositories
            .get(&location.repository)
            .ok_or(CodeReadError::RepositoryNotAllowed)?;
        let root = canonical(root).await?;
        let git_dir = canonical(&root.join(".git")).await?;
        if !tokio::fs::metadata(&git_dir)
            .await
            .map_err(|e| io_error(&e))?
            .is_dir()
            || !git_dir.starts_with(&root)
        {
            return Err(CodeReadError::UnsupportedEntry);
        }
        let objects = canonical(&git_dir.join("objects")).await?;
        if !objects.starts_with(&git_dir)
            || tokio::fs::try_exists(git_dir.join("objects/info/alternates"))
                .await
                .map_err(|e| io_error(&e))?
        {
            return Err(CodeReadError::UnsupportedEntry);
        }
        let rev = format!("{commit}^{{commit}}");
        let resolved = command(
            &git_dir,
            &["rev-parse", "--verify", "--end-of-options", &rev],
            128,
        )
        .await
        .map_err(|e| {
            if e == CodeReadError::Unavailable {
                CodeReadError::RevisionMissing
            } else {
                e
            }
        })?;
        let resolved = String::from_utf8(resolved)
            .map_err(|_| CodeReadError::Unavailable)?
            .trim()
            .to_owned();
        if !resolved.eq_ignore_ascii_case(commit) {
            return Err(CodeReadError::RevisionMismatch);
        }
        // Literal pathspec: wildcard and magic characters in a filename remain names.
        let pathspec = format!(":(literal){}", location.path);
        let tree = command(
            &git_dir,
            &["ls-tree", "-z", &resolved, "--", &pathspec],
            65536,
        )
        .await?;
        if tree.is_empty() {
            return Err(CodeReadError::PathMissing);
        }
        let tree = std::str::from_utf8(&tree).map_err(|_| CodeReadError::UnsupportedEntry)?;
        let record = tree
            .strip_suffix('\0')
            .ok_or(CodeReadError::UnsupportedEntry)?;
        let (header, path) = record
            .split_once('\t')
            .ok_or(CodeReadError::UnsupportedEntry)?;
        if path != location.path || record.contains('\0') {
            return Err(CodeReadError::UnsupportedEntry);
        }
        let parts: Vec<_> = header.split(' ').collect();
        if parts.len() != 3 || !matches!(parts[0], "100644" | "100755") || parts[1] != "blob" {
            return Err(CodeReadError::UnsupportedEntry);
        }
        let object = parts[2];
        let bytes = command(&git_dir, &["cat-file", "blob", object], self.max_bytes).await?;
        let text = String::from_utf8(bytes).map_err(|_| CodeReadError::NotText)?;
        if text.contains('\0') {
            return Err(CodeReadError::NotText);
        }
        Ok(CodeDocument::new(location.clone(), resolved, text))
    }
}
impl Provider for LocalGitCode {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            "git",
            "local_code",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}
#[async_trait]
impl CodeReader for LocalGitCode {
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
        tokio::time::timeout(self.timeout, self.read(location, commit))
            .await
            .map_err(|_| CodeReadError::TimedOut)?
    }
}
async fn command(
    git_dir: &Path,
    args: &[&str],
    max_bytes: usize,
) -> Result<Vec<u8>, CodeReadError> {
    let mut cmd = Command::new("git");
    // No ambient tokens, SSH agents, askpass, proxy credentials or Git redirection.
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("HOME", git_dir.parent().unwrap_or(git_dir));
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("--no-replace-objects")
        .arg("--no-lazy-fetch")
        .arg("-c")
        .arg("credential.helper=")
        .arg("-c")
        .arg("core.askPass=")
        .arg("-c")
        .arg("http.extraHeader=")
        .arg("-c")
        .arg("http.followRedirects=false")
        .arg("--git-dir")
        .arg(git_dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    run_command(cmd, max_bytes).await
}

async fn run_command(cmd: Command, max_bytes: usize) -> Result<Vec<u8>, CodeReadError> {
    let mut wrapped = CommandWrap::from(cmd);
    #[cfg(unix)]
    wrapped.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    wrapped.wrap(JobObject);
    wrapped.wrap(KillOnDrop);
    let mut running = Running {
        child: wrapped.spawn().map_err(|e| io_error(&e))?,
        finished: false,
    };
    let child = &mut running.child;
    let stdout = child.stdout().take().ok_or(CodeReadError::Unavailable)?;
    let mut bytes = Vec::new();
    stdout
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| io_error(&e))?;
    if bytes.len() > max_bytes {
        let _ = child.start_kill();
        running.finished = child.wait().await.is_ok();
        return Err(CodeReadError::TooLarge);
    }
    let status = child.wait().await.map_err(|e| io_error(&e))?;
    running.finished = true;
    if !status.success() {
        return Err(CodeReadError::Unavailable);
    }
    Ok(bytes)
}
// KillOnDrop on Tokio's direct child is insufficient for transport helpers.
struct Running {
    child: Box<dyn ChildWrapper>,
    finished: bool,
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.start_kill();
        }
    }
}
/// `path`, absolute with links resolved. On Windows, `canonicalize` gives a verbatim
/// path (`\\?\D:\…`), which Git doesn't accept as `--git-dir`; it is turned back into
/// the ordinary form when that names the same file (`dunce`), so the containment
/// checks and Git see the same path.
async fn canonical(path: &Path) -> Result<PathBuf, CodeReadError> {
    let resolved = tokio::fs::canonicalize(path)
        .await
        .map_err(|e| io_error(&e))?;
    Ok(dunce::simplified(&resolved).to_path_buf())
}

fn io_error(error: &std::io::Error) -> CodeReadError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        CodeReadError::PermissionDenied
    } else {
        CodeReadError::Unavailable
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    #[tokio::test]
    async fn cancellation_stops_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let leaked = dir.path().join("leaked");
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("(printf ready > \"$1\"; sleep 0.5; printf leaked > \"$2\") & wait")
            .arg("--")
            .arg(&ready)
            .arg(&leaked)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let result = tokio::time::timeout(Duration::from_millis(200), run_command(cmd, 1024)).await;
        assert!(result.is_err());
        assert!(ready.exists(), "descendant must have started");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!leaked.exists(), "cancelled descendant continued executing");
    }
}
