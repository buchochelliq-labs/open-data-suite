//! The tools `ods mcp` serves. All read-only and local.
//!
//! Tools that mirror a CLI command run it in-process with `--json` and return its
//! `result`, so the MCP contract is the CLI's JSON contract. Values from the agent are
//! passed as `--flag=value`, a single argument, so they can never be read as flags.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use ods_core::{ColumnRef, IndirectKind};
use ods_erd::{Basis, Erd};
use ods_lineage::GraphFilter;
use ods_mcp::{Tool, ToolDefinition, ToolOutput};
use ods_provider_dbt::state_config::resolve;
use ods_provider_dbt::{ArtifactPreference, Artifacts, ResourceType};
use serde_json::{Value, json};

use super::erd::{ErdOptions, ErdReport, project_erd};
use super::lineage::{LoadOptions, Loaded, Summary, shared_cache};
use crate::app::{Io, run};
use crate::exit::CliError;

/// The project the server answers about.
pub(super) struct Project {
    target_dir: PathBuf,
    load: LoadOptions,
    preference: ArtifactPreference,
    artifacts_flag: String,
}

impl Project {
    pub(super) fn new(
        target_dir: PathBuf,
        load: LoadOptions,
        preference: ArtifactPreference,
        artifacts_flag: String,
    ) -> Self {
        Self {
            target_dir,
            load,
            preference,
            artifacts_flag,
        }
    }

    /// Reads and analyzes the project now; artifacts may have changed since the last
    /// call. Unchanged models come from the shared cache.
    pub(super) fn load(&self) -> Result<Loaded, CliError> {
        Loaded::from_dir(&self.target_dir, &self.load, shared_cache())
    }

    fn artifact_args(&self) -> Vec<String> {
        vec![
            format!("--target-dir={}", self.target_dir.display()),
            format!("--artifacts={}", self.artifacts_flag),
        ]
    }

    fn lineage_args(&self) -> Vec<String> {
        let mut args = self.artifact_args();
        if let Some(dialect) = &self.load.dialect {
            args.push(format!("--dialect={dialect}"));
        }
        if let Some(observed) = &self.load.observed {
            args.push(format!("--observed={}", observed.display()));
            if self.load.trust_observed {
                args.push("--trust-observed".into());
            }
        }
        args
    }
}

fn error_text(error: &CliError) -> String {
    match &error.hint {
        Some(hint) => format!("{} ({}); {hint}", error.message, error.code),
        None => format!("{} ({})", error.message, error.code),
    }
}

/// Runs `ods <args> --json` in-process and returns its result, or its diagnostics as a
/// tool error.
fn run_cli(args: &[String]) -> ToolOutput {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    run(
        &super::default_registry(),
        std::iter::once("ods".to_owned())
            .chain(args.iter().cloned())
            .chain(std::iter::once("--json".to_owned())),
        &mut Io {
            out: &mut out,
            err: &mut err,
            stdout_is_terminal: false,
            stderr_is_terminal: false,
            ods_log: None,
            no_color: true,
            dumb_terminal: true,
            cwd: None,
            env: Vec::new(),
            invalid_env: Vec::new(),
        },
    );
    let envelope: Value = serde_json::from_slice(&out).unwrap_or(Value::Null);
    if envelope["result"].is_object() {
        return ToolOutput::Json(envelope["result"].clone());
    }
    let mut messages: Vec<String> = envelope["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| match d["hint"].as_str() {
            Some(hint) => format!(
                "{} ({}); {hint}",
                d["message"].as_str().unwrap_or_default(),
                d["code"].as_str().unwrap_or_default()
            ),
            None => format!(
                "{} ({})",
                d["message"].as_str().unwrap_or_default(),
                d["code"].as_str().unwrap_or_default()
            ),
        })
        .collect();
    if messages.is_empty() {
        // Usage errors from the argument parser are text on stderr.
        messages.push(String::from_utf8_lossy(&err).trim().to_owned());
    }
    ToolOutput::Error(messages.join("\n"))
}

fn text_arg<'a>(arguments: &'a Value, name: &str) -> Option<&'a str> {
    arguments[name]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn strings_arg(arguments: &Value, name: &str) -> Vec<String> {
    match &arguments[name] {
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_owned()],
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::trim).filter(|v| !v.is_empty()))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn usize_arg(arguments: &Value, name: &str) -> Option<usize> {
    arguments[name]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
}

// ---------------------------------------------------------------- tool implementations

/// Versions, counts, lineage coverage and dbt State usage.
pub(super) fn project_summary(project: &Project) -> ToolOutput {
    let artifacts = match Artifacts::load_with(&project.target_dir, project.preference) {
        Ok(artifacts) => artifacts,
        Err(e) => {
            return ToolOutput::Error(format!(
                "{e}; run `dbt compile` (or `dbt parse`) in the project first"
            ));
        }
    };
    let loaded = match project.load() {
        Ok(loaded) => loaded,
        Err(e) => return ToolOutput::Error(error_text(&e)),
    };
    let manifest = &artifacts.manifest;
    let count = |t: ResourceType| {
        manifest
            .nodes
            .iter()
            .filter(|n| n.resource_type == t)
            .count()
    };
    let python = manifest
        .nodes
        .iter()
        .filter(|n| n.language.as_deref() == Some("python"))
        .count();
    ToolOutput::Json(json!({
        "target_dir": project.target_dir,
        "dbt_version": manifest.dbt_version,
        "adapter_type": manifest.adapter_type,
        "artifacts": match manifest.source {
            ods_provider_dbt::ArtifactSource::InfoSchema => "dbt Information Schema (Parquet)",
            _ => "manifest.json",
        },
        "has_catalog": artifacts.catalog.is_some(),
        "counts": {
            "models": count(ResourceType::Model),
            "python_models": python,
            "seeds": count(ResourceType::Seed),
            "snapshots": count(ResourceType::Snapshot),
            "sources": count(ResourceType::Source),
            "data_tests": count(ResourceType::Test),
        },
        "lineage": Summary::of(&loaded),
        "uses_dbt_state": resolve(manifest).uses_dbt_state,
    }))
}

fn search(project: &Project, arguments: &Value) -> ToolOutput {
    let Some(query) = text_arg(arguments, "query") else {
        return ToolOutput::Error("`query` is required".into());
    };
    let loaded = match project.load() {
        Ok(loaded) => loaded,
        Err(e) => return ToolOutput::Error(error_text(&e)),
    };
    let document = loaded
        .graph
        .document(&|id| loaded.node_name(id), &GraphFilter::default());
    let limit = usize_arg(arguments, "limit").unwrap_or(20).min(200);
    ToolOutput::Json(json!({
        "query": query,
        "hits": ods_web::search(&document, query, limit),
    }))
}

/// A node's columns and their inputs (`ods lineage columns --model`).
pub(super) fn get_node(project: &Project, node: &str) -> ToolOutput {
    let mut args = vec!["lineage".to_owned(), "columns".to_owned()];
    args.extend(project.lineage_args());
    args.push(format!("--model={node}"));
    run_cli(&args)
}

/// The whole lineage graph document.
pub(super) fn whole_graph(project: &Project) -> ToolOutput {
    match project.load() {
        Ok(loaded) => ToolOutput::Json(
            serde_json::to_value(
                loaded
                    .graph
                    .document(&|id| loaded.node_name(id), &GraphFilter::default()),
            )
            .unwrap_or(Value::Null),
        ),
        Err(e) => ToolOutput::Error(error_text(&e)),
    }
}

fn lineage(project: &Project, arguments: &Value) -> ToolOutput {
    let focus = strings_arg(arguments, "focus");
    if focus.is_empty() {
        return ToolOutput::Error(
            "`focus` is required: one or more `model` or `model.column` to trace".into(),
        );
    }
    let loaded = match project.load() {
        Ok(loaded) => loaded,
        Err(e) => return ToolOutput::Error(error_text(&e)),
    };
    let mut endpoints = Vec::new();
    for spec in &focus {
        match loaded.endpoint(spec) {
            Ok(endpoint) => endpoints.push(endpoint),
            Err(e) => return ToolOutput::Error(error_text(&e)),
        }
    }
    let filter = GraphFilter::focused(endpoints).with_depth(
        usize_arg(arguments, "upstream"),
        usize_arg(arguments, "downstream"),
    );
    let document = loaded.graph.document(&|id| loaded.node_name(id), &filter);
    let structured = serde_json::to_value(&document).unwrap_or(Value::Null);
    match text_arg(arguments, "format") {
        Some("mermaid") => ToolOutput::Text {
            text: document.to_mermaid(),
            structured: Some(structured),
        },
        _ => ToolOutput::Json(structured),
    }
}

fn impact(project: &Project, arguments: &Value) -> ToolOutput {
    let mut args = vec!["lineage".to_owned(), "impact".to_owned()];
    args.extend(project.lineage_args());
    let columns = strings_arg(arguments, "columns");
    match (text_arg(arguments, "base_dir"), columns.is_empty()) {
        (Some(base), true) => args.push(format!("--base={base}")),
        (None, false) => args.extend(columns.iter().map(|c| format!("--column={c}"))),
        _ => {
            return ToolOutput::Error(
                "pass either `columns` (e.g. [\"orders.amount=removed\"]) or `base_dir`, not both"
                    .into(),
            );
        }
    }
    run_cli(&args)
}

fn list_opaque(project: &Project) -> ToolOutput {
    let loaded = match project.load() {
        Ok(loaded) => loaded,
        Err(e) => return ToolOutput::Error(error_text(&e)),
    };
    let nodes: Vec<Value> = loaded
        .graph
        .nodes()
        .filter(|n| n.is_opaque())
        .map(|n| {
            let (reason, diagnostics) = match &n.lineage {
                Some(l) => (
                    if l.confidence == ods_core::Confidence::Observed {
                        "lineage observed at run time only"
                    } else {
                        "SQL could not be analyzed"
                    },
                    l.diagnostics.clone(),
                ),
                None => (
                    "no SQL to analyze (e.g. a Python model or snapshot)",
                    Vec::new(),
                ),
            };
            json!({
                "id": n.id,
                "name": loaded.node_name(&n.id),
                "kind": n.kind,
                "reason": reason,
                "diagnostics": diagnostics,
                "reads": n.depends_on.iter().map(ToString::to_string).collect::<Vec<_>>(),
            })
        })
        .collect();
    ToolOutput::Json(json!({
        "opaque": nodes,
        "note": "Any change to what an opaque node reads makes it run; its columns can't be traced.",
    }))
}

fn compare_observed(project: &Project, arguments: &Value) -> ToolOutput {
    let file = text_arg(arguments, "observed_file")
        .map(PathBuf::from)
        .or_else(|| project.load.observed.clone());
    let Some(file) = file else {
        return ToolOutput::Error(
            "`observed_file` is required (an export of system.access.column_lineage), or start `ods mcp` with --observed".into(),
        );
    };
    let mut args = vec!["lineage".to_owned(), "compare".to_owned()];
    args.extend(project.artifact_args());
    if let Some(dialect) = &project.load.dialect {
        args.push(format!("--dialect={dialect}"));
    }
    args.push(format!("--observed={}", file.display()));
    run_cli(&args)
}

fn state_policies(project: &Project, arguments: &Value) -> ToolOutput {
    let mut args = vec!["state".to_owned(), "policies".to_owned()];
    args.extend(project.artifact_args());
    if let Some(model) = text_arg(arguments, "model") {
        args.push(format!("--model={model}"));
    }
    run_cli(&args)
}

fn erd_options(arguments: &Value) -> ErdOptions {
    ErdOptions {
        select: strings_arg(arguments, "select"),
        depth: usize_arg(arguments, "depth").unwrap_or(1),
        infer: arguments["infer"].as_bool().unwrap_or(false),
        all: arguments["all"].as_bool().unwrap_or(false),
    }
}

/// The entity-relationship diagram.
pub(super) fn erd(project: &Project, arguments: &Value) -> ToolOutput {
    let format = match text_arg(arguments, "format") {
        Some(f @ ("json" | "dot" | "mermaid")) => f,
        Some(other) => return ToolOutput::Error(format!("unknown format `{other}`")),
        None => "mermaid",
    };
    let erd = match project_erd(
        &project.target_dir,
        project.preference,
        &erd_options(arguments),
    ) {
        Ok(erd) => erd,
        Err(e) => return ToolOutput::Error(error_text(&e)),
    };
    match ErdReport::new(project.target_dir.clone(), format, erd) {
        Ok(report) if format == "json" => {
            ToolOutput::Json(serde_json::to_value(&report).unwrap_or(Value::Null))
        }
        Ok(report) => {
            let mut structured = serde_json::to_value(&report).unwrap_or(Value::Null);
            // The rendered text is the content; don't send it twice.
            if let Some(object) = structured.as_object_mut() {
                object.remove("rendered");
            }
            ToolOutput::Text {
                text: report.rendered,
                structured: Some(structured),
            }
        }
        Err(e) => ToolOutput::Error(error_text(&e)),
    }
}

/// Suggestions for missing tests, from keys (ERD) and join columns (lineage).
fn test_gaps(project: &Project, arguments: &Value) -> ToolOutput {
    let only = text_arg(arguments, "model");
    let limit = usize_arg(arguments, "limit").unwrap_or(50).min(500);
    let options = ErdOptions {
        select: Vec::new(),
        depth: 0,
        infer: true,
        all: true,
    };
    let erd = match project_erd(&project.target_dir, project.preference, &options) {
        Ok(erd) => erd,
        Err(e) => return ToolOutput::Error(error_text(&e)),
    };
    let loaded = match project.load() {
        Ok(loaded) => loaded,
        Err(e) => return ToolOutput::Error(error_text(&e)),
    };
    let mut suggestions = key_gaps(&erd);
    suggestions.extend(join_gaps(&erd, &loaded));
    if let Some(model) = only {
        suggestions.retain(|s| s["model"] == model || s["model_id"] == model);
    }
    let total = suggestions.len();
    suggestions.truncate(limit);
    ToolOutput::Json(json!({
        "suggestions": suggestions,
        "total": total,
        "note": "Suggestions come from naming conventions and join usage: review them before adding tests. \
                 Sources are listed too; their tests go in the source's YAML.",
    }))
}

fn suggestion(
    entity: &ods_erd::Entity,
    column: &str,
    test: &str,
    yaml: &str,
    reason: &str,
) -> Value {
    json!({
        "model": entity.name,
        "model_id": entity.id,
        "column": column,
        "test": test,
        "reason": reason,
        "yaml": yaml,
    })
}

/// Untested keys and relationships the ERD inferred, and keys missing half their tests.
fn key_gaps(erd: &Erd) -> Vec<Value> {
    let mut out = Vec::new();
    for entity in &erd.entities {
        if let Some(key) = &entity.primary_key
            && key.basis == Basis::Inferred
        {
            for column in &key.columns {
                out.push(suggestion(
                    entity,
                    column,
                    "unique + not_null",
                    &format!("- name: {column}\n  data_tests: [unique, not_null]"),
                    &format!(
                        "`{column}` looks like the key of `{}` but nothing tests it",
                        entity.name
                    ),
                ));
            }
        }
        for key in &entity.unique_keys {
            if let [column] = key.columns.as_slice()
                && !entity
                    .columns
                    .iter()
                    .any(|c| &c.name == column && c.not_null)
            {
                out.push(suggestion(
                    entity,
                    column,
                    "not_null",
                    &format!("- name: {column}\n  data_tests: [not_null]"),
                    &format!(
                        "`{column}` is tested unique but may be null, so it isn't a primary key"
                    ),
                ));
            }
        }
    }
    for rel in erd
        .relationships
        .iter()
        .filter(|r| r.basis == Basis::Inferred)
    {
        let (Some(from), Some(to)) = (erd.entity(&rel.from), erd.entity(&rel.to)) else {
            continue;
        };
        let (Some(column), Some(field)) = (rel.from_columns.first(), rel.to_columns.first()) else {
            continue;
        };
        out.push(suggestion(
            from,
            column,
            "relationships",
            &format!(
                "- name: {column}\n  data_tests:\n    - relationships:\n        arguments:\n          to: ref('{}')\n          field: {field}",
                to.name
            ),
            &format!("`{column}` looks like a reference to `{}.{field}`", to.name),
        ));
    }
    out
}

/// Columns used as join keys downstream but not tested at all on their own entity.
fn join_gaps(erd: &Erd, loaded: &Loaded) -> Vec<Value> {
    let mut joins: std::collections::BTreeMap<ColumnRef, BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for node in loaded.graph.nodes() {
        let Some(lineage) = &node.lineage else {
            continue;
        };
        for (column, kind) in &lineage.row_inputs {
            if *kind == IndirectKind::Join {
                joins
                    .entry(column.clone())
                    .or_default()
                    .insert(loaded.node_name(&node.id));
            }
        }
    }
    let mut out = Vec::new();
    for (column, readers) in joins {
        let Some(node) = loaded.graph.node_for(&column.relation) else {
            continue;
        };
        let Some(entity) = erd.entity(&node.id) else {
            continue;
        };
        let tested = entity.columns.iter().any(|c| {
            c.name.eq_ignore_ascii_case(&column.column)
                && (c.not_null || c.primary_key || c.foreign_key)
        }) || entity.unique_keys.iter().any(|k| {
            k.columns
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&column.column))
        });
        if tested {
            continue;
        }
        out.push(suggestion(
            entity,
            &column.column,
            "not_null",
            &format!("- name: {}\n  data_tests: [not_null]", column.column),
            &format!(
                "joined on in {}; a null key silently drops or duplicates rows",
                readers.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    out
}

// ---------------------------------------------------------------- registration

type Handler = fn(&Project, &Value) -> ToolOutput;

struct ProjectTool {
    definition: ToolDefinition,
    project: Arc<Project>,
    handler: Handler,
}

impl Tool for ProjectTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn call(&self, arguments: &Value) -> ToolOutput {
        (self.handler)(&self.project, arguments)
    }
}

fn schema(properties: &Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Every tool, in the order clients list them.
#[allow(clippy::too_many_lines, reason = "one table of tool definitions")]
pub(super) fn all(project: &Arc<Project>) -> Vec<Box<dyn Tool>> {
    let tools: Vec<(ToolDefinition, Handler)> = vec![
        (
            ToolDefinition::read_only(
                "ods_project_summary",
                "Project summary",
                "Start here. The dbt project's version and adapter, how many models, seeds, sources and tests it has, how much of it column lineage covers (and how many models are opaque), and whether it uses dbt State.",
                schema(&json!({}), &[]),
            ),
            |p, _| project_summary(p),
        ),
        (
            ToolDefinition::read_only(
                "ods_search",
                "Search models and columns",
                "Find models, seeds, sources and columns by name. Every whitespace-separated term must match; prefix matches rank first.",
                schema(
                    &json!({
                        "query": {"type": "string", "description": "e.g. `orders amount`"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 20}
                    }),
                    &["query"],
                ),
            ),
            search,
        ),
        (
            ToolDefinition::read_only(
                "ods_get_node",
                "Model columns and their inputs",
                "Every output column of a model and the upstream columns it comes from (identity, transformation, aggregation), plus the columns that shape its rows (joins, filters, grouping), with confidence and diagnostics.",
                schema(
                    &json!({"node": {"type": "string", "description": "Model name or unique_id"}}),
                    &["node"],
                ),
            ),
            |p, a| match text_arg(a, "node") {
                Some(node) => get_node(p, node),
                None => ToolOutput::Error("`node` is required".into()),
            },
        ),
        (
            ToolDefinition::read_only(
                "ods_lineage",
                "Trace column lineage",
                "The part of the column-level lineage graph connected to the given models or columns: upstream sources and downstream uses. JSON (nodes, node edges, column edges with direct/indirect kinds) or a Mermaid flowchart of models.",
                schema(
                    &json!({
                        "focus": {"type": "array", "items": {"type": "string"}, "minItems": 1,
                                  "description": "`model` or `model.column`, e.g. [\"customers.lifetime_value\"]"},
                        "upstream": {"type": "integer", "minimum": 0, "description": "Hops upstream (default: all)"},
                        "downstream": {"type": "integer", "minimum": 0, "description": "Hops downstream (default: all)"},
                        "format": {"type": "string", "enum": ["json", "mermaid"], "default": "json"}
                    }),
                    &["focus"],
                ),
            ),
            lineage,
        ),
        (
            ToolDefinition::read_only(
                "ods_impact",
                "What must run for a change",
                "Given changed columns (or another build to compare with), which downstream models must run and why (the chain of column evidence), and which readers can be skipped because they don't use the changed columns. Opaque models are always included.",
                schema(
                    &json!({
                        "columns": {"type": "array", "items": {"type": "string"},
                                    "description": "`MODEL.COLUMN[=modified|removed|added]`, e.g. [\"stg_orders.status=removed\"]"},
                        "base_dir": {"type": "string",
                                     "description": "Target directory of another build (e.g. production's); every difference is analysed"}
                    }),
                    &[],
                ),
            ),
            impact,
        ),
        (
            ToolDefinition::read_only(
                "ods_erd",
                "Entity-relationship diagram",
                "Keys and relationships between models, seeds, snapshots and sources, from dbt tests (unique, not_null, relationships) and contract constraints. Each carries its basis: declared, tested, or inferred (only with `infer`). Mermaid erDiagram by default; JSON or Graphviz DOT on request.",
                schema(
                    &json!({
                        "select": {"type": "array", "items": {"type": "string"},
                                   "description": "Only these models (names or unique_ids) and their related entities"},
                        "depth": {"type": "integer", "minimum": 0, "default": 1,
                                  "description": "With `select`, how many relationships away to include"},
                        "infer": {"type": "boolean", "default": false,
                                  "description": "Also propose keys and relationships from naming conventions (labelled inferred)"},
                        "all": {"type": "boolean", "default": false,
                                "description": "Include entities without relationships"},
                        "format": {"type": "string", "enum": ["mermaid", "json", "dot"], "default": "mermaid"}
                    }),
                    &[],
                ),
            ),
            erd,
        ),
        (
            ToolDefinition::read_only(
                "ods_test_gaps",
                "Missing tests",
                "Tests worth adding, with evidence and ready-to-paste YAML: keys that look like primary keys but are untested, unique columns missing not_null, references that look like relationships, and columns used as join keys downstream that nothing tests.",
                schema(
                    &json!({
                        "model": {"type": "string", "description": "Only this model"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50}
                    }),
                    &[],
                ),
            ),
            test_gaps,
        ),
        (
            ToolDefinition::read_only(
                "ods_list_opaque",
                "Where lineage is unknown",
                "Models whose column lineage ODS can't determine (Python models, SQL it can't analyze, snapshots), with the reason. Changes to anything they read always make them run.",
                schema(&json!({}), &[]),
            ),
            |p, _| list_opaque(p),
        ),
        (
            ToolDefinition::read_only(
                "ods_state_policies",
                "Freshness policies",
                "How stale each model may get before new upstream data makes it rebuild, read from the project's dbt State configs (lag_tolerance, require_fresh_data_from, build_after), and how each source reports new data.",
                schema(
                    &json!({"model": {"type": "string", "description": "Only this model"}}),
                    &[],
                ),
            ),
            state_policies,
        ),
        (
            ToolDefinition::read_only(
                "ods_compare_observed",
                "Check lineage against the warehouse",
                "Compare static column lineage with what the warehouse recorded (an export of Unity Catalog's system.access.column_lineage): per model agreement, missed edges, precision and recall.",
                schema(
                    &json!({"observed_file": {"type": "string",
                                             "description": "CSV or JSON export; defaults to the server's --observed"}}),
                    &[],
                ),
            ),
            compare_observed,
        ),
    ];
    tools
        .into_iter()
        .map(|(definition, handler)| {
            Box::new(ProjectTool {
                definition,
                project: project.clone(),
                handler,
            }) as Box<dyn Tool>
        })
        .collect()
}
