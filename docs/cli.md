# `ods` command-line reference

`ods` is one binary with a subcommand per module. This page covers what every
command shares. Design rationale is in [ADR-0003](adr/0003-cli-presentation-boundary.md)
(output) and [ADR-0004](adr/0004-cli-framework-and-exit-codes.md) (framework, exit
codes).

## Commands

| Command | Status |
|---|---|
| `ods state policies` | available (preview): freshness policies read from dbt State configs, see [below](#dbt-state-configuration) |
| `ods state plan\|run\|explain\|…` | planned: M1 State MVP (v0.1.0) |
| `ods erd` | planned: M3 ERD & Usage (v0.3.0) |
| `ods usage` | planned: M3 ERD & Usage (v0.3.0) |
| `ods ci` | planned: M4 ODS CI (v0.4.0) |
| `ods lsp` | planned: M5 LSP & VS Code (v0.5.0) |
| `ods agent` | planned: M6 ODS Agent (v0.6.0) |
| `ods lineage columns\|impact\|export\|graph\|view` | available (preview): column-level lineage, see [below](#column-level-lineage) |
| `ods config explain [KEY]` | available |
| `ods version` | available |
| `ods completions <shell>` | available |

Planned commands already appear in `--help`. They accept any arguments and exit with
status 3 (`ods state plan --select x` reports "not implemented", not a usage error).

## Global flags

These work anywhere on the line: `ods --json version`, `ods version --json` and
`ods state plan --json` all produce JSON. After a `--` separator, everything is taken
literally.

| Flag | Values | Default |
|---|---|---|
| `-o`, `--output` | `human`, `plain`, `json` | `human` when stdout is a terminal, otherwise `plain` |
| `--json` | shorthand for `--output json` | |
| `--color` | `auto`, `always`, `never` | `auto` |
| `--width` | columns (≥ 20) | terminal width, or 100 when not a terminal |
| `-v`, `--verbose` | repeatable: `-v` info, `-vv` debug, `-vvv` trace | warnings only |
| `-q`, `--quiet` | errors only; conflicts with `-v` | |
| `--profile` | configuration profile | `ODS_PROFILE`, then `default_profile` |
| `-h`, `--help` | help for `ods` or any command | |
| `-V`, `--version` | top level only (`ods -V`); `ods version` gives details | |

## Output and streams

- **stdout** carries results only: styled (`human`), line-oriented (`plain`), or one
  JSON document (`json`).
- **stderr** carries logs, progress and, outside JSON mode, error messages.
- In **JSON mode**, stdout always receives exactly one document, even when the command
  fails:

```json
{
  "schema_version": {"major": 0, "minor": 1},
  "command": "state",
  "ods_version": "0.1.0",
  "result": null,
  "diagnostics": [
    {
      "level": "error",
      "code": "ODS-E0003",
      "message": "`ods state` is not implemented yet",
      "hint": "planned for M1 State MVP (v0.1.0); see docs/ROADMAP.md"
    }
  ]
}
```

`result` is the command's result model on success and `null` on failure. `hint` is
omitted when there is none.

Exceptions to the one-document rule:
- **Usage errors** (bad flags or an unknown command) are detected before the output mode
  is known. They are always printed as text on stderr, with exit status 2.
- **Output failures**: if writing to stdout itself fails (for example a full disk), the
  error is printed on stderr with exit status 1, because stdout can't carry it.
- **`ods completions`** always prints its shell script as-is, whatever the output mode.

## Exit status

| Code | Name | Meaning |
|---|---|---|
| 0 | success | The command did what was asked, including when stdout closes early (`ods … \| head`). |
| 1 | failure | The command could not complete: I/O, provider or internal error. |
| 2 | usage | Invalid arguments or flags. |
| 3 | not implemented | The command is planned but not available yet. |
| 4 | config | Configuration is invalid, including an unknown `ODS_LOG` value. |
| 5 | check failed | The command ran and its verdict is negative, e.g. a CI gate found blocking issues. |

The output mode never changes the exit status. These codes are a stable contract: new
meanings get new numbers.

## Error codes

| Code | Meaning |
|---|---|
| `ODS-E0001` | Writing command output failed. |
| `ODS-E0002` | Unexpected internal error. Please report it. |
| `ODS-E0003` | The command is planned but not implemented yet. |
| `ODS-E0004` | `ODS_LOG` holds an unknown log level. |
| `ODS-E0101` | A configuration file can't be read or isn't valid TOML. |
| `ODS-E0102` | Configuration schema violation: unknown key, wrong type or value out of range. |
| `ODS-E0103` | A credential is written as plaintext instead of a secret reference. |
| `ODS-E0104` | The selected profile is not defined. |
| `ODS-E0201` | dbt artifacts are missing, unreadable or an unsupported version, or lineage output can't be written. |
| `ODS-E0202` | The project graph is inconsistent (duplicate ids or relations, or a dependency cycle). |
| `ODS-E0203` | A model, column, change kind or dialect named on the command line doesn't exist. |

## Environment variables

| Variable | Effect |
|---|---|
| `ODS_LOG` | Log level: `off`, `error`, `warn`, `info`, `debug` or `trace`. Overrides `-v`/`-q` and `log.level`. |
| `ODS_PROFILE` | Configuration profile to use; `--profile` overrides it. |
| `ODS__SECTION__KEY` | Sets a configuration key, e.g. `ODS__OUTPUT__WIDTH=120`. |
| `NO_COLOR` | Any non-empty value disables colour when `--color auto`. `--color always` overrides it. |
| `TERM` | `dumb` or `unknown` disables colour (results and logs) when `--color auto`. |

## Shell completion

`ods completions <shell>` prints a completion script for `bash`, `zsh`, `fish`,
`powershell` or `elvish`. The script is generated from the commands in the binary, so it
always matches your version.

```bash
# bash
ods completions bash > ~/.local/share/bash-completion/completions/ods
# zsh (a directory on your $fpath)
ods completions zsh > ~/.zfunc/_ods
# fish
ods completions fish > ~/.config/fish/completions/ods.fish
# PowerShell (add to your profile)
ods completions powershell | Out-String | Invoke-Expression
```

## Configuration

`ods` reads TOML configuration from up to three files, plus profiles, environment
variables and flags. The design is in [ADR-0005](adr/0005-configuration-and-profiles.md).

| Layer (lowest → highest precedence) | Where |
|---|---|
| user file | `$XDG_CONFIG_HOME/ods/config.toml` (or `~/.config/ods/config.toml`, or `%APPDATA%\ods\config.toml`) |
| project file | nearest `ods.toml` in the current directory or a parent; commit it |
| local file | `.ods/local.toml` next to `ods.toml`; add `.ods/` to `.gitignore` |
| active profile | `[profiles.<name>]` sections from the files above |
| environment | `ODS__<SECTION>__<KEY>=value`, e.g. `ODS__OUTPUT__WIDTH=120` |
| flags | `--output`, `--json`, `--color`, `--width` |

Example `ods.toml`:

```toml
version = 1
default_profile = "dev"

[project]
name = "jaffle_shop"

[output]
width = 100

[providers.warehouse]
kind = "databricks"
settings = { host = "prod.cloud.databricks.com", token = { secret = "env:DATABRICKS_TOKEN" } }

[profiles.dev.providers.warehouse.settings]
host = "dev.cloud.databricks.com"

[profiles.ci.output]
format = "json"
```

- **Profiles:** select one with `--profile NAME`, `ODS_PROFILE=NAME` or
  `default_profile`, in that order of precedence. Selecting an undefined profile is an
  error. `default_profile` can only be set at a file's top level.
- **Environment variables:**
  - `ODS__A__B=value` sets key `a.b`. Values are read as TOML when possible (`120`,
    `true`), so quote a string that looks like a number: `ODS__PROJECT__NAME='"1.0"'`.
  - Segments match existing keys case-insensitively. Keys whose names contain `__` or
    `.` can't be set this way.
- **Replacing tables:** a higher layer can replace a whole table with a single value,
  or a value with a table. `explain` shows what was replaced.
- **Keys:**
  - `version`
  - `default_profile`
  - `project.name`
  - `output.format` (`human`, `plain` or `json`), `output.color` (`auto`, `always` or
    `never`) and `output.width` (at least 20)
  - `log.level` (`off`, `error`, `warn`, `info`, `debug` or `trace`)
  - `providers.<name>.kind` and `providers.<name>.settings`
  - `policy.rules`

  Unknown keys are errors.
- **Secrets:** credentials must be references such as `{ secret = "env:VAR" }`.
  - A plaintext value at or under a credential-like key (`token`, `password`,
    `api_key`, `access_token`, `dsn`, …) is rejected. This applies anywhere in any file,
    including values that a higher layer overrides.
  - Malformed references are rejected too.
  - Error messages never repeat configured values, and the configuration never holds
    a secret's value.
- **Explain:** `ods config explain [KEY]` shows each effective value, where it came
  from and what it overrode (`--json` for machines):

```text
key                                 value                         source
output.width                        120                           local file ./.ods/local.toml
providers.warehouse.settings.host   "dev.cloud.databricks.com"    profile `dev` in project file ./ods.toml
providers.warehouse.settings.token  secret(env:DATABRICKS_TOKEN)  project file ./ods.toml
```

Configuration errors exit with status 4.

## Column-level lineage

`ods lineage` reads a dbt target directory, parses every model's compiled SQL, and builds
column-level lineage. It reads dbt 1.7–1.12 (`manifest.json` v11/v12, plus `catalog.json`
when `dbt docs generate` has run) and dbt v2, either its `manifest.json` or, with
`--generate-info-schema`, the Parquet "dbt Information Schema"; with `--no-write-json` the
Information Schema is used automatically. All three give the same lineage ([ADR-0008](adr/0008-column-level-lineage.md)). It needs no dbt login, no
warehouse connection and no network.

```sh
dbt compile                      # or run/build: compiled SQL must be in the manifest
dbt docs generate                # optional: warehouse column lists for sources and seeds

ods lineage columns --model customers
ods lineage impact --column stg_orders.status            # which models must run, and why
ods lineage impact --column orders.amount=removed
ods lineage impact --base ../prod-target                 # diff two builds, impact of every change
ods lineage export --namespace unitycatalog://adb-123.azuredatabricks.net \
                   --output-file lineage.ndjson          # OpenLineage JobEvents
ods lineage view --open                                  # offline HTML explorer
ods lineage graph --format dot-columns --output-file g.dot && dot -Tsvg g.dot > g.svg
```

![The offline lineage explorer tracing customers.lifetime_value](images/lineage-viewer.png)

`ods lineage view` writes one self-contained HTML file (no network, no external scripts):
search models and columns (`/`), click a column to highlight everything upstream (blue)
and downstream (orange), toggle indirect edges, focus on the selection, and deep-link with
`lineage.html#node=<id>&column=<name>`. The same page and JSON contract will power the
VS Code view (#107).

`ods lineage graph --format` writes `json` (the documented graph contract,
`schema_version` 1), `dot` / `dot-columns` (Graphviz), `mermaid` (Markdown, model level)
or `graphml` (Gephi, yEd, Neo4j). With `graph` and `view`, `--focus MODEL[.COLUMN]`
(repeatable) plus `--upstream N` / `--downstream N` keeps only the connected part.

| Flag | Meaning |
|---|---|
| `--target-dir DIR` | dbt target directory; default `target` |
| `--artifacts FORMAT` | `auto` (default: `manifest.json` if present, else the Information Schema), `json`, or `info-schema` (dbt v2's Parquet `target/info_schema/v1/`) |
| `--dialect NAME` | `databricks`, `spark`, `duckdb`, `snowflake`, `bigquery`, `postgres`, `redshift` or `generic`; default: the manifest's adapter type |
| `--column MODEL.COLUMN[=KIND]` | (`impact`) a changed column; `KIND` is `modified` (default), `added` or `removed`; repeatable |
| `--base DIR` | (`impact`) another build to compare with; every difference in compiled SQL becomes column changes |
| `--run-events` | (`export`) write `COMPLETE` RunEvents instead of JobEvents, for sinks that only accept runs |
| `--indirect-in-fields` | (`export`) also copy row-shaping inputs into every field, for consumers that ignore the facet's `dataset` array |

How impact is decided, most conservative first:
- a model whose SQL can't be analyzed (a Python model, `select *` over a relation with
  unknown columns, unsupported syntax) is **opaque**: any change to what it reads makes it run;
- a change to which rows exist (filters, joins, grouping) makes every reader run;
- a modified or removed column makes a reader run only if it uses that column; added columns
  only reach readers that `select *`;
- every reader that is *not* affected is listed as skipped, with the changed columns it doesn't use.

## dbt State configuration

ODS reads dbt State configuration exactly as you already write it
([ADR-0011](adr/0011-dbt-state-config-compatibility.md)):
- `+state:` in `dbt_project.yml`;
- `config: state:` in YAML;
- `{{ config(state={...}) }}` in SQL;
- SAO's `freshness: build_after:`;
- source `loaded_at_field` / `loaded_at_query`.

It takes the values dbt resolved into `manifest.json` or the dbt v2 Information Schema,
so it behaves like your dbt version. dbt v2 merges the `state` block key by key; dbt 1.x
lets a more specific block replace a less specific one.

```sh
dbt parse                                  # or compile/build
ods state policies                         # every model, and how sources report new data
ods state policies --model orders --json
```

| Setting | ODS |
|---|---|
| `state.lag_tolerance` (`4h`, `45m`, `{count, period}`) | applied |
| `state.require_fresh_data_from` (`any`/`all`) | applied |
| `freshness.build_after` (`count`, `period`, `updates_on`) | applied where `state` leaves a setting unset |
| source `loaded_at_query` / `loaded_at_field` | applied (query first); otherwise warehouse metadata |
| `state.compare_unrendered_code`, `state.pre_clone` | not applied yet; ignoring them only means more rebuilds |
| `state.evaluate_volatile_sql: true`, `state.execute_hooks_on_any_reuse: true` | not applied yet; the model is never reused |
| any other `state.*` key, or a value ODS can't read | reported; the model is never reused |

Defaults: if any model configures `state:` or `build_after`, the project relies on dbt
State, so models without settings get dbt State's defaults (`45m`, `any`). Otherwise
ODS rebuilds on any new upstream data (tolerance `0`).
