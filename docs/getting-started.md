# Getting started

!!! warning "Experimental"
    These steps work on the project's demo and on small dbt projects, but ODS is pre-alpha. Expect
    rough edges, and please [open an issue](https://github.com/buchochelliq-labs/open-data-suite/issues)
    when something breaks.

## 1. Install

From the first release (v0.0.1), install next to dbt with pip:

```sh
pip install opendatasuite
ods version
```

cargo-binstall and direct downloads with checksums (Homebrew coming soon) are on the
[Install](install.md) page. Until v0.0.1 is out, build from source with Rust 1.90 or
newer ([rustup.rs](https://rustup.rs)):

```sh
cargo install --locked --git https://github.com/buchochelliq-labs/open-data-suite ods-cli
ods --version
```

The binary is called `ods`. CI builds and tests it on Linux, macOS and Windows.

## 2. Compile your dbt project

ODS reads the artifacts dbt writes. Lineage, ERDs and the MCP server only read them;
the `ods state` commands of step 6 run dbt for you, and reach the warehouse only
through dbt's own connection.

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

## 6. Build only what changed

`ods state` runs dbt on only the models whose code or upstream data changed since the
last successful run, and keeps that state in `.ods/state.db` in the project. Check the
setup first, then run it where you'd run `dbt build`:

```sh
ods doctor                     # configuration, project, dbt, target and state store
ods state plan                 # what would build, what would be reused, and why
ods state build                # dbt build, on only what needs it; records the run
```

![ods doctor --project on the demo project](assets/recordings/doctor/doctor.svg)

The first run builds everything. After you edit a model, the plan builds it and what
reads it, and reuses the rest:

![ods state plan after an edit to stg_orders](assets/recordings/state-plan/state-plan-after-change.svg)

`ods state build` shows dbt's progress and each node's result as it finishes, then a
report: what it ran, the run's totals (rows read "at least N" when the adapter didn't
report them for every node) and, per node, its result, time taken, rows and why it ran.

![ods state build after the edit](assets/recordings/state-build/state-build.svg)

Then ask why, or look back:

```sh
ods state explain customers        # why it builds, traced upstream to the root cause
ods state history                  # every recorded run, with its time and rows
ods state history --run <run_id>   # one run's per-node stats, from its journal
ods state retry --failed           # after a failure: build only what failed or was skipped
```

![ods state explain customers](assets/recordings/state-explain/state-explain.svg)

A failed node keeps its last good build, and its error is shown with quoted values and
SQL removed; the nodes that succeeded are recorded:

![ods state build with a failing node](assets/recordings/state-retry/state-build-failed.svg)

`ods state run`, `seed`, `snapshot`, `test` and `compile` work the same way, each named
after the dbt command it runs. The [CLI reference](cli.md#state-run) has every option.

## 7. Open the dashboard

```sh
ods serve                      # http://127.0.0.1:8765/
```

`ods serve` is read-only and listens on loopback. It shows Home, the Catalog and model
pages, Lineage with the State overlay, the Plan with its Why panel, and every run with
each node's stats ([more](cli.md#the-dashboard)).

![A tour of ods serve: Home, the Runs page and a partial run's nodes](assets/recordings/dashboard/runs/runs.webp)

## 8. Give an AI agent the same answers

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
- [State on Databricks](databricks.md), with source table versions.
- [CLI reference](cli.md), which covers every command and flag.
- [Roadmap](roadmap.md), for what's coming and when.
