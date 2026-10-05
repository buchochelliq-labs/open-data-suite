# ADR-0029: Build timings and the run ledger: what reuse saved

- **Status:** Accepted (2026-10-05)
- **Date:** 2026-10-05
- **Issues:** #210 (`ods state savings`)
- **Deciders:** @n1ckyb

## Context
Every `ods state run` knows what it reused, but nothing says what that saved. #210 asks
for the number that drives adoption: per run and in total, the nodes reused and built,
the build time avoided, and optionally its cost. That needs two facts ODS doesn't keep:
- **How long each node's build took.** The run's events have it (ADR-0024: dbt's
  `execution_time`, else finish minus start), but the journal that holds them is pruned
  to the 50 newest runs, and the snapshot records no time.
- **What each run reused.** The plan says, but it isn't kept. A run with nothing to
  build writes no journal and commits no snapshot, and those runs save the most.

Constraints:
- **Conservative** (rule 3): a node without a timing is counted, never guessed; every
  figure is an estimate and says so, with the runs whose timings it used.
- **Canonical state** (rule 5): whatever records runs is evidence, never read by the
  planner, and never moves a scope's head.
- **No vendor logic in core** (rule 1): a cost is a rate and a free label (`USD`,
  `credits`), not a warehouse's price list.
- **History survives** (ADR-0018): older snapshots and databases stay readable.

## Decision
### 1. A build's time, on its state
`NodeState` gains `build_ms: Option<u64>`: how long the build that state records took,
from the run's per-node stats (`ods state run`, `build`, `seed`, `snapshot`, `retry`)
or `run_results.json`'s `execution_time` (`ods state record`). It is set when a node
advances and carried unchanged while the node is reused, so the last measured time of
every build stays with it. A rebuild without a timing gets `None`: it never inherits the
old build's. State schema 1.3; the field is optional, so 1.0–1.2 documents read as
untimed. As with 1.1 and 1.2, an older ODS refuses 1.3 documents.

### 2. The estimate
For a run, **build time avoided** is the sum of `build_ms` of each node the plan reused
that the command would otherwise have built (its kinds of node, or, for `retry`, what
failed: reusing a model saves `ods state seed` nothing),
from the snapshot the plan read: the serial build time, an estimate that overstates
wall-clock time when dbt builds in parallel, and labelled so. Tests aren't counted.
Reused nodes without a timing are counted (`untimed`), and the total is then a lower
bound. The estimate names the runs whose timings it used.

`ods state run` (and `build`, `seed`, `snapshot`, `retry`) ends with one line, e.g.
`saved ~4m12s of build time (estimate: 9 of 13 nodes reused)`, and `savings` in JSON.
This needs nothing new beyond §1.

### 3. The run ledger
A `runs` table in the state store, behind the `StateStore` contract (0.3):
`record_run(scope, entry)` and `runs(scope, since, limit)`, newest first. An entry
(`RunEntry`, versioned) holds the run's id, scope, times, outcome (`nothing_to_build`,
`succeeded`, `failed`, `not_recorded`), the snapshot it read and the one it committed,
and per node its action (built, reused, failed, skipped) with, for a reused node, the
timing and the run it came from, frozen when written. Every non-dry run writes one, a
run with nothing to build included (with an ODS run id, as it has no executor's). It is
written after the commit attempt, in its own transaction: if it can't be, the run warns
and carries on; a failed run's entry never touches state. The methods default to
`Unsupported` (capability `run_ledger`), so a store without a ledger keeps working
and savings say it keeps no run history. Pruning is left for later.

Options rejected for the ledger:
- **A field on the snapshot:** runs that commit nothing would have to commit, moving the
  head and copying every node's state.
- **A journal event:** no journal when nothing runs, and journals are pruned.

### 4. Reporting
`ods state savings [--since DATE] [--limit N]` (with the State commands' target and
environment options) reports per run and in total
from the ledger, plain and JSON, and the dashboard's Runs page shows the same. A cost
is shown only when `[state.cost]` sets `rate_per_hour` and `unit` in `ods.toml`: a
number and a label, no secrets (ADR-0005).

### Delivery
1. §1 and §2 (timings and the one-line summary).
2. §3 (the ledger: contract, fake, conformance, SQLite migration 2) and §4's
   `ods state savings`.
3. The rest of §4 (`[state.cost]`, the dashboard).

## Consequences
- Snapshots grow by one number per node.
- Savings for runs before §3 ships can't be known; the docs say so.
- The serial estimate overstates wall-clock savings under parallelism; showing it as
  "build time" rather than "time" keeps it honest.
- The ledger grows by one row per run until pruning exists.

## References
- #210; [ADR-0013](0013-state-snapshots-fingerprints-and-store.md),
  [ADR-0018](0018-state-store-migrations-and-recovery.md),
  [ADR-0024](0024-run-events-node-stats-and-run-journal.md),
  [ADR-0005](0005-configuration-and-profiles.md)
