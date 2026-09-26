# ADR-0005: Configuration and profiles

- **Status:** Proposed
- **Date:** 2026-09-24
- **Issues:** #7 (secret resolution is #126; policy semantics are #9)
- **Deciders:** @n1ckyb

## Context
ODS needs a single configuration system for project identity, output and logging
defaults, provider instances (dbt, Databricks, …), policy settings and per-environment
variation (dev, CI, prod). Issue #7 requires:
- deterministic, documented and tested precedence;
- schema validation;
- an explain command;
- that provider secrets never end up in state snapshots.

The constraints come from AGENTS.md:
- rule 1: configuration can't encode vendor logic in core;
- rule 3: problems must fail loudly, not silently fall back;
- rule 9: secrets are referenced and never stored.

ADR-0004 already reserves exit status 4 for configuration errors.

## Options considered

### Format
- **TOML (chosen).** Rust ecosystem standard (`Cargo.toml`), typed values, comments,
  no indentation traps, strict parser.
- *YAML* is familiar from dbt, but its implicit typing (`no` → false) and indentation
  make it error-prone. An unmaintained YAML crate was also just removed from our tree
  (ADR-0003).
- *JSON* has no comments, so it's poor for hand-edited files.

### Merging model
- **Flatten every layer to `key path → value`, overwrite in precedence order (chosen).**
  Precedence becomes a simple, testable overwrite, and every value keeps its full
  history, which makes `explain` trivial and exact. Arrays and secret references are
  leaves, so they are replaced, never merged element by element. Every setting carries
  a sequence number. A key is in effect only if no ancestor or descendant key was set
  later, so a higher layer can replace a table with a scalar, or a scalar with a table.
  The replaced keys then drop out, and `explain` lists them under what replaced them.
- *Deep-merge nested tables, then deserialize.* Loses provenance, so `explain` would
  have to re-derive it.
- *A config library (`figment`, `config-rs`).* Adds a dependency, and provenance or
  secret rules would still be ours to build.

## Decision

### 1. Files and discovery
| Layer | Location | Committed? |
|---|---|---|
| user | `$XDG_CONFIG_HOME/ods/config.toml` (only if that path is absolute, per the XDG spec), else `$HOME/.config/ods/config.toml`, else `%APPDATA%\ods\config.toml`. On Windows shells that set `HOME` (e.g. Git Bash), `HOME` wins. | no |
| project | nearest `ods.toml` in the working directory or an ancestor | yes |
| local | `.ods/local.toml` next to `ods.toml` (add `.ods/` to `.gitignore`) | no |

A missing file is not an error. `explain` lists which files were found. When no
`ods.toml` exists, it shows the directory the upward search started from.

### 2. Precedence (lowest → highest)
1. built-in defaults
2. user file
3. project file
4. local file
5. **the active profile**: `[profiles.<name>]` sections from all three files, merged
   in the same file order
6. environment variables `ODS__<SECTION>__<KEY>`, e.g. `ODS__OUTPUT__WIDTH=120`.
   - The value is parsed as a TOML value when possible (`120`, `true`, `"x"`), otherwise
     as a string. To force a string that looks like a number, quote it, e.g.
     `ODS__PROJECT__NAME='"1.0"'`.
   - Each segment matches an existing key case-insensitively, so
     `ODS__PROVIDERS__MYWH__KIND` reaches `[providers.MyWh]`. Segments that match no
     existing key are lowercased.
   - Keys whose names contain `__` or `.` can't be reached from the environment.
   - Variables are applied in sorted order, so the result does not depend on
     environment order.
   - An `ODS…` variable whose value is not valid UTF-8 is an error, not silently
     ignored.
7. command-line flags (`--output`, `--json`, `--color`, `--width`)

The active profile is chosen by `--profile`, then `ODS_PROFILE`, then
`default_profile`. Selecting a profile that no file defines is an error (ODS-E0104) that
lists the defined profiles. `default_profile` may only be set at a file's top level:
setting it inside a profile or through `ODS__DEFAULT_PROFILE` could never take effect, so
it is an error.

Logging keeps ADR-0004's order and slots the configured level below the flags:
`--log-level` (added in #220), then `ODS_LOG`, then `-v`/`-q`, then `log.level`, then
`warn`.

### 3. Schema
The typed schema lives in `ods-config` (a foundation crate, per ADR-0001):
- `version` (must be 1)
- `default_profile`
- `[project] name`
- `[output] format | color | width`
- `[log] level`
- `[providers.<name>] kind` plus a `settings` table
- `[policy] rules` table

Every schema table rejects unknown keys (ODS-E0102), so a typo fails instead of being
ignored. Errors name the dotted key and the layer that set it, e.g.
`invalid configuration at output.width (from environment ODS__OUTPUT__WIDTH)`.

Provider `settings` and policy `rules` are opaque tables. The provider (#2) or policy
engine (#9) that consumes them validates their contents. Core never branches on a
provider's `kind` (rule 1); the CLI uses `kind` to pick an implementation.

### 4. Secrets
- A credential is written as a **reference**: `token = { secret = "env:DATABRICKS_TOKEN" }`.
  The form is `<scheme>:<name>`. Which schemes exist, and how references are resolved,
  is decided by `SecretProvider`s in #126.
- Anywhere in the configuration, any value at or under a key named like a credential
  **must** be a reference. Credential-like names are `token`, `password`, `passwd`,
  `passphrase`, `secret`, `api_key`, `private_key`, `credentials`, `client_secret`,
  `connection_string` and `dsn`, or a name ending in `_<word>`, such as
  `access_token`. The check covers:
  - values nested in tables or arrays (`token = { value = "…" }`,
    `conn = [{ password = "…" }]`);
  - every layer, including values that a higher layer overrides.

  A plaintext value is an error (ODS-E0103).
- Every `{ secret = … }` reference must parse as `<scheme>:<name>`. A malformed one is
  also ODS-E0103.
- No configuration error ever includes a configured value:
  - credential and reference errors name only the key;
  - TOML parse errors give line, column and the parser's short message, without the
    source line;
  - quoted values in schema errors are replaced with `<value>`.
- As a result, the effective configuration **never contains a secret value**, only
  references. This is what lets later work (the State store, #11; audit, #98) persist
  or fingerprint configuration safely. State snapshots may record the references,
  never resolved values, and `SecretProvider`s must not write resolved values back
  into `Config`.

### 5. Explain
`ods config explain [KEY]` lists, for each effective key (or keys under `KEY`):
- the value, with secrets shown as `secret(env:NAME)`;
- the layer that set it;
- the values it overrode.

It also lists the files considered and the active profile, and how that profile was
selected. It's available in every output mode (ADR-0003), so CI can check configuration
with `--json`.

### 6. Errors
| Code | Meaning |
|---|---|
| ODS-E0101 | A configuration file can't be read or isn't valid TOML (the message includes line and column). |
| ODS-E0102 | Schema violation: unknown key, wrong type or value out of range. |
| ODS-E0103 | Plaintext credential. |
| ODS-E0104 | Unknown profile. |

All configuration errors exit with status 4 and are reported in the active output mode
(ADR-0004 §4). Configuration loads for every command, so a broken file is reported
immediately rather than when a feature first reads it.

## Consequences
- **Positive:**
  - Precedence is one ordered overwrite with tests for every layer.
  - `explain` is exact, because provenance is recorded, not reconstructed.
  - Plaintext values under credential-like keys are rejected, which reduces the risk
    of leaking secrets into state, logs and CI output.
- **Negative / trade-offs:**
  - Arrays are replaced, never merged: a local file can't append to a project-level list.
  - The credential-name heuristic can reject an unusual non-secret key such as
    `api_key_header_name`. The workaround is renaming the key; the heuristic is
    deliberately conservative (rule 3).
  - Every command pays the cost of loading config (a few small files).
- **Follow-up work:**
  - #126: `SecretProvider` contract and resolvers (`env`, keychain, cloud).
  - #2: providers validate their `settings`.
  - #9: policy validates `rules`.
  - #11: State snapshots store configuration fingerprints and references only.

## References
- #7, #126; ADR-0001 (layers); ADR-0003 (output modes); ADR-0004 (exit status 4)
- `docs/cli.md#configuration`
