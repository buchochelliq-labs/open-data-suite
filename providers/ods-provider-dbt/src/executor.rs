//! Runs dbt for ODS: the [`Executor`] contract over the dbt CLI (#23, ADR-0014).
//!
//! - [`prepare`](Executor::prepare) runs `dbt compile`, so the manifest carries the
//!   compiled SQL fingerprints need, and, if asked, `dbt source freshness`.
//! - [`execute`](Executor::execute) runs `dbt build --select <name>…` with exactly the
//!   requested nodes; [`ExecutionMode::Run`] leaves out data tests and unit tests
//!   (`--exclude-resource-type`, dbt 1.8+). Outcomes come from the `run_results.json`
//!   that invocation wrote; a file left by an earlier invocation is never read as this
//!   one's.
//!
//! dbt's own output goes to ODS's stderr (or is captured), never to stdout, which
//! carries ODS's report.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_core::state::Timestamp;
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, Executor, NodeExecution,
    PrepareReport, PrepareRequest,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::runs::{RunResults, RunStatus, SourceFreshness};

/// The provider kind, as written in configuration.
pub const KIND: &str = "dbt";

/// Lines of captured dbt output kept in error messages.
const OUTPUT_TAIL: usize = 20;

/// Where dbt's own output goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum DbtOutput {
    /// To ODS's stderr, as it runs.
    #[default]
    Stderr,
    /// Captured; the last lines appear in error messages.
    Capture,
}

/// Runs the dbt CLI.
#[derive(Debug, Clone)]
pub struct DbtExecutor {
    program: PathBuf,
    target_path: PathBuf,
    project_dir: Option<PathBuf>,
    profiles_dir: Option<PathBuf>,
    target: Option<String>,
    env: BTreeMap<String, String>,
    output: DbtOutput,
}

impl DbtExecutor {
    /// Runs `program` (e.g. `dbt`), writing artifacts to `target_path`, which ODS reads.
    pub fn new(program: impl Into<PathBuf>, target_path: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            target_path: target_path.into(),
            project_dir: None,
            profiles_dir: None,
            target: None,
            env: BTreeMap::new(),
            output: DbtOutput::default(),
        }
    }

    /// dbt's `--project-dir`.
    #[must_use]
    pub fn project_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.project_dir = Some(dir.into());
        self
    }

    /// dbt's `--profiles-dir`.
    #[must_use]
    pub fn profiles_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.profiles_dir = Some(dir.into());
        self
    }

    /// dbt's `--target` (the profile output, not the target directory).
    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Sets an environment variable for dbt only.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Where dbt's output goes.
    #[must_use]
    pub fn output(mut self, output: DbtOutput) -> Self {
        self.output = output;
        self
    }

    /// The target path as dbt must see it: absolute, since dbt resolves a relative one
    /// against the project directory rather than where ODS runs.
    fn target_path(&self) -> PathBuf {
        std::path::absolute(&self.target_path).unwrap_or_else(|_| self.target_path.clone())
    }

    /// The arguments every invocation shares.
    fn common_args(&self) -> Vec<String> {
        let mut args = vec![
            "--target-path".to_owned(),
            self.target_path().display().to_string(),
        ];
        if let Some(dir) = &self.project_dir {
            args.extend(["--project-dir".to_owned(), dir.display().to_string()]);
        }
        if let Some(dir) = &self.profiles_dir {
            args.extend(["--profiles-dir".to_owned(), dir.display().to_string()]);
        }
        if let Some(target) = &self.target {
            args.extend(["--target".to_owned(), target.clone()]);
        }
        args
    }

    /// The command line, for people.
    fn display(&self, args: &[String]) -> String {
        std::iter::once(self.program.display().to_string())
            .chain(args.iter().map(|a| {
                if a.is_empty() || a.contains(char::is_whitespace) {
                    format!("'{a}'")
                } else {
                    a.clone()
                }
            }))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Runs dbt with `args`; returns whether it exited successfully and, if captured,
    /// the tail of its output.
    async fn invoke(&self, args: &[String]) -> Result<(bool, String), ProviderError> {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(args)
            .envs(&self.env)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        match self.output {
            DbtOutput::Stderr => {
                command
                    .stdout(Stdio::from(std::io::stderr()))
                    .stderr(Stdio::from(std::io::stderr()));
            }
            DbtOutput::Capture => {
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
            }
        }
        let output = command.output().await.map_err(|e| {
            ProviderError::Other(format!("couldn't start `{}`: {e}", self.program.display()))
        })?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(OUTPUT_TAIL)..].join("\n");
        Ok((output.status.success(), tail))
    }

    fn failure(what: &str, tail: &str) -> ProviderError {
        if tail.trim().is_empty() {
            ProviderError::Other(format!("{what}; see dbt's output above"))
        } else {
            ProviderError::Other(format!("{what}:\n{tail}"))
        }
    }

    fn artifact(&self, name: &str) -> PathBuf {
        self.target_path().join(name)
    }
}

fn invocation_of(path: &Path) -> Option<String> {
    RunResults::read(path).ok().and_then(|r| r.invocation_id)
}

fn sources_invocation_of(path: &Path) -> Option<String> {
    SourceFreshness::read(path)
        .ok()
        .and_then(|r| r.invocation_id)
}

fn is_check(id: &str) -> bool {
    id.starts_with("test.") || id.starts_with("unit_test.")
}

impl Provider for DbtExecutor {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(KIND, "dbt", env!("CARGO_PKG_VERSION"), CapabilitySet::new())
    }
}

#[async_trait]
impl Executor for DbtExecutor {
    async fn prepare(&self, request: &PrepareRequest) -> Result<PrepareReport, ProviderError> {
        // Freshness first: every dbt command rewrites `manifest.json`, and only
        // `compile`'s carries the compiled SQL fingerprints need.
        let mut warnings = Vec::new();
        let mut measured = false;
        if request.measure_sources {
            let sources = self.artifact("sources.json");
            let before = sources_invocation_of(&sources);
            let mut args = vec!["source".to_owned(), "freshness".to_owned()];
            args.extend(self.common_args());
            let (ok, _) = self.invoke(&args).await?;
            let after = sources_invocation_of(&sources);
            // A file from an earlier invocation says nothing about the data now.
            measured = after.is_some() && after != before;
            if !measured {
                warnings.push(
                    "`dbt source freshness` wrote no results: nodes reading sources are built"
                        .to_owned(),
                );
            } else if !ok {
                warnings.push(
                    "`dbt source freshness` reported stale or unmeasurable sources".to_owned(),
                );
            }
        }
        let mut args = vec!["compile".to_owned()];
        args.extend(self.common_args());
        let (ok, tail) = self.invoke(&args).await?;
        if !ok {
            return Err(Self::failure(
                &format!("`{}` failed", self.display(&args)),
                &tail,
            ));
        }
        Ok(PrepareReport::new(measured, warnings))
    }

    async fn execute(&self, request: &ExecutionRequest) -> Result<ExecutionReport, ProviderError> {
        // dbt reads an empty selection as "everything".
        if request.nodes.is_empty() {
            return Err(ProviderError::Other(
                "nothing to execute: the request names no nodes".to_owned(),
            ));
        }
        let mut args = vec!["build".to_owned(), "--select".to_owned()];
        args.extend(request.nodes.iter().map(|n| n.name.clone()));
        if request.mode == ExecutionMode::Run {
            args.extend(
                [
                    "--exclude-resource-type",
                    "test",
                    "--exclude-resource-type",
                    "unit_test",
                ]
                .map(str::to_owned),
            );
        }
        args.extend(self.common_args());
        let command = self.display(&args);
        let results_path = self.artifact("run_results.json");
        let before = invocation_of(&results_path);
        let (ok, tail) = self.invoke(&args).await?;
        let run = match RunResults::read(&results_path) {
            Ok(run) if run.invocation_id.is_some() && run.invocation_id != before => run,
            _ => {
                return Err(Self::failure(
                    &format!("`{command}` wrote no run results"),
                    &tail,
                ));
            }
        };
        let by_id: BTreeMap<&str, &crate::runs::NodeResult> = run
            .results
            .iter()
            .map(|r| (r.unique_id.as_str(), r))
            .collect();
        let requested: BTreeSet<&str> = request.nodes.iter().map(|n| n.id.as_str()).collect();
        let nodes = request
            .nodes
            .iter()
            .map(|n| match by_id.get(n.id.as_str()) {
                Some(r) => NodeExecution::new(
                    n.id.clone(),
                    match r.status {
                        RunStatus::Success => ExecutionStatus::Success,
                        RunStatus::Skipped => ExecutionStatus::Skipped,
                        _ => ExecutionStatus::Failed,
                    },
                    r.completed_at
                        .as_deref()
                        .and_then(|t| Timestamp::parse(t).ok()),
                    Some(r.raw_status.clone()),
                ),
                None => NodeExecution::new(
                    n.id.clone(),
                    ExecutionStatus::Skipped,
                    None,
                    Some("dbt didn't run it".to_owned()),
                ),
            })
            .collect();
        let checks_failed = run
            .results
            .iter()
            .filter(|r| {
                is_check(&r.unique_id)
                    && !requested.contains(r.unique_id.as_str())
                    && r.status != RunStatus::Success
                    && r.status != RunStatus::Skipped
            })
            .map(|r| r.unique_id.clone())
            .collect();
        let unrequested = run
            .results
            .iter()
            .filter(|r| {
                !is_check(&r.unique_id)
                    && !requested.contains(r.unique_id.as_str())
                    && r.status == RunStatus::Success
            })
            .map(|r| r.unique_id.clone())
            .collect();
        let finished = run
            .generated_at
            .as_deref()
            .and_then(|t| Timestamp::parse(t).ok())
            .unwrap_or_else(Timestamp::now);
        let report = ExecutionReport::new(
            run.invocation_id.clone().unwrap_or_default(),
            run.started_at
                .as_deref()
                .and_then(|t| Timestamp::parse(t).ok()),
            finished,
            nodes,
            checks_failed,
        )
        .with_unrequested(unrequested)
        .with_command(command);
        Ok(if ok { report } else { report.failed() })
    }
}
