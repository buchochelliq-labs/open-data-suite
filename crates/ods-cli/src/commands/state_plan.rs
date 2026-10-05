//! `ods state plan`, `ods state record` and `ods state history` (#11, #20, #22, #25;
//! ADR-0013). This is where dbt artifacts, the planner and the state store meet.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_config::Loaded;
use ods_core::state::{
    DataVersion, Exactness, ExecutionPlan, PlanAction, PlanEntry, SnapshotId, StateSnapshot,
    Timestamp,
};
use ods_core::{Capability, CapabilitySet};
use ods_provider_dbt::fingerprint::{checks_digest, fingerprint};
use ods_provider_dbt::state_config::resolve;
use ods_provider_dbt::{
    ArtifactPreference, Artifacts, ResourceType, RunResults, RunStatus, SourceFreshness,
};
use ods_sdk::ProviderError;
use ods_sdk::contracts::state_store::{SnapshotSummary, StateScope, StateStore, StoredSnapshot};
use ods_state::{
    Node, Outcome, Project, Recorded, RunResult, Source, VersionAnswer, VersionReading,
};
use ods_store_sqlite::SqliteStateStore;
use serde::Serialize;

use super::state_settings::{DEFAULT_STORE, StateSettings};
use crate::exit::{CliError, ExitStatus, codes};
use crate::present::{Level, Line, Link, Present, Span, Tone, ViewNode};

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
                .help("SQLite state database (created if missing) [config: state.db]"),
        )
        .arg(
            Arg::new("environment")
                .long("environment")
                .value_name("NAME")
                .help("Keep separate state per environment, e.g. dev and prod [config: state.environment; default: the dbt target, else `default`]"),
        )
        .arg(
            Arg::new("target")
                .long("target")
                .value_name("NAME")
                .env("DBT_TARGET")
                .hide_env_values(true)
                .help("dbt's --target (the profile output to use); also the default environment"),
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
    common(Command::new("history").about(
        "List recorded state snapshots, newest first; with a node, its builds and why each happened",
    ))
    .arg(
        Arg::new("limit")
            .long("limit")
            .value_name("N")
            .value_parser(clap::value_parser!(usize))
            .default_value("20")
            .help("How many to show"),
    )
    .arg(
        Arg::new("node")
            .value_name("NODE")
            .conflicts_with("run")
            .help("A model, seed or snapshot (name or unique id): its builds, tests, and what changed before each build"),
    )
    .arg(
        Arg::new("run")
            .long("run")
            .value_name("RUN_ID")
            .help("A run's id (as the list shows it): each node's status, time taken, rows and error, from the run's journal, also for a run that failed and recorded nothing"),
    )
}

pub(super) fn store_error(error: &ProviderError) -> CliError {
    match error {
        ProviderError::Conflict(_) => CliError::new(
            ExitStatus::Failure,
            codes::STATE_CONFLICT,
            error.to_string(),
        ),
        ProviderError::Corrupt(_) => {
            CliError::new(ExitStatus::Failure, codes::STATE_DAMAGED, error.to_string()).with_hint(
                "nothing was changed; `ods state doctor` says what is wrong and how to recover",
            )
        }
        _ => CliError::new(ExitStatus::Failure, codes::STATE_STORE, error.to_string()),
    }
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
    /// The manifest the project was read from.
    pub(super) manifest: ods_provider_dbt::Manifest,
    /// What was read about sources' data versions, which the planner picks from.
    pub(super) readings: Vec<VersionReading>,
}

/// `sources.json`'s `max_loaded_at` per source, as a `source_freshness` reading, taken
/// at `taken_at`. A source whose measurement errored is unknown, with dbt's status.
fn freshness_reading(freshness: &SourceFreshness, taken_at: Option<Timestamp>) -> VersionReading {
    let versions = freshness.max_loaded_at.iter().map(|(id, at)| {
        // Normalised, so the same instant written differently compares equal.
        let value = Timestamp::parse(at).map_or_else(|_| at.clone(), |t| t.to_string());
        (
            id.clone(),
            VersionAnswer::Version(DataVersion::new(
                value,
                Exactness::Semantic,
                "sources.json max_loaded_at",
            )),
        )
    });
    let errors = freshness.errors.iter().map(|(id, status)| {
        (
            id.clone(),
            VersionAnswer::Unknown(format!("`dbt source freshness` reported {status}")),
        )
    });
    VersionReading::new(
        CapabilitySet::from([Capability::SourceFreshness]),
        taken_at,
        errors.chain(versions).collect(),
    )
}

impl Workspace {
    pub(super) fn load(
        args: &ArgMatches,
        settings: &StateSettings,
        sources: Sources,
    ) -> Result<Self, CliError> {
        let target_dir = settings.target_dir();
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
        // Separate state per dbt target, unless told otherwise (#227).
        let scope = StateScope::new(&project_name, &settings.environment.value)
            .map_err(|e| CliError::new(ExitStatus::Usage, codes::STATE_INPUT, e))?;
        let policies = resolve(manifest);
        let nodes = plan_nodes(manifest, &policies);
        let mut sources: Vec<Source> = manifest
            .nodes
            .iter()
            .filter(|n| n.resource_type == ResourceType::Source)
            .map(|n| {
                Source::new(n.unique_id.clone(), display_name(&n.unique_id), None)
                    // Its data tests (#232), identified as a node's are.
                    .with_checks(checks_digest(manifest, &n.unique_id))
            })
            .collect();
        let readings: Vec<VersionReading> = freshness
            .as_ref()
            .map(|f| freshness_reading(f, sources_taken_at))
            .into_iter()
            .collect();
        // The planner picks each source's version from what was read (ADR-0022 §2).
        ods_state::choose_source_versions(&mut sources, &readings);
        Ok(Self {
            state_db: settings.state_db(),
            sources_taken_at,
            invocation_id: manifest.invocation_id.clone(),
            source_errors: freshness
                .map(|f| f.errors.into_keys().collect())
                .unwrap_or_default(),
            sources_file,
            target_dir,
            project: Project::new(nodes, sources),
            scope,
            manifest: artifacts.manifest,
            readings,
        })
    }

    /// Adds what another reader found about sources' data versions (e.g. table
    /// versions), and has the planner pick each source's version again from
    /// everything read (ADR-0022 §2).
    pub(super) fn add_reading(&mut self, reading: VersionReading) {
        self.readings.push(reading);
        ods_state::choose_source_versions(&mut self.project.sources, &self.readings);
    }

    /// Whether some source's version could come from `sources.json`: one without a
    /// relation version.
    fn freshness_matters(&self) -> bool {
        self.project.sources.iter().any(|s| {
            !s.version_evidence.iter().any(|e| {
                e.kind == "source_version_strategy"
                    && e.value.as_deref() == Some("relation_versions")
            })
        })
    }

    pub(super) fn open_store(&self) -> Result<SqliteStateStore, CliError> {
        block_on(SqliteStateStore::open(&self.state_db))?.map_err(|e| match e {
            // Its own hint says what to do.
            ProviderError::Corrupt(_) => store_error(&e),
            _ => {
                store_error(&e).with_hint(format!("check `--state-db {}`", self.state_db.display()))
            }
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
    let ephemeral = |n: &ods_provider_dbt::ManifestNode| {
        n.resource_type == ResourceType::Model && n.materialized.as_deref() == Some("ephemeral")
    };
    let by_id = manifest.nodes_by_id();
    manifest
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.resource_type,
                ResourceType::Model | ResourceType::Seed | ResourceType::Snapshot
            ) && !ephemeral(n)
        })
        .map(|n| {
            let node = Node::new(
                n.unique_id.clone(),
                node_name(n),
                kind_word(n.resource_type),
                ods_provider_dbt::Manifest::dependencies_through(&by_id, &n.depends_on, ephemeral)
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
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

/// A node's name as dbt selects it: `orders`, or `orders.v2` for a model version.
pub(super) fn node_name(n: &ods_provider_dbt::ManifestNode) -> String {
    let name = n.name.clone().unwrap_or_else(|| display_name(&n.unique_id));
    match &n.version {
        Some(v) => format!("{name}.v{v}"),
        None => name,
    }
}

/// `model.shop.orders` → `orders`; `source.shop.raw.orders` → `raw.orders`;
/// `test.shop.not_null_orders_id.1a2b3c4d5e` → `not_null_orders_id`.
pub(super) fn display_name(id: &str) -> String {
    let mut parts = id.splitn(3, '.');
    let kind = parts.next().unwrap_or_default();
    let _package = parts.next();
    let rest = parts.next().unwrap_or(id);
    if kind == "source" {
        rest.to_owned()
    } else if kind == "test" {
        // `test.shop.not_null_orders_id.1a2b3c4d5e`: the name, not the hash after it.
        rest.split('.').next().unwrap_or(rest).to_owned()
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
    pub(super) state_db: PathBuf,
    pub(super) scope: String,
    pub(super) based_on: Option<SnapshotId>,
    /// The target the recorded state was built in, as recorded: `plan` doesn't run
    /// dbt, so it isn't checked against the target dbt would build in now.
    #[serde(skip_serializing_if = "Option::is_none")]
    recorded_target: Option<ods_core::state::TargetIdentity>,
    sources_file: Option<PathBuf>,
    build: usize,
    reuse: usize,
    /// The build time reusing would save, estimated from each reused node's last
    /// measured build (#210, ADR-0029).
    savings: ods_state::Savings,
    /// The dbt command that builds exactly the BUILD set.
    dbt_command: Option<String>,
    pub(super) plan: ExecutionPlan,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) warnings: Vec<String>,
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
    // Table versions may have been read, but if none was usable either, every reader
    // still builds.
    if ws.sources_file.is_none()
        && !ws.project.sources.is_empty()
        && ws.project.sources.iter().all(|s| s.version.is_none())
    {
        warnings.push(
            "no source freshness results: every node reading a source is built. Run `dbt source freshness` before planning."
                .to_owned(),
        );
    }
    if let (Some(taken), Some(head)) = (ws.sources_taken_at, latest)
        && ws.freshness_matters()
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
    // Not every command that plans selects.
    args.try_get_many::<String>("select")
        .ok()
        .flatten()
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

/// A plan against `latest`, as `ods state plan` makes it, with the notes and warnings
/// that qualify it.
pub(super) struct Planned {
    pub(super) plan: ExecutionPlan,
    pub(super) warnings: Vec<String>,
    pub(super) based_on: Option<SnapshotId>,
    pub(super) recorded_target: Option<ods_core::state::TargetIdentity>,
}

/// Plans `ws` against `latest` (the scope's head, if any), offline. Shared by
/// `ods state plan` and `ods serve`'s dashboard, so both say the same.
pub(super) fn plan_latest(
    ws: &Workspace,
    settings: &StateSettings,
    latest: Option<StoredSnapshot>,
    specs: &[String],
    now: Timestamp,
) -> Result<Planned, CliError> {
    // `plan` doesn't run dbt, so it can't ask which target dbt builds in (#227):
    // state recorded under another target name than --target is planned as `run`
    // would plan it, with none of it reused; state that doesn't name one (recorded
    // with `ods state record`) is planned as recorded, and the plan says so.
    let recorded_target = latest.as_ref().and_then(|l| l.snapshot.target.clone());
    let mut notes = Vec::new();
    let other = match (&recorded_target, settings.target.as_ref().map(|t| &t.value)) {
        (None, _) if latest.is_some() => {
            notes.push("the recorded state doesn't say which target it was built in: reuse assumes dbt still builds in the same one (`ods state run` checks, and rebuilds if it can't tell)".to_owned());
            None
        }
        (Some(recorded), Some(given)) if &recorded.name != given => Some(format!(
            "the recorded state was built in target {recorded}, not {given}: nothing in it is reused"
        )),
        _ => None,
    };
    let options = ods_state::PlanOptions::default();
    let (latest, options) = match (latest, other) {
        (Some(latest), Some(note)) => {
            notes.push(note);
            let mut empty = latest.snapshot.clone();
            empty.nodes.clear();
            empty.sources.clear();
            (
                Some(StoredSnapshot::new(latest.id, empty)),
                options.target_changed(),
            )
        }
        (latest, _) => (latest, options),
    };
    // Offline, as the relation check is (ADR-0016): it runs no dbt command.
    if super::state_versions::reads_table_versions(ws) {
        // `ods state explain` plans through here, so it says the same.
        notes.push("sources' table versions weren't read: this command doesn't run dbt, so their versions come from sources.json only; `ods state build --dry-run` reads them".to_owned());
    }
    let (plan, mut warnings) = plan_against(ws, latest.as_ref(), specs, now, options)?;
    warnings.extend(notes);
    Ok(Planned {
        based_on: latest.map(|s| s.id),
        plan,
        warnings,
        recorded_target,
    })
}

impl PlanReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let settings = StateSettings::resolve(args, config)?;
        let ws = Workspace::load(args, &settings, Sources::AsGiven)?;
        let now = match args.try_get_one::<String>("now").ok().flatten() {
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
        let before = latest.as_ref().map(|s| s.snapshot.clone());
        let Planned {
            plan,
            warnings,
            based_on,
            recorded_target,
        } = plan_latest(&ws, &settings, latest, &select_specs(args), now)?;
        let build = plan.with_action(PlanAction::Build).count();
        Ok(Self {
            dbt_command: dbt_command(&plan),
            savings: ods_state::plan_savings(&plan, build, before.as_ref()),
            build,
            reuse: plan.with_action(PlanAction::Reuse).count(),
            based_on,
            recorded_target,
            target_dir: ws.target_dir,
            state_db: ws.state_db,
            scope: ws.scope.to_string(),
            sources_file: ws.sources_file,
            plan,
            warnings,
        })
    }
}

/// A plan's nodes as a table, `action` naming each one's action: in sections by action
/// (each in plan order, what is built first), with the count of each in the footer.
pub(super) fn plan_table(entries: &[PlanEntry], action: impl Fn(&PlanEntry) -> Span) -> ViewNode {
    let mut sections: Vec<(String, Vec<Vec<Line>>)> = Vec::new();
    for e in entries {
        let label = action(e);
        let row = vec![
            vec![Span::toned(e.name.as_str(), Tone::Code)],
            vec![label.clone()],
            vec![Span::plain(
                e.reasons
                    .iter()
                    .map(|r| r.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            )],
        ];
        match sections.iter_mut().find(|(text, _)| *text == label.text) {
            Some((_, rows)) => rows.push(row),
            None => sections.push((label.text, vec![row])),
        }
    }
    // What is built comes first, whatever the plan's first node does.
    sections.sort_by_key(|(text, _)| text != "build");
    let counts = sections
        .iter()
        .map(|(text, rows)| format!("{} {text}", rows.len()))
        .collect::<Vec<_>>()
        .join(" · ");
    let mut breaks = Vec::new();
    let mut rows = Vec::new();
    for (_, section) in sections {
        breaks.push(rows.len());
        rows.extend(section);
    }
    ViewNode::Table {
        title: None,
        columns: vec!["node".into(), "action".into(), "why".into()],
        rows,
        breaks,
        footer: Some(vec![
            vec![Span::toned(
                format!(
                    "{} {}",
                    entries.len(),
                    if entries.len() == 1 { "node" } else { "nodes" }
                ),
                Tone::Emphasis,
            )],
            vec![Span::plain(counts)],
            Vec::new(),
        ]),
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
                    "recorded in".into(),
                    vec![Span::plain(self.recorded_target.as_ref().map_or_else(
                        || "no target recorded".to_owned(),
                        |t| format!("target {t} (not checked: `ods state compile` asks dbt)"),
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
            plan_table(&self.plan.entries, |e| match e.action {
                PlanAction::Build => Span::toned("build", Tone::Warning),
                _ => Span::toned("reuse", Tone::Success),
            }),
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
                message: vec![
                    Span::plain("reuse assumes each relation built earlier still exists: "),
                    Span::toned("ods state plan", Tone::Code),
                    Span::plain(" doesn't check the warehouse; "),
                    Span::toned("ods state build --dry-run", Tone::Code),
                    Span::plain(" does"),
                ],
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
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let settings = StateSettings::resolve(args, config)?;
        let ws = Workspace::load(args, &settings, Sources::AsGiven)?;
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
                // dbt's `execution_time` (ADR-0029).
                .timed(r.details.execution_ms)
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
                breaks: Vec::new(),
                footer: None,
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

/// A snapshot, with what its run's journal says, when it has one (#322).
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct HistoryEntry {
    #[serde(flatten)]
    snapshot: SnapshotSummary,
    /// The run's totals, from its journal.
    #[serde(skip_serializing_if = "Option::is_none")]
    run_stats: Option<super::run_stats::RunBrief>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct HistoryReport {
    state_db: PathBuf,
    scope: String,
    snapshots: Vec<HistoryEntry>,
}

impl HistoryReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let settings = StateSettings::resolve(args, config)?;
        let state_db = settings.state_db();
        let ws = Workspace::load(args, &settings, Sources::AsGiven)?;
        let limit = args.get_one::<usize>("limit").copied().unwrap_or(20);
        let snapshots = if Path::new(&state_db).is_file() {
            let store = ws.open_store()?;
            block_on(store.history(&ws.scope, limit))?.map_err(|e| store_error(&e))?
        } else {
            Vec::new()
        };
        let snapshots = snapshots
            .into_iter()
            .map(|snapshot| HistoryEntry {
                run_stats: super::run_stats::RunBrief::of_run(&state_db, &snapshot.run_id),
                snapshot,
            })
            .collect();
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
        let missing = || super::run_stats::MISSING.to_owned();
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
                    "took".into(),
                    "rows".into(),
                ],
                rows: self
                    .snapshots
                    .iter()
                    .map(|e| {
                        let s = &e.snapshot;
                        let brief = e.run_stats.as_ref();
                        vec![
                            vec![Span::plain(s.id.to_string())],
                            vec![Span::plain(
                                s.parent.map_or_else(|| "-".to_owned(), |p| p.to_string()),
                            )],
                            vec![Span::plain(s.created_at.to_string())],
                            vec![Span::toned(s.run_id.as_str(), Tone::Code)],
                            vec![Span::plain(s.nodes.to_string())],
                            vec![Span::plain(
                                brief
                                    .and_then(|b| b.duration_ms)
                                    .map_or_else(missing, super::run_stats::duration),
                            )],
                            vec![Span::plain(brief.map_or_else(missing, |b| {
                                super::run_stats::rows_line(&b.totals)
                            }))],
                        ]
                    })
                    .collect(),
                breaks: Vec::new(),
                footer: None,
            },
        ])
    }
}

/// `ods state history --run <id>` (#322): one run's per-node stats, from its journal.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct RunHistoryReport {
    state_db: PathBuf,
    journal: PathBuf,
    /// Lines of the journal that couldn't be read (a newer version, or a last line cut
    /// short).
    #[serde(skip_serializing_if = "is_zero")]
    unreadable_lines: usize,
    run: ods_sdk::contracts::run_events::RunSummary,
    /// Each failed node, explained (#323, ADR-0025), with the evidence there is now.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failures: Vec<ods_core::failure::ErrorExplanation>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes a reference"
)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl RunHistoryReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let settings = StateSettings::resolve(args, config)?;
        let state_db = settings.state_db();
        let run_id = args.get_one::<String>("run").map_or("", String::as_str);
        let (journal, read) = read_journal(&state_db, Some(run_id))?;
        let run = ods_sdk::contracts::run_events::RunSummary::from_events(&read.events);
        let failures = explain_history(args, &settings, &run);
        Ok(Self {
            run,
            unreadable_lines: read.unreadable,
            journal,
            state_db,
            failures,
        })
    }
}

/// The journal of run `run_id`, or of the last run that has one, read: its path and
/// events. An unknown run, or none at all, is an input error saying where journals are
/// kept.
pub(super) fn read_journal(
    state_db: &Path,
    run_id: Option<&str>,
) -> Result<(PathBuf, super::run_journal::ReadJournal), CliError> {
    let no_journal = |why: String| {
        CliError::new(ExitStatus::Failure, codes::STATE_INPUT, why).with_hint(format!(
            "journals are kept beside the state database, in {}, for the {} most recent runs that ran dbt",
            super::run_journal::dir_for(state_db).display(),
            super::run_journal::KEEP
        ))
    };
    let (run_id, journal) = if let Some(run_id) = run_id {
        let Some(journal) = super::run_journal::path_for(state_db, run_id) else {
            return Err(no_journal(format!(
                "`{}` isn't a run id",
                run_id.escape_debug()
            )));
        };
        (run_id.to_owned(), journal)
    } else {
        let last = ods_sdk::run_journal::Journals::beside(state_db)
            .list()
            .map_err(|e| {
                CliError::new(
                    ExitStatus::Failure,
                    codes::STATE_INPUT,
                    format!("can't list the run journals: {e}"),
                )
            })?
            .into_iter()
            .next()
            .ok_or_else(|| {
                no_journal(format!("no run of `{}` has a journal", state_db.display()))
            })?;
        (last.run_id, last.path)
    };
    let read = super::run_journal::read(&journal)
        .map_err(|why| CliError::new(ExitStatus::Failure, codes::STATE_INPUT, why))?
        .ok_or_else(|| no_journal(format!("run {} has no journal", run_id.escape_debug())))?;
    Ok((journal, read))
}

/// Explains a past run's failed nodes from what is known now: the project as it is,
/// and the state before and after the run. Best effort: without a project or a store,
/// with less evidence.
pub(super) fn explain_history(
    args: &ArgMatches,
    settings: &StateSettings,
    run: &ods_sdk::contracts::run_events::RunSummary,
) -> Vec<ods_core::failure::ErrorExplanation> {
    if ods_state::failed_nodes(run).is_empty() && ods_state::failed_checks(run).is_empty() {
        return Vec::new();
    }
    let state_db = settings.state_db();
    let ws = Workspace::load(args, settings, Sources::Ignore).ok();
    let (before, after) = ws
        .as_ref()
        .filter(|ws| ws.state_db.is_file())
        .and_then(|ws| states_around(ws, run))
        .unwrap_or_default();
    let parents = |id: &str| {
        ws.as_ref()
            .and_then(|w| w.project.nodes.iter().find(|n| n.id == id))
            .map(|n| n.parents.clone())
            .unwrap_or_default()
    };
    let plan = ods_state::plan_from_states(run, before.as_ref(), after.as_ref(), &parents);
    let project_dir = super::failures::project_dir(settings);
    let target_dir = ws.as_ref().map_or_else(
        || PathBuf::from(&settings.target_dir.value),
        |w| w.target_dir.clone(),
    );
    let evidence = super::failures::Evidence {
        files: super::failures::ProjectFiles {
            project_dir: &project_dir,
            target_dir: &target_dir,
            manifest: ws.as_ref().map(|w| &w.manifest),
            last_manifest: None,
        },
        plan: Some(&plan),
        before: before.as_ref(),
        state_db: &state_db,
        retry: None,
        state_db_flag: super::failures::retry_state_db(settings),
        // The manifest describes this run's code only if this run wrote it (dbt's
        // invocation is the run id); otherwise it is the project as it is now.
        project_is_run: ws
            .as_ref()
            .and_then(|w| w.manifest.invocation_id.as_deref())
            .is_some_and(|id| run.run_id.as_deref() == Some(id)),
        // The configuration now may not be the run's: no health checks.
        doctor: None,
    };
    super::failures::explain_run(run, &evidence)
}

/// The state committed before `run` started, and the one it committed, if any.
fn states_around(
    ws: &Workspace,
    run: &ods_sdk::contracts::run_events::RunSummary,
) -> Option<(Option<StateSnapshot>, Option<StateSnapshot>)> {
    let store = ws.open_store().ok()?;
    let summaries = block_on(store.history(&ws.scope, usize::MAX)).ok()?.ok()?;
    let run_id = run.run_id.as_deref()?;
    let started = run.started_at?.to_seconds();
    let get = |id| {
        block_on(store.get(&ws.scope, id))
            .ok()
            .and_then(Result::ok)
            .flatten()
            .map(|s| s.snapshot)
    };
    let after = summaries.iter().find(|s| s.run_id == run_id);
    let before = match after {
        Some(after) => after.parent,
        None => summaries
            .iter()
            .find(|s| s.created_at <= started && s.run_id != run_id)
            .map(|s| s.id),
    };
    Some((before.and_then(get), after.and_then(|a| get(a.id))))
}

impl Present for RunHistoryReport {
    const COMMAND: &'static str = "state.history";

    fn view(&self) -> ViewNode {
        let run = &self.run;
        let mut summary = vec![(
            "run".into(),
            vec![Span::toned(
                run.run_id.as_deref().unwrap_or("?"),
                Tone::Code,
            )],
        )];
        if let Some(scope) = &run.scope {
            summary.push((
                "scope".into(),
                vec![Span::toned(scope.as_str(), Tone::Code)],
            ));
        }
        summary.push((
            "started".into(),
            vec![Span::plain(run.started_at.map_or_else(
                || super::run_stats::MISSING.to_owned(),
                |t| t.to_seconds().to_string(),
            ))],
        ));
        summary.push((
            "outcome".into(),
            vec![match run.outcome {
                Some(ods_sdk::contracts::run_events::RunOutcome::Succeeded) => {
                    Span::toned("succeeded", Tone::Success)
                }
                Some(ods_sdk::contracts::run_events::RunOutcome::Failed) => {
                    Span::toned("failed", Tone::Error)
                }
                Some(_) => Span::toned("unknown: dbt's results couldn't be read", Tone::Warning),
                None => Span::toned("still running, or stopped without finishing", Tone::Warning),
            }],
        ));
        summary.extend(super::run_stats::totals(run));
        summary.push((
            "journal".into(),
            vec![
                Span::toned(self.journal.display().to_string(), Tone::Code)
                    .linked(Link::file(&self.journal)),
            ],
        ));
        let mut blocks = vec![
            ViewNode::Heading("Run".into()),
            ViewNode::KeyValue(summary),
            super::run_stats::nodes_table(run),
        ];
        blocks.extend(super::failures::section(&self.failures));
        if self.unreadable_lines > 0 {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(format!(
                    "{} line(s) of the journal couldn't be read: written by a newer ODS, or cut short when the run stopped",
                    self.unreadable_lines
                ))],
            });
        }
        ViewNode::Group(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn display_names_are_what_people_select_by() {
        assert_eq!(display_name("model.shop.marts.orders"), "orders");
        assert_eq!(display_name("source.shop.raw.orders"), "raw.orders");
        assert_eq!(
            display_name("test.shop.not_null_orders_id.1a2b3c4d5e"),
            "not_null_orders_id"
        );
    }
}

#[cfg(test)]
mod plan_table_tests {
    use ods_core::state::{PlanAction, Reason, ReasonCode};

    use super::*;

    fn entry(name: &str, action: PlanAction) -> PlanEntry {
        PlanEntry::new(
            format!("model.p.{name}"),
            name,
            "model",
            action,
            vec![Reason::new(ReasonCode::Unchanged, "why")],
            ods_core::FreshnessPolicy::conservative(),
            0,
        )
    }

    #[test]
    fn builds_come_first_in_their_own_section_with_counts_below() {
        let entries = [
            entry("a", PlanAction::Reuse),
            entry("b", PlanAction::Build),
            entry("c", PlanAction::Reuse),
            entry("d", PlanAction::Build),
        ];
        let ViewNode::Table {
            rows,
            breaks,
            footer,
            ..
        } = plan_table(&entries, |e| match e.action {
            PlanAction::Build => Span::toned("build", Tone::Warning),
            _ => Span::toned("reuse", Tone::Success),
        })
        else {
            panic!("a table");
        };
        let names: Vec<String> = rows
            .iter()
            .map(|r| crate::present::view::plain_text(&r[0]))
            .collect();
        // Each section keeps the plan's order.
        assert_eq!(names, ["b", "d", "a", "c"]);
        assert_eq!(breaks, [0, 2]);
        let footer: Vec<String> = footer
            .unwrap()
            .iter()
            .map(|c| crate::present::view::plain_text(c))
            .collect();
        assert_eq!(footer, ["4 nodes", "2 build · 2 reuse", ""]);
    }
}
