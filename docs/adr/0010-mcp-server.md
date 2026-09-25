# ADR-0010: `ods mcp`, a read-only, local MCP server

- **Status:** Proposed
- **Date:** 2026-09-25
- **Issues:** #169; related #173 (skills pack), #32 (agent)
- **Deciders:** @n1ckyb

## Context
AI coding agents (Claude Code, Cursor, VS Code, Codex) reach tools through the Model
Context Protocol (MCP). dbt Labs' `dbt-mcp` has 62 tools
([`docs/research/mcp-strategy.md`](../research/mcp-strategy.md)). Most of them need a dbt
Platform account. Its column-lineage tools need the proprietary LSP or the Platform. It
mixes read tools with commands that build models or run SQL.

ODS already has the engines an agent needs to be *right*: column lineage and impact,
observed lineage, dbt State policies, and now an ERD (ADR-0012). The agent strategy
([`docs/research/agent-strategy.md`](../research/agent-strategy.md)) puts them in the
agents teams already use, before ODS builds its own agent.

## Options considered
- **`rmcp` (the official Rust SDK, Apache-2.0).**
  - Pros: complete spec coverage, and HTTP transports.
  - Cons: async (Tokio) and macro-heavy, and its 3.x API still moves. We only need
    stdio, tools, resources and prompts.
- **A small synchronous implementation in its own crate (chosen).**
  - JSON-RPC 2.0 over newline-delimited stdio, about 350 lines.
  - No new dependencies (`serde_json` only).
  - Easy to test without a runtime.
  - We can switch to `rmcp` behind the same `Tool`/`Resources`/`Prompts` traits when we
    need streamable HTTP or auth (#97).
- **Tools inside `ods-web`'s HTTP server.** Wrong transport for local agents (stdio is
  the norm), and it couples two surfaces.

## Decision
- **`ods-mcp`**, an EDGE crate (ADR-0009): the protocol only. It covers:
  - `initialize`, with version negotiation: 2025-11-25, 2025-06-18, 2025-03-26 and
    2024-11-05;
  - `ping`;
  - `tools/list`, `tools/call` (structured content plus text);
  - `resources/list`, `resources/templates/list`, `resources/read`;
  - `prompts/list`, `prompts/get`;
  - notifications, batches, and JSON-RPC errors.

  It knows nothing about dbt, and depends on nothing but `serde`.
- **`ods mcp`** (the CLI, the composition root) registers the tools, resources and
  prompts. It takes `ods lineage`'s options: `--target-dir`, `--artifacts`, `--dialect`,
  `--observed`, `--trust-observed`.
- **One contract.** Tools that mirror a CLI command run it in-process with `--json` and
  return its `result`. MCP output is the CLI's JSON output, covered by the same tests.
- **Read-only, local, no telemetry.**
  - Every tool is annotated `readOnlyHint: true`, `destructiveHint: false` and
    `openWorldHint: false`.
  - No tool writes files, runs dbt, or queries a warehouse.
  - Paths given as arguments (`base_dir`, `observed_file`) are only read.
  - Agent-supplied values are passed as single `--flag=value` arguments, so they can
    never become flags. A test checks this.
- **Fresh answers.** Artifacts are re-read on every call, and a process-wide analysis
  cache means only changed models are re-analyzed. A missing target directory is not
  fatal: tools report it with a hint, and the server keeps running.
- **Tools (v1):**

  | Tool | Answers |
  |---|---|
  | `ods_project_summary` | versions, counts, lineage coverage, whether the project uses dbt State |
  | `ods_search` | models and columns by name |
  | `ods_get_node` | each output column's inputs, row-shaping inputs, confidence |
  | `ods_lineage` | the connected part of the column graph (JSON or Mermaid) |
  | `ods_impact` | what must run for column changes or another build, and what can be skipped |
  | `ods_erd` | keys and relationships, declared, tested or inferred (Mermaid, JSON or DOT) |
  | `ods_test_gaps` | tests worth adding, with evidence and YAML |
  | `ods_list_opaque` | where lineage is unknown, and why |
  | `ods_state_policies` | freshness policies from dbt State configs |
  | `ods_compare_observed` | static lineage against Unity Catalog's recorded lineage |
  | `ods_find_data` | for data users: tables and columns by meaning, with grain |
  | `ods_describe_entity` | a table explained: grain, columns, joins with cardinality and evidence |
  | `ods_plan_query` | the most trustworthy join path and starting SQL, with fan-out warnings |

- **Resources:**
  - `ods://project/summary`;
  - `ods://erd`;
  - `ods://lineage/graph`;
  - the template `ods://node/{id}`.
- **Prompts:**
  - `assess_change_impact`;
  - `review_breaking_changes`;
  - `add_missing_tests`;
  - `answer_data_question`: find data → describe → plan → SQL, stating grain and
    assumptions, for someone who doesn't know the project.
- **Robustness.** A panicking tool returns a JSON-RPC internal error and the server
  keeps serving; a line that isn't UTF-8 is one parse error; responses sent to the
  server are ignored; ids must be strings or numbers.
- **Versioning.**
  - Tool names are stable.
  - Result shapes follow the CLI's JSON (`schema_version` where persisted).
  - Removing or renaming a tool, or making a breaking result change, needs a CHANGELOG
    entry and a deprecation period.

## Consequences
- Positive:
  - any MCP client gets column lineage, impact, ERD and State answers with no account;
  - the tools can be auto-approved, since none of them writes;
  - one JSON contract across the CLI, HTTP API and MCP.
- Negative / trade-offs:
  - We maintain the protocol layer ourselves (small, and covered by tests).
  - It is stdio only until we add HTTP (behind #97).
  - Each call re-reads artifacts: a few ms for small projects, around 200 ms at 2,000
    models, mostly cached.
- Follow-ups:
  - #173, a skills pack that calls these tools;
  - an MCP App resource for the explorer page (ADR-0009);
  - `/mcp` on `ods serve`;
  - an eval set comparing answers with `dbt-mcp`.
