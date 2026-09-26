//! `ods state plan`, `ods state record` and `ods state history` (#11, #20, #22, #25;
//! ADR-0013). This is where dbt artifacts, the planner and the state store meet.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::state::{
    DataVersion, Exactness, ExecutionPlan, PlanAction, SnapshotId, StateSnapshot, Timestamp,
};
use ods_provider_dbt::fingerprint::{checks_digest, fingerprint};
use ods_provider_dbt::state_config::resolve;
use ods_provider_dbt::{
    ArtifactPreference, Artifacts, ResourceType, RunResults, RunStatus, SourceFreshness,
};
use ods_sdk::ProviderError;
use ods_sdk::contracts::state_store::{SnapshotSummary, StateScope, StateStore, StoredSnapshot};
use ods_state::{Node, Outcome, Project, Recorded, RunResult, Source};
use ods_store_sqlite::SqliteStateStore;
use serde::Serialize;

use crate::exit::{CliError, ExitStatus, codes};
use crate::present::{Level, Present, Span, Tone, ViewNode};

const DEFAULT_STORE: &str = ".ods/state.db";

/// Arguments every State command takes.
pub(super) fn common(command: Command) -> Command {
    command
        .arg(
            Arg::new("target-dir")
                .long("target-dir")
                .value_name("DIR")
                .env("DBT_TARGET_PATH")
                .hide_env_values(true)
                .help("dbt target directory [default: <project-dir>/target]"),
        )
        .arg(
            Arg::new("project-dir")
                .long("project-dir")
                .value_name("DIR")
                .env("DBT_PROJECT_DIR")
                .hide_env_values(true)
                .help("dbt's --project-dir: the dbt project, whose target directory ODS reads [default: .]"),
        )
        .arg(
            Arg::new("artifacts")
                .long("artifacts")
                .value_name("FORMAT")
                .value_parser(["auto", "json", "info-schema"])
                .default_value("auto")
                .help("dbt artifacts to read"),
        )
        .arg(
            Arg::new("state-db")
                .long("state-db")
                .value_name("PATH")
                .default_value(DEFAULT_STORE)
                .help("SQLite state database (created if missing)"),
        )
        .arg(
            Arg::new("environment")
                .long("environment")
                .value_name("NAME")
                .default_value("default")
                .help("Keep separate state per environment, e.g. dev and prod"),
        )
        .arg(Arg::new("sources").long("sources").value_name("PATH").help(
            "`dbt source freshness` results [default: <target-dir>/sources.json, if present]",
        ))
}

/// `ods state plan`'s own arguments.
pub(super) fn plan_command() -> Command {
    common(
        Command::new("plan")
            .about("Show what would be built and what can be reused, and why, without running anything"),
    )
    .arg(
        Arg::new("select")
            .long("select")
            .short('s')
            .value_name("SPEC")
            .action(ArgAction::Append)
            .help("Only these nodes: `name`, `+name` (with ancestors), `name+` (with descendants); repeatable"),
    )
    .arg(
        Arg::new("now")
            .long("now")
            .value_name("TIMESTAMP")
            .hide(true)
            .help("Plan as of this time (RFC 3339), for reproducible output"),
    )
}

/// `ods state record`'s own arguments.
pub(super) fn record_command() -> Command {
    common(
        Command::new("record")
            .about("Record a finished dbt run as the new state: only nodes that succeeded advance"),
    )
    .arg(
        Arg::new("run-results")
            .long("run-results")
            .value_name("PATH")
            .help("dbt's run results [default: <target-dir>/run_results.json]"),
    )
}

/// `ods state history`'s own arguments.
pub(super) fn history_command() -> Command {
    common(Command::new("history").about("List recorded state snapshots, newest first")).arg(
        Arg::new("limit")
            .long("limit")
            .value_name("N")
            .value_parser(clap::value_parser!(usize))
            .default_value("20")
            .help("How many to show"),
    )
}

pub(super) fn store_error(error: &ProviderError) -> CliError {
    let code = if matches!(error, ProviderError::Conflict(_)) {
        codes::STATE_CONFLICT
    } else {
        codes::STATE_STORE
    };
    CliError::new(ExitStatus::Failure, code, error.to_string())
}

/// Runs the store's async API from these synchronous commands.
pub(super) fn block_on<T>(future: impl std::future::Future<Output = T>) -> Result<T, CliError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::new(ExitStatus::Failure, codes::INTERNAL, e.to_string()))?;
    Ok(runtime.block_on(future))
}

/// Whether to read `dbt source freshness` results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sources {
    /// `--sources`, or `<target-dir>/sources.json` if present.
    AsGiven,
    /// None: a measurement was attempted and failed, so any file left is out of date.
    Ignore,
}

/// What every State command loads: the project as it is now, and where its state is.
pub(super) struct Workspace {
    pub(super) target_dir: PathBuf,
    pub(super) project: Project,
    pub(super) scope: StateScope,
    pub(super) state_db: PathBuf,
    pub(super) sources_file: Option<PathBuf>,
    pub(super) sources_taken_at: Option<Timestamp>,
    /// The dbt invocation that wrote the manifest.
    pub(super) invocation_id: Option<String>,
    /// Sources `dbt source freshness` couldn't measure.
    pub(super) source_errors: Vec<String>,
}

/// Where dbt writes the artifacts ODS reads, as dbt resolves it: `--target-dir`, or
/// `DBT_TARGET_PATH` relative to the project (#227), or the project's `target`.
pub(super) fn target_dir(args: &ArgMatches) -> PathBuf {
    let project = args
        .try_get_one::<String>("project-dir")
        .ok()
        .flatten()
        .map(PathBuf::from);
    match args.get_one::<String>("target-dir") {
        // dbt reads a relative target path against the project, not where it runs.
        Some(dir)
            if args.value_source("target-dir") == Some(ValueSource::EnvVariable)
                && Path::new(dir).is_relative() =>
        {
            project.map_or_else(|| PathBuf::from(dir), |p| p.join(dir))
        }
        Some(dir) => PathBuf::from(dir),
        None => project.map_or_else(|| PathBuf::from("target"), |p| p.join("target")),
    }
}

impl Workspace {
    pub(super) fn load(args: &ArgMatches, sources: Sources) -> Result<Self, CliError> {
        let target_dir = target_dir(args);
        let preference = match args.get_one::<String>("artifacts").map(String::as_str) {
            Some("json") => ArtifactPreference::Json,
            Some("info-schema") => ArtifactPreference::InfoSchema,
            _ => ArtifactPreference::Auto,
        };
        let artifacts = Artifacts::load_with(&target_dir, preference).map_err(|e| {
            CliError::new(ExitStatus::Failure, codes::LINEAGE_ARTIFACTS, e.to_string())
                .with_hint("run `dbt compile` (or `run`/`build`) first")
        })?;
        let sources_file = match (sources, args.get_one::<String>("sources")) {
            (Sources::Ignore, _) => None,
            (Sources::AsGiven, Some(path)) => Some(PathBuf::from(path)),
            (Sources::AsGiven, None) => {
                Some(target_dir.join("sources.json")).filter(|p| p.is_file())
            }
        };
        let freshness = sources_file
            .as_deref()
            .map(SourceFreshness::read)
            .transpose()
            .map_err(|e| CliError::new(ExitStatus::Failure, codes::STATE_INPUT, e.to_string()))?;
        let manifest = &artifacts.manifest;
        let sources_taken_at = freshness
            .as_ref()
            .and_then(|f| f.generated_at.as_deref())
            .and_then(|t| Timestamp::parse(t).ok());
        // Without a name, unrelated projects would share state: refuse.
        let project_name = manifest.project_name.clone().ok_or_else(|| {
            CliError::new(
                ExitStatus::Failure,
                codes::STATE_INPUT,
                "the dbt artifacts don't name the project, so its state can't be told apart from other projects'",
            )
            .with_hint("use artifacts from dbt 1.7 or later (their metadata has `project_name`)")
        })?;
        let environment = args
            .get_one::<String>("environment")
            .map_or("default", String::as_str);
        let scope = StateScope::new(&project_name, environment)
            .map_err(|e| CliError::new(ExitStatus::Usage, codes::STATE_INPUT, e))?;
        let policies = resolve(manifest);
        let nodes = plan_nodes(manifest, &policies);
        let sources = manifest
            .nodes
            .iter()
            .filter(|n| n.resource_type == ResourceType::Source)
            .map(|n| {
                let version = freshness
                    .as_ref()
                    .and_then(|f| f.max_loaded_at.get(&n.unique_id))
                    .map(|at| {
                        // Normalised, so the same instant written differently compares equal.
                        let value =
                            Timestamp::parse(at).map_or_else(|_| at.clone(), |t| t.to_string());
                        DataVersion::new(value, Exactness::Semantic, "sources.json max_loaded_at")
                    });
                Source::new(n.unique_id.clone(), display_name(&n.unique_id), version)
                    .observed_at(sources_taken_at)
            })
            .collect();
        Ok(Self {
            state_db: PathBuf::from(
                args.get_one::<String>("state-db")
                    .map_or(DEFAULT_STORE, String::as_str),
            ),
            sources_taken_at,
            invocation_id: manifest.invocation_id.clone(),
            source_errors: freshness
                .map(|f| f.errors.into_keys().collect())
                .unwrap_or_default(),
            sources_file,
            target_dir,
            project: Project::new(nodes, sources),
            scope,
        })
    }

    pub(super) fn open_store(&self) -> Result<SqliteStateStore, CliError> {
        block_on(SqliteStateStore::open(&self.state_db))?.map_err(|e| {
            store_error(&e).with_hint(format!("check `--state-db {}`", self.state_db.display()))
        })
    }

    pub(super) fn latest(
        &self,
        store: &SqliteStateStore,
    ) -> Result<Option<StoredSnapshot>, CliError> {
        block_on(store.latest(&self.scope))?.map_err(|e| store_error(&e))
    }
}

/// The models, seeds and snapshots ODS plans, as planner nodes.
fn plan_nodes(
    manifest: &ods_provider_dbt::Manifest,
    policies: &ods_provider_dbt::state_config::StatePolicies,
) -> Vec<Node> {
    // Ephemeral models are never built: their SQL is inlined into their readers'
    // compiled SQL (so it is in their fingerprints), and their readers depend on
    // their parents instead.
    let ephemeral: BTreeMap<&str, &[String]> = manifest
        .nodes
        .iter()
        .filter(|n| {
            n.resource_type == ResourceType::Model && n.materialized.as_deref() == Some("ephemeral")
        })
        .map(|n| (n.unique_id.as_str(), n.depends_on.as_slice()))
        .collect();
    manifest
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.resource_type,
                ResourceType::Model | ResourceType::Seed | ResourceType::Snapshot
            ) && !ephemeral.contains_key(n.unique_id.as_str())
        })
        .map(|n| {
            let node = Node::new(
                n.unique_id.clone(),
                node_name(n),
                kind_word(n.resource_type),
                through_ephemeral(&n.depends_on, &ephemeral),
                fingerprint(manifest, n),
                policies
                    .nodes
                    .get(&n.unique_id)
                    .cloned()
                    .unwrap_or_else(ods_core::FreshnessPolicy::conservative),
            )
            .with_checks(checks_digest(manifest, &n.unique_id));
            // dbt's --full-refresh rebuilds incremental models from scratch and reloads
            // seeds, unless the node opts out with `full_refresh: false`. Snapshots are
            // never full-refreshed: their history is the point.
            let refreshable = (n.resource_type == ResourceType::Seed
                || n.materialized.as_deref() == Some("incremental"))
                && n.config.raw.get("full_refresh") != Some(&serde_json::Value::Bool(false));
            let node = if refreshable {
                node.full_refresh_rebuilds()
            } else {
                node
            };
            // A seed's rows are its file: nothing else feeds it.
            if n.resource_type == ResourceType::Seed {
                node.self_contained()
            } else {
                node
            }
        })
        .collect()
}

/// Parents, with ephemeral models replaced by their own parents.
fn through_ephemeral(parents: &[String], ephemeral: &BTreeMap<&str, &[String]>) -> Vec<String> {
    let mut out = BTreeSet::new();
    let mut stack: Vec<&str> = parents.iter().map(String::as_str).collect();
    let mut seen = BTreeSet::new();
    while let Some(p) = stack.pop() {
        if !seen.insert(p) {
            continue;
        }
        match ephemeral.get(p) {
            Some(grandparents) => stack.extend(grandparents.iter().map(String::as_str)),
            None => {
                out.insert(p.to_owned());
            }
        }
    }
    out.into_iter().collect()
}

/// A node's name as dbt selects it: `orders`, or `orders.v2` for a model version.
fn node_name(n: &ods_provider_dbt::ManifestNode) -> String {
    let name = n.name.clone().unwrap_or_else(|| display_name(&n.unique_id));
    match &n.version {
        Some(v) => format!("{name}.v{v}"),
        None => name,
    }
}

/// `model.shop.orders` → `orders`; `source.shop.raw.orders` → `raw.orders`.
pub(super) fn display_name(id: &str) -> String {
    let mut parts = id.splitn(3, '.');
    let kind = parts.next().unwrap_or_default();
    let _package = parts.next();
    let rest = parts.next().unwrap_or(id);
    if kind == "source" {
        rest.to_owned()
    } else {
        rest.rsplit('.').next().unwrap_or(rest).to_owned()
    }
}

fn kind_word(t: ResourceType) -> &'static str {
    match t {
        ResourceType::Model => "model",
        ResourceType::Seed => "seed",
        ResourceType::Snapshot => "snapshot",
        ResourceType::Source => "source",
        _ => "other",
    }
}

// ---------------------------------------------------------------------------- plan

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct PlanReport {
    target_dir: PathBuf,
    state_db: PathBuf,
    scope: String,
    based_on: Option<SnapshotId>,
    sources_file: Option<PathBuf>,
    build: usize,
    reuse: usize,
    /// The dbt command that builds exactly the BUILD set.
    dbt_command: Option<String>,
    plan: ExecutionPlan,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

/// A plan against the latest state, with the warnings that qualify it.
pub(super) fn plan_against(
    ws: &Workspace,
    latest: Option<&StoredSnapshot>,
    specs: &[String],
    now: Timestamp,
    options: ods_state::PlanOptions,
) -> Result<(ExecutionPlan, Vec<String>), CliError> {
    let selected = ods_state::select(&ws.project, specs)
        .map_err(|e| CliError::new(ExitStatus::Usage, codes::LINEAGE_TARGET, e))?;
    let plan = ods_state::plan_with(
        &ws.project,
        latest.map(|s| (s.id, &s.snapshot)),
        &selected,
        now,
        options,
    )
    .map_err(|e| CliError::new(ExitStatus::Failure, codes::LINEAGE_BUILD, e.to_string()))?;
    for entry in &plan.entries {
        tracing::debug!(
            node = %entry.name,
            action = ?entry.action,
            why = entry.reasons.first().map_or("", |r| r.message.as_str()),
            "planned"
        );
    }
    let mut warnings = Vec::new();
    if ws.project.sources.iter().any(|s| s.version.is_none()) && ws.sources_file.is_none() {
        warnings.push(
            "no source freshness results: every node reading a source is built. Run `dbt source freshness` before planning."
                .to_owned(),
        );
    }
    if let (Some(taken), Some(head)) = (ws.sources_taken_at, latest)
        && !ws.project.sources.is_empty()
        && taken <= head.snapshot.created_at
    {
        warnings.push(format!(
            "sources.json was measured at {taken}, before the last recorded run ({}): nodes reading sources are built. Run `dbt source freshness` again before planning.",
            head.snapshot.created_at
        ));
    }
    if !ws.source_errors.is_empty() {
        warnings.push(format!(
            "`dbt source freshness` couldn't measure {}",
            ws.source_errors.join(", ")
        ));
    }
    Ok((plan, warnings))
}

/// `--select` values.
pub(super) fn select_specs(args: &ArgMatches) -> Vec<String> {
    args.get_many::<String>("select")
        .into_iter()
        .flatten()
        .cloned()
        .collect()
}

/// The dbt command that builds exactly the plan's BUILD set.
pub(super) fn dbt_command(plan: &ExecutionPlan) -> Option<String> {
    let build: Vec<&str> = plan
        .with_action(PlanAction::Build)
        .map(|e| e.name.as_str())
        .collect();
    (!build.is_empty()).then(|| format!("dbt build --select {}", build.join(" ")))
}

impl PlanReport {
    pub(super) fn build(args: &ArgMatches) -> Result<Self, CliError> {
        let ws = Workspace::load(args, Sources::AsGiven)?;
        let now = match args.get_one::<String>("now") {
            Some(at) => Timestamp::parse(at)
                .map_err(|e| CliError::new(ExitStatus::Usage, codes::STATE_INPUT, e))?,
            None => Timestamp::now(),
        };
        // Planning never writes: an absent database is simply "no state yet".
        let latest = if ws.state_db.is_file() {
            ws.latest(&ws.open_store()?)?
        } else {
            None
        };
        let (plan, warnings) = plan_against(
            &ws,
            latest.as_ref(),
            &select_specs(args),
            now,
            ods_state::PlanOptions::default(),
        )?;
        Ok(Self {
            dbt_command: dbt_command(&plan),
            build: plan.with_action(PlanAction::Build).count(),
            reuse: plan.with_action(PlanAction::Reuse).count(),
            based_on: latest.map(|s| s.id),
            target_dir: ws.target_dir,
            state_db: ws.state_db,
            scope: ws.scope.to_string(),
            sources_file: ws.sources_file,
            plan,
            warnings,
        })
    }
}

impl Present for PlanReport {
    const COMMAND: &'static str = "state.plan";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading("State plan".into()),
            ViewNode::KeyValue(vec![
                (
                    "scope".into(),
                    vec![Span::toned(self.scope.as_str(), Tone::Code)],
                ),
                (
                    "compared with".into(),
                    vec![Span::plain(self.based_on.map_or_else(
                        || "no recorded state: everything is built".to_owned(),
                        |id| format!("snapshot {id} in {}", self.state_db.display()),
                    ))],
                ),
                (
                    "decision".into(),
                    vec![Span::plain(format!(
                        "{} to build, {} to reuse",
                        self.build, self.reuse
                    ))],
                ),
            ]),
            ViewNode::Table {
                title: None,
                columns: vec!["node".into(), "action".into(), "why".into()],
                rows: self
                    .plan
                    .entries
                    .iter()
                    .map(|e| {
                        vec![
                            vec![Span::toned(e.name.as_str(), Tone::Code)],
                            vec![match e.action {
                                PlanAction::Build => Span::toned("build", Tone::Warning),
                                _ => Span::toned("reuse", Tone::Success),
                            }],
                            vec![Span::plain(
                                e.reasons
                                    .iter()
                                    .map(|r| r.message.as_str())
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            )],
                        ]
                    })
                    .collect(),
            },
        ];
        if let Some(command) = &self.dbt_command {
            blocks.push(ViewNode::KeyValue(vec![(
                "run".into(),
                vec![Span::toned(command.as_str(), Tone::Code)],
            )]));
        }
        if self.reuse > 0 {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(
                    "reuse assumes each relation built earlier still exists; ODS doesn't check the warehouse yet",
                )],
            });
        }
        for warning in &self.warnings {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(warning.as_str())],
            });
        }
        ViewNode::Group(blocks)
    }
}

// ---------------------------------------------------------------------------- record

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct RecordReport {
    state_db: PathBuf,
    scope: String,
    snapshot: SnapshotId,
    parent: Option<SnapshotId>,
    run_id: String,
    /// Whether source versions were recorded (only if measured before the run).
    sources_recorded: bool,
    #[serde(skip)]
    has_sources: bool,
    #[serde(flatten)]
    recorded: Recorded,
}

impl RecordReport {
    pub(super) fn build(args: &ArgMatches) -> Result<Self, CliError> {
        let ws = Workspace::load(args, Sources::AsGiven)?;
        let results_path = args
            .get_one::<String>("run-results")
            .map_or_else(|| ws.target_dir.join("run_results.json"), PathBuf::from);
        let run = RunResults::read(&results_path).map_err(|e| {
            CliError::new(ExitStatus::Failure, codes::STATE_INPUT, e.to_string()).with_hint(
                "record right after `dbt run` or `dbt build`, from the same target directory",
            )
        })?;
        check_recordable(&run, ws.invocation_id.as_deref())?;
        let started = run
            .started_at
            .as_deref()
            .and_then(|t| Timestamp::parse(t).ok());
        let finished = run
            .generated_at
            .as_deref()
            .and_then(|t| Timestamp::parse(t).ok())
            .unwrap_or_else(Timestamp::now);
        let sources_predate_run = matches!(
            (ws.sources_taken_at, started),
            // Strictly before: timestamps are to the second.
            (Some(taken), Some(started)) if taken < started
        );
        let planned: BTreeSet<&str> = ws.project.nodes.iter().map(|n| n.id.as_str()).collect();
        let results: Vec<RunResult> = run
            .results
            .iter()
            // Tests and operations aren't state; they don't advance anything.
            .filter(|r| {
                planned.contains(r.unique_id.as_str()) || !is_test_or_operation(&r.unique_id)
            })
            .map(|r| {
                RunResult::new(
                    r.unique_id.clone(),
                    match r.status {
                        RunStatus::Success => Outcome::Success,
                        RunStatus::Skipped => Outcome::Skipped,
                        _ => Outcome::Failed,
                    },
                    r.completed_at
                        .as_deref()
                        .and_then(|t| Timestamp::parse(t).ok()),
                )
            })
            .collect();
        let run_id = run
            .invocation_id
            .clone()
            .unwrap_or_else(|| format!("run-{}", finished.unix()));
        let store = ws.open_store()?;
        let latest = ws.latest(&store)?;
        let recent = block_on(store.history(&ws.scope, 1000))?.map_err(|e| store_error(&e))?;
        if recent.iter().any(|h| h.run_id == run_id) {
            return Err(CliError::new(
                ExitStatus::Failure,
                codes::STATE_INPUT,
                format!("run {run_id} is already recorded"),
            ));
        }
        if let (Some(head), Some(started)) = (&latest, started)
            && started < head.snapshot.created_at
        {
            return Err(CliError::new(
                ExitStatus::Failure,
                codes::STATE_INPUT,
                format!(
                    "run {run_id} started at {started}, before the recorded state (snapshot {}, {})",
                    head.id, head.snapshot.created_at
                ),
            )
            .with_hint("record runs in the order they ran"));
        }
        let recorded = ods_state::record(
            &ws.project,
            latest.as_ref().map(|s| (s.id, &s.snapshot)),
            &results,
            &run_id,
            finished,
            sources_predate_run,
        );
        let snapshot: &StateSnapshot = &recorded.snapshot;
        let id = block_on(store.commit(&ws.scope, snapshot))?.map_err(|e| {
            store_error(&e).with_hint("another run recorded state first; plan again and retry")
        })?;
        Ok(Self {
            state_db: ws.state_db,
            scope: ws.scope.to_string(),
            snapshot: id,
            parent: latest.map(|s| s.id),
            run_id,
            sources_recorded: sources_predate_run,
            has_sources: !ws.project.sources.is_empty(),
            recorded,
        })
    }
}

/// dbt commands whose successes are builds.
const RECORDABLE: [&str; 4] = ["build", "run", "seed", "snapshot"];

/// Refuses run results that don't show real builds of this manifest's code.
fn check_recordable(run: &RunResults, manifest_invocation: Option<&str>) -> Result<(), CliError> {
    let refuse = |message: String, hint: &str| {
        Err(CliError::new(ExitStatus::Failure, codes::STATE_INPUT, message).with_hint(hint))
    };
    match run.command.as_deref() {
        Some(command) if RECORDABLE.contains(&command) => {}
        other => {
            return refuse(
                format!(
                    "`run_results.json` is from `dbt {}`, which doesn't build anything",
                    other.unwrap_or("?")
                ),
                "record right after `dbt build`, `run`, `seed` or `snapshot`",
            );
        }
    }
    if run.empty {
        return refuse(
            "the run used `--empty`: its relations have no rows".to_owned(),
            "record a run without `--empty`",
        );
    }
    match (manifest_invocation, run.invocation_id.as_deref()) {
        (Some(manifest), Some(results)) if manifest == results => Ok(()),
        _ => refuse(
            "the manifest and the run results come from different dbt invocations, so ODS can't tell which code was built"
                .to_owned(),
            "record right after the run, before another dbt command rewrites the target directory (with `manifest.json`: `--artifacts json`)",
        ),
    }
}

pub(super) fn is_test_or_operation(id: &str) -> bool {
    ["test.", "unit_test.", "operation.", "analysis."]
        .iter()
        .any(|p| id.starts_with(p))
}

impl Present for RecordReport {
    const COMMAND: &'static str = "state.record";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading("State recorded".into()),
            ViewNode::KeyValue(vec![
                (
                    "scope".into(),
                    vec![Span::toned(self.scope.as_str(), Tone::Code)],
                ),
                (
                    "snapshot".into(),
                    vec![Span::plain(match self.parent {
                        Some(parent) => format!("{} (after {parent})", self.snapshot),
                        None => format!("{} (first)", self.snapshot),
                    })],
                ),
                (
                    "run".into(),
                    vec![Span::toned(self.run_id.as_str(), Tone::Code)],
                ),
                (
                    "advanced".into(),
                    vec![Span::plain(format!(
                        "{} nodes",
                        self.recorded.advanced.len()
                    ))],
                ),
            ]),
        ];
        if !self.recorded.kept.is_empty() {
            blocks.push(ViewNode::Table {
                title: Some("Kept their last successful state".into()),
                columns: vec!["node".into(), "why".into()],
                rows: self
                    .recorded
                    .kept
                    .iter()
                    .map(|(node, why)| {
                        vec![
                            vec![Span::toned(display_name(node), Tone::Code)],
                            vec![Span::plain(why.as_str())],
                        ]
                    })
                    .collect(),
            });
        }
        if self.has_sources && !self.sources_recorded {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(
                    "source versions weren't recorded (no sources.json measured before the run), so nodes reading sources will be built next time",
                )],
            });
        }
        ViewNode::Group(blocks)
    }
}

// ---------------------------------------------------------------------------- history

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct HistoryReport {
    state_db: PathBuf,
    scope: String,
    snapshots: Vec<SnapshotSummary>,
}

impl HistoryReport {
    pub(super) fn build(args: &ArgMatches) -> Result<Self, CliError> {
        let state_db = PathBuf::from(
            args.get_one::<String>("state-db")
                .map_or(DEFAULT_STORE, String::as_str),
        );
        let ws = Workspace::load(args, Sources::AsGiven)?;
        let limit = args.get_one::<usize>("limit").copied().unwrap_or(20);
        let snapshots = if Path::new(&state_db).is_file() {
            let store = ws.open_store()?;
            block_on(store.history(&ws.scope, limit))?.map_err(|e| store_error(&e))?
        } else {
            Vec::new()
        };
        Ok(Self {
            state_db,
            scope: ws.scope.to_string(),
            snapshots,
        })
    }
}

impl Present for HistoryReport {
    const COMMAND: &'static str = "state.history";

    fn view(&self) -> ViewNode {
        ViewNode::Group(vec![
            ViewNode::Heading(format!("State history of {}", self.scope)),
            ViewNode::Table {
                title: None,
                columns: vec![
                    "snapshot".into(),
                    "after".into(),
                    "recorded".into(),
                    "run".into(),
                    "nodes".into(),
                ],
                rows: self
                    .snapshots
                    .iter()
                    .map(|s| {
                        vec![
                            vec![Span::plain(s.id.to_string())],
                            vec![Span::plain(
                                s.parent.map_or_else(|| "-".to_owned(), |p| p.to_string()),
                            )],
                            vec![Span::plain(s.created_at.to_string())],
                            vec![Span::toned(s.run_id.as_str(), Tone::Code)],
                            vec![Span::plain(s.nodes.to_string())],
                        ]
                    })
                    .collect(),
            },
        ])
    }
}
