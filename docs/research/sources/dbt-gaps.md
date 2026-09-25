# Where dbt falls short on state-aware execution: research for the ODS State module

> Research source (2026-09-24), kept verbatim for traceability. Paths such as `DOCS/...` or `docs/...` refer to files in the public docs repo named at the top. Tags: [V]/FACT = checked in a primary source; [S]/REPORTED = search summary; [I]/OPINION = inference. The synthesis is [`../ods-state-strategy.md`](../ods-state-strategy.md).

Researched 2026-09-24. Legend:
- **[FACT]**: checked against a primary source: dbt docs source, dbt, dbt-databricks or dbt-state code, PyPI metadata, or Databricks docs.
- **[REPORTED]**: comes from a third-party post, an issue report or a search-result snippet that I couldn't open.
- **[OPINION/INFERENCE]**: my analysis, or community sentiment.

Local sources:
- `DOCS` = the `website/` directory of the public [dbt-labs/docs.getdbt.com](https://github.com/dbt-labs/docs.getdbt.com) repo.
- `DBTSTATE` = `dbt_state/` in the unpacked `dbt-state==2.52.0` wheel from PyPI. It is Apache-2.0 and "Copyright 2026 Fivetran, Inc."
- `DBX` = a checkout of databricks/dbt-databricks `main`. It is Apache-2.0.


> Clean-room note for ODS (rule 8): `dbt-state`, `query-cache-common` and `query-cache-protobuf` on PyPI are all **Apache-2.0**. Reading them is permitted. However, the *decision engine* runs server-side at `api.state.dbt.com` and is **closed**. ODS should not copy code verbatim, and it should not build a client for dbt Labs' proprietary service. The findings below describe behaviour. They are not code to reuse.

---

## 0. The landscape as of September 2026 [FACT]

- **State-aware orchestration (SAO)**
  - Ran only on dbt platform (Fusion), Enterprise plan, in private preview. Deploy jobs only; CI and merge jobs weren't supported. SQL models only. (`DOCS/docs/docs/deploy/state-aware-about.md`, `state-aware-setup.md:40-45`)
  - Replaced by **dbt State** on 2026-06-01, the date the Fivetran and dbt Labs merger closed. (`DOCS/docs/faqs/Runs/what-happened-to-sao.md`)
- **What dbt State is**
  - A **separate, usage-based paid service**. It works with dbt Core 1.7–1.12 through the `pip install dbt-state` plugin, which the dbt-state repo README says is bundled from 1.12. It also works with dbt v2 ("dbt", formerly Fusion) and with "dbt OSS" (formerly dbt Core v2).
  - It requires a dbt platform account and login.
  - It supports Snowflake, Databricks, BigQuery and Redshift. Postgres also has an extension class in the client (`DBTSTATE/adapters/__init__.py`). (`DOCS/docs/docs/deploy/dbt-state-setup.md:17-21`)
- **Distributions after dbt v2 went GA on 2026-09-16**
  - "dbt" (formerly Fusion) is a binary under a *dbt product license*. It contains proprietary code, is free to use, and has paid features that unlock after login.
  - "dbt OSS" is 100% Apache-2.0 and is built from `github.com/dbt-labs/dbt` (the dbt-core repo was renamed; `main` is now the Rust v2 code and v1 lives on `1.latest`).
  - dbt Core 1.13 will be the last v1 minor release. It gets critical patches for about 3–5 years.
  - Sources: `DOCS/blog/2026-09-16-comparing-dbt-and-dbt-oss.md`, `DOCS/blog/2026-09-16-dbt-v2-is-ga.md`, and the repo README at https://raw.githubusercontent.com/dbt-labs/dbt/main/README.md.
- **Licensing history**
  - Fusion launched on 2025-05-28 under ELv2.
  - In June 2026 the Rust "runtime" was relicensed to Apache-2.0 as dbt Core v2, and the Fusion binary moved to a licence "more permissive than ELv2" that allows managed-service use. Operators must still let end users enable premium features.
  - Source: `DOCS/blog/2026-06-01-dbt-core-v2-is-here.md:28-80`.
  - Mesh and Catalog (seat-based) and dbt State (usage-based) remain paid. Mesh and Catalog are **not** available in dbt OSS; dbt State is available in both distributions. (Comparison table in `comparing-dbt-and-dbt-oss.md`.)

---

## 1. Gaps and limitations of dbt State and SAO

### 1.1 Vendor lock-in, login and paid hosting [FACT]
- **A dbt platform account is required.**
  - Interactive use goes through `dbt login` in a browser.
  - Non-interactive use needs `DBT_CLOUD_TOKEN`, `DBT_CLOUD_ACCOUNT_HOST` and `DBT_CLOUD_ACCOUNT_ID` (a service token with at least Job Runner), or OAuth client credentials. The standalone app at app.state.dbt.com is being retired.
  - If CI has no credentials, "dbt State disables itself and displays a warning". (`DOCS/docs/docs/deploy/dbt-state-cicd.md`)
- **Pricing**
  - USD **$0.094 per Daily Active Target Table (DATT)**. A DATT is a distinct model, seed, snapshot *or each distinct test* that is skipped, cloned or has its test result reused on a UTC day. (`DOCS/snippets/_dbt-state-pricing.md`)
  - The trial is 30 days, can't be paused, and then needs a credit card or contract. It isn't available on the legacy Starter plan. (`_dbt-state-trial-how-it-works.md`, `dbt-state-setup.md:21`)
  - [INFERENCE] A project of 500 models and 1,500 tests that is fully reused daily comes to 2,000 × 30 × $0.094 ≈ **$5,640/month**.
  - Billing scales with how much gets *reused*, so cost grows with the very savings the product promises.
  - The dbt blog warns that "a model inside its lag tolerance window will still be counted as a DATT if you select it", so it recommends still using selectors in development. (`DOCS/blog/2026-06-17-how-dbt-state-works.md`)
- **The client is open, the brain is not.**
  - The client is Apache-2.0, but decisions come from a gRPC service at `api.state.dbt.com:443`, with authentication at `auth.state.dbt.com`. (`DBTSTATE/grpc/client.py:59,146`, `DBTSTATE/auth/sso.py:35`)
  - The decision logic and history store are proprietary, and there is **no self-hosted option**.

### 1.2 Closed state format and data residency [FACT]
- **What is sent**
  - Docs: last-modified timestamps and SQL statement hashes. The FAQ says "only the hash is persisted".
  - The service runs in "a **single US multi-tenant instance**". (`DOCS/docs/faqs/State/data-storage.md`)
- **What the client actually sends**
  - Per node, it sends `SubmitEnrichedSQLRequest` containing the **full compiled SQL text**, fully qualified target and upstream table names with last-modified epochs, dialect, default catalog, labels, microbatch windows and persisted-docs "semantic extras". (`DBTSTATE/run_cache.py:1487-1500,1597-1610`)
  - The blog confirms the query text is sent and is "discarded after the server finishes its work". (`how-dbt-state-works.md`)
  - Headers carry a persistent `system_user_id` UUID and the OS name. (`DBTSTATE/grpc/interceptors.py:281-285`, `system_info.py`)
  - The git branch is read locally into environment variables. (`DBTSTATE/plugin.py:435-446`)
- **[INFERENCE]** Compiled SQL often contains business logic, literal values such as customer IDs in filters, and PII column names. For EU, regulated or air-gapped teams, a US-only multi-tenant processor is a blocker.
- **The service tells your warehouse what to run.** For clones, the server returns `clone_sqls`, and the client executes them with *your* warehouse credentials. (`DBTSTATE/run_cache.py:1036-1044`, `dev_cloner.py:102-118`)
  - [OPINION] This is a meaningful trust boundary. A remote SaaS emits DDL that runs in prod.
- The state format isn't documented or exportable. Users can see only the `logs/state/responses_*.jsonl` decision logs (`reference/commands/state-explain.md`) and the platform Explain tab.

### 1.3 Offline and air-gapped use [FACT + INFERENCE]
- Not possible: every decision needs a round trip to api.state.dbt.com.
- On a server outage, "dbt gracefully falls back to normal dbt behavior" (`faqs/State/server-failure.md`). That means a **full rebuild**, which can cause a surprise compute spike during a vendor incident. [INFERENCE]
- In dbt v2, adapter drivers download on demand from dbt's CDN. Air-gapped installs need `pip install dbt-<adapter>`. (`comparing-dbt-and-dbt-oss.md`)

### 1.4 Adapter coverage and Databricks specifics [FACT unless marked]
- **Adapter coverage**
  - Extensions exist for postgres, bigquery, snowflake, databricks and redshift only. Any other adapter raises `AdapterExtensionError`. (`DBTSTATE/adapters/__init__.py`)
  - Not covered: dbt-spark (non-UC Spark), Fabric, Trino, Athena, DuckDB and others.
- **Databricks freshness detection** (`DBTSTATE/adapters/databricks.py`)
  - **Tables:** `DESCRIBE DETAIL <fqn>` → `lastModified`, **one statement per table**. It "can't be used as a subquery" and is run in parallel on a thread pool of `min(32, cpu+4)` threads.
    - [INFERENCE] With thousands of upstream relations this adds many metadata round trips on the SQL warehouse at the start of every run.
  - **Views:** `information_schema.tables.last_altered`, batched per catalog. The code comment explains why tables don't use it: `last_altered` "only shows when the table *schema* was last altered, not the data", citing the Databricks KB article https://kb.databricks.com/unity-catalog/last_altered-column-in-information_schema-not-reflecting-data-modifications.
    - The official column doc says "Timestamp when the relation definition was last altered in any way" (https://learn.microsoft.com/azure/databricks/sql/language-manual/information-schema/tables).
  - **Views without a `loaded_at_field`:** State "will traverse further upstream until it finds a real table" (`how-dbt-state-works.md`). View definitions come from `information_schema.views`. If a view has unqualified references, the client runs an extra `DESCRIBE EXTENDED`.
  - **[INFERENCE, unverified]** `DESCRIBE DETAIL.lastModified` changes on *any* Delta commit, including OPTIMIZE, ZORDER and liquid-clustering maintenance, predictive optimization, and property changes. Those would read as "new data" and cause **extra rebuilds**: conservative, but they cost money.
    - This matters because predictive optimization is enabled by default on many UC accounts, and it runs on its own schedule.
    - The better signal is `DESCRIBE HISTORY` filtered to data-changing operations (WRITE, MERGE, DELETE, UPDATE, STREAMING UPDATE, …), or Delta change data feed or row-tracking versions.
- **dbt-databricks' own metadata-based source freshness is weaker** (`DBX/dbt/include/databricks/macros/adapters/metadata.sql:62-95`)
  - For UC it uses `system.information_schema.tables.last_altered`. Per the KB above, that may *not* reflect DML, so a source can look stale when it isn't (false-stale) or fresh when it isn't.
  - Only Hive metastore uses `max(timestamp) FROM (DESCRIBE HISTORY …)`, and that includes OPTIMIZE and VACUUM commits.
  - [REPORTED] `ALTER TABLE … REFRESH` on external tables bumps `last_altered` without new data.
- **Clone semantics on Databricks vs Snowflake**
  - **Databricks** clones are always **SHALLOW CLONE**. (`DBX/.../materializations/clone/strategies.sql`, `clone.sql`)
    - UC can't `CREATE OR REPLACE` over an existing shallow clone, so dbt-databricks **drops and then creates**. That is not atomic: there is a window where the relation doesn't exist. A shallow clone also has to be dropped before an incremental or table rebuild. (`DBX/.../incremental/incremental.sql:47,122`, `table.sql:28`)
    - Delta history isn't cloned, so time travel on the clone starts fresh.
    - Shallow clone works for Delta only, **not Iceberg** (including managed Iceberg).
    - A clone of a clone is not allowed. dbt-state sets `CLONE_CHAIN_DEPTH_LIMIT = 1` for Databricks (BigQuery is 3; Snowflake has no limit). (`DBTSTATE/adapters/databricks.py:40`, `bigquery.py:23`)
    - Managed-to-managed and external-to-external only.
    - Clones of a **dropped base table** keep working only until the base files are purged: 7 days by default, configurable 0–30. `DROP … FORCE` breaks them.
    - VACUUM with UC is clone-aware, but for managed tables "VACUUM on either the source or target … might delete data files from the source table".
    - Source for these points: https://learn.microsoft.com/azure/databricks/delta/clone-unity-catalog.
    - **Snowflake**, by contrast, uses zero-copy clones. These are independent metadata copies, and dropping the source doesn't break them.
  - **[INFERENCE, high importance for ODS]** dbt State's `allow_clones` **defaults to true** and will "clone a dev table into prod when the data and logic match" (`reference/resource-configs/allow-clones.md`).
    - On Databricks, a prod table would then be a shallow clone whose data files belong to a *developer's* table.
    - If that dev schema is dropped (a common cleanup), prod keeps reading only until the files are purged, then fails. Ownership, lineage and audit provenance are also muddied.
    - dbt's own docs concede that clone eligibility is judged from **modification timestamps, not contents**, so tables edited manually outside dbt can be cloned incorrectly.
  - **Time-travel cloning** ("fresh upstream data can still be cloned from time travel" in the decision tree) is supported **only on BigQuery**. The same limit covers `clone_time_travel_limit` (`SUPPORTED_DIALECT_TIME_TRAVEL_DEFAULTS = {"bigquery": 604800}` in query-cache-common 2.3.0 `constants.py:14`). Databricks and Snowflake don't get it.
  - Dev pre-clone of incrementals and snapshots is on by default (`pre_clone: if_missing`, `reference/resource-configs/pre-clone.md`). On Databricks, the dev incremental therefore becomes a shallow clone of prod. The next incremental run writes into the clone; the prod files themselves are safe. [INFERENCE]
- **Unsupported node types**
  - Python models are always rebuilt; they're common on Databricks.
  - Custom materializations are always rebuilt. (`dbt-state-about.md`)
  - [OPEN QUESTION] It's unclear whether the dbt-databricks materializations `materialized_view` and `streaming_table` count as "custom". The docs don't say.

### 1.5 Freshness detection accuracy [FACT]
- Freshness defaults to warehouse metadata, with `loaded_at_field` or `loaded_at_query` as an opt-in. Views are resolved by walking upstream to tables. (`how-dbt-state-works.md`)
- **`select *` from a `ref()` or `source()` in a view always rebuilds**, because columns can't be resolved without querying.
- On BigQuery, external sources always rebuild. (`faqs/State/views-rebuilt.md`)
- Non-deterministic Jinja (for example `get_relations_by_pattern`+`union_relations` ordering, or `env_var('AIRFLOW_RUN_ID')`) causes a rebuild on every run, which cascades downstream. The workaround is `compare_unrendered_code: true`.
- When an incremental model moves from its first full load to incremental, its compiled SQL changes. That forces **all downstream models to rebuild**, regardless of `lag_tolerance`. (`reference/resource-configs/lag-tolerance.md`)
- Bug: freshness hydration ignores `freshness.filter` and **runs serially**. On BigQuery that adds 20–135 s per source and 10+ minutes before any model runs. Status: open. https://github.com/dbt-labs/dbt/issues/15796
- **Seeds:** classic `state:modified` only hashes seeds under 1 MiB; larger seeds are compared by path only (`reference/node-selection/state-comparison-caveats.md`). dbt State claims it doesn't have this limit.

### 1.6 Risky defaults: non-conservative by design [FACT; the risk framing is INFERENCE]
| Default | Behaviour | Risk |
|---|---|---|
| `lag_tolerance: 45m` | Rebuild only when the last build is more than 45 minutes old *and* upstream has changed. In the docs' own example, data that arrived at 08:20 wasn't picked up until 09:00. | Data is silently stale by up to 45 minutes plus the job interval. There's no per-run signal that consumers see stale data. |
| `evaluate_volatile_sql: false` | `current_date()` and `current_timestamp()` are hashed as code, not values. | A model filtering on `current_date` can be **reused after midnight with yesterday's result**. |
| `allow_clones: true` (profile) | Clones from *any* environment, including dev into prod. | Provenance and audit problems. On Databricks, prod data files can depend on dev tables (see 1.4). |
| `pre_clone: if_missing` | Dev incrementals and snapshots are seeded from prod. | Prod data in dev schemas: a privacy and governance issue. |
| `execute_hooks_on_any_reuse: false` | Hooks are skipped on reuse. | Hooks that apply grants or tags aren't re-run. |
| `defer_to_target` without a manifest | Prod relation locations are *guessed* by re-rendering `generate_schema_name`; aliases are not re-rendered. | Docs: "most likely to cause **data corruption**" when `generate_alias_name` varies by target (`reference/resource-configs/defer-to-target.md`, "Caveats"). |

### 1.7 Explainability limits [FACT]
- `dbt state explain` (v2) or `dbt-state explain` (v1) runs *after* the run and reads local `logs/state/*.jsonl`.
- Some nodes come back as `UNKNOWN … explain details unavailable`, for example unit tests. (`reference/commands/state-explain.md`)
- Reasons are server-generated strings. You can't replay a decision offline or check it against a local state store.
- There is no dry-run or "plan" mode that shows decisions *before* spending compute. [INFERENCE: none is documented]
- The command name differs between v1 and v2, which trips people up. The docs call this out.

### 1.8 Failure semantics and concurrency [FACT]
- **"No concurrent build detection"**
  - If two jobs build the same **snapshot or incremental** model at the same time, "both jobs can detect the same changes in their separate transactions and commit them, which can lead to **duplicate records or other data corruption**".
  - The user is told to avoid overlapping schedules. (`docs/docs/deploy/dbt-state-migration.md`, "Known differences")
  - SAO had model-level queueing; dbt State dropped it.
- Efficient Testing from SAO (test reuse and aggregation) is **not in dbt State**.
- On a clone failure the client "falls back to full execution" (`DBTSTATE/run_cache.py:1061`). A clone is skipped if the source changed since the decision (`clone_required_last_modified_epoch` check, around line 1018).
- SAO rebuilt models that had failed tests. With dbt State, reused failing tests are "surfaced without being re-executed" (`how-dbt-state-works.md`), so a stale failure can persist until upstream changes. [INFERENCE]
- Canonical state on failed or partial runs: executions are confirmed per node through `confirm_execution` after success. The atomicity of the server-side state is **not documented**.

### 1.9 CI and dev workflows [FACT]
- SAO: deploy jobs only. dbt State adds dev and CI support, but through the auto-cloning described above.
- `defer-env-id` (platform) is manifest-based and **disables** the State-powered `state:*` selectors. Those selectors are in beta, and self-managed setups still need a `project-id` pointing at dbt platform. (`docs/docs/deploy/dbt-state-deferral.md`)
- Classic Slim CI with dbt Core still means managing `manifest.json` yourself: S3 or artifact storage, and not letting `--state` equal `--target-path`, because dbt overwrites `target/manifest.json` during parsing (`DOCS/snippets/_overwrites-the-manifest.md`). Community pattern: https://discourse.getdbt.com/t/can-you-persist-manifest-json-in-s3/13717, https://datacoves.com/post/dbt-slim-ci.

### 1.10 Multi-project and Mesh [FACT]
- Multiple projects need `dbt-cloud: project-id` or `state-org-id` in `dbt_project.yml`. (`faqs/State/multiple-projects.md`, `dbt-state-deferral.md`)
- Cross-project Mesh is a **seat-based paid feature, not in dbt OSS**. (`comparing-dbt-and-dbt-oss.md`)
- SAO propagated freshness "from an upstream model in the case of dbt Mesh" only inside the platform.

---

## 2. Community complaints and known pain points

### 2.1 `state:modified` false positives and negatives [FACT: GitHub issues]
- **Vars and env vars, false negatives:** a changed `var` or `env_var` value doesn't select the model. https://github.com/dbt-labs/dbt-core/issues/4304. Documented in `state-comparison-caveats.md` ("Vars").
- **Env-aware config, false positives:** https://github.com/dbt-labs/dbt-core/issues/9563, /9564, and discussion #10518. They are mitigated by the `state_modified_compare_more_unrendered_values` behaviour flag (1.9+).
- **Macros:** `state:modified.macros` gives false positives when a package macro shares a name with a builtin. https://github.com/dbt-labs/dbt-core/issues/10277
- **Dynamic `sql_header` from a macro gives wrong detection:** https://github.com/dbt-labs/dbt-core/issues/11150
- **Partition config gives false positives:** https://github.com/dbt-labs/dbt-core/issues/3645
- **Cross-version manifests:** 1.12.3 `statically_parse_unrendered_config` stringifies list and dict configs, which gives false positives whenever manifests from different dbt versions are compared. https://github.com/dbt-labs/dbt/issues/16133
  - [INFERENCE] For ODS, never compare fingerprints across dbt versions without normalisation. Record `dbt_version` and the schema version in state.
- dbt's own docs: "State comparison is complex. We hope to reach eventual consistency…" (`state-comparison-caveats.md`).
- `state:modified` says nothing about *data* changes: scheduled jobs with only `state:modified` build nothing (`DOCS/snippets/_state-modified-scheduled-jobs.md`). Users combine it with `source_status:fresher+`, which requires `sources.json` from previous runs.

### 2.2 `--defer` / `--state` fragility [FACT]
- Microbatch plus `--defer` fails with TABLE_OR_VIEW_NOT_FOUND. https://github.com/dbt-labs/dbt-core/issues/11128
- Snapshots in Slim CI reference the wrong schema. https://github.com/dbt-labs/dbt-core/issues/4110
- Ephemeral models with `--defer --full-refresh` fail. https://github.com/dbt-labs/dbt-core/issues/7595
- `relationships` tests under defer compare data across two environments. (`state-comparison-caveats.md`, "Tests")
- dbt State's own no-manifest guessing breaks with branch-, var- or path-derived schema names and target-specific aliases. (`defer-to-target.md`)

### 2.3 Microbatch and incremental on Databricks [FACT/REPORTED]
- The Databricks adapter runs microbatch batches sequentially. Concurrent batches hit `DELTA_CONCURRENT_APPEND`. https://github.com/databricks/dbt-databricks/issues/914
- `incremental_predicates` can bypass merge-key conditions. https://github.com/databricks/dbt-databricks/issues/1291
- `insert_overwrite` fails on schema change. https://github.com/databricks/dbt-databricks/issues/1057
- Materialization v2 breaks full refresh. https://github.com/databricks/dbt-databricks/issues/1201
- Liquid clustering updates on incremental runs break concurrency. https://github.com/databricks/dbt-databricks/issues/826
- `dbt_utils.star()` in microbatch gives PARSE_SYNTAX_ERROR. https://github.com/dbt-labs/dbt-utils/issues/1025
- dbt State wraps microbatch execution per batch window. The window start and end are sent to the server. (`DBTSTATE/runner.py:289-310`, `run_cache.py:1590-1596`)

### 2.4 Licensing and the future after the merger [FACT + OPINION]
- [FACT] Timeline:
  - 2025-05: Fusion under ELv2.
  - 2025-10-13: merger announced.
  - 2026-06-01: merger closed; Core v2 released under Apache-2.0; the ELv2 dbt-fusion repo archived.
  - 2026-09-16: renamed to "dbt" (proprietary binary) and "dbt OSS" (Apache).
  - Sources: `DOCS/blog/2026-06-01-dbt-core-v2-is-here.md`, https://github.com/dbt-labs/dbt-fusion (archived), https://www.getdbt.com/licenses-faq.
- [FACT] dbt Labs' own framing: dbt OSS is for "very few people", those who need Apache or are "building your own data transformation tool". Advanced local features (SQL comprehension, linting, LSP) are **not** in OSS. (`comparing-dbt-and-dbt-oss.md`)
- [OPINION: community] The worry is investment drift rather than the licence itself, with "Core stays functional, Fusion gets the exciting features", and Fivetran lacking an OSS culture. Sources:
  - https://kestra.io/resources/data/fivetran-dbt-merger-fusion-engine
  - https://nexla.com/blog/open-source-vs-saas-fivetran-dbt-merger
  - https://datacoves.com/post/dbt-fivetran (framed as "Risks, Lock-In")
  - https://dataengineeringcentral.substack.com/p/fivetran-dbt-labs-merger-what-does
  - https://dataengineeringcommunity.substack.com/p/data-engineering-digest-september-944 ("some are already looking for forks of dbt core")
  - Tobiko/SQLMesh, "Is dbt Fusion the death of dbt Core?": https://www.tobikodata.com/blog/dbt-fusion-death-of-dbt-core (couldn't be fetched)
  - https://driftwave.io/blog/dbt_fusion_license/
- [OPINION] What's still valuable for ODS: an open, self-hosted, auditable state engine is exactly what the dbt ecosystem lacks. Even "dbt OSS" users must pay dbt Labs and send SQL to the US to get state-aware skipping.
- I found no specific Reddit or HN thread criticising dbt State itself; the product is only about 4 months old. Early coverage is largely vendor or tutorial content, for example https://medium.com/@karthikrajashekaran/dbt-state-build-only-what-changed-1ff4874fd828.

---

## 3. Artifact formats: what's public, which versions, which licence

### 3.1 JSON schemas by dbt version [FACT]
Probed on `raw.githubusercontent.com/dbt-labs/dbt/<branch>/schemas/dbt/...` and checked against `DOCS/snippets/_manifest-versions.md` and the `reference/artifacts/*.md` pages.

| dbt | manifest | run_results | sources | catalog | notes |
|---|---|---|---|---|---|
| 1.7 | v11 | v5 | v3 | v1 | the `1.7.latest` branch has no manifest v12 or run-results v6 |
| 1.8 – 1.11 (and 1.12) | v12 | v6 | v3 | v1 | the `1.latest` branch has no v13, v7, v4 or catalog v2 as of today |
| v2.0 (dbt / dbt OSS) | v12 (docs table) | v6 | v3 (legacy, sources only) | v1, now only with `--write-catalog` | **new** `freshness.json` **v0** from `dbt freshness`, covering sources and models with a `resource_type`; `dbt source freshness` is "legacy" |

- **Canonical URLs:** `https://schemas.getdbt.com/dbt/<artifact>/vN.json` (the `$id` of manifest v12 is `https://schemas.getdbt.com/dbt/manifest/v12.json`, title `WritableManifest`). schemas.getdbt.com was blocked from this sandbox; the repo copies are reachable.
- **Where they live:** `schemas/dbt/` in the dbt-labs/dbt repo on the `1.latest` and `1.X.latest` branches (verified 200 for 1.7.latest, 1.10.latest, 1.11.latest and 1.latest). **`main` is now the Rust v2 code, and `schemas/dbt/manifest/v12.json` is 404 there.** Pin to a v1 branch or tag, or to schemas.getdbt.com.
- **Licence:** the dbt-labs/dbt repository is **Apache-2.0** (`LICENSE` → `core/LICENSE` on `1.latest`; Apache text on `main`). The schemas live in that repo, so they're under Apache-2.0. dbt Labs publishes no separate licence for schemas.getdbt.com. [INFERENCE: ODS can vendor them with attribution.]
- dbt docs: "Artifact versions may change in any minor version of dbt (v1.x.0). Each artifact is versioned independently", and every artifact carries `metadata.dbt_schema_version`. (`reference/artifacts/dbt-artifacts.md`)

### 3.2 What dbt v2 changed [FACT]
- JSON artifacts "continue to be produced for backwards compatibility" (repo README). v2 promotes `--no-write-json` / `DBT_ENGINE_WRITE_JSON=0` for speed. (`blog/2026-09-16-dbt-v2-is-ga.md`)
- **New: the dbt Information Schema.** It is a set of Parquet files under `target/info_schema/v1/`, written with `--generate-info-schema`, and it is "a contracted interface". Column types and column-level lineage need `--static-analysis strict`. It is part of dbt OSS (Apache). (`DOCS/docs/docs/build/dbt-information-schema.md`, `/reference/info-schema`)
  - [INFERENCE] This is a likely future deprecation path for JSON. ODS's `ArtifactProvider` should plan for a Parquet reader behind the same capability interface.
- 1.12 adds the `osi_document.json` artifact (Apache Ossie semantic document). (`reference/artifacts/dbt-artifacts.md`)
- `catalog.json` is no longer produced by `docs generate` in v2; it needs the `--write-catalog` flag.

---

## 4. How dbt State detects data changes on Databricks: summary [FACT, from `DBTSTATE/adapters/databricks.py`]

1. **Tables:** `DESCRIBE DETAIL` → `lastModified`, one query per table, in parallel.
   - The code comment explicitly rejects `information_schema.tables.last_altered` because it doesn't track DML (per the Databricks KB).
2. **Views:** `information_schema.tables.last_altered`, batched per catalog, which is correct for detecting a view *definition* change. Data freshness for a view is inferred by walking `information_schema.views` definitions (parsed with sqlglot) upstream to the base tables.
3. `loaded_at_field` or `loaded_at_query` override metadata when configured.
4. Current time comes from the warehouse: `SELECT unix_micros(current_timestamp())`.
5. It doesn't use Delta change data feed, `DESCRIBE HISTORY` operation filtering, table versions or row tracking.

Accuracy assessment [INFERENCE]:
- **Good:** it avoids the known `last_altered` trap for tables.
- **Coarse:** `lastModified` bumps on maintenance commits (OPTIMIZE, predictive optimization, clustering), giving false "fresh" signals and wasted rebuilds. It also carries no version number, so equal timestamps and clock skew aren't handled robustly.
  - A **Delta table version** from `DESCRIBE HISTORY` (the max version among data-changing operations) would be a strictly better, monotonic fingerprint.
- **Per-table `DESCRIBE DETAIL`** costs latency on large DAGs. A related serial-hydration bug exists on BigQuery (#15796).
- For comparison, dbt-databricks' own metadata freshness for UC sources uses `last_altered`, which is exactly the signal dbt State rejects. dbt's two freshness paths on Databricks can disagree.

---

## 5. Implications for ODS (the opportunity)

1. **Local, open state.** SQLite and Postgres. No login, no US processor, and it works air-gapped. Only the compiled SQL *hash* ever leaves the process, and only into the user's own store.
2. **Conservative defaults** (ODS rule 3), each the opposite of dbt State:
   - `lag_tolerance` 0.
   - Volatile functions (`current_date` and similar) mark a node non-reusable across day boundaries.
   - Cross-environment cloning into prod off by default.
   - No schema *guessing* without a manifest. Missing evidence → BUILD.
3. **Databricks-correct change detection.** Use the Delta table *version* and a history filtered to data-changing operations, ignoring OPTIMIZE, VACUUM and predictive optimization. Fall back to `DESCRIBE DETAIL`, then to BUILD. Views are resolved through lineage. All of this is a `ChangeProvider` capability, never core branching.
4. **Clone safety as capabilities.**
   - Declare `clone.zero_copy_independent` (Snowflake) versus `clone.shallow_dependent_on_source` with `max_chain_depth=1`, `no_iceberg`, and `replace_requires_drop` (Databricks).
   - The planner refuses clones whose source lives in a less durable environment, such as dev into prod, and always explains why.
5. **Explainability before execution.** A plan or dry-run with the reason chain and evidence (timestamps and versions, hashes) in text and JSON, reproducible offline from local state.
6. **Failure and concurrency.**
   - State advances only on success (rule 5).
   - Per-node leases or locks in the state store close dbt State's documented duplicate-record race for incrementals and snapshots.
7. **Artifacts.** Support manifest v11 and v12, run_results v5 and v6, sources v3, catalog v1, and freshness v0. Validate against the Apache-2.0 schemas pinned from the dbt-labs/dbt `1.latest` branch. Plan a Parquet Information Schema v1 reader for dbt v2.
8. **Cost transparency.** No per-table fee. Report avoided compute from the user's own run history.

## Open questions I couldn't verify
- Whether `DESCRIBE DETAIL.lastModified` changes on OPTIMIZE or predictive-optimization commits. Very likely, but Databricks docs weren't reachable. Test on a real workspace.
- Whether dbt State treats dbt-databricks `materialized_view` and `streaming_table` as "custom materializations" that always rebuild.
- The exact contents of the "labels" and "semantic_extras" sent to the server, and whether the git branch is included.
- The exact wording of the licence for the proprietary "dbt" binary (getdbt.com was blocked).
