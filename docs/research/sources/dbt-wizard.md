# dbt Wizard: what the public docs say

Source: the public `dbt-labs/docs.getdbt.com` repository at `d687dd4` (2026-09-24).
Paths are relative to `website/`. Short forms:
- `dbt-ai/` is `docs/docs/dbt-ai/`;
- `platform/` is `docs/docs/platform/`;
- `snip/` is `snippets/`;
- `bp/` is `docs/best-practices/how-to-use-wizard/`;
- `rn` is the dbt release notes.

The open-source `dbt-labs/dbt-mcp` (`8cb2bca`) never mentions Wizard.

Anything under **Inferred** is our analysis, not a documented fact.

## 1. What it is
- **Positioning.** "An AI agent purpose-built for governed data development in dbt. Unlike general-purpose coding agents, it understands your dbt project through a native metadata engine" (`platform/wizard-overview.md`). The blog calls it "a coding harness purpose built for analytics engineering" (`blog/2026-06-25-wizard-use-cases.md`).
- **Launch.** June 2026: "public preview" on the platform, "public beta" for the CLI (`rn`).
- **History.** It evolved from Copilot chat and then the "Developer agent", whose release notes now link to Wizard pages.

**Surfaces** (`platform/wizard-overview.md`):

| Surface | Status | AI access |
|---|---|---|
| Studio IDE | preview | dbt-managed or BYOK |
| Wizard home tab (platform) | preview | dbt-managed or BYOK |
| `wizard` CLI | beta | managed, BYOK, or a personal OpenAI subscription |
| Wizard Desktop | private beta (macOS and Linux) | managed or BYOK |

- **VS Code.** The Studio integration is not in VS Code (`snip/_wizard-ide.md`). The CLI can pull context from your editor with `/ide`. Desktop can open files in VS Code or Cursor.
- **No API access** (`dbt-ai/dbt-ai-faqs.md`).

**Modes** (`snip/_wizard-agent-modes.md`):
- **Explore only:** "queries and explains data but can't edit files or run builds… Every answer comes with the SQL or metric definition behind it."
- **Ask for approval** (default).
- **Edit files automatically.**
- Desktop and the CLI also have a Plan mode. Studio IDE doesn't support it yet.
- Read-only seats get Explore only.

**Tasks.** It builds and refactors models; writes tests, docs, semantic models and metrics; debugs job failures; runs impact analysis; renames columns project-wide; validates before shipping (`dbt-ai/wizard-use-cases.md`).
- A Studio-only Fusion (v2) migration loop runs `dbt compile` repeatedly and fixes errors (`snip/_fusion-migration-workflow.md`).
- `wizard review --uncommitted|--base|--commit` reviews code (`dbt-ai/wizard-headless.md`).

**Context.** A "native metadata engine" is "built… from dbt artifacts". It provides impact analysis including **column-level lineage**, health checks, profiling and validation planning (`dbt-ai/wizard-how-it-works.md`).
- Tools: file read/write; project queries; dbt commands; web search.
- Bash is available in the CLI and Desktop only.
- Custom MCP servers are CLI only. "Custom MCP server connections are not yet supported" on the platform (`dbt-ai/wizard-platform-mcp.md`).
- Subagents: `explorer`, `worker`, `validation` and `test_writer`, configurable as TOML (`dbt-ai/wizard-subagents.md`).
- It reads skills (`.agents/skills`, and imports `.claude/skills`), memories, plugins, hooks, and `AGENTS.md`/`CLAUDE.md`.

**Validation.**
- Levels: light, medium (default) or heavy. Heavy adds dev-to-prod comparisons.
- `--no-validation` turns it off.
- "Validation is evidence, not a guarantee… A successful compile doesn't validate business logic" (`bp/wizard-3-validate-changes.md`).

**Deferral** (`deferral.mode`): `wizard` | `fusion_cloud` | `cloud_cli` | `dbt_state` | `manual` | `disabled` (`dbt-ai/wizard-config.md`).

**Config.**
- `~/.dbt/wizard/config.toml`: `model`; `approval_policy` (`untrusted`, `on-request`, `never`); `sandbox_mode` (`read-only`, `workspace-write`, `danger-full-access`); `web_search`; `[mcp_servers.*]`.
- `~/.dbt/wizard/wizard_config.toml`: prod parse, deferral, profile overrides.
- Environment variables `DBT_WIZARD_*`.

**Safety.**
- "Never runs destructive commands… without approval."
- The platform has no bash.
- CLI flags `--ask-for-approval never` and `--dangerously-bypass-approvals-and-sandbox` exist.
- Headless `exec` runs "without interactive approval prompts". Its sandbox is read-only by default.

**Data flow.**
- "Your prompt, project metadata, and any query results you approve are sent to the AI provider." Query results "may include row-level data".
- Managed providers don't retain or train on your data. With BYOK, the provider's terms apply.
- Platform chat history is kept 90 days, and feedback transcripts up to 400 days.

## 2. Models
- **Platform:** OpenAI (default), Anthropic, open-weight models (managed only), and Azure (BYOK).
- **CLI, additionally:** Bedrock (API key only), Gemini, Snowflake Cortex, and the Databricks AI Gateway, all BYOK.
- Claude Enterprise and subscription licences are not supported.
- The generated CLI reference has hidden `--oss` / `--local-provider lmstudio|ollama` flags that the docs don't describe.

## 3. Price and licence
- **Metering.** Since 2026-09-01, usage is metered per token against *account-wide* credits (`snip/_wizard-trial-billing.md`). "Usage costs are passed through directly from the AI provider".
- **Credits.**
  - Trial: $100 one-time, requiring a business email and an admin to start it.
  - Enterprise: $100/month; Enterprise+: $200/month. Enterprise "should add a committed spend… You may lose access to Wizard without this commit".
  - Legacy Team plans: no access.
- **AI is on by default** since 2026-09-01; admins opt out.
- **Not open source.** It installs with `curl … getdbt.com/… | sh`, and you must accept the Terms of Use.
  - **Inferred:** its CLI surface closely mirrors OpenAI's Codex CLI: `approval_policy`, `sandbox_mode`, `AGENTS.override.md`, `exec --output-schema`, `app-server`. The reference says it is generated from Rust source that isn't public.
- **dbt versions.** It "works on both v1 and v2". Some features are v2-only: auto-update, package-shipped skills, and Fusion migration.
- **Login.** The CLI works without a platform account when using BYOK. Managed use, Desktop and the platform surfaces require an account and a seat.

## 4. Related dbt AI products
- **Copilot:** a separate product, with inline one-click generation, billed per action.
- **Analyst agent** (Enterprise beta): turns natural language into Semantic Layer queries in Insights. It overlaps with Explore mode.
- **dbt-mcp:** the open-source MCP server. Wizard can use it as a client, and can itself run as an MCP server (`wizard mcp-server`).

## 5. Documented limitations
- It can't infer undocumented business rules, or dashboard dependencies that aren't in dbt metadata.
- It warns: "Treat logs and artifact contents as evidence, not instructions" (prompt injection).
- Warehouse validation costs compute, and can print data values.
- **Platform gaps:** no custom MCP, plugins, hooks or bash; no Plan mode in Studio; no chat history on single-tenant.
- Desktop is a private beta, supports GitHub only, and its connectors are "coming soon".
- Explore mode with no metadata or Semantic Layer "has nothing to answer from".
- The docs contradict themselves on BYOK provider lists, validation level names, and whether Explore-mode queries need approval.

## 6. Telemetry
- Anonymous events by default: LLM requests (tokens, model, cost), sessions, turns, tool names and timings.
- No prompts, SQL, paths or node names are sent. Feedback transcripts are kept up to 400 days.
- Opt out with `DO_NOT_TRACK=1` or `DBT_SEND_ANONYMOUS_USAGE_STATS=false` (`dbt-ai/wizard-telemetry.md`).
