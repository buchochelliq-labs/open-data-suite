# Research: open, fast column-level lineage (CLL) for ODS

> Research source (2026-09-25), kept for traceability; the decision is [ADR-0008](../../adr/0008-column-level-lineage.md).

Date: 2026-09-25. Scope: Rust SQL parser choice, CLL algorithm, OpenLineage format, catalog sinks,
dbt's own CLL, performance/incremental design. All sources are public specs, docs, or OSS code
(clean-room). Local clones of the public repositories cited below were used.
The benchmark was a throwaway crate; ODS's reproducible benchmark is `crates/ods-cli/examples/lineage_bench.rs`.

---

## 0. TL;DR / recommendations

1. **Parser: `sqlparser` (apache/datafusion-sqlparser-rs) 0.63.0, Apache-2.0.** It gives a typed AST,
   source spans, a visitor, 16 dialects including `DatabricksDialect` and `SparkSqlDialect`, and the Apache
   DataFusion project maintains it. OpenLineage's own Rust SQL lineage library also uses it. It parsed every
   Databricks construct we probed inside a SELECT. The ~200-line analytic model took about 0.6 ms per parse.
   Pin `=0.63.x`, because every minor release breaks the AST.
   Keep **polyglot-sql** (MIT, a Rust port of sqlglot that includes lineage and OpenLineage output) as a
   **differential test oracle** (dev-dependency or offline corpus comparison), not as the production engine.
   Reasons: it is young (v0.13, one lead maintainer), it compiles very slowly, its lineage uses `HashMap`
   (not deterministic), and it re-qualifies the query for every column.
2. **Algorithm:** implement sqlglot's approach: normalize → qualify tables → build scopes →
   qualify columns / expand `*` with an upstream schema → walk each output column's expression through the
   scopes. Add what dbt and sqlglot leave out: **INDIRECT** lineage (JOIN/FILTER/GROUP_BY/SORT/WINDOW/
   CONDITIONAL), OpenLineage transformation subtypes, and an explicit **confidence / evidence** model
   (ODS rule 3 and rule 4). Unknown means *inferred* or *unresolved*, never silently "no dependency".
3. **dbt ordering makes schemas available:** process models in DAG order and feed each model's inferred
   output schema downstream. Seed from `catalog.json` for sources and from manifest `columns` as a weaker
   fallback. This removes most "unknown schema for `select *`" cases without warehouse access.
4. **Format:** emit OpenLineage `ColumnLineageDatasetFacet` **1-2-0** (stable) on `outputs[]` of
   RunEvents/JobEvents. Include the `dataset` array for indirect lineage. Track the new, experimental
   `LineageFacet` 1-0-0 (OpenLineage 1.53.0, 2026-09-01), which is meant to eventually replace CLL.
5. **Sinks:** OpenLineage over HTTP reaches Marquez, **OpenMetadata** (native `POST /api/v1/openlineage/lineage`),
   DataHub (`/openapi/openlineage/api/v1/lineage`, but column lineage support there is partial), Google Dataplex,
   Collibra, and others. Native adapters worth writing: **OpenMetadata `PUT /api/v1/lineage`** (full
   control over `columnsLineage`, `source: DbtLineage`, SQL) and **DataHub `fineGrainedLineages`**
   (confidence score). Unity Catalog does not need an adapter for dbt-on-Databricks, because UC already
   captures runtime CLL. Its External Lineage API is only for non-UC assets.

---

## 1. Rust SQL parsers

### 1.1 `sqlparser` (datafusion-sqlparser-rs)

| Item | Finding |
|---|---|
| Repo / crate | https://github.com/apache/datafusion-sqlparser-rs · https://crates.io/crates/sqlparser |
| Licence | Apache-2.0 |
| Latest | **0.63.0, released 2026-09-13**. Previous: 0.62.0 (2026-05-07), 0.61.0 (2026-02-10), 0.60.0 (2025-12-07). Tracking issue for 0.64.0 targets about 2026-10-31 (https://github.com/apache/datafusion-sqlparser-rs/issues/2454) |
| MSRV | No `rust-version` in Cargo.toml (crate edition 2021). It builds on rustc 1.94 here. DataFusion's MSRV policy applies in practice. **ODS should run its own MSRV 1.90 CI job** |
| Dialects (src/dialect/) | ansi, bigquery, clickhouse, **databricks**, duckdb, generic, hive, mssql, mysql, oracle, postgresql, redshift, snowflake, **spark** (`SparkSqlDialect`), sqlite, teradata |
| Spans | Yes. The `Spanned` trait (`src/ast/spans.rs`) gives `Span(Location(line,col)..)` for every node, and tokens carry spans (`tokenize_with_location`). Verified: CTE `orders` spans `(1,6)..(14,2)`. Caveat: `spans.rs` still has 185 `Span::empty()` fallbacks, so some nodes return empty spans; ODS code must tolerate that |
| Visitor | `visitor` feature (`Visit`/`VisitMut`, `visit_relations`, `visit_expressions`) |
| Semantics | Syntax only. No binder, no name resolution, no types. ODS writes scope/qualification itself |
| Versioning | Every 0.N release is breaking (the changelog says "any changes to the AST will technically be breaking"). Pin `=0.63.x` and upgrade deliberately |

**Databricks probes** (0.63.0, `DatabricksDialect`, probe programs not kept
in the repo). All of these parse OK:

`QUALIFY`; lambdas `transform(xs, x -> x + 1)`; colon JSON path `raw:customer.id`, `raw:items[0].sku::string`
(parsed as `Expr::JsonAccess`), `v:['name']`; `struct(1 as a, …)`, `named_struct`, `CAST(... AS STRUCT<a:int,…>)`,
`MAP<string,int>`; backtick identifiers; `LATERAL VIEW [OUTER] explode/posexplode … AS …`; `PIVOT`/`UNPIVOT`;
`SELECT * EXCEPT (…)` and `t.* EXCEPT (…)`; `GROUP BY ALL`, `ORDER BY ALL`; `count(*) FILTER (WHERE …)`;
`INTERVAL 7 DAYS`; `VERSION AS OF`/`TIMESTAMP AS OF`; `MERGE … UPDATE SET * / INSERT *`; `INSERT OVERWRITE`;
array/map subscripts; `try_cast`, `::`; `IDENTIFIER('…')`; `read_files(..., format => 'csv')`; inline `VALUES … AS v(id,name)`;
`WINDOW w AS (...)`; `LEFT ANTI/SEMI JOIN`; `TABLESAMPLE`; `CLUSTER BY`; `RLIKE`; nested block comments; pipe syntax `|>`.

**Gaps found in 0.63.0 `DatabricksDialect`:**
- `CREATE [OR REPLACE] TABLE … USING delta [AS SELECT]` → error. `SparkSqlDialect` parses it. Fix PR #2425
  (https://github.com/apache/datafusion-sqlparser-rs/pull/2425) adds the missing Spark flags to DatabricksDialect.
- `a DIV 2` → error in DatabricksDialect, OK in SparkSqlDialect (same PR).
- Delta shorthand `t@v12` → error in both.
- `INSERT … BY NAME` was added in PR #2403 (merged 2026-09-14, after 0.63.0).
- Permissiveness: many dialects accept syntax that the real engine rejects (Postgres accepted `QUALIFY`).
  That is harmless for lineage, but ODS must not claim the SQL is "validated".

**Mitigation for ODS:** CLL parses dbt **compiled SELECT** SQL (`target/compiled/…`, manifest `compiled_code`),
not the materialization DDL. That keeps us away from most DDL gaps. For the rest, fall back
`DatabricksDialect → SparkSqlDialect → GenericDialect` and record which dialect succeeded as evidence.
A `Dialect` impl is a small trait, so ODS can also ship a wrapper dialect that overrides individual flags.

**Benchmark** (release build, Intel Xeon @2.10 GHz VM, 4 vCPU, single thread; `query.sql` = 194 lines /
6,150 bytes: 10 CTEs, 4 left joins + 3 inner joins, 5 window functions, many CASE/COALESCE, EXISTS subquery,
UNION ALL, ORDER BY; 50 warm-ups then 1,000 iterations of `Parser::parse_sql`):

| Dialect | Parse time / query | Throughput |
|---|---|---|
| DatabricksDialect | **602 µs** | 10.2 MB/s |
| GenericDialect | 560 µs | 11.0 MB/s |
| SnowflakeDialect | 584 µs | 10.5 MB/s |
| Tokenize only (Databricks) | 246 µs | — |

Implication: 1,000 such models parse in about 0.6 s on one core, and about 0.15 s with rayon on 4 cores.
Parsing is not the bottleneck. Name resolution and graph building, plus avoiding repeated work, matter more.

### 1.2 polyglot-sql (Rust port of sqlglot)

| Item | Finding |
|---|---|
| Repo / crate | https://github.com/tobilg/polyglot · https://crates.io/crates/polyglot-sql |
| Licence | **MIT** |
| Latest | 0.13.0 (2026-09-24); about 72k total downloads. Edition 2021, no declared MSRV |
| Scope | Parse, generate, and transpile across 30+ dialects (incl. Databricks, Spark, Snowflake, BigQuery, Postgres, DuckDB, Redshift, Hive). Also has an optimizer (qualify, annotate_types), `scope.rs`, **`lineage.rs` (7.7k LOC)** and **`openlineage.rs`**, which builds OL 2-0-2 events with the ColumnLineageDatasetFacet 1-2-0 |
| Claims | Runs 11,333 sqlglot fixture cases at 100% (README / https://tobilg.com/posts/introducing-polyglot-a-rust-wasm-sql-transpilation-library/) |
| Spans | Tokens have `Span`. AST span coverage is less complete than sqlparser's |
| Concerns | Very heavy crate (the `sqlbench` release build took 12.5 min on this 4-vCPU VM with default features). Young, with a small maintainer base. `lineage.rs` uses `HashMap`/`HashSet` (the ODS determinism rule would require sorting at the edges). `lineage(column, …)` re-runs `prepare_lineage_expression` for each column. Semantics copy sqlglot's (direct lineage only) |
| Benchmark | See §1.6 (parse 434 µs vs sqlparser 602 µs; lineage ≈2.3 ms/column) |

### 1.3 sqruff (quarylabs/sqruff)

- Apache-2.0, edition 2024. Crates `sqruff-lib-core` / `sqruff-lib-dialects` 0.40.0 (2026-08-14), about 690k downloads.
  https://github.com/quarylabs/sqruff
- A sqlfluff-style **CST** (grammar segments). Good for linting and formatting and lossless, but untyped. That makes
  semantic analysis clumsy compared with sqlparser's typed AST. Has a databricks dialect (derived from sparksql).
- Contains an **unpublished `crates/lineage`** (~3k LOC). It is a port of sqlglot's lineage (scope.rs, qualify.rs,
  expand.rs). It is useful to read, but it is not a dependency option.

### 1.4 sqlglotrs

- **Deprecated.** PyPI `sqlglotrs` 0.13.0 (2026-02-23) summary reads: "Deprecated: use sqlglotc instead".
  sqlglot now ships `sqlglotc` (mypyc-compiled Python, MIT). It was only a Python tokenizer extension and was never a
  Rust library. Not an option.

### 1.5 Other Rust crates seen (2025–2026)

| Crate | Licence | Notes |
|---|---|---|
| `openlineage_sql` (OpenLineage repo `integration/sql`, v1.54.0) | Apache-2.0 | Rust, on `sqlparser =0.62.0`. Table and column lineage (`ColumnLineage{descendant, lineage[]}`), with no transformation types and no schema/star expansion. **Not published to crates.io.** The dbt OL integration uses it (see §3.5). Good reference, too weak to adopt |
| `sqllineage` 0.2.0 | MIT/Apache-2.0 | sqlparser-based, schema-agnostic; tiny adoption (299 downloads) |
| `inbq` 0.18.0 | MIT | BigQuery-only, schema-aware CLL incl. nested structs |
| `datafusion-openlineage` 0.0.7 / `openlineage-client` 0.0.4 | Apache-2.0, MSRV 1.91 | CLL from DataFusion **logical plans** (needs a registered catalog); an OL client in Rust. The client could be a reference for ODS's OL types, but MSRV 1.91 is above ODS's 1.90 |
| `sqlglot-rust` 0.10.30 | MIT | Another sqlglot port (protegrity), small |
| `dbt-lineage`, `dbmcp-sql-lineage` | MIT | Small tools; not foundations |

### 1.6 Polyglot benchmark

Same query and machine. `polyglot-sql = "=0.13.0"` with default features.
Release build of the bench took **12 min 31 s**, almost all of it compiling polyglot-sql.

| Measurement | Result |
|---|---|
| `parse_one` DatabricksDialect | **434 µs/query** (faster than sqlparser's 602 µs) |
| `parse_one` Generic | 513 µs/query |
| `output_columns` | 37 columns (correct, star-free final projection incl. `f.*` from the CTE expanded) |
| `lineage(col, …)` without schema, 7 columns | **16 ms total ≈ 2.3 ms/column**. Every call re-prepares (qualifies) the whole query, so all 37 columns ≈ 85 ms/model |
| Correctness (source tables) | `amount_usd ← {fx_rates, raw_orders}`, `lifetime_value_usd ← {fx_rates, raw_orders}` (through the aggregate CTE), `is_top_regional_order ← {fx_rates, raw_orders, regions}` (via window `partition by region_name`), `segment ← {customers}`: all correct. `get_source_tables` returns bare table names (`raw_orders`, not `main.sales.raw_orders`) |
| Databricks probes | All OK, **including `CREATE OR REPLACE TABLE … USING delta AS SELECT`**, which sqlparser's DatabricksDialect rejects |

Takeaway: polyglot's parser is fast and has better Databricks coverage than sqlparser today. It is also a working
end-to-end lineage engine. Its costs are build time, API maturity and churn (0.x, single lead maintainer),
non-deterministic collections, per-column re-qualification, and the fact that ODS would inherit sqlglot's
direct-only semantics instead of owning the INDIRECT/confidence model. That makes it a strong **oracle**
and fallback candidate, and the reason to keep the parser behind an ODS trait.

### 1.7 Recommendation

Use **`sqlparser` 0.63 (Apache-2.0)** as the production parser, behind an ODS-owned trait
(`SqlFrontend::parse(dialect, sql) -> ods IR`). Lower sqlparser's AST into a small **ODS lineage IR**
(relations, projections, expression column-refs with spans, clauses tagged by role). Reasons:
1. Permissive licence, Apache governance, broad use (DataFusion, OpenLineage `openlineage_sql`), frequent releases.
2. Typed AST with spans, needed for explainability (rule 4): "column X derives from `a.amount * fx.rate` at model.sql:67:9".
3. Databricks/Spark coverage is good for SELECT bodies, and the gaps are small and being fixed upstream.
4. Speed is more than enough (≈0.6 ms per 200-line model; polyglot parses ~30 % faster, which doesn't matter at this scale).
5. Putting our own IR in between keeps a later parser swap (sqruff, polyglot) or a per-dialect fallback cheap.

No ADR needed for the licence (Apache-2.0 is permissive). The **choice of parser** is an ADR anyway (#73 per AGENTS.md).
Note the `sqlparser` API churn in that ADR.

---

## 2. Column-lineage algorithms

### 2.1 sqlglot `lineage` (reference algorithm)

Source: `sqlglot/lineage.py` (710 LOC), `optimizer/scope.py`, `optimizer/qualify.py`, `optimizer/qualify_columns.py`
(https://github.com/tobymao/sqlglot, MIT; docs https://sqlglot.com/sqlglot/lineage.html). Steps:

1. **Parse.** Optionally **inline `sources`**: a mapping of table name → SQL, so a dbt parent's query can be expanded
   inline (`exp.expand`).
2. **Qualify** (`qualify.qualify`): `normalize_identifiers` (dialect case rules) → `qualify_tables` (default
   catalog/db, alias every source) → `qualify_columns` (attach every column to its source; **expand `*` using
   the schema**; expand alias references in WHERE/GROUP BY/HAVING/QUALIFY; resolve positional `GROUP BY 1`; USING
   joins) → `quote_identifiers`. `validate_qualify_columns=False` in lineage, so unresolved columns do not raise.
   `infer_schema` can guess column ownership when only one source exists.
3. **Build scopes** (`build_scope`): a tree of `Scope`s typed `ROOT | SUBQUERY | DERIVED_TABLE | CTE | SET_OPERATION | UDTF`.
   Each scope has `sources` (alias → `Table` or child `Scope`), `selected_sources`, `derived_tables`,
   `subquery_scopes`, `union/set_operation_scopes`, `pivots`.
4. **`to_node(column, scope)` recursion**: find the projection by alias (or `*`).
   - If the scope is a **Subquery wrapper**, recurse into the inner scope.
   - **Set operations**: find the column's **ordinal**, then recurse into each branch by index
     (UNION matches by position, not name).
   - Otherwise create a Node for this hop. Recurse into scalar/correlated **subqueries** inside the projection.
     If the projection is `*` with no schema, attach every source as a "star" dependency. For each `Column`
     found in the projection expression: if its table is a child scope (CTE / derived table) recurse with the column
     name; if it is a pivot output, map it through the pivot chain; otherwise it is a **leaf** (physical table.column).
   - Memoized per `(column, scope, …)` (the `_cache` dict) so shared CTEs are resolved once.
     `lineage(None, sql)` returns every output column with a shared cache.
5. The result is a Node tree (name, source SQL, expression, downstream).

**What sqlglot does *not* do:** it only follows the **projection expression** (direct lineage). WHERE/JOIN/GROUP BY
columns are not reported. The Node carries no transformation classification. A missing schema for `*` gives
coarse "table.*" leaves.

### 2.2 SQLMesh

`sqlmesh/core/lineage.py` (https://github.com/TobikoData/sqlmesh, Apache-2.0): for each model it renders the query,
`qualify(query, schema=model.mapping_schema, infer_schema=True, validate_qualify_columns=False)`, builds the scope,
caches `(query, scope)` per model, and calls sqlglot `lineage`. `mapping_schema` = the **upstream models' column types**
(declared or inferred via `annotate_types`), built in DAG order, which is the same idea as §0 point 3.
`column_dependencies()` walks leaves → `{parent_model: {columns}}`. It is used for column-description inheritance
(the same idea as dbt's "passthrough/rename" inheritance) and for impact/breaking-change reasoning.

### 2.3 DataHub (sqlglot-based)

`metadata-ingestion/src/datahub/sql_parsing/sqlglot_lineage.py` (~2.7k LOC, Apache-2.0,
https://github.com/datahub-project/datahub). Docs: https://docs.datahub.com/docs/lineage/sql_parsing ·
blog https://datahub.com/blog/extracting-column-level-lineage-from-sql/ (claims **97–99 % accuracy**, chose sqlglot
after comparing it with OpenLineage's parser and sqllineage on 7k BigQuery SELECT + 2k CTAS statements).

- **Schema-aware:** it resolves every table against the DataHub graph (`SchemaResolver`) before qualification.
- **Confidence:** `SqlParsingDebugInfo.confidence = 0.9` if every table's schema resolved, else
  `0.2 + 0.3 × resolved/discovered` (0.2–0.5). It is stored in `FineGrainedLineage.confidenceScore`.
- **Result model:** `SqlParsingResult{query_type, in_tables, out_tables, column_lineage: [ColumnLineageInfo{downstream{table,
  column, column_type, native_column_type}, upstreams[ColumnRef{table,column}], logic: ColumnTransformation{is_direct_copy:
  bool, column_logic: str}}], joins: [JoinInfo{join_type, left_tables, right_tables, on_clause, columns_involved}], debug_info}`.
- **`column_logic`** = the SQL text of the expression that produces the column. `is_direct_copy` = the chain is
  a single bare column all the way down (`_get_column_transformation`).
- Robustness: LRU result cache (`SQL_PARSE_RESULT_CACHE_SIZE`), **cooperative timeout** per statement
  (`SQL_LINEAGE_TIMEOUT_SECONDS`). Unsupported statement → `UnsupportedStatementTypeError`. Column-resolution failure →
  table lineage is kept and `column_error` is set.

### 2.4 OpenMetadata (collate-sqllineage)

`ingestion/src/metadata/ingestion/lineage/parser.py` (https://github.com/open-metadata/OpenMetadata) with
`collate-sqllineage==2.1.7` (fork of reata/sqllineage, https://github.com/open-metadata/openmetadata-sqllineage).
It runs a **cascade with a 30 s timeout per parser** (`LINEAGE_PARSING_TIMEOUT = 30`):
`SqlGlotLineageAnalyzer` → `SqlFluffLineageAnalyzer` → `SqlParseLineageAnalyzer`. The first one that succeeds wins,
and the query hash is suffixed with the parser used. It is schema-agnostic (graph of tables/columns in networkx), so
`select *` cannot be expanded. OpenMetadata's dbt connector uses this on compiled SQL to create `DbtLineage` edges.

### 2.5 Steps ODS should implement

Per dbt model, in DAG (topological) order:

1. **Input**: the compiled SQL (manifest `compiled_code` or `target/compiled`), the dialect from the adapter type, the default
   catalog/schema from the node, and **upstream schemas**: `catalog.json` (sources/models, warehouse truth),
   ODS-inferred output schemas of parent models (computed earlier in this walk), and manifest `columns` (declared, weakest).
   Map `ref`/`source` relation names → `unique_id` via manifest `relation_name`, not by guessing.
2. **Parse** with sqlparser (dialect fallback chain). On failure, emit a model-level `Unparsed` finding and
   **conservatively treat every output column as depending on every input column (INDIRECT, inferred)**
   for impact analysis.
3. **Normalize identifiers** per dialect (Databricks/Spark are case-insensitive; Snowflake uppercases
   unquoted; BigQuery is mixed). Canonical key = normalized, unquoted.
4. **Resolve relations**: each FROM item → physical table (by 3-part name), CTE, derived table, UDTF
   (`explode`, `range`, `read_files`, `VALUES`), or LATERAL VIEW. Alias everything.
5. **Build scopes** (sqlglot's six kinds) with parent links (correlated subqueries resolve outward).
6. **Qualify columns**: unqualified column → the unique source that has it (needs schemas). If it is ambiguous or unknown:
   one source → assume it (evidence: `inferred_single_source`); several → attach to all candidates with
   `confidence=low`. Expand alias references (Databricks allows lateral column aliases in SELECT). Resolve
   `GROUP BY ALL` / `ORDER BY ALL` / positional refs.
7. **Expand stars** (`*`, `t.*`, `* EXCEPT (…)`, `* REPLACE`) from schemas. If the schema is unknown, keep a
   `Wildcard{source}` output entry. Downstream, a `Wildcard` maps "any column of source" (see 2.6).
8. **Trace each output column** (memoized per `(scope, column)`):
   - **DIRECT** edges from column refs in the projection expression. Subtype: `IDENTITY` (bare column,
     possibly renamed), `TRANSFORMATION` (scalar function/arith/cast), `AGGREGATION` (inside an aggregate).
     `masking=true` for hash/mask functions and `count` (per OL doc).
   - **INDIRECT** edges from columns in `JOIN … ON`/`USING` (JOIN), `WHERE`/`HAVING`/`QUALIFY` (FILTER),
     `GROUP BY` (GROUP_BY), `ORDER BY` (SORT), `PARTITION BY/ORDER BY` of the column's window (WINDOW), and
     CASE/IF/COALESCE predicate positions (CONDITIONAL; the value branches stay DIRECT). Dataset-wide ones go to the facet's
     `dataset` array. The column-specific ones (window/conditional) go on the field.
   - **Set ops**: map by ordinal across branches. Output names come from the first branch.
   - **Subqueries**: scalar subquery in the projection → its projection is DIRECT. `EXISTS`/`IN` in WHERE → FILTER.
   - **LATERAL VIEW explode(arr) e AS x** / UDTFs: `x` ← `arr` (TRANSFORMATION).
   - **PIVOT/UNPIVOT**: follow sqlglot's `_pivot_chain_mapping` (value columns ← aggregated column + pivot column).
   - **Struct/JSON paths** (`raw:a.b`, `s.a`, `xs[0]`): the lineage target is the **root column** (`raw`), with the
     path kept in `description`/evidence. OL fields are top-level names, with nested paths as `a.b` if needed.
9. **Compose across models**: a leaf that is a dbt model/seed/snapshot relation links to that node's output column.
   This gives an end-to-end column graph keyed by `(unique_id, column)`.
10. **Emit** the ODS view model (with reason chain and evidence) → OpenLineage facet / sinks.

### 2.6 Hard edge cases and conservative policy

| Case | Policy |
|---|---|
| `select *` with unknown upstream schema | Keep `Wildcard(source)`. Impact: any change to any column of the source affects all wildcard-derived columns. Mark lineage `inferred`, confidence ≤0.5 (DataHub-style score) |
| Unqualified column, several candidate sources | Edge to every candidate, `ambiguous=true`. Never pick one silently |
| UDFs / unknown functions | Assume every argument flows DIRECT/TRANSFORMATION. Table-valued UDF → every argument column flows to every output column. Optional per-function registry (known deterministic/masking/aggregate) |
| Non-deterministic / constant columns (`current_timestamp()`, literals) | Output with no inputs: `FineGrainedLineageUpstreamType.NONE` equivalent. The column still exists for impact purposes |
| Dynamic SQL (`EXECUTE IMMEDIATE`, `IDENTIFIER(:var)`, `run_query` results baked in at compile time) | dbt's compiled SQL has Jinja resolved. `IDENTIFIER('lit')` → resolve the literal. Anything unresolvable → unparsed/opaque (treated as all→all) |
| Jinja | Use dbt compiled SQL only. Never render Jinja ourselves for lineage. Missing compiled_code (e.g. not yet compiled) → the model is `Unknown` → conservative |
| Python models | No SQL → opaque: all inputs (from `depends_on`) → all outputs (from catalog/columns), INDIRECT/inferred. dbt's own CLL errors here |
| Incremental models (`is_incremental()` branch) | The compiled SQL depends on the incremental flag at compile time. The `{{ this }}` self-reference is a self-edge and must not create a cycle in the model DAG; lineage is taken from the compiled SELECT |
| Snapshots | The output adds `dbt_valid_from/to`, `dbt_scd_id`, `dbt_updated_at` (derived from strategy columns). Model them explicitly |
| Ephemeral models | Already inlined as CTEs `__dbt__cte__x` in compiled SQL. Map the CTE back to the node for display |
| Seeds | Leaves with a schema from the CSV header / catalog |
| Lateral column alias (Databricks) | Resolve SELECT-list alias references before source columns (engine precedence differs; document it) |
| Correlated subqueries | Outer references resolve through the parent scope and count as FILTER/INDIRECT for the outer column |
| Case sensitivity / quoting | Per-dialect normalization, done once. Keep the original spelling for display |
| Huge generated SQL (dbt_utils.union_relations, pivots) | Memoization, recursion limit, and a per-model time budget → partial result flagged `truncated` (like DataHub's cooperative timeout) |
| MERGE / DDL in hooks or `run_query` | Out of scope for model CLL. Treat hooks as opaque table-level operations |

### 2.7 Impact analysis ("which models must re-run when columns X change")

- Build a reverse index `(node, column) → [(child_node, child_column, edge_kind)]`.
- A change to column `c` of `N` propagates: DIRECT edges → the child column's values change → recurse.
  INDIRECT dataset-level edges (FILTER/JOIN/GROUP_BY) → **all** columns / row set of the child may change → the child
  must re-run and every child column is impacted.
- Removing or renaming a column: every consumer that references it (DIRECT or INDIRECT) breaks. Wildcard consumers
  change schema.
- Conservative defaults: unknown/opaque/ambiguous edges count as impacting (rule 3). The explanation lists the path and
  the evidence per hop (rule 4).

---

## 3. OpenLineage column lineage

### 3.1 `ColumnLineageDatasetFacet` (current: **1-2-0**)

Schema URL: `https://openlineage.io/spec/facets/1-2-0/ColumnLineageDatasetFacet.json`
(source: https://github.com/OpenLineage/OpenLineage/blob/main/spec/facets/ColumnLineageDatasetFacet.json,
docs: https://openlineage.io/docs/spec/facets/dataset-facets/column_lineage_facet). Key parts, verbatim:

```json
"ColumnLineageDatasetFacet": {
  "allOf": [
    { "$ref": "https://openlineage.io/spec/2-0-2/OpenLineage.json#/$defs/DatasetFacet" },
    { "type": "object",
      "properties": {
        "fields": {
          "description": "Column level lineage that maps output fields into input fields used to evaluate them.",
          "type": "object",
          "additionalProperties": {
            "type": "object",
            "properties": {
              "inputFields": { "type": "array", "items": { "$ref": "#/$defs/InputField" } },
              "transformationDescription": { "type": "string", "deprecated": true },
              "transformationType": { "type": "string", "deprecated": true,
                "description": "IDENTITY|MASKED ..." }
            },
            "additionalProperties": true,
            "required": ["inputFields"]
          }
        },
        "dataset": {
          "description": "Column level lineage that affects the whole dataset. This includes filtering, sorting, grouping (aggregates), joining, window functions, etc.",
          "type": "array",
          "items": { "$ref": "#/$defs/InputField" }
        }
      },
      "additionalProperties": true,
      "required": ["fields"]
    }
  ],
  "type": "object"
},
"InputField": {
  "type": "object",
  "properties": {
    "namespace": { "type": "string", "description": "The input dataset namespace" },
    "name":      { "type": "string", "description": "The input dataset name" },
    "field":     { "type": "string", "description": "The input field" },
    "transformations": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "type":        { "type": "string", "description": "The type of the transformation. Allowed values are: DIRECT, INDIRECT" },
          "subtype":     { "type": "string", "description": "The subtype of the transformation" },
          "description": { "type": "string", "description": "a string representation of the transformation applied" },
          "masking":     { "type": "boolean", "description": "is transformation masking the data or not" }
        },
        "required": ["type"],
        "additionalProperties": true
      }
    }
  },
  "additionalProperties": true,
  "required": ["namespace", "name", "field"]
}
```
The facet is attached as `outputs[i].facets.columnLineage` with `_producer` and `_schemaURL` (from `BaseFacet`).

**Type/subtype enumeration.** Not an enum in the JSON schema (free string). Defined in docs and in the Java client
`client/java/.../utils/TransformationInfo.java`:
- `type`: `DIRECT`, `INDIRECT`
- DIRECT subtypes: `IDENTITY`, `TRANSFORMATION`, `AGGREGATION`
- INDIRECT subtypes: `JOIN`, `GROUP_BY`, `FILTER`, `SORT`, `WINDOW`, `CONDITIONAL` (`IF`/`CASE WHEN`/`COALESCE`)
- `masking`: e.g. `hash` (TRANSFORMATION) and `count` (AGGREGATION) are masking.
- Placement: with dataset lineage enabled, indirect dataset-wide inputs go in `dataset[]`. Legacy producers
  (Spark default `datasetLineageEnabled=false`) copy them into every field's `inputFields`. **Consumers therefore
  vary.** ODS should make this configurable, defaulting to the spec (`dataset[]`) with a "legacy" option for sinks
  that ignore `dataset`.

### 3.2 New: explicit `LineageFacet` 1-0-0 (experimental)

- Added in OpenLineage **1.53.0 (2026-09-01)**, PR #4804. Spec `spec/facets/LineageFacet.json`
  (`https://openlineage.io/spec/facets/1-0-0/LineageFacet.json`), proposal
  `proposals/explicit_lineage_facet/ExplicitLineageProposal.md` (status **Experimental**, dated 2026-04-02,
  issue https://github.com/OpenLineage/OpenLineage/issues/4359).
- `LineageJobFacet{entries:[LineageDatasetEntry{namespace,name,type:"DATASET",inputs[],fields{<col>:{inputs:[{namespace,name,type,field,transformations[]}]}}} | LineageJobEntry]}`
  on RunEvent/JobEvent. `LineageDatasetFacet{inputs[], fields{}}` on DatasetEvent.
- It fixes the "cartesian product of inputs × outputs" problem and allows **mixed granularity**. The docs say it
  "supersedes the Column Lineage Dataset Facet"; the goal is to "eventually deprecate CLL".
- **ODS action:** emit the 1-2-0 CLL facet now, since that is what consumers read. Put the OL facet builders behind a version
  switch so `LineageJobFacet` can be added once consumers support it. dbt models are one-output jobs, so the
  cartesian problem does not affect us much.

### 3.3 Events that carry it (spec `https://openlineage.io/spec/2-0-2/OpenLineage.json`)

The top level is `oneOf [RunEvent, DatasetEvent, JobEvent]`. All share `BaseEvent{eventTime, producer, schemaURL}` (required).
- **RunEvent**: `eventType ∈ {START,RUNNING,COMPLETE,ABORT,FAIL,OTHER}`, `run{runId}`, `job{namespace,name,facets}`,
  `inputs[]`, `outputs[]` (`OutputDataset` has `facets` incl. `columnLineage`). Use this when ODS observes a dbt run
  (run_results) and wants lineage tied to execution.
- **JobEvent** (static, no `run`): `job`, `inputs[]`, `outputs[]`. **The best fit for ODS's static, design-time
  lineage** ("this dbt model's job reads X, writes Y with this CLL").
- **DatasetEvent** (static, no job/run): `dataset: StaticDataset{namespace,name,facets}`. Use it for schema/docs,
  or with the new `LineageDatasetFacet`.
- Caveat: OpenMetadata's native endpoint takes **RunEvents** (default filter `COMPLETE`). ODS should be able to wrap static
  lineage in a synthetic `COMPLETE` RunEvent (deterministic runId = UUIDv5 of manifest hash + node) for such sinks.

### 3.4 Dataset naming (https://openlineage.io/docs/spec/naming; repo `website/docs/spec/naming.md`)

| Platform | Namespace | Name |
|---|---|---|
| Unity Catalog (Databricks) | `unitycatalog://{host}` or `unitycatalog://{host}:{port}` | `{catalog}.{schema}.{table}` |
| DBFS | `dbfs://{workspace name}` | `{path}` |
| Snowflake | `snowflake://{organization name}-{account name}` (or legacy `snowflake://{locator}(.{compliance})(.{region})(.{cloud})`) | `{database}.{schema}.{table}` |
| BigQuery | `bigquery` | `{project id}.{dataset name}.{table name}` |
| Postgres | `postgres://{host}:{port}` | `{database}.{schema}.{table}` |
| Redshift | `redshift://{cluster_identifier}.{region_name}:{port}` | `{database}.{schema}.{table}` |
| Hive | `hive://{host}:{port}` | `{database}.{table}` |
| Trino | `trino://{host}:{port}` | `{catalog}.{schema}.{table}` |
| Athena | `awsathena://athena.{region}.amazonaws.com` | `{catalog}.{database}.{table}` |

Note: the OpenLineage **dbt integration** uses `databricks://{host}` for the Databricks adapter
(`integration/common/src/openlineage/common/provider/dbt/processor.py:1176`), which is not the spec's `unitycatalog://`.
ODS should default to the spec value, allow namespace override per profile, and let sinks map namespaces
(OpenMetadata has `namespaceToServiceMapping`). Put the naming behind a provider capability, so core stays
vendor-neutral (rule 1).

### 3.5 Prior art: OpenLineage's dbt integration

`openlineage-dbt` builds `ColumnLineageDatasetFacet` from **compiled SQL via `openlineage_sql.parse`**
(the Rust parser above), without schemas and without transformation types (`processor.py:get_column_lineage`).
ODS can beat this easily with schema-aware expansion, transformations, and confidence.

---

## 4. Sinks

### 4.a OpenMetadata

1. **Native REST (full control).** `PUT /api/v1/lineage` with `AddLineageRequest{edge: EntitiesEdge}`
   (schema `openmetadata-spec/.../api/lineage/addLineage.json`, `type/entityLineage.json`):
   ```json
   { "edge": {
       "fromEntity": {"id": "<uuid>", "type": "table"},
       "toEntity":   {"id": "<uuid>", "type": "table"},
       "lineageDetails": {
         "sqlQuery": "<compiled sql>",
         "source": "DbtLineage",
         "description": "...",
         "pipeline": {"id": "<uuid>", "type": "pipeline"},
         "columnsLineage": [
           {"fromColumns": ["svc.db.schema.orders.amount", "svc.db.schema.fx.rate"],
            "toColumn": "svc.db.schema.fct_orders.amount_usd",
            "function": "o.amount * coalesce(fx.rate_to_usd, 1.0)"} ] } } }
   ```
   `lineageDetails.source` enum: `Manual, ViewLineage, QueryLineage, PipelineLineage, DashboardLineage, DbtLineage,
   SparkLineage, OpenLineage, ExternalTableLineage, CrossDatabaseLineage, ChildAssets`. Edges are table→table, and
   columns use **fully-qualified names** (`service.database.schema.table.column`), so ODS must look up table FQN→UUID
   (`GET /api/v1/tables/name/{fqn}`) first. Auth: `Authorization: Bearer <JWT>` of an OpenMetadata **bot** (ingestion bot or a dedicated
   bot), referenced as an ODS secret (rule 9). There are no INDIRECT/transformation types, only `function` text.
2. **Native OpenLineage (since 2026).** `POST /api/v1/openlineage/lineage` (single) and
   `POST /api/v1/openlineage/lineage/batch` (`openmetadata-service/.../resources/lineage/OpenLineageResource.java`).
   Same bot JWT. Settings (`openLineageSettings.json`): `enabled`, `autoCreateEntities`, `defaultPipelineService`,
   `namespaceToServiceMapping`, `eventTypeFilter` (default **COMPLETE only**). `OpenLineageMapper` reads
   `outputs[].facets.columnLineage.fields` and turns it into `columnsLineage`. Tables must already exist in OpenMetadata
   (unresolved datasets are skipped with a warning). Docs: https://docs.open-metadata.org/v2.0.x/connectors/ingestion/lineage/spark-lineage.
   There is also the older **OpenLineage pipeline connector** (Kafka / Kinesis consumer):
   https://docs.open-metadata.org/v1.12.x/connectors/pipeline/openlineage.
3. **Watch out:** OpenMetadata's own dbt connector already creates `DbtLineage` edges with its sqlglot→sqlfluff→sqlparse cascade.
   ODS edges should use a distinct `source`/description and the same FQNs to avoid duplicates. Decide on
   replace-vs-merge semantics (PUT is an upsert of the edge, including lineageDetails).

### 4.b DataHub

- Aspect `upstreamLineage.fineGrainedLineages[]` (`metadata-models/.../dataset/FineGrainedLineage.pdl`):
  `upstreamType: FIELD_SET|DATASET|NONE`, `upstreams: [schemaField URN]`, `downstreamType: FIELD|FIELD_SET`,
  `downstreams: [schemaField URN]`, `transformOperation: string`, **`confidenceScore: float = 1.0`**, `query: Urn`,
  `matchType`. SchemaField URN: `urn:li:schemaField:(urn:li:dataset:(urn:li:dataPlatform:databricks,cat.sch.tbl,PROD),col)`.
  Emit via REST `POST /aspects?action=ingestProposal` / OpenAPI v3, or the GraphQL `updateLineage` mutation.
- OpenLineage: `POST {GMS}/openapi/openlineage/api/v1/lineage` (https://docs.datahub.com/docs/lineage/openlineage).
  DataHub states that column-level lineage through this endpoint is **limited** (full CLL via its Spark plugin).
  **A native adapter is needed** for good CLL, and it maps ODS confidence directly onto `confidenceScore`.

### 4.c Marquez (OpenLineage reference backend)

- Ingests OL natively: `POST /api/v1/lineage`. CLL has been stored since 0.27.0 and is queryable via
  `GET /api/v1/column-lineage?nodeId=datasetField:<ns>:<name>:<field>&depth=20&withDownstream=true`
  (https://marquezproject.ai/docs/api/get-column-lineage/). Best **conformance/integration test target**
  (Docker), since it is the reference implementation.

### 4.d Databricks Unity Catalog

- UC **automatically captures runtime table and column lineage** for queries run on Databricks, incl. dbt-databricks
  runs (system tables `system.access.table_lineage`, `system.access.column_lineage`; REST lineage-tracking API).
  For dbt-on-Databricks there is nothing to push. ODS could instead **read** UC column lineage as ground-truth
  evidence to validate its static CLL (capability-gated provider feature).
- **External Lineage API** (`POST /api/2.0/lineage-tracking/external-lineage`,
  `POST /api/2.0/lineage-tracking/external-metadata`; https://learn.microsoft.com/en-us/azure/databricks/data-governance/unity-catalog/external-lineage):
  relationships with `columns:[{source,target}]` between UC tables / paths / model versions and **external metadata
  objects**. Linking two UC tables directly is not supported: you must insert an external metadata object between
  them. External lineage is **not** written to the lineage system tables. Limits: 10,000 external metadata objects and
  100,000 relationships per metastore. Use case for ODS: only for non-Databricks upstream/downstream assets. Low priority.

### 4.e Others (brief)

- **Apache Atlas / Microsoft Purview**: Atlas `Process` entities with a `columnMapping` attribute (JSON string of
  `[{DatasetMapping:{Source,Sink}, ColumnMapping:[{Source,Sink}]}]`) via `/api/atlas/v2/entity/bulk`.
  UI support for custom-process column mapping is patchy (e.g. Microsoft Tech Community thread "Column-Level Lineage
  Visualization Issue for Custom Entities and Processes in Azure Purview"). Native adapter only on demand.
- **Collibra**: ingests OpenLineage files through its lineage harvester
  (https://productresources.collibra.com/docs/collibra/latest/Content/CollibraDataLineage/DataSources/OpenLineage/ref_openlineage-architecture.htm).
- **Google Dataplex/Data Lineage**: accepts OpenLineage RunEvents (`ProcessOpenLineageRunEvent`,
  https://docs.cloud.google.com/dataplex/docs/open-lineage).
- **Atlan, Dataedo, Alteryx, Astronomer**, etc. also consume OpenLineage.

### 4.f Conclusion

- **Single open format: OpenLineage** (static `JobEvent` plus an optional synthetic `COMPLETE` `RunEvent`, CLL facet 1-2-0).
  It reaches Marquez, OpenMetadata (native HTTP endpoint), DataHub (table-level reliable, CLL partial), Dataplex, Collibra,
  Atlan and others through one transport (HTTP, file, Kafka).
- **Native adapters worth building:** (1) **OpenMetadata REST** (`PUT /api/v1/lineage`) as the first sink, for idempotent
  upsert/delete, `DbtLineage` source tagging, and `function` text, without depending on OL settings on the server.
  (2) **DataHub `fineGrainedLineages`** (confidence, precise CLL). Not needed: Unity Catalog (it captures CLL natively),
  Marquez (OL is native). Optional later: Purview/Atlas.
- Architecture: sinks live in `providers/*` (vendor code) behind an SDK `LineageSink` contract with capabilities
  (`supports_indirect`, `supports_confidence`, `supports_delete`, `requires_existing_entities`), plus a fake sink and
  conformance tests (per AGENTS.md).

---

## 5. dbt's own CLL

Sources: dbt docs repo (public `dbt-labs/docs.getdbt.com`, `website/`): `docs/docs/explore/column-level-lineage.md`,
`docs/docs/build/dbt-information-schema.md`, `docs/reference/info-schema.md`, `snippets/_fusion-features.md`,
`docs/docs/dbt-licensing.md`. Public page: https://docs.getdbt.com/docs/explore/column-level-lineage.

- **dbt Catalog (platform)**: CLL requires an **Enterprise or Enterprise+** plan with Catalog. It updates after each
  prod/staging run, and at least one job must run `dbt docs generate`. Features: column card lineage, the **column evolution
  lens** (Transformed vs **Passthrough / Rename**), and **inherited descriptions** for passthrough/rename columns.
- **Documented limitations:** "reflects the lineage from `select` statements … doesn't reflect other usage like
  joins and filters" (direct only, like sqlglot); parsing errors (complex lateral joins, JSON unpacking); **Python
  model → "Python error"** (lineage not possible); "Unknown error", e.g. hard-coded table names instead of `ref`.
  CLL is not available in the Studio IDE's development tab.
- **dbt v2 locally**: CLL via `dbt compile --generate-info-schema --static-analysis strict`, then
  `dbt show --info column_lineage`, or read the Parquet `target/info_schema/v1/dbt.column_lineage.parquet`
  directly. The "dbt Information Schema" is described as a **contracted interface**: Parquet tables `dbt.models`,
  `dbt.node_columns` (`node_unique_id, column_name, data_type_declared, description, tags, …`), `dbt.edges`,
  `dbt.column_lineage` ("populated with `--static-analysis strict`"), and a `dbt_rt.*` runtime namespace. The docs
  do **not** publish the column list of `dbt.column_lineage`.
- **Licensing / openness:** `_fusion-features.md` lists "Precise column-level lineage" and "dbt docs v2 (full), including
  column-level lineage" as **requires login** (to a free or paid dbt platform account), in **Fusion**, which is
  proprietary (dbt Product Licensing Agreement). `dbt-oss` (Apache-2.0 v2 runtime) and dbt Core v1 **do not emit
  column lineage**. v1 artifacts carry only *declared* `columns` (manifest) and warehouse columns and types
  (`catalog.json`). There is no lineage in either.
- **ODS stance (clean-room):** compute CLL ourselves from compiled SQL plus catalog. Optionally, as a user-supplied
  input, **read** `dbt.column_lineage.parquet` when present and use it as corroborating evidence. Use only the documented
  file/interface, never Fusion code. Its schema is undocumented, so treat that as best-effort and version-gated
  (rule: don't invent fields; inspect a real file before coding against it). Requires a Parquet reader dependency
  (`arrow`/`parquet` crates, Apache-2.0).

---

## 6. Performance and incremental design

### 6.1 Published numbers

- DataHub: 97–99 % accuracy on its benchmark (7k BigQuery SELECT + 2k CTAS). No published throughput. It uses an
  LRU cache and a per-statement cooperative timeout, which implies worst cases in the seconds range.
- OpenMetadata: 30 s timeout per parser in the cascade (`LINEAGE_PARSING_TIMEOUT = 30`), so slow queries are
  expected to happen.
- sqlglot: pure Python (now optionally mypyc-compiled via `sqlglotc`). Qualification plus lineage of large dbt models
  commonly takes tens to hundreds of ms per model. Community tools add wall-clock budgets (e.g. `MAX_LINEAGE_SECONDS` in
  Oisix/dbt-column-lineage) for hub columns in large projects. No authoritative benchmark was found. Treat as
  anecdotal.
- sqlglot's own `lineage(None, …)` shares a cache across all output columns. Calling it per column without
  `scope=` re-qualifies every time. SQLMesh caches `(query, scope)` per model for that reason. polyglot's
  `lineage()` API has the same per-column re-prepare cost.
- This study: sqlparser parses a 6 KB analytic model in about 0.6 ms, and polyglot in 0.43 ms. polyglot's per-column lineage costs ≈2.3 ms/column (≈85 ms for a 37-column model) because it re-qualifies per column. A shared-scope, all-columns-at-once design (sqlglot `lineage(None)`, SQLMesh's cache) removes that factor (§1.1, §1.6).

### 6.2 Incremental CLL design for ODS

- **Unit of work** = one model. **Cache key** = `blake3(schema_version ‖ engine_version ‖ dialect ‖ normalized
  compiled SQL ‖ canonical(relevant upstream schemas) ‖ default catalog/schema ‖ lineage options)`.
  "Relevant upstream schemas" = the sorted `(relation, [(column, type)])` for relations the model reads (from
  the parse's relation list). The first pass extracts relations cheaply; the second pass keys on them.
- **Per-model result** (content-addressed, stored in the ODS state store): output schema (columns, types if
  known), per-column edges with type/subtype/evidence/confidence, relation refs, diagnostics. Persist it with
  `schema_version` (rule: persisted formats are versioned). Write it only after the whole run succeeds (rule 5).
- **Early cutoff (like build systems/salsa):** if a model's recomputed **output schema** hash is unchanged, its children's
  keys do not change, so they stay cached even if the model's SQL changed. Only changes to column names or types ripple.
- **Parallelism:** a topological wavefront over the DAG, with rayon inside each level. Parsing and per-model analysis are pure
  and synchronous (AGENTS.md "async only at I/O").
- **Project-level graph** built by joining cached per-model results. Impact queries = BFS on the reverse index,
  O(affected edges).
- **Determinism:** BTreeMap / sorted Vec everywhere in results and hashes. Canonical JSON for hashing.
- **Budgets:** a per-model node/recursion budget and a time budget → `truncated` diagnostic plus conservative all→all edges.
- **Invalidation sources:** manifest `checksum` is *raw* code only. Compiled SQL can change through macros or vars
  without the raw checksum changing, so **key on compiled SQL**, not on dbt's checksum.

---

## 7. Proposed follow-up issues / ADRs (for ROADMAP triage, not created)

- ADR: SQL parser choice (`sqlparser` 0.63, pinned) and the ODS lineage IR (#73).
- ADR: CLL result model plus persisted format (edges with OL type/subtype, confidence, evidence, `schema_version`).
- ADR: Lineage export contract (`LineageSink`) and OpenLineage mapping (namespace capability, `dataset[]` vs legacy mode).
- Provider crates: `ods-sink-openlineage` (HTTP/file), `ods-provider-openmetadata` (native REST), later DataHub.
- Test corpus: fixtures dbt project plus golden CLL snapshots (insta). Optional differential check vs polyglot/sqlglot output
  generated offline (store the expected JSON, with no Python in CI).

---

## Sources

- sqlparser: https://github.com/apache/datafusion-sqlparser-rs · https://crates.io/crates/sqlparser ·
  PR #2425 https://github.com/apache/datafusion-sqlparser-rs/pull/2425 · PR #2403 https://github.com/apache/datafusion-sqlparser-rs/pull/2403 ·
  release issue https://github.com/apache/datafusion-sqlparser-rs/issues/2454
- polyglot: https://github.com/tobilg/polyglot · https://crates.io/crates/polyglot-sql · https://docs.rs/polyglot-sql/latest/polyglot_sql/lineage/index.html
- sqruff: https://github.com/quarylabs/sqruff · https://crates.io/crates/sqruff-lib-dialects
- sqlglot: https://github.com/tobymao/sqlglot · https://sqlglot.com/sqlglot/lineage.html · https://pypi.org/project/sqlglotrs/
- SQLMesh: https://github.com/TobikoData/sqlmesh (sqlmesh/core/lineage.py)
- DataHub: https://docs.datahub.com/docs/lineage/sql_parsing · https://datahub.com/blog/extracting-column-level-lineage-from-sql/ ·
  https://docs.datahub.com/docs/lineage/openlineage · FineGrainedLineage.pdl in https://github.com/datahub-project/datahub
- OpenMetadata: https://github.com/open-metadata/OpenMetadata · https://github.com/open-metadata/openmetadata-sqllineage ·
  https://docs.open-metadata.org/v1.12.x/connectors/pipeline/openlineage · https://docs.open-metadata.org/v2.0.x/connectors/ingestion/lineage/spark-lineage
- OpenLineage: https://github.com/OpenLineage/OpenLineage (spec/, website/docs/spec/, integration/sql, integration/common/.../dbt/processor.py) ·
  https://openlineage.io/docs/spec/facets/dataset-facets/column_lineage_facet · https://openlineage.io/docs/spec/naming ·
  https://github.com/OpenLineage/OpenLineage/issues/4359
- Marquez: https://marquezproject.ai/docs/api/get-column-lineage/ · https://github.com/MarquezProject/marquez/blob/main/proposals/2045-column-lineage-endpoint.md
- Databricks: https://learn.microsoft.com/en-us/azure/databricks/data-governance/unity-catalog/external-lineage ·
  https://docs.databricks.com/aws/en/data-governance/unity-catalog/data-lineage · https://docs.databricks.com/aws/en/admin/system-tables/lineage
- Purview/Atlas: https://learn.microsoft.com/en-us/purview/data-gov-api-create-lineage-relationships ·
  https://datasmackdown.com/oss/pyapacheatlas/entities-with-lineage-column-mapping.html ·
  https://techcommunity.microsoft.com/discussions/azurepurview/column-level-lineage-visualization-issue-for-custom-entities-and-processes-in-az/4437869
- Collibra: https://productresources.collibra.com/docs/collibra/latest/Content/CollibraDataLineage/DataSources/OpenLineage/ref_openlineage-architecture.htm
- Dataplex: https://docs.cloud.google.com/dataplex/docs/open-lineage
- dbt: https://docs.getdbt.com/docs/explore/column-level-lineage · https://docs.getdbt.com/docs/build/dbt-information-schema ·
  https://docs.getdbt.com/reference/info-schema · https://docs.getdbt.com/docs/dbt-licensing
- Other crates: https://crates.io/crates/sqllineage · https://crates.io/crates/inbq · https://crates.io/crates/datafusion-openlineage ·
  https://crates.io/crates/sqlglot-rust · https://github.com/Oisix/dbt-column-lineage
