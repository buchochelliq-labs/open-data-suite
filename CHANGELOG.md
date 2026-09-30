# Changelog

All notable, user-visible changes to ODS. The format follows
[Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/), and ODS follows
[Semantic Versioning](https://semver.org/). [ADR-0019](docs/adr/0019-release-and-versioning.md)
defines what counts as breaking, the compatibility rules for each interface, and the
deprecation policy.

Before 1.0, a minor release may break. **While versions are 0.0.x, every release may
break**, even though 0.0.1 → 0.0.2 looks like a patch: it may change interfaces or
migrate the state store. Every break is listed under **Breaking**, with what to do.

## [Unreleased]

Everything so far is pre-release. The first public release, 0.0.1, will summarise the
State MVP and the first dashboard screens. Releases stay 0.0.x until the whole dashboard
design is built; 0.1.0 marks the complete dashboard. Entries below record changes since
the changelog was introduced.

### Breaking
- `SDK_VERSION` is now 0.3 (#323): the executor contract is 0.5 (an `ErrorSummary` may
  carry the `line` the engine reported), the SQL lineage analyzer contract is 0.2 (an
  opaque `QueryLineage` names the columns it couldn't resolve, `unresolved`), and there
  is a new `error_catalogue` contract (0.1). Out-of-process plugins must be rebuilt
  against it.
- `ods serve`'s dashboard JSON (`/api/shell`, `/api/home`, `/api/state/…`,
  `/api/catalog…`) is now `schema_version` 2: a run in `/api/state/runs` may have no
  snapshot (a failed run listed from its journal), so its `snapshot`, `recorded_at` and
  `kept` can be `null`, and `at` is the time it is listed by. Its `outcome` can also be
  `partial`, `unknown` or `unfinished` (#322).
- `SDK_VERSION` is now 0.2, as ADR-0019 bumps it whenever a contract changes (here the
  executor contract, #322). Out-of-process plugins must be rebuilt against it.
- The executor contract is now version 0.4: `Executor::execute_with_events` reports a
  run's events, and `ExecutionRequest` carries the state `scope` they are for. The
  method has a default, so an executor only changes to report events live.
  Out-of-process executor plugins must be rebuilt against the current SDK, whose
  `EXECUTOR` contract is 0.4 (#322, ADR-0024).
- The executor contract is now version 0.3: an `ExecutionRequest` carries the sources
  whose tests to run, and an `ExecutionReport` returns their outcomes. Out-of-process
  executor plugins must be rebuilt against the current SDK, whose `EXECUTOR` contract
  is 0.3 (#288, #232).
- `ods serve` now opens on the dashboard's Home page; the lineage explorer moved from
  `/` to `/lineage` (under `--base-path`, from `<base>/` to `<base>/lineage`). Update
  bookmarks and links to the explorer. Its API routes are unchanged (#310).

### Added
- A failed node is explained (#323, ADR-0025): `ods state run`, `seed`, `snapshot`,
  `build`, `test` and `ods state history --run` end with **Why it failed**, and `--json`
  with `failures`. Each explanation has a plain-language headline, a category (e.g.
  `database error · missing column`), how sure ODS is (`known pattern + evidence`,
  `known pattern` or `not recognised`), why ODS thinks so, from its own evidence (column
  lineage showing the column an upstream no longer produces and what it was renamed to,
  the manifest's macros, what changed in the run, new upstream data, and earlier runs),
  where (the file, and the line dbt's adapter reported), what to try with real commands
  to copy (`ods lineage impact --column …=removed`, `ods state retry --failed`,
  `dbt deps`), the nodes it blocked, and dbt's own message, redacted. An error ODS
  doesn't recognise never gets a guessed cause: it lists what ODS knows. A command that
  fails before any node runs (e.g. `dbt compile` can't find a macro) is explained too,
  with outcome `failed_before_running`. Explanations are computed when shown, so older
  runs get them.
- An `error_explain` capability and `ErrorCatalogue` SDK contract (#323, ADR-0025):
  providers classify a failed node's redacted error summary into ODS's neutral
  taxonomy, with a fake and a conformance suite. The dbt provider's catalogue
  recognises dbt's own errors (an undefined macro, a ref to a missing node, packages not
  installed, a missing profile or target, Jinja syntax, a Python model's exception, a
  failed test), DuckDB's (missing column, table or view, conversion, constraint,
  dependent entries, conflicts and locks), PostgreSQL's documented messages, and Apache
  Spark's and Delta Lake's error classes as Databricks reports them.
- The dashboard's Runs and Run pages (`ods serve`) show each run's outcome, duration,
  node counts and rows from its run journal (#322, ADR-0024): *succeeded*, *partial*,
  *failed*, or *unknown* / *running or stopped without finishing* when the journal
  doesn't say it ended (*probably stopped*, marked inferred, after 10 quiet minutes),
  never a success; an executor's "succeeded" with a node whose outcome isn't known
  reads *unknown*. A failed run that recorded no snapshot is
  listed too. The Run page's timeline draws when each node started and finished, its
  Nodes tab lists each node's status, start, time taken (compile and execute), rows,
  thread, tests and why it ran, and a failed node shows its redacted error summary.
  A stat that isn't reported reads `—` with the reason, never `0`; a rows total reads
  "at least N" when some nodes didn't report. A run without a journal says so. The
  `?outcome=` filter uses these outcomes. The `[duration]`, `[wall clock]` and
  `[start time]` placeholders are gone.
- `ods state run`, `seed`, `snapshot`, `build` and `test` show each node's stats
  (#322, ADR-0024): result, time taken, rows affected (`—` when dbt's adapter doesn't
  report them, never `0`), and a failed node's error with quoted values, numbers and
  SQL removed. Each node's result is a step line on stderr as it finishes, the report's
  table has *took* and *rows* columns, and the summary gives the run's totals, with
  "at least N" rows when some nodes didn't report. `--json` includes them under
  `run_stats`. The dbt executor reads them from dbt's structured log
  (`--log-format json --log-level debug`) as dbt runs, and from `run_results.json`.
- Every run that runs dbt appends its events to a journal,
  `<state-db>.runs/<run_id>.jsonl`, as they happen: one JSON event per line, with
  its `schema_version`, under the run id `.last-run.json` keeps (`journal` in the
  report). A failed run keeps its journal. It holds no SQL, `--vars` values or
  secrets. The 50 most recent journals are kept.
- `ods state history --run <run_id>` shows one run's per-node stats from its journal,
  also for a failed run that recorded nothing; `ods state history` shows each
  snapshot's run time and rows where its journal is kept (`run_stats` in JSON) (#322).
- Run events and per-node run stats in the SDK (#322, ADR-0024): an executor reports
  `run_started`, `node_queued`, `node_started`, `node_finished`, `check_finished` and
  `run_finished` as a run goes, each with the run id, scope and a millisecond
  timestamp. A finished node carries its status, start and end, time taken (compile and
  execute when timed), rows affected, the engine's other reported values, thread, test
  counts and, when it failed, a one-line error summary with quoted values, numbers and
  SQL removed (read failing closed: an apostrophe in `can't` opens no quote, and a line
  whose quoting can't be read loses everything from its first quote; any SQL statement
  or clause start is cut, after colour codes are removed; unquoted values after `=`
  are removed). The engine's
  other values and the thread are redacted too. A stat that isn't reported is missing,
  never zero, and the rows total says `rows_at_least` when any node that ran, or may
  have, didn't report rows. Executors with the new
  `run_events` capability report events live; for others they are rebuilt from the
  final report, without timing or rows. The fake executor simulates a run on parallel
  workers, and the executor conformance suite checks the events (14 cases).
- `ods serve` hosts the first slice of the ODS Dashboard (#310): the shell (navigation
  with every section of the design, planned ones greyed; project and target; search;
  the current snapshot; a *Local · read-only* badge) and Home. Home shows the planned
  nodes, what the last run built and which nodes kept an earlier build, the recent runs from the state store,
  the nodes that need attention (changed code, missing evidence, opaque lineage, from
  the plan against the latest snapshot) with a count of every planned build by reason,
  `[n]` placeholders for health and coverage, and which modules are ready or
  available. A run's nodes read *built* or *kept earlier build*, since a snapshot
  can't tell reuse from a node left out or failed. The plan is made again on every
  request, as lag tolerances expire with time. Without a state store it says how to
  record a first run; if the project's files can't be read, it says that instead. It
  never writes: the state database is opened read-only and is never created or
  migrated. New `serve` options `--state-db`, `--environment`, `--target` and
  `--sources` pick the state shown, as for `ods state plan`; the server also reloads
  when the state database or the source freshness results change.
- Two JSON routes return the dashboard's view models at `schema_version` 1:
  `/api/shell` and `/api/home`. Beyond loopback, `/api/home` omits the store's path and
  error text (#310, ADR-0009).
- The dashboard's Lineage page (#312): `ods serve` shows the lineage explorer at
  `/lineage` inside the dashboard's shell, restyled to the design, with a *State
  overlay* that colours each node by what the next run does with it: build, reuse,
  never built, or unknown (the evidence to reuse it is missing, so it builds). The
  decisions are `ods state plan`'s, made again on every request; without a state store
  every node shows as never built. Opaque nodes, such as Python models, are drawn
  dashed. A side panel gives the selected node's decision and reason chain, the
  readers that build with it, and links to its Model page (`/catalog/<id>`) and to its
  decision on the State plan page (`/state/plan?node=<id>`); its other tabs keep the
  explorer's details, columns and impact. `/lineage?node=<id>` selects a node. The
  overlay can be switched off. The page doesn't check the warehouse, so it says reuse
  is taken on trust, as `ods state run` does. A column trace that reaches an opaque
  node names where it stops and shows everything past it as *may change*. Nodes and
  columns can be reached and selected from the keyboard. A new JSON route,
  `/api/lineage/overlay`, returns the overlay at `schema_version` 1; beyond loopback it
  omits error text (ADR-0009).
- The dashboard uses IBM Plex Sans and Mono (SIL Open Font License 1.1), vendored and
  served by `ods serve` from `/assets/fonts/`; the Content-Security-Policy adds only
  `font-src 'self'`, so no font CDN is contacted (#310, ADR-0009).
- The dashboard's State pages (#311). **Plan** (`/state/plan`) lists every planned node,
  builds first, with its Build or Reuse pill and reason, filters by action, and opens a
  **Why** panel for the selected node (`?node=<id>`): its recorded build, which
  fingerprint parts changed, what it reads (parents' decisions, and each source's
  version with its strategy and origin, graded exact, semantic, proxy, inferred or
  unknown), the relation check, the decision and the reason chain, which is exactly
  `ods state explain`'s. The plan is made offline, so reused relations read *not
  checked*. **Runs** (`/state/runs`) lists the recorded runs, newest first, with
  outcome, target and date filters and counts; nodes read *built* or *kept earlier
  build*. Failures are known only for the last run, from the file `ods state retry`
  keeps beside the store, and only for the target it ran for: the page shows the nodes
  that failed or weren't recorded, the skipped ones, the state kept and
  `ods state retry --failed`. **Run** (`/state/runs/<run_id>`) shows a timeline of built
  and kept nodes, why each was built from what the snapshots record, and the earlier
  runs. Durations, start times and users read as placeholders; CI runs are a *Planned*
  tab. The State section lists its pages in the navigation, and Home's *All runs* and
  last run link here. JSON routes `/api/state/plan`, `/api/state/plan/<node>`,
  `/api/state/runs` and `/api/state/runs/<run_id>` return the same view models at
  `schema_version` 1 (`GET` only; beyond loopback without paths or error text). A
  command line shown there keeps option names but redacts their values (except the
  selection and target) and everything after `--`. The plan is made at most every 30
  seconds per reload, and the server also reloads when the last run's file changes.
- The last-run file beside the state database (`<state-db>.last-run.json`) is now at
  version 1.2: it also keeps the state scope the run was for and its run id, so the
  dashboard shows a run only for its own target and ties it to its snapshot.
  `ods state test` now keeps how its run ended too, as `run` and `build` do. Older
  files still read (#311, ADR-0009).
- The dashboard's Catalog and model pages (#313). `/catalog` lists every model, seed
  and snapshot with facets (resource type, layer from the model's folder, materialization,
  tags, next-run decision from the plan, lineage confidence) whose selections, search and
  sort are kept in the URL, each node's next-run pill and last successful build, and a
  `[n]` health placeholder (#117). `/catalog/<unique_id>` has Overview, Code, Columns,
  Lineage, State and Tests tabs; column types are shown only when the artifacts record
  them (warehouse types as of the catalog's date), inferred layers and column lineage
  are marked, and a test reads *passed* only while the checks that passed are still the
  node's checks. Compiled SQL is never served, as it can contain resolved secrets: the
  Code tab shows the code as written. A *Reuse* says its relation isn't checked by the
  offline plan. Without a state store every node reads *never built*. `/api/catalog` and `/api/catalog/<unique_id>` return the same view
  models at `schema_version` 1 (ADR-0009).
- `ods doctor` checks that ODS can work in the current project: configuration (files,
  profile, every effective value and where it came from, credentials only as
  references and connection strings without their user, query or options), the dbt
  project and its manifest (found, readable, a supported schema, named, not older than
  the project's files), dbt and its adapter, the target dbt builds in, the state
  database (the same check as `ods state doctor`) and what the providers can do, with
  the consequence of what's missing. By default ODS runs no warehouse query: it runs
  `dbt --version` and has dbt render the profile, which doesn't connect (dbt's own
  version check and usage statistics may use the network). `--connect` adds the
  relation check and, on Databricks, the table-version probe, through dbt.
  `--project` and `--provider dbt|databricks|sqlite` narrow it; `--strict` fails on
  warnings. Each check is `ok`, `warning`, `error`, `unknown` or `skipped`, with a
  stable code, evidence and a hint, in human, plain and JSON output
  (`command: "doctor"`); a check that couldn't conclude is never `ok`. It exits 0 when
  healthy or with warnings only, and 5 (`ODS-E0501`) when a check fails or a required
  check can't conclude. Invalid configuration is reported as a finding rather than
  stopping it. The new codes (`ODS-U0001`, `ODS-E0204`–`U0207`, `ODS-E0501`–`E0509`,
  `ODS-W0601`–`U0607`) are listed in `docs/cli.md` (#181, ADR-0023).
- On Databricks, `ods state run`, `build`, `seed`, `snapshot` and `compile` (with or
  without `--dry-run`) read each source's Delta table version through dbt's own
  connection, in one `dbt show` query, and reuse the models reading a source only while
  its version (`<table id>/<version>`, exactness `exact`, origin `delta_history`) is
  unchanged. `loaded_at_field` is no longer needed for data-aware reuse there. A table
  version wins over `max_loaded_at`; a source that isn't a Delta table falls back to
  it; if the query fails, a warning names dbt's error and those sources count as
  changed. The
  first run after upgrading builds the readers of these sources once, since versions
  from different origins never compare equal. `ods state plan` and `ods state explain`
  stay offline and say they read no table versions (#308, #17, ADR-0022).
- Two plugin contracts for reading sources' data versions: `ChangeProvider` 0.1 reports
  each source's current data version, or why it can't; `RelationProbe` 0.1 runs a few
  read-only statement templates against each source's relation and returns their first
  rows. The `relation_probe` capability is new. Both have fakes and conformance suites
  (#17, #16, ADR-0022).
- `ods state explain` and plan JSON say where each source's data version came from:
  `source_version_strategy` names the strategy chosen (`relation_versions`,
  `source_freshness` or `no_version`), `source_version_origin` the version's own origin
  (e.g. `sources.json max_loaded_at`), and `source_version_skipped` says why a
  preferred strategy that could have applied wasn't used (#307, #17, ADR-0022).
- `ods state export --dbt-state <dir> --upstream <state dir>` writes a dbt state
  directory for `dbt retry --defer-state <dir>` and other runs with
  `--defer --favor-state`: the upstream `manifest.json` (e.g. prod's), in which the
  nodes ODS recorded as built in this target, and whose tables a warehouse check shows
  are still there, point at this target. Every other node keeps the upstream pointer,
  with a reason (`--output json`). It fixes a retry after a failure building on prod's
  copy of a model this target just built. New error code `ODS-E0406` for a held lock
  or a failed write (#304, #296).
- `ods-export.json`, written next to the exported `manifest.json`, is a new ODS
  document at `schema_version` 1.0: when and from which snapshot and target the export
  was made, the SHA-256 of the manifest it describes, and each node's choice and
  reason (#304, #296).
- A "State on Databricks" docs page shows `ods state` on a real Databricks workspace:
  after one model changes, only it and the view reading it are rebuilt. Its screenshots
  come from the nightly Databricks CI job (#302, #294).
- `ods state retry --failed` reruns the last command, but builds only the nodes that
  failed, or were skipped because of a failure, and tests only the sources whose tests
  failed, as `dbt retry` does. They are still planned: a node the plan now reuses is
  reused, with why; a node whose parent isn't built with it is held back rather than
  run on stale input. Nodes that changed since aren't built, and are listed as
  "changed since, not retried". If the last run succeeded, or kept no outcome,
  `--failed` says so and exits with `ODS-E0403` without running dbt. JSON output gains
  a `retry` object (#292).
- `ods state build` (with tests) and `ods state test` run the tests defined on sources,
  as `dbt build` does, but only when they could find something new: the source has new
  data, its data version is unknown, its tests changed, or they haven't passed yet. A
  failing source test fails the command and skips the models that read the source.
  Sources can be tested before anything is built. JSON output gains `source_tests`,
  `execution.sources` and `record.source_tests`; `based_on` is `null` when nothing was
  recorded yet (#288, #232).
- `ods state test` runs `dbt source freshness` first, as the documentation said;
  `--no-source-freshness` skips it (#288).
- CI runs the real dbt + DuckDB integration tests against dbt 1.11 and 1.12 on every
  pull request (#287, #233).
- A release and versioning policy: one version for `ods`, a version for each interface,
  this changelog, and a deprecation window (#101).
- Release builds of `ods` for Linux (x86_64 and arm64, static), macOS (Intel and Apple
  silicon) and Windows, from the first release on. Install with `pip install
  opendatasuite`, `cargo binstall` or a direct download; each archive comes
  with `SHA256SUMS`, a build provenance attestation, the licence and third-party licence
  notices. See the Install page of the documentation (#212).

### Changed
- `ods lineage impact --column MODEL.COLUMN=removed` accepts a column that is already
  gone when some model still reads it, and reports what reads it (#323).
- A failed Python model's error summary is its exception (`Python model failed:
  KeyError: [value removed]`) and the line it was raised at, instead of dbt's
  `Python model failed:` alone (#323).
- `ods state run`, `build` and `test` show dbt's output from its structured log, as
  `HH:MM:SS  message` with the time in UTC, from the level dbt would show (`info`, or
  `DBT_LOG_LEVEL`, `-- --log-level` or `-- --quiet`, which only filter what is shown:
  dbt always streams at debug so node progress keeps coming), never its debug lines,
  which hold SQL and options (`--debug` shows `info` and above). A structured line that can't be read, or has no level, shows as a
  placeholder rather than its text. Pass `-- --log-format text` for dbt's own output,
  without live stats (#322).
- `--vars` values no longer appear in what `ods state` prints, logs or reports: the
  command lines logged with `-v`, the report's `dbt` and `ran` lines, `execution.command`
  and the `dbt` settings in `--json`, and the "compiled with vars" warning show them as
  `[value removed]`. dbt still receives them (Part of #321, #322).
- The release plan: the first public release is 0.0.1 (the State MVP and the first
  dashboard screens), and 0.1.0 ships once every dashboard screen is built. `ods state`
  help and "not implemented yet" hints now name "M1 State MVP (v0.0.1)" (#309).
- The last-run file beside the state database (`<state-db>.last-run.json`) is now
  format 1.1: it also keeps which nodes failed or were skipped, and which sources'
  tests failed. Files written at 1.0 still read; an older ODS refuses a 1.1 file, as
  written by a newer ODS (#292).
- State snapshots are now schema version 1.2: they record each source's last passing
  tests against its data version. Snapshots written at 1.0 and 1.1 still read (#288).
- `ods state test` names failed checks by test name rather than by their hash (#288).
- `Timestamp` values are parsed with `jiff`. Impossible dates such as `2026-02-30` are
  now rejected (#283).
- Dependency cycles are reported as the cycle itself, e.g. `a → b → a`, instead of every
  node that couldn't be ordered (#281).
- The lineage explorer lays out graphs with dagre, which shortens edges and reduces
  crossings (#282).
- The offline explorer (`ods lineage view`, and `--site`) has the dashboard's look:
  nodes coloured by kind, the same toolbar, legend and side panel. It still works
  offline from one file, with the graph only: the State overlay and impact need
  `ods serve` (#312).
- The terminal renderer is `rs-rich` 0.0.9, built without its syntax-highlighting and
  Markdown support, which ODS doesn't use. The `ods` binary is about 140 KB smaller
  (stripped, Linux x86_64), and the dependency tree no longer carries `syntect`,
  `pulldown-cmark`, a second `fancy-regex` or the unmaintained `bincode` 1.x. Output is
  unchanged (ADR-0003).

### Fixed
- Lineage diagnostics no longer quote the SQL they couldn't analyze. The SQL is dbt's
  compiled code, which can hold values resolved from `env_var()`, `var()` or macros,
  credentials included, and the diagnostics reach `ods lineage` output, the offline
  explorer (`ods lineage view`), and `ods serve`'s `/api/graph`, `/api/node`, Lineage
  page and Home. A diagnostic now names the construct (e.g. "unsupported FROM source:
  a `TableFunction`") and, for SQL that doesn't parse, only where it failed (#312).
- `ods config explain` no longer shows a one-element array such as `["env:X"]` as the
  secret reference `secret(env:X)`. Only the table form `{ secret = "<scheme>:<name>" }`
  is a secret reference, as ADR-0005 says; any other value, in configuration or
  anywhere else ODS reads a reference, is not taken for one. A credential given as an
  array is still refused as a plaintext credential, without its value.
- `ods state run` and `ods state build` no longer say that ODS doesn't check the
  warehouse when they reuse nodes: they do check, and say reuse is taken on trust only
  when the check didn't run. `ods state plan`, which doesn't check, now points to
  `ods state build --dry-run`, which does (#301).
- From dbt 1.11, every dbt setting can also be spelled `DBT_ENGINE_<name>`, and dbt
  prefers that spelling. ODS now treats those names as the setting they spell, so
  e.g. `DBT_ENGINE_DEFER` and `DBT_ENGINE_SAMPLE` no longer get past its checks. The
  `DBT_ENGINE_` spelling of a variable ODS reads as a default for its own options
  (`DBT_TARGET`, `DBT_PROFILE`, …) is refused with a message (#287).
- The settings dbt 1.11 and 1.12 added are classified (#287).
- `ods state` could fail on Windows to set a damaged state database aside, because a
  database connection was still open after the store closed (#279).
- A lag tolerance longer than the last representable date is now reported as
  "never due", not as a date in the year 9999 (#283).
- The lineage graph now links a node to every parent it declares, not only to those
  its SQL reads, so a Python model is no longer drawn apart from what it reads. Impact
  already counted those parents. This adds edges to `ods lineage graph` in every format
  and to the explorers, and `--focus` with `--upstream`/`--downstream` can now keep
  more nodes. Each JSON `node_edges` entry (and `/api/graph`) gains `via`: `sql`, or
  `declared` when only declared. A declared-only edge is dashed in DOT (`dot` and
  `dot-columns`, where it runs node to node since no column edge covers it) and
  dotted in Mermaid; GraphML gives it an edge of kind `declared` (#312).
