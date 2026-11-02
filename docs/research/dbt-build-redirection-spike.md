# Spike: can dbt build into locations ODS chooses?

Date: 2026-11-02.
- For: [ADR-0028](../adr/0028-open-data-environments.md) (open data environments). Its
  first risk is that dbt can't reliably build a node into a schema and alias ODS
  chooses without changing the user's models.
- Setup: dbt-core 1.12.5 and dbt-duckdb, pinned as in `.github/dbt/requirements-1.12.txt`.
  The `jaffle-ods` fixture (views, tables, a Python model, a constraint-enforced model,
  10 data tests), on DuckDB. No warehouse, no network.

## Answer
**Yes, with one small addition to the user's project and no change to any model.**
- ODS's placement reaches dbt as `--vars`. A small ODS macro package turns it into a
  schema and alias per node.
- dbt only takes `generate_schema_name` and `generate_alias_name` from the root
  project, so the package alone isn't enough. The root project needs either:
  - **Route A:** a two-macro file ODS adds, when the project has no naming macros of
    its own; or
  - **Route B:** a three-line change to the user's own naming macros, making them ask
    ODS first.
- Everything else followed the redirection: `ref()`s, data tests, `{{ this }}` in hooks,
  the Python model, and `--defer` to tables of an earlier version.
- One thing broke: a relation name written out by hand instead of through `ref()`.

## What was tried

| # | Approach | Result |
|---|---|---|
| 1 | `generate_schema_name`/`generate_alias_name` in an installed package | **Ignored.** Everything built in `main`. |
| 2 | Same, plus `dispatch: [{macro_namespace: dbt, search_order: [ods_env, …]}]` | **Ignored.** These macros aren't dispatched. |
| 3 | Route A: ODS adds a root macro file; the project has no naming macros | **Works** for a SQL table, a SQL view and the Python model. |
| 4 | Route A when the project already has its own `generate_schema_name` | **Compilation error:** `dbt found multiple macros named "generate_schema_name"`. |
| 5 | Route B: the user's macro asks ODS first | **Works**, same as 3. |
| 6 | Dev build of a changed model with `--defer --state <prod manifest>` | **Works.** Only changed versions build. Unchanged parents are read from prod's physical tables. Prod's tables are untouched. |
| 7 | Promote and roll back by repointing prod's views | **Works**, as one DuckDB transaction. |
| 8 | Route C: dbt builds as usual, then ODS moves the table and leaves a pointer | **Rejected for views**; tables only, and DuckDB can't move a table across schemas. |
| 9 | Data tests and a post-hook using `{{ this }}` under Route A | **Work.** 10 of 10 tests pass on the physical tables. Every hook saw the physical relation. |

### 1–2: why a package can't do it
dbt resolves these macros when it parses the project, in `RelationUpdate`
(`dbt/parser/base.py`), through `Manifest.find_generate_macro_by_name`. A node uses its
own package's `generate_<component>_name` if that package defines one. Otherwise it uses
the root project's, and failing that, dbt's built-in. Another installed package is never
consulted, and `dispatch` plays no part. So ODS can ship the logic as a package, but the
root project has to call it.

### 3–5: the two working routes
The package (`ods_env`) only looks up ODS's placement. Nodes ODS doesn't place fall back
to the project's normal naming:

```jinja
{% macro schema_for(node) -%}
  {%- set m = var('ods_env', {}) -%}
  {%- if node is not none and node.unique_id in m -%}{{ return(m[node.unique_id]['schema']) }}{%- endif -%}
  {{ return(none) }}
{%- endmacro %}
{# alias_for(node) is the same with 'alias'. #}
```

Route A is the root file ODS adds when the project has no naming macros:

```jinja
{% macro generate_schema_name(custom_schema_name, node) -%}
  {%- set s = ods_env.schema_for(node) -%}
  {{ return(s if s is not none else dbt.default__generate_schema_name(custom_schema_name, node)) }}
{%- endmacro %}
{# generate_alias_name: the same with alias_for and default__generate_alias_name. #}
```

Route B is the one-time change to a user's existing macro. It adds one line at the
top; the user's own logic below it is unchanged:

```jinja
{% macro generate_schema_name(custom_schema_name, node) -%}
  {%- set s = ods_env.schema_for(node) -%}{%- if s is not none -%}{{ return(s) }}{%- endif -%}
  {# … the user's existing logic … #}
{%- endmacro %}
```

The placement was passed as
`--vars '{ods_env: {model.jaffle_ods.orders: {schema: ods_physical, alias: orders__3f9a1c}, …}}'`.
The compiled SQL of every child then read `"ods_physical"."orders__3f9a1c"`.

### 6: dev environments with defer
1. **Prod build.** Every model was placed at `ods_physical.<name>__<version key>`. The
   key here was a digest of the model's file and its parents' keys, standing in for
   ADR-0028's version key. `prod.<name>` views pointed at those tables.
2. **The change.** A column was added to `customers`.
3. **New placement.** Four models got new versions: `customers` and the three models
   downstream of it. The six others kept theirs.
4. **Dev build.** `dbt build -s customers+ --defer --state prod_state` built exactly
   those four, into new physical tables. `customers` read
   `ods_physical.orders__278ca4` and `ods_physical.stg_customers__7c3d07`, prod's own
   tables. Both versions of each changed model then existed side by side.

The parent part of the key is what keeps prod safe. With a key from the file alone, the
three downstream models would have been given the same names as prod's tables, and the
dev build would have written over them. ADR-0028's version key covers this.

A dbt *view* model built this way is a view in `ods_physical` that names its parents'
physical tables. That pins the view to its parents' versions, which is what a version
should mean.

### 7: promotion and rollback
Repointing all prod views at dev's versions, inside one DuckDB transaction, made the
new column visible in `prod.customers`. Repointing back removed it. DuckDB has
transactional DDL, so a DuckDB provider could offer ADR-0028's `atomic_multi_pointer`.

### 8: why "build in place, then move" doesn't work for views
Route C needs nothing added to the project: dbt builds where it always does, then ODS
moves the result into `ods_physical` and leaves a pointer behind. Two problems:
- **Views can't be moved.** A view built in place names its parents by their names
  in the environment (`"main"."raw_orders"`), not by version. Promoting it to prod would
  leave prod reading dev's tables. Views would have to be rebuilt on promotion.
- **Moving tables depends on the warehouse.** DuckDB can't move a table to another
  schema (`ALTER TABLE … SET SCHEMA` is not implemented), so it would copy the data.
  Other warehouses differ (see *Not tested*).

Route C stays as the fallback for tools that can't redirect writes at all, for table
models only.

### The one breakage: relation names written by hand
`customer_order_rank` failed. Its enforced constraint in `models/schema.yml` names its
parent directly, as `foreign key (order_id) references main.orders (order_id)`. After
redirection there is no `main.orders`. The same applies to anything that names a
relation without `ref()` or `source()`:
- constraint expressions;
- hooks and macros with literal table names;
- grants or post-hooks naming other models;
- raw SQL in analyses or operations.

Following rule 3, ODS should find these and refuse to place the node they name; it
builds in place instead, and the plan says why. ODS's lineage parser
(ADR-0008) already reads every table a model's SQL names, so most of these can be found.
Constraints and hooks need their own check.

## Not tested
- **Databricks.** No workspace was available, so these are open:
  - whether Unity Catalog can rename a table into another schema;
  - whether a shallow-clone pointer survives its source being replaced;
  - whether any transaction covers several objects.

  These decide the phase 1 pointer strategy and whether promotion needs the
  non-atomic opt-in.
- **dbt 1.11.** The lookup code read here is from 1.12.5. 1.11 is expected to behave
  the same, but wasn't run.
- **Large projects.** The placement travels as `--vars`, which grows with the number of
  models. dbt also re-parses the project when `--vars` change, which costs time on big
  projects. A smaller channel, such as an environment variable read with `env_var()`,
  or placement computed by the macro from a short environment name and a key map, is
  untested.
- **Incrementals, snapshots, seeds and materialized views.** These are opted out in
  ADR-0028's phase 1.
- **Unit tests, exposures, semantic models** and dbt's `--empty` and `--sample`.

## What this changes in ADR-0028
- **Write redirection for dbt:** an ODS macro package plus Route A or Route B, never
  model edits. An `ods env init` command could apply Route A, or print the Route B
  change for the user to review.
- **New opt-out:** nodes referenced by a hand-written relation name build in place.
- **Pointer strategy:** "build in place, then move" is limited to table models and
  tools that can't redirect writes.
- **Phase 1 next steps:** the Databricks questions above, and a placement channel that
  scales.
