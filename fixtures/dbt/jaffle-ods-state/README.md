# jaffle-ods-state

The `jaffle-ods` project with dbt State configuration in every place users put it (#168):

| Where | Config |
|---|---|
| `dbt_project.yml` (`marts/`) | `+state: {lag_tolerance: 4h, require_fresh_data_from: all}` |
| `models/marts/orders.sql` | `config(state={'lag_tolerance': '2h', 'evaluate_volatile_sql': true})` |
| `models/marts/order_events.sql` | `config(state={'execute_hooks_on_any_reuse': true})` |
| `models/schema.yml` (`customers`) | `state: {lag_tolerance: 1d, pre_clone: never}` and SAO `freshness.build_after` |
| `models/sources.yml` | `loaded_at_field`, `loaded_at_query`, and a source with neither |

The staging models configure nothing, so they get dbt State's defaults: the project
uses dbt State.

The two dbt versions resolve the `state` block differently. dbt v2 merges it key by
key: `orders` keeps `require_fresh_data_from: all` from the project. dbt 1.10 lets the
SQL `config()` replace the whole project block.

## Regenerate
Everything runs locally, with no warehouse. `parse` and `compile` are enough, because
only configs are read.

```sh
DBT_SEND_ANONYMOUS_USAGE_STATS=false dbt parse --profiles-dir .          # dbt-core 1.10 + dbt-duckdb
DBT_SEND_ANONYMOUS_USAGE_STATS=false dbt compile --profiles-dir . \
    --generate-info-schema --target-path target-v2                      # dbt-oss 2.0.5
```

Then copy `target/manifest.json` to `artifacts/dbt-1.10/`, and `target-v2/manifest.json`
and `target-v2/info_schema/v1/` to `artifacts/dbt-2.0/`. Replace the absolute project
path with `<project_root>`.
