//! Script checks and out-of-process plugins as processes (ADR-0030 §4, ADR-0031 §5,
//! §6): the only code in ODS that starts a check's or a plugin's process. A
//! [`ScriptCheck`] is a [`HealthCheck`] that runs a configured command, writes one
//! [`CheckRequest`] to its stdin and reads one [`CheckResponse`] from its stdout. A
//! [`PluginProcess`] does the same for each call to a plugin, after asking it to
//! describe itself: its checks are [`PluginCheck`]s, and its source versions a
//! [`PluginVersions`].
//!
//! - **No shell:** the command is started directly, its first item as the program, so
//!   nothing in it is expanded or interpreted.
//! - **Where:** in the project's root, with the user's environment, as `dbt` would be.
//!   ODS adds no credential and no resolved secret (AGENTS rule 9).
//! - **Failing safe:** a command that can't start, exits non-zero, takes longer than its
//!   timeout (it is then killed, with every process it started), or answers with
//!   anything but the protocol's JSON in a major this host speaks is an error: the host makes every node it was asked about
//!   *unknown*, never a pass (AGENTS rule 3). So is an answer about a node it wasn't
//!   asked about, which the host checks.
//! - **Its log:** what it writes to stderr is logged, and its last line, cut to
//!   [`QUOTED`] characters, is in the error when it fails. It is the script's own output,
//!   so a script that prints a credential from its environment puts it there.
//! - **Bounded:** an answer longer than [`ANSWER_LIMIT`] is an error, and only the last
//!   [`STDERR_KEPT`] bytes of stderr are kept, so a runaway script can't exhaust memory.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::health_check::{
    CheckFinding, CheckInfo, CheckScope, HealthCheck, Severity,
};
use ods_sdk::protocol::{CheckCall, CheckRequest, CheckResponse, PROTOCOL};
use ods_sdk::{Provider, ProviderError, ProviderInfo};
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};

mod plugin;

pub use plugin::{Described, PluginCheck, PluginProcess, PluginVersions};

/// The provider kind.
pub const KIND: &str = "process";

/// The most a script may write to stdout: an answer is a few hundred bytes a node.
pub const ANSWER_LIMIT: usize = 64 * 1024 * 1024;

/// How much of the end of a script's stderr is kept.
pub const STDERR_KEPT: usize = 1024 * 1024;

/// The most characters of stderr's last line an error quotes.
pub const QUOTED: usize = 300;

/// A configured check run as a process, over the script protocol.
#[derive(Debug, Clone)]
pub struct ScriptCheck {
    id: String,
    process: Process,
    params: BTreeMap<String, serde_json::Value>,
    artifacts: BTreeMap<String, String>,
}

/// A command, run once per request: started in `root`, given the request on stdin, and
/// stopped, with all it started, after `timeout`.
#[derive(Debug, Clone)]
struct Process {
    /// Who it runs for, in its log lines: a check's id, or a plugin's name.
    label: String,
    command: Vec<String>,
    root: PathBuf,
    timeout: Duration,
}

impl ScriptCheck {
    /// The check `id`, which runs `command` (its program, then its arguments) in the
    /// project's `root`, giving up after `timeout`.
    pub fn new(
        id: impl Into<String>,
        command: Vec<String>,
        root: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Self {
        let id = id.into();
        Self {
            process: Process {
                label: id.clone(),
                command,
                root: root.into(),
                timeout,
            },
            id,
            params: BTreeMap::new(),
            artifacts: BTreeMap::new(),
        }
    }

    /// With its configured parameters, passed on as given.
    #[must_use]
    pub fn with_params(mut self, params: BTreeMap<String, serde_json::Value>) -> Self {
        self.params = params;
        self
    }

    /// With the project's artifacts, by name (e.g. `manifest`), as paths.
    #[must_use]
    pub fn with_artifacts(mut self, artifacts: BTreeMap<String, String>) -> Self {
        self.artifacts = artifacts;
        self
    }

    /// The command, for people: its items joined by spaces.
    pub fn shown(&self) -> String {
        self.process.shown()
    }
}

impl Process {
    /// The command, for people: its items joined by spaces.
    fn shown(&self) -> String {
        self.command.join(" ")
    }

    /// Runs the command with `request` on its stdin: its stdout, or why it failed.
    async fn run(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        let (program, args) = self
            .command
            .split_first()
            .ok_or_else(|| "it has no command to run".to_owned())?;
        let mut command = CommandWrap::with_new(program, |command| {
            command
                .args(args)
                .current_dir(&self.root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        });
        // In a process group (a job object on Windows) of its own, so what it starts in
        // turn is stopped with it.
        #[cfg(unix)]
        command.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(JobObject);
        command.wrap(KillOnDrop);
        let mut running = Running {
            child: command
                .spawn()
                .map_err(|e| format!("`{}` couldn't be started: {e}", self.shown()))?,
            finished: false,
        };
        let child = &mut running.child;
        // Written while its output is read, so neither side waits on a full pipe. A
        // script that answers without reading its request closes the pipe: that's
        // its business, not an error.
        let mut stdin = child.stdin().take();
        let request = request.to_vec();
        let writing = tokio::spawn(async move {
            if let Some(stdin) = stdin.as_mut() {
                let _ = stdin.write_all(&request).await;
                let _ = stdin.shutdown().await;
            }
        });
        let (stdout, stderr) = (child.stdout().take(), child.stderr().take());
        let ending = async {
            let (stdout, stderr) = tokio::join!(
                read_capped(stdout, ANSWER_LIMIT, false),
                read_capped(stderr, STDERR_KEPT, true)
            );
            // Until every process in its group has ended.
            (stdout, stderr, child.wait().await)
        };
        // On a timeout or an error, `running` stops what is left as it is dropped.
        let (stdout, stderr, status) = match tokio::time::timeout(self.timeout, ending).await {
            Ok((Ok(stdout), Ok(stderr), Ok(status))) => {
                running.finished = true;
                (stdout, stderr, status)
            }
            Ok((Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e))) => {
                return Err(format!("`{}` couldn't be run: {e}", self.shown()));
            }
            Err(_) => {
                return Err(format!(
                    "`{}` didn't answer within {}s, so it was stopped",
                    self.shown(),
                    self.timeout.as_secs_f64()
                ));
            }
        };
        let _ = writing.await;
        let stderr = String::from_utf8_lossy(&stderr.0);
        for line in stderr.lines().filter(|l| !l.trim().is_empty()) {
            tracing::info!(process = %self.label, "{line}");
        }
        if !status.success() {
            let last = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| {
                    let line = l.trim();
                    let cut: String = line.chars().take(QUOTED).collect();
                    let more = if cut.len() < line.len() { "…" } else { "" };
                    format!("; its last line on stderr: {cut}{more}")
                })
                .unwrap_or_default();
            return Err(format!("`{}` exited with {status}{last}", self.shown()));
        }
        let (stdout, over) = stdout;
        if over {
            return Err(format!(
                "`{}` wrote more than {} MiB to stdout",
                self.shown(),
                ANSWER_LIMIT / (1024 * 1024)
            ));
        }
        Ok(stdout)
    }
}

impl Process {
    /// Sends `request` and reads its answer: an error unless the process answers with
    /// the protocol's JSON for `T`, in a major this host speaks (`protocol` reads it).
    async fn ask<T: serde::de::DeserializeOwned>(
        &self,
        request: &impl serde::Serialize,
        protocol: impl FnOnce(&T) -> ods_sdk::protocol::ProtocolVersion,
    ) -> Result<T, ProviderError> {
        let request = serde_json::to_vec(request)
            .map_err(|e| ProviderError::Other(format!("the request couldn't be written: {e}")))?;
        let stdout = self.run(&request).await.map_err(ProviderError::Other)?;
        // The major first, so an answer in another major says so, not that its fields
        // are wrong.
        if let Ok(Major { protocol: Some(v) }) = serde_json::from_slice::<Major>(&stdout)
            && v.major != PROTOCOL.major
        {
            return Err(self.other_major(v));
        }
        let answer: T = serde_json::from_slice(&stdout).map_err(|e| {
            ProviderError::Other(format!(
                "`{}` didn't answer with the script protocol's JSON: {e}",
                self.shown()
            ))
        })?;
        let v = protocol(&answer);
        if v.major != PROTOCOL.major {
            return Err(self.other_major(v));
        }
        Ok(answer)
    }

    fn other_major(&self, v: ods_sdk::protocol::ProtocolVersion) -> ProviderError {
        ProviderError::Other(format!(
            "`{}` answered in protocol {}.{}; this ods speaks {}.x",
            self.shown(),
            v.major,
            v.minor,
            PROTOCOL.major
        ))
    }
}

/// Only an answer's protocol version.
#[derive(serde::Deserialize)]
struct Major {
    protocol: Option<ods_sdk::protocol::ProtocolVersion>,
}

/// A started script: unless it has finished, it is stopped with every process it
/// started when dropped, which is how a timeout, an error or a host that gives up (the
/// check's future dropped) leaves nothing behind.
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

/// Reads `reader` to its end, keeping at most `cap` bytes: the first (or, with
/// `keep_end`, the last) of them, and whether there were more. It reads on past `cap`,
/// so the process never waits on a full pipe.
async fn read_capped(
    reader: Option<impl AsyncRead + Unpin>,
    cap: usize,
    keep_end: bool,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut over = false;
    let Some(mut reader) = reader else {
        return Ok((kept, over));
    };
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Ok((kept, over));
        }
        if keep_end {
            kept.extend_from_slice(&chunk[..n]);
            if kept.len() > cap {
                kept.drain(..kept.len() - cap);
                over = true;
            }
        } else {
            let room = cap.saturating_sub(kept.len());
            kept.extend_from_slice(&chunk[..n.min(room)]);
            over |= n > room;
        }
    }
}

impl Provider for ScriptCheck {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            self.id.clone(),
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::HealthCheck]),
        )
    }
}

#[async_trait]
impl HealthCheck for ScriptCheck {
    fn describe(&self) -> CheckInfo {
        // The configured severity is the host's to apply.
        CheckInfo::new(
            self.id.clone(),
            format!("runs `{}`", self.shown()),
            Severity::Warn,
        )
    }

    async fn check(&self, scope: &CheckScope) -> Result<Vec<CheckFinding>, ProviderError> {
        let request = CheckRequest::new(
            CheckCall::new(self.id.clone(), self.params.clone()),
            scope.clone(),
            self.artifacts.clone(),
        );
        let response: CheckResponse = self
            .process
            .ask(&request, |r: &CheckResponse| r.protocol)
            .await?;
        Ok(response.findings)
    }
}
