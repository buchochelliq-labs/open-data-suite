# dbt error patterns

How ODS recognises what dbt, and the engines dbt runs on, say when a node or a test
fails ([ADR-0025](../adr/0025-error-explanations.md)). This page is the dbt error
catalogue (`ods-provider-dbt::error_catalogue`) at catalogue version **5**. A test
keeps it in step with the code.

A pattern reads only the error's redacted summary: its kind and its first line, with
quoted values, numbers and SQL removed (ADR-0024), so a phrase can never match on, or
show, a value. The message is lowercased before matching. *Kind* is the error's kind,
which is either the engine's own (`Binder Error`) or the one dbt's header gave around it
(`Database Error`); `any` means any kind. The first pattern whose kind and phrases all
match wins. A space at either end of a phrase matters and is shown as `␠`: `column␠`
matches `column "x"`, not `columnar`.

*Recorded from real dbt*: `yes` when a message captured from a real run of dbt 1.10,
1.11 or 1.12 (`fixtures/dbt/jaffle-ods/artifacts/dbt-<version>-errors`) matches it.
Otherwise the pattern is taken from the public source named; a test still reaches it
with a written message. Recording the Databricks ones is #349.

<!-- patterns:begin -->
| Pattern | Symptom | Kind | The message holds | Source | Recorded from real dbt |
|---|---|---|---|---|---|
| `dbt-undefined-macro` | `unknown_macro` | any | `is undefined` and `macro that does not exist` | dbt-core's messages | yes |
| `dbt-missing-ref` | `missing_ref` | any | `depends on a node named` and `which was not found` | dbt-core's messages | yes |
| `dbt-packages-not-installed` | `packages_missing` | any | `specified in packages.yml, but only` and `installed in` | dbt-core's messages | yes |
| `dbt-packages-expected` | `packages_missing` | any | `based on packages specified in packages.yml, but found only` and `installed in` | dbt-core's messages | yes |
| `dbt-profile-not-found` | `profile_not_found` | any | `could not find profile named` | dbt-core's messages | yes |
| `dbt-target-not-found` | `profile_not_found` | any | `does not have a target named` | dbt-core's messages | no: from the source named |
| `dbt-template-unexpected` | `template_syntax` | `compilation error` | `unexpected␠` | dbt-core's messages | yes |
| `dbt-template-expected-token` | `template_syntax` | `compilation error` | `expected token` | dbt-core's messages | no: from the source named |
| `dbt-template-unknown-tag` | `template_syntax` | `compilation error` | `unknown tag` | dbt-core's messages | no: from the source named |
| `dbt-python-model` | `python_exception` | any | `python model failed` | dbt-core's messages | yes |
| `dbt-test-failed` | `test_failed` | any | `configured to fail if` | dbt-core's messages | yes |
| `duckdb-values-list-column` | `missing_column` | `binder error` | `does not have a column named` | DuckDB's errors, via dbt-duckdb | yes |
| `duckdb-referenced-column` | `missing_column` | `binder error` | `referenced column` and `not found` | DuckDB's errors, via dbt-duckdb | yes |
| `duckdb-table-missing` | `missing_relation` | `catalog error` | `table with name` and `does not exist` | DuckDB's errors, via dbt-duckdb | yes |
| `duckdb-view-missing` | `missing_relation` | `catalog error` | `view with name` and `does not exist` | DuckDB's errors, via dbt-duckdb | no: from the source named |
| `duckdb-schema-missing` | `missing_schema` | `catalog error` | `schema with name` and `does not exist` | DuckDB's errors, via dbt-duckdb | no: from the source named |
| `duckdb-function-missing` | `missing_function` | `catalog error` | `function with name` and `does not exist` | DuckDB's errors, via dbt-duckdb | yes |
| `duckdb-conversion` | `type_mismatch` | `conversion error` | (any message) | DuckDB's errors, via dbt-duckdb | yes |
| `duckdb-constraint` | `constraint_violation` | `constraint error` | (any message) | DuckDB's errors, via dbt-duckdb | no: from the source named |
| `duckdb-dependent-entries` | `dependent_objects` | `dependency error` | `because there are entries that depend on it` | DuckDB's errors, via dbt-duckdb | yes |
| `duckdb-write-conflict` | `lock_conflict` | `transactioncontext error` | `conflict` | DuckDB's errors, via dbt-duckdb | no: from the source named |
| `duckdb-file-lock` | `lock_conflict` | any | `could not set lock on file` | DuckDB's errors, via dbt-duckdb | no: from the source named |
| `duckdb-permission` | `permission_denied` | `permission error` | (any message) | DuckDB's errors, via dbt-duckdb | no: from the source named |
| `duckdb-interrupted` | `query_timeout` | `interrupt error` | (any message) | DuckDB's errors, via dbt-duckdb | no: from the source named |
| `postgres-column-missing` | `missing_column` | `database error` | `column␠` and `does not exist` | PostgreSQL's documented messages | no: from the source named |
| `postgres-schema-missing` | `missing_schema` | `database error` | `schema␠` and `does not exist` | PostgreSQL's documented messages | no: from the source named |
| `postgres-function-missing` | `missing_function` | `database error` | `function␠` and `does not exist` | PostgreSQL's documented messages | no: from the source named |
| `postgres-relation-missing` | `missing_relation` | `database error` | `relation␠` and `does not exist` | PostgreSQL's documented messages | no: from the source named |
| `postgres-permission-denied` | `permission_denied` | `database error` | `permission denied for` | PostgreSQL's documented messages | no: from the source named |
| `postgres-statement-timeout` | `query_timeout` | `database error` | `canceling statement due to statement timeout` | PostgreSQL's documented messages | no: from the source named |
| `postgres-invalid-input` | `type_mismatch` | `database error` | `invalid input syntax for type` | PostgreSQL's documented messages | no: from the source named |
| `postgres-password` | `credentials_missing` | `database error` | `password authentication failed` | PostgreSQL's documented messages | no: from the source named |
| `postgres-unique` | `constraint_violation` | `database error` | `violates unique constraint` | PostgreSQL's documented messages | no: from the source named |
| `postgres-not-null` | `constraint_violation` | `database error` | `violates not-null constraint` | PostgreSQL's documented messages | no: from the source named |
| `postgres-connect` | `warehouse_unavailable` | `database error` | `could not connect to server` | PostgreSQL's documented messages | no: from the source named |
| `spark-unresolved-column` | `missing_column` | any | `[unresolved_column` | Apache Spark's `error-conditions.json` | no: from the source named |
| `spark-table-or-view-not-found` | `missing_relation` | any | `[table_or_view_not_found]` | Apache Spark's `error-conditions.json` | no: from the source named |
| `spark-cast-invalid-input` | `type_mismatch` | any | `[cast_invalid_input]` | Apache Spark's `error-conditions.json` | no: from the source named |
| `spark-datatype-mismatch` | `type_mismatch` | any | `[datatype_mismatch` | Apache Spark's `error-conditions.json` | no: from the source named |
| `spark-check-constraint` | `constraint_violation` | any | `[check_constraint_violation]` | Apache Spark's `error-conditions.json` | no: from the source named |
| `spark-not-null-constraint` | `constraint_violation` | any | `[not_null_constraint_violation]` | Apache Spark's `error-conditions.json` | no: from the source named |
| `delta-not-null-constraint` | `constraint_violation` | any | `[delta_not_null_constraint_violated]` | Delta Lake's `delta-error-classes.json` | no: from the source named |
| `delta-check-constraint` | `constraint_violation` | any | `[delta_violate_constraint_with_values]` | Delta Lake's `delta-error-classes.json` | no: from the source named |
| `spark-schema-not-found` | `missing_schema` | any | `[schema_not_found]` | Apache Spark's `error-conditions.json` | no: from the source named |
| `spark-unresolved-routine` | `missing_function` | any | `[unresolved_routine]` | Apache Spark's `error-conditions.json` | no: from the source named |
| `delta-concurrent-write` | `lock_conflict` | any | `[delta_concurrent_` | Delta Lake's `delta-error-classes.json` | no: from the source named |
| `databricks-cluster-start` | `warehouse_unavailable` | any | `error starting cluster` | dbt-databricks's source (1.12) | no: from the source named |
| `databricks-cluster-status` | `warehouse_unavailable` | any | `error getting status of cluster` | dbt-databricks's source (1.12) | no: from the source named |
| `databricks-connection` | `warehouse_unavailable` | any | `failed to create connection` | dbt-databricks's source (1.12) | no: from the source named |
| `databricks-command-timeout` | `query_timeout` | any | `command execution timed out` | dbt-databricks's source (1.12) | no: from the source named |
| `databricks-python-timeout` | `query_timeout` | any | `python model run timed out` | dbt-databricks's source (1.12) | no: from the source named |
| `databricks-oauth-required` | `credentials_missing` | any | `is required when not using access token` | dbt-databricks's source (1.12) | no: from the source named |
| `databricks-client-id-required` | `credentials_missing` | any | `is required to connect to databricks when` and `is present` | dbt-databricks's source (1.12) | no: from the source named |
<!-- patterns:end -->

Besides these, a Python exception's name given as the error's kind (`KeyError: …`)
is recognised as `python_exception` (`python-exception`). Only Python's built-in
exceptions are ever named.

## Deliberately not recognised
- An error that resembles a symptom without being it gets no pattern, or a symptom of
  its own: a missing schema is `missing_schema`, not a missing table, and a missing SQL
  function is `missing_function`, not an undefined macro. A pattern that named the wrong
  symptom would let unrelated evidence confirm the wrong cause (rule 3).
- PostgreSQL's `cannot drop … because other objects depend on it`: the summary removes
  `drop …` as SQL, so the phrase never reaches the catalogue.

An error no pattern recognises is `not_recognised`. It takes the category its kind
implies (dbt's header kind when the message's own says nothing), and ODS doesn't guess
a cause.
