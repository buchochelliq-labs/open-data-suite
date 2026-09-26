//! Runs dbt for ODS: the [`Executor`] contract over the dbt CLI (#23, ADR-0014).
//!
//! - [`prepare`](Executor::prepare) runs `dbt compile`, so the manifest carries the
//!   compiled SQL fingerprints need, and, if asked, `dbt source freshness`.
//! - [`execute`](Executor::execute) runs `dbt build --select …` with
//!   [exact selectors](crate::selection) for the requested nodes, checked against the
//!   manifest so that no other node matches; [`ExecutionMode::Run`] leaves out data tests and unit tests
//!   (`--exclude-resource-type`, dbt 1.8+). Outcomes come from the `run_results.json`
//!   that invocation wrote; a file left by an earlier invocation is never read as this
//!   one's.
//!
//! dbt's own output goes to ODS's stderr (or is captured), never to stdout, which
//! carries ODS's report.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

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

/// A dbt command the executor is about to run, for callers that show progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DbtStep {
    /// `dbt source freshness`: how new each source's data is.
    SourceFreshness,
    /// `dbt compile`: the compiled SQL fingerprints need.
    Compile,
    /// A dbt command that builds `nodes` nodes: `build`, or `run`, `seed` or
    /// `snapshot` when they are all models, seeds or snapshots.
    Build {
        /// The dbt command, e.g. `run`.
        command: &'static str,
        /// How many nodes are selected.
        nodes: usize,
        /// Whether their tests run too.
        tests: bool,
    },
    /// `dbt test` of `nodes` nodes' tests.
    Test {
        /// How many nodes' tests are selected.
        nodes: usize,
    },
}

/// Called before each dbt command runs.
pub type StepHook = Arc<dyn Fn(DbtStep) + Send + Sync>;

/// Runs the dbt CLI.
#[derive(Clone)]
pub struct DbtExecutor {
    program: PathBuf,
    target_path: PathBuf,
    project_dir: Option<PathBuf>,
    profiles_dir: Option<PathBuf>,
    target: Option<String>,
    env: BTreeMap<String, String>,
    output: DbtOutput,
    on_step: Option<StepHook>,
}

// Environment values can be credentials: show only their names.
impl std::fmt::Debug for DbtExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbtExecutor")
            .field("program", &self.program)
            .field("target_path", &self.target_path)
            .field("project_dir", &self.project_dir)
            .field("profiles_dir", &self.profiles_dir)
            .field("target", &self.target)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("output", &self.output)
            .field("on_step", &self.on_step.is_some())
            .finish()
    }
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
            on_step: None,
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

    /// Calls `hook` before each dbt command, e.g. to say which one runs and why.
    #[must_use]
    pub fn on_step(mut self, hook: impl Fn(DbtStep) + Send + Sync + 'static) -> Self {
        self.on_step = Some(Arc::new(hook));
        self
    }

    fn step(&self, step: DbtStep) {
        if let Some(hook) = &self.on_step {
            hook(step);
        }
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
        // Names only: values can be credentials (AGENTS.md rule 9).
        tracing::info!(command = %self.display(args), "running dbt");
        tracing::debug!(env = ?self.env.keys().collect::<Vec<_>>(), output = ?self.output, "dbt settings");
        let started = std::time::Instant::now();
        // Not `Command::output()`: tokio's always pipes stdout and stderr, which would
        // swallow dbt's output in `DbtOutput::Stderr` mode.
        let output = command
            .spawn()
            .map_err(|e| {
                ProviderError::Other(format!("couldn't start `{}`: {e}", self.program.display()))
            })?
            .wait_with_output()
            .await
            .map_err(|e| {
                ProviderError::Other(format!("`{}` failed: {e}", self.program.display()))
            })?;
        tracing::info!(
            exit = ?output.status.code(),
            seconds = started.elapsed().as_secs_f64(),
            "dbt finished"
        );
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

/// dbt options that may be passed through (after `--`): they change how dbt runs or
/// logs, never which nodes run, against what, or what their results mean. Anything else
/// is refused: an option ODS doesn't know could select nodes, point at another
/// warehouse, or build something that isn't the real thing.
const PASSTHROUGH_FLAGS: [&str; 30] = [
    "--fail-fast",
    "--no-fail-fast",
    "--debug",
    "--no-debug",
    "--quiet",
    "--no-quiet",
    "--use-colors",
    "--no-use-colors",
    "--use-colors-file",
    "--no-use-colors-file",
    "--partial-parse",
    "--no-partial-parse",
    "--static-parser",
    "--no-static-parser",
    "--version-check",
    "--no-version-check",
    "--print",
    "--no-print",
    "--warn-error",
    "--no-warn-error",
    "--store-failures",
    "--no-store-failures",
    "--show-resource-report",
    "--no-show-resource-report",
    "--introspect",
    "--no-introspect",
    "--cache-selected-only",
    "--no-cache-selected-only",
    "--send-anonymous-usage-stats",
    "--no-send-anonymous-usage-stats",
];

/// Pass-through options that take a value, as `--opt value` or `--opt=value`.
const PASSTHROUGH_OPTIONS: [&str; 9] = [
    "--threads",
    "--log-level",
    "--log-level-file",
    "--log-format",
    "--log-format-file",
    "--log-path",
    "--log-file-max-bytes",
    "--printer-width",
    "--warn-error-options",
];

/// Short pass-through flags (`-x` fail fast, `-d` debug, `-q` quiet), alone or bundled.
const PASSTHROUGH_SHORT: [char; 3] = ['x', 'd', 'q'];

/// dbt settings read from the environment that, like the refused options, make a build
/// something other than the real thing (empty, sampled, one time window) or read
/// relations ODS didn't build.
const REFUSED_ENV: [&str; 8] = [
    "DBT_EMPTY",
    "DBT_SAMPLE",
    "DBT_EVENT_TIME_START",
    "DBT_EVENT_TIME_END",
    "DBT_DEFER",
    "DBT_FAVOR_STATE",
    // Deprecated spellings dbt still maps to the two above.
    "DBT_DEFER_TO_STATE",
    "DBT_FAVOR_STATE_MODE",
];

/// The arguments in `args` that may not be passed through.
fn refused_args(args: &[String]) -> Vec<String> {
    let mut refused = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            let name = format!("--{name}");
            if PASSTHROUGH_FLAGS.contains(&name.as_str()) && value.is_none() {
                continue;
            }
            if PASSTHROUGH_OPTIONS.contains(&name.as_str())
                && (value.is_some() || args.next().is_some())
            {
                continue;
            }
            // Its value, if it takes one, isn't worth naming too.
            if value.is_none() {
                let mut peek = args.clone();
                if peek.next().is_some_and(|v| !v.starts_with('-')) {
                    args = peek;
                }
            }
            refused.push(name);
        } else if !arg
            .strip_prefix('-')
            .is_some_and(|c| !c.is_empty() && c.chars().all(|c| PASSTHROUGH_SHORT.contains(&c)))
        {
            refused.push(arg.clone());
        }
    }
    refused
}

fn refuse_engine_args(args: &[String]) -> Result<(), ProviderError> {
    let refused = refused_args(args);
    if refused.is_empty() {
        return Ok(());
    }
    Err(ProviderError::Other(format!(
        "can't pass {} to dbt: only options that don't change which nodes run, where, or what their results mean are passed through ({}, {}, and -x, -d, -q). Use the matching ODS option instead where there is one (e.g. --select, --exclude, --resource-type, --full-refresh, --target, --project-dir)",
        refused.join(", "),
        PASSTHROUGH_OPTIONS.join(", "),
        PASSTHROUGH_FLAGS
            .iter()
            .filter(|f| !f.starts_with("--no-"))
            .copied()
            .collect::<Vec<_>>()
            .join(", "),
    )))
}

impl DbtExecutor {
    /// Refuses to run when dbt would read a setting that makes builds not the real
    /// thing from the environment.
    fn refuse_env(&self) -> Result<(), ProviderError> {
        let set: Vec<&str> = REFUSED_ENV
            .into_iter()
            .filter(|key| {
                let value = self
                    .env
                    .get(*key)
                    .cloned()
                    .or_else(|| std::env::var(key).ok());
                value.is_some_and(|v| {
                    !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false")
                })
            })
            .collect();
        if set.is_empty() {
            Ok(())
        } else {
            Err(ProviderError::Other(format!(
                "{} set in the environment: dbt would build something ODS can't record as a real build; unset it",
                set.join(", ")
            )))
        }
    }
}

/// The dbt command for `mode` and the requested nodes: `test` for a test run; without
/// tests, `run`, `seed` or `snapshot` when every node is of that one type, so dbt's
/// own output reads as a user expects; otherwise `build`.
fn dbt_command(mode: ExecutionMode, manifest: &crate::Manifest, ids: &[String]) -> &'static str {
    match mode {
        ExecutionMode::Test => return "test",
        ExecutionMode::Build => return "build",
        _ => {}
    }
    let kinds: BTreeSet<crate::ResourceType> = ids
        .iter()
        .filter_map(|id| manifest.nodes.iter().find(|n| &n.unique_id == id))
        .map(|n| n.resource_type)
        .collect();
    match kinds.iter().collect::<Vec<_>>().as_slice() {
        [crate::ResourceType::Model] => "run",
        [crate::ResourceType::Seed] => "seed",
        [crate::ResourceType::Snapshot] => "snapshot",
        _ => "build",
    }
}

/// Hooks and other operations dbt reports alongside the nodes: neither nodes nor checks.
fn is_operation(id: &str) -> bool {
    id.starts_with("operation.")
}

/// Every check the manifest this run wrote knows (data tests and unit tests), with
/// the nodes it reads. `None` if that manifest can't be read or is from another
/// invocation, so the caller assumes the worst.
fn check_coverage(manifest: &Path, run: &RunResults) -> Option<BTreeMap<String, Vec<String>>> {
    let Ok(manifest) = crate::Manifest::read(manifest) else {
        tracing::info!("can't read the run's manifest: no test pass counts");
        return None;
    };
    if manifest.invocation_id.is_none() || manifest.invocation_id != run.invocation_id {
        tracing::info!(
            manifest = ?manifest.invocation_id,
            run = ?run.invocation_id,
            "the manifest isn't from this run: no test pass counts"
        );
        return None;
    }
    Some(
        manifest
            .nodes
            .iter()
            .filter(|n| n.resource_type == crate::ResourceType::Test)
            .map(|n| (n.unique_id.clone(), n.depends_on.clone()))
            .chain(
                manifest
                    .unit_tests
                    .iter()
                    .map(|t| (t.unique_id.clone(), t.depends_on.clone())),
            )
            .collect(),
    )
}

/// The checks in `run` (not requested nodes) whose status matches.
fn checks_where(
    run: &RunResults,
    requested: &BTreeSet<&str>,
    status: impl Fn(RunStatus) -> bool,
) -> Vec<String> {
    run.results
        .iter()
        .filter(|r| {
            is_check(&r.unique_id) && !requested.contains(r.unique_id.as_str()) && status(r.status)
        })
        .map(|r| r.unique_id.clone())
        .collect()
}

/// The checks in one run's results, and which nodes each covers.
struct Checks<'r> {
    mode: ExecutionMode,
    failed: Vec<String>,
    skipped: Vec<String>,
    passed: Vec<String>,
    /// Every result id in the run.
    ran: BTreeSet<&'r str>,
    coverage: Option<BTreeMap<String, Vec<String>>>,
}

impl<'r> Checks<'r> {
    fn new(request: &ExecutionRequest, run: &'r RunResults, manifest: &Path) -> Self {
        let requested: BTreeSet<&str> = request.nodes.iter().map(|n| n.id.as_str()).collect();
        Self {
            mode: request.mode,
            failed: checks_where(run, &requested, |s| {
                s != RunStatus::Success && s != RunStatus::Skipped
            }),
            skipped: checks_where(run, &requested, |s| s == RunStatus::Skipped),
            passed: checks_where(run, &requested, |s| s == RunStatus::Success),
            ran: run.results.iter().map(|r| r.unique_id.as_str()).collect(),
            coverage: check_coverage(manifest, run),
        }
    }

    /// A failed or skipped check the manifest doesn't place could cover anything.
    fn may_cover(&self, check: &str, node: &str) -> bool {
        self.coverage.as_ref().is_none_or(|c| {
            c.get(check)
                .is_none_or(|deps| deps.iter().any(|d| d == node))
        })
    }

    /// A pass only counts for the nodes the manifest says it checks.
    fn covers(&self, check: &str, node: &str) -> bool {
        self.coverage.as_ref().is_some_and(|c| {
            c.get(check)
                .is_some_and(|deps| deps.iter().any(|d| d == node))
        })
    }

    fn failed_on(&self, node: &str) -> Vec<String> {
        self.failed
            .iter()
            .filter(|c| self.may_cover(c, node))
            .cloned()
            .collect()
    }

    fn passed_on(&self, node: &str) -> Vec<String> {
        self.passed
            .iter()
            .filter(|c| self.covers(c, node))
            .cloned()
            .collect()
    }

    /// Checks on the node that didn't run count as skipped: stopped early, deselected
    /// (e.g. by indirect selection) or missing from the results, they tested nothing.
    fn skipped_on(&self, node: &str) -> Vec<String> {
        let mut skipped: BTreeSet<String> = self
            .skipped
            .iter()
            .filter(|c| self.may_cover(c, node))
            .cloned()
            .collect();
        if self.mode != ExecutionMode::Run
            && let Some(coverage) = &self.coverage
        {
            skipped.extend(
                coverage
                    .iter()
                    .filter(|(check, deps)| {
                        deps.iter().any(|d| d == node) && !self.ran.contains(check.as_str())
                    })
                    .map(|(check, _)| check.clone()),
            );
        }
        skipped.into_iter().collect()
    }
}

/// In a test run nothing is built: each node's outcome is its checks'.
fn test_outcomes(
    request: &ExecutionRequest,
    run: &RunResults,
    checks: &Checks<'_>,
) -> Vec<NodeExecution> {
    let finished = run
        .generated_at
        .as_deref()
        .and_then(|t| Timestamp::parse(t).ok());
    request
        .nodes
        .iter()
        .map(|n| {
            let failed = checks.failed_on(&n.id);
            let skipped = checks.skipped_on(&n.id);
            let passed = checks.passed_on(&n.id);
            // Only checks that ran and passed vouch for a build.
            let (status, message) = if !failed.is_empty() {
                (ExecutionStatus::Failed, None)
            } else if !skipped.is_empty() {
                (ExecutionStatus::Skipped, None)
            } else if passed.is_empty() {
                (
                    ExecutionStatus::Skipped,
                    Some("no checks ran on it".to_owned()),
                )
            } else {
                (ExecutionStatus::Success, None)
            };
            NodeExecution::new(n.id.clone(), status, finished, message)
                .with_checks_failed(failed)
                .with_checks_skipped(skipped)
                .with_checks_passed(passed)
        })
        .collect()
}

/// Each requested node's outcome, the failed checks, and the nodes built unrequested.
fn outcomes(
    request: &ExecutionRequest,
    run: &RunResults,
    manifest: &Path,
) -> (Vec<NodeExecution>, Vec<String>, Vec<String>) {
    let checks = Checks::new(request, run, manifest);
    let (nodes, failed, unrequested) = node_outcomes(request, run, checks);
    for n in &nodes {
        tracing::debug!(
            node = %n.node,
            status = ?n.status,
            checks_passed = ?n.checks_passed,
            checks_failed = ?n.checks_failed,
            checks_skipped = ?n.checks_skipped,
            "dbt result"
        );
    }
    (nodes, failed, unrequested)
}

fn node_outcomes(
    request: &ExecutionRequest,
    run: &RunResults,
    checks: Checks<'_>,
) -> (Vec<NodeExecution>, Vec<String>, Vec<String>) {
    if request.mode == ExecutionMode::Test {
        return (
            test_outcomes(request, run, &checks),
            checks.failed,
            Vec::new(),
        );
    }
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
            )
            .with_checks_failed(checks.failed_on(&n.id))
            .with_checks_skipped(checks.skipped_on(&n.id))
            .with_checks_passed(checks.passed_on(&n.id)),
            None => NodeExecution::new(
                n.id.clone(),
                ExecutionStatus::Skipped,
                None,
                Some("dbt didn't run it".to_owned()),
            ),
        })
        .collect();
    let unrequested = run
        .results
        .iter()
        .filter(|r| {
            !is_check(&r.unique_id)
                && !is_operation(&r.unique_id)
                && !requested.contains(r.unique_id.as_str())
                && r.status != RunStatus::Skipped
        })
        .map(|r| r.unique_id.clone())
        .collect();
    (nodes, checks.failed, unrequested)
}

impl Provider for DbtExecutor {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(KIND, "dbt", env!("CARGO_PKG_VERSION"), CapabilitySet::new())
    }
}

#[async_trait]
impl Executor for DbtExecutor {
    async fn prepare(&self, request: &PrepareRequest) -> Result<PrepareReport, ProviderError> {
        // Up front, so the reason isn't lost in whatever dbt makes of it.
        self.refuse_env()?;
        // Freshness first: every dbt command rewrites `manifest.json`, and only
        // `compile`'s carries the compiled SQL fingerprints need.
        let mut warnings = Vec::new();
        let mut measured = false;
        if request.measure_sources {
            let sources = self.artifact("sources.json");
            let before = sources_invocation_of(&sources);
            let mut args = vec!["source".to_owned(), "freshness".to_owned()];
            args.extend(self.common_args());
            self.step(DbtStep::SourceFreshness);
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
        self.step(DbtStep::Compile);
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
        // Exact selectors, from the manifest the plan was made from.
        let manifest = crate::Manifest::read(&self.artifact("manifest.json")).map_err(|e| {
            ProviderError::Other(format!(
                "can't read the manifest to select nodes exactly: {e}"
            ))
        })?;
        let ids: Vec<String> = request.nodes.iter().map(|n| n.id.clone()).collect();
        let selectors = crate::selection::exact_selectors(&manifest, &ids).map_err(|why| {
            ProviderError::Other(format!("can't select exactly the planned nodes: {why}"))
        })?;
        refuse_engine_args(&request.engine_args)?;
        self.refuse_env()?;
        let dbt_command = dbt_command(request.mode, &manifest, &ids);
        let mut args = vec![dbt_command.to_owned(), "--select".to_owned()];
        args.extend(selectors);
        // `dbt snapshot` has no --full-refresh: a snapshot's history is the point.
        if request.full_refresh && !matches!(dbt_command, "test" | "snapshot") {
            args.push("--full-refresh".to_owned());
        }
        // Only `build` would run tests alongside; the others never do.
        if request.mode == ExecutionMode::Run && dbt_command == "build" {
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
        args.extend(request.engine_args.iter().cloned());
        let command = self.display(&args);
        let results_path = self.artifact("run_results.json");
        let before = invocation_of(&results_path);
        self.step(if request.mode == ExecutionMode::Test {
            DbtStep::Test {
                nodes: request.nodes.len(),
            }
        } else {
            DbtStep::Build {
                command: dbt_command,
                nodes: request.nodes.len(),
                tests: request.mode == ExecutionMode::Build,
            }
        });
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
        let (nodes, checks_failed, unrequested) =
            outcomes(request, &run, &self.artifact("manifest.json"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(args: &[&str]) -> bool {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        refuse_engine_args(&args).is_err()
    }

    #[test]
    fn only_known_harmless_engine_args_are_passed_through() {
        for args in [
            &["--select", "x"][..],
            &["--select=x"],
            &["-s", "x"],
            &["-sx"],
            &["-m", "x"],
            &["--model", "x"],
            &["--target", "prod"],
            &["-tprod"],
            &["-f"],
            &["-xf"],
            &["--vars", "{a: 1}"],
            &["--no-write-json"],
            &["--sample=3 days"],
            &["--event-time-start", "2024-01-01"],
            &["--indirect-selection=empty"],
            &["--project-dir", "elsewhere"],
            &["--profile", "other"],
            &["--defer-state", "prod"],
            &["--fail-fast=x"],
            &["--threads"],
            &["orders"],
            &["-"],
        ] {
            assert!(refused(args), "{args:?}");
        }
        for args in [
            &["--threads", "4"][..],
            &["--threads=4"],
            &["-x"],
            &["-xq"],
            &["--fail-fast"],
            &["--debug"],
            &["--store-failures"],
            &["--log-level", "debug", "--no-use-colors"],
        ] {
            assert!(!refused(args), "{args:?}");
        }
        let named =
            |args: &[&str]| refused_args(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>());
        assert_eq!(named(&["--select", "x", "--threads", "2"]), ["--select"]);
        assert_eq!(named(&["-s", "x"]), ["-s", "x"]);
    }
}
