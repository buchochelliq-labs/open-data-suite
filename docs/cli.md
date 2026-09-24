# `ods` command-line reference

`ods` is one binary with a subcommand per module. This page covers what every
command shares. Design rationale is in [ADR-0003](adr/0003-cli-presentation-boundary.md)
(output) and [ADR-0004](adr/0004-cli-framework-and-exit-codes.md) (framework, exit
codes).

## Commands

| Command | Status |
|---|---|
| `ods state` | planned: M1 State MVP (v0.1.0) |
| `ods erd` | planned: M3 ERD & Usage (v0.3.0) |
| `ods usage` | planned: M3 ERD & Usage (v0.3.0) |
| `ods ci` | planned: M4 ODS CI (v0.4.0) |
| `ods lsp` | planned: M5 LSP & VS Code (v0.5.0) |
| `ods agent` | planned: M6 ODS Agent (v0.6.0) |
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
  error.
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
- **Secrets:** credentials must be references such as `{ secret = "env:VAR" }`. A
  plaintext value under a credential-like key (`token`, `password`, `api_key`,
  `access_token`, …) is rejected. The configuration never holds a secret's value.
- **Explain:** `ods config explain [KEY]` shows each effective value, where it came
  from and what it overrode (`--json` for machines):

```text
key                                 value                         source
output.width                        120                           local file ./.ods/local.toml
providers.warehouse.settings.host   "dev.cloud.databricks.com"    profile `dev` in project file ./ods.toml
providers.warehouse.settings.token  secret(env:DATABRICKS_TOKEN)  project file ./ods.toml
```

Configuration errors exit with status 4.
