# Competitive landscape: state-aware / incremental / change-driven transformation execution

> Research source (2026-09-24), kept verbatim for traceability. Paths such as `DOCS/...` or `docs/...` refer to files in the public docs repo named at the top. Tags: [V]/FACT = checked in a primary source; [S]/REPORTED = search summary; [I]/OPINION = inference. The synthesis is [`../ods-state-strategy.md`](../ods-state-strategy.md).

Research note for ODS State · compiled 2026-09-24 · clean-room (public docs, OSS READMEs, press only)

## How to read this

- **[V]**: verified. I read it in a primary source: official docs fetched as raw Markdown from the vendor's public docs repo, an OSS README or LICENSE, or Microsoft Learn for Databricks.
- **[S]**: secondary. It comes from web-search result summaries of the cited page. `docs.getdbt.com`, `getdbt.com`, `docs.dagster.io`, `getorchestra.io`, `y42.com` and `astronomer.github.io` were blocked for direct fetch. Re-check [S] items before quoting them publicly.
- **[I]**: my inference or opinion. It is not a vendor claim.

---

## 0. Corrections for the existing ODS docs (please read first)

1. **dbt State timeline.** `docs/ROADMAP.md` says "dbt State (GA Sept 2026)". dbt's own docs date dbt State to **June 1, 2026**: "If you were using state-aware orchestration prior to June 1, 2026, you can continue using it" [V] ([dbt-state-vs-sao snippet](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/snippets/_dbt-state-vs-sao.md)). Web search reports that on June 1, 2026 "dbt Labs and Fivetran announced dbt State … now generally available" [S] ([dbt blog: dbt State is GA](https://www.getdbt.com/blog/dbt-state-is-ga)). `docs/research/dbt-state-comparison.md` says "preview on 2026-06-01, GA in September". I could not confirm the September date. Treat it as unverified.
2. **dbt State needs a dbt platform account.** "It requires authentication through a dbt platform account" [V] ([dbt-state-about.md](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/docs/deploy/dbt-state-about.md)). It also sends SQL hashes and last-modified timestamps to "a single US multi-tenant (MT) instance" [V] ([FAQ data-storage](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/faqs/State/data-storage.md)). The existing comparison doc says "the dbt platform is not required". That is only true in the sense that you do not need to *run jobs* on the platform.
3. **What `lag_tolerance` means.** The reference page says: "`lag_tolerance` controls how often a node can rebuild, not how fresh its upstream data has to be". A node rebuilds only when *both* conditions hold: (a) its last build is older than the tolerance, and (b) upstream data changed since that build [V] ([lag-tolerance.md](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/reference/resource-configs/lag-tolerance.md)). The migration page words it differently: "skips the model unless upstream data is newer than the model's last run by at least the configured interval" [V] ([dbt-state-migration.md](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/docs/deploy/dbt-state-migration.md)). **dbt's own docs are ambiguous here.** ODS's `MaxStaleness` should define its semantics precisely and show them in `explain`.
4. **Industry consolidation.** Fivetran acquired Tobiko Data (SQLMesh, SQLGlot) on 2025-09-03 [S] ([Fivetran press](https://www.fivetran.com/press/fivetran-acquires-tobiko-data-to-power-the-next-generation-of-advanced-ai-ready-data-transformation)). Fivetran and dbt Labs completed an all-stock merger on 2026-06-01 [S] ([Fivetran press](https://www.fivetran.com/press/fivetran-dbt-labs-complete-merger-to-create-the-data-infrastructure-for-trusted-ai-agents)). SQLMesh was contributed to the Linux Foundation; its README now says "SQLMesh is a project of the Linux Foundation" [V] ([SQLMesh README](https://github.com/SQLMesh/sqlmesh)). **dbt State and SQLMesh/Tobiko Cloud now belong to the same company** [I]. That leaves room for a vendor-neutral OSS option.

---

## 1. Reference competitor: dbt State (dbt Labs / Fivetran)

| Aspect | Findings |
|---|---|
| Decision | For each selected node it chooses one of three actions [V] ([dbt-state-about.md](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/docs/deploy/dbt-state-about.md)):<br>• **Reuse (skip)**: the object exists, the logic is unchanged, and the node is not due under `lag_tolerance`.<br>• **Clone**: "looks across all environments and jobs for a matching object with identical logic and fresh data … clones from the one with the freshest data, regardless of which environment it came from".<br>• **Build**: otherwise, with automatic deferral. |
| Code signal | "compares rendered SQL"; it parses "the rendered SQL into a syntax tree and comparing the hash", so whitespace and comments are ignored. The opt-in `compare_unrendered_code` also compares the Jinja template, which helps with non-deterministic macros [V] ([model-change-calculation](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/faqs/State/model-change-calculation.md), [views-rebuilt](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/faqs/State/views-rebuilt.md)). |
| Data signal | Last-modified timestamps "directly from the data warehouse, for example from `INFORMATION_SCHEMA`", or `loaded_at_field`/`loaded_at_query` [V] ([last-updated-timestamp](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/faqs/State/last-updated-timestamp.md)). Freshness propagates through views [V]. |
| Conservative cases | It always rebuilds Python models and custom materializations. It rebuilds `select *` on a `ref()`/`source()`. It rebuilds BigQuery external sources that have no timestamp [V] ([views-rebuilt](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/faqs/State/views-rebuilt.md)). |
| Knobs | `lag_tolerance` (default 45m, Jinja allowed), `require_fresh_data_from: any\|all`, `compare_unrendered_code`, `evaluate_volatile_sql`, `pre_clone: never\|if_missing\|always`, `execute_hooks_on_any_reuse`, `allow_clones` (profile), `defer_to_target`, `metadata_warehouse` [V] ([dbt-state-configs.md](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/reference/resource-configs/dbt-state-configs.md)). |
| Tests | "if the nodes being tested haven't changed since the last run, the previous test result is reused" [V]. |
| Explainability | `dbt state explain` prints decision codes such as `SKIP_EXECUTION` and `READY_TO_EXECUTE`. With `--verbose` it shows a step tree: table analysis, query analysis, data-freshness analysis per upstream with reason codes (`TARGET_TABLE_EXISTS`, `NODE_QUERY_UNCHANGED`), and a run-config summary. It reads local `logs/state/responses_*.jsonl`. Some nodes come back `UNKNOWN … details unavailable` [V] ([state-explain.md](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/reference/commands/state-explain.md)). |
| State storage | SaaS, one US multi-tenant instance. It stores SQL hashes and timestamps; "No actual data … transmitted". If the servers are down, dbt "gracefully falls back to normal dbt behavior" [V] ([data-storage](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/faqs/State/data-storage.md), [server-failure](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/faqs/State/server-failure.md)). |
| Warehouses | Snowflake, Databricks, BigQuery, Redshift. Plugin for dbt 1.7–1.12; native in Fusion / dbt v2 [V] ([dbt-state-setup.md](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/docs/deploy/dbt-state-setup.md)). |
| Pricing | **USD 0.094 per "daily active target table"**: a table (or test) that State skips, clones or reuses at least once in a UTC day. There is a 30-day trial. It is not available on the legacy Starter plan [V] ([_dbt-state-pricing.md](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/snippets/_dbt-state-pricing.md), [setup](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/docs/deploy/dbt-state-setup.md)). |
| Documented weaknesses | • "No concurrent build detection": two jobs that run the same snapshot or incremental model at once "can lead to duplicate records or other data corruption".<br>• "Efficient Testing" is not yet available.<br>• Non-deterministic Jinja causes rebuild cascades [V] ([migration](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/docs/deploy/dbt-state-migration.md)).<br>• It is closed source, data leaves the tenant (metadata only), it is US-only and priced per reuse [V].<br>• [I] Pricing per reuse means that the better it works, the more you pay. |

Predecessor: **state-aware orchestration (SAO)** in dbt Fusion/platform used `freshness.build_after {count, period, updates_on}`. A SQL change *alone* did not trigger a rebuild under `build_after` [V] ([migration](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/docs/deploy/dbt-state-migration.md)).

---

## 2. SQLMesh (Tobiko Data → Fivetran → Linux Foundation)

- **Fingerprints and snapshots.** A snapshot stores the model plus "all macro definitions and global variables", and "the intervals of time for which they have data". Fingerprints come from SQLGlot-canonicalised SQL, so "superficial changes … will not return a new fingerprint" [V] ([snapshots.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/concepts/architecture/snapshots.md)).
- **Virtual data environments.** "each model variant's data is stored in a separate physical table". An environment is a set of views that point at physical tables. Promotion is a *virtual update*: the views are swapped, so there is no recompute [V] ([plans.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/concepts/plans.md)); see also [Tobiko VDE post](https://www.tobikodata.com/blog/virtual-data-environments) [S].
- **Change categories.** A **breaking** change backfills the model and everything downstream. A **non-breaking** change backfills only the model. When categories conflict, "the most conservative category (breaking) is assigned". SQLMesh auto-categorises by default; `select *` downstream is handled "best-effort" [V] ([plans.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/concepts/plans.md)).
- **Forward-only.** No new physical table and no backfill; in dev it uses a temp table or a "shallow clone of the production table". It detects destructive schema changes (`--allow-destructive-model`) and supports `--effective-from` for retroactive application [V] ([plans.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/concepts/plans.md)).
- **Intervals.** The scheduler evaluates "a model over a specific time interval", with `cron`, `batch_size` and `--min-intervals`. Restatement plans reprocess ranges and cascade downstream [V] ([plans.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/concepts/plans.md), [signals.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/guides/signals.md)).
- **Data-change awareness is weak by default.** Running is cron- and interval-driven. **Signals** are user-written Python functions that gate interval readiness (for example, late-arriving data) [V] ([signals.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/guides/signals.md)). [I] SQLMesh does *not* natively skip a FULL model because "upstream data did not change". Its model is "time has passed, so compute the next interval".
- **Column-level lineage.** CLL exists in OSS. However, *CLL-driven plan pruning* ("Unaffected Downstream", "advanced column-level impact analysis") is a **Tobiko Cloud** feature [V] ([costs_savings.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/cloud/features/costs_savings.md)). An open RFC (2026-09-12) says "SQLMesh OSS's plan algorithm is not as efficient as Tobiko Cloud" and proposes CLL-based `INDIRECT_BREAKING`/`INDIRECT_NON_BREAKING` [V] ([issue #6062](https://github.com/SQLMesh/sqlmesh/issues/6062)).
- **State.** A separate OLTP state DB with transactions. Using the warehouse for state is "not suitable for production". `sqlmesh state export/import` produces JSON with `schema_version` and `sqlglot_version`. Import may leave the DB inconsistent on engines without transactional DDL [V] ([state.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/concepts/state.md)).
- **dbt.** Native dbt adapter with a subset of Jinja methods; some features are unsupported [V] ([integrations/dbt.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/integrations/dbt.md)).
- **Cost awareness (Cloud only).** Per-model cost and savings estimates for BigQuery on-demand and Snowflake credits. Savings are split into "Prevented Reruns", "Unaffected Downstream" and "Virtual Environments" [V] ([costs_savings.md](https://github.com/SQLMesh/sqlmesh/blob/main/docs/cloud/features/costs_savings.md)). Databricks is not listed.
- **Licence and pricing.** Apache-2.0 [V] ([LICENSE](https://github.com/SQLMesh/sqlmesh/blob/main/LICENSE)). Tobiko Cloud charges a platform fee plus usage, via sales [S] ([Tobiko Cloud](https://www.tobikodata.com/tobiko-cloud)).
- **Weaknesses.**
  - Open bug: `INDIRECT_NON_BREAKING` dev snapshots repeatedly re-backfill because two ledgers disagree ("completed work never recognized") [V] ([issue #5793](https://github.com/SQLMesh/sqlmesh/issues/5793)).
  - Adopting it means moving to SQLMesh semantics [I].
  - Governance is uncertain after the Fivetran/dbt merger [I].

## 3. Dagster (declarative automation, versioning)

- **AutomationCondition.** Composable predicates evaluated by a sensor: `on_cron`, `eager`, `on_missing`, `code_version_changed`, and others. `eager` treats an *observed* upstream as updated "only … if the data version has changed", but a *materialization* counts as an update "regardless of the data version". It is partition-aware: time partitions consider the latest partition [V] ([declarative-automation](https://github.com/dagster-io/dagster/blob/master/docs/docs/guides/automate/declarative-automation/index.md)).
- **Code and data versions.** A data version is a hash of the code version plus input data versions. The UI shows "Unsynced" with the reason: code version changed, dependencies changed, or parent data version changed [V] ([asset-versioning-and-caching](https://github.com/dagster-io/dagster/blob/master/docs/docs/guides/build/assets/asset-versioning-and-caching.md)).
- **dbt.** `dagster-dbt`'s default `code_version` is dbt's manifest `checksum` [V] ([asset_utils.py](https://github.com/dagster-io/dagster/blob/master/python_modules/libraries/dagster-dbt/dagster_dbt/asset_utils.py)). [I] That checksum is a raw-file checksum, so a whitespace edit counts as a change; there is no SQL canonicalisation.
- **Freshness policies.** Time-window and cron policies with warn/fail windows. They are observability and alerting only; they do not drive skips. They supersede freshness checks as of 1.12 [V] ([asset-freshness-policies](https://github.com/dagster-io/dagster/blob/master/docs/docs/guides/observe/asset-freshness-policies.md)).
- **Licence and pricing.** Apache-2.0 [V] ([LICENSE](https://github.com/dagster-io/dagster/blob/master/LICENSE)). Dagster+ is billed per credit, where one credit is one materialization, at about $0.035–0.040 [S] ([Dagster pricing update May 2026](https://support.dagster.io/articles/3171123463-dagster-solo-and-starter-pricing-updates-may-2026)).
- **Strengths:** the most expressive policy algebra, and an evaluation tree UI. **Weaknesses:** you must adopt the Dagster runtime, and there is no warehouse clone or zero-copy reuse [I].

## 4. Orchestra: SAO and the OSS "Sao Paolo" (`dbt-orchestra`). The closest OSS analogue to ODS State

- `orc dbt run|build|test` wraps dbt Core 1.10–1.11. It "loads state, computes reusable nodes, patches clean nodes, runs dbt, updates and saves state" [V] ([sao-paolo README](https://github.com/orchestra-hq/sao-paolo)).
- **Data signal:** `dbt source freshness`, meaning `loaded_at_field`/`loaded_at_query`. Implicit fallback exists **only for Databricks, via `DESCRIBE HISTORY`**. There is an opt-in `require_explicit_source_freshness`, because metadata on views is misleading [V].
- **State:** a JSON file stored locally or on S3, GCS or ABS, or in Orchestra Cloud [V].
- **Not conservative by default:** `verify_relations_exist` defaults to **false**. Without it, a dropped relation "still looks clean in state, so it gets skipped and the run 'succeeds' with a missing relation" [V].
- **Tests:** "a test runs if *any* of its models is being built" [V].
- **Licence and pricing:** Apache-2.0 [V] ([LICENSE](https://github.com/orchestra-hq/sao-paolo/blob/main/LICENSE)). Orchestra-hosted dbt costs about $0.033/min [S] ([Orchestra blog](https://www.getorchestra.io/blog/open-sourcing-state-aware-orchestration-for-dbt)). Uses `build_after` config [S].
- [I] **Gaps relative to ODS:** Python only, a JSON blob as state, no clone or zero-copy, and no documented explain/reason-chain output.

## 5. dbt-centric platforms

- **Paradime (Bolt).** Scheduler plus "Turbo CI", which uses `state:modified+` and defer against the last prod manifest. Templates exist for "Build and Test Models with New Source Data" (the `source_status:fresher` pattern) [S] ([Paradime deferral guide](https://www.paradime.io/blog/deferral-using-dbt-a-definitive-guide), [template docs](https://docs.paradime.io/app-help/documentation/bolt/creating-schedules/templates/deferred-model-execution)). I found no native per-node skip engine [S]. Commercial ([pricing](https://www.paradime.io/pricing)).
- **Datacoves.** Managed dbt Core and Airflow. Slim CI via the prod manifest plus `--defer`; it stores artifacts through a "dbt-api" [S] ([Datacoves Slim CI](https://datacoves.com/post/dbt-slim-ci)). Commercial.
- **Y42.** "Virtual Data Builds": every materialization is tied to a Git commit, and a customer-facing layer of pointers enables "zero-copy deployments and rollbacks" [S] ([Y42 VDB docs](https://www.y42.com/docs/branch-environments/virtual-data-builds), [blog](https://www.y42.com/blog/virtual-data-builds-one-data-warehouse-environment-for-every-git-commit)). This is conceptually the same as SQLMesh VDEs. The company still appears to be active [S] ([Crunchbase](https://www.crunchbase.com/organization/y42)). Commercial and closed.
- **Coalesce.** Visual, column-aware builder for Snowflake and Databricks, with column-level lineage built in and incremental node types. Pricing is custom quote [S] ([Coalesce incremental docs](https://docs.coalesce.io/docs/guides/using-incremental-nodes), [review](https://datatoolindex.com/tools/coalesce/)). I found no data-change-driven skip engine [S].
- **Google Dataform.** Manual "include dependents/dependencies" selection and incremental tables. I found no state or skip engine [S] ([Dataform schedule runs](https://docs.cloud.google.com/dataform/docs/schedule-runs)).
- **Astronomer Cosmos** (Apache-2.0, Airflow). Caches `dbt ls`. `ExecutionMode.WATCHER` (stable in 1.15.0, 2026-07-01) runs `dbt source freshness` first, and a `freshness_callback` decides which dependent nodes to skip [V] ([CHANGELOG](https://github.com/astronomer/astronomer-cosmos/blob/main/CHANGELOG.rst)); [S] ([phData](https://www.phdata.io/blog/how-to-achieve-source-freshness-and-stateful-selection-with-cosmos-api-dbt-core/)). It has no code-fingerprint state.

## 6. Warehouse-native incremental maintenance

- **Databricks MVs / Lakeflow Spark Declarative Pipelines (Enzyme).**
  - Incremental refresh runs only on serverless. It needs Delta sources with **row tracking**, and CDF is "recommended".
  - A **cost model** chooses between incremental and full refresh. `REFRESH POLICY AUTO | INCREMENTAL | INCREMENTAL STRICT | FULL` controls this.
  - The chosen technique (`NO_OP`, `ROW_BASED`, `PARTITION_OVERWRITE`, `APPEND_ONLY`, `GROUP_AGGREGATE`, `FULL_RECOMPUTE`, …) is logged in the pipeline event log as `planning_information`.
  - `EXPLAIN CREATE MATERIALIZED VIEW` shows whether a query can be incrementalised.
  - On classic compute an MV is always fully recomputed.

  Sources [V]: [MS Learn: incremental refresh](https://learn.microsoft.com/azure/databricks/ldp/incremental-refresh), [standalone MVs](https://learn.microsoft.com/azure/databricks/ldp/dbsql/materialized), [refresh semantics](https://learn.microsoft.com/azure/databricks/ldp/concepts/refresh). The core was donated to Apache Spark 4.1 as Spark Declarative Pipelines [S] ([Databricks blog](https://www.databricks.com/blog/bringing-declarative-pipelines-apache-spark-open-source-project)). dbt-databricks supports `materialized_view` and `streaming_table` materializations [V] ([databricks-configs.md](https://github.com/dbt-labs/docs.getdbt.com/blob/current/website/docs/reference/resource-configs/databricks-configs.md)).
- **Delta primitives ODS can use.**
  - `table_changes(table, start[, end])` returns `_change_type`, `_commit_version` and `_commit_timestamp`.
  - `DESCRIBE HISTORY` keeps 30 days of history by default.
  - `SHALLOW CLONE` is supported on UC managed tables from DBR 13.3 (Public Preview). Limits: no `CREATE OR REPLACE` over a shallow clone, no nesting, MVs and streaming tables cannot be cloned, and history is not copied.

  Sources [V]: [table_changes](https://learn.microsoft.com/azure/databricks/sql/language-manual/functions/table_changes), [DESCRIBE HISTORY](https://learn.microsoft.com/azure/databricks/sql/language-manual/delta-describe-history), [UC shallow clone](https://learn.microsoft.com/azure/databricks/tables/operations/clone-unity-catalog), [CLONE](https://learn.microsoft.com/azure/databricks/sql/language-manual/delta-clone).
- **Snowflake Dynamic Tables.**
  - `TARGET_LAG` (or `DOWNSTREAM`) sets a staleness target.
  - Refresh modes are `AUTO`, `INCREMENTAL`, `FULL` and `ADAPTIVE` [S] ([target lag](https://docs.snowflake.com/en/user-guide/dynamic-tables/target-lag), [refresh modes](https://docs.snowflake.com/en/user-guide/dynamic-tables/refresh-modes)).
  - `DYNAMIC_TABLE_REFRESH_HISTORY.refresh_action` is one of `NO_DATA`, `INCREMENTAL`, `FULL` or `REINITIALIZE`, with a `REINIT_REASON` [S] ([refresh history](https://docs.snowflake.com/en/sql-reference/account-usage/dynamic_table_refresh_history)).
- **BigQuery materialized views.** Incremental by default, but only for a limited SQL subset. `max_staleness` bounds staleness. "Smart tuning" rewrites queries to use the MV. There is a 3-day delta-retention pitfall [S] ([create MVs](https://docs.cloud.google.com/bigquery/docs/materialized-views-create), [use MVs](https://docs.cloud.google.com/bigquery/docs/materialized-views-use)).
- [I] **Takeaway.** Warehouses now solve *within-object* incrementality: how to refresh one MV cheaply. They do not solve *cross-object, cross-environment* decisions: whether to run this dbt node at all, whether to reuse the CI table, or why. The pattern is the same everywhere: a declared staleness target, a cost-based choice between incremental and full refresh, and a logged "refresh action". ODS should treat these as *evidence sources*: a `NO_OP` or `NO_DATA` refresh action means "no change".

## 7. IVM engines (adjacent, not direct competitors)

- **Materialize.** Differential and timely dataflow, strict-serializable. Licensed under **BSL** [V] ([LICENSE](https://github.com/MaterializeInc/materialize/blob/main/LICENSE)).
- **RisingWave.** Postgres-compatible streaming DB. **Apache-2.0** [V] ([LICENSE](https://github.com/risingwavelabs/risingwave/blob/main/LICENSE)).
- **Feldera.** DBSP in Rust: "evaluate arbitrary SQL programs incrementally". Open-source edition is **MIT**; an Enterprise edition exists [V] ([README](https://github.com/feldera/feldera), [LICENSE](https://github.com/feldera/feldera/blob/main/LICENSE)).
- **Epsio.** Incremental views on Postgres, MySQL and MSSQL. Commercial [S] ([Epsio FAQ](https://docs.epsio.io/faq/)).
- [I] These engines replace batch dbt runs rather than decide them. The idea worth borrowing is "cost is proportional to the change rate, not the data size", which also applies to *planning* cost. They are relevant to ODS only as possible future `Executor` targets.

## 8. Build-system analogies (Bazel, Nix, Turborepo)

- **Bazel.** The remote cache is split into an **action cache** (action digest → result) and a **CAS** of output blobs. The action digest hashes the command, arguments, environment and input digests with SHA-256 by default. Unexpected misses are debugged by diffing execution logs for "non-hermetic action inputs" [S] ([Bazel remote caching](https://bazel.build/remote/caching), [BuildBuddy explainer](https://www.buildbuddy.io/blog/bazels-remote-caching-and-remote-execution-explained/)).
- **Turborepo and Nx.** Task-level input hashing with a remote cache shared across CI and developers. Correctness depends on declaring inputs and outputs correctly [S] (same sources).
- **Ideas that transfer to ODS** [I]:
  1. **Action key = hash(canonical SQL, relevant config, adapter/engine version, upstream data fingerprints).** A matching key means the output already exists somewhere, so it can be reused or cloned. This is exactly what dbt State's cross-environment clone does.
  2. **Hermeticity audit.** Flag non-hermetic inputs such as `current_timestamp()`, `env_var`, `run_started_at`, non-deterministic macros and `select *`. Each flag lowers reuse *confidence* and is reported as a reason. dbt State only has the `evaluate_volatile_sql` and `compare_unrendered_code` toggles.
  3. **Cache-miss explanation.** Bazel's execution-log diff is the equivalent of `ods state why`: show which input digest changed.
  4. **Content-addressed physical tables.** Name physical tables by fingerprint, as SQLMesh and Y42 do. This turns promotion into a pointer swap.

## 9. Data diff, CI and git-for-data

- **Datafold.** OSS `data-diff` was archived on 2024-05-17. Cloud offers diff, column-level lineage and monitors [S] ([sunsetting post](https://www.datafold.com/blog/sunsetting-open-source-data-diff/)).
- **Recce** (Apache-2.0 [V], [LICENSE](https://github.com/DataRecce/recce/blob/main/LICENSE)). Lineage diff, schema diff, row-count diff, value and profile diff for dbt PR review [S] ([lineage diff docs](https://docs.reccehq.com/3-visualized-change/lineage/)).
- **Parrant** (MIT [V]). A newer OSS "change-impact decision engine for dbt". It does column-level blast radius from `manifest.json` and `catalog.json` via SQLGlot, "never connects to your warehouse", and gives a policy-gated PR verdict [V] ([README](https://github.com/Fszta/dbt-column-lineage)).
- **Bauplan.** Git-for-data over Iceberg and Nessie, with branch → run → atomic merge. Moved SQL execution to DataFusion [S] ([Bauplan docs](https://docs.bauplanlabs.com/), [year in review](https://bauplanlabs.com/post/bauplan-a-year-in-review)).
- **lakeFS.** Git-like branches over object storage. Licence is now **BSL 1.1** (per its LICENSE at 1.87.0) [V] ([LICENSE](https://github.com/treeverse/lakeFS/blob/master/LICENSE)).
- **Nessie** (Apache-2.0 [V]) and **Iceberg branches/tags.** Catalog-level or table-level branching that enables write-audit-publish (WAP) [S] ([Iceberg branching](https://iceberg.apache.org/docs/latest/branching/)).
- [I] **Takeaway.** These tools answer "what *changed*, and is it safe?" at PR time. None of them feed that answer into the *run* decision. Column-level impact and data-diff evidence could become inputs to the ODS planner, for example "the diff shows no value change, so downstream may REUSE".

## 10. Other 2025–2026 signals

- **dbt v2.0 alpha (2026-06-01).** Rust runtime shared with Fusion, Apache-2.0 [S] ([Datacoves on Fusion/2.0](https://datacoves.com/post/dbt-fusion)). dbt State is native to it [V].
- **OpenLineage.** Its run/job/dataset model carries facets for schema, stats and **version** [S] ([object model](https://openlineage.io/docs/spec/object-model/)). [I] Nobody seems to use OpenLineage `DatasetVersion` facets as *change evidence for skip decisions*. ODS could both consume and emit them.

---

## 11. Capability matrix

Legend: ● strong/native · ◐ partial or opt-in · ○ absent · $ paid tier only · ? unclear. Evidence is in the sections above.

| Capability | dbt State | SQLMesh OSS | Tobiko Cloud | Dagster | Orchestra Sao Paolo | Cosmos | Y42 | Paradime / Datacoves | Databricks MV/LSDP | Snowflake DT | BigQuery MV | Recce / Parrant | **ODS (planned)** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Code fingerprint (semantic, not raw text) | ● AST hash | ● SQLGlot canonical | ● | ◐ manifest checksum | ◐ ? | ○ | ◐ git commit | ◐ `state:modified` | ● (query def) | ● | ● | ● (diff) | ● #13 |
| Upstream data-change detection | ● metadata timestamps | ◐ signals (user code) | ◐ | ◐ data versions / observations | ● source freshness; DESCRIBE HISTORY on Databricks | ◐ freshness callback | ? | ◐ `source_status:fresher` | ● row tracking / CDF | ● | ● | ○ | ● #16 (freshness + Delta versions) |
| Freshness through views | ● | n/a | n/a | ◐ | ○ ? | ○ | ? | ○ | n/a | n/a | n/a | ○ | ● #18 |
| Staleness tolerance / SLA | ● `lag_tolerance` | ◐ cron | ◐ | ● freshness policy (alerts) + conditions | ● `build_after` | ○ | ? | ○ | ◐ schedule | ● `TARGET_LAG` | ● `max_staleness` | ○ | ● `MaxStaleness` #19 |
| Column-level impact pruning | ○ | ○ (RFC open) | ●$ | ○ | ○ | ○ | ○ | ○ | ● internal | ● internal | ● internal | ● analysis only | ◐ #31 research |
| Breaking/non-breaking categorisation | ○ | ● | ● | ○ | ○ | ○ | ○ | ○ | n/a | n/a | n/a | ● verdict | ○ (idea) |
| Cross-env reuse / zero-copy clone | ● clone from freshest env | ● VDE pointer swap | ● | ○ | ○ | ○ | ● pointer layer | ◐ defer only | ◐ (shallow clone primitive) | ◐ | ○ | ○ | ◐ M2 #29/#30 |
| Partition / interval awareness | ○ | ● intervals | ● | ● partitions | ○ | ○ | ? | ○ | ● partition overwrite | ● | ◐ | ○ | ◐ M2 #17 |
| Explainability (per-node reasons) | ● `state explain` with codes | ◐ plan diff | ● | ● evaluation tree | ◐ log lines | ○ | ? | ○ | ◐ event log technique | ◐ `refresh_action` | ○ | ● | ● explain/why/diff, JSON |
| Open state format, self-hosted | ○ SaaS, US-only | ● own DB + JSON export | ◐ | ● own DB | ● JSON on disk or object store | ○ | ○ | ○ | ○ | ○ | ○ | n/a | ● SQLite/PG + versioned schema |
| Failed run cannot corrupt state | ? (concurrency can corrupt data) | ◐ transactional state DB | ◐ | ● event log | ? | n/a | ? | n/a | ● | ● | ● | n/a | ● (rule 5) |
| Concurrency safety | ○ documented gap | ◐ | ◐ | ● run queue | ○ | ○ | ? | ○ | ● | ● | ● | n/a | ◐ M2 #28 leases/fencing |
| Cost awareness | ◐ (savings UI) | ○ | ●$ (Snowflake/BQ only) | ◐ Insights$ | ○ | ○ | ? | ○ | ● internal cost model | ● adaptive | ◐ | ○ | ○ (gap → idea) |
| Multi-warehouse | ● 4 | ● many | ● | ● | ● via dbt | ● via dbt | ◐ | ● via dbt | ○ | ○ | ○ | ● | ◐ (Databricks first, provider-neutral) |
| Works with unmodified dbt | ● | ◐ (adapter subset) | ◐ | ● | ● | ● | ◐ | ● | n/a | n/a | n/a | ● | ● |
| OSS licence | ○ proprietary | ● Apache-2.0 (LF) | ○ | ● Apache-2.0 | ● Apache-2.0 | ● Apache-2.0 | ○ | ○ | ◐ SDP core Apache | ○ | ○ | ● Apache/MIT | ● |
| Pricing | $0.094 per reused table per day | free | platform + usage | per materialization ($) | free; hosted $/min | free | ? | seat/plan | DBU (serverless) | credits | slots/bytes | free/cloud | free |

---

## 12. White space (what nobody covers well) [I, except where cited]

1. **An open, self-hosted, auditable decision engine for *unmodified* dbt.** dbt State is SaaS, US-only and per-reuse priced. Sao Paolo is OSS but thin: JSON-blob state, no explain, unsafe defaults. SQLMesh needs a framework switch, and its best pruning is Cloud-only. There is room for a single binary with local SQLite state, a documented format and air-gapped operation.
2. **Explainability as a first-class, machine-readable contract.** dbt State's explain works from local JSONL logs and can return `UNKNOWN … details unavailable` [V]. Nobody publishes a *stable JSON schema* for decisions and evidence that CI, bots and agents can consume. Nobody offers counterfactuals ("would rebuild if X") or diffs of decisions between two runs.
3. **Confidence-graded evidence.** Every tool treats evidence as a boolean. None records *how* a freshness claim was obtained: a Delta commit version (exact), a `loaded_at` column (semantic), `INFORMATION_SCHEMA` last-altered (proxy, wrong for views; see the [Sao Paolo README](https://github.com/orchestra-hq/sao-paolo)), or none. None then downgrades to BUILD when evidence is weak. ODS's "inferred vs fact" rule fits this gap exactly.
4. **Exact data fingerprints on lakehouse formats.** Delta and Iceberg expose monotonic commit versions and CDF. Tools still mostly compare *timestamps*. dbt State uses last-modified metadata [V]; Sao Paolo uses `DESCRIBE HISTORY` timestamps [V]. Keying reuse on `(table_id, commit_version)` is exact, hermetic, and immune to clock skew.
5. **Safety for concurrent runs and failures.** dbt State documents possible "duplicate records or other data corruption" from overlapping jobs [V]. Leases, fencing tokens and "replace canonical state only on success" are open ground (ODS rule 5 and #28).
6. **Using warehouse-native IVM signals as evidence.** Databricks `NO_OP` planning events, Snowflake `refresh_action = NO_DATA`, and MV/streaming-table refresh metadata are not used by any dbt-level planner. A dbt MV node whose last refresh was `NO_OP` provably has unchanged output. Skip its downstream nodes.
7. **Column-level pruning in the open.** It exists only in Tobiko Cloud ($) and warehouse internals. The OSS RFC is still open [V]. Parrant and Recce show that artifact-only CLL is feasible without a warehouse connection.
8. **Cost-aware planning on Databricks.** Tobiko's cost view covers only Snowflake and BigQuery [V]. Nobody attaches estimated DBU or $ saved to each decision, or uses cost to choose between CLONE, REUSE and incremental BUILD.
9. **A test-result reuse policy that is explicit and safe.** dbt State reuses test results [V]; Sao Paolo re-runs tests if any parent is built [V]. Neither exposes the policy with reasons.

## 13. Concrete ideas for ODS (adopt / leapfrog)

**Adopt (table stakes against dbt State):**
- **A1.** Canonical-AST code fingerprint over *rendered* SQL, plus an optional unrendered-template fingerprint, plus a "relevant config" subset (#13). Ignore comments and whitespace. Record the canonicaliser version inside the fingerprint so upgrades invalidate deliberately.
- **A2.** Three-way decision ladder REUSE → CLONE → BUILD, always falling back to BUILD. Include `allow_clones=false` per target and `require_fresh_data_from any|all` (M1/M2 #19, #29).
- **A3.** Carry freshness through views (#18). Rebuild a view on `select *` over `ref`/`source`, as dbt does, but *explain* why.
- **A4.** A verified-relation-exists check **on by default**. This is the opposite of Sao Paolo's default [V]. Missing evidence means BUILD.
- **A5.** Explicit, documented `MaxStaleness` semantics. Explain which clock is used: the model's last build or upstream data time. dbt's docs are ambiguous (§0.3).

**Leapfrog:**
- **L1. Evidence ledger with confidence levels.** Each decision stores `evidence[]` entries as `{source: delta_commit | cdf | loaded_at | info_schema | none, value, exactness: exact | semantic | proxy | inferred}`. Policy: a proxy on a view means BUILD, and a proxy is never enough for REUSE unless the user opts in. Render the ledger in `explain` as text and JSON (ADR-worthy, rule 4).
- **L2. Delta commit-version fingerprints.** Use `DESCRIBE HISTORY` and `table_changes` versions for data fingerprints [V]. Treat the CDF `_change_type` mix as extra evidence: an insert-only delta allows an append-safe incremental build. This is exact where competitors use timestamps.
- **L3. Content-addressed action key and cross-environment reuse**, in the style of Bazel: `key = H(code_fp, config_fp, engine_fp, upstream data fps)`. Keep an index `key → physical relation(s)` in the state store. A key hit in CI, dev or prod lets ODS CLONE with a Databricks shallow clone, subject to UC limits: no `CREATE OR REPLACE`, no nesting [V]. Offer this without SaaS.
- **L4. `ods state why --diff <run_a> <run_b>`.** The equivalent of Bazel's execution-log diff: which input digest changed. Add counterfactuals ("would REUSE if tolerance ≥ 2h").
- **L5. Ingest warehouse IVM outcomes as evidence.** Read Databricks `planning_information` (`NO_OP`) [V] and Snowflake `refresh_action` [S] through capability-gated providers. Keep vendor names out of core (rule 1).
- **L6. Cost annotations per decision** from Databricks system billing tables, when available through a capability. Report estimated compute avoided as *inferred*. Unlike dbt State, charge nothing per reuse.
- **L7. Artifact-only column-level impact (#31).** Use a SQL parser over `manifest.json` and `catalog.json`, as Parrant does. Classify changes as breaking or non-breaking in the SQLMesh style, conservative on conflict [V]. Start as *advice* in `plan`, and only later as a REUSE input.
- **L8. Concurrency safety as a headline.** Leases and fencing on the state store (#28). Refuse overlapping incremental or snapshot builds of the same node. This directly addresses dbt State's documented corruption risk [V].
- **L9. OpenLineage interop.** Emit decisions and dataset versions as OpenLineage facets. Consume `DatasetVersion` facets from other tools as change evidence [I].
- **L10. Test-result reuse policy** as an explicit, explained policy keyed on the test's code fingerprint plus parents' data fingerprints. Default to re-running when any parent was built, matching the Sao Paolo semantics [V].

**Positioning line** [I]: *"dbt State's decisions, open-source and self-hosted, with exact lakehouse evidence, auditable reasons, and no per-reuse fee."*

## 14. Open questions and limits of this research

- I could not read the dbt GA blog, the Orchestra, Y42 and Snowflake docs, or the Dagster docs pages directly; the corresponding items are marked [S]. dbt, Dagster, SQLMesh, Sao Paolo and Cosmos were verified through their GitHub-hosted docs.
- Not verified: the dbt State GA date (June vs September 2026); Y42's current product scope; Paradime Bolt internals; whether Coalesce has any data-change skip.
- The dbt State decision-tree image (`run-cache-decision-tree.png`) was not inspected.
