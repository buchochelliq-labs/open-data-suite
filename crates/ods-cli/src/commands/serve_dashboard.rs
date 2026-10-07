//! What `ods serve`'s dashboard shows (#310): the project, its state store and the plan,
//! read here, where the providers are, and handed to `ods-web` as neutral facts
//! (ADR-0001, ADR-0009).
//!
//! Everything is read-only: the state store is opened without migrating or creating it,
//! and planning runs nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Arg, ArgMatches, Command};
use ods_core::state::Timestamp;
use ods_lineage::GraphFilter;
use ods_sdk::contracts::state_store::StateStore;
use ods_store_sqlite::SqliteStateStore;
use ods_web::catalog::LastBuild;
use ods_web::dashboard::explain::Explainer;
use ods_web::dashboard::state::{History, LastOutcome, LastRun, RUNS_LISTED};
use ods_web::dashboard::{
    ModuleState, ModuleStatus, OpaqueNode, Planner, RECENT_RUNS, Recorded, RunRecord, StateInput,
    StoreLocation, Target,
};
use ods_web::{Dashboard, Snapshot};

use super::lineage::Loaded;
use super::relation_links::{LinkSettings, Links};
use super::state_plan::{Planned, Sources, Workspace, block_on, plan_latest};
use super::state_settings::{DEFAULT_STORE, StateSettings};
use crate::exit::CliError;

/// `ods state`'s options that say where the state is, for `ods serve`.
pub(super) fn state_args(command: Command) -> Command {
    command
        .arg(
            Arg::new("state-db")
                .long("state-db")
                .value_name("PATH")
                .help(format!(
                    "SQLite state database the dashboard reads; never created or changed [config: state.db; default: {DEFAULT_STORE}]"
                )),
        )
        .arg(
            Arg::new("environment")
                .long("environment")
                .value_name("NAME")
                .help("Whose state to show, e.g. dev or prod [config: state.environment; default: the dbt target, else `default`]"),
        )
        .arg(
            Arg::new("target")
                .long("target")
                .value_name("NAME")
                .env("DBT_TARGET")
                .hide_env_values(true)
                .help("dbt's --target; also the default environment"),
        )
        .arg(Arg::new("sources").long("sources").value_name("PATH").help(
            "`dbt source freshness` results for the plan [default: <target-dir>/sources.json, if present]",
        ))
}

/// The health checks `[health]` sets up (ADR-0030), with this `ods`'s plugin checks
/// (ADR-0031 §4).
///
/// # Errors
/// A check id that isn't a built-in's, a plugin check whose id another check has, a
/// `[health.plugins.<id>]` naming no plugin check, or a path glob that isn't valid
/// (exit 4).
pub(super) fn health_settings(
    config: &ods_config::Loaded,
) -> Result<ods_health::HealthSettings, CliError> {
    health_settings_with(config, crate::plugins::installed())
}

/// [`health_settings`] with the plugin checks of `plugins`.
pub(super) fn health_settings_with(
    config: &ods_config::Loaded,
    plugins: &crate::plugins::Plugins,
) -> Result<ods_health::HealthSettings, CliError> {
    let mut health = ods_health::HealthSettings::from_config(&config.config.health)
        .map_err(|e| config_error(&e))?;
    for check in plugins.health_checks() {
        health = health
            .with_check(check.clone())
            .map_err(|e| config_error(&e))?;
    }
    health.check_plugin_config().map_err(|e| config_error(&e))?;
    // Before any project is read, probes are checked in the generic dialect, so a probe
    // that can write is an error however ODS is started.
    check_probe_sql(&mut health, None, &config.config.warehouses)?;
    Ok(health)
}

/// Checks every probe's SQL is one read-only query in the dialect of `adapter` (the
/// project's warehouse type, if known), with the SQL analyzer lineage uses (ADR-0030
/// §4a).
///
/// # Errors
/// A probe that isn't one read-only query (exit 4).
pub(super) fn check_probe_sql(
    health: &mut ods_health::HealthSettings,
    adapter: Option<&str>,
    warehouses: &crate::plugins::Warehouses,
) -> Result<(), CliError> {
    // The warehouse plugin's dialect, else the kind's; else generic SQL, which can only
    // refuse more (ADR-0031 §3a).
    let dialect = crate::plugins::installed()
        .dialect(adapter, warehouses)
        .as_deref()
        .and_then(ods_provider_sqlparser::SqlDialect::from_name)
        .unwrap_or(ods_provider_sqlparser::SqlDialect::Generic);
    health
        .check_probe_sql(&|sql| ods_provider_sqlparser::read_only_query(dialect, sql))
        .map_err(|e| config_error(&e))
}

fn config_error(e: &ods_health::HealthConfigError) -> CliError {
    CliError::new(
        crate::exit::ExitStatus::Config,
        crate::exit::codes::HEALTH_CONFIG,
        e.to_string(),
    )
}

/// Reads the dashboard's facts; built once, then asked again on every reload.
pub(super) struct DashboardSource {
    args: ArgMatches,
    settings: StateSettings,
    /// Where warehouse links point (#329), read once like the rest of the settings.
    links: LinkSettings,
    /// What an hour of build time costs (`[state.cost]`), for the savings panel.
    cost: Option<ods_config::CostConfig>,
    /// The health checks as `[health]` configures them (#392); a mistake there is a
    /// configuration error when the server starts, not a badge.
    health: ods_health::HealthSettings,
}

impl DashboardSource {
    pub(super) fn new(args: &ArgMatches, config: &ods_config::Loaded) -> Result<Self, CliError> {
        Ok(Self {
            args: args.clone(),
            settings: StateSettings::resolve(args, config)?,
            links: LinkSettings::read(config),
            cost: config.config.state.cost.clone(),
            health: health_settings(config)?,
        })
    }

    /// Files whose change means new state or a new plan: the database and its
    /// write-ahead log, where a commit lands first, the last run beside it, and the
    /// source freshness results
    /// the plan reads (`--sources`, else `<target-dir>/sources.json`). A file that
    /// doesn't exist yet is watched too: creating it is a change.
    pub(super) fn watched(&self) -> Vec<PathBuf> {
        let db = self.settings.state_db();
        let mut wal = db.clone().into_os_string();
        wal.push("-wal");
        let sources = self.args.get_one::<String>("sources").map_or_else(
            || self.settings.target_dir().join("sources.json"),
            PathBuf::from,
        );
        // A run that records nothing (everything failed) only changes the last run
        // kept for `ods state retry`, which the Runs page shows (#311).
        let mut last_run = db.clone().into_os_string();
        last_run.push(".last-run.json");
        // `ods health check` adds a record to this directory, which changes its time
        // (ADR-0030 §6).
        let health = ods_health::record::dir_for(&db);
        vec![
            db,
            PathBuf::from(wal),
            sources,
            PathBuf::from(last_run),
            health,
        ]
    }

    /// The server's snapshot of `loaded`, with the dashboard's facts.
    pub(super) fn snapshot(&self, loaded: &Loaded, source: String) -> Snapshot {
        let links = loaded.links(&self.links);
        let document = loaded.linked_document(&GraphFilter::default(), &links);
        let opaque_ids: Vec<(String, String, Option<String>)> = document
            .nodes
            .iter()
            .filter(|n| n.opaque)
            .map(|n| (n.id.clone(), n.name.clone(), n.diagnostics.first().cloned()))
            .collect();
        let erd = super::erd::dashboard_erd(loaded);
        let mut dashboard = self.dashboard(&opaque_ids, &loaded.graph, &links);
        // The ERD page (#64) shows it for this project, when it could be built.
        if erd.erd.is_ok() {
            for module in &mut dashboard.modules {
                if module.name == "ERD" {
                    *module = ModuleStatus::new("ERD", ModuleState::Ready, None);
                }
            }
        }
        let dashboard = dashboard.with_erd(erd);
        Snapshot::new(document, loaded.graph.clone(), source).with_dashboard(dashboard)
    }

    /// Never fails: what can't be read is shown as such.
    fn dashboard(
        &self,
        opaque: &[(String, String, Option<String>)],
        graph: &ods_lineage::ColumnGraph,
        links: &Links,
    ) -> Dashboard {
        let environment = self.settings.environment.value.clone();
        let ws = match Workspace::load(&self.args, &self.settings, Sources::AsGiven) {
            Ok(ws) => ws,
            // The project's files (artifacts, source freshness results) couldn't be
            // read: the store wasn't even opened, so it isn't blamed.
            Err(e) => {
                tracing::warn!(error = %e.message, "dashboard: the project can't be read");
                let hint = e.hint.clone().or_else(|| {
                    Some("check the files named above (`--sources`, `--target-dir`); the page reloads when they change".to_owned())
                });
                return Dashboard::new("project", environment)
                    .with_state(StateInput::ProjectUnreadable {
                        error: e.message,
                        hint,
                    })
                    .with_modules(modules(false));
            }
        };
        let ws = Arc::new(ws);
        let project = ws.manifest.project_name.clone().unwrap_or_default();
        let mut kinds = BTreeMap::new();
        for node in &ws.project.nodes {
            *kinds.entry(node.kind.clone()).or_insert(0) += 1;
        }
        let languages: BTreeMap<&str, &str> = ws
            .manifest
            .nodes
            .iter()
            .filter_map(|n| Some((n.unique_id.as_str(), n.language.as_deref()?)))
            .collect();
        let opaque = opaque
            .iter()
            .map(|(id, name, diagnostic)| {
                let why = match (languages.get(id.as_str()), diagnostic) {
                    (Some(language), _) if *language != "sql" => {
                        format!("{} model: column lineage unknown", capitalized(language))
                    }
                    (_, Some(diagnostic)) => format!("column lineage unknown: {diagnostic}"),
                    _ => "its SQL couldn't be analyzed: column lineage unknown".to_owned(),
                };
                OpaqueNode::new(id, name, why)
            })
            .collect();
        let explainer = explainer(&ws, &self.settings, graph);
        let (state, last_builds) = self.state(Arc::clone(&ws), explainer);
        // The Catalog (#313): the project's nodes, and their last builds from the
        // snapshot the plan is made against.
        let catalog =
            super::serve_catalog::catalog(&ws.manifest, &ws.target_dir, last_builds, links);
        // The Freshness evidence screen (#350): the sources as the planner sees them.
        let freshness = super::serve_catalog::freshness(&ws);
        let recorded = matches!(&state, StateInput::Recorded(r) if !r.runs.is_empty());
        // The checks `ods health check` ran that the dashboard doesn't run itself
        // (ADR-0030 §6), from the newest record for this scope.
        let health_record = ods_health::record::latest(
            &ods_health::record::dir_for(&ws.state_db),
            &ws.scope.to_string(),
        );
        let target = self
            .settings
            .target
            .as_ref()
            .map_or_else(|| environment.clone(), |t| t.value.clone());
        Dashboard::new(project, environment)
            .with_scope(ws.scope.to_string())
            .with_target(Some(Target::new(target, ws.manifest.adapter_type.clone())))
            .with_node_kinds(kinds)
            .with_opaque(opaque)
            .with_state(state)
            .with_modules(modules(recorded))
            .with_catalog(catalog)
            .with_freshness(freshness)
            .with_health(self.health.clone())
            .with_health_record(health_record.as_ref())
            // The live run view (#322): journals are read even before the store
            // exists, since a first run writes its journal before its first snapshot.
            .with_journals(ods_sdk::run_journal::Journals::beside(&ws.state_db))
    }

    /// What the store holds, and each node's last build (#313), from one read, so
    /// the plan and the builds shown with it rest on the same snapshot.
    fn state(
        &self,
        ws: Arc<Workspace>,
        explainer: Explainer,
    ) -> (StateInput, BTreeMap<String, LastBuild>) {
        let store = store_location(&ws.state_db);
        // Never created here: the dashboard only reads (AGENTS rule 5, #310).
        if !ws.state_db.is_file() {
            return (StateInput::NoStore { store }, BTreeMap::new());
        }
        let unreadable = |error: String| {
            tracing::warn!(%error, "dashboard: the state store can't be read");
            StateInput::Unreadable {
                store: store.clone(),
                error,
            }
        };
        let read = block_on(async {
            let db = SqliteStateStore::open_existing(&ws.state_db).await?;
            // The State pages list the newest runs, and read one more snapshot, so the
            // oldest listed run can say what it replaced (#311). The Snapshots tile
            // counts as far: it says "at least" beyond it.
            let history = db.history(&ws.scope, SNAPSHOTS_READ).await?;
            let mut snapshots = Vec::new();
            for summary in &history {
                if let Some(stored) = db.get(&ws.scope, summary.id).await? {
                    snapshots.push((stored.id.0, stored.snapshot));
                }
            }
            let runs: Vec<RunRecord> = snapshots
                .iter()
                .take(RECENT_RUNS)
                .map(|(id, snapshot)| RunRecord::of(*id, snapshot))
                .collect();
            // Which snapshot recorded each run, further back than the runs listed, so
            // the Catalog can say which snapshot a node's last build came from (#313).
            let runs_index = db.history(&ws.scope, RUNS_INDEXED).await?;
            let latest = db.latest(&ws.scope).await?;
            // The run ledger (ADR-0029), once the database has one: a ledger that can't
            // be read only leaves the savings panel out.
            let ledger = if db.schema_version().await? >= 2 {
                db.runs(&ws.scope, None, usize::MAX)
                    .await
                    .inspect_err(
                        |e| tracing::warn!(error = %e, "dashboard: the run ledger can't be read"),
                    )
                    .ok()
            } else {
                None
            };
            db.close().await;
            Ok::<_, ods_sdk::ProviderError>((
                history.len(),
                runs,
                snapshots,
                (runs_index, latest, ledger),
            ))
        });
        let (counted, runs, snapshots, (runs_index, latest, ledger)) = match read {
            Ok(Ok(read)) => read,
            Ok(Err(e)) => return (unreadable(e.to_string()), BTreeMap::new()),
            Err(e) => return (unreadable(e.message), BTreeMap::new()),
        };
        let last_run = last_run(&ws.state_db);
        // The Catalog's last builds (#313), from the snapshot the plan is made against.
        let last_builds =
            super::serve_catalog::last_builds(latest.as_ref(), &runs_index, &ws.manifest);
        // Plans depend on time (lag tolerances expire), so Home plans again on every
        // request, as of then; this first plan is the fallback.
        let journals = ods_sdk::run_journal::Journals::beside(&ws.state_db);
        let settings = self.settings.clone();
        let planner: Planner = Arc::new(move |now| {
            plan_latest(&ws, &settings, latest.clone(), &[], now)
                .map(|Planned { plan, warnings, .. }| (plan, warnings))
                .map_err(|e| {
                    tracing::warn!(error = %e.message, "dashboard: the plan can't be made");
                    e.message
                })
        });
        let (plan, warnings) = match planner(Timestamp::now()) {
            Ok((plan, warnings)) => (Ok(plan), warnings),
            Err(error) => (Err(error), Vec::new()),
        };
        let mut history = History::new(snapshots)
            .with_last_run(last_run)
            // Each run's outcome, times and per-node stats (#322): read by the pages
            // when asked, through ods-sdk's journal reader.
            .with_journals(journals)
            // Failed nodes explained with dbt's error catalogue (#323).
            .with_explainer(explainer);
        // The savings panel only from a ledger that was read: one that couldn't be is
        // left out, never shown as empty (AGENTS rule 3).
        if let Some(ledger) = ledger_view(ledger, self.cost.as_ref()) {
            history = history.with_ledger(ledger);
        }
        let state = StateInput::Recorded(Box::new(
            Recorded::new(store, runs, counted, plan)
                .capped(counted >= SNAPSHOTS_READ)
                .with_warnings(warnings)
                .with_planner(planner)
                .with_history(history),
        ));
        (state, last_builds)
    }
}

/// The run ledger and the cost rate, for the Runs page's savings panel (ADR-0029);
/// `None` when there is no ledger yet or it couldn't be read.
fn ledger_view(
    runs: Option<Vec<ods_core::state::RunEntry>>,
    cost: Option<&ods_config::CostConfig>,
) -> Option<ods_web::dashboard::state::Ledger> {
    let ledger = ods_web::dashboard::state::Ledger::new(runs?);
    Some(match cost {
        Some(cost) => ledger.with_rate(cost.rate_per_hour, cost.unit.clone()),
        None => ledger,
    })
}

/// What explains failed nodes on the Run pages (#323, ADR-0025): dbt's error
/// catalogue, the project index from the manifest, each node's parents, and the
/// columns each node reads that its upstreams don't produce (column lineage).
fn explainer(
    ws: &Workspace,
    settings: &StateSettings,
    graph: &ods_lineage::ColumnGraph,
) -> Explainer {
    let project_dir = super::failures::project_dir(settings);
    let files = super::failures::ProjectFiles {
        project_dir: &project_dir,
        target_dir: &ws.target_dir,
        manifest: Some(&ws.manifest),
        last_manifest: None,
        warehouses: &settings.warehouses,
    };
    let missing = graph
        .nodes()
        .map(|n| (n.id.clone(), graph.missing_columns(&n.id)))
        .filter(|(_, m)| !m.is_empty())
        .collect();
    let parents = ws
        .project
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.parents.clone()))
        .collect();
    let mut explainer = Explainer::new(Arc::new(
        crate::plugins::installed()
            .project_catalogue(ws.manifest.adapter_type.as_deref(), &settings.warehouses),
    ))
    .with_missing_columns(missing)
    .with_parents(parents)
    .with_artifacts_from(ws.manifest.invocation_id.clone())
    .with_retry_state_db(super::failures::retry_state_db(settings).map(str::to_owned));
    if let Some(index) = files.index() {
        explainer = explainer.with_index(index);
    }
    explainer
}

/// The last run kept beside the store for `ods state retry`, if any: the only record
/// of which nodes failed (#292), for the State pages (#311).
fn last_run(state_db: &Path) -> Option<LastRun> {
    let (path, last) = super::state_retry::peek(state_db)?;
    let outcome = last.outcome.as_ref().map(|o| {
        LastOutcome::new(
            o.failed.iter().cloned().collect(),
            o.skipped.iter().cloned().collect(),
            o.failed_source_tests.iter().cloned().collect(),
        )
    });
    Some(
        // Redacted here, before it leaves the CLI: option values such as `--vars` and
        // dbt's own options may carry secrets (AGENTS.md rule 9).
        LastRun::new(
            last.redacted(),
            format!("ods state {}", last.command),
            last.recorded_at,
            store_location(&path),
        )
        .with_outcome(outcome)
        .with_run(last.scope.clone(), last.run_id.clone())
        // Both exist in this ODS (`ods state retry`, #276, and `--failed`, #292);
        // `--failed` refuses a run that only tested, so it isn't offered then.
        // With placeholders for what the run withheld, which retry needs again (#321).
        .with_retry(
            Some(last.retry_line(false)),
            (last.command != "test").then(|| last.retry_line(true)),
        ),
    )
}

/// How many snapshots are read: the runs the State pages list, and the one before the
/// oldest. The store has no count query, so the Snapshots tile counts this far.
const SNAPSHOTS_READ: usize = RUNS_LISTED + 1;

/// How many history lines (summaries only) are read to tell which snapshot recorded a
/// node's last build (#313); older ones show the run without a snapshot.
pub(super) const RUNS_INDEXED: usize = 10_000;

/// The store as people read it: as given when relative, else relative to the working
/// directory or with `~` for the home directory; and in full.
fn store_location(path: &Path) -> StoreLocation {
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let shown = if path.is_relative() {
        path.to_owned()
    } else if let Some(relative) = std::env::current_dir()
        .ok()
        .and_then(|cwd| full.strip_prefix(cwd).ok().map(Path::to_owned))
    {
        relative
    } else if let Some(below_home) = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .and_then(|home| full.strip_prefix(home).ok().map(|p| Path::new("~").join(p)))
    {
        below_home
    } else {
        full.clone()
    };
    StoreLocation::new(shown.display().to_string(), full.display().to_string())
}

/// `python` → `Python`.
fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// What `ods` can do for a project today.
fn modules(recorded: bool) -> Vec<ModuleStatus> {
    vec![
        // Ready: this very server analyzed it.
        ModuleStatus::new("Lineage (column-level)", ModuleState::Ready, None),
        if recorded {
            ModuleStatus::new("State", ModuleState::Ready, None)
        } else {
            ModuleStatus::new(
                "State",
                ModuleState::NotSetUp,
                Some("record a first run".to_owned()),
            )
        },
        // Not checked for this project here: it works from the CLI.
        ModuleStatus::new("ERD", ModuleState::Available, Some("ods erd".to_owned())),
        ModuleStatus::new("Usage", ModuleState::Planned, None),
    ]
}
