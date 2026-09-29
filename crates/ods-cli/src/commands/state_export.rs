//! `ods state export --dbt-state <dir> --upstream <state dir>` (#296, ADR-0020): a dbt
//! state directory in which the nodes ODS recorded as built in this target, and can
//! show still exist here, point at this target's relations. Every other node keeps the
//! upstream's pointer. dbt reads it with `--defer-state <dir>`, including on
//! `dbt retry`.
//!
//! 1. Load the project from the artifacts already in the target directory. No
//!    `dbt compile`: it would overwrite `run_results.json`, which `dbt retry` reads.
//! 2. Read the upstream `manifest.json`, and refuse another project's or an
//!    unsupported version.
//! 3. Ask dbt which target it builds in, and read the scope's latest snapshot.
//! 4. Check, in one dbt call, that the relations of the nodes that could point here
//!    exist (unless `--no-check-relations`).
//! 5. Choose each node's pointer (`ods_state::choose_pointers`), and write
//!    `ods-export.json`, then `manifest.json`, each atomically, under a lock.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::state::{SnapshotId, TargetIdentity, Timestamp, sha256_hex};
use ods_provider_dbt::ArtifactSource;
use ods_provider_dbt::executor::DbtExecutor;
use ods_provider_dbt::export::{
    ExportError, RelationFields, read_upstream, rewrite_manifest, same_project,
};
use ods_sdk::contracts::executor::RequestedNode;
use ods_state::{ExportRecord, Pointer, PointerChoice, PointerReason, RelationFact, UpstreamNode};
use serde::Serialize;

use super::state_plan::{Sources, Workspace, common, display_name};
use super::state_run::{
    CheckFor, Steps, dbt_invocation_options, executor, identify, relation_facts,
};
use super::state_settings::StateSettings;
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::Context;
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// The lock file in the export directory; dbt never reads it.
const LOCK: &str = ".ods-export.lock";
/// ODS's record of the export; dbt never reads it.
const RECORD: &str = "ods-export.json";
/// The state dbt reads: written last, so a dbt run sees a whole export.
const MANIFEST: &str = "manifest.json";

/// `ods state export`'s arguments.
pub(super) fn export_command() -> Command {
    dbt_invocation_options(common(Command::new("export").about(
        "Write a dbt state directory in which nodes built here point at this target, for `dbt retry --defer-state` and other deferred runs",
    )))
    .arg(
        Arg::new("dbt-state")
            .long("dbt-state")
            .value_name("DIR")
            .required(true)
            .help("The directory to write: pass it to dbt as --defer-state. Created if missing; only its manifest.json and ods-export.json are replaced"),
    )
    .arg(
        Arg::new("upstream")
            .long("upstream")
            .value_name("DIR")
            .required(true)
            .help("The state directory dbt defers to now, e.g. prod's (holding its manifest.json). Never written to"),
    )
    .arg(
        Arg::new("no-check-relations")
            .long("no-check-relations")
            .action(ArgAction::SetTrue)
            .help("Don't ask the warehouse whether tables built here still exist: every node then keeps the upstream pointer"),
    )
    .arg(
        Arg::new("now")
            .long("now")
            .value_name("TIMESTAMP")
            .hide(true)
            .help("Export as of this time (RFC 3339), for reproducible output"),
    )
    .arg(
        Arg::new("lock-wait")
            .long("lock-wait")
            .value_name("SECONDS")
            .value_parser(clap::value_parser!(u64))
            .default_value("10")
            .hide(true)
            .help("How long to wait for another export to the same directory"),
    )
}

fn input_error(message: impl Into<String>) -> CliError {
    CliError::new(ExitStatus::Usage, codes::STATE_INPUT, message)
}

/// The export reads where each node builds from dbt's `manifest.json`: dbt's
/// Information Schema doesn't record it.
fn info_schema_refused() -> CliError {
    input_error(
        "`ods state export` needs dbt's manifest.json in the target directory: dbt's Information Schema doesn't say where each node builds",
    )
    .with_hint("run dbt with JSON artifacts (the default), and pass --artifacts json or auto")
}

fn export_error(message: impl Into<String>) -> CliError {
    CliError::new(ExitStatus::Failure, codes::STATE_EXPORT, message)
}

/// The directory as the filesystem resolves it, so two spellings of one directory
/// compare equal. One that doesn't exist yet is compared as an absolute path.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_owned())
}

/// Counts of each reason, in rule order, leaving out reasons no node has.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ReasonCount {
    reason: PointerReason,
    pointer: Pointer,
    nodes: usize,
}

/// What `ods state export` did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct ExportReport {
    /// The directory written.
    dbt_state: PathBuf,
    /// The upstream state directory read.
    upstream: PathBuf,
    scope: String,
    /// The snapshot the choices were made from.
    based_on: Option<SnapshotId>,
    /// The target the export points at: its non-secret form only (ADR-0017).
    target: TargetIdentity,
    /// Whether the warehouse was asked which relations exist.
    relations_checked: bool,
    /// SHA-256 of the manifest written.
    manifest_sha256: String,
    /// The files replaced, in the order they were.
    written: Vec<String>,
    /// How many nodes point where, and why.
    counts: Vec<ReasonCount>,
    /// Every upstream node's choice, by id.
    nodes: Vec<PointerChoice>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

impl ExportReport {
    pub(super) fn run(args: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let report = Self::build(args, ctx)?;
        ctx.emit(&report)
    }

    fn build(args: &ArgMatches, ctx: &Context<'_>) -> Result<Self, CliError> {
        let settings = StateSettings::resolve(args, ctx.config)?;
        let (dir, upstream_dir) = directories(args, &settings)?;
        let now = match args.get_one::<String>("now") {
            Some(at) => Timestamp::parse(at).map_err(input_error)?,
            None => Timestamp::now(),
        };
        let mut warnings = Vec::new();

        // 1–2: the project as the last dbt run left it, and the upstream state.
        let ws = Workspace::load(args, &settings, Sources::Ignore)?;
        if ws.manifest.source != ArtifactSource::ManifestJson {
            return Err(info_schema_refused());
        }
        let upstream = read_upstream(&upstream_dir).map_err(|e| {
            input_error(format!("can't export from --upstream {}: {e}", upstream_dir.display()))
                .with_hint("--upstream names the directory holding the state manifest.json dbt defers to, e.g. prod's")
        })?;
        warnings.extend(
            same_project(
                &upstream,
                ws.manifest.project_name.as_deref(),
                ws.manifest.project_id.as_deref(),
            )
            .map_err(|e| input_error(e.to_string()))?,
        );

        // 3: which target, and what is recorded in it.
        let executor = executor(args, &settings);
        executor.refuse_env().map_err(|e| {
            input_error(e.to_string()).with_hint(
                "these dbt settings change what dbt builds in ways ODS can't record; see `docs/cli.md`",
            )
        })?;
        warnings.extend(executor.env_warnings());
        let steps = Steps::new(
            ctx.progress,
            1 + usize::from(!args.get_flag("no-check-relations")),
        );
        let executor = steps.attach(executor);
        let target = identify(&executor)?;
        let latest = if ws.state_db.is_file() {
            ws.latest(&ws.open_store()?)?
        } else {
            None
        };
        let snapshot = latest.as_ref().map(|l| &l.snapshot);
        let nodes: Vec<UpstreamNode> = upstream
            .nodes
            .iter()
            .map(|n| UpstreamNode::new(n.id.clone(), n.deferrable))
            .collect();

        // 4: the relation check, for the nodes that could point here.
        let current = ws.manifest.nodes_by_id();
        let relation = |id: &str| current.get(id).copied().and_then(RelationFields::of);
        let candidates = ods_state::defer_candidates(&ws.project, &nodes, snapshot, &target);
        let facts = if args.get_flag("no-check-relations") {
            None
        } else {
            check_relations(&executor, &candidates, &relation, &mut warnings)?
        };

        // 5: choose, and write.
        let choices =
            ods_state::choose_pointers(&ws.project, &nodes, snapshot, &target, facts.as_ref());
        let relations = pointed_here(&choices, &relation)?;
        let manifest = rewrite_manifest(&upstream.document, &relations).map_err(|e| match e {
            ExportError::Serialize(_) => export_error(e.to_string()),
            _ => input_error(format!(
                "can't export from --upstream {}: {e}",
                upstream_dir.display()
            )),
        })?;
        let manifest_sha256 = sha256_hex(&manifest);
        let record = ExportRecord::new(
            now,
            latest.as_ref().map(|l| l.id),
            target.clone(),
            upstream.invocation_id.clone(),
            manifest_sha256.clone(),
            choices.clone(),
        );
        let record = serde_json::to_vec_pretty(&record)
            .map_err(|e| export_error(format!("couldn't write {RECORD}: {e}")))?;
        let wait = Duration::from_secs(args.get_one::<u64>("lock-wait").copied().unwrap_or(10));
        let written = write_export(&dir, &[(RECORD, record), (MANIFEST, manifest)], wait)?;

        let counts = count_reasons(&choices);
        Ok(Self {
            dbt_state: dir,
            upstream: upstream_dir,
            scope: ws.scope.to_string(),
            based_on: latest.map(|l| l.id),
            target,
            relations_checked: facts.is_some(),
            manifest_sha256,
            written,
            counts,
            nodes: choices,
            warnings,
        })
    }
}

/// `--dbt-state` and `--upstream`, refused when the export would write into the
/// upstream or dbt's target directory; and a refusal of `--artifacts info-schema`.
fn directories(
    args: &ArgMatches,
    settings: &StateSettings,
) -> Result<(PathBuf, PathBuf), CliError> {
    if args.get_one::<String>("artifacts").map(String::as_str) == Some("info-schema") {
        return Err(info_schema_refused());
    }
    let dir = PathBuf::from(
        args.get_one::<String>("dbt-state")
            .map_or("", String::as_str),
    );
    let upstream_dir = PathBuf::from(
        args.get_one::<String>("upstream")
            .map_or("", String::as_str),
    );
    let (into, from, target_dir) = (
        resolved(&dir),
        resolved(&upstream_dir),
        resolved(&settings.target_dir()),
    );
    if into == from {
        return Err(input_error(format!(
            "--dbt-state {} is the upstream directory: the export never writes into it",
            dir.display()
        ))
        .with_hint("export to a directory of its own, e.g. --dbt-state .ods/dbt-state"));
    }
    if into == target_dir {
        return Err(input_error(format!(
            "--dbt-state {} is dbt's target directory, whose manifest.json dbt rewrites on every run",
            dir.display()
        ))
        .with_hint("export to a directory of its own, e.g. --dbt-state .ods/dbt-state"));
    }
    Ok((dir, upstream_dir))
}

/// What the warehouse says about the candidates' relations, in one dbt call. A node
/// whose manifest entry names no relation can't be checked, so it is unverified.
fn check_relations(
    executor: &DbtExecutor,
    candidates: &[String],
    relation: &dyn Fn(&str) -> Option<RelationFields>,
    warnings: &mut Vec<String>,
) -> Result<Option<BTreeMap<String, RelationFact>>, CliError> {
    let (named, unnamed): (Vec<&String>, Vec<&String>) =
        candidates.iter().partition(|id| relation(id).is_some());
    let requested: Vec<RequestedNode> = named
        .iter()
        .map(|id| RequestedNode::new((*id).clone(), display_name(id)))
        .collect();
    Ok(
        relation_facts(executor, &requested, CheckFor::Export, warnings)?.map(|mut facts| {
            for id in unnamed {
                facts.insert(
                    id.clone(),
                    RelationFact::Unverified("the manifest names no relation".to_owned()),
                );
            }
            facts
        }),
    )
}

/// The relation fields of each node pointing at this target.
///
/// `check_relations` marks nodes without relation fields unverified, so each node
/// pointing here has them; if one didn't, fail rather than leave it on the upstream
/// while the record says it points here.
fn pointed_here(
    choices: &[PointerChoice],
    relation: &dyn Fn(&str) -> Option<RelationFields>,
) -> Result<BTreeMap<String, RelationFields>, CliError> {
    choices
        .iter()
        .filter(|c| c.pointer == Pointer::ThisTarget)
        .map(|c| {
            relation(&c.node)
                .map(|r| (c.node.clone(), r))
                .ok_or_else(|| {
                    export_error(format!(
                        "{} points at this target but its manifest entry names no relation",
                        c.node
                    ))
                })
        })
        .collect()
}

/// How many nodes have each reason, in rule order.
fn count_reasons(choices: &[PointerChoice]) -> Vec<ReasonCount> {
    PointerReason::ALL
        .into_iter()
        .filter_map(|reason| {
            let matching: Vec<&PointerChoice> =
                choices.iter().filter(|c| c.reason == reason).collect();
            let first = matching.first()?;
            Some(ReasonCount {
                reason,
                pointer: first.pointer,
                nodes: matching.len(),
            })
        })
        .collect()
}

/// Takes the export directory's lock, waiting up to `wait` for another export to
/// finish. The lock is released when the file is dropped, or the process ends.
fn lock(dir: &Path, wait: Duration) -> Result<File, CliError> {
    let path = dir.join(LOCK);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| export_error(format!("can't open the lock `{}`: {e}", path.display())))?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < wait => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(export_error(format!(
                    "another export to {} is still running (it holds `{}`): nothing was written",
                    dir.display(),
                    path.display()
                ))
                .with_hint("wait for it to finish, then export again"));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(export_error(format!(
                    "can't lock `{}`: {e}",
                    path.display()
                )));
            }
        }
    }
}

/// Replaces `path` with `file`. On Windows a rename fails while another process has
/// the target open, so a denied rename is retried briefly.
fn persist(mut file: tempfile::NamedTempFile, path: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match file.persist(path) {
            Ok(_) => return Ok(()),
            Err(e) if e.error.kind() == std::io::ErrorKind::PermissionDenied && tries < 20 => {
                tries += 1;
                file = e.file;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.error),
        }
    }
}

/// Writes each file in full to a temporary file in `dir`, flushed to disk, then
/// renames each over its name, in order (ADR-0020 §1). A reader sees the old or the
/// new version of each file, never part of one. Returns the names replaced.
fn write_export(
    dir: &Path,
    files: &[(&str, Vec<u8>)],
    wait: Duration,
) -> Result<Vec<String>, CliError> {
    std::fs::create_dir_all(dir).map_err(|e| {
        export_error(format!("can't create {}: {e}", dir.display()))
            .with_hint("nothing was written")
    })?;
    let _lock = lock(dir, wait)?;
    let mut staged = Vec::new();
    for (name, bytes) in files {
        let write = || -> std::io::Result<tempfile::NamedTempFile> {
            let mut file = tempfile::Builder::new()
                .prefix(".ods-export-")
                .tempfile_in(dir)?;
            file.write_all(bytes)?;
            file.as_file().sync_all()?;
            Ok(file)
        };
        let file = write().map_err(|e| {
            export_error(format!("can't write {name} in {}: {e}", dir.display()))
                .with_hint("nothing was replaced")
        })?;
        staged.push((*name, file));
    }
    let mut replaced: Vec<String> = Vec::new();
    for (name, file) in staged {
        persist(file, &dir.join(name)).map_err(|e| {
            let done = if replaced.is_empty() {
                "nothing was replaced".to_owned()
            } else {
                format!(
                    "already replaced: {}; the rest are as before",
                    replaced.join(", ")
                )
            };
            export_error(format!(
                "can't replace {name} in {}: {e}; {done}",
                dir.display()
            ))
            .with_hint("run the export again to repair the directory")
        })?;
        replaced.push(name.to_owned());
    }
    Ok(replaced)
}

impl Present for ExportReport {
    const COMMAND: &'static str = "state.export";

    fn view(&self) -> ViewNode {
        let here: Vec<&PointerChoice> = self
            .nodes
            .iter()
            .filter(|c| c.pointer == Pointer::ThisTarget)
            .collect();
        let mut blocks = vec![
            ViewNode::Heading("State export for dbt".into()),
            ViewNode::KeyValue(vec![
                (
                    "written".into(),
                    vec![Span::toned(
                        self.dbt_state.display().to_string(),
                        Tone::Code,
                    )],
                ),
                (
                    "upstream".into(),
                    vec![Span::toned(self.upstream.display().to_string(), Tone::Code)],
                ),
                (
                    "scope".into(),
                    vec![Span::toned(self.scope.as_str(), Tone::Code)],
                ),
                ("target".into(), vec![Span::plain(self.target.to_string())]),
                (
                    "recorded state".into(),
                    vec![Span::plain(self.based_on.map_or_else(
                        || "none: every node points upstream".to_owned(),
                        |id| format!("snapshot {id}"),
                    ))],
                ),
                (
                    "relations".into(),
                    vec![Span::plain(if self.relations_checked {
                        "checked in the warehouse"
                    } else {
                        "not checked: nothing points at this target"
                    })],
                ),
            ]),
            ViewNode::Table {
                title: Some("Pointers".into()),
                columns: vec!["points at".into(), "reason".into(), "nodes".into()],
                rows: self
                    .counts
                    .iter()
                    .map(|c| {
                        let (word, tone) = match c.pointer {
                            Pointer::ThisTarget => ("this target", Tone::Added),
                            Pointer::Upstream => ("upstream", Tone::Muted),
                            _ => ("unchanged", Tone::Muted),
                        };
                        vec![
                            vec![Span::toned(word, tone)],
                            vec![Span::toned(c.reason.code(), Tone::Code)],
                            vec![Span::plain(c.nodes.to_string())],
                        ]
                    })
                    .collect(),
            },
        ];
        if !here.is_empty() {
            blocks.push(ViewNode::Table {
                title: Some("Pointing at this target".into()),
                columns: vec!["node".into(), "built by run".into(), "at".into()],
                rows: here
                    .iter()
                    .map(|c| {
                        vec![
                            vec![Span::toned(c.node.as_str(), Tone::Code)],
                            vec![Span::plain(c.run_id.clone().unwrap_or_default())],
                            vec![Span::plain(
                                c.built_at.map(|t| t.to_string()).unwrap_or_default(),
                            )],
                        ]
                    })
                    .collect(),
            });
        }
        for warning in &self.warnings {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(warning.as_str())],
            });
        }
        blocks.push(ViewNode::Paragraph(vec![
            Span::plain("Use it with "),
            Span::toned(
                format!("dbt retry --defer-state {}", self.dbt_state.display()),
                Tone::Code,
            ),
            Span::plain(", or add "),
            Span::toned(
                format!("--defer-state {}", self.dbt_state.display()),
                Tone::Code,
            ),
            Span::plain(" to a deferred run. `--output json` explains every node."),
        ]));
        ViewNode::Group(blocks)
    }
}
