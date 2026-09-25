# Metadata index, explorer UX and differentiation

Status: proposal · 2026-09-25 · Companion to [`ods-state-strategy.md`](ods-state-strategy.md)
and ADR-0008 (column-level lineage, on the lineage branch).

## 1. What `dbt-index` is
These findings come from reading the Apache-2.0 `dbt-labs/dbt` repo (v2.0.5). The
proprietary parts were not read, only the open code that names them.

- **What the open part contains.** `dbt-index-core` is the open *interface* crate. Its
  own docs say it defines "abstract interfaces for features whose implementation lives
  in the proprietary `dbt-index` crate (column lineage, column impact, etc.)". It ships
  no implementation.
- **What it does.** `dbt-index` is a **DuckDB query engine over dbt's Parquet "Information
  Schema"**, the per-run tables `dbt` writes under `target/`. It powers:
  - single-hop column lineage and multi-hop **column impact** (a breadth-first walk over
    `dbt.column_lineage`);
  - freshness;
  - a **`Backend`** that gives untyped SQL access, which the docs site uses for "node
    listing, project info, catalog stats".
- **How it's gated.** Every open interface defaults to `UnavailableProvider`, which
  reports the feature as unavailable. Its purpose, in the code's words: "so callers can
  render a PLG upsell rather than crashing". So the open docs site ships with the smart
  parts switched off.
- **dbt Docs v2.** It is a static single-page app that reads the Parquet with
  **DuckDB-WASM in the browser**, with no server, and can be hosted anywhere. It's a good
  architecture, but lineage, impact and search go through the gated providers.
- **dbt Catalog (the platform).** It adds:
  - lineage "lenses": resource type, materialization, model layer, latest run and test
    status, query history;
  - keyword search, data health signals, recommendations and cost insights;
  - model query history and external metadata ingestion, both on Enterprise tiers only.

**Do we need something similar? Yes, but open.** ODS needs one local, queryable metadata
index that every surface reads: the CLI, the web explorer, VS Code, agents and catalog
sync. The difference from dbt is that ours is the product, not the upsell.

## 2. The ODS index (proposal)
| Decision | Choice | Why |
|---|---|---|
| Storage format | Versioned tables with a documented schema, written as **Parquet** | Columnar, compact, readable by DuckDB, Polars, Spark and pandas; the same interoperability dbt chose |
| Transactional state | **SQLite** (State, #25) | State needs transactions and leases (ADR-0006); the index is derived and rebuildable |
| SQL over the index | `ods query "<sql>"` on **DataFusion** (Rust, Apache-2.0) | Pure Rust, no C++ build; Parquet-native |
| In the browser | Embedded JSON for small projects (today's viewer). For large ones, JSON/Parquet shards plus optional DuckDB-WASM (MIT) | Offline single file where possible, scalable where needed |

**Tables** (each with `schema_version`):
- `nodes`, `columns` (with types), `node_edges`;
- `column_edges`, with kind, subtype and confidence (ADR-0008);
- `tests` and `test_results`, `runs`;
- `freshness_evidence`, with exactness grades (State pillar 4.2);
- `state_decisions`: reuse, clone or build, with reasons;
- `relationships`: the ERD (PK/FK, explicit or inferred), kept separate from lineage
  (rule 6);
- `usage`: real consumers from query history (#55);
- `owners`, `tags`, `descriptions`;
- `cost` (inferred).

The index is incremental and content-addressed, like the lineage cache. It's rebuilt from
artifacts plus the state store, so it's never the source of truth. We'll publish its
schema, as we do for the lineage graph JSON.

## 3. What dbt doesn't do, and our answer
| Gap in dbt (as of Sept 2026) | ODS answer | Status |
|---|---|---|
| Column lineage paid (Enterprise), SELECT only, fails on Python models; closed in OSS | Open column lineage with join, filter, group, window and conditional edges; confidence grades; Python models treated as opaque, never skipped | **Built** (ADR-0008) |
| Column impact closed (`dbt-index`) | `ods lineage impact`: which models must run, why, and **which were safely skipped** | **Built** |
| Lineage export needs Enterprise+ for external metadata | OpenLineage export; Mermaid, DOT and GraphML | **Built** |
| Only dbt-built SQL is visible | Unified lineage over any SQL: Databricks jobs and notebooks, BI queries (Unity Catalog query history), dbt | Next (#56, #15) |
| Impact stops at the warehouse | **Usage-aware impact**: "this column change breaks 3 dashboards and 1 ML feature" | Next (#58, #59) |
| State is a paid SaaS; explain via local log files; 45-minute staleness default | Open, local State with an evidence ledger, counterfactual `why` and safe defaults | Planned (M1) |
| No concurrency safety; guesses object locations | Leases and fencing (built), never guess | Contract built; M1/M2 |
| Warehouse-native change signals ignored (Delta versions, MV `NO_OP`) | Exact lakehouse evidence | Planned (#17, M2) |
| No breaking/non-breaking classification in CI | Column-level CI verdicts in PR comments (added, removed or modified columns; consumers affected) | Next (#75, #84, #85) |
| No ERD inference | ERD kept separate from lineage, with inferred relationships marked as inferred | Planned (M3) |
| Agent features tied to the platform | **`ods mcp`**: any agent can call search, lineage, impact and explain on the local index, with no login | Proposed |
| Docs smart features gated | Every ODS surface works offline with every feature | Principle |

## 4. Explorer UX: parity where it matters, then beyond
### Surfaces (one index, one JSON contract)
1. **`ods lineage view`** (built) grows into **`ods explore`**:
   - a static site you can host anywhere (a single file for small projects, shards for
     large ones);
   - `ods serve` (Axum, already in our stack) for live reload while developing.
2. **VS Code (#107)** reuses the same page in a webview. On top of that:
   - CodeLens on model columns ("12 downstream columns · 2 dashboards");
   - hover to see upstream inputs;
   - on save, "impact of this change" against the base build;
   - lineage requests served by the LSP (#67).
3. **CLI**: rich, plain or JSON output for every view (ADR-0003); `ods query` for SQL.
4. **Agents**: `ods mcp`, an MCP server over the index (read-only by default, rule 9 on
   secrets).
5. **Catalogs**: OpenLineage, OpenMetadata and DataHub sync for org-wide discovery (#92).

### Features, matching dbt Catalog
- **Search.** Fuzzy search over models, columns, descriptions, tags and owners, with
  facet syntax: `type:model layer:mart tag:finance column:revenue owner:@data-eng`.
- **Resource pages.** Each shows:
  - the description and each column's lineage;
  - tests and last results, freshness evidence, and state decisions over time;
  - compiled SQL, and a diff against the base build.
- **Lineage graph with lenses.** dbt's lenses (resource type, materialization, layer,
  run and test status), plus our own:
  - **lineage confidence and opaque models**;
  - **freshness evidence exactness**;
  - **State decision** (reuse, clone or build), cost, owner, and usage heat.

### Beyond dbt
- **Impact simulator.** Pick a column and choose "change", "remove" or "add a column". See
  the run set, the pruned readers with reasons, the tests that will rerun, and the
  affected dashboards.
- **Column journey.** One click traces a dashboard field back to the source column, with
  every transformation shown.
- **State timeline.** Why each model was reused or rebuilt on every run, plus "would
  REUSE if…" counterfactuals.
- **Diff mode.** Compare two builds side by side (branch against prod): changed columns
  are highlighted, and impact is overlaid.
- **Everything works offline, with deep links.** For example,
  `lineage.html#node=model.x&column=y` can be pasted into a PR or chat.

## 5. Proposed sequencing
| Step | Deliverable | Depends on |
|---|---|---|
| 1 | `ods lineage view` (done) + VS Code webview spike reusing it | #107 |
| 2 | ODS index v1: Parquet tables for nodes, columns and lineage; `ods query` | ADR (index format) |
| 3 | `ods explore` static site + `ods serve`: search, resource pages, lenses | 2 |
| 4 | `ods mcp`: search, node, lineage, impact, explain | 2 |
| 5 | Impact simulator and diff mode in the explorer | 2, 3 |
| 6 | Usage-aware impact (UC query history) and dashboards in the graph | #55, #56 |
| 7 | State decisions and evidence in the index and explorer | M1 State |

New ADRs needed: the ODS index format and query engine (DataFusion), and the explorer
architecture (static files first, optional server).
