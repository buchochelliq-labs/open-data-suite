# MCP: what dbt's server does, and what ODS should expose

Date: 2026-09-25. Source: `dbt-labs/dbt-mcp` at `8cb2bca` (Apache-2.0), read in full;
`src/dbt_mcp/tools/tool_names.py` lists 62 tools.

## What dbt-mcp is

dbt-mcp is a Python MCP server. Its tools are grouped in toolsets, and each toolset can be
switched off with a `DISABLE_*` variable. **Most of them are thin clients of dbt Platform
APIs and need a Platform account and token.** Only the `dbt_cli` toolset works on a
local project without an account. It sends usage tracking unless you turn it off.

| Toolset | Tools | Needs |
|---|---|---|
| SQL | `text_to_sql`, `execute_sql` | Platform (Copilot, SQL API) |
| Semantic Layer | `list_metrics`, `list_saved_queries`, `get_dimensions`, `get_entities`, `get_dimension_values`, `query_metrics`, `get_metrics_compiled_sql` | Platform (Semantic Layer, paid tiers) |
| Discovery | `get_mart_models`, `get_all_models`, `get_node_details`, `get_model_details`, `get_model_parents`, `get_model_children`, `get_model_health`, `get_model_performance`, `get_lineage`, `get_all_sources`, `get_source_details`, `get_exposures`, `get_exposure_details`, `get_related_models`, `get_all_macros`, `get_macro_details`, `get_seed_details`, `get_semantic_model_details`, `get_snapshot_details`, `get_test_details`, `search` | Platform (Discovery API) |
| dbt CLI | `build`, `compile`, `docs`, `list`, `parse`, `run`, `test`, `show`, `clone`, `get_lineage_dev`, `get_node_details_dev` | local dbt; **runs commands against your warehouse** |
| Admin API | `list_projects`, `list_jobs`, `get_job_details`, `trigger_job_run`, `list_jobs_runs`, `get_job_run_details`, `cancel_job_run`, `retry_job_run`, `list_job_run_artifacts`, `get_job_run_artifacts`, `get_job_run_error` | Platform |
| Codegen | `generate_source`, `generate_model_yaml`, `generate_staging_model` | dbt-codegen package; off by default |
| LSP | `get_column_lineage` | the **proprietary** `dbt lsp` (Fusion) or the `dbt-lsp` binary installed by the dbt Labs VS Code extension |
| Fusion (remote) | `fusion.compile_sql`, `fusion.get_column_lineage` | Platform |
| Product docs | `search_product_docs`, `get_product_doc_pages` | network |
| Server metadata | `get_mcp_server_version`, `get_mcp_server_branch` | — |

It also ships MCP prompts and "MCP Apps" (UI resources rendered by the client).

### Where it falls short (and where ODS is different)

1. **Column lineage is gated.** Both CLL tools need either the proprietary LSP binary or
   a Platform account. `get_lineage_dev` is model-level only. ODS computes CLL locally,
   from open artifacts, with no login (ADR-0008).
2. **No "what must run?" tool.** An agent can walk parents and children, but nothing
   answers *"I changed `stg_orders.status`; which models must rebuild, which can be
   skipped, and why?"* That is `ColumnGraph::impact`, with reasons and a pruned list.
3. **No explanations or confidence.** Discovery returns facts. It never says *how sure*
   it is or *why*. ODS answers carry reason chains and `exact / inferred / observed /
   unknown` confidence (rules 3 and 4). An agent can then say "this model is opaque
   (Python), so I can't rule it out" instead of guessing.
4. **No check against reality.** ODS can compare its lineage with what the warehouse
   recorded (`lineage compare`, Unity Catalog), and fill Python models in from it.
5. **Write access sits next to read access.** `run`, `build`, `trigger_job_run` and
   `execute_sql` live in the same server as the read tools, so a prompt injection in a
   model description can reach them. ODS's server should be read-only, with any action
   behind the policy engine (#9).
6. **Vendor-bound.** Discovery, Admin, Fusion and the Semantic Layer are one vendor's
   APIs. ODS tools are provider-neutral: the same tool works over dbt 1.x JSON, dbt v2
   Parquet and, later, other formats.

What dbt-mcp does well, and we should match:
- tools you can switch off one by one;
- search as the entry point;
- node details that include SQL and columns;
- prompts for common workflows;
- MCP Apps for visual results.

## Proposed ODS MCP server

**Shape:**
- `ods mcp`: a stdio server for local agents (Claude Code, Cursor, VS Code).
- Later, the same tools over streamable HTTP at `ods serve`'s `/mcp`, behind the same
  Host checks and, later, authentication (#97).
- A new EDGE crate, `ods-mcp`, that reuses `ods-web`'s `Snapshot`/`Loader` (ADR-0009).
- Tool results are the same view models the CLI emits with `--output json`, so there is
  one contract, one set of snapshot tests, and no drift.
- Read-only. No data values, only metadata. No network. No telemetry.

**v1 tools** (everything here exists today on this branch):

| Tool | Answers | Backed by |
|---|---|---|
| `ods_search` | "find the model or column called …" | `ods_web::search` |
| `ods_get_node` | columns, lineage with confidence, diagnostics, SQL path | `/api/node` |
| `ods_column_lineage` | "where does `customers.lifetime_value` come from / go to?" (up/down, depth) | `GraphFilter` focus |
| `ods_impact` | "I change these columns (or rows): what must run, what is skipped, why?" | `ColumnGraph::impact` |
| `ods_diff_builds` | "what changed between this build and prod's artifacts?" (column-level) | `lineage impact --base` |
| `ods_list_opaque` | "where is lineage unknown, and why?" (Python, parse failures, `select *` on unknown schemas) | `NodeLineage::is_opaque` + diagnostics |
| `ods_compare_observed` | "does static lineage match what the warehouse recorded?" | `compare_observed` |
| `ods_graph` | a subgraph as Mermaid (for chat) or JSON | `GraphDocument` exports |

**Resources:**
- `ods://graph`, the `GraphDocument`;
- `ods://node/{id}`;
- the explorer page as an MCP App UI resource, so clients that render apps show the
  interactive graph inline.

**Prompts:**
- "assess the impact of my working-tree changes";
- "review this PR for breaking column changes";
- "why can't you tell whether X is affected?" (walks opaque nodes).

**Later, as the modules land:**
- `ods_state_plan` / `ods_explain_decision`: why a model is reused or rebuilt (M1 State,
  #19/#17);
- `ods_usage`: which dashboards and queries read this column (M3, observed from query
  history);
- `ods_erd`: keys and relationships, kept separate from lineage (rule 6);
- `ods_ci_check`: the M4 selective-CI verdict;
- `ods_query_index`: read-only SQL over the ODS metadata index (needs its own ADR).

**Deliberately not copied:**
- `execute_sql` / `text_to_sql`: warehouse MCP servers already do this, and ODS never
  touches data;
- job triggers and admin tools (platform-specific);
- product docs.

## Next steps

1. **ADR-0010 (MCP server):** transport, crate, and SDK choice (the official Rust SDK
   `rmcp`, with its licence checked by `cargo deny`), read-only guarantee, and tool
   versioning.
2. **Issues:** `ods mcp` with the v1 tools; MCP App for the explorer; `/mcp` on
   `ods serve`; add the prompts.
3. **Eval set:** questions answered with dbt-mcp (Discovery) vs ODS on the fixture,
   scored on correctness and on "admits uncertainty".
