# ODS State strategy: beating dbt State in the open

Status: proposal · 2026-09-25 · Built on three research notes in [`sources/`](sources/):
[dbt State public docs](sources/dbt-state-public-docs.md) ·
[dbt's gaps](sources/dbt-gaps.md) · [landscape](sources/state-landscape.md).
Every claim about another product is cited there; this document keeps only the conclusions.

## 1. The market in one paragraph
dbt State launched on **2026-06-01**. It is a proprietary service in one US multi-tenant
region, it needs a dbt platform account, and it costs **$0.094 per table it reuses per
day**. That is roughly $5.6k per month for about 2,000 models and tests fully reused daily,
so the better it works, the more you pay. For each model it decides REUSE → CLONE → BUILD
from a hash of the parsed, rendered SQL and warehouse *last-modified timestamps*, and it
can explain the decision (`dbt state explain`).

The open alternatives are thin or require switching frameworks:
- **SQLMesh** now sits under the same parent company, Fivetran, and its project is at the
  Linux Foundation. Its column-level pruning is Tobiko Cloud only, and it doesn't skip
  work when upstream data is unchanged.
- **Orchestra's `sao-paolo`** keeps state as a JSON blob, has no explain, and has unsafe
  defaults.
- **Dagster** requires adopting its runtime.

Nobody offers an **open, self-hosted, auditable decision engine for unmodified dbt**.

## 2. Where dbt State falls short (and each is an ODS design point)
| dbt State behaviour (documented) | Risk | ODS answer |
|---|---|---|
| Closed SaaS, US-only; sends compiled SQL and table names; outage means full rebuild | Lock-in, data residency, no air-gapped use | Single binary, local SQLite or Postgres state, nothing leaves the network |
| Per-reuse fee | Savings are taxed | Free; report avoided compute instead |
| `lag_tolerance` defaults to **45 min**, and its meaning is inconsistent across dbt's own docs | Silent staleness | Two precisely defined knobs, both **default 0** (§4.4) |
| `evaluate_volatile_sql` off by default: `current_date()` models are reused after midnight | Wrong data | Volatile SQL is never reusable across its volatility window unless opted in |
| No concurrent-build detection; docs warn of "duplicate records or other data corruption" | Data corruption | Leases and fencing tokens per node; the contract already exists (ADR-0006) |
| Guesses where prod objects live without a manifest; alias logic "most likely to cause data corruption" | Data corruption | Never guess: no manifest or no resolvable relation means BUILD |
| `allow_clones` on; clones from "any environment … the freshest", including dev | Prod depends on dev objects; Databricks shallow clones break when the source is vacuumed | Cross-environment clone off by default; never clone from a less durable environment into prod |
| Timestamp freshness from `DESCRIBE DETAIL.lastModified` / `INFORMATION_SCHEMA` | OPTIMIZE and VACUUM commits look like new data (likely; to be tested); manual edits look fresh | Delta **commit versions** filtered to data-changing operations |
| Explain from local JSONL logs; storage and JSON formats unpublished; `UNKNOWN … details unavailable` | Not auditable, not automatable | A published JSON Schema for decisions and evidence, plus counterfactuals |
| Python models and custom materializations never reused; `select *` on `ref`/`source` always rebuilds | Lost savings | The same safe behaviour, plus explicit reasons; column-level lineage later narrows `select *` |

## 3. Positioning
> **dbt State's decisions — open-source and self-hosted, with exact lakehouse evidence,
> auditable reasons, safe concurrency and no per-reuse fee.**

## 4. The eight pillars
### 4.1 Open and sovereign (M1)
- One binary; state in SQLite (#25) or Postgres (#27).
- Works air-gapped, no login.
- The state schema is versioned and documented, and exportable as JSON.
- Only hashes and warehouse metadata are ever stored.

### 4.2 Evidence ledger with exactness grades (M1) — *nobody has this*
Every decision stores `evidence[]` entries:

`{ kind, subject, value, observed_at, exactness }`, where `exactness` is
`exact | semantic | proxy | inferred | none`.

| Signal | Exactness |
|---|---|
| Delta commit version | exact |
| `loaded_at` column max | semantic |
| `INFORMATION_SCHEMA` last-altered | proxy |
| Propagated through a view | inferred |

**Policy:** REUSE needs `exact` or `semantic` evidence for every upstream; `proxy` means
BUILD unless the user opts in. The ledger is rendered by `ods state explain` as text and
as JSON validated by a published JSON Schema (rule 4). This extends #21 and #20.

### 4.3 Counterfactual explanations (M1)
- `ods state why <node>` answers "would REUSE if tolerance ≥ 2h" or "rebuilds because
  `orders` moved from v41 to v42 (WRITE)".
- `ods state why --diff <run_a> <run_b>` shows which input digest changed, in the style of
  Bazel's execution-log diff. This extends #21.

### 4.4 Precise freshness semantics (M1)
dbt conflates two ideas. ODS separates them, and each is shown in `explain`:
- **`max_staleness`**: how far upstream data may be ahead of this model's last build
  before a rebuild is required. The default is 0: any new upstream data triggers a rebuild.
- **`min_interval`**: a rate limit, the minimum time between rebuilds. The default is 0,
  and the ledger shows when it deferred a rebuild.
- `require_fresh_data_from: any | all`, defaulting to `any`.
- All of these are configurable per model, group and profile (ADR-0005). This is the #19
  slice.

### 4.5 Exact lakehouse fingerprints (M1 slice of #17, full in M2)
- A data fingerprint is `(table_id, delta_version)`, advanced only by **data-changing**
  operations: WRITE, MERGE, UPDATE, DELETE, STREAMING UPDATE, CREATE/REPLACE.
- OPTIMIZE, VACUUM, ZORDER and predictive optimization are ignored. This needs testing on
  a real workspace.
- Change Data Feed `_change_type` mixes (insert-only?) are evidence for append-safe
  incremental builds (M2).
- Everything sits behind capabilities, never in core (rule 1).

### 4.6 Correct by construction (M1 and M2)
- State advances only on success (rule 5, #24).
- **Node leases:** the local SQLite store implements `LockProvider`, so two `ods state run`
  processes can't build the same incremental model or snapshot. Pull a local lease into
  #25; distributed leases stay in #28.
- A relation-exists check is on by default.
- Volatile SQL is detected from the parsed SQL and is not reusable across its window.
- No location guessing: missing manifest or unresolvable relation means BUILD.

### 4.7 Content-addressed reuse across environments (M2, #29/#30)
- Action key `= H(code_fp, relevant_config_fp, engine_fp, upstream data fingerprints)`.
  The state store indexes `key → physical relations`.
- A key hit in CI, dev or prod allows CLONE, subject to durability rules (no dev→prod by
  default) and provider clone capabilities (e.g. `replace_requires_drop`,
  `max_chain_depth`).
- This gives Bazel-style remote-cache semantics for warehouse tables, without SaaS.

### 4.8 Column-level impact, in the open (new track; see ADR for column lineage)
- Use column-level lineage to skip downstream models that don't consume a changed column.
  dbt's blog claims a narrow version of this. SQLMesh keeps it in its paid cloud.
- Classify changes as **breaking or non-breaking**, taking the most conservative category
  on conflict.
- Export in an open format (OpenLineage), so any catalog can use it.

### Beyond the pillars (M2+)
- **Warehouse refresh outcomes as evidence:** a Databricks MV `NO_OP` or Snowflake
  `NO_DATA` refresh proves unchanged output, so its downstream models can be skipped.
- **Cost annotations** from Databricks `system.billing`, marked *inferred*.
- **An explicit test-result reuse policy:** rerun a test when any parent was built.
- **OpenLineage decision and dataset-version facets:** emit them, and consume other tools'
  `DatasetVersion` facets as evidence.

## 5. Roadmap impact
| Pillar | Milestone | Issues |
|---|---|---|
| 4.1 open state | M1 | #25, #26, #11 (schema_version) |
| 4.2 evidence ledger + JSON Schema | M1 | #20, #21 (+ new: decision/evidence schema) |
| 4.3 counterfactuals | M1 | #21 |
| 4.4 freshness semantics | M1 | #19 slice |
| 4.5 Delta versions (data ops only) | M1 slice / M2 | #17 |
| 4.6 local node lease | M1 | #25 (uses the ADR-0006 `LockProvider`) |
| 4.6 volatile SQL policy | M1 | #13 (+ new) |
| 4.7 content-addressed clone | M2 | #29, #30 |
| 4.8 column-level impact | M2 (engine can start now) | #73, #74, #31 |
| IVM evidence, cost, OpenLineage facets, test reuse | M2+ | new |

**Proposed new issues** (drafts to file on request):
1. A published JSON Schema for State decisions and the evidence ledger, with exactness grades.
2. A volatile-SQL policy for non-deterministic functions.
3. A local node lease in the SQLite store.
4. Delta data-operation filtering for fingerprints, verified on a live workspace.
5. Warehouse refresh outcomes (MV `NO_OP`) as change evidence.
6. Cost annotations per decision.
7. An explicit test-result reuse policy.
8. OpenLineage facets for decisions and dataset versions.

## 6. Still to verify (needs a live Databricks workspace)
- Does OPTIMIZE or predictive optimization bump `DESCRIBE DETAIL.lastModified`? Which
  `DESCRIBE HISTORY` operations are data-changing?
- How do dbt-databricks materialized views and streaming tables expose refresh outcomes?
- Measure metadata query cost per table, and batch `DESCRIBE HISTORY` where possible.
