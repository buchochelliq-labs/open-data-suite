# ADR-0023: `ods doctor`: typed health checks, stable codes and exit semantics

- **Status:** Accepted (2026-10-04)
- **Date:** 2026-09-29
- **Issues:** #181 (builds on #6, #7, #188, #227, #230, #17)
- **Deciders:** @n1ckyb

## Context
Issue #181 asks for a first-class `ods doctor` that says whether ODS can work in the
current project and environment: project discovery and dbt artifacts, configuration and
where each value came from, dbt and its version, the target, the state store,
provider capabilities, and live connectivity. Its constraints:

- diagnostics are typed data, not formatted strings;
- human output uses the ODS presentation layer (ADR-0003), with plain and JSON too;
- unknown or inconclusive checks are reported as such, never as success;
- exit statuses are deterministic for the healthy, warning and failure cases.

Much of what it checks already exists, and must not be implemented twice:

- `ods state doctor` (#188, ADR-0018) checks the state database: integrity, schema
  version, migrations and snapshot chains;
- configuration loading (ADR-0005) already reports each value's source and refuses
  plaintext credentials;
- the dbt executor already finds dbt (#214), renders the target identity (ADR-0017),
  checks relations (ADR-0016) and probes table versions (ADR-0022).

The M1 audit on #181 limits it to project, configuration, store and tools checks: live
provider connectivity (a Databricks SQL connection, #15) depends on M2. The live checks
that already exist in M1 run *through dbt's own connection*, so they need no credential
in ODS (AGENTS.md rule 9).

AGENTS.md rules that apply: 1 (the model in core is provider-neutral), 3 (missing
evidence is never success), 4 (findings carry evidence and render as text and JSON), 7
(presentation at the CLI edge, `CliError` and ADR-0004 exit codes) and 9 (never print a
resolved secret).

## Options considered

### Where the model lives
#### Option A: in `ods-cli`, next to the command
- ✅ Nothing to share yet.
- ❌ The LSP (#67), the MCP server and `ods-web` will show the same findings; they would
  each re-model them.

#### Option B: in `ods-core` (chosen)
- ✅ Presentation-free and provider-neutral, at the bottom of the graph, so every
  consumer (CLI, server, editor) can use it (ADR-0001).
- ❌ `ods-core` grows a small module unrelated to State.

`ods-sdk` was also possible, but nothing in the model is a provider contract: providers
don't report health themselves in M1.

### How codes are allocated
#### Option A: a new `ODS-D####` namespace for every doctor finding
- ✅ Obviously distinct.
- ❌ A missing manifest would be `ODS-E0201` from `ods state plan` and `ODS-D…` from
  `ods doctor`: two codes for one problem.

#### Option B: extend ADR-0004's `ODS-<letter><NNNN>` scheme (chosen)
- ✅ One code per problem across commands; the number blocks already group areas.
- ❌ Letters other than `E` are new, and must be documented.

### What a failed verdict exits with
#### Option A: the exit status of the worst finding's area (e.g. 4 for configuration)
- ❌ Several failures give several candidate statuses; the order of checks would decide.

#### Option B: 1 (failure)
- ❌ Scripts can't tell "the doctor couldn't run" from "the doctor found problems".

#### Option C: 5, "check failed" (chosen)
- ✅ ADR-0004 defines 5 as "the command ran correctly and its verdict is negative",
  which is exactly a doctor finding problems. One status, whatever failed; the
  findings' codes carry the detail.

## Decision

### 1. Model (`ods_core::diagnostic`)
Each check yields a `CheckResult`:

| Field | Meaning |
|---|---|
| `id` | stable id, `<category>.<name>`, e.g. `project.manifest` |
| `category` | `config`, `project`, `tools`, `target`, `state_store`, `capabilities`, `connectivity` |
| `provider` | the provider the check concerns (`dbt`, `databricks`, `sqlite`), if any |
| `status` | `ok`, `warning`, `error`, `unknown` or `skipped` |
| `required` | whether ODS can't work without it |
| `code` | the finding's stable code; absent for `ok` and `skipped` |
| `message` | what was found, for people |
| `evidence` | ordered `{key, value, source?}` facts it relied on; `source` says where a value came from (flag, variable, file, default) |
| `hint` | what to do |

A `HealthReport` holds the checks (grouped by category, in a fixed order), a `summary`
of counts per status, `strict`, and the `verdict`: `healthy`, `warnings` or `failed`.
All types are `Serialize` and `Deserialize` (so a consumer can read a report back), `snake_case` and `#[non_exhaustive]`. Nothing in them names a
provider; the CLI supplies ids and provider names as data.

- `unknown` means the check couldn't conclude: a check it depends on failed (code
  `ODS-U0001`, evidence `depends_on`), or the evidence was inconclusive. It is never
  shown as `ok`.
- `skipped` means not run by choice (a live check without `--connect`, or an adapter
  with nothing to probe).

### 2. Codes
Codes are `ODS-<L><NNNN>`: `E` error, `W` warning, `U` unknown. The number blocks are
ADR-0004's (`00xx` general, `01xx` configuration, `02xx` project and artifacts, `04xx`
state), plus `05xx` tools, target and the doctor itself, and `06xx` provider
capabilities and live checks. A number is used with one letter only, so a code alone
identifies the finding.

- A finding that means what an existing command error means reuses its code:
  `ODS-E0101`–`E0104` (configuration), `ODS-E0201` (manifest missing, unreadable or
  unsupported), `ODS-E0401` and `ODS-E0405` (state store).
- New: `ODS-U0001` (depends on a failed check), `ODS-E0204`, `W0205`, `W0206`, `U0207`
  (project), `ODS-E0501` (the doctor's verdict), `E0502`, `E0503`, `W0504`, `U0505`,
  `E0506`, `U0507`, `U0508` (dbt and its adapter), `E0509` (target), `W0601`, `W0602`,
  `E0603`, `E0604`, `U0605`, `W0606`, `U0607` (capabilities, live checks).

`docs/cli.md` lists every code with its hint; a test fails if one is missing there.

### 3. Exit semantics
The verdict decides, the output mode never does:

| Outcome | Exit |
|---|---|
| every check `ok` or `skipped` | 0 |
| warnings, or `unknown` on optional checks | 0; with `--strict`, 5 |
| any `error`, or `unknown` on a required check | 5 |

- **Required** checks are those ODS can't work without: `config.load`,
  `config.resolution`, `project.dbt_project`, `project.manifest`, `tools.dbt`,
  `target.identity`, `state_store.database`. An unknown required check fails: ODS can't
  say it will work, so it doesn't (AGENTS.md rule 3).
- A failed verdict is `ODS-E0501` with exit status 5. In JSON the envelope carries the
  whole report in `result` and `ODS-E0501` in `diagnostics`; in human and plain modes
  the report goes to stdout and the error to stderr (ADR-0004 §4).
- **Invalid configuration is a finding.** Other commands stop with status 4 before
  they run. `ods doctor` runs on configuration from the flags alone, reports the error
  as `config.load` (with its `ODS-E010x` code) and exits 5. Checks that read settings
  are `unknown` (`depends_on: config.load`), since a configured `project_dir` may not
  have been read. A `Module` opts into this with `diagnoses_config()`.
- Usage errors (e.g. `--provider nope`) are still status 2, and a failure to write
  output is still 1.

### 4. Checks, offline by default
Offline means **ODS runs no warehouse query**: it runs `dbt --version`, and dbt renders
the profile without connecting. dbt itself may use the network for its own version
check and usage statistics. ODS writes nothing; dbt writes the target check's artifacts
and its log. An integration test asserts which dbt commands a default run makes. The
default set:

| Check | What | Provider |
|---|---|---|
| `config.load` | files considered, profile; a load error is its code | |
| `config.values` | every effective value and its source; credentials only as references, and connection strings without user, query, fragment or options | |
| `config.resolution` | where `ods state` finds dbt, the project, target dir, state db and environment, and from what (flag, `DBT_*` variable, config file, profile, default) | |
| `project.dbt_project` | `dbt_project.yml` in the project directory | dbt |
| `project.manifest` | the manifest (or dbt's Information Schema) reads, at a supported schema | dbt |
| `project.name` | the manifest names its project | dbt |
| `project.freshness` | the manifest is newer than every project file (`.sql`, `.yml`, `.yaml`, `.csv`, `.py`, outside `target/`, packages and hidden directories) | dbt |
| `tools.dbt` | `dbt --version` runs; 1.7+ supported (manifest v11), 2.x untested | dbt |
| `tools.adapter` | the manifest's `adapter_type` is among dbt's plugins | dbt |
| `target.identity` | dbt renders the target (ADR-0017) | dbt |
| `state_store.database` | `ods state doctor`'s check, shared, not re-implemented | sqlite |
| `capabilities.relation_existence` | a provider can check relations before reuse (#230) | dbt |
| `capabilities.relation_versions` | sources get data versions: table versions for the adapter, else `loaded_at_field`; what's missing and its consequence | by adapter* |

- **`target.identity` is in the default set.** It needs dbt to render the profile
  (`dbt compile --inline` with `--no-populate-cache --no-introspect`), which reads
  `profiles.yml` but opens no warehouse connection. A profile dbt can't render stops
  every recording run (ADR-0017 §4), so it is the first thing a user needs to know. If
  dbt can't be run, the target is `unknown`, which fails the run, since it is required.
  dbt writes its artifacts for this check under `target/ods-target-check`, as
  `ods state run` does; the doctor writes nothing else.
- \* The source-version checks concern `databricks` when the manifest's adapter is
  Databricks (whose table versions ODS reads) and `dbt` otherwise, so `--provider`
  selects them by the project's adapter. If the manifest can't be read, they are shown
  under the provider asked for, so the failure isn't hidden.
- `dbt --version` may look up dbt's latest release itself; ODS doesn't, and dbt reads no
  profile to answer.
- Only dbt Core's `- installed:` line is read (`installed version:` before dbt 1.0);
  anything else, including other programs that call themselves dbt, is an unknown
  version (`ODS-U0505`), never a number picked from elsewhere in the output. dbt's
  versions are PEP 440 (`1.9.0b1`), which semantic-version crates reject. They are
  parsed with `pep440_rs` (Apache-2.0 OR BSD-2-Clause, about 3,500 lines, used by uv);
  without default features it adds only `unicode-width` and `unscanny` (both MIT OR
  Apache-2.0), plus `once_cell` and `serde`, already in the tree. Only major and minor
  decide support.
- A freshness scan that can't read a directory or file, or a modification time that
  isn't a date, makes `project.freshness` `unknown` (`ODS-U0207`), never `ok`.

**`--connect`** adds the live checks that already exist in M1, through dbt's own
connection (no credential in ODS):

| Check | What | Provider |
|---|---|---|
| `connectivity.relations` | the relation check (`dbt show`, ADR-0016) over every model, seed and snapshot | dbt |
| `connectivity.table_versions` | the table-version probe (ADR-0022) over every source; `skipped` for adapters without table versions, `unknown` (`ODS-U0507`) when the manifest names no adapter | by adapter* |

A failure there is an error (`ODS-E0603`, `ODS-E0604`) with a hint. A check that ran
but couldn't conclude isn't a pass: a relation check that couldn't tell about some
relations is `unknown` (`ODS-U0605`); a probe that found no table version for some
sources warns (`ODS-W0606`), and for none is `unknown` (`ODS-U0607`). Without
`--connect` both are `skipped`, so it is visible that they didn't run.

**Filters.** `--project` keeps the project checks; `--provider dbt|databricks|sqlite`
keeps the checks that concern that provider. Both together keep their intersection.
A check whose precondition was filtered out still depends on it (e.g.
`--provider databricks` still reads the manifest) and says so if it failed.

### 5. Presentation
`ods doctor` is a `Module` whose result model is the `HealthReport` plus the `scope`
(`project_only`, `provider`, `connect`). Human output renders one tree per category,
each check with its status, code, evidence and hint; plain is the same line by line;
JSON is the ADR-0003 envelope, `command: "doctor"`.

## Consequences
- **Positive:**
  - One command answers "why doesn't ODS work here?", with a code per finding that
    docs, issues and scripts can refer to.
  - Invalid configuration, the most common first failure, is reported with the rest
    instead of stopping everything.
  - The model is reusable by the LSP, MCP server and `ods-web`.
  - `ods state doctor` and `ods doctor` can't disagree about the store: one code path.
- **Negative / trade-offs:**
  - The default run starts dbt twice (`--version`, the target check), which takes a
    few seconds on a large project. `--project` and `--provider sqlite` avoid it.
  - `ODS-E0101`–`E0104` exit 4 from other commands and 5 from the doctor.
  - Two new direct dependencies: `pep440_rs` (see §4) and `walkdir` (Unlicense OR MIT, already in the tree
    through other crates), for the freshness check's directory walk.
- **Follow-up work:**
  - M2 (#15, #126): a Databricks SQL connectivity check and secret resolution checks
    (a reference that doesn't resolve), under `connectivity` and `config`.
  - Provider-reported health (a `health()` on providers, ADR-0006) once third-party
    providers exist.
  - `ods doctor --fix` for safe remedies (e.g. `dbt parse` for stale artifacts).

## References
- #181; AGENTS.md rules 1, 3, 4, 7 and 9
- ADR-0003 (presentation and JSON envelope), ADR-0004 (exit and error codes), ADR-0005
  (configuration, secrets as references), ADR-0006 (capabilities), ADR-0016 (relation
  check), ADR-0017 (target identity), ADR-0018 (`ods state doctor`), ADR-0022 (table
  versions)
- `docs/cli.md#ods-doctor`
