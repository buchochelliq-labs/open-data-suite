//! What `ods serve`'s dashboard shows (#310): the project, its state store and the plan,
//! read here, where the providers are, and handed to `ods-web` as neutral facts
//! (ADR-0001, ADR-0009).
//!
//! Everything is read-only: the state store is opened without migrating or creating it,
//! and planning runs nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Arg, ArgMatches, Command};
use ods_core::state::Timestamp;
use ods_lineage::GraphFilter;
use ods_sdk::contracts::state_store::StateStore;
use ods_store_sqlite::SqliteStateStore;
use ods_web::dashboard::{
    ModuleState, ModuleStatus, OpaqueNode, RECENT_RUNS, Recorded, RunRecord, StateInput,
    StoreLocation, Target,
};
use ods_web::{Dashboard, Snapshot};

use super::lineage::Loaded;
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

/// Reads the dashboard's facts; built once, then asked again on every reload.
pub(super) struct DashboardSource {
    args: ArgMatches,
    settings: StateSettings,
}

impl DashboardSource {
    pub(super) fn new(args: &ArgMatches, config: &ods_config::Loaded) -> Result<Self, CliError> {
        Ok(Self {
            args: args.clone(),
            settings: StateSettings::resolve(args, config)?,
        })
    }

    /// Files whose change means new state: the database and its write-ahead log, where
    /// a commit lands first.
    pub(super) fn watched(&self) -> Vec<PathBuf> {
        let db = self.settings.state_db();
        let mut wal = db.clone().into_os_string();
        wal.push("-wal");
        vec![db, PathBuf::from(wal)]
    }

    /// The server's snapshot of `loaded`, with the dashboard's facts.
    pub(super) fn snapshot(&self, loaded: &Loaded, source: String) -> Snapshot {
        let document = loaded
            .graph
            .document(&|id| loaded.node_name(id), &GraphFilter::default());
        let opaque_ids: Vec<(String, String, Option<String>)> = document
            .nodes
            .iter()
            .filter(|n| n.opaque)
            .map(|n| (n.id.clone(), n.name.clone(), n.diagnostics.first().cloned()))
            .collect();
        let dashboard = self.dashboard(&opaque_ids);
        Snapshot::new(document, loaded.graph.clone(), source).with_dashboard(dashboard)
    }

    /// Never fails: what can't be read is shown as such.
    fn dashboard(&self, opaque: &[(String, String, Option<String>)]) -> Dashboard {
        let environment = self.settings.environment.value.clone();
        let ws = match Workspace::load(&self.args, &self.settings, Sources::AsGiven) {
            Ok(ws) => ws,
            Err(e) => {
                return Dashboard::new("project", environment)
                    .with_state(StateInput::Unreadable {
                        store: store_location(&self.settings.state_db()),
                        error: e.message,
                    })
                    .with_modules(modules(false));
            }
        };
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
        let state = self.state(&ws);
        let recorded = matches!(&state, StateInput::Recorded(r) if !r.runs.is_empty());
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
    }

    fn state(&self, ws: &Workspace) -> StateInput {
        let store = store_location(&ws.state_db);
        // Never created here: the dashboard only reads (AGENTS rule 5, #310).
        if !ws.state_db.is_file() {
            return StateInput::NoStore { store };
        }
        let unreadable = |error: String| StateInput::Unreadable {
            store: store.clone(),
            error,
        };
        let read = block_on(async {
            let db = SqliteStateStore::open_existing(&ws.state_db).await?;
            let history = db.history(&ws.scope, usize::MAX).await?;
            let mut runs = Vec::new();
            for summary in history.iter().take(RECENT_RUNS) {
                if let Some(stored) = db.get(&ws.scope, summary.id).await? {
                    runs.push(RunRecord::of(stored.id.0, &stored.snapshot));
                }
            }
            let latest = db.latest(&ws.scope).await?;
            db.close().await;
            Ok::<_, ods_sdk::ProviderError>((history.len(), runs, latest))
        });
        let (snapshots, runs, latest) = match read {
            Ok(Ok(read)) => read,
            Ok(Err(e)) => return unreadable(e.to_string()),
            Err(e) => return unreadable(e.message),
        };
        let planned = plan_latest(ws, &self.settings, latest, &[], Timestamp::now());
        let (plan, warnings) = match planned {
            Ok(Planned { plan, warnings, .. }) => (Ok(plan), warnings),
            Err(e) => (Err(e.message), Vec::new()),
        };
        StateInput::Recorded(Box::new(
            Recorded::new(store, runs, snapshots, plan).with_warnings(warnings),
        ))
    }
}

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
        ModuleStatus::new("ERD", ModuleState::Ready, Some("ods erd".to_owned())),
        ModuleStatus::new("Usage", ModuleState::Planned, None),
    ]
}
