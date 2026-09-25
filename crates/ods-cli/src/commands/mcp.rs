//! `ods mcp`: the ODS engines as a Model Context Protocol server over stdio (ADR-0010,
//! #169).
//!
//! This is the composition root for `ods-mcp`: it builds the tools, resources and
//! prompts from the same providers and reports as the CLI. Every tool is read-only and
//! local; tools that mirror a CLI command run that command in-process with `--json`, so
//! an MCP result is exactly what `ods … --json` prints.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{ArgMatches, Command};
use ods_mcp::{
    PromptArgument, PromptDefinition, Prompts, ReadError, ResourceContent, ResourceDefinition,
    ResourceTemplate, Resources, Server, ServerInfo, ToolOutput,
};
use ods_provider_dbt::ArtifactPreference;
use serde_json::{Map, Value};

use super::lineage::{LoadOptions, common_args};
use super::mcp_tools::{self, Project};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};

/// `ods mcp`.
pub struct Mcp;

const INSTRUCTIONS: &str = "\
OpenDataSuite (ODS) answers questions about a dbt project from its compiled artifacts, \
locally and read-only. Answers come from deterministic engines, not guesses: use them \
instead of reading SQL by hand.

Start with `ods_project_summary`, then `ods_search` to find models and columns.
- Where does a column come from, and what uses it? `ods_get_node`, `ods_lineage`.
- What must run if I change something, and what can be skipped? `ods_impact`, with \
columns (`orders.amount=removed`) or another build's target directory.
- Keys and relationships: `ods_erd` (Mermaid diagram, or JSON).
- Which tests are missing? `ods_test_gaps`.
- Freshness policies (dbt State configs): `ods_state_policies`.
- Where lineage is unknown (Python models, unparseable SQL): `ods_list_opaque`.

For someone who uses the data but doesn't know the project, answering a question with SQL: `ods_find_data` (tables and columns by meaning), `ods_describe_entity` (what one row is, the columns, what it joins to), then `ods_plan_query` (join path and SQL skeleton). Write SQL only with the columns and join conditions these tools return; a key may span several columns, so join on all of them. State the grain of the answer and every assumption, and repeat any warning about joins that repeat rows.

Be conservative: an opaque model may use any column it reads, so treat it as impacted. \
Inferred keys and relationships are suggestions, not facts. Artifacts are re-read on \
every call, so run `dbt compile` (and `dbt docs generate` for column types) after \
editing the project.";

impl Module for Mcp {
    fn command(&self) -> Command {
        common_args(
            Command::new("mcp")
                .about("Serve the ODS tools to AI agents over the Model Context Protocol (stdio)"),
        )
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let project = Arc::new(Project::new(
            PathBuf::from(
                matches
                    .get_one::<String>("target-dir")
                    .map_or("target", String::as_str),
            ),
            LoadOptions::from_args(matches),
            match matches.get_one::<String>("artifacts").map(String::as_str) {
                Some("json") => ArtifactPreference::Json,
                Some("info-schema") => ArtifactPreference::InfoSchema,
                _ => ArtifactPreference::Auto,
            },
            matches
                .get_one::<String>("artifacts")
                .cloned()
                .unwrap_or_else(|| "auto".into()),
        ));
        // A missing target directory is not fatal: the agent may run `dbt compile`
        // after starting the server. Say so on stderr, which MCP clients log.
        if let Err(e) = project.load() {
            tracing::warn!(
                "{}; tools will report it until the artifacts exist",
                e.message
            );
        }
        let server = server(&project);
        let stdin = std::io::stdin();
        server
            .serve(stdin.lock(), ctx.raw_out())
            .map_err(|e| CliError::new(ExitStatus::Failure, codes::OUTPUT_WRITE, e.to_string()))
    }
}

/// The server with every tool, resource and prompt.
pub(super) fn server(project: &Arc<Project>) -> Server {
    let mut server = Server::new(ServerInfo::new(
        "ods",
        env!("CARGO_PKG_VERSION"),
        INSTRUCTIONS,
    ))
    .with_resources(Box::new(ProjectResources(project.clone())))
    .with_prompts(Box::new(ProjectPrompts));
    for tool in mcp_tools::all(project) {
        server = server.with_tool(tool);
    }
    server
}

struct ProjectResources(Arc<Project>);

impl ProjectResources {
    fn from_tool(output: ToolOutput, mime_type: &str) -> Result<ResourceContent, ReadError> {
        match output {
            ToolOutput::Json(value) => Ok(ResourceContent::new(
                mime_type,
                serde_json::to_string_pretty(&value).unwrap_or_default(),
            )),
            ToolOutput::Text { text, .. } => Ok(ResourceContent::new(mime_type, text)),
            ToolOutput::Error(message) => Err(ReadError::Failed(message)),
            _ => Err(ReadError::NotFound),
        }
    }
}

impl Resources for ProjectResources {
    fn list(&self) -> Vec<ResourceDefinition> {
        vec![
            ResourceDefinition::new(
                "ods://project/summary",
                "project-summary",
                "The dbt project: versions, node counts, lineage coverage, dbt State usage",
                "application/json",
            ),
            ResourceDefinition::new(
                "ods://erd",
                "erd",
                "Entity-relationship diagram (Mermaid) of tested and declared keys",
                "text/vnd.mermaid",
            ),
            ResourceDefinition::new(
                "ods://lineage/graph",
                "lineage-graph",
                "The whole column-level lineage graph (ODS graph JSON, schema_version 1)",
                "application/json",
            ),
        ]
    }

    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![ResourceTemplate::new(
            "ods://node/{id}",
            "node",
            "A model, seed, snapshot or source: its columns and where each comes from",
            "application/json",
        )]
    }

    fn read(&self, uri: &str) -> Result<ResourceContent, ReadError> {
        let project = &self.0;
        match uri {
            "ods://project/summary" => {
                Self::from_tool(mcp_tools::project_summary(project), "application/json")
            }
            "ods://erd" => Self::from_tool(
                mcp_tools::erd(project, &Value::Object(Map::new())),
                "text/vnd.mermaid",
            ),
            "ods://lineage/graph" => {
                Self::from_tool(mcp_tools::whole_graph(project), "application/json")
            }
            _ => match uri.strip_prefix("ods://node/") {
                Some(id) if !id.is_empty() => {
                    Self::from_tool(mcp_tools::get_node(project, id), "application/json")
                }
                _ => Err(ReadError::NotFound),
            },
        }
    }
}

struct ProjectPrompts;

fn argument<'a>(arguments: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
}

impl Prompts for ProjectPrompts {
    fn list(&self) -> Vec<PromptDefinition> {
        vec![
            PromptDefinition::new(
                "assess_change_impact",
                "Assess the impact of a change",
                "Which models must run, which can be skipped, and what could break, for a change you describe",
                vec![PromptArgument::new(
                    "change",
                    "The change, e.g. `drop orders.status` or `rename stg_customers.email`",
                    true,
                )],
            ),
            PromptDefinition::new(
                "review_breaking_changes",
                "Review a build for breaking changes",
                "Compare this build with another (e.g. production's artifacts) and review every column change downstream",
                vec![PromptArgument::new(
                    "base_dir",
                    "Target directory of the build to compare with",
                    true,
                )],
            ),
            PromptDefinition::new(
                "answer_data_question",
                "Answer a question about the data",
                "Find the right tables, explain them, and write SQL for a business question, for someone who doesn't know the dbt project",
                vec![PromptArgument::new(
                    "question",
                    "The question, e.g. `which customers spent the most last month?`",
                    true,
                )],
            ),
            PromptDefinition::new(
                "add_missing_tests",
                "Add missing tests",
                "Find untested keys, relationships and join columns, and propose dbt tests",
                vec![PromptArgument::new(
                    "model",
                    "Only this model (optional)",
                    false,
                )],
            ),
        ]
    }

    fn get(&self, name: &str, arguments: &Map<String, Value>) -> Option<Result<String, String>> {
        let required = |key: &str| {
            argument(arguments, key)
                .map(str::to_owned)
                .ok_or_else(|| format!("`{key}` is required"))
        };
        Some(match name {
            "assess_change_impact" => required("change").map(|change| {
                format!(
                    "I plan this change to the dbt project: {change}\n\n\
                     Using the ODS tools:\n\
                     1. Find the exact models and columns involved with `ods_search`.\n\
                     2. Call `ods_impact` with them (`MODEL.COLUMN=modified|removed|added`).\n\
                     3. List the models that must run and why, and the readers that can be skipped.\n\
                     4. Call out opaque models (lineage unknown): they must be assumed affected.\n\
                     5. Say which tests and downstream consumers to check before merging."
                )
            }),
            "review_breaking_changes" => required("base_dir").map(|base| {
                format!(
                    "Compare the current build with the one in `{base}` using `ods_impact` \
                     with `base_dir`. For every changed column, say whether it is removed, \
                     modified or added, which downstream models it reaches and through \
                     which columns, and whether that is likely to break consumers. Group \
                     the answer by severity and cite the evidence ODS gives."
                )
            }),
            "answer_data_question" => required("question").map(|question| {
                format!(
                    "Question about our data: {question}\n\n\
                     I don't know the dbt project, so use the ODS tools:\n\
                     1. `ods_find_data` with the question's key words to find candidate tables.\n\
                     2. `ods_describe_entity` on the best candidates: what one row is (the \
                     key may be several columns), the columns and their meaning, and what \
                     each table joins to.\n\
                     3. `ods_plan_query` with the tables, the one whose rows the answer is \
                     about first, and the columns you need.\n\
                     4. Write the final SQL from that plan: keep its joins (every column of \
                     a composite key), add filters, grouping and measures, and use only \
                     columns the tools listed. Aggregate before joining where the plan warns \
                     that a join repeats rows.\n\
                     5. Explain the answer's grain, the tables used and why, and any \
                     assumption (e.g. how \"last month\" or \"active\" is defined). If the data \
                     can't answer the question, say so rather than guessing."
                )
            }),
            "add_missing_tests" => Ok(format!(
                "Call `ods_test_gaps`{scope} and review each suggestion. For the ones that \
                 hold, add the dbt tests to the model's properties YAML (the snippets are \
                 ready to paste), then run `dbt parse` and check `ods_erd` shows the keys \
                 and relationships as tested. Explain any suggestion you reject.",
                scope = argument(arguments, "model")
                    .map(|m| format!(" with `model: {m}`"))
                    .unwrap_or_default()
            )),
            _ => return None,
        })
    }
}
