# Column-level lineage

!!! warning "Experimental"
    Lineage is ODS's most developed capability, but it is still a preview. Check
    surprising results against the SQL, and please report them.

`ods lineage` parses every model's compiled SQL and works out, **for each output
column**, which upstream columns feed it and how. It also records which columns
decide **which rows** exist. It reads only dbt's artifacts, with no dbt login and no
warehouse connection.

## Try the live demo

[Open the lineage explorer on the demo project :material-open-in-new:](demo/lineage/){ .md-button .md-button--primary target="_blank" }

The demo is `jaffle-ods`, a small dbt project in the repository (`fixtures/dbt/`). This
site's build generates the explorer from it with `ods lineage view --site`. Search a
column (press `/`), click it to highlight everything upstream (blue) and downstream
(orange), and toggle indirect edges.

![The lineage explorer tracing customers.lifetime_value](images/lineage-viewer.png)

## Where does each column come from?

```console
$ ods lineage columns --model customers
customers
  customer_id
    stg_customers.customer_id (identity)
  full_name
    stg_customers.first_name (transformation)
    stg_customers.last_name (transformation)
  first_order
    orders.order_date (aggregation)
  lifetime_value
    orders.amount (aggregation)
  value_tier
    orders.amount (indirect:conditional)
  rows shaped by
    orders.customer_id (indirect:join)
    orders.customer_id (indirect:group_by)
    orders.status (indirect:filter)
```

(Output shortened.) Every edge has a kind:

| Kind | Meaning |
|---|---|
| `identity` | the value is copied unchanged |
| `transformation` | the value is computed from the input (`first_name || ' ' || last_name`) |
| `aggregation` | the value is aggregated from the input (`sum(amount)`) |
| `indirect:*` | the input doesn't flow into the value but shapes it or the rows: `join`, `filter`, `group_by`, `conditional`, window ordering |

Edges are traced through CTEs, subqueries, `select *`, unions and window functions,
to physical columns.

## Change impact

`ods lineage impact` answers "what must run if this changes, and what can be skipped?"

```console
$ ods lineage impact --column stg_orders.status
changes: 1
must run: 4
pruned: 1

model.jaffle_ods.orders
  changed columns: status
  jaffle_ods.main.stg_orders.status modified → `status` (identity)

model.jaffle_ods.customers
  all rows may change
  jaffle_ods.main.orders.status modified → its rows (indirect:filter)
…
Skipped: they read changed models but none of the changed columns
model                        reads                       unused changed columns
model.jaffle_ods.order_events  jaffle_ods.main.stg_orders  status
```

Impact is decided conservatively:

- A model ODS **can't analyze** (a Python model, unsupported SQL, `select *` over
  unknown columns) is *opaque*. Any change to what it reads makes it run.
- A change to a column that decides **which rows exist** (a filter, join or grouping)
  makes every reader run.
- A **modified or removed** column makes a reader run only if it uses that column.
- Every reader that is **skipped** is listed, with the changed columns it doesn't use,
  so you can check the reasoning.

Compare two whole builds with `--base ../prod/target`. Every difference in compiled
SQL becomes column changes.

## Export and share

| Command | Output |
|---|---|
| `ods lineage view` | one offline HTML file, or a static site with `--site DIR` |
| `ods serve` | the explorer plus a read-only JSON API, reloading when artifacts change |
| `ods lineage graph --format …` | `json` (documented, `schema_version` 1), Graphviz `dot`, `mermaid`, `graphml` |
| `ods lineage export` | [OpenLineage](https://openlineage.io) events, for catalogs that accept them |

## Observed lineage from the warehouse

Databricks Unity Catalog records column lineage for the queries it runs, including
Python. ODS can read an **export** of that table; it doesn't connect to the
workspace itself:

- `ods lineage compare` checks the static analysis against what actually ran;
- `--observed FILE` fills in models the analyzer can't read, labelled `observed`.

## Limits today

- SQL dialects: Databricks/Spark, DuckDB, Snowflake, BigQuery, Postgres, Redshift and a
  generic dialect. Unusual syntax can make a model opaque; ODS says so rather than
  guessing.
- Macros are seen only as the SQL they compile to.
- Python models are opaque without observed lineage.

Full flags are in the [CLI reference](cli.md#column-level-lineage). The design is in
[ADR-0008](adr/0008-column-level-lineage.md).
