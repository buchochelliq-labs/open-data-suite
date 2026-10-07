//! Trust for `[[health.checks]]` probes (ADR-0030 §4b, §4d): where the definitions come
//! from, which of them the user trusted for this project, and `ods health trust`, which
//! shows them and records the current ones.
//!
//! A probe sends SQL to the warehouse, and a repository's `ods.toml` can name it, so a
//! probe a project defines runs only once the user has trusted that exact definition for
//! that project. Probes defined in the user's own `config.toml` need no trust: the user
//! wrote them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_config::{FileKind, Loaded, Source};
use ods_core::state::Timestamp;
use ods_health::trust::{self, Standing, TrustStore};
use ods_health::{HealthSettings, ProbeDefinition};
use serde::Serialize;

use crate::exit::{CliError, ExitStatus, codes};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// Where the probes come from, and so whether they need trust.
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
fn store_path(config: &Loaded) -> Option<PathBuf> {
    config
        .files
        .iter()
        .find(|f| f.kind == FileKind::User)
        .map(|f| trust::path_beside(&f.path))
}

/// Marks the probes in `health` trusted as `config`'s origin and the trust store say,
/// or all of them with `allow_scripts` (this run only). A store that can't be read
/// trusts nothing: it never fails open (AGENTS rule 3).
pub(super) fn apply(health: &mut HealthSettings, config: &Loaded, allow_scripts: bool) {
    let definitions = health.probe_definitions();
    if definitions.is_empty() {
        return;
    }
    let all = || definitions.iter().map(|d| d.id.clone()).collect();
    let trusted: BTreeSet<String> = match Origin::of(config) {
        _ if allow_scripts => all(),
        Origin::User(_) => all(),
        Origin::None => BTreeSet::new(),
        Origin::Project { root, .. } => match store_path(config).map(|p| TrustStore::read(&p)) {
            Some(Ok(store)) => store.trusted(&root, &definitions),
            Some(Err(e)) => {
                tracing::warn!(error = %e, "health: probes aren't trusted");
                BTreeSet::new()
            }
            None => BTreeSet::new(),
        },
    };
    health.trust_probes(&trusted);
}

/// `ods health trust`.
pub(super) fn command() -> Command {
    Command::new("trust")
        .about("Review this project's probe checks and trust them to run (ADR-0030 §4b)")
        .long_about(
            "Lists the probe checks this project's ods.toml defines, with their SQL and \
             whether each is new, changed or already trusted, then trusts them as they \
             are now. A probe runs only once its exact definition is trusted for this \
             project; changing its SQL or what it selects makes it untrusted again. \
             Trust is kept in your own configuration directory, never in the repository.",
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
    /// probes).
    outcome: &'static str,
    probes: Vec<Reviewed>,
}

impl TrustReport {
    pub(super) fn build(
        args: &ArgMatches,
        config: &Loaded,
        health: &HealthSettings,
    ) -> Result<Self, CliError> {
        let definitions: Vec<ProbeDefinition> = health.probe_definitions();
        let revoke = args.get_flag("revoke");
        let (file, root) = match Origin::of(config) {
            Origin::Project { file, root } => (file, root),
            origin => {
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
                    probes: reviewed(&definitions, &BTreeMap::new()),
                });
            }
        };
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
        let standing = store.standing(&root, &definitions);
        let outcome = if revoke {
            store.revoke(&root);
            "revoked"
        } else if definitions.is_empty() {
            "nothing"
        } else {
            store.trust(&root, &definitions, Timestamp::now());
            "trusted"
        };
        if outcome != "nothing" {
            store
                .write(&store_path)
                .map_err(|e| error(e.to_string(), "nothing was changed"))?;
        }
        Ok(Self {
            project: Some(root),
            defined_in: Some(file),
            store: Some(store_path),
            outcome,
            probes: reviewed(&definitions, &standing),
        })
    }
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
                        let (was, tone) = match p.standing {
                            Standing::Trusted => ("trusted", Tone::Muted),
                            Standing::Changed => ("changed", Tone::Warning),
                            _ => ("new", Tone::Warning),
                        };
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
        let message = match self.outcome {
            "trusted" => format!(
                "trusted {} probe check{} for this project, as defined now; a change to one makes it untrusted again",
                self.probes.len(),
                if self.probes.len() == 1 { "" } else { "s" }
            ),
            "revoked" => "forgot this project's trusted checks: its probes won't run".to_owned(),
            "not_needed" => {
                "these probes come from your own configuration and need no trust".to_owned()
            }
            _ => "no probe checks to trust".to_owned(),
        };
        blocks.push(ViewNode::Notice {
            level: Level::Info,
            message: vec![Span::plain(message)],
        });
        ViewNode::Group(blocks)
    }
}
