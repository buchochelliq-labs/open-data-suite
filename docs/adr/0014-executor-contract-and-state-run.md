# ADR-0014: The Executor contract and `ods state run`

- **Status:** Proposed
- **Date:** 2026-09-25
- **Issues:** #23 (dbt execution provider), #24 (`ods state run`), #211 (exact selection), #220 (run/test modes)
- **Deciders:** @n1ckyb

## Context
[ADR-0013](0013-state-snapshots-fingerprints-and-store.md) gave ODS a plan (what to
build, what to reuse, why) and a way to record a dbt run the user started. The State MVP
also has to run the plan itself: build exactly the BUILD set, then commit only what
succeeded (#24). The constraints:
- **No vendor logic in core** (AGENTS.md rule 1): planning and recording can't know
  that the engine is dbt. Something else (another project format, a remote runner) must
  be able to build the nodes instead (#23: "executor replaceable via plugin contract").
- **Canonical state only replaced on success** (rule 5): a failed run must not advance a
  failed node or anything it skipped.
- **Conservative defaults** (rule 3): if ODS can't tell what ran, or which code it ran,
  it records nothing.
- dbt reads an empty `--select` as "everything", and every dbt command rewrites
  `target/manifest.json`, but only `compile`, `run` and `build` write compiled SQL into
  it.

## Options considered
### Option A: the CLI shells out to dbt directly
Pros: least code. Cons: no contract, so no fake, no conformance tests, and nothing else
can execute a plan. The CLI would grow dbt-specific logic.

### Option B: an `Executor` SDK contract, with a dbt provider (chosen)
Pros: the run flow depends only on the contract. A fake executor tests it without dbt,
and a conformance suite holds every executor to the same semantics. Cons: one more
contract to version.

### Option C: dbt's Python API (`dbtRunner`) in process
Cons: embeds Python in a Rust binary and ties ODS to one dbt major version. dbt v2
doesn't have that API. The CLI is the stable interface.

## Decision
**Contract** `executor` 0.1 (`ods_sdk::contracts::executor`):
- `prepare(PrepareRequest { measure_sources })` refreshes the project's metadata so a
  plan describes the code that would run (dbt: `dbt compile`). If asked, it first
  measures sources (dbt: `dbt source freshness`). It builds nothing. It reports whether
  sources were measured *by this call*: a results file left by an earlier invocation
  doesn't count.
- `execute(ExecutionRequest { nodes: [{id, name}], mode: build | run })` builds exactly
  the requested nodes. It returns an `ExecutionReport`:
  - a `run_id`, unique per execution;
  - start and finish times;
  - one outcome per requested node, in request order;
  - failed checks;
  - `unrequested` nodes the engine built anyway;
  - `succeeded`;
  - the command, for people.
- It refuses an empty request.
- A failed node is an `Ok` report with `failed` status. `Err` means the execution
  couldn't start, or its outcome can't be read.
- A requested node the engine didn't report on is `skipped`, never a success.
- `build` mode also runs the nodes' checks (dbt tests). `run` mode doesn't. Checks are
  never reported as nodes. A failed check is listed on each requested node it checks.
  When the executor can't tell which nodes a check covers, it lists the check on all of
  them.
- Every executor must pass `ods_sdk::conformance::executor` (7 cases).
  `ods-provider-fake` has the in-memory reference implementation.

**dbt provider** (`ods_provider_dbt::executor::DbtExecutor`):
- It runs the dbt CLI as `dbt build --select …` with **exact selectors** (#211).
  dbt has no selector for a single node id. A bare name also matches a folder or
  package of that name, and `fqn:` matches by prefix. So every selector is checked
  against the manifest with dbt's own matching rules before dbt runs:
  - a node is selected as `fqn:<its fqn>,resource_type:<type>`;
  - a folder whose nodes of a type are all requested is selected whole
    (`fqn:<prefix>,resource_type:<type>`), so a large BUILD set still makes a short
    command;
  - if a node's fqn also reaches another node (a folder named like the model), its file
    narrows it: `path:<file>,fqn:<fqn>,resource_type:<type>`;
  - a package node that can't be selected exactly is an error: nothing runs.

  The check follows dbt's `fqn:` matching:
  - a selector also matches a node's fqn without its package, so `fqn:stripe` reaches
    the root project's `models/stripe/` folder;
  - a folder or file name dbt's selector syntax would split or reinterpret (a space,
    `,`, `+`, `@`, `:`, a wildcard) is never put in a selector;
  - a node without an fqn counts as reachable by every selector.

  Tests still come in through dbt's indirect selection, as for any selection.
  `run_results.json` is still checked, and anything built unrequested is still
  reported. `run` mode adds
  `--exclude-resource-type test --exclude-resource-type unit_test` (dbt 1.8+).
- `--target-path` is always passed as an absolute path. dbt would otherwise resolve it
  against `--project-dir`, and ODS would read a different directory.
- Outcomes come from the `run_results.json` of *this* invocation. The invocation id must
  differ from the file's id before the run. Otherwise the run is an error, however dbt
  exited.
- `prepare` runs `source freshness` *before* `compile`. Otherwise freshness would
  overwrite the compiled manifest.
- dbt's output goes to stderr (or is captured and quoted on failure), never to stdout,
  which carries ODS's report. With capture, the quoted lines can reach the JSON
  envelope. dbt masks `DBT_ENV_SECRET_*` values in its output, and ODS adds nothing
  from the environment; `Debug` shows only environment variable names.

**`ods state run`**:
1. **Prepare**, unless `--no-compile`. If a source measurement was attempted and
   failed, any `sources.json` left behind is ignored, so nodes reading sources build.
   `--no-compile` measures nothing: `dbt source freshness` would rewrite the compiled
   manifest. A leftover `sources.json` could predate new data, so only an explicit
   `--sources` file is read then.
2. **Plan**, exactly as `ods state plan`, against the head snapshot read now.
3. **Execute** the BUILD set. This is skipped by `--dry-run`, or when nothing needs
   building; no state changes then.
4. **Record** from the artifacts the run wrote:
   - Their manifest describes the code that was built. It must come from the executor's
     `run_id` invocation, or nothing is recorded.
   - Nodes that built *and passed their checks* advance. Failed and skipped nodes keep
     their last successful state. So does a node that built but failed a test: it isn't
     validated, so it and its tests run again next time instead of the failure going
     unseen.
   - `unrequested` nodes aren't recorded, so they build again next time.
   - If nothing succeeded, nothing is committed.
   - The commit is a compare-and-swap on the head read in step 2. If another run
     recorded state meanwhile, this run's results are dropped (`ODS-E0402`), not merged.
   - If recording fails after dbt ran, the report still says what dbt did (outcome
     `not_recorded`), next to the error.

A run that didn't fully succeed exits 1 with `ODS-E0404`, after recording its successes.
In JSON mode its envelope carries both the result and the error diagnostic: the report
is what the caller needs to see which nodes failed.

### Run and test modes (#220), contract 0.2
- `ExecutionMode::Test` runs only the requested nodes' checks and builds nothing. A
  node fails when a check on it fails.
- Only checks that ran vouch for a node. `NodeExecution::checks_passed` lists the
  checks that ran and passed on it; `checks_skipped` lists the ones the engine
  skipped (dbt's `--fail-fast`, a test whose other parent failed) **and** the ones it
  knows cover the node but that didn't run at all (deselected, e.g. by
  `DBT_INDIRECT_SELECTION`, or missing from partial results). A node is
  `fully_checked` only when checks passed on it and none failed or was skipped: a node
  without checks, or a run in which none ran, is never tested. For dbt, "the checks
  that cover a node" are read from the manifest of the same invocation; if it can't be
  read, no pass counts. In a test run such a node is `skipped`; after `--test` it is
  recorded as built but untested, so `ods state test` picks it up.
- `ods state test` records passes only when dbt succeeded or a test failed (the usual
  reason `dbt test` exits non-zero). If dbt failed with no test failing, nothing is
  marked tested. Nodes whose tests didn't all run keep what they had, and the command
  reports `incomplete` (exit 1).
- `ExecutionRequest::full_refresh` and `engine_args` pass options through. An executor
  refuses engine arguments that would change which nodes run, or where results are
  written, or would make what ran differ from what gets recorded. The dbt executor
  uses an **allowlist**, since dbt has many spellings (`--model`, `-sx`, options added
  in later versions): only options about how dbt runs and logs pass (`--threads`,
  `--log-*`, `--printer-width`, `--warn-error(-options)`, `--fail-fast`/`-x`,
  `--debug`/`-d`, `--quiet`/`-q`, colour, parsing, version check, `--store-failures`,
  and a few others). Everything else is refused, with the ODS option to use where
  there is one. It also refuses to build when the environment sets `DBT_EMPTY`,
  `DBT_SAMPLE`, `DBT_EVENT_TIME_START`/`END`, `DBT_DEFER` or `DBT_FAVOR_STATE` (or
  their deprecated spellings `DBT_DEFER_TO_STATE` and `DBT_FAVOR_STATE_MODE`), which
  would make a build something ODS can't record as the real thing.
  *Amended (#227):* every `DBT_*` setting dbt reads is classified in one table
  (`ods_provider_dbt::settings`), checked against the installed dbt's `params.py` in a
  test. The settings ODS has options for (`DBT_TARGET`, `DBT_PROFILE`,
  `DBT_PROFILES_DIR`, `DBT_PROJECT_DIR`, `DBT_TARGET_PATH`, `DBT_FULL_REFRESH`) are read
  as those options' defaults, passed as flags, and removed from dbt's environment.
  Settings a flag beats are overridden with a warning (`--no-defer`,
  `--no-favor-state`, `--no-empty`; `--write-json` and `--indirect-selection eager`
  always, which also covers `dbt_project.yml`'s `flags:`). The rest that change what
  is built or recorded are refused, including `DBT_STATE` and its relatives and the
  recorder's replay mode. Harmless settings and non-dbt variables pass through.
- `ods state run` builds **without tests by default**, like `dbt run` plus the seeds and
  snapshots the plan needs. `--test` builds and tests, like `dbt build`.
  *Amended (#229):* the build commands are named after dbt's: `ods state run` (models,
  `dbt run`), `seed`, `snapshot`, and `build` (models, seeds and snapshots with their
  tests, `dbt build`; `--exclude-resource-type test` without). `ods state compile`
  prepares and plans only. `--test` is gone. The executor runs `dbt run`, `seed` or
  `snapshot` when every requested node is of that one type, and `build` otherwise.
- `ods state test` tests what was built but not yet tested (`--all`: everything), with
  `dbt test` and exact selection, and records the results. It builds nothing.
- `--exclude` and `--resource-type` narrow the BUILD set. What they leave out keeps its
  last state, stays "to build", and is listed as `left_out`.

## Consequences
- Positive: `ods state run` is the M1 flow end to end. The flow is tested without dbt
  (fake executor, fake dbt script) and against real dbt with DuckDB (`ODS_TEST_DBT`).
  Another engine plugs in by implementing `Executor`.
- Negative / trade-offs:
  - `prepare` compiles the whole project on every run. That is correct, because
    fingerprints need compiled SQL, but slower than compiling only the selected nodes.
  - Exact selection needs the manifest in the target directory (after `prepare`, or
    with `--no-compile`), and it relies on dbt's `fqn`, `path` and `resource_type`
    selector semantics. ODS mirrors them, and the real-dbt test checks them.
  - A BUILD set scattered across many partly-built folders still lists one selector
    per node.
  - The check uses the manifest from `prepare`, but `dbt build` parses the project
    again. A file added in between could be reached by a folder selector. That is only
    caught afterwards: it is reported as `unrequested` and not recorded.
  - The JSON envelope can now carry a result *and* an error. This extends ADR-0004 §4
    for commands whose partial result matters.
- Follow-up issues:
  - Per-node leases (#28) so concurrent runs don't both build a node.
  - Checking that reused relations still exist in the warehouse: done in
    [ADR-0016](0016-relation-existence-before-reuse.md) (#230).

## References
- #23, #24; [ADR-0006](0006-plugin-sdk-and-capabilities.md) (contracts, conformance);
  [ADR-0013](0013-state-snapshots-fingerprints-and-store.md) (state model).
- dbt's public `run_results.json` and `sources.json` schemas (run results v4–v6,
  sources v2–v3), and `dbt build --help` for the selection flags.
