//! Script checks as processes (ADR-0030 §4, ADR-0031 §6): the only code in ODS that
//! starts a check's process. A [`ScriptCheck`] is a [`HealthCheck`] that runs a
//! configured command, writes one [`CheckRequest`] to its stdin and reads one
//! [`CheckResponse`] from its stdout.
//!
//! - **No shell:** the command is started directly, its first item as the program, so
//!   nothing in it is expanded or interpreted.
//! - **Where:** in the project's root, with the user's environment, as `dbt` would be.
//!   ODS adds no credential and no resolved secret (AGENTS rule 9).
//! - **Failing safe:** a command that can't start, exits non-zero, takes longer than its
//!   timeout (it is then killed), or answers with anything but the protocol's JSON in a
//!   major this host speaks is an error: the host makes every node it was asked about
//!   *unknown*, never a pass (AGENTS rule 3). So is an answer about a node it wasn't
//!   asked about, which the host checks.
//! - **Its log:** what it writes to stderr is logged, and its last line is in the error
//!   when it fails.

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
use tokio::io::AsyncWriteExt as _;

/// The provider kind.
pub const KIND: &str = "process";

/// A configured check run as a process, over the script protocol.
#[derive(Debug, Clone)]
pub struct ScriptCheck {
    id: String,
    command: Vec<String>,
    params: BTreeMap<String, serde_json::Value>,
    root: PathBuf,
    artifacts: BTreeMap<String, String>,
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
        Self {
            id: id.into(),
            command,
            params: BTreeMap::new(),
            root: root.into(),
            artifacts: BTreeMap::new(),
            timeout,
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
        self.command.join(" ")
    }

    /// Runs the command with `request` on its stdin: its stdout, or why it failed.
    async fn run(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        let (program, args) = self
            .command
            .split_first()
            .ok_or_else(|| "it has no command to run".to_owned())?;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A check that times out, or a host that gives up, leaves no process behind.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("`{}` couldn't be started: {e}", self.shown()))?;
        // Written while its output is read, so neither side waits on a full pipe. A
        // script that answers without reading its request closes the pipe: that's
        // its business, not an error.
        let mut stdin = child.stdin.take();
        let request = request.to_vec();
        let writing = tokio::spawn(async move {
            if let Some(stdin) = stdin.as_mut() {
                let _ = stdin.write_all(&request).await;
                let _ = stdin.shutdown().await;
            }
        });
        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(format!("`{}` couldn't be run: {e}", self.shown())),
            Err(_) => {
                return Err(format!(
                    "`{}` didn't answer within {}s, so it was stopped",
                    self.shown(),
                    self.timeout.as_secs_f64()
                ));
            }
        };
        let _ = writing.await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stderr.lines().filter(|l| !l.trim().is_empty()) {
            tracing::info!(check = %self.id, "{line}");
        }
        if !output.status.success() {
            let last = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| format!("; its last line on stderr: {}", l.trim()))
                .unwrap_or_default();
            return Err(format!(
                "`{}` exited with {}{last}",
                self.shown(),
                output.status
            ));
        }
        Ok(output.stdout)
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
        let request = serde_json::to_vec(&request)
            .map_err(|e| ProviderError::Other(format!("the request couldn't be written: {e}")))?;
        let stdout = self.run(&request).await.map_err(ProviderError::Other)?;
        let response: CheckResponse = serde_json::from_slice(&stdout).map_err(|e| {
            ProviderError::Other(format!(
                "`{}` didn't answer with the script protocol's JSON: {e}",
                self.shown()
            ))
        })?;
        if response.protocol.major != PROTOCOL.major {
            return Err(ProviderError::Other(format!(
                "`{}` answered in protocol {}.{}; this ods speaks {}.x",
                self.shown(),
                response.protocol.major,
                response.protocol.minor,
                PROTOCOL.major
            )));
        }
        Ok(response.findings)
    }
}
