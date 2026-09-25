//! `ods lineage`: column-level lineage of a dbt project, downstream impact, and
//! `OpenLineage` export (#74, ADR-0008).
//!
//! This adapter is the composition root for lineage: it reads dbt artifacts with
//! `ods-provider-dbt`, analyzes SQL with `ods-provider-sqlparser`, and hands the neutral
//! project to `ods-lineage`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::{ColumnRef, EdgeKind};
use ods_lineage::export::Endpoint;
use ods_lineage::openlineage::{EventKind, ExportOptions, IndirectPlacement};
use ods_lineage::{
    BuildStats, Change, ColumnChangeKind, ColumnGraph, GraphFilter, Impact, ImpactReason,
    LineageNode, LineageProject, MemoryCache, NodeKind, build, diff,
};
use ods_provider_dbt::{ArtifactPreference, Artifacts, ResourceType};
use ods_provider_sqlparser::{SqlDialect, SqlparserAnalyzer};
use ods_sdk::contracts::sql_lineage::SqlLineageAnalyzer;
use serde::Serialize;

use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};
use crate::present::{Level, Present, Span, Tone, TreeItem, ViewNode};

/// `ods lineage`.
pub struct Lineage;

pub(super) fn common_args(command: Command) -> Command {
    command
        .arg(
            Arg::new("target-dir")
                .long("target-dir")
                .value_name("DIR")
                .default_value("target")
                .help("dbt target directory with manifest.json (and catalog.json, if generated)"),
        )
        .arg(
            Arg::new("artifacts")
                .long("artifacts")
                .value_name("FORMAT")
                .value_parser(["auto", "json", "info-schema"])
                .default_value("auto")
                .help("dbt artifacts to read: manifest.json, or dbt v2's Parquet Information Schema (auto prefers manifest.json)"),
        )
        .arg(
            Arg::new("dialect")
                .long("dialect")
                .value_name("DIALECT")
                .help("SQL dialect; defaults to the manifest's adapter type"),
        )
}

impl Module for Lineage {
    fn command(&self) -> Command {
        Command::new("lineage")
            .about("Column-level lineage, downstream impact and OpenLineage export")
            .subcommand_required(true)
            .subcommand(common_args(
                Command::new("columns")
                    .about("Show where every model column comes from")
                    .arg(
                        Arg::new("model")
                            .long("model")
                            .value_name("MODEL")
                            .help("Only this model (name or unique_id)"),
                    ),
            ))
            .subcommand(common_args(
                Command::new("impact")
                    .about("Which downstream models must run for these changes, and why")
                    .arg(
                        Arg::new("column")
                            .long("column")
                            .value_name("MODEL.COLUMN[=KIND]")
                            .action(ArgAction::Append)
                            .help("A changed column; KIND is modified (default), added or removed"),
                    )
                    .arg(
                        Arg::new("base")
                            .long("base")
                            .value_name("DIR")
                            .conflicts_with("column")
                            .help("Compare with another build's target directory and use every difference"),
                    ),
            ))
            .subcommand(graph_command())
            .subcommand(view_command())
            .subcommand(export_command())
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let Some((name, args)) = matches.subcommand() else {
            return Ok(());
        };
        let loaded = Loaded::load(args)?;
        match name {
            "columns" => {
                let model = args.get_one::<String>("model").map(String::as_str);
                ctx.emit(&ColumnsReport::build(&loaded, model)?)
            }
            "impact" => {
                let report = if let Some(base) = args.get_one::<String>("base") {
                    ImpactReport::against(&loaded, Path::new(base))?
                } else {
                    let columns: Vec<&String> =
                        args.get_many("column").into_iter().flatten().collect();
                    if columns.is_empty() {
                        return Err(CliError::new(
                            ExitStatus::Usage,
                            codes::LINEAGE_TARGET,
                            "nothing to analyze: pass --column or --base",
                        ));
                    }
                    ImpactReport::for_columns(&loaded, &columns)?
                };
                ctx.emit(&report)
            }
            "export" => ctx.emit(&ExportReport::write(&loaded, args)?),
            "graph" => ctx.emit(&GraphReport::write(&loaded, args, false)?),
            "view" => ctx.emit(&GraphReport::write(&loaded, args, true)?),
            _ => Ok(()),
        }
    }
}

fn graph_command() -> Command {
    focus_args(common_args(
                Command::new("graph")
                    .about("Export the lineage graph as JSON, DOT, Mermaid or GraphML")
                    .arg(
                        Arg::new("format")
                            .long("format")
                            .value_name("FORMAT")
                            .value_parser(["json", "dot", "dot-columns", "mermaid", "graphml"])
                            .default_value("json")
                            .help("json (the viewer/VS Code contract), dot (models), dot-columns, mermaid (models) or graphml"),
                    )
                    .arg(
                        Arg::new("output-file")
                            .long("output-file")
                            .value_name("PATH")
                            .required(true)
                            .help("Where to write the graph"),
                    ),
            ))
}

fn view_command() -> Command {
    focus_args(common_args(
        Command::new("view")
            .about("Write a self-contained, offline HTML lineage explorer")
            .arg(
                Arg::new("output-file")
                    .long("output-file")
                    .value_name("PATH")
                    .default_value("lineage.html")
                    .help("Where to write the page"),
            )
            .arg(
                Arg::new("open")
                    .long("open")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("site")
                    .help("Open the page in the default browser"),
            )
            .arg(
                Arg::new("site")
                    .long("site")
                    .value_name("DIR")
                    .conflicts_with("output-file")
                    .help("Write a static site (index.html + graph.json) to host on any web server instead"),
            ),
    ))
}

fn export_command() -> Command {
    common_args(
                Command::new("export")
                    .about("Write OpenLineage events with column-lineage facets")
                    .arg(
                        Arg::new("output-file")
                            .long("output-file")
                            .value_name("PATH")
                            .required(true)
                            .help("Where to write the events, one JSON object per line"),
                    )
                    .arg(
                        Arg::new("namespace")
                            .long("namespace")
                            .value_name("NS")
                            .required(true)
                            .help("Dataset namespace, e.g. unitycatalog://<workspace-host>"),
                    )
                    .arg(
                        Arg::new("job-namespace")
                            .long("job-namespace")
                            .value_name("NS")
                            .default_value("ods")
                            .help("Job namespace"),
                    )
                    .arg(
                        Arg::new("run-events")
                            .long("run-events")
                            .action(ArgAction::SetTrue)
                            .help("Write COMPLETE RunEvents instead of JobEvents (e.g. for OpenMetadata)"),
                    )
                    .arg(
                        Arg::new("indirect-in-fields")
                            .long("indirect-in-fields")
                            .action(ArgAction::SetTrue)
                            .help("Also copy row-shaping inputs into every field, for consumers that ignore `dataset`"),
                    )
                    .arg(
                        Arg::new("event-time")
                            .long("event-time")
                            .value_name("RFC3339")
                            .help("eventTime for every event; defaults to now"),
                    ),
            )
}

fn focus_args(command: Command) -> Command {
    command
        .arg(
            Arg::new("focus")
                .long("focus")
                .value_name("MODEL[.COLUMN]")
                .action(ArgAction::Append)
                .help("Only what is connected to this model or column; repeatable"),
        )
        .arg(
            Arg::new("upstream")
                .long("upstream")
                .value_name("N")
                .value_parser(clap::value_parser!(usize))
                .help("With --focus, at most N hops upstream"),
        )
        .arg(
            Arg::new("downstream")
                .long("downstream")
                .value_name("N")
                .value_parser(clap::value_parser!(usize))
                .help("With --focus, at most N hops downstream"),
        )
}

/// The `--artifacts` choice.
pub(super) fn preference(args: &ArgMatches) -> ArtifactPreference {
    match args.get_one::<String>("artifacts").map(String::as_str) {
        Some("json") => ArtifactPreference::Json,
        Some("info-schema") => ArtifactPreference::InfoSchema,
        _ => ArtifactPreference::Auto,
    }
}

/// A project analyzed end to end.
pub(super) struct Loaded {
    target_dir: PathBuf,
    analyzer: SqlparserAnalyzer,
    /// dbt `unique_id` → node name, for friendly lookups.
    names: BTreeMap<String, String>,
    /// dbt `unique_id` → source file checksum, to detect changes to nodes without lineage.
    checksums: BTreeMap<String, Option<String>>,
    pub(super) graph: ColumnGraph,
    stats: BuildStats,
    elapsed_ms: u128,
}

impl Loaded {
    pub(super) fn load(args: &ArgMatches) -> Result<Self, CliError> {
        let target_dir = PathBuf::from(
            args.get_one::<String>("target-dir")
                .map_or("target", String::as_str),
        );
        let dialect = args.get_one::<String>("dialect").map(String::as_str);
        Self::from_dir(
            &target_dir,
            dialect,
            preference(args),
            &MemoryCache::default(),
        )
    }

    pub(super) fn from_dir(
        target_dir: &Path,
        dialect: Option<&str>,
        preference: ArtifactPreference,
        cache: &MemoryCache,
    ) -> Result<Self, CliError> {
        let started = Instant::now();
        let artifacts = Artifacts::load_with(target_dir, preference).map_err(|e| {
            CliError::new(ExitStatus::Failure, codes::LINEAGE_ARTIFACTS, e.to_string()).with_hint(
                "run `dbt compile` (and `dbt docs generate` for warehouse columns) first",
            )
        })?;
        let dialect_name = dialect
            .map(str::to_owned)
            .or_else(|| artifacts.manifest.adapter_type.clone())
            .unwrap_or_else(|| "generic".to_owned());
        let dialect = SqlDialect::from_name(&dialect_name).ok_or_else(|| {
            CliError::new(
                ExitStatus::Usage,
                codes::LINEAGE_TARGET,
                format!("unsupported SQL dialect `{dialect_name}`"),
            )
            .with_hint(format!(
                "pass --dialect with one of: {}",
                SqlDialect::ALL
                    .iter()
                    .map(|d| d.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        let analyzer = SqlparserAnalyzer::new(dialect);
        let (project, names) = project(&artifacts, &analyzer)?;
        let checksums = artifacts
            .manifest
            .nodes
            .iter()
            .map(|n| (n.unique_id.clone(), n.checksum.clone()))
            .collect();
        let (graph, stats) = build(&project, &analyzer, cache)
            .map_err(|e| CliError::new(ExitStatus::Failure, codes::LINEAGE_BUILD, e.to_string()))?;
        Ok(Self {
            target_dir: target_dir.to_owned(),
            analyzer,
            names,
            checksums,
            graph,
            stats,
            elapsed_ms: started.elapsed().as_millis(),
        })
    }

    /// Finds a node by `unique_id` or unique name.
    fn node_id(&self, name: &str) -> Result<String, CliError> {
        if self.graph.node(name).is_some() {
            return Ok(name.to_owned());
        }
        let matches: Vec<&String> = self
            .names
            .iter()
            .filter(|(_, n)| n.as_str() == name)
            .map(|(id, _)| id)
            .collect();
        match matches.as_slice() {
            [id] => Ok((*id).clone()),
            [] => Err(CliError::new(
                ExitStatus::Usage,
                codes::LINEAGE_TARGET,
                format!("no model, seed, snapshot or source is called `{name}`"),
            )),
            many => Err(CliError::new(
                ExitStatus::Usage,
                codes::LINEAGE_TARGET,
                format!("`{name}` is ambiguous"),
            )
            .with_hint(format!(
                "use a unique_id: {}",
                many.iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn display(&self, column: &ColumnRef) -> String {
        match self.graph.node_for(&column.relation) {
            Some(node) => format!(
                "{}.{}",
                self.names.get(&node.id).unwrap_or(&node.id),
                column.column
            ),
            None => column.to_string(),
        }
    }

    pub(super) fn node_name(&self, id: &str) -> String {
        self.names.get(id).cloned().unwrap_or_else(|| id.to_owned())
    }
}

/// Maps dbt nodes to the neutral lineage project.
fn project(
    artifacts: &Artifacts,
    analyzer: &SqlparserAnalyzer,
) -> Result<(LineageProject, BTreeMap<String, String>), CliError> {
    let catalog = artifacts.catalog.as_ref();
    let by_id: BTreeMap<&str, &ods_provider_dbt::ManifestNode> = artifacts
        .manifest
        .nodes
        .iter()
        .map(|n| (n.unique_id.as_str(), n))
        .collect();
    let included = |n: &ods_provider_dbt::ManifestNode| {
        kind(n.resource_type).is_some() && n.relation_name.is_some()
    };
    let mut nodes = Vec::new();
    let mut names = BTreeMap::new();
    for node in &artifacts.manifest.nodes {
        let (Some(kind), Some(relation_name)) = (kind(node.resource_type), &node.relation_name)
        else {
            continue;
        };
        let relation = analyzer.relation_name(relation_name).map_err(|e| {
            CliError::new(
                ExitStatus::Failure,
                codes::LINEAGE_ARTIFACTS,
                format!("{}: {e}", node.unique_id),
            )
        })?;
        // Ephemeral models have no relation: their SQL is inlined into consumers as a
        // CTE, so a consumer really depends on the ephemeral model's own upstreams.
        let mut depends_on = BTreeSet::new();
        let mut pending: Vec<&str> = node.depends_on.iter().map(String::as_str).collect();
        let mut seen = BTreeSet::new();
        while let Some(dep) = pending.pop() {
            if !seen.insert(dep) {
                continue;
            }
            match by_id.get(dep) {
                Some(upstream) if included(upstream) => {
                    depends_on.insert(dep.to_owned());
                }
                Some(upstream) => pending.extend(upstream.depends_on.iter().map(String::as_str)),
                None => {}
            }
        }
        let mut lineage_node =
            LineageNode::new(&node.unique_id, relation, kind).with_depends_on(depends_on);
        let sql_model =
            kind == NodeKind::Model && node.language.as_deref().unwrap_or("sql") == "sql";
        if sql_model && let Some(sql) = &node.compiled_code {
            lineage_node = lineage_node.with_sql(sql);
        }
        // Only warehouse catalog columns are a table's real, ordered schema. Columns
        // documented in YAML are often incomplete, so they are not used: an unknown schema
        // is handled conservatively rather than a partial one presented as complete.
        if let Some(columns) = catalog.and_then(|c| c.columns.get(&node.unique_id)) {
            lineage_node =
                lineage_node.with_columns(columns.iter().map(|c| analyzer.column_name(c)));
        }
        names.insert(
            node.unique_id.clone(),
            node.unique_id
                .rsplit('.')
                .next()
                .unwrap_or_default()
                .to_owned(),
        );
        nodes.push(lineage_node);
    }
    Ok((LineageProject::new(nodes), names))
}

fn kind(resource_type: ResourceType) -> Option<NodeKind> {
    match resource_type {
        ResourceType::Model => Some(NodeKind::Model),
        ResourceType::Seed => Some(NodeKind::Seed),
        ResourceType::Snapshot => Some(NodeKind::Snapshot),
        ResourceType::Source => Some(NodeKind::Source),
        _ => None,
    }
}

fn edge_name(edge: EdgeKind) -> String {
    match edge {
        EdgeKind::Direct(k) => k.openlineage_subtype().to_lowercase(),
        EdgeKind::Indirect(k) => format!("indirect:{}", k.openlineage_subtype().to_lowercase()),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct Summary {
    target_dir: PathBuf,
    dialect: &'static str,
    analyzer: String,
    models_analyzed: usize,
    models_opaque: usize,
    waves: usize,
    elapsed_ms: u128,
}

impl Summary {
    pub(super) fn of(loaded: &Loaded) -> Self {
        Self {
            target_dir: loaded.target_dir.clone(),
            dialect: loaded.analyzer.dialect().name(),
            analyzer: loaded.analyzer.analyzer_version(),
            models_analyzed: loaded.stats.analyzed + loaded.stats.cached,
            models_opaque: loaded.stats.opaque,
            waves: loaded.stats.waves,
            elapsed_ms: loaded.elapsed_ms,
        }
    }

    pub(super) fn view(&self) -> ViewNode {
        ViewNode::KeyValue(vec![
            (
                "artifacts".into(),
                vec![Span::toned(
                    self.target_dir.display().to_string(),
                    Tone::Code,
                )],
            ),
            ("dialect".into(), vec![Span::plain(self.dialect)]),
            (
                "models".into(),
                vec![Span::plain(format!(
                    "{} analyzed, {} opaque, {} dependency waves, {} ms",
                    self.models_analyzed, self.models_opaque, self.waves, self.elapsed_ms
                ))],
            ),
        ])
    }
}

// ---------------------------------------------------------------- columns

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ColumnsReport {
    summary: Summary,
    models: Vec<ModelColumns>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ModelColumns {
    unique_id: String,
    name: String,
    relation: String,
    confidence: ods_core::Confidence,
    opaque: bool,
    columns: Vec<ColumnInputs>,
    row_inputs: Vec<Input>,
    diagnostics: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ColumnInputs {
    name: String,
    inputs: Vec<Input>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Input {
    column: String,
    edge: EdgeKind,
}

impl ColumnsReport {
    fn build(loaded: &Loaded, model: Option<&str>) -> Result<Self, CliError> {
        let only = model.map(|m| loaded.node_id(m)).transpose()?;
        let models = loaded
            .graph
            .nodes()
            .filter(|n| only.as_ref().is_none_or(|id| *id == n.id))
            .filter_map(|n| {
                let lineage = n.lineage.as_ref()?;
                let input = |c: &ColumnRef, edge| Input {
                    column: loaded.display(c),
                    edge,
                };
                Some(ModelColumns {
                    unique_id: n.id.clone(),
                    name: loaded.node_name(&n.id),
                    relation: n.relation.to_string(),
                    confidence: lineage.confidence,
                    opaque: lineage.opaque,
                    columns: lineage
                        .outputs
                        .iter()
                        .map(|o| ColumnInputs {
                            name: o.name.clone(),
                            inputs: o.inputs.iter().map(|(c, e)| input(c, *e)).collect(),
                        })
                        .collect(),
                    row_inputs: lineage
                        .row_inputs
                        .iter()
                        .map(|(c, k)| input(c, EdgeKind::Indirect(*k)))
                        .collect(),
                    diagnostics: lineage.diagnostics.clone(),
                })
            })
            .collect();
        Ok(Self {
            summary: Summary::of(loaded),
            models,
        })
    }
}

impl Present for ColumnsReport {
    const COMMAND: &'static str = "lineage.columns";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading("Column lineage".into()),
            self.summary.view(),
        ];
        for model in &self.models {
            let mut children: Vec<TreeItem> = model
                .columns
                .iter()
                .map(|c| TreeItem {
                    label: vec![Span::toned(c.name.as_str(), Tone::Emphasis)],
                    children: c
                        .inputs
                        .iter()
                        .map(|i| {
                            TreeItem::leaf(vec![
                                Span::toned(i.column.as_str(), Tone::Code),
                                Span::toned(format!(" ({})", edge_name(i.edge)), Tone::Muted),
                            ])
                        })
                        .collect(),
                })
                .collect();
            if !model.row_inputs.is_empty() {
                children.push(TreeItem {
                    label: vec![Span::toned("rows shaped by", Tone::Muted)],
                    children: model
                        .row_inputs
                        .iter()
                        .map(|i| {
                            TreeItem::leaf(vec![
                                Span::toned(i.column.as_str(), Tone::Code),
                                Span::toned(format!(" ({})", edge_name(i.edge)), Tone::Muted),
                            ])
                        })
                        .collect(),
                });
            }
            let mut label = vec![Span::toned(model.name.as_str(), Tone::Code)];
            if model.opaque {
                label.push(Span::toned(
                    " opaque: every input may affect every column",
                    Tone::Warning,
                ));
            }
            for note in &model.diagnostics {
                children.push(TreeItem::leaf(vec![Span::toned(
                    note.as_str(),
                    Tone::Warning,
                )]));
            }
            blocks.push(ViewNode::Tree(TreeItem { label, children }));
        }
        ViewNode::Group(blocks)
    }
}

// ---------------------------------------------------------------- impact

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ImpactReport {
    summary: Summary,
    /// The changes analyzed.
    changes: Vec<Change>,
    /// Models whose own code changed.
    changed_models: Vec<String>,
    /// Every model that must run: changed plus impacted, sorted.
    run: Vec<String>,
    impact: Impact,
}

impl ImpactReport {
    fn for_columns(loaded: &Loaded, specs: &[&String]) -> Result<Self, CliError> {
        let mut changes = Vec::new();
        for spec in specs {
            let (target, kind) = match spec.split_once('=') {
                Some((target, kind)) => (target, kind),
                None => (spec.as_str(), "modified"),
            };
            let kind = match kind {
                "modified" => ColumnChangeKind::Modified,
                "added" => ColumnChangeKind::Added,
                "removed" => ColumnChangeKind::Removed,
                other => {
                    return Err(CliError::new(
                        ExitStatus::Usage,
                        codes::LINEAGE_TARGET,
                        format!(
                            "unknown change kind `{other}`; expected modified, added or removed"
                        ),
                    ));
                }
            };
            let (model, column) = target.rsplit_once('.').ok_or_else(|| {
                CliError::new(
                    ExitStatus::Usage,
                    codes::LINEAGE_TARGET,
                    format!("`{target}` is not MODEL.COLUMN"),
                )
            })?;
            let id = loaded.node_id(model)?;
            let relation = loaded
                .graph
                .node(&id)
                .map(|n| n.relation.clone())
                .ok_or_else(|| {
                    CliError::new(
                        ExitStatus::Failure,
                        codes::INTERNAL,
                        format!("`{id}` vanished from the graph"),
                    )
                })?;
            changes.push(Change::Column {
                column: ColumnRef::new(relation, loaded.analyzer.column_name(column)),
                kind,
            });
        }
        Ok(Self::finish(loaded, changes, Vec::new()))
    }

    fn against(head: &Loaded, base_dir: &Path) -> Result<Self, CliError> {
        // The base may be in either format (e.g. prod on dbt 1.x, a branch on v2). If the
        // formats differ, checksums of nodes without lineage differ too, so those count
        // as changed: conservative.
        let base = Loaded::from_dir(
            base_dir,
            Some(head.analyzer.dialect().name()),
            ArtifactPreference::Auto,
            &MemoryCache::default(),
        )?;
        let mut changes = Vec::new();
        let mut changed_models = Vec::new();
        for node in head.graph.nodes() {
            let before_node = base.graph.node(&node.id);
            let Some(after) = &node.lineage else {
                // No SQL lineage (seeds, snapshots, Python models): compare dbt's file
                // checksum; any difference, or a new node, may change every row.
                let same = before_node.is_some()
                    && base.checksums.get(&node.id) == head.checksums.get(&node.id);
                if !same {
                    changed_models.push(node.id.clone());
                    changes.push(Change::Rows {
                        relation: node.relation.clone(),
                    });
                }
                continue;
            };
            if before_node.and_then(|b| b.cache_key.as_ref()) == node.cache_key.as_ref() {
                continue;
            }
            let before = before_node.and_then(|b| b.lineage.as_ref());
            let node_changes = diff(&node.relation, before, after);
            if !node_changes.is_empty() {
                changed_models.push(node.id.clone());
                changes.extend(node_changes);
            }
        }
        Ok(Self::finish(head, changes, changed_models))
    }

    fn finish(loaded: &Loaded, changes: Vec<Change>, changed_models: Vec<String>) -> Self {
        let impact = loaded.graph.impact(&changes);
        let run: BTreeSet<String> = changed_models
            .iter()
            .cloned()
            .chain(impact.node_ids().map(str::to_owned))
            .collect();
        Self {
            summary: Summary::of(loaded),
            changes,
            changed_models,
            run: run.into_iter().collect(),
            impact,
        }
    }
}

fn change_word(kind: ColumnChangeKind) -> &'static str {
    match kind {
        ColumnChangeKind::Added => "added",
        ColumnChangeKind::Removed => "removed",
        ColumnChangeKind::Modified => "modified",
    }
}

impl Present for ImpactReport {
    const COMMAND: &'static str = "lineage.impact";

    fn view(&self) -> ViewNode {
        // Re-deriving names needs the graph, which isn't serialized; the view uses ids.
        let mut blocks = vec![
            ViewNode::Heading("Downstream impact".into()),
            self.summary.view(),
        ];
        blocks.push(ViewNode::KeyValue(vec![
            (
                "changes".into(),
                vec![Span::plain(self.changes.len().to_string())],
            ),
            (
                "must run".into(),
                vec![Span::toned(self.run.len().to_string(), Tone::Emphasis)],
            ),
            (
                "pruned".into(),
                vec![Span::toned(
                    self.impact
                        .pruned
                        .iter()
                        .map(|p| p.node.as_str())
                        .collect::<BTreeSet<_>>()
                        .len()
                        .to_string(),
                    Tone::Success,
                )],
            ),
        ]));
        if self.run.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain("nothing downstream is affected")],
            });
        }
        for id in &self.run {
            let mut children = Vec::new();
            if self.changed_models.contains(id) {
                children.push(TreeItem::leaf(vec![Span::toned(
                    "its SQL changed",
                    Tone::Emphasis,
                )]));
            }
            if let Some(node) = self.impact.nodes.get(id) {
                if node.rows_changed {
                    children.push(TreeItem::leaf(vec![Span::toned(
                        "all rows may change",
                        Tone::Warning,
                    )]));
                } else if !node.changed_columns.is_empty() {
                    children.push(TreeItem::leaf(vec![
                        Span::plain("changed columns: "),
                        Span::toned(
                            node.changed_columns
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", "),
                            Tone::Code,
                        ),
                    ]));
                }
                children.extend(node.reasons.iter().map(|r| TreeItem::leaf(reason_line(r))));
            }
            blocks.push(ViewNode::Tree(TreeItem {
                label: vec![Span::toned(id.as_str(), Tone::Code)],
                children,
            }));
        }
        if !self.impact.pruned.is_empty() {
            blocks.push(ViewNode::Table {
                title: Some(
                    "Skipped: they read changed models but none of the changed columns".into(),
                ),
                columns: vec![
                    "model".into(),
                    "reads".into(),
                    "unused changed columns".into(),
                ],
                rows: self
                    .impact
                    .pruned
                    .iter()
                    .map(|p| {
                        vec![
                            vec![Span::toned(p.node.as_str(), Tone::Code)],
                            vec![Span::plain(p.upstream.to_string())],
                            vec![Span::plain(
                                p.unused_changed_columns
                                    .iter()
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            )],
                        ]
                    })
                    .collect(),
            });
        }
        ViewNode::Group(blocks)
    }
}

fn reason_line(reason: &ImpactReason) -> Vec<Span> {
    match reason {
        ImpactReason::Column {
            upstream,
            change,
            output,
            edge,
        } => {
            let target = output
                .as_deref()
                .map_or_else(|| "its rows".to_owned(), |o| format!("`{o}`"));
            vec![
                Span::toned(upstream.to_string(), Tone::Code),
                Span::plain(format!(
                    " {} → {target} ({})",
                    change_word(*change),
                    edge_name(*edge)
                )),
            ]
        }
        ImpactReason::Rows { upstream } => vec![
            Span::plain("rows of "),
            Span::toned(upstream.to_string(), Tone::Code),
            Span::plain(" may differ"),
        ],
        ImpactReason::Wildcard { upstream } => vec![
            Span::plain("selects * and gains "),
            Span::toned(upstream.to_string(), Tone::Code),
        ],
        ImpactReason::NameCapture { upstream } => vec![
            Span::plain("uses a column named like new "),
            Span::toned(upstream.to_string(), Tone::Code),
            Span::plain("; an unqualified reference may now bind to it"),
        ],
        ImpactReason::Opaque { upstream } => vec![
            Span::toned("lineage unknown", Tone::Warning),
            Span::plain(", reads changed "),
            Span::toned(upstream.to_string(), Tone::Code),
        ],
        other => vec![Span::plain(format!("{other:?}"))],
    }
}

// ---------------------------------------------------------------- export

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ExportReport {
    summary: Summary,
    output_file: PathBuf,
    events: usize,
    with_column_lineage: usize,
    event_type: &'static str,
}

impl ExportReport {
    fn write(loaded: &Loaded, args: &ArgMatches) -> Result<Self, CliError> {
        let path = PathBuf::from(
            args.get_one::<String>("output-file")
                .map_or("", String::as_str),
        );
        let namespace = args
            .get_one::<String>("namespace")
            .cloned()
            .unwrap_or_default();
        let job_namespace = args
            .get_one::<String>("job-namespace")
            .cloned()
            .unwrap_or_default();
        let event_time = args
            .get_one::<String>("event-time")
            .cloned()
            .unwrap_or_else(now_rfc3339);
        let run_events = args.get_flag("run-events");
        let mut options = ExportOptions::new(namespace, job_namespace, event_time);
        if run_events {
            options = options.with_events(EventKind::Run);
        }
        if args.get_flag("indirect-in-fields") {
            options = options.with_indirect(IndirectPlacement::DatasetAndFields);
        }
        let events = ods_lineage::openlineage::events(&loaded.graph, &options);
        let with_column_lineage = events
            .iter()
            .filter(|e| e["outputs"][0].get("facets").is_some())
            .count();
        let mut text = String::new();
        for event in &events {
            text.push_str(&event.to_string());
            text.push('\n');
        }
        let write_error = |e: std::io::Error| {
            CliError::new(
                ExitStatus::Failure,
                codes::LINEAGE_ARTIFACTS,
                format!("cannot write `{}`: {e}", path.display()),
            )
        };
        let mut file = fs::File::create(&path).map_err(write_error)?;
        file.write_all(text.as_bytes()).map_err(write_error)?;
        Ok(Self {
            summary: Summary::of(loaded),
            output_file: path,
            events: events.len(),
            with_column_lineage,
            event_type: if run_events { "RunEvent" } else { "JobEvent" },
        })
    }
}

impl Present for ExportReport {
    const COMMAND: &'static str = "lineage.export";

    fn view(&self) -> ViewNode {
        ViewNode::Group(vec![
            ViewNode::Heading("OpenLineage export".into()),
            self.summary.view(),
            ViewNode::KeyValue(vec![
                (
                    "file".into(),
                    vec![Span::toned(
                        self.output_file.display().to_string(),
                        Tone::Code,
                    )],
                ),
                (
                    "events".into(),
                    vec![Span::plain(format!(
                        "{} {}s, {} with column lineage",
                        self.events, self.event_type, self.with_column_lineage
                    ))],
                ),
            ]),
        ])
    }
}

/// The current UTC time as RFC 3339, without a date-time dependency.
fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ---------------------------------------------------------------- graph and view

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct GraphReport {
    summary: Summary,
    format: String,
    output_file: PathBuf,
    nodes: usize,
    node_edges: usize,
    column_edges: usize,
    opened: bool,
}

impl GraphReport {
    fn write(loaded: &Loaded, args: &ArgMatches, viewer: bool) -> Result<Self, CliError> {
        let mut focus = Vec::new();
        for spec in args.get_many::<String>("focus").into_iter().flatten() {
            focus.push(loaded.endpoint(spec)?);
        }
        let filter = GraphFilter::focused(focus).with_depth(
            args.get_one::<usize>("upstream").copied(),
            args.get_one::<usize>("downstream").copied(),
        );
        let document = loaded.graph.document(&|id| loaded.node_name(id), &filter);
        let format = if viewer {
            "html".to_owned()
        } else {
            args.get_one::<String>("format")
                .cloned()
                .unwrap_or_else(|| "json".into())
        };
        if viewer && let Some(dir) = args.get_one::<String>("site") {
            return Self::site(loaded, &document, Path::new(dir));
        }
        let text = match format.as_str() {
            "html" => ods_web::standalone_page(&document)
                .map_err(|e| CliError::new(ExitStatus::Failure, codes::INTERNAL, e.to_string()))?,
            "json" => serde_json::to_string_pretty(&document)
                .map_err(|e| CliError::new(ExitStatus::Failure, codes::INTERNAL, e.to_string()))?,
            "dot" => document.to_dot(false),
            "dot-columns" => document.to_dot(true),
            "mermaid" => document.to_mermaid(),
            _ => document.to_graphml(),
        };
        let path = PathBuf::from(
            args.get_one::<String>("output-file")
                .map_or("lineage.html", String::as_str),
        );
        fs::write(&path, text).map_err(|e| {
            CliError::new(
                ExitStatus::Failure,
                codes::LINEAGE_ARTIFACTS,
                format!("cannot write `{}`: {e}", path.display()),
            )
        })?;
        let opened = viewer && args.get_flag("open") && open_in_browser(&path);
        Ok(Self {
            summary: Summary::of(loaded),
            format,
            output_file: path,
            nodes: document.nodes.len(),
            node_edges: document.node_edges.len(),
            column_edges: document.column_edges.len(),
            opened,
        })
    }
}

impl GraphReport {
    fn site(
        loaded: &Loaded,
        document: &ods_lineage::GraphDocument,
        dir: &Path,
    ) -> Result<Self, CliError> {
        let files = ods_web::export_site(document, dir).map_err(|e| {
            CliError::new(
                ExitStatus::Failure,
                codes::LINEAGE_ARTIFACTS,
                format!("cannot write the site to `{}`: {e}", dir.display()),
            )
        })?;
        Ok(Self {
            summary: Summary::of(loaded),
            format: "site".to_owned(),
            output_file: files
                .into_iter()
                .next()
                .unwrap_or_else(|| dir.join("index.html")),
            nodes: document.nodes.len(),
            node_edges: document.node_edges.len(),
            column_edges: document.column_edges.len(),
            opened: false,
        })
    }
}

impl Present for GraphReport {
    const COMMAND: &'static str = "lineage.graph";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading("Lineage graph".into()),
            self.summary.view(),
            ViewNode::KeyValue(vec![
                (
                    "file".into(),
                    vec![Span::toned(
                        self.output_file.display().to_string(),
                        Tone::Code,
                    )],
                ),
                ("format".into(), vec![Span::plain(self.format.as_str())]),
                (
                    "graph".into(),
                    vec![Span::plain(format!(
                        "{} nodes, {} node edges, {} column edges",
                        self.nodes, self.node_edges, self.column_edges
                    ))],
                ),
            ]),
        ];
        if self.format == "html" && !self.opened {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(format!(
                    "open {} in a browser; it works offline",
                    self.output_file.display()
                ))],
            });
        }
        if self.format == "site" {
            // Browsers refuse to fetch graph.json from file:// pages, so say how to host it.
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(
                    "serve the directory with any static web server; \
                     browsers won't load graph.json from a file:// page",
                )],
            });
        }
        ViewNode::Group(blocks)
    }
}

/// Best effort: returns whether a browser launcher started.
fn open_in_browser(path: &Path) -> bool {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

impl Loaded {
    /// Parses `MODEL` or `MODEL.COLUMN` into a graph endpoint.
    fn endpoint(&self, spec: &str) -> Result<Endpoint, CliError> {
        if let Ok(node) = self.node_id(spec) {
            return Ok(Endpoint { node, column: None });
        }
        let (model, column) = spec.rsplit_once('.').ok_or_else(|| {
            CliError::new(
                ExitStatus::Usage,
                codes::LINEAGE_TARGET,
                format!("no model, seed, snapshot or source is called `{spec}`"),
            )
        })?;
        let node = self.node_id(model)?;
        let column = self.analyzer.column_name(column);
        let known = self
            .graph
            .node(&node)
            .is_some_and(|n| n.columns.contains(&column));
        if !known {
            return Err(CliError::new(
                ExitStatus::Usage,
                codes::LINEAGE_TARGET,
                format!("`{model}` has no column `{column}`"),
            ));
        }
        Ok(Endpoint {
            node,
            column: Some(column),
        })
    }
}
