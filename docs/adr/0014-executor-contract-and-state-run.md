# ADR-0014: The Executor contract and `ods state run`

- **Status:** Proposed
- **Date:** 2026-09-25
- **Issues:** #23 (dbt execution provider), #24 (`ods state run`)
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
  never reported as nodes.
- Every executor must pass `ods_sdk::conformance::executor` (7 cases).
  `ods-provider-fake` has the in-memory reference implementation.

**dbt provider** (`ods_provider_dbt::executor::DbtExecutor`):
- It runs the dbt CLI as `dbt build --select <name>…` with the names ODS plans by
  (`orders`, `orders.v2`). `run` mode adds
  `--exclude-resource-type test --exclude-resource-type unit_test` (dbt 1.8+).
- `--target-path` is always passed as an absolute path. dbt would otherwise resolve it
  against `--project-dir`, and ODS would read a different directory.
- Outcomes come from the `run_results.json` of *this* invocation. The invocation id must
  differ from the file's id before the run. Otherwise the run is an error, however dbt
  exited.
- `prepare` runs `source freshness` *before* `compile`. Otherwise freshness would
  overwrite the compiled manifest.
- dbt's output goes to stderr (or is captured and quoted on failure), never to stdout,
  which carries ODS's report.

**`ods state run`**:
1. **Prepare**, unless `--no-compile`. If a source measurement was attempted and
   failed, any `sources.json` left behind is ignored, so nodes reading sources build.
2. **Plan**, exactly as `ods state plan`, against the head snapshot read now.
3. **Execute** the BUILD set. This is skipped by `--dry-run`, or when nothing needs
   building; no state changes then.
4. **Record** from the artifacts the run wrote:
   - Their manifest describes the code that was built. It must come from the executor's
     `run_id` invocation, or nothing is recorded.
   - Successes advance. Failed and skipped nodes keep their last successful state.
   - `unrequested` nodes aren't recorded, so they build again next time.
   - If nothing succeeded, nothing is committed.
   - The commit is a compare-and-swap on the head read in step 2. If another run
     recorded state meanwhile, this run's results are dropped (`ODS-E0402`), not merged.

A run that didn't fully succeed exits 1 with `ODS-E0404`, after recording its successes.
In JSON mode its envelope carries both the result and the error diagnostic: the report
is what the caller needs to see which nodes failed.

## Consequences
- Positive: `ods state run` is the M1 flow end to end. The flow is tested without dbt
  (fake executor, fake dbt script) and against real dbt with DuckDB (`ODS_TEST_DBT`).
  Another engine plugs in by implementing `Executor`.
- Negative / trade-offs:
  - `prepare` compiles the whole project on every run. That is correct, because
    fingerprints need compiled SQL, but slower than compiling only the selected nodes.
  - Selection by name can match more than one node when a folder or package has the
    same name. Such nodes are reported as `unrequested`, not hidden, but they still ran.
  - Very large BUILD sets make a long command line.
  - The JSON envelope can now carry a result *and* an error. This extends ADR-0004 §4
    for commands whose partial result matters.
- Follow-up issues:
  - Exact selection that can't widen (e.g. by `fqn:` or a generated selector file).
  - Per-node leases (#28) so concurrent runs don't both build a node.
  - Checking that reused relations still exist in the warehouse.

## References
- #23, #24; [ADR-0006](0006-plugin-sdk-and-capabilities.md) (contracts, conformance);
  [ADR-0013](0013-state-snapshots-fingerprints-and-store.md) (state model).
- dbt's public `run_results.json` and `sources.json` schemas (run results v4–v6,
  sources v2–v3), and `dbt build --help` for the selection flags.
