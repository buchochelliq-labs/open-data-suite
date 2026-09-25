# Getting started

!!! warning "Experimental"
    These steps work on the project's demo and on small dbt projects, but ODS is pre-alpha. Expect
    rough edges, and please [open an issue](https://github.com/buchochelliq-labs/open-data-suite/issues)
    when something breaks.

## 1. Install

There are no prebuilt binaries yet. Build from source with Rust 1.90 or newer
([rustup.rs](https://rustup.rs)):

```sh
cargo install --locked --git https://github.com/buchochelliq-labs/open-data-suite ods-cli
ods --version
```

The binary is called `ods`. CI builds and tests it on Linux, macOS and Windows.

## 2. Compile your dbt project

ODS reads the artifacts dbt writes. It doesn't run dbt, and today's commands don't
connect to your warehouse.

```sh
cd my-dbt-project
dbt compile            # writes target/manifest.json, with compiled SQL
dbt docs generate      # optional: column types, and columns of sources and seeds
```

ODS reads dbt 1.7 to 1.12 (`manifest.json` v11 and v12) and dbt v2 (its
`manifest.json`, or the Parquet "Information Schema").

!!! tip "No dbt project handy?"
    Clone the repository and use the demo project's artifacts:
    `--target-dir fixtures/dbt/jaffle-ods/artifacts/dbt-1.10`.

## 3. Explore the lineage

```sh
ods lineage view --open                      # an offline HTML explorer
ods lineage columns --model customers        # where each column of a model comes from
```

## 4. Check what a change affects

```sh
ods lineage impact --column stg_orders.status
ods lineage impact --column orders.amount=removed
ods lineage impact --base ../prod/target     # compare two builds
```

## 5. See keys and relationships

```sh
ods erd generate                  # Mermaid erDiagram on stdout
ods erd generate --format dot | dot -Tsvg > erd.svg
```

## 6. Give an AI agent the same answers

```sh
claude mcp add ods -- ods mcp --target-dir target
```

Any MCP client works. See [the MCP server](capabilities.md#mcp-server-for-ai-agents).

## Output for scripts

Every command prints readable text by default. Add `--json` for a stable,
machine-readable envelope, or `--output plain` for line-oriented output. Exit codes
are documented in the [CLI reference](cli.md#exit-status).

## Next steps

- [Column-level lineage](lineage.md), with a live demo.
- [CLI reference](cli.md), which covers every command and flag.
- [Roadmap](roadmap.md), for what's coming and when.
