//! Trust for `[[health.checks]]` probes and scripts (ADR-0030 §4b, §4d): where the
//! definitions come from, which of them the user trusted for this project, and
//! `ods health trust`, which shows them and records the current ones.
//!
//! A probe sends SQL to the warehouse and a script runs a command, and a repository's
//! `ods.toml` can name either, so one a project defines runs only once the user has
//! trusted that exact definition for that project. Those defined in the user's own
//! `config.toml` need no trust: the user wrote them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_config::{FileKind, Loaded, Source};
use ods_core::state::Timestamp;
use ods_health::trust::{self, Standing, TrustStore};
use ods_health::{CheckDefinition, HealthSettings, Pinned, ProbeDefinition, ScriptDefinition};
use serde::Serialize;

use super::plugin_process::{Declaration, Declared};
use crate::exit::{CliError, ExitStatus, codes};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// Where the probes and scripts come from, and so whether they need trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Origin {
    /// No `[[health.checks]]` at all.
    None,
    /// The user's own configuration: trusted without an entry.
    User(PathBuf),
    /// A project (or local) file: trusted per definition, under the canonical path of
    /// the directory the file is in.
    Project { file: PathBuf, root: String },
}

impl Origin {
    /// Where `config`'s probes come from. The `[[health.checks]]` array is one setting:
    /// a later layer replaces it whole, so its source names the file every check in it
    /// is from. `[health.probes]` says where they run, so a project file that sets it
    /// makes even the user's own probes the project's to answer for: a repository can't
    /// point them at another target without the user trusting that.
    pub(super) fn of(config: &Loaded) -> Self {
        Self::resolve(config, true)
    }

    /// Where `config`'s scripts come from: where `[[health.checks]]` does. `[health.probes]`
    /// changes nothing about how a script runs, so it doesn't make the user's own scripts
    /// the project's.
    fn of_scripts(config: &Loaded) -> Self {
        Self::resolve(config, false)
    }

    fn resolve(config: &Loaded, probes: bool) -> Self {
        let source = |key: &[&str]| {
            let key: Vec<String> = key.iter().map(|k| (*k).to_owned()).collect();
            config.effective(&key).map(|s| s.source.clone())
        };
        let Some(checks) = source(&["health", "checks"]) else {
            return Self::None;
        };
        let from_project = [
            ["health", "probes", "target"],
            ["health", "probes", "profile"],
        ]
        .iter()
        .filter(|_| probes)
        .filter_map(|key| source(key))
        .find(|s| file_kind(s) != Some(FileKind::User));
        let source = match (file_kind(&checks), from_project) {
            (Some(FileKind::User), None) => return Self::User(file_path(&checks)),
            (Some(FileKind::User), Some(project)) => project,
            _ => checks,
        };
        let file = match file_kind(&source) {
            Some(_) => file_path(&source),
            // Anything but a file is the project's to answer for: the current directory.
            None => std::env::current_dir().unwrap_or_default().join("ods.toml"),
        };
        let dir = file.parent().unwrap_or(Path::new("."));
        let root = std::fs::canonicalize(dir)
            .unwrap_or_else(|_| dir.to_owned())
            .to_string_lossy()
            .into_owned();
        Self::Project { file, root }
    }
}

/// The kind of file a setting comes from; `None` when it isn't from a file.
fn file_kind(source: &Source) -> Option<FileKind> {
    match source {
        Source::File { kind, .. } | Source::Profile { kind, .. } => Some(*kind),
        _ => None,
    }
}

/// The file a setting comes from; empty when it isn't from a file.
fn file_path(source: &Source) -> PathBuf {
    match source {
        Source::File { path, .. } | Source::Profile { path, .. } => path.clone(),
        _ => PathBuf::new(),
    }
}

/// Where the trust store is: beside the user's own `config.toml`.
pub(super) fn store_path(config: &Loaded) -> Option<PathBuf> {
    config
        .files
        .iter()
        .find(|f| f.kind == FileKind::User)
        .map(|f| trust::path_beside(&f.path))
}

/// Marks the probes and scripts in `health` trusted as `config`'s origin and the trust
/// store say, or all of them with `allow_scripts` (this run only). A store that can't be
/// read trusts nothing: it never fails open (AGENTS rule 3).
pub(super) fn apply(health: &mut HealthSettings, config: &Loaded, allow_scripts: bool) {
    let definitions = health.trust_definitions();
    if definitions.is_empty() {
        return;
    }
    let all = || definitions.iter().map(|d| d.id().to_owned()).collect();
    let mut trusted: BTreeSet<String> = match Origin::of(config) {
        _ if allow_scripts => all(),
        Origin::User(_) => all(),
        Origin::None => BTreeSet::new(),
        Origin::Project { root, .. } => match store_path(config).map(|p| TrustStore::read(&p)) {
            Some(Ok(store)) => store.trusted(&root, &definitions),
            Some(Err(e)) => {
                tracing::warn!(error = %e, "health: probes and scripts aren't trusted");
                BTreeSet::new()
            }
            None => BTreeSet::new(),
        },
    };
    if matches!(Origin::of_scripts(config), Origin::User(_)) {
        trusted.extend(health.script_definitions().into_iter().map(|d| d.id));
    }
    health.trust_checks(&trusted);
}

/// `ods health trust`.
pub(super) fn command() -> Command {
    Command::new("trust")
        .about("Review this project's probe and script checks and trust them to run (ADR-0030 §4b)")
        .long_about(
            "Lists the probe and script checks this project's ods.toml defines, with each \
             probe's SQL and each script's command, and whether each is new, changed or \
             already trusted, then trusts them as they are now. A probe or script runs \
             only once its exact definition is trusted for this project; changing its \
             SQL, its command, a file its command names or what it selects makes it \
             untrusted again. Trust is kept \
             in your own configuration directory, never in the repository.",
        )
        .arg(
            Arg::new("revoke")
                .long("revoke")
                .action(ArgAction::SetTrue)
                .help("Forget this project's trusted checks instead"),
        )
}

/// One probe, as `ods health trust` shows it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Reviewed {
    id: String,
    /// How it stood before this run.
    standing: Standing,
    /// Its query, or a table of them by warehouse kind (ADR-0031 §3b), as configured.
    sql: ods_config::ProbeSql,
    digest: String,
}

/// One script, as `ods health trust` shows it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ReviewedScript {
    id: String,
    /// How it stood before this run.
    standing: Standing,
    /// Its command: its program, then its arguments.
    command: Vec<String>,
    digest: String,
}

/// `ods health trust`'s report.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct TrustReport {
    /// The directory trust is kept for; absent when the checks are the user's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    /// The file the probes are defined in.
    #[serde(skip_serializing_if = "Option::is_none")]
    defined_in: Option<PathBuf>,
    /// The trust store.
    #[serde(skip_serializing_if = "Option::is_none")]
    store: Option<PathBuf>,
    /// `trusted`, `revoked`, `not_needed` (the user's own checks) or `nothing` (no
    /// probes or scripts).
    outcome: &'static str,
    probes: Vec<Reviewed>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    scripts: Vec<ReviewedScript>,
    /// The out-of-process plugins the project declares (ADR-0031 §5).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    plugins: Vec<ReviewedPlugin>,
}

/// One out-of-process plugin, as `ods health trust` shows it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ReviewedPlugin {
    name: String,
    /// How it stood before this run.
    standing: Standing,
    /// Its command: its program, then its arguments.
    command: Vec<String>,
    digest: String,
}

/// What a trust entry records of a definition: its id and digest.
struct Entry {
    id: String,
    digest: String,
}

impl Pinned for Entry {
    fn id(&self) -> &str {
        &self.id
    }
    fn digest(&self) -> &str {
        &self.digest
    }
}

impl Entry {
    fn of(d: &impl Pinned) -> Self {
        Self {
            id: d.id().to_owned(),
            digest: d.digest().to_owned(),
        }
    }
}

impl TrustReport {
    pub(super) fn build(
        args: &ArgMatches,
        config: &Loaded,
        health: &HealthSettings,
    ) -> Result<Self, CliError> {
        let mut definitions: Vec<CheckDefinition> = health.trust_definitions();
        let probes = health.probe_definitions();
        let mut scripts = health.script_definitions();
        // The user's own scripts need no trust, even where the project's probe settings
        // make their probes need it: they are neither recorded nor shown.
        if matches!(Origin::of_scripts(config), Origin::User(_))
            && matches!(Origin::of(config), Origin::Project { .. })
        {
            definitions.retain(|d| !matches!(d, CheckDefinition::Script(_)));
            scripts.clear();
        }
        // Plugins a project declares; the user's own need no trust, and aren't shown.
        let plugins: Vec<Declared> = super::plugin_process::declared(config)?
            .into_iter()
            .filter(|p| p.project.is_some())
            .collect();
        let revoke = args.get_flag("revoke");
        // Each directory trust is kept for, with what is trusted under it: the checks'
        // file's, then each plugin's (usually the same: the project's).
        let mut roots: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        let first_plugin = plugins.first().and_then(|p| p.project.clone());
        let (file, root) = match (Origin::of(config), first_plugin) {
            (Origin::Project { file, root }, _) => {
                roots
                    .entry(root.clone())
                    .or_default()
                    .extend(definitions.iter().map(Entry::of));
                (file, root)
            }
            (_, Some(Declaration { file, root })) => (file, root),
            (origin, None) => {
                let defined_in = match origin {
                    Origin::User(path) => Some(path),
                    _ => None,
                };
                let outcome = if definitions.is_empty() || defined_in.is_none() {
                    "nothing"
                } else {
                    "not_needed"
                };
                return Ok(Self {
                    project: None,
                    defined_in,
                    store: None,
                    outcome,
                    probes: reviewed(&probes, &BTreeMap::new()),
                    scripts: reviewed_scripts(&scripts, &BTreeMap::new()),
                    plugins: Vec::new(),
                });
            }
        };
        if !matches!(Origin::of(config), Origin::Project { .. }) {
            // Only plugins need trust: the checks are the user's own, or there are none.
            definitions.clear();
        }
        for plugin in &plugins {
            if let Some(declaration) = &plugin.project {
                roots
                    .entry(declaration.root.clone())
                    .or_default()
                    .push(Entry::of(plugin));
            }
        }
        let (store_path, standing, outcome) = record(config, &roots, &root, revoke)?;
        if definitions.is_empty() {
            // Shown only when they are what this run trusted.
            return Ok(Self {
                project: Some(root),
                defined_in: Some(file),
                store: Some(store_path),
                outcome,
                probes: Vec::new(),
                scripts: Vec::new(),
                plugins: reviewed_plugins(&plugins, &standing),
            });
        }
        Ok(Self {
            project: Some(root),
            defined_in: Some(file),
            store: Some(store_path),
            outcome,
            probes: reviewed(&probes, &standing),
            scripts: reviewed_scripts(&scripts, &standing),
            plugins: reviewed_plugins(&plugins, &standing),
        })
    }
}

/// Records `roots`' entries in the trust store, or forgets them (and `root`) with
/// `revoke`: where the store is, how each entry stood before, and the outcome.
fn record(
    config: &Loaded,
    roots: &BTreeMap<String, Vec<Entry>>,
    root: &String,
    revoke: bool,
) -> Result<(PathBuf, BTreeMap<String, Standing>, &'static str), CliError> {
    let store_path = store_path(config).ok_or_else(|| {
        error(
            "there is no user configuration directory to keep trust in".to_owned(),
            "set XDG_CONFIG_HOME or HOME (APPDATA on Windows)",
        )
    })?;
    // A store that can't be read is never overwritten: it may trust other projects.
    let mut store = TrustStore::read(&store_path).map_err(|e| {
        error(
            e.to_string(),
            "fix or move the file aside; nothing was changed",
        )
    })?;
    let standing: BTreeMap<String, Standing> = roots
        .iter()
        .flat_map(|(root, entries)| store.standing(root, entries))
        .collect();
    let outcome = if revoke {
        for root in roots.keys().chain([root]) {
            store.revoke(root);
        }
        "revoked"
    } else if roots.values().all(Vec::is_empty) {
        "nothing"
    } else {
        let now = Timestamp::now();
        for (root, entries) in roots {
            store.trust(root, entries, now);
        }
        "trusted"
    };
    if outcome != "nothing" {
        store
            .write(&store_path)
            .map_err(|e| error(e.to_string(), "nothing was changed"))?;
    }
    Ok((store_path, standing, outcome))
}

fn reviewed_plugins(
    plugins: &[Declared],
    standing: &BTreeMap<String, Standing>,
) -> Vec<ReviewedPlugin> {
    plugins
        .iter()
        .map(|p| ReviewedPlugin {
            name: p.name.clone(),
            standing: standing.get(p.id()).copied().unwrap_or(Standing::Trusted),
            command: p.command.clone(),
            digest: p.digest.clone(),
        })
        .collect()
}

/// A probe's queries for people: the one query, or `<kind>: <query>` on a line each.
fn sql_text(sql: &ods_config::ProbeSql) -> String {
    match sql {
        ods_config::ProbeSql::PerWarehouse(queries) => queries
            .iter()
            .map(|(kind, sql)| format!("{kind}: {sql}"))
            .collect::<Vec<_>>()
            .join("\n"),
        ods_config::ProbeSql::One(sql) => sql.clone(),
        _ => String::new(),
    }
}

fn reviewed(
    definitions: &[ProbeDefinition],
    standing: &BTreeMap<String, Standing>,
) -> Vec<Reviewed> {
    definitions
        .iter()
        .map(|d| Reviewed {
            id: d.id.clone(),
            standing: standing.get(&d.id).copied().unwrap_or(Standing::Trusted),
            sql: d.sql.clone(),
            digest: d.digest.clone(),
        })
        .collect()
}

fn reviewed_scripts(
    definitions: &[ScriptDefinition],
    standing: &BTreeMap<String, Standing>,
) -> Vec<ReviewedScript> {
    definitions
        .iter()
        .map(|d| ReviewedScript {
            id: d.id.clone(),
            standing: standing.get(&d.id).copied().unwrap_or(Standing::Trusted),
            command: d.command.clone(),
            digest: d.digest.clone(),
        })
        .collect()
}

/// A table of what runs a command (scripts, plugins): its name, how it stood, and its
/// command.
fn commands_table<'a>(
    title: &str,
    what: &str,
    rows: impl Iterator<Item = (&'a String, Standing, &'a Vec<String>)>,
) -> ViewNode {
    ViewNode::Table {
        title: Some(title.to_owned()),
        columns: vec![what.into(), "was".into(), "command".into()],
        rows: rows
            .map(|(name, standing, command)| {
                let (was, tone) = was(standing);
                vec![
                    vec![Span::toned(name.as_str(), Tone::Code)],
                    vec![Span::toned(was, tone)],
                    vec![Span::toned(command.join(" "), Tone::Code)],
                ]
            })
            .collect(),
        breaks: Vec::new(),
        footer: None,
    }
}

/// How a definition stood, for people.
fn was(standing: Standing) -> (&'static str, Tone) {
    match standing {
        Standing::Trusted => ("trusted", Tone::Muted),
        Standing::Changed => ("changed", Tone::Warning),
        _ => ("new", Tone::Warning),
    }
}

fn error(message: String, hint: &str) -> CliError {
    CliError::new(ExitStatus::Failure, codes::HEALTH_TRUST, message).with_hint(hint)
}

impl Present for TrustReport {
    const COMMAND: &'static str = "health.trust";

    fn view(&self) -> ViewNode {
        let mut blocks = Vec::new();
        let mut facts = Vec::new();
        if let Some(project) = &self.project {
            facts.push((
                "project".to_owned(),
                vec![Span::toned(project.as_str(), Tone::Code)],
            ));
        }
        if let Some(file) = &self.defined_in {
            facts.push((
                "defined in".to_owned(),
                vec![Span::toned(file.display().to_string(), Tone::Code)],
            ));
        }
        if let Some(store) = &self.store {
            facts.push((
                "trust store".to_owned(),
                vec![Span::toned(store.display().to_string(), Tone::Code)],
            ));
        }
        if !facts.is_empty() {
            blocks.push(ViewNode::KeyValue(facts));
        }
        if !self.probes.is_empty() {
            blocks.push(ViewNode::Table {
                title: Some("Probe checks".to_owned()),
                columns: vec!["check".into(), "was".into(), "sql".into()],
                rows: self
                    .probes
                    .iter()
                    .map(|p| {
                        let (was, tone) = was(p.standing);
                        vec![
                            vec![Span::toned(p.id.as_str(), Tone::Code)],
                            vec![Span::toned(was, tone)],
                            vec![Span::toned(sql_text(&p.sql), Tone::Code)],
                        ]
                    })
                    .collect(),
                breaks: Vec::new(),
                footer: None,
            });
        }
        if !self.scripts.is_empty() {
            blocks.push(commands_table(
                "Script checks",
                "check",
                self.scripts.iter().map(|s| (&s.id, s.standing, &s.command)),
            ));
        }
        if !self.plugins.is_empty() {
            blocks.push(commands_table(
                "Plugins",
                "plugin",
                self.plugins
                    .iter()
                    .map(|p| (&p.name, p.standing, &p.command)),
            ));
        }
        let mut what = match (self.probes.len(), self.scripts.len()) {
            (p, 0) => format!("{p} probe check{}", if p == 1 { "" } else { "s" }),
            (0, s) => format!("{s} script check{}", if s == 1 { "" } else { "s" }),
            (p, s) => format!(
                "{p} probe check{} and {s} script check{}",
                if p == 1 { "" } else { "s" },
                if s == 1 { "" } else { "s" }
            ),
        };
        match (self.probes.len() + self.scripts.len(), self.plugins.len()) {
            (_, 0) => {}
            (0, n) => what = format!("{n} plugin{}", if n == 1 { "" } else { "s" }),
            (_, n) => {
                let _ = write!(what, " and {n} plugin{}", if n == 1 { "" } else { "s" });
            }
        }
        let message = match self.outcome {
            "trusted" => format!(
                "trusted {what} for this project, as defined now; a change to one makes it untrusted again"
            ),
            "revoked" => {
                "forgot this project's trusted checks: its probes, scripts and plugins won't run"
                    .to_owned()
            }
            "not_needed" => {
                "these checks come from your own configuration and need no trust".to_owned()
            }
            _ => "no probe or script checks or plugins to trust".to_owned(),
        };
        blocks.push(ViewNode::Notice {
            level: Level::Info,
            message: vec![Span::plain(message)],
        });
        ViewNode::Group(blocks)
    }
}
