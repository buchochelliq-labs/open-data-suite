---
hide:
  - navigation
---

# OpenDataSuite (ODS)

!!! danger "Very experimental"
    ODS is **pre-alpha software**. There is no release yet. Commands, flags, output
    and file formats change without notice. Results can be incomplete or wrong. Use it
    to explore and give feedback, and don't rely on it for production decisions.

ODS is an open-source toolkit for analytics engineering, written in Rust. It reads a
dbt project's compiled artifacts and answers questions about them. Today's commands
need no account, read only local files and collect no telemetry. Answers come with the
evidence behind them.

<div class="grid cards" markdown>

- **Column-level lineage.** Where every column comes from and what uses it, down to
  joins, filters and aggregations. Includes an offline explorer.
  [See it on a demo project →](lineage.md)

- **Change impact.** Which models must run when a column changes, and which can be
  skipped because they don't read it.
  [How impact is decided →](lineage.md#change-impact)

- **Entity-relationship diagrams.** Keys and relationships from your tests,
  constraints and joins, each labelled with how it's known.
  [ERDs →](capabilities.md#entity-relationship-diagrams)

- **An MCP server for AI agents.** The same engines as read-only tools for agents,
  including help writing SQL for people new to a project.
  [MCP →](capabilities.md#mcp-server-for-ai-agents)

</div>

## Try it in five minutes

```sh
cargo install --locked --git https://github.com/buchochelliq-labs/open-data-suite ods-cli
cd my-dbt-project && dbt compile
ods lineage view --open
```

[Getting started](getting-started.md) has the details.

## What makes it different

- **Conservative.** When ODS can't tell, it assumes the safer answer. It marks a
  model as affected, not skipped. It labels a relationship as a guess, not a fact.
- **Explainable.** Every decision carries a reason chain and evidence, as text and
  as JSON.
- **Provider-neutral.** dbt is the first project format and Databricks the first
  warehouse. Neither is built into the core ([Principles](principles.md)).
- **Open.** ODS works with dbt through its public artifact formats, and is built on
  open-source libraries ([Legal and trademarks](legal.md)).

ODS is an independent open-source project. It is not affiliated with, endorsed by or
sponsored by dbt Labs, Databricks or any other vendor named on this site.
