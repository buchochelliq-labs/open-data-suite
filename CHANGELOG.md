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

## [0.0.2] - 2026-10-06

The second release: dashboard screens backed by real signals, configurable health
checks with a CI gate, what reuse saves, and failures explained node by node. **It
migrates the state store and changes the dashboard's JSON APIs:** see **Breaking**.

- **Health:** each node gets a health badge (*failing*, *warning*, *healthy* or
  *unknown*) from checks `[health]` configures, with the findings and evidence behind
  it; `ods health check` gates CI on them, and the `health_check` SDK contract lets
  checks be added. Home shows coverage of tests, descriptions, constraints and source
  freshness.
- **New dashboard screens:** Freshness evidence, the ERD page and the Impact simulator.
- **What reuse saved:** every run says so, and `ods state savings` totals it, as time
  and, with `[state.cost]`, as cost, from a run ledger in the state store.
- **Why it failed:** `ods state explain-failure` explains one node's failure, and
  Databricks failures are checked against messages recorded from a real warehouse.
- **Install:** Homebrew and Chocolatey packages, beside PyPI and the release archives.

### Breaking
- The dashboard's JSON view models (`/api/home`, `/api/catalog`, `/api/shell` and the
  other page APIs) are now at `schema_version` 3 (#354): a Catalog row's `health` is an
  object (`health`, `reasons`) instead of `null`, Home's `coverage` rows replace
  `placeholder` with `how` and `uncovered`, and its count rows gain `of`, `how`, `href`
  and `note`. **What to do:** a script reading these APIs reads the new fields; the
  live stream (`/api/runs/live`, `live_schema_version`) is unchanged.
- The SQLite state database migrates to version 2 the first time an `ods state`
  command that opens it for use (`run`, `build`, `plan`, `history`, `test`, `export`, …)
  runs (a new `runs` table, the run ledger, ADR-0029); a copy of version 1 is kept beside
  it. `ods state savings` and `ods state doctor` read it without migrating. **What to
  do:** nothing, unless you go back to an older `ods`, or the database is read-only: an
  older `ods` refuses the migrated database, so restore the copy (`ods state doctor`
  lists it); a read-only one must be migrated where it is writable first.
- `SDK_VERSION` is now 0.8 (0.0.1 shipped 0.6). The `state_store` contract is 0.3, with
  `record_run` and `runs` (the run ledger) behind the new capability `run_ledger`; both
  default to `Unsupported` (#210). The `health_check` contract 0.1 is new (#392, under
  **Added**). **What to do:** rebuild out-of-process plugins against SDK 0.8; a store
  that keeps no ledger needs no other change.

### Added
- `ods health check` runs the health checks `[health]` configures and gates CI on them:
  exit 5 when a check at severity `error` fails, and with `--strict` also when one
  couldn't decide; `--json` for one document. Each run is kept beside the state store as
  a versioned health record (`<state db>.health/`, the newest 20), unless `--no-record`
  ([docs](docs/cli.md#ods-health-check)) (#392)
- The `health_check` SDK contract 0.1 (capability `health_check`), with a fake and a
  conformance suite: checks registered through it run beside the built-ins, under a
  timeout, and one that errs, times out, skips a node or answers about one it wasn't
  asked about is *unknown*, never a pass. Findings carry machine-readable `evidence`
  (sorted keys and values) beside their reason, in the dashboard's `findings` too.
  `SDK_VERSION` is now 0.8 (#392)
- `[health]` in `ods.toml` tunes the dashboard's health checks
  ([ADR-0030](docs/adr/0030-configurable-and-pluggable-health-checks.md)): turn each
  built-in check (`built`, `last_run_failed`, `last_run_skipped`, `tests_required`,
  `tests_passed`) off or set its severity (`error`, `warn` or `info`), scope it with
  `select`/`exclude` by resource type, tag, path glob or name, and choose whether a check
  that couldn't decide counts as unknown or a warning. Each badge now lists its
  `findings`, one per check ([docs](docs/cli.md#health-settings-in-odstoml)) (#394, #392)
- Health and coverage on the dashboard from real signals, replacing the `[n]`
  placeholders. Each node in the Catalog has a health badge (*failing*, *warning*,
  *healthy* or *unknown*), with its reasons and a `health` filter. Home counts nodes by
  health, stale nodes and runs with failures, and shows coverage of tests, descriptions,
  column constraints and source freshness, each listing what it misses. What ODS can't
  measure reads *not measured*, never 0
  ([docs](docs/cli.md#the-catalog-and-model-pages)) (#391, #354)
- The dashboard's **Freshness evidence** screen (`/catalog/sources`, under Catalog):
  for each source and seed, the evidence ODS has about its data now, its grade
  (*exact*, *semantic*, *proxy*, *inferred*, *unknown*), what its readers were last
  built from, their decisions with a link to **Why**, and every node downstream. Unknown evidence always means build. Its numbers are the plan's, as
  `ods state explain` gives them; `/api/catalog/sources` returns the same view
  ([docs](docs/cli.md#the-catalog-and-model-pages)) (#386, #350)
- In a terminal that follows hyperlinks, the run's journal path (`ods state run`,
  `build`, `seed`, `snapshot`, `test`, `retry`, `ods state history --run`) is a link to
  the file. ODS only links paths and pages it builds itself; set `FORCE_HYPERLINK=0` to
  turn links off, or `1` to force them (#379)
- Conformance suites for the `sql_lineage` and `observed_lineage` contracts, so every
  contract in `ods-sdk` now has one; the SQL parser (in every dialect), the Unity
  Catalog lineage export reader and the fakes run them. A new guide, Writing a provider,
  shows how a provider built outside this repository runs the suites
  ([docs](docs/plugins.md)) (#373, #99)
- What reuse saved, as a cost: `[state.cost] rate_per_hour` and `unit` in `ods.toml`
  make `ods state savings` add the cost avoided at that rate, per run and in total (an
  estimate, like the time). The dashboard's Runs page shows what reuse saved, from the
  run ledger (`savings` in `/api/state/runs`) ([docs](docs/cli.md#what-reuse-saved))
  (#371, #210).
- `ods state savings [--since DATE] [--limit N]`: what reuse saved, per run and in
  total, as estimates from each build's last measured time. It reads a new run ledger in the state
  database, to which every run that goes ahead adds an entry, a run with nothing to build
  included ([docs](docs/cli.md#what-reuse-saved),
  [ADR-0029](docs/adr/0029-build-timings-and-the-run-ledger.md)) (#370, #210).
- `ods state explain-failure <node> [--run <id>]`: why one model, seed, snapshot or test
  failed in the last run (or any run whose journal is kept), with the evidence and what
  to try, as `ods state history --run` explains it. A node that didn't fail says how it
  ended, and a skipped one what blocked it. `ods mcp` gains the matching
  `ods_explain_failure` tool and a `--state-db` option
  ([docs](docs/cli.md#ods-state-explain-failure)) (#368, #348).
- `ods state run`, `build`, `seed`, `snapshot` and `retry` say what reuse saved:
  `saved: ~2.3s of build time (estimate, serial: 9 of 13 nodes reused)`, and `savings`
  in JSON (also in `ods state plan`, for what a run would save). Each build's time is
  recorded with its state and kept while it is reused; reused nodes without a time are
  counted, not guessed ([docs](docs/cli.md#what-reuse-saved),
  [ADR-0029](docs/adr/0029-build-timings-and-the-run-ledger.md)) (#369, #210).
- The ERD page on the dashboard (`/erd`): the project's entities, keys and relationships
  as `ods erd generate --infer` finds them. Each edge is drawn by its evidence (tested,
  declared, joined in SQL, or inferred), with cardinality where keys prove it. Click an
  edge for its evidence, scope it with `--select` and a depth, hide inferred edges or
  non-key columns, and export SVG. Each untested relationship comes with the
  `relationships` test to paste. `/api/erd` returns the same as JSON
  ([dashboard](docs/cli.md#the-erd-page)) (#366, #64).
- The Impact simulator on the dashboard (`/lineage/impact`): propose a rename, type
  change or drop of one or more columns and see which models must run (the same set as
  `ods lineage impact`), which would break because their SQL names a column that goes
  away, which only lose it through `select *`, and which can't be told because their
  lineage is unknown; plus what can be skipped, the column trail, the `ods state build -s …`
  command for exactly what must run, and the tests that run. Opened from a column's *Simulate* link on its Model page or the
  explorer's *Impact* tab; `/api/lineage/impact` returns the same as JSON
  ([dashboard](docs/cli.md#the-impact-simulator)) (#365, #347).
- Homebrew: `brew install buchochelliq-labs/tap/ods` installs the release binary on
  macOS and Linux ([Install](docs/install.md)) (#362, #212).
- Chocolatey: each release is packaged as `opendatasuite` and pushed to the Chocolatey
  community repository, where it is listed once Chocolatey approves it
  ([Install](docs/install.md)) (#363, #212).

### Changed
- `ods state plan` and `ods state build --dry-run` list what is built first, then what
  is reused, each in plan order, as separate sections of the table, with a footer of
  totals in the terminal. Run results tables end with a totals row too. Plain output
  keeps its table shape; only the row order of plans changes (#377)
- Each failure under **Why it failed** is framed in the terminal, titled with what failed
  (`customer_segments failed`, `test not_null on customers.customer_id failed`). In plain
  output that title replaces the `failed:`/`failed test:` line (#378)
- Failure explanations on Databricks are checked against messages recorded from a real
  dbt-databricks run on a SQL warehouse (error catalogue version 6). A model contract's
  `not_null` or `check` constraint that the model's rows break is now recognised as a
  constraint violation, and a Spark error's position (`line 31 pos 4`) gives the failing
  line. A table in a missing schema is explained as a missing table, as Databricks
  reports it ([reference](docs/reference/error-patterns.md)) (#383, #349)

### Fixed
- A carriage return in a value shown in the terminal (a node name, an engine's message or
  output line) is now shown as a line break: on its own it moved the cursor back, so a
  value could overwrite what was printed before it (#375, #192)
- Error summaries no longer keep a quoted value written straight after a word: a prefixed
  literal (`X'…'`, `r'…'`, `E'…'`) or a value glued to a word (`v'…'`) can't be told from
  an apostrophe, so the rest of the line is removed from there. Property tests over `ods-core`'s graph order, fingerprints,
  timestamps, strategy choice and redaction found it (#374, #192)

## [0.0.1] - 2026-10-04

The first public release: the State MVP and the first dashboard screens.

- **`ods state`** decides what a dbt project needs to build, and why, from its code
  *and* its data: a model whose code and upstream data haven't changed since its last
  successful build is reused, with each decision explained (`plan`, `explain`,
  `why-build`, `why-skip`, `diff`, `history`). It runs dbt on exactly that selection
  (`run`, `build`, `seed`, `snapshot`, `test`, `retry`), keeps state per target in a
  local SQLite store, and never lets a failed run replace the last good state. Upstream
  data is read from dbt's source freshness and, on Databricks, Delta table versions,
  with per-node staleness tolerance and dbt's own `state:` and `build_after` settings.
- **Runs are recorded and explained**: each node's time, rows and outcome, a journal of
  every run, and failed nodes and tests explained in plain language with ODS's own
  evidence.
- **`ods serve`**, a read-only dashboard: Home, the Catalog and Model pages, the Plan
  and Why, Runs and Run pages, and the Lineage page with the State overlay, a live view
  of a run and run playback.
- **Column-level lineage** (`ods lineage`), **ERDs** from tests and constraints (`ods
  erd generate`), **`ods mcp`** for AI agents, and **`ods doctor`**.
- Binaries for Linux, macOS and Windows: `pip install opendatasuite`, `cargo binstall`,
  or a download with checksums and provenance ([Install](docs/install.md)).

Everything is a preview: while versions are 0.0.x, any release may break. The entries
below record the changes since the changelog was introduced, before this first release;
**Breaking** ones only affect builds from source made before it.

### Breaking
- An unreachable warehouse, cluster or server is explained in a new category,
  `connection`, no longer `timeout` ("timeout or lock") (#323). Explanations are
  `schema_version` 1.3, which also adds the symptoms `missing_schema` and
  `missing_function`. **What to do:** where you match an explanation's `category` (in
  `--output json`'s `failures` or the dashboard's API), expect `connection` for
  `symptom: warehouse_unavailable`, and accept the new symptoms.
- `SDK_VERSION` is now 0.6 (#323): the `error_catalogue` contract is 0.4 (an
  `ErrorSummary` may carry `outer_kind`, the kind the tool around the engine gave, which
  patterns may match on; the symptoms `missing_schema` and `missing_function`).
  **What to do:** rebuild out-of-process plugins against it; an executor that wraps the
  engine's message in its own header can keep that header's kind with
  `ErrorSummary::with_outer_kind`.
- `SDK_VERSION` is now 0.5 (#323): the `error_catalogue` contract is 0.3 (a
  `ProjectIndex` node may say other nodes refer to it by name,
  `IndexedNode::referable`). **What to do:** rebuild out-of-process plugins against it;
  a catalogue that indexes a project marks the nodes a reference can name (for dbt:
  models, seeds and snapshots) with `IndexedNode::referable()`, or leaves them unmarked
  and gets no did-you-mean for missing references.
- `--output json` of `ods state run`, `seed`, `snapshot`, `build`, `test` and `retry`
  lists tests by their handle, `check-<12 hex digits>`, instead of dbt's id for them,
  which holds a generic test's arguments (an `accepted_values` test's id names the
  values it accepts) (#323): `execution.checks_failed`, and each node's and source's
  `checks_failed`, `checks_skipped` and `checks_passed`, are still arrays of strings.
  `ods serve`'s live stream (`/api/runs/<run_id>/events`, and with `?since=`) sends a
  `check_finished` event's `check` as the handle too. So the `--output json` envelope
  is now `schema_version` 1.0 (was 0.1; `ods version` reports it), and the stream's
  messages, and `/api/runs/live`, are `live_schema_version` / `schema_version` 2 (was 1):
  every stream message's data now carries `live_schema_version`. The journal on disk
  (`<state-db>.runs/<run_id>.jsonl`) keeps dbt's id. **What to do:** accept output
  schema 1.x and live schema 2; match a failed
  test to its explanation by the handle (`failures[].node`), whose `check` says what it
  tests (`test`, `column`, `covers`); to find a test from its handle, compute it from
  the manifest's ids: `check-` and the first 12 hex digits of the SHA-256 of the id.
- `ods state retry` needs `--vars` and the arguments after `--` given again when the
  last run had them (#321). **What to do:** pass the same `--vars` and `-- …` that the
  run had, e.g. `ods state retry --vars '{…}' -- --threads 8`; without them, or with
  ones the run didn't have, `retry` exits 2 (`ODS-E0403`) and says what to add. The
  last-run file (`<state-db>.last-run.json`, now format 1.3) no longer stores their
  values, which may hold secrets, or a digest of them: only that they were given
  (`withheld`). A file written by an older ODS is read without those values, and
  rewritten without them the first time `retry` reads it (a dry run too); one with an
  option this ODS doesn't know is removed. To clear an older file's secrets now, run
  `ods state retry --dry-run` once, or delete the file.
- In `ods state test --output json`, a model whose test failed is no longer in
  `failures` as a failed model (#323): the failed test is, with its own explanation
  (`node` is the test's handle, `check-<12 hex digits>`, and `check` says what it tests).
  A model that failed with an error of its own is still listed as before. **What to
  do:** read failed tests from the entries with a `check` (`check.covers` names the
  models they test) instead of looking for the model's entry.
- `SDK_VERSION` is now 0.4 (#323): the executor contract is 0.6 (a run's
  `check_finished` event may carry `failures`, the rows a failed check found, and
  `error`, its redacted message), and the `error_catalogue` contract is 0.2 (a
  `ProjectIndex` node may say what it checks, `IndexedNode::check`, including whether it
  is a `singular` test, and a pattern may offer a step that runs what failed again with
  an engine argument, `PatternMatch::rerun`). **What to do:** rebuild out-of-process
  plugins against it; one that builds `RunEventKind::CheckFinished` sets the two new
  fields (`None` when it doesn't know).
- `SDK_VERSION` is now 0.3 (#323): the executor contract is 0.5 (an `ErrorSummary` may
  carry the `line` the engine reported), the SQL lineage analyzer contract is 0.2 (an
  opaque `QueryLineage` names the columns it couldn't resolve, `unresolved`), and there
  are two new contracts, `error_catalogue` (0.1) and `relation_linker` (0.1, #329).
  Out-of-process plugins must be rebuilt against it.
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
- The docs list every dbt error pattern ODS recognises, with the symptom each means,
  the text it matches, where that text comes from, and whether it was recorded from a
  real dbt run ([dbt error patterns](docs/reference/error-patterns.md)); a test keeps
  the page in step with the code (#357).
- Run playback ([ADR-0026](docs/adr/0026-run-playback.md)): `ods serve` replays any run
  whose journal it keeps on the Lineage page (`/lineage?replay=<run_id>`, *Replay on
  the DAG* on the Run page, or *Replay* when a live run finishes). Play, pause, step
  event by event, change speed (0.25× to 64×) or drag the play bar to any moment, as on
  a video, with the keys a video player has; a band over the bar shows how many nodes
  ran at each moment, with markers for failures. `&t=<seconds>` links to a moment.
- A failure from a `ref()` to a model that doesn't exist suggests the project's models,
  seeds and snapshots with a close name, at most three, closest first ("Did you mean
  `customers`?"), as a guess that doesn't raise the confidence; the name the `ref()`
  used is never shown (#323). The dbt error catalogue is version 3.
- A missing schema and a missing SQL function are recognised as such (#323), from
  DuckDB, PostgreSQL and Spark (`SCHEMA_NOT_FOUND`, `UNRESOLVED_ROUTINE`), with their own
  steps, and never confirmed by what confirms a missing table or an undefined macro.
  dbt's own error kind (`Database Error`) is kept in the run journal beside the
  message's (`outer_kind`, journal format 1.2), so an error no pattern knows is
  categorised by it rather than as `unknown`. The dbt error catalogue is version 5.
- More dbt failures on Databricks are explained (#323): a cluster that can't be
  started, or a connection that can't be made, reads as *warehouse unavailable* with
  `dbt debug` to try; a command or Python model run that timed out as a *query
  timeout*; OAuth or client credentials missing from the profile as *credentials
  missing*; Spark's and Delta's `CHECK` and `NOT NULL` constraint violations too. The dbt error catalogue is
  version 4.
- A failure of dbt's profile, target or credentials is explained with `ods doctor`'s
  local configuration checks (`config.load`, `config.values` for credentials,
  `config.resolution`), as evidence marked `[ods doctor]` (`source: doctor`, with
  `data.kind: doctor_check`, `check` and `status`, in `--output json`). Explanations are
  `schema_version` 1.2 (#323, #181).
- `ods doctor`'s `config.resolution` warns (`ODS-W0510`) when the profiles directory
  given to dbt (`--profiles-dir`, `DBT_PROFILES_DIR` or `profiles_dir`) has no
  `profiles.yml`; an explanation of a missing profile counts that as confirming it
  (`known pattern + evidence`). Only the file's presence is checked (#181).
- A failed test is explained (#323, ADR-0025): `ods state build`, `ods state test`
  and `ods state history --run` say which test failed, on which column of which model,
  and how many rows don't pass ("The `not_null` test on `customer_id` of `customers`
  failed: 5 rows don't pass"), with what changed in the run, whether the test failed
  before, and what to try: `ods state test --select <model> -- --store-failures` to keep
  the failing rows, then `ods state test --select <model>`. The count comes from dbt
  (`failures`); when dbt didn't give one, the headline says so, never `0`. A test that
  couldn't run (e.g. a compile error in it) is explained as that error, not as failing
  rows; a test that only warned isn't explained. A test is named by its kind, column and
  model, never by its arguments (e.g. the values `accepted_values` accepts), which dbt
  puts in the test's name and id. In `ods state test`, a model untested because its
  test failed is explained by the test, not as a failed model (see **Breaking**).
  `--output json`'s `failures` include them, with `node` (the test's handle,
  `check-<12 hex digits>`: the same for the same test in every run, never its id) and
  `check` (`covers`, `test`, `column`), and evidence `data` of kind `failing_rows`
  (`rows`) and `test_target` (`test`, `column`, `node`); explanations are now
  `schema_version` 1.1. `ods serve` shows them under each node a failed test checks
  (`failed_tests` on each node in `/api/state/runs…`), and a failed test that checks no
  node the run shows in a "Failed tests" section of the Run page (`failed_tests` in
  `/api/state/runs/<run_id>`). The run journal is now format 1.1: a `check_finished`
  that didn't pass may carry `failures` and `error` (redacted); 1.0 journals read as
  before.
- The live run view (#322): `ods serve`'s Lineage page shows a run as it goes
  (`/lineage?live=<run_id>`), with each node's state, time and rows, the run's progress
  and events, and each node's stats card (the Run page's, with a failed node explained).
  Follow mode keeps the running nodes in view (never below 60% zoom; one focus node,
  edge chips and a minimap when they are too far apart), turns off on any pan, zoom, Fit
  or node click, and back on with *Follow run* or `F`. The node menu (right-click, the
  menu key or Shift+F10) follows a node and its downstream, its upstream, or just it
  (`&follow=<node>:down|up|self`). Home shows a *Run probably in progress* banner
  (inferred from the run's journal), and an
  unfinished run's page links to the live view.
- `ods serve` streams a run while it goes (#322, ADR-0024): `/api/runs/<run_id>/events`
  sends the run's journal as Server-Sent Events, from the start and then each event as
  `ods state run`, `build` or `test` writes it, ending with `end`; ids are journal line
  numbers, so a reconnect with `Last-Event-ID` resumes. `?since=<n>` answers the same as
  JSON lines for clients without `EventSource`, and `/api/runs/live` lists the runs that
  are probably running (inferred from a journal that changed recently and doesn't say it
  finished). Every event is redacted again as it is read; at most 16 streams are open at
  once.
- A failed node is explained (#323, ADR-0025): `ods state run`, `seed`, `snapshot`,
  `build`, `test` and `ods state history --run` end with **Why it failed**, and
  `--output json` with `failures`. Each explanation has a plain-language headline, a category (e.g.
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
  runs get them. `ods serve` shows them on the Run page's Nodes tab and in the Runs side
  panel, as board 9 of the live-run design, with Copy buttons for the commands
  (`explanation` on failed nodes in `/api/state/runs…`).
- An `error_explain` capability and `ErrorCatalogue` SDK contract (#323, ADR-0025):
  providers classify a failed node's redacted error summary into ODS's neutral
  taxonomy, with a fake and a conformance suite. The dbt provider's catalogue
  recognises dbt's own errors (an undefined macro, a ref to a missing node, packages not
  installed, a missing profile or target, Jinja syntax, a Python model's exception, a
  failed test), DuckDB's (missing column, table or view, conversion, constraint,
  dependent entries, conflicts and locks), PostgreSQL's documented messages, and Apache
  Spark's and Delta Lake's error classes as Databricks reports them.
- An **Open in warehouse** link for a model's relation. With a Databricks
  target and the workspace `host` configured (`[providers.<name>] kind =
  "databricks"`, or `DATABRICKS_HOST`), the dashboard's model page has an **Open in
  Catalog Explorer ↗** button to
  `https://<host>/explore/data/<catalog>/<schema>/<table>?o=<workspace id>`, with the
  workspace id from the provider's `workspace_id` setting or an Azure or GCP host that
  contains it (left out when neither gives it); the lineage explorer's side
  panel and a run's Nodes table have the same link. `ods lineage graph --format json`,
  `ods lineage columns --output json` and `/api/catalog/<id>` add `relation_url` and
  `relation_url_label`, or `relation_url_unavailable` with the reason there is no
  link: another warehouse, no host, or a relation that isn't fully qualified. The link
  is the relation's expected location, not proof it exists, and never carries a token.
  `/api/state/runs/<run>` adds `relation_links` by node. Plugin authors get the
  `relation_link` capability and the `relation_linker` 0.1 contract, with a fake and a
  conformance suite (ADR-0006 §7). The lineage graph document keeps `schema_version`
  1: the new fields are optional, and a single-integer version can't mark a minor
  change (#329).
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
- The terminal names a failed or skipped test by what it tests, never by dbt's id or a
  generic test's name (#323): `failed: accepted_values on orders.status` on the node's
  line of `ods state test` and on a source's line, `warning: failed checks: …` after
  `ods state build`, a singular test by its own name, and any other test as `a test on
  orders (check-…)`. The live view announces a failed test as `A test on orders failed
  (check-…)`.
- `ods serve`'s Run pages say *the node didn't build* for a failed or skipped node's
  rows (`rows_missing` in `/api/state/runs/<run_id>`), instead of *not reported by the
  adapter* or *didn't run* (#322).
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
- Two of the dbt error catalogue's PostgreSQL patterns never matched (#323): an invalid
  input (`invalid input syntax for type …:`) and a failed connection (`could not
  connect to server:`) are now recognised. The pattern for `cannot drop … because other
  objects depend on it` is removed: that message never reached the catalogue, since
  the summary removes its `drop …` as SQL.
- dbt 1.12's missing-packages error ("dbt expects 1 package(s) based on packages
  specified in packages.yml, but found only 0…") is recognised again as packages not
  installed, with `dbt deps` to try (#323). The error catalogue is now version 2, and
  its tests check every recorded message on dbt 1.10, 1.11 and 1.12.
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
