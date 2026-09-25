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

ODS needs column-level lineage that it can compute locally from public dbt artifacts,
that records joins and filters as well as selected columns, and that it can export in
an open format. When this ADR was written (September 2026), the column-lineage
features we found in other tools' public documentation were part of hosted or
commercial offerings, or needed their own binaries. See each vendor's current
documentation for what they offer today.

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
  young, and in our tests (September 2026) its output ordering wasn't deterministic and
  it took about 2.3 ms per column. Kept as a possible test oracle.
- *Depend on dbt v2 crates.* They aren't published to crates.io, and they pull in
  DataFusion and the whole dbt workspace.

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
**A query is opaque** when any of these holds:
- the SQL doesn't parse, or is a Python model;
- it has a `select *` over a relation with unknown columns;
- it uses table functions, NATURAL joins, recursive CTEs, or `*` with REPLACE/RENAME/ILIKE;
- **a column reference can't be resolved**. Niladic keywords such as `current_date` are
  the only exception;
- a window, CASE, subquery or lambda appears inside an expression shape the analyzer
  doesn't walk structurally.

Unknown-column tables are only considered after every known column in every enclosing
scope. That way an outer reference is never captured by an inner table we know nothing
about.

**Readers.** A node reads what its SQL reads **plus everything it declares** (dbt
`depends_on`, with ephemeral models replaced by their own dependencies).

**Impact is designed not to under-report:**
- **Opaque readers:** a node with no usable lineage is impacted by any change to what it
  declares. That covers nodes without SQL (seeds with upstreams, snapshots, Python
  models), opaque ones, and nodes that declare a relation their SQL never reads (e.g. a
  `-- depends_on:` hint).
- **Row changes:** a changed row input reruns every reader, and changes its rows.
- **Modified or removed columns** rerun exactly the readers that use them. Indirect uses
  attached to an output, such as a CASE condition, modify that output.
- **Added columns** reach three kinds of reader:
  - `*` readers, which gain the column;
  - `*` readers that also shape rows with that relation, where DISTINCT, UNION or
    GROUP BY may change the row set;
  - readers that use a same-named column from another relation, since an unqualified
    reference may now bind to the new column.

**Diffs.** A digest covers the expression with resolved inputs, the named windows, the
sort direction and nulls order, and GROUP BY modifiers. A moved column counts as modified
for positional consumers. Duplicate output names make the whole model a row change.
Under `--base`, a node without SQL lineage is compared by dbt's file checksum.

Pruned readers are always reported, with the changed columns they don't use (rule 4).

### 4. Fast by construction
- **Dependency waves.** Models are analyzed wave by wave, each wave in parallel with
  `rayon`.
- **Content-addressed cache.** The key is
  `sha256(analyzer version, SQL, the columns of each declared upstream)`.
  - Unchanged models are never re-analyzed.
  - Changing a model re-analyzes only that model, unless its output columns changed.
  - A result that reads relations outside its declared dependencies is not cached.
- **Measured on a synthetic 2,000-model project** (about 38k column edges, 41 waves;
  `crates/ods-cli/examples/lineage_bench.rs`, release build, 4 vCPU):

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
- **dbt v2** is read either from its `manifest.json` (still schema v12) or from the Parquet
  "dbt Information Schema" v1 (`dbt.models`/`seeds`/`snapshots`/`sources`, `dbt.edges`,
  `dbt.node_columns`, `dbt.project`), via the `parquet` crate (Apache-2.0, no Arrow).
  dbt-oss leaves `compiled_code` empty there, so compiled SQL is read from
  `target/compiled/<package>/<original_file_path>`. Warehouse column lists come from
  `node_columns` rows with `data_type_actual`. The fixture proves all three inputs (dbt
  1.10 JSON, v2 JSON, v2 Parquet) produce identical lineage.

## Consequences
- **Positive:**
  - Open CLL that records joins, filters and windows as indirect edges.
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
  - A v2 Parquet artifact reader.
  - Emit the experimental OpenLineage `LineageFacet` 1-0-0 once consumers support it.
  - Masking detection (`hash`, `count`).
  - Wire impact into the State planner (#20) and the CI planner (#84).

## References
- sqlglot `lineage.py` (MIT) and SQLMesh (Apache-2.0), for the algorithm; DataHub `sqlglot_lineage.py` (Apache-2.0), for confidence
- OpenLineage `ColumnLineageDatasetFacet` 1-2-0 and the naming spec
- dbt v2's published artifact formats (the Parquet Information Schema), from its Apache-2.0 repository

## Addendum (2026-09-25): observed lineage

Static analysis can't read Python models, and nothing checks it against reality. Catalogs
that execute queries record lineage (Unity Catalog's `system.access.column_lineage`,
Snowflake's `ACCESS_HISTORY`, `OpenLineage` events).

We add:
- **An `ObservedLineageSource` SDK contract.** It returns neutral `ObservedLineage`:
  column edges, row inputs and relation edges. It has a fake in `ods-provider-fake`.
- **`Confidence::Observed`.** It ranks below `Inferred`, because observed lineage is
  true but possibly incomplete.
- **In `ods-lineage`:**
  - `ColumnGraph::compare_observed` gives per-model agreement, precision and recall.
    Observed edges from row-shaping inputs count as agreement.
  - `ColumnGraph::with_observed` stitches observed lineage into **opaque nodes only**;
    analyzable models are never overwritten.
- **`ods-provider-databricks`**, which reads UC exports (CSV/JSON). A live system-table
  query can come later behind the same contract.

The conservative rule (rule 3) is kept:
- stitched lineage stays `opaque` for impact unless the user passes `--trust-observed`;
- relations a node declares but wasn't observed reading still make it run;
- when comparing builds (`impact --base`), changes are derived from the code
  (unstitched graph), never from what happened to run.
