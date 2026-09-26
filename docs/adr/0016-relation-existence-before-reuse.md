# ADR-0016: Check that a relation still exists before reusing it

- **Status:** Proposed
- **Date:** 2026-09-26
- **Issues:** #230
- **Deciders:** @n1ckyb

## Context
The planner (ADR-0013) reuses a node when its code and inputs haven't changed since its
last recorded build. Until now it didn't check that the build was still in the
warehouse. If the table or view was dropped, renamed, or never created in this target,
ODS still said "unchanged, reuse", and whatever read it failed or read nothing. Every
REUSE carried `relation_exists` evidence with exactness `none` to say so. That is a gap
in rule 3: missing evidence must mean BUILD.

Constraints:
- Rule 1: the planner and the CLI can't know which warehouse they are talking to.
- Rule 9: ODS must not hold warehouse credentials just to ask this.
- Cost: one extra dbt call per run at most, however many nodes there are. A dbt call
  costs seconds (parsing, connecting).
- Nothing may be written into the user's project. The check only writes dbt's own
  artifacts, to a directory of its own under the target path.

## Options considered
### Option A: a method on `Executor`
Simple, but only an executor could answer. A provider with its own warehouse
connection (e.g. #15, Databricks) would have to be an executor too.

### Option B: a `RelationInspector` contract with a capability (chosen)
A small read-only contract, advertised as the `relation_existence` capability.
- The CLI picks a strategy with `ods_core::choose`: a warehouse check, or the fallback
  of trusting the recorded build. The fallback keeps today's labelled reuse (exactness
  `none`), so a provider that can't check still reuses.
- The dbt executor implements it.
- A native metadata provider can implement it later without changing the planner.

### How dbt answers: `dbt show --inline` (chosen) vs `dbt run-operation`
- `run-operation` only runs a named macro from the project or its packages. ODS would
  have to write a macro into the user's project, and its answer would have to be
  scraped from log output.
- `dbt show --inline "<jinja>" --output json --log-format json --limit 1` runs a query
  ODS supplies.
  - Its Jinja loops over `graph.nodes`, taking the models, seeds and snapshots that
    aren't ephemeral, and calls `adapter.get_relation(database, schema, alias)` for
    each. That uses the adapter's own caching, quoting and case rules; on most
    adapters it is one listing per schema.
  - Its one row is JSON: a marker, how many relations it checked, and the ids of the
    missing ones.
  - The query names no nodes, so its size is fixed (under 1 KiB) and within
    command-line limits on every OS. Only missing ids come back, so the result stays
    small in large projects.

## Decision
1. **Capability and contract.**
   - `ods-core` gains the capability `relation_existence`.
   - `ods-sdk` gains the contract `relation_inspector` (0.1):
     `inspect(&[RequestedNode]) -> RelationReport`.
     - The report lists every requested node once, in order, as `present {kind}`,
       `missing` or `unknown(why)`.
     - An unresolvable node is never `present`.
     - `Err` means nothing was verified.
     - It is read-only, and answers in one batch.
   - There is a conformance suite, run by the fake and dbt providers.
2. **Planner.**
   - A node carries a `RelationFact`: `unchecked` (the default), `present`, `missing`
     or `unverified(why)`.
   - A node the rules would REUSE is instead:
     - BUILT with `relation_missing` ("its table isn't in the warehouse") when it is
       missing;
     - BUILT with `relation_unverified` ("couldn't check that its table is still in
       the warehouse: …") when the check couldn't tell.
   - Its `relation_exists` evidence is `exact` when checked, and `none` when not.
   - This is decided before its children are planned. Neither code is a code change,
     so readers see new upstream data and their lag and quorum rules apply.
   - `reuse_candidates` returns the nodes a plan would reuse without any facts. It
     covers the whole project and plans it without a full refresh (which depends on
     the selection and only adds builds), so it is a superset of what any selection
     reuses. Facts only turn REUSE into BUILD, so checking those once is enough.
   - Once relations were checked (`PlanOptions::relations_checked`), a node that would
     be reused but wasn't checked is BUILT as `relation_unverified`. Nothing is reused
     on trust by accident.
3. **CLI.** `ods state run|seed|snapshot|build|compile`, including `--dry-run` and
   `--no-compile`, check the candidates before planning.
   - There is no call when there is nothing to reuse, e.g. on a first run.
   - If the check fails, every candidate is BUILT, and a warning says why.
   - A step line announces the `dbt show`.
   - `ods state plan` stays offline and artifact-only: its REUSE keeps exactness
     `none`.
4. **dbt provider.**
   - `dbt show` writes its artifacts to `<target>/ods-relation-check/`, so the plan's
     `manifest.json` and `run_results.json` are never overwritten.
     `partial_parse.msgpack` is copied there first so parsing stays fast.
   - Which nodes were checked comes from the `manifest.json` that `dbt show` itself
     wrote, not from the plan's. dbt parses the project again, so a node it no longer
     has (e.g. renamed since the compile) is `unknown`, never `present`. A count that
     disagrees with that manifest fails the check.
   - An executor without the capability is warned about: its reuse trusts the
     recorded build.

## Consequences
- Positive:
  - A dropped table or view is rebuilt on the next run, with the reason in the plan,
    the step lines and the JSON.
  - Reuse is now backed by evidence from the warehouse, not just by the recorded build.
- Negative / trade-offs:
  - One more dbt call (a parse and a connection) whenever something could be reused,
    typically a few seconds.
  - A relation that exists but holds something else (rebuilt outside ODS) isn't
    detected; that needs relation versions (#17).
  - A custom materialization that creates no relation would be rebuilt every run. Only
    ephemeral models are exempt.
  - `dbt show --output json` needs dbt 1.5 or later.
  - A relation dropped between the check and the build shows up as a dbt failure.
- Follow-up issues: #15, which can answer natively; #17 (relation versions).

## References
- #230; [ADR-0006](0006-plugin-sdk-and-capabilities.md) (capabilities, `choose`);
  [ADR-0013](0013-state-snapshots-fingerprints-and-store.md) (planner rules);
  [ADR-0014](0014-executor-contract-and-state-run.md) (`ods state run`).
