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
use ods_health::Pinned;
use ods_health::trust::TrustStore;
use ods_provider_process::{Described, PluginProcess};
use ods_sdk::contracts::changes::ChangeProvider;
use ods_sdk::contracts::health_check::CheckInfo;
use sha2::{Digest as _, Sha256};

use crate::exit::{CliError, ExitStatus, codes};
use crate::plugins::process::ProcessWarehouse;
use crate::plugins::{Origin, Plugins, WarehouseSettings};

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
    for plugin in declared {
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
        add(
            &mut plugins,
            config,
            &plugin,
            &process,
            &described,
            artifacts,
        )?;
        running.push(Running {
            declared: plugin,
            described,
        });
    }
    Ok(WithDeclared {
        plugins,
        running,
        held,
        held_commands,
    })
}

/// Adds what `described` offers to `plugins`.
fn add(
    plugins: &mut Plugins,
    config: &Loaded,
    plugin: &Declared,
    process: &Arc<PluginProcess>,
    described: &Described,
    artifacts: &BTreeMap<String, String>,
) -> Result<(), CliError> {
    let answer = &described.answer;
    let origin = Origin {
        name: format!(
            "{} (plugins.{})",
            if answer.name.trim().is_empty() {
                &plugin.name
            } else {
                answer.name.trim()
            },
            plugin.name
        )
        .into(),
        version: if answer.version.trim().is_empty() {
            "(no version)".into()
        } else {
            answer.version.trim().to_owned().into()
        },
    };
    let refused =
        |e: crate::plugins::PluginError| config_error(format!("plugins.{}: {e}", plugin.name));
    for check in process.checks(described, artifacts) {
        plugins
            .add_health_check(origin.clone(), Arc::new(check))
            .map_err(refused)?;
    }
    if let Some(warehouse) = answer.warehouse.as_deref().filter(|w| !w.is_empty()) {
        let settings = WarehouseSettings::from_config(&config.config, warehouse)
            .providers()
            .iter()
            .map(|(name, table)| {
                (
                    name.clone(),
                    serde_json::to_value(table).unwrap_or(serde_json::Value::Null),
                )
            })
            .collect();
        let changes = process
            .versions(described, settings)
            .map(|v| Arc::new(v) as Arc<dyn ChangeProvider>);
        plugins
            .add_warehouse(Arc::new(ProcessWarehouse {
                origin,
                warehouse: warehouse.to_owned(),
                changes,
                versions_read: answer.versions_read.clone(),
            }))
            .map_err(refused)?;
    }
    Ok(())
}

/// The plugin set for the commands that read source versions: the installed plugins,
/// and the declared plugins that may run, as each describes itself. One that can't run
/// is a warning: without its versions, sources are judged by `sources.json` alone, which
/// only ever costs a rebuild (AGENTS rule 3).
///
/// # Errors
/// A declaration isn't valid, or a plugin's warehouse is another plugin's (exit 4).
pub(super) fn for_warehouses(
    config: &Loaded,
    allow: bool,
) -> Result<(Plugins, Vec<String>), CliError> {
    if config.config.plugins.is_empty() {
        return Ok((crate::plugins::installed().clone(), Vec::new()));
    }
    let declared = super::state_plan::block_on(with_declared(config, allow, &BTreeMap::new()))??;
    let warnings = declared.held.iter().map(Held::warning).collect();
    Ok((declared.plugins, warnings))
}
