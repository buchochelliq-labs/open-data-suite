# ADR-0008: Open, fast column-level lineage

- **Status:** Proposed
- **Date:** 2026-09-25
- **Issues:** #74 (column lineage), #73 (SQL parser), #31 (column-aware invalidation), #92 (OpenMetadata); related #12, #75, #84, #100
- **Deciders:** @n1ckyb

(ADR-0007 stays reserved for the clean-room and licensing policy, #10.)

## Context
Column-level lineage (CLL) answers the question State and CI keep asking: **when these
columns change, which downstream models must run, and why?** Model-level lineage
over-rebuilds. A column nobody reads still triggers every consumer.

Today CLL for dbt is behind a paywall. The research is in
[`docs/research/sources/column-lineage.md`](../research/sources/column-lineage.md).
- **dbt Catalog's CLL** needs an Enterprise plan. It covers SELECT only (no joins or
  filters), and it errors on Python models.
- **dbt v2's open code** ("dbt OSS", Apache-2.0) ships only the CLL *types*. The code
  itself says the provider that computes lineage is proprietary (`dbt-index`). The
  Parquet file `dbt.column_lineage` is only written by the proprietary `dbt` binary,
  with a login and `--static-analysis strict`.
- **SQLMesh** keeps CLL-based plan pruning in its paid cloud.

We want CLL that is open, fast, conservative, and exportable to any catalog.

## Options considered
### Parser
- **`sqlparser` 0.63, Apache-2.0 (chosen).**
  - It has 16 dialects, including Databricks and Spark, plus spans and a visitor.
  - Every Databricks construct we probed parses, including QUALIFY, lambdas, `:` JSON
    paths, LATERAL VIEW, PIVOT and `* EXCEPT`.
  - It takes about 0.6 ms for a 200-line model.
  - OpenLineage's own Rust lineage library uses it.
  - It is syntax only, so we write the scope resolution ourselves.
- *`polyglot-sql`, MIT (a Rust port of sqlglot).* It has lineage built in, but it is
  young, has non-deterministic ordering, and costs about 2.3 ms per column. Kept as a
  possible test oracle.
- *Depend on dbt v2 crates.* They aren't published, they pull in DataFusion and the whole
  dbt workspace, and the lineage part isn't open anyway.

### Output format
- **Our own neutral model, exported as the OpenLineage `ColumnLineageDatasetFacet` 1-2-0
  (chosen).** OpenLineage reaches Marquez, OpenMetadata (natively, at
  `/api/v1/openlineage`), DataHub, Dataplex and Collibra with one export.
- *A catalog-specific format first.* That locks us in; native sinks come later, as
  providers.

## Decision
### 1. Layers (ADR-0001)
| Where | What |
|---|---|
| `ods-core::lineage` | Vocabulary: `RelationName`, `ColumnRef`, `EdgeKind` = `Direct(Identity\|Transformation\|Aggregation)` or `Indirect(Join\|Filter\|GroupBy\|Sort\|Window\|Conditional)`, mirroring OpenLineage; `Confidence` = `Exact`, `Inferred` or `Unknown`. |
| `ods-sdk::contracts::sql_lineage` | The `SqlLineageAnalyzer` contract. Per query it returns `QueryLineage`: outputs with inputs and an expression digest, row-shaping inputs, relations read, relations reached through `*`, a row digest, and `opaque` plus diagnostics. **Synchronous**, unlike ADR-0006's async contracts, because analysis is pure CPU work that callers parallelize. |
| `ods-lineage` (module) | Builds the `ColumnGraph`, runs the cache, diffs two versions of a model, computes impact, and exports OpenLineage. |
| `ods-provider-sqlparser` | The analyzer: dialects, identifier normalization, scope resolution. |
| `ods-provider-dbt` | Lean readers for manifest v11/v12 and catalog v1 (#12). |
| `ods-provider-fake` | `FakeSqlLineageAnalyzer`, which returns scripted results. |
| `ods-cli` | `ods lineage columns\|impact\|export`, the composition root. |

### 2. Analysis algorithm (sqlglot-style, plus indirect edges)
Per model, in dependency order:
1. Parse the **compiled** SQL. Try the dialect first, then a more permissive relative,
   then generic.
2. Build a scope per query level. Each `FROM` source is a table, a CTE, a derived table
   or a lateral view.
3. Qualify columns against the sources and the **upstream schemas**. For models, those
   are the analyzed outputs; for sources and seeds, the catalog.
   - An ambiguous column attaches to every candidate, marked `Inferred`.
   - A correlated column resolves outward to the enclosing query.
   - A lateral alias resolves to the output it names.
4. Expand `*`, `t.*` and `* EXCEPT`.
5. Trace each output column to **physical** columns through CTEs, subqueries and set
   operations. UNION branches match by position.
6. Record **indirect** inputs:
   - joins, WHERE/HAVING/QUALIFY, GROUP BY (including `ALL` and positional forms),
     DISTINCT and set-operation deduplication, and ORDER BY with a LIMIT, as row inputs;
   - CASE conditions, window partitioning and ordering, and aggregate FILTERs, as inputs
     of the specific column.
7. Compute digests over the expression with every column reference replaced by its
   resolved target. Table aliases, formatting and comments don't change a digest; a
   change inside a CTE does.

### 3. Conservative by construction (AGENTS.md rule 3)
A query is **opaque** when any of these holds:
- the SQL doesn't parse;
- it is a Python model;
- it has a `select *` over a relation with unknown columns;
- it uses table functions, NATURAL joins, recursive CTEs, or `*` with REPLACE/RENAME/ILIKE;
- it has a subquery nested where it can't be scoped.

An opaque model is impacted by **any** change to what it reads. Impact never under-reports:
- a changed row input makes every reader rerun;
- a modified or removed column reruns exactly the readers that use it;
- an added column only reaches `*` readers.

Readers that are pruned are always reported, with the changed columns they don't use
(rule 4).

### 4. Fast by construction
- **Dependency waves.** Models are analyzed wave by wave, each wave in parallel with
  `rayon`.
- **Content-addressed cache.** The key is
  `sha256(analyzer version, SQL, the columns of each declared upstream)`.
  - Unchanged models are never re-analyzed.
  - Changing a model re-analyzes only that model, unless its output columns changed.
  - A result that reads relations outside its declared dependencies is not cached.
- **Measured on a synthetic 2,000-model project** (about 38k column edges, 41 waves;
  `examples/lineage_bench.rs`, release build, 4 vCPU):

  | Operation | Time |
  |---|---|
  | Cold build | 225 ms |
  | Fully cached rebuild | 162 ms |
  | One model changed | 174 ms |
  | Impact query | 0.7 ms |

### 5. Open export and sync
- `ods lineage export` writes one OpenLineage event per model:
  - a static **JobEvent** by default;
  - a `COMPLETE` **RunEvent** with a deterministic UUIDv5-style run id, for sinks that
    only accept runs, such as OpenMetadata's native endpoint.
- Direct inputs go in `fields`. Row-shaping inputs go in the facet's `dataset` array,
  and optionally also in every field, for consumers that ignore `dataset`.
- Opaque models still export table-level lineage, but never a column facet: nothing is
  claimed that isn't known.
- The dataset namespace is configurable, e.g. `unitycatalog://{host}` per the
  OpenLineage naming spec.
- **Planned: a `LineageSink` contract.** Providers for OpenLineage HTTP (Marquez and
  OpenMetadata), a native OpenMetadata `PUT /api/v1/lineage` with `columnsLineage`, and
  DataHub `fineGrainedLineages` with confidence scores.

### 6. dbt specifics
- Only public artifacts are read: manifest v11/v12 `compiled_code`, `relation_name` and
  `depends_on`, plus catalog v1 columns. No Jinja is rendered and no warehouse is queried.
- dbt v2's Parquet artifacts are a planned second input. So is importing Fusion's
  `dbt.column_lineage` Parquet, when a user has it, as a cross-checked second source:
  its `direct`/`indirect`/`scan` kinds map onto our edge kinds, and a disagreement
  lowers confidence.

## Consequences
- **Positive:**
  - Open CLL that also covers joins, filters and windows, which dbt's paid version doesn't.
  - Useful straight away to State (#31: column-aware invalidation) and CI (#75, #84:
    selective runs with evidence).
  - Any catalog can ingest it through OpenLineage.
- **Negative / trade-offs:**
  - We own the scope resolution. That is about 1.3k lines, with tests.
  - `sqlparser` breaks its AST on every 0.N release. We pin `=0.63.0` and upgrade
    deliberately.
  - Opaque models reduce pruning, which is correct but less efficient. Coverage grows
    per construct.
  - Unresolved bare words (e.g. `current_date`) are recorded as diagnostics, not
    columns, and lower confidence to `Inferred`.
- **Follow-up work:**
  - A `LineageSink` contract, plus OpenMetadata and DataHub sinks (#92).
  - Persist the cache in the state store (#25).
  - Share cached results instead of deep-copying them (`Arc`), to cut warm-rebuild time.
  - A v2 Parquet artifact reader, and import of Fusion lineage.
  - Emit the experimental OpenLineage `LineageFacet` 1-0-0 once consumers support it.
  - Masking detection (`hash`, `count`).
  - Wire impact into the State planner (#20) and the CI planner (#84).

## References
- `docs/research/sources/column-lineage.md`, `docs/research/ods-state-strategy.md` §4.8
- sqlglot `lineage.py` (MIT) and SQLMesh (Apache-2.0), for the algorithm; DataHub `sqlglot_lineage.py` (Apache-2.0), for confidence
- OpenLineage `ColumnLineageDatasetFacet` 1-2-0 and the naming spec
- dbt v2 (`dbt-labs/dbt`, Apache-2.0): `dbt-lineage-core`, `dbt-metadata-parquet/src/cll_epoch.rs`, read for format and positioning only
