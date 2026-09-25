# ODS's answer to dbt Wizard

Date: 2026-09-25.
- Facts about Wizard: [`sources/dbt-wizard.md`](sources/dbt-wizard.md).
- Related: [`mcp-strategy.md`](mcp-strategy.md) (dbt-mcp) and [`metadata-ux-strategy.md`](metadata-ux-strategy.md).
- Roadmap: M6 (#32–#54), plus the new #169 and #173.

## What Wizard is, in one paragraph
Wizard is dbt Labs' AI agent for analytics engineering.
- **How it works:** a coding-agent harness (a closed Rust CLI whose surface mirrors OpenAI's Codex CLI) plus a proprietary "metadata engine" built from dbt artifacts. The engine answers lineage, column-lineage and impact questions for the model.
- **Where it runs:** Studio IDE, a platform home tab, a CLI and a Desktop app.
- **What it does:** builds and refactors models, writes tests, docs and metrics, debugs failed jobs, reviews diffs, and answers data questions in Explore mode.
- **How it checks its work:** a validation sub-agent (compile, deferred runs, dev vs prod comparison). The docs say plainly that validation "is evidence, not a guarantee".
- **What it costs:** usage is metered per token against account-wide credits. Trials need an admin and a business email. AI is on by default.

## Where it falls short
1. **Its evidence is prose.** Wizard shows SQL and diffs in a chat. There is no structured, machine-checkable record of *why* a change is safe, and correctness is explicitly disclaimed. In CI you can't gate on a chat transcript.
2. **The engine that makes it good is closed.** Column-level lineage, impact and health checks live in dbt's proprietary metadata engine. Other agents (Claude Code, Cursor, Codex) only get dbt-mcp, whose column-lineage tools need a platform account or the proprietary LSP.
3. **It is one more harness.** Teams already standardise on a coding agent. Wizard asks them to adopt a second one, with its own config, skills paths, plugin format and billing.
4. **Account and cost coupling.** Managed inference, Desktop and the platform surfaces require a dbt account and seats.
   - Credits are shared per account, so 5 or 500 users get the same $100/month.
   - Sub-agents and validation multiply token use.
   - Enterprise may lose access without a committed spend.
   - Legacy Team plans have no access.
5. **Data leaves by default.** Approved query results, which can be row-level, go to the LLM provider. Chat history is kept 90 days, and feedback transcripts up to 400 days.
6. **Uneven surfaces.**
   - The platform has no custom MCP, plugins, hooks or bash.
   - Studio has no Plan mode.
   - The Fusion migration workflow is Studio-only.
   - Desktop is a private beta and supports GitHub only.
   - Some features are v2-only.
7. **Safety is prompt-based.** Wizard asks for approval on each action, or you run it with `--dangerously-bypass-approvals-and-sandbox`. There is no declarative policy of what may run where, and no audit trail beyond the chat.
8. **No awareness of State or cost.** Wizard can defer to dbt State, but it doesn't use State's decisions, freshness or cost when planning a change.

## What ODS should build
**Principle: the LLM proposes, ODS proves.** Every answer and every change carries evidence produced by deterministic ODS engines, never by the model alone.

### 1. Engines first; they are already ODS's moat
Everything an agent needs to be *right* already exists or is planned as a plain, testable engine:
- column-level lineage and impact, with reasons and the readers it skips (shipped in #164–#167);
- observed lineage from Unity Catalog (#167);
- State decisions (M1);
- usage (M3);
- ERD (M3);
- CI impact (M4);
- data diff (#109).

These are open, local and free. An agent built on them doesn't have to guess what a change affects; it asks.

### 2. Plug into the agents people already use (M2)
- **`ods mcp`** (#169) exposes the engines to any MCP client: read-only, no login.
- **The ODS skills pack** (#173): skills in the Agent Skills (`SKILL.md`) format for change impact, safe refactor, explaining State, and validation. They work in Claude Code, Codex, Cursor and others.

This beats Wizard on its own ground at a fraction of the cost. We don't build or maintain a harness, and users keep their agent, model and billing.

### 3. A thin ODS agent where a harness is needed (M6)
Needed for CI, scheduled jobs and headless review. This is the re-scoped #32. It is not a chat UI:
- `ods agent review --base main` for PRs;
- `ods agent investigate <run>` (#35);
- `ods agent fix` for scoped tasks.

It runs on any provider through the LLM provider abstraction (#33): BYOK, local models (Ollama, LM Studio), or a gateway.

### 4. Proof-carrying changes (#50, #51, #52)
Every change the agent proposes comes with an evidence bundle: JSON with a schema version, rendered as text and as a PR comment. It contains:
- the column-level impact set, and which readers were skipped and why;
- the State plan: what would rebuild and what can be reused;
- tests run on the impacted set, and their results;
- data diff, dev vs prod (#109), when policy allows it to run;
- confidence and opaque spots: "`scores` is a Python model; lineage observed, not proven";
- the policy decisions taken.

CI can gate on it: "no removed column with downstream readers", "impact limited to marts". Wizard can't offer this.

### 5. Policy instead of prompts (#9, #98)
A declarative policy states what the agent may do: read, edit paths, run `dbt build` on dev targets, query data or not, and a token budget. Destructive actions are denied by policy, not left to a y/n prompt. Every decision is recorded in the audit log (#98).

The default is **metadata-only**: no row-level data is sent to the model unless the policy allows it, and then with redaction.

### 6. Cost you can see
- Deterministic engines answer most questions without tokens.
- A per-task budget and a cost line in the evidence bundle.
- No credits, no markup, no account.

## Naming
The dbt name, *Wizard*, promises magic. ODS's pitch is the opposite: **no magic, just evidence**. Options:

| Name | Command | Why | Risk |
|---|---|---|---|
| **Steward** (recommended) | `ods steward review`, `ods steward fix` | A data steward is the trusted, accountable guardian of data: governance, care and evidence. It contrasts well with "wizard". | A common word, so pair it with the ODS brand ("ODS Steward"). |
| Foreman | `ods foreman …` | The person who checks the work on site. | Industrial tone; less familiar to data people. |
| Sentinel | `ods sentinel …` | Guarding production, CI gates. | Sounds like monitoring rather than building. |
| Keep "ODS Agent" | `ods agent …` | Plain and self-explanatory. The roadmap already uses it. | Forgettable; hard to market. |

**Recommendation.** Keep `ods-agent` as the crate and module name (it's descriptive, and ADR-0001 uses it). Brand the user-facing experience **ODS Steward**, with `ods steward` as an alias of `ods agent`, and the tagline "*Every change, with proof.*" Before adopting the name, check trademark and package-name availability (#104).

## Roadmap impact
- **M2:** `ods mcp` (#169) and the skills pack (#173). This makes ODS "agent-ready" roughly a year before M6.
- **M6:** re-scope #32 as the headless agent described in section 3, with the evidence bundle as a first-class contract (#51, #52).
- **#33 (LLM providers):** add local model providers and gateways, and a per-task budget.
