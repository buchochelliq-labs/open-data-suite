//! `ods erd`: planned for M3. `ods erd generate` already draws an entity-relationship
//! diagram from a dbt project's tests and constraints (#60–#63, ADR-0012); the other
//! subcommands report "not implemented".
//!
//! This adapter maps dbt specifics (test names, constraint syntax, node ids) to the
//! neutral facts `ods-erd` understands.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_erd::{Basis, BuildOptions, ColumnInput, EntityInput, EntityKind, Erd, Fact, build};
use ods_provider_dbt::{ArtifactPreference, Artifacts, DbtConstraint, ManifestNode, ResourceType};
use serde::Serialize;

use super::Planned;
use super::lineage::{LoadOptions, Loaded, shared_cache};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};
use crate::output::Mode;
use crate::present::{Level, Present, Span, Tone, ViewNode};

const PASSTHROUGH: &str = "args";
const ABOUT: &str = "Generate and inspect entity-relationship models";
const MILESTONE: &str = "M3 ERD & Usage (v0.3.0)";

/// `ods erd`.
pub struct ErdCommand;

impl Module for ErdCommand {
    fn command(&self) -> Command {
        Command::new("erd")
            .about(format!(
                "{ABOUT} [planned: {MILESTONE}] (`ods erd generate` is available)"
            ))
            .args_conflicts_with_subcommands(true)
            .subcommand(generate_command())
            .arg(
                Arg::new(PASSTHROUGH)
                    .num_args(0..)
                    .trailing_var_arg(true)
                    .allow_hyphen_values(true)
                    .hide(true),
            )
    }

    fn passthrough_arg(&self) -> Option<&'static str> {
        Some(PASSTHROUGH)
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        match matches.subcommand() {
            Some(("generate", args)) => generate(args, ctx),
            _ => Planned::new("erd", ABOUT, MILESTONE).run(matches, ctx),
        }
    }
}

fn generate_command() -> Command {
    Command::new("generate")
        .about("Draw the entity-relationship diagram implied by the project's tests and constraints")
        .arg(
            Arg::new("target-dir")
                .long("target-dir")
                .value_name("DIR")
                .default_value("target")
                .help("dbt target directory with manifest.json (and catalog.json for column types) or dbt v2's Information Schema"),
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
            Arg::new("dialect")
                .long("dialect")
                .value_name("DIALECT")
                .help("SQL dialect for reading joins; defaults to the manifest's adapter type"),
        )
        .arg(
            Arg::new("format")
                .long("format")
                .value_name("FORMAT")
                .value_parser(["mermaid", "dot", "json"])
                .default_value("mermaid")
                .help("mermaid (Markdown), dot (Graphviz) or json"),
        )
        .arg(
            Arg::new("select")
                .long("select")
                .value_name("MODEL")
                .action(ArgAction::Append)
                .help("Only this model (name or unique_id) and its related entities; repeatable"),
        )
        .arg(
            Arg::new("depth")
                .long("depth")
                .value_name("N")
                .value_parser(clap::value_parser!(usize))
                .default_value("1")
                .help("With --select, how many relationships away to include"),
        )
        .arg(
            Arg::new("infer")
                .long("infer")
                .action(ArgAction::SetTrue)
                .help("Also propose keys and relationships from naming (`id`, `<entity>_id`); shown as inferred"),
        )
        .arg(
            Arg::new("all")
                .long("all")
                .action(ArgAction::SetTrue)
                .help("Include entities without any relationship"),
        )
        .arg(
            Arg::new("output-file")
                .long("output-file")
                .value_name("PATH")
                .help("Write the diagram here instead of printing it"),
        )
}

/// What to draw.
pub(super) struct ErdOptions {
    pub(super) select: Vec<String>,
    pub(super) depth: usize,
    pub(super) infer: bool,
    pub(super) all: bool,
}

/// Loads artifacts and builds the (filtered) diagram. Joins in the project's SQL are
/// evidence too, so the project is analyzed; if that fails, the diagram is still drawn
/// from tests and constraints, with a diagnostic.
pub(super) fn project_erd(
    target_dir: &std::path::Path,
    load: &LoadOptions,
    options: &ErdOptions,
) -> Result<Erd, CliError> {
    let artifacts = Artifacts::load_with(target_dir, load.preference).map_err(|e| {
        CliError::new(ExitStatus::Failure, codes::LINEAGE_ARTIFACTS, e.to_string()).with_hint(
            "run `dbt parse` or `dbt compile` (and `dbt docs generate` for column types) first",
        )
    })?;
    let loaded = Loaded::from_dir(target_dir, load, shared_cache());
    let (entities, facts, mut diagnostics) = erd_input(&artifacts, loaded.as_ref().ok());
    if let Err(e) = &loaded {
        diagnostics.push(format!(
            "joins in the SQL weren't used as evidence: {}",
            e.message
        ));
    }
    let mut erd = build(
        &entities,
        &facts,
        BuildOptions::default().with_inference(options.infer),
    );
    erd.diagnostics.append(&mut diagnostics);
    erd.diagnostics.sort();
    for wanted in &options.select {
        if erd.entity(wanted).is_none() {
            return Err(CliError::new(
                ExitStatus::Usage,
                codes::LINEAGE_TARGET,
                format!("no model, seed, snapshot or source is called `{wanted}`"),
            ));
        }
    }
    let erd = erd.focused(&options.select, options.depth);
    Ok(if options.all || !options.select.is_empty() {
        erd
    } else {
        erd.connected_only()
    })
}

/// A node's display name: `model.pkg.orders` → `orders`, `source.pkg.raw.orders` →
/// `raw.orders`.
fn display_name(node: &ManifestNode) -> String {
    let parts: Vec<&str> = node.unique_id.split('.').collect();
    // `<type>.<package>.<name…>`; a source's name is `<source>.<table>`.
    parts
        .get(2..)
        .map_or_else(|| node.unique_id.clone(), |p| p.join("."))
}

fn entity_kind(node: &ManifestNode) -> Option<EntityKind> {
    match node.resource_type {
        ResourceType::Model => match node.materialized.as_deref() {
            Some("ephemeral") => None,
            Some("view") => Some(EntityKind::View),
            _ => Some(EntityKind::Table),
        },
        ResourceType::Seed => Some(EntityKind::Seed),
        ResourceType::Snapshot => Some(EntityKind::Snapshot),
        ResourceType::Source => Some(EntityKind::Source),
        _ => None,
    }
}

/// Node ids by display name; `None` where two nodes share a name, so an ambiguous
/// reference resolves to nothing rather than to the wrong node.
type ByName = BTreeMap<String, Option<String>>;

/// `ref('orders')`, `ref('pkg', 'orders')`, `ref('orders', v=2)`,
/// `source('raw', 'orders')` → the node id.
fn resolve_reference(text: &str, by_name: &ByName) -> Option<String> {
    let text = text.trim();
    let args = |prefix: &str| -> Option<Vec<String>> {
        let inner = text.strip_prefix(prefix)?.trim_start().strip_prefix('(')?;
        let inner = inner.strip_suffix(')')?;
        Some(
            inner
                .split(',')
                .map(|a| a.trim().trim_matches(|c| c == '\'' || c == '"').to_owned())
                .filter(|a| !a.is_empty() && !a.contains('='))
                .collect(),
        )
    };
    if let Some(args) = args("ref") {
        return by_name.get(args.last()?).cloned().flatten();
    }
    if let Some(args) = args("source") {
        return by_name.get(&args.join(".")).cloned().flatten();
    }
    by_name.get(text).cloned().flatten()
}

/// The dot-separated parts of a warehouse identifier, unquoted and lowercased:
/// `"db"."Main".orders` → `[db, main, orders]`.
fn identifier_parts(text: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut quote: Option<char> = None;
    for c in text.trim().chars() {
        match (quote, c) {
            (None, '"' | '`' | '[') => quote = Some(if c == '[' { ']' } else { c }),
            (Some(q), _) if c == q => quote = None,
            (None, '.') => parts.push(String::new()),
            (None, c) if c.is_whitespace() => {}
            (_, c) => parts
                .last_mut()
                .expect("never empty")
                .extend(c.to_lowercase()),
        }
    }
    parts
}

/// Node ids by every trailing part of their warehouse relation (`orders`,
/// `main.orders`, `db.main.orders`), for constraints that name a table instead of a
/// `ref()`. `None` marks an ambiguous name.
fn relation_index(artifacts: &Artifacts) -> ByName {
    let mut index = ByName::new();
    for node in &artifacts.manifest.nodes {
        let Some(relation) = &node.relation_name else {
            continue;
        };
        let parts = identifier_parts(relation);
        for start in 0..parts.len() {
            index
                .entry(parts[start..].join("."))
                .and_modify(|id| {
                    if id.as_deref() != Some(node.unique_id.as_str()) {
                        *id = None;
                    }
                })
                .or_insert_with(|| Some(node.unique_id.clone()));
        }
    }
    index
}

/// The target and columns of a foreign key written as an expression,
/// `schema.table (column, …)` (dbt before `to`/`to_columns`, and still accepted).
fn parse_reference_expression(
    expression: &str,
    relations: &ByName,
) -> Option<(String, Vec<String>)> {
    let (table, rest) = expression.trim().split_once('(')?;
    let columns: Vec<String> = rest
        .strip_suffix(')')?
        .split(',')
        .map(|c| identifier_parts(c).join("."))
        .filter(|c| !c.is_empty())
        .collect();
    let target = relations
        .get(&identifier_parts(table).join("."))
        .cloned()
        .flatten()?;
    (!columns.is_empty()).then_some((target, columns))
}

/// Entities (models, seeds, snapshots, sources) with their columns, and node ids by
/// display name for resolving `ref()`/`source()` references.
fn entities_of(artifacts: &Artifacts) -> (Vec<EntityInput>, ByName) {
    let catalog = artifacts.catalog.as_ref();
    let mut entities = Vec::new();
    let mut by_name = ByName::new();
    for node in &artifacts.manifest.nodes {
        let Some(kind) = entity_kind(node) else {
            continue;
        };
        let columns: Vec<ColumnInput> = match catalog.and_then(|c| c.columns.get(&node.unique_id)) {
            Some(columns) => {
                let types = catalog.and_then(|c| c.types.get(&node.unique_id));
                columns
                    .iter()
                    .map(|c| {
                        ColumnInput::new(
                            c.clone(),
                            types
                                .and_then(|t| t.get(c).cloned())
                                .or_else(|| node.declared_types.get(c).cloned()),
                        )
                        .with_description(node.column_descriptions.get(c).cloned())
                    })
                    .collect()
            }
            None => node
                .declared_columns
                .iter()
                .map(|c| {
                    ColumnInput::new(c.clone(), node.declared_types.get(c).cloned())
                        .with_description(node.column_descriptions.get(c).cloned())
                })
                .collect(),
        };
        let name = display_name(node);
        by_name
            .entry(name.clone())
            .and_modify(|id| *id = None)
            .or_insert_with(|| Some(node.unique_id.clone()));
        entities.push(
            EntityInput::new(node.unique_id.clone(), name, kind, columns)
                .with_description(node.description.clone())
                .with_relation(node.relation_name.clone()),
        );
    }
    (entities, by_name)
}

/// Entities, facts and anything that couldn't be mapped.
fn erd_input(
    artifacts: &Artifacts,
    loaded: Option<&Loaded>,
) -> (Vec<EntityInput>, Vec<Fact>, Vec<String>) {
    let (entities, by_name) = entities_of(artifacts);
    let relations = relation_index(artifacts);
    let known: std::collections::BTreeSet<&str> = entities.iter().map(|e| e.id.as_str()).collect();
    let mut facts = Vec::new();
    let mut diagnostics = Vec::new();
    for node in &artifacts.manifest.nodes {
        if known.contains(node.unique_id.as_str()) {
            for constraint in &node.constraints {
                let names = (&by_name, &relations);
                constraint_facts(node, constraint, names, &mut facts, &mut diagnostics);
            }
            // Incremental models and snapshots name the key dbt merges on; often
            // several columns.
            if !node.config.unique_key.is_empty() {
                facts.push(Fact::PrimaryKey {
                    entity: node.unique_id.clone(),
                    columns: node.config.unique_key.clone(),
                    basis: Basis::Declared,
                    evidence: format!("config unique_key on {}", display_name(node)),
                });
            }
        }
        test_facts(node, &known, &by_name, &mut facts, &mut diagnostics);
    }
    if let Some(loaded) = loaded {
        facts.extend(join_facts(loaded, &known));
    }
    (entities, facts, diagnostics)
}

/// Joins the project's SQL makes (`a.x = b.y`), between entities of the diagram.
fn join_facts(loaded: &Loaded, known: &std::collections::BTreeSet<&str>) -> Vec<Fact> {
    let mut facts = Vec::new();
    for node in loaded.graph.nodes() {
        let Some(lineage) = &node.lineage else {
            continue;
        };
        for join in &lineage.join_keys {
            let entity = |columns: &[ods_core::ColumnRef]| {
                let relation = &columns.first()?.relation;
                let id = loaded.graph.node_for(relation)?.id.clone();
                known.contains(id.as_str()).then_some(id)
            };
            let (Some(left), Some(right)) = (entity(&join.left), entity(&join.right)) else {
                continue;
            };
            facts.push(Fact::Joined {
                left,
                left_columns: join.left.iter().map(|c| c.column.clone()).collect(),
                right,
                right_columns: join.right.iter().map(|c| c.column.clone()).collect(),
                evidence: node.id.clone(),
            });
        }
    }
    facts
}

/// What a data test asserts about keys: `unique`, `not_null`, `relationships`, and
/// `dbt_utils.unique_combination_of_columns`. Other tests say nothing about keys.
fn test_facts(
    node: &ManifestNode,
    known: &std::collections::BTreeSet<&str>,
    by_name: &ByName,
    facts: &mut Vec<Fact>,
    diagnostics: &mut Vec<String>,
) {
    let Some(test) = &node.test else {
        return;
    };
    let evidence = node.unique_id.clone();
    // dbt leaves `attached_node` empty for tests on sources: fall back to the only
    // entity the test depends on, or to the `ref()`/`source()` it tests.
    let attached = test
        .attached_node
        .clone()
        .filter(|a| known.contains(a.as_str()))
        .or_else(|| {
            let deps: Vec<&String> = node
                .depends_on
                .iter()
                .filter(|d| known.contains(d.as_str()))
                .collect();
            match deps.as_slice() {
                [one] => Some((*one).clone()),
                _ => test.arguments["model"].as_str().and_then(|m| {
                    let start = m.find("ref(").or_else(|| m.find("source("))?;
                    let end = start + m[start..].find(')')? + 1;
                    resolve_reference(&m[start..end], by_name)
                }),
            }
        });
    let Some(attached) = attached else {
        if matches!(test.name.as_str(), "unique" | "not_null" | "relationships") {
            diagnostics.push(format!("{evidence}: can't tell which model it tests"));
        }
        return;
    };
    // A filtered test only checks the rows it keeps: not a fact about the table.
    if let Some(filter) = &test.where_clause {
        if matches!(test.name.as_str(), "unique" | "not_null" | "relationships")
            || test.name == "unique_combination_of_columns"
        {
            diagnostics.push(format!(
                "{evidence}: only checks rows where `{filter}`, so it isn't used as a key"
            ));
        }
        return;
    }
    let column = test.column_name.clone();
    match (test.namespace.as_deref(), test.name.as_str(), column) {
        (None, "unique", Some(column)) => facts.push(Fact::Unique {
            entity: attached,
            columns: vec![column],
            basis: Basis::Tested,
            evidence,
        }),
        (None, "not_null", Some(column)) => facts.push(Fact::NotNull {
            entity: attached,
            column,
            basis: Basis::Tested,
            evidence,
        }),
        (None, "relationships", Some(column)) => {
            // The target is the test's other dependency; else its `to` argument.
            let others: Vec<&String> = node
                .depends_on
                .iter()
                .filter(|d| **d != attached && known.contains(d.as_str()))
                .collect();
            let target = match others.as_slice() {
                [one] => Some((*one).clone()),
                _ => test.arguments["to"]
                    .as_str()
                    .and_then(|to| resolve_reference(to, by_name)),
            };
            match (target, test.arguments["field"].as_str()) {
                (Some(to), Some(field)) => facts.push(Fact::ForeignKey {
                    entity: attached,
                    columns: vec![column],
                    to,
                    to_columns: vec![field.to_owned()],
                    basis: Basis::Tested,
                    evidence,
                }),
                _ => diagnostics.push(format!("{evidence}: can't tell what it refers to")),
            }
        }
        (Some("dbt_utils"), "unique_combination_of_columns", _) => {
            let columns: Vec<String> = test.arguments["combination_of_columns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(str::to_owned))
                .collect();
            if !columns.is_empty() {
                facts.push(Fact::Unique {
                    entity: attached,
                    columns,
                    basis: Basis::Tested,
                    evidence,
                });
            }
        }
        _ => {}
    }
}

fn constraint_facts(
    node: &ManifestNode,
    constraint: &DbtConstraint,
    (by_name, relations): (&ByName, &ByName),
    facts: &mut Vec<Fact>,
    diagnostics: &mut Vec<String>,
) {
    let entity = node.unique_id.clone();
    let evidence = format!("constraint {} on {}", constraint.kind, display_name(node));
    let columns = constraint.columns.clone();
    match constraint.kind.as_str() {
        "primary_key" if !columns.is_empty() => facts.push(Fact::PrimaryKey {
            entity,
            columns,
            basis: Basis::Declared,
            evidence,
        }),
        "unique" if !columns.is_empty() => facts.push(Fact::Unique {
            entity,
            columns,
            basis: Basis::Declared,
            evidence,
        }),
        "not_null" => {
            for column in columns {
                facts.push(Fact::NotNull {
                    entity: entity.clone(),
                    column,
                    basis: Basis::Declared,
                    evidence: evidence.clone(),
                });
            }
        }
        "foreign_key" => {
            let target = match constraint.to.as_deref() {
                Some(to) => {
                    resolve_reference(to, by_name).map(|id| (id, constraint.to_columns.clone()))
                }
                None => constraint
                    .expression
                    .as_deref()
                    .and_then(|e| parse_reference_expression(e, relations)),
            };
            match target {
                Some((to, to_columns)) if !columns.is_empty() => facts.push(Fact::ForeignKey {
                    entity,
                    columns,
                    to,
                    to_columns,
                    basis: Basis::Declared,
                    evidence,
                }),
                _ => diagnostics.push(format!(
                    "{evidence}: can't tell what it refers to{}",
                    constraint
                        .expression
                        .as_deref()
                        .map(|e| format!(" (`{e}`)"))
                        .unwrap_or_default()
                )),
            }
        }
        _ => {}
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct ErdReport {
    pub(super) target_dir: PathBuf,
    pub(super) format: String,
    pub(super) entities: usize,
    pub(super) relationships: usize,
    pub(super) declared: usize,
    pub(super) tested: usize,
    pub(super) joined: usize,
    pub(super) inferred: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) output_file: Option<PathBuf>,
    pub(super) rendered: String,
    pub(super) erd: Erd,
}

impl ErdReport {
    pub(super) fn new(target_dir: PathBuf, format: &str, erd: Erd) -> Result<Self, CliError> {
        let rendered = match format {
            "dot" => erd.to_dot(),
            "json" => erd
                .to_json()
                .map_err(|e| CliError::new(ExitStatus::Failure, codes::INTERNAL, e.to_string()))?,
            _ => erd.to_mermaid(),
        };
        let count = |basis: Basis| {
            erd.relationships
                .iter()
                .filter(|r| r.basis == basis)
                .count()
        };
        Ok(Self {
            target_dir,
            format: format.to_owned(),
            entities: erd.entities.len(),
            relationships: erd.relationships.len(),
            declared: count(Basis::Declared),
            tested: count(Basis::Tested),
            joined: count(Basis::Joined),
            inferred: count(Basis::Inferred),
            output_file: None,
            rendered,
            erd,
        })
    }
}

fn generate(args: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
    let target_dir = PathBuf::from(
        args.get_one::<String>("target-dir")
            .map_or("target", String::as_str),
    );
    let preference = match args.get_one::<String>("artifacts").map(String::as_str) {
        Some("json") => ArtifactPreference::Json,
        Some("info-schema") => ArtifactPreference::InfoSchema,
        _ => ArtifactPreference::Auto,
    };
    let options = ErdOptions {
        select: args
            .get_many::<String>("select")
            .into_iter()
            .flatten()
            .cloned()
            .collect(),
        depth: args.get_one::<usize>("depth").copied().unwrap_or(1),
        infer: args.get_flag("infer"),
        all: args.get_flag("all"),
    };
    let format = args
        .get_one::<String>("format")
        .map_or("mermaid", String::as_str);
    let load = LoadOptions {
        dialect: args.get_one::<String>("dialect").cloned(),
        preference,
        observed: None,
        trust_observed: false,
    };
    let erd = project_erd(&target_dir, &load, &options)?;
    let mut report = ErdReport::new(target_dir, format, erd)?;
    if let Some(path) = args.get_one::<String>("output-file") {
        let path = PathBuf::from(path);
        fs::write(&path, &report.rendered).map_err(|e| {
            CliError::new(
                ExitStatus::Failure,
                codes::OUTPUT_WRITE,
                format!("cannot write `{}`: {e}", path.display()),
            )
        })?;
        report.output_file = Some(path);
        return ctx.emit(&report);
    }
    if ctx.output.mode == Mode::Json {
        return ctx.emit(&report);
    }
    // Like a completion script: the diagram itself, ready to paste or pipe.
    ctx.raw_out()
        .write_all(report.rendered.as_bytes())
        .map_err(|e| CliError::new(ExitStatus::Failure, codes::OUTPUT_WRITE, e.to_string()))
}

impl Present for ErdReport {
    const COMMAND: &'static str = "erd.generate";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading("Entity-relationship diagram".into()),
            ViewNode::KeyValue(vec![
                (
                    "artifacts".into(),
                    vec![Span::toned(
                        self.target_dir.display().to_string(),
                        Tone::Code,
                    )],
                ),
                (
                    "diagram".into(),
                    vec![Span::plain(format!(
                        "{} entities, {} relationships ({} declared, {} tested, {} joined in SQL, {} inferred)",
                        self.entities,
                        self.relationships,
                        self.declared,
                        self.tested,
                        self.joined,
                        self.inferred
                    ))],
                ),
            ]),
        ];
        if let Some(path) = &self.output_file {
            blocks.push(ViewNode::KeyValue(vec![(
                "file".into(),
                vec![Span::toned(path.display().to_string(), Tone::Code)],
            )]));
        }
        for diagnostic in &self.erd.diagnostics {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(diagnostic.as_str())],
            });
        }
        ViewNode::Group(blocks)
    }
}
