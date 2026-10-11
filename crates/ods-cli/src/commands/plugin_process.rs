//! `[plugins.<name>]`: out-of-process plugins (ADR-0031 §5). Each is an executable
//! that speaks the plugin protocol. A command that uses plugins asks each one it may run
//! to describe itself, and adds what it describes to the plugin set: its health checks,
//! and its warehouse's source versions.
//!
//! **Trust** (ADR-0030 §4b): a plugin a project declares runs only once `ods health
//! trust` has recorded the digest of its command (and of the files its command names
//! in the directory it runs in), or for one run with `--allow-scripts`. One the user
//! declares in their own `config.toml` is trusted: the user wrote it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ods_config::{FileKind, Loaded, Source};
use ods_core::state::Timestamp;
use ods_health::Pinned;
use ods_health::trust::TrustStore;
use ods_provider_process::{AnsweredCatalogue, AnsweredLinker, Described, PluginProcess};
use ods_sdk::contracts::changes::ChangeProvider;
use ods_sdk::contracts::error_catalogue::{CatalogueInfo, ERROR_CATALOGUE};
use ods_sdk::contracts::health_check::CheckInfo;
use ods_sdk::contracts::observed_lineage::OBSERVED_LINEAGE_SOURCE;
use ods_sdk::contracts::relation_link::{NoRelationLink, RELATION_LINKER};
use ods_sdk::contracts::run_events::ErrorSummary;
use ods_sdk::protocol::LinkOutcome;
use sha2::{Digest as _, Sha256};

use crate::exit::{CliError, ExitStatus, codes};
use crate::plugins::process::ProcessWarehouse;
use crate::plugins::record::{self, PluginRecord};
use crate::plugins::{Origin, Plugins, WarehouseSettings};

thread_local! {
    /// Set while the MCP server runs a command: no plugin is started, and declared
    /// plugins answer only from their records (ADR-0031 §3d).
    static NO_PROCESSES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` so that no plugin is started: declared plugins answer only from what they
/// answered before, as recorded. The MCP server runs every command this way, since it
/// never starts a process (ADR-0009, ADR-0031 §3d).
pub(crate) fn without_processes<T>(f: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            NO_PROCESSES.set(self.0);
        }
    }
    let _reset = Reset(NO_PROCESSES.replace(true));
    f()
}

/// How long one call to a plugin may take unless its `timeout` says otherwise.
pub(super) const PLUGIN_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest `timeout` a plugin may have.
const LONGEST: Duration = Duration::from_secs(60 * 60);

/// A `[plugins.<name>]`, as configured.
#[derive(Debug, Clone)]
pub(super) struct Declared {
    /// Its name, the key of its table.
    pub(super) name: String,
    /// `plugin:<name>`: its id in the trust store, which no check id can be.
    id: String,
    /// Its command: its program, then its arguments.
    pub(super) command: Vec<String>,
    /// How long one call may take.
    pub(super) timeout: Duration,
    /// Where it runs: the directory of the project's `ods.toml`, or the current one.
    pub(super) root: PathBuf,
    /// Where a project declares it; `None` when the user's own configuration does.
    pub(super) project: Option<Declaration>,
    /// `sha256:` and the digest of what decides what runs: its command and the files it
    /// names in `root`.
    pub(super) digest: String,
}

/// The project file a plugin is declared in, and the directory trust is kept for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Declaration {
    pub(super) file: PathBuf,
    pub(super) root: String,
}

impl Pinned for Declared {
    fn id(&self) -> &str {
        &self.id
    }
    fn digest(&self) -> &str {
        &self.digest
    }
}

fn config_error(message: String) -> CliError {
    CliError::new(ExitStatus::Config, codes::HEALTH_CONFIG, message)
}

/// Every `[plugins.<name>]`, by name.
///
/// # Errors
/// A name, command or timeout that isn't valid (exit 4).
pub(super) fn declared(config: &Loaded) -> Result<Vec<Declared>, CliError> {
    let root = run_root(config);
    config
        .config
        .plugins
        .iter()
        .map(|(name, plugin)| {
            let at = format!("plugins.{name}");
            if !CheckInfo::valid_id(name) {
                return Err(config_error(format!(
                    "{at}: a plugin's name is lowercase letters, digits, `_`, `-` and `.`, starting with a letter"
                )));
            }
            if plugin.command.first().is_none_or(|p| p.trim().is_empty()) {
                return Err(config_error(format!(
                    "{at}.command: its program, then its arguments, e.g. [\"acme-ods-plugin\"]"
                )));
            }
            let timeout = match plugin.timeout.as_deref() {
                None => PLUGIN_TIMEOUT,
                Some(text) => {
                    let timeout = humantime::parse_duration(text).map_err(|e| {
                        config_error(format!(
                            "{at}.timeout: `{text}` isn't a duration ({e}), e.g. \"30s\" or \"2m\""
                        ))
                    })?;
                    if timeout.is_zero() || timeout > LONGEST {
                        return Err(config_error(format!(
                            "{at}.timeout: `{text}` must be more than 0 and at most 1h"
                        )));
                    }
                    timeout
                }
            };
            // What runs: its command and the files it names. Its timeout changes only
            // how long it may take, so, as for scripts, changing it needs no trust.
            let digested = serde_json::json!({
                "command": plugin.command,
                "files": ods_health::command_files(&plugin.command, &root),
            });
            let digest = format!(
                "sha256:{}",
                hex::encode(Sha256::digest(digested.to_string().as_bytes()))
            );
            Ok(Declared {
                name: name.clone(),
                id: format!("plugin:{name}"),
                command: plugin.command.clone(),
                timeout,
                root: root.clone(),
                project: declaration(config, name),
                digest,
            })
        })
        .collect()
}

/// Where plugins run: the directory of the project's `ods.toml`, or else the current
/// directory, absolute.
fn run_root(config: &Loaded) -> PathBuf {
    let dir = config
        .files
        .iter()
        .find(|f| f.kind == FileKind::Project && f.loaded)
        .and_then(|f| f.path.parent().map(Path::to_owned))
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."));
    std::path::absolute(&dir).unwrap_or(dir)
}

/// Where `[plugins.<name>]` is declared, unless it is the user's own configuration. A
/// setting from anywhere but a file (the environment) is the project's to answer for:
/// its directory is the current one.
fn declaration(config: &Loaded, name: &str) -> Option<Declaration> {
    let key = ["plugins", name, "command"].map(str::to_owned).to_vec();
    let file = match config.effective(&key).map(|s| &s.source) {
        Some(Source::File { kind, .. } | Source::Profile { kind, .. })
            if *kind == FileKind::User =>
        {
            return None;
        }
        Some(Source::File { path, .. } | Source::Profile { path, .. }) => path.clone(),
        _ => std::env::current_dir().unwrap_or_default().join("ods.toml"),
    };
    let dir = file.parent().unwrap_or(Path::new("."));
    let root = std::fs::canonicalize(dir)
        .unwrap_or_else(|_| dir.to_owned())
        .to_string_lossy()
        .into_owned();
    Some(Declaration { file, root })
}

/// The names of the plugins in `declared` that may run: the user's own, those the trust
/// store holds as they are now, or all of them with `allow` (this run only). A store
/// that can't be read trusts nothing: it never fails open (AGENTS rule 3).
pub(super) fn trusted(config: &Loaded, declared: &[Declared], allow: bool) -> BTreeSet<String> {
    let mut trusted = BTreeSet::new();
    let mut store: Option<Option<TrustStore>> = None;
    for plugin in declared {
        let ok = match &plugin.project {
            None => true,
            Some(_) if allow => true,
            Some(declaration) => {
                let store = store.get_or_insert_with(|| {
                    let path = super::health_trust::store_path(config)?;
                    TrustStore::read(&path)
                        .inspect_err(|e| tracing::warn!(error = %e, "plugins aren't trusted"))
                        .ok()
                });
                store.as_ref().is_some_and(|s| {
                    s.trusted(&declaration.root, std::slice::from_ref(plugin))
                        .contains(&plugin.id)
                })
            }
        };
        if ok {
            trusted.insert(plugin.name.clone());
        }
    }
    trusted
}

/// A plugin that couldn't be used, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Held {
    pub(super) name: String,
    pub(super) why: String,
}

impl Held {
    /// It as a warning, for commands that go on without it.
    pub(super) fn warning(&self) -> String {
        format!("the plugin `{}` wasn't used: {}", self.name, self.why)
    }

    /// It as an error, for commands that can't go on without it.
    pub(super) fn error(&self) -> CliError {
        CliError::new(
            ExitStatus::Failure,
            codes::PLUGIN_UNAVAILABLE,
            format!("the plugin `{}` can't be used: {}", self.name, self.why),
        )
        .with_hint(if self.why.contains("isn't trusted") {
            "review it with `ods health trust`, which trusts it; or pass --allow-scripts for this run only"
        } else {
            "`ods plugin show` describes it; `ods plugin test` checks it"
        })
    }
}

/// A plugin that described itself.
#[derive(Debug, Clone)]
pub(super) struct Running {
    pub(super) declared: Declared,
    pub(super) described: Described,
    pub(super) process: Arc<PluginProcess>,
    /// Its warehouse's settings, when it serves one.
    pub(super) settings: BTreeMap<String, serde_json::Value>,
}

impl Running {
    /// The warehouse kind it serves, if it serves one.
    pub(super) fn warehouse(&self) -> Option<&str> {
        self.described
            .answer
            .warehouse
            .as_deref()
            .filter(|w| !w.is_empty())
    }
}

/// The plugin set with the declared plugins that may run added, as each describes
/// itself, and those that can't run.
#[derive(Debug, Clone)]
pub(super) struct WithDeclared {
    pub(super) plugins: Plugins,
    pub(super) running: Vec<Running>,
    pub(super) held: Vec<Held>,
    /// The command of each held plugin, by name.
    pub(super) held_commands: BTreeMap<String, Vec<String>>,
}

/// The installed plugins, and `config`'s declared plugins: each that may run (see
/// [`trusted`]) is asked to describe itself, and its checks (given the project's
/// `artifacts`) and warehouse are added. One that isn't trusted, can't start or doesn't
/// describe itself as the protocol says is held, with why, and adds nothing.
///
/// # Errors
/// A declaration isn't valid; a plugin's check has the id of another plugin's; or its
/// warehouse is served by another plugin (exit 4).
pub(super) async fn with_declared(
    config: &Loaded,
    allow: bool,
    artifacts: &BTreeMap<String, String>,
) -> Result<WithDeclared, CliError> {
    let mut plugins = crate::plugins::installed().clone();
    let mut running = Vec::new();
    let mut held = Vec::new();
    let mut held_commands = BTreeMap::new();
    let declared = declared(config)?;
    let trusted = trusted(config, &declared, allow);
    let no_processes = NO_PROCESSES.get();
    for plugin in declared {
        if no_processes {
            held_commands.insert(plugin.name.clone(), plugin.command.clone());
            held.push(Held {
                name: plugin.name.clone(),
                why: "the MCP server never starts a plugin: what it last answered, as recorded, is used".to_owned(),
            });
            continue;
        }
        if !trusted.contains(&plugin.name) {
            held_commands.insert(plugin.name.clone(), plugin.command.clone());
            held.push(Held {
                name: plugin.name.clone(),
                why: format!(
                    "{} declares it, and it isn't trusted as it is now",
                    plugin
                        .project
                        .as_ref()
                        .map_or_else(String::new, |d| d.file.display().to_string())
                ),
            });
            continue;
        }
        let process = Arc::new(PluginProcess::new(
            &plugin.name,
            plugin.command.clone(),
            &plugin.root,
            plugin.timeout,
        ));
        let described = match process.describe().await {
            Ok(described) => described,
            Err(e) => {
                held_commands.insert(plugin.name.clone(), plugin.command.clone());
                held.push(Held {
                    name: plugin.name.clone(),
                    why: e.to_string(),
                });
                continue;
            }
        };
        let settings = described
            .answer
            .warehouse
            .as_deref()
            .map(|w| warehouse_settings(config, w))
            .unwrap_or_default();
        let plugin = Running {
            declared: plugin,
            described,
            process,
            settings,
        };
        add(&mut plugins, &plugin, artifacts)?;
        running.push(plugin);
    }
    Ok(WithDeclared {
        plugins,
        running,
        held,
        held_commands,
    })
}

/// The `settings` of each `[providers.<name>]` of kind `warehouse`, by name, as a
/// plugin's `versions` request carries them: secret references left unresolved.
pub(super) fn warehouse_settings(
    config: &Loaded,
    warehouse: &str,
) -> BTreeMap<String, serde_json::Value> {
    WarehouseSettings::from_config(&config.config, warehouse)
        .providers()
        .iter()
        .map(|(name, table)| {
            (
                name.clone(),
                serde_json::to_value(table).unwrap_or(serde_json::Value::Null),
            )
        })
        .collect()
}

/// Where a plugin comes from, for people: `<its name> (plugins.<name>) <its version>`.
fn origin_of(plugin: &str, name: &str, version: &str) -> Origin {
    let name = if name.trim().is_empty() {
        plugin
    } else {
        name.trim()
    };
    Origin {
        name: format!("{name} (plugins.{plugin})").into(),
        version: if version.trim().is_empty() {
            "(no version)".into()
        } else {
            version.trim().to_owned().into()
        },
    }
}

/// The warehouse `plugin` serves, as it described it, with what `record` holds when the
/// record is this plugin's, for this warehouse.
fn warehouse_of(plugin: &Running, record: Option<&PluginRecord>) -> Option<ProcessWarehouse> {
    let warehouse = plugin.warehouse()?;
    let described = &plugin.described;
    let answer = &described.answer;
    let record = record.filter(|r| r.plugin == plugin.declared.name && r.warehouse == warehouse);
    Some(ProcessWarehouse {
        origin: origin_of(&plugin.declared.name, &answer.name, &answer.version),
        warehouse: warehouse.to_owned(),
        changes: plugin
            .process
            .versions(described, plugin.settings.clone())
            .map(|v| Arc::new(v) as Arc<dyn ChangeProvider>),
        versions_read: answer.versions_read.clone(),
        dialect: answer.dialect.clone(),
        offers_links: described.provides(&RELATION_LINKER),
        links: record.and_then(recorded_links),
        errors: answer
            .catalogue
            .clone()
            .filter(|_| described.provides(&ERROR_CATALOGUE))
            .map(|catalogue| recorded_catalogue(&plugin.declared.name, catalogue, record)),
        lineage: described
            .provides(&OBSERVED_LINEAGE_SOURCE)
            .then(|| (plugin.process.clone(), plugin.settings.clone())),
        plugin: plugin.declared.name.clone(),
    })
}

/// The links `record` holds, once a command has asked for them.
fn recorded_links(record: &PluginRecord) -> Option<Arc<AnsweredLinker>> {
    record.links_at?;
    let links = record
        .links
        .iter()
        .map(|(relation, outcome)| {
            let link = match outcome {
                LinkOutcome::Link(link) => Ok(link.clone()),
                LinkOutcome::NoLink(why) => Err(why.clone()),
                _ => Err(NoRelationLink::NotAnswered {
                    plugin: record.plugin.clone(),
                    why: "its recorded answer is of a kind this ods doesn't know".to_owned(),
                }),
            };
            (relation.clone(), link)
        })
        .collect();
    Some(Arc::new(AnsweredLinker::new(record.plugin.clone(), links)))
}

/// `catalogue`, answering what `record` holds.
fn recorded_catalogue(
    plugin: &str,
    catalogue: CatalogueInfo,
    record: Option<&PluginRecord>,
) -> Arc<AnsweredCatalogue> {
    let answers = record
        .map(|r| {
            r.classifications
                .iter()
                .map(|c| (c.error.clone(), c.classification.clone()))
                .collect()
        })
        .unwrap_or_default();
    Arc::new(AnsweredCatalogue::new(plugin, catalogue, answers))
}

/// Adds what `plugin` offers to `plugins`: its checks (given the project's `artifacts`)
/// and its warehouse.
fn add(
    plugins: &mut Plugins,
    plugin: &Running,
    artifacts: &BTreeMap<String, String>,
) -> Result<(), CliError> {
    let answer = &plugin.described.answer;
    let origin = origin_of(&plugin.declared.name, &answer.name, &answer.version);
    let refused = |e: crate::plugins::PluginError| {
        config_error(format!("plugins.{}: {e}", plugin.declared.name))
    };
    for check in plugin.process.checks(&plugin.described, artifacts) {
        plugins
            .add_health_check(origin.clone(), Arc::new(check))
            .map_err(refused)?;
    }
    if let Some(warehouse) = warehouse_of(plugin, None) {
        plugins
            .add_warehouse(Arc::new(warehouse))
            .map_err(refused)?;
    }
    Ok(())
}

/// A project's plugins as a CLI command that runs them sees them: the installed ones,
/// the declared ones that described themselves, and the answers recorded for the
/// project's scope (ADR-0031 §3d), which this keeps up to date as it asks for more.
pub(super) struct ProjectPlugins {
    declared: WithDeclared,
    /// The installed plugins and the running ones' warehouses, before any record.
    base: Plugins,
    /// The names `[plugins]` declares: a record of another plugin is ignored.
    names: BTreeSet<String>,
    /// Where the record is kept, and the scope it is for; `None` without a state store,
    /// when answers are used but not kept.
    record_at: Option<(PathBuf, String)>,
    /// The record, as last read or written.
    record: Option<PluginRecord>,
    /// What didn't work, for the command's warnings.
    pub(super) warnings: Vec<String>,
}

impl ProjectPlugins {
    /// `declared`, with the answers recorded for `scope` beside the state store at
    /// `state_db`, when there is one.
    pub(super) fn new(
        config: &Loaded,
        declared: WithDeclared,
        state: Option<(&Path, &str)>,
    ) -> Self {
        let mut this = Self {
            warnings: declared
                .held
                .iter()
                .filter(|h| !NO_PROCESSES.get() || h.why.contains("isn't trusted"))
                .map(Held::warning)
                .collect(),
            base: declared.plugins.clone(),
            declared,
            names: config.config.plugins.keys().cloned().collect(),
            record_at: None,
            record: None,
        };
        if let Some((state_db, scope)) = state {
            this.attach(state_db, scope);
        }
        this
    }

    /// Reads, and from now on keeps, the answers recorded for `scope` beside the state
    /// store at `state_db`.
    pub(super) fn attach(&mut self, state_db: &Path, scope: &str) {
        let dir = record::dir_for(state_db);
        self.record = record::read(&dir, scope);
        self.record_at = Some((dir, scope.to_owned()));
        self.rebuild();
    }

    /// The plugin set.
    pub(super) fn plugins(&self) -> &Plugins {
        &self.declared.plugins
    }

    /// The declared plugins that described themselves, and those that couldn't run.
    pub(super) fn declared(&self) -> &WithDeclared {
        &self.declared
    }

    /// The probes of the plugin `name`, over its own connection, for a project on
    /// `adapter`; or why there are none.
    ///
    /// # Errors
    /// It isn't running here, serves another warehouse, or doesn't probe.
    pub(super) fn probe(
        &self,
        name: &str,
        adapter: Option<&str>,
    ) -> Result<ods_provider_process::PluginProbe, String> {
        let plugin = self
            .declared
            .running
            .iter()
            .find(|p| p.declared.name == name)
            .ok_or_else(|| format!("`{name}` isn't a `[plugins.<name>]` that runs here"))?;
        if plugin.warehouse() != adapter {
            return Err(format!(
                "`{name}` serves the warehouse `{}`, and this project's is `{}`",
                plugin.warehouse().unwrap_or("none"),
                adapter.unwrap_or("unknown")
            ));
        }
        plugin
            .process
            .probe(&plugin.described, plugin.settings.clone())
            .ok_or_else(|| format!("`{name}` doesn't implement `relation_probe`"))
    }

    /// The running plugin that serves `adapter`, if one does.
    fn serving(&self, adapter: Option<&str>) -> Option<Running> {
        let adapter = adapter?;
        self.declared
            .running
            .iter()
            .find(|p| p.warehouse() == Some(adapter))
            .cloned()
    }

    /// Each running plugin's warehouse again, with what the record now holds; and a
    /// recorded plugin that isn't running here (the MCP server never starts one) from
    /// its record alone.
    fn rebuild(&mut self) {
        let mut plugins = self.base.clone();
        for plugin in &self.declared.running {
            if let Some(warehouse) = warehouse_of(plugin, self.record.as_ref())
                && let Err(e) = plugins.replace_warehouse(Arc::new(warehouse))
            {
                tracing::warn!(plugin = %plugin.declared.name, "{e}");
            }
        }
        if let Some(record) = &self.record
            && !self
                .declared
                .running
                .iter()
                .any(|p| p.declared.name == record.plugin)
        {
            add_recorded(&mut plugins, &self.names, record);
        }
        self.declared.plugins = plugins;
    }

    /// The record to add `plugin`'s answers to: the one read when it is this plugin's
    /// for its warehouse, else a new one.
    fn record_for(&self, plugin: &Running) -> Option<PluginRecord> {
        let warehouse = plugin.warehouse()?;
        let answer = &plugin.described.answer;
        let mut record = match &self.record {
            Some(r) if r.plugin == plugin.declared.name && r.warehouse == warehouse => r.clone(),
            _ => PluginRecord::new(
                self.record_at
                    .as_ref()
                    .map(|(_, scope)| scope.clone())
                    .unwrap_or_default(),
                plugin.declared.name.clone(),
                (answer.name.clone(), answer.version.clone()),
                warehouse,
            ),
        };
        answer.name.clone_into(&mut record.name);
        answer.version.clone_into(&mut record.version);
        record.dialect.clone_from(&answer.dialect);
        record.catalogue.clone_from(&answer.catalogue);
        Some(record)
    }

    /// Keeps `record`, beside the state store when there is one, and serves from it.
    fn keep(&mut self, record: PluginRecord) {
        if let Some((dir, _)) = &self.record_at
            && let Err(e) = record::write(dir, &record)
        {
            self.warnings.push(format!(
                "the plugin `{}`'s answers can't be kept in {} for the dashboard: {e}",
                record.plugin,
                dir.display()
            ));
        }
        self.record = Some(record);
        self.rebuild();
    }

    /// Asks the plugin that serves `adapter` for the links of `relations`, in one call,
    /// and keeps them. A failed call is a warning, and no relation has a link.
    pub(super) async fn ask_links(&mut self, adapter: Option<&str>, relations: &[String]) {
        let Some(plugin) = self
            .serving(adapter)
            .filter(|p| p.described.provides(&RELATION_LINKER))
        else {
            return;
        };
        let Some(mut record) = self.record_for(&plugin) else {
            return;
        };
        let links = match plugin
            .process
            .links(relations, plugin.settings.clone())
            .await
        {
            Ok(links) => links,
            Err(e) => {
                self.warnings.push(format!(
                    "the plugin `{}` gave no links: {e}",
                    plugin.declared.name
                ));
                let why = NoRelationLink::NotAnswered {
                    plugin: plugin.declared.name.clone(),
                    why: format!("its call failed: {e}"),
                };
                relations
                    .iter()
                    .map(|r| (r.clone(), Err(why.clone())))
                    .collect()
            }
        };
        record.links = links
            .into_iter()
            .map(|(relation, link)| {
                let outcome = match link {
                    Ok(link) => LinkOutcome::Link(link),
                    Err(why) => LinkOutcome::NoLink(why),
                };
                (relation, outcome)
            })
            .collect();
        record.links_at = Some(Timestamp::now());
        self.keep(record);
    }

    /// Asks the plugin that serves `adapter` to classify those of `errors` it hasn't
    /// yet, in one call, and keeps what it says. A failed call is a warning: dbt's
    /// catalogue explains them instead.
    pub(super) async fn ask_classifications(
        &mut self,
        adapter: Option<&str>,
        errors: &[ErrorSummary],
    ) {
        let Some(plugin) = self
            .serving(adapter)
            .filter(|p| p.described.provides(&ERROR_CATALOGUE))
        else {
            return;
        };
        let Some(mut record) = self.record_for(&plugin) else {
            return;
        };
        let mut asked: Vec<ErrorSummary> = Vec::new();
        for error in errors {
            if !record.classified(error) && !asked.contains(error) {
                asked.push(error.clone());
            }
        }
        if asked.is_empty() {
            return;
        }
        match plugin.process.classify(&asked).await {
            Ok(classifications) => {
                record
                    .add_classifications(asked.into_iter().zip(classifications), Timestamp::now());
                self.keep(record);
            }
            Err(e) => self.warnings.push(format!(
                "the plugin `{}` didn't explain the errors: {e}",
                plugin.declared.name
            )),
        }
    }
}

/// The installed plugins, with the warehouse a declared plugin last answered for, as
/// recorded beside the state store at `state_db` for `scope` (ADR-0031 §3d): what the
/// dashboard, which never starts a plugin, uses.
pub(super) fn recorded(config: &Loaded, state_db: &Path, scope: &str) -> Plugins {
    let mut plugins = crate::plugins::installed().clone();
    if let Some(record) = record::read(&record::dir_for(state_db), scope) {
        let names = config.config.plugins.keys().cloned().collect();
        add_recorded(&mut plugins, &names, &record);
    }
    plugins
}

/// The installed plugins, with each warehouse a declared plugin answered for in any
/// scope beside the state store at `state_db`: for what doesn't depend on the scope,
/// such as the dialect the dashboard reads the project's SQL in before it knows the
/// scope (ADR-0031 §3d).
pub(super) fn recorded_in_any_scope(config: &Loaded, state_db: &Path) -> Plugins {
    let mut plugins = crate::plugins::installed().clone();
    if config.config.plugins.is_empty() {
        return plugins;
    }
    let names = config.config.plugins.keys().cloned().collect();
    for record in record::read_all(&record::dir_for(state_db)) {
        add_recorded(&mut plugins, &names, &record);
    }
    plugins
}

/// Adds the warehouse `record` describes to `plugins`, from the record alone: its links,
/// classifications and dialect. A record of a plugin not among `names` (no longer
/// declared), or for a warehouse another plugin serves, is ignored.
fn add_recorded(plugins: &mut Plugins, names: &BTreeSet<String>, record: &PluginRecord) {
    if !names.contains(&record.plugin) || plugins.warehouse(Some(&record.warehouse)).is_some() {
        return;
    }
    let warehouse = ProcessWarehouse {
        origin: origin_of(&record.plugin, &record.name, &record.version),
        warehouse: record.warehouse.clone(),
        changes: None,
        versions_read: None,
        dialect: record.dialect.clone(),
        offers_links: record.links_at.is_some(),
        links: recorded_links(record),
        errors: record
            .catalogue
            .clone()
            .map(|c| recorded_catalogue(&record.plugin, c, Some(record))),
        lineage: None,
        plugin: record.plugin.clone(),
    };
    if let Err(e) = plugins.add_warehouse(Arc::new(warehouse)) {
        tracing::warn!(plugin = %record.plugin, "its recorded answers aren't used: {e}");
    }
}

/// A project's plugins for a CLI command (see [`ProjectPlugins`]), with the answers
/// recorded for `state`'s scope when given; `None` when the configuration declares no
/// plugin, so commands without any start nothing. `allow` is `--allow-scripts`.
///
/// # Errors
/// A declaration isn't valid, or a plugin's warehouse is another plugin's (exit 4).
pub(super) fn for_project(
    config: &Loaded,
    allow: bool,
    artifacts: &BTreeMap<String, String>,
    state: Option<(&Path, &str)>,
) -> Result<Option<ProjectPlugins>, CliError> {
    if config.config.plugins.is_empty() {
        return Ok(None);
    }
    let declared = super::state_plan::block_on(with_declared(config, allow, artifacts))??;
    Ok(Some(ProjectPlugins::new(config, declared, state)))
}

/// The plugins of `project`, else the installed ones.
pub(super) fn plugins_of(project: Option<&ProjectPlugins>) -> &Plugins {
    project.map_or_else(|| crate::plugins::installed(), ProjectPlugins::plugins)
}

/// The relation of every node `manifest` names one for, each once: what a warehouse
/// plugin is asked for links to.
pub(super) fn manifest_relations(manifest: &ods_provider_dbt::Manifest) -> Vec<String> {
    let relations: BTreeSet<String> = manifest
        .nodes
        .iter()
        .filter_map(|n| n.relation_name.clone())
        .collect();
    relations.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ods_sdk::contracts::relation_link::RelationLink;

    fn loaded(dir: &Path, toml: &str) -> Loaded {
        let file = dir.join("ods.toml");
        std::fs::write(&file, toml).unwrap();
        ods_config::load(&ods_config::Inputs {
            project_file: Some(file),
            ..ods_config::Inputs::default()
        })
        .unwrap()
    }

    /// The MCP server never starts a plugin: a declared one answers from its record.
    #[test]
    fn without_processes_a_declared_plugin_answers_only_from_its_record() {
        let dir = tempfile::tempdir().unwrap();
        // A program that doesn't exist: starting it would fail differently.
        let config = loaded(
            dir.path(),
            "[plugins.acme]\ncommand = [\"ods-no-such-plugin-anywhere\"]\n",
        );
        let db = dir.path().join("state.db");
        let mut record = PluginRecord::new("shop/dev", "acme", ("acme", "1.2.0"), "acme_db");
        record.links.insert(
            "\"shop\".\"main\".\"orders\"".to_owned(),
            LinkOutcome::Link(RelationLink::new(
                "https://acme.example/explore/shop/main/orders",
                "Open in Acme",
            )),
        );
        record.links_at = Some(Timestamp::now());
        record::write(&record::dir_for(&db), &record).unwrap();

        let project = without_processes(|| {
            for_project(&config, true, &BTreeMap::new(), Some((&db, "shop/dev")))
                .unwrap()
                .unwrap()
        });
        let held = &project.declared().held[0];
        assert!(
            held.why.contains("MCP server never starts a plugin"),
            "{held:?}"
        );
        let linker = project
            .plugins()
            .links(Some("acme_db"), &crate::plugins::WarehouseSettings::empty())
            .unwrap();
        assert_eq!(
            linker.link("\"shop\".\"main\".\"orders\"").unwrap().url,
            "https://acme.example/explore/shop/main/orders"
        );
        // Outside the MCP server, the same declaration is started (and fails here).
        let project = for_project(&config, true, &BTreeMap::new(), None)
            .unwrap()
            .unwrap();
        assert!(
            project.declared().held[0]
                .why
                .contains("couldn't be started"),
            "{:?}",
            project.declared().held
        );
    }
}
