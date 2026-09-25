# dbt State (successor to state-aware orchestration): public-docs research report

> Research source (2026-09-24), kept verbatim for traceability. Paths such as `DOCS/...` or `docs/...` refer to files in the public docs repo named at the top. Tags: [V]/FACT = checked in a primary source; [S]/REPORTED = search summary; [I]/OPINION = inference. The synthesis is [`../ods-state-strategy.md`](../ods-state-strategy.md).

Prepared for OpenDataSuite (ODS) State module. Date: 2026-09-24.

**Sources.** Only public documentation was used: the `dbt-labs/docs.getdbt.com` source tree (sparse clone, `website/`) and three public images from the same repo on `raw.githubusercontent.com` (branch `current`). No proprietary code was looked for or read. The docs link a `github.com/dbt-labs/dbt-state` repo (`docs/docs/dbt-versions/compatible-track-changelog.md:51`). I did **not** open it, because nothing public says whether it is open source.

**Paths.** All paths are relative to `website/`. "[img]" marks facts that come only from a public docs image.

**Labels.** "Inference" marks my own reading of the docs and is not a documented fact.

---

## 0. Executive summary

- **What it is.** dbt State is a **hosted, paid, usage-metered service** from dbt Labs. Before each node runs, dbt (the client) asks the service whether to **skip** the node (reuse it in place), **clone** it from another schema or environment, or **build** it normally.
- **What the client sends.** Only last-modified timestamps and hashes of normalized SQL are persisted. The query text is also sent, but it is discarded once the server has made its decision.
- **Engines.** It works with dbt v2 (the renamed Fusion engine), dbt OSS, dbt Core v1.7–1.12 through a `pip install dbt-state` plugin, the dbt platform, and external orchestrators. A dbt platform account is required.
- **Pricing.** The price is **USD $0.094 per "daily active target table" (DATT)**. A DATT is a distinct target table (model, seed, snapshot, or each test) that State skips, clones, or reuses test results for, at least once in a UTC day. There is a 30-day free trial with no usage cap.
- **The core algorithm, in order:**
  1. Check whether the node is eligible for State at all.
  2. Hash the node's SQL: render Jinja, parse to a syntax tree, normalize, then hash.
  3. Look for a matching hash on any known execution, in any schema.
  4. Run a freshness check using warehouse metadata or `loaded_at_*`. Freshness is propagated through views, and `lag_tolerance` is applied (default `45m`).
  5. If the match is the same table: reuse, as a no-op.
  6. If the match is in another schema: clone it, provided cloning is allowed.
  7. Otherwise, rebuild.
- **Storage formats.** The service's storage formats are **not publicly documented**. The docs do show:
  - the local log file naming (`logs/state/responses_<timestamp>.jsonl`),
  - human-readable examples of the explain output,
  - the reason codes printed in verbose output (for example `TARGET_TABLE_EXISTS` and `NODE_QUERY_UNCHANGED`),
  - the telemetry `skip_reason: cached`.
- **Licensing.** dbt OSS (v2) is Apache 2.0. The full "dbt" binary contains proprietary code under a license "more permissive than ELv2". The dbt State service is proprietary. The earlier SAO service code was explicitly called "proprietary" (`blog/2025-05-28-dbt-fusion-engine-components.md:90`).

---

## 1. Architecture

### 1.1 Where state lives and what is sent

- **A hosted service.** "When dbt runs, it will check with the dbt State server whether a model…" (`blog/2026-06-17-how-dbt-state-works.md`). The blog calls it "a new paid service".
- **What is sent and persisted** (`docs/faqs/State/data-storage.md`):
  - "**Last-modified timestamps**: Used to determine whether upstream data has changed since the last run."
  - "**SQL statement hashes**: SQL statements are processed to detect and classify changes, then hashed. Only the hash is persisted for future comparisons."
  - "No actual data from your warehouse is transmitted."
- **Query text is transmitted but not kept.** "It sends the freshness information and the query text to the dbt State server to decide whether and how it can be reused (the query text is discarded after the server finishes its work)" (`blog/2026-06-17-how-dbt-state-works.md`, section *Freshness checks*). The FAQ says only hashes are persisted.
- **Deployment.**
  - "a single US multi-tenant (MT) instance. The service never connects to your data warehouse … The only connection is to your running dbt process (CLI or platform)" (`docs/faqs/State/data-storage.md`).
  - "dbt State is currently only available in a Google Cloud Platform US region" (`docs/docs/platform/about-platform/access-regions-ip-addresses.md:22`).
- **Where timestamps come from.** "Last updated timestamps come directly from the data warehouse, for example from `INFORMATION_SCHEMA` tables" (`docs/faqs/State/last-updated-timestamp.md`). The client gathers these itself with metadata queries (see `metadata_warehouse`).
- **When metadata is fetched.** "dbt State fetches table metadata (for example, last-modified timestamps) in the background at the start of each run. Any node ready to skip, clone, or execute proceeds immediately; nodes with an undetermined action wait for the fetch to complete" (`docs/docs/deploy/dbt-state-about.md`). The August 2026 release notes describe this as an enhancement (`docs/docs/dbt-versions/release-notes.md:101`).
- **If the service is down.** "If dbt State servers are unavailable, dbt gracefully falls back to normal dbt behavior" (`docs/faqs/State/server-failure.md`).
- **State is shared across people and environments.** Reuse candidates come from "all environments and jobs", including dev and CI schemas (`docs/docs/deploy/dbt-state-about.md`). "The more team members you have using dbt State, the better it gets" (`docs/docs/deploy/dbt-state-setup.md`).
- **The client component.**
  - Logs show `State adapter: dbt-state v2.43.1 is enabled` and `State adapter: Fetching freshness metadata` (`docs/docs/deploy/dbt-state-examples.md`). Another page shows `dbt State adapter: dbt-state v2.10.1 is enabled` (`docs/docs/deploy/dbt-state-cicd.md`).
  - The compatible release track pins `dbt-state 2.42.0` (`docs/docs/dbt-versions/compatible-track-changelog.md:51`).
  - Inference: on v1 the client ships as a plugin/adapter-wrapper package named `dbt-state`.
- **Contrast with the predecessor (SAO).** SAO cached "a hash of both code and data state for each model in an environment stored in Redis" and had a **Clear cache** button per environment (`docs/docs/deploy/state-aware-interface.md`). SAO state was per environment, whereas dbt State is cross-environment.

### 1.2 Authentication

**Interactive: `dbt login` (v2 only)**

- Browser-based login to a dbt platform account. Credentials are stored in `~/.dbt/` (Windows: `C:\Users\[username]\.dbt\`) (`docs/reference/commands/login.md`).
- Session lifetime: "A 24-hour access token … dbt renews it about once an hour", backed by "A 7-day sign-in"; any activity resets the 7-day clock.
- After login, the v2 CLI compares account-level and local enablement (`docs/reference/commands/login.md`, table "dbt login with dbt State"):

| Enabled in the platform? | Enabled locally? | What happens |
|---|---|---|
| Yes | Yes | dbt State is ready to use. |
| Yes | No | The CLI prompts to enable it locally and, on confirmation, writes `~/.dbt/user_settings.yml`. |
| No | Yes | The CLI prompts you to enable it in the platform account. |

- Snippet with the same logic: `snippets/_state-login.md`.
- If State is enabled locally but not on the account, "dbt fails with an error on your next `dbt run` or `dbt build`" (`docs/reference/global-configs/user-settings.md`).

**v1 plugin**

- On the first `dbt run` or `dbt build`, a browser window opens for login or account creation (`docs/docs/deploy/dbt-state-setup.md`, tab "dbt v1.7-1.12").

**Non-interactive environments (CI, Airflow, and so on)** (`docs/docs/deploy/dbt-state-cicd.md`, `docs/reference/commands/login.md`)

- Service token, set through environment variables:
  - `DBT_CLOUD_TOKEN`
  - `DBT_CLOUD_ACCOUNT_HOST` (for example `abc123.us1.dbt.com`)
  - `DBT_CLOUD_ACCOUNT_ID`
  - optionally `DBT_CLOUD_PROJECT_ID` (listed on the login page)
- Token permission set: the minimum recommended is **Job Runner**. Owner, Account Admin, Job Admin, Job Creator, and Developer also work. "Service tokens don't expire."
- Legacy standalone app (`app.state.dbt.com`, being retired and closed to new users): OAuth client credentials via `DBT_ENGINE_STATE_OAUTH_CLIENT_ID` and `DBT_ENV_SECRET_STATE_OAUTH_CLIENT_SECRET`. The app had these roles:
  - Owner and Admin: access to the Usage, Users, Billing, and Clients tabs.
  - Developer: access to Usage only.
- "dbt State automatically detects when it's running in a non-interactive environment. If valid credentials are not provided, dbt State disables itself and displays a warning, allowing your dbt commands to continue without caching."
- License check: `dbt license info [--json]` shows `features: compare, state-aware-orchestration, strict-static-analysis`. Status values are `valid`, `expired`, `not_found`, `invalid`, and `transient_error` (`docs/reference/commands/login.md`).

**Project and org selection** (`docs/docs/deploy/dbt-state-deferral.md`, `docs/faqs/State/multiple-projects.md`), set in the `dbt-cloud:` block of `dbt_project.yml`:

- `project-id`: for platform users with multiple projects. It is also required for State-powered `state:*` selectors.
- `defer-env-id`: optional.
- `state-org-id`: for self-managed deployments with multiple State orgs.

### 1.3 Supported versions and engines

- "natively available in dbt platform and Fusion engine. It's also available as a plugin for dbt v1.7–1.12" (`docs/docs/deploy/dbt-state-setup.md`).
- Release notes: "works locally (dbt v1.7 through v2.0)" (`docs/docs/dbt-versions/release-notes.md:53`).
- A blog says "versions 1.11 and lower need to explicitly `pip install dbt-state` first" (`blog/2026-06-17-how-dbt-state-works.md`). This implies 1.12 may bundle or auto-prompt for it. That is my inference, and it conflicts with the setup page, which says to use the plugin for 1.7–1.12.
- **dbt OSS.** "Can I use dbt State with dbt OSS and dbt? Yes, both distributions include support for dbt State" (`blog/2026-09-16-comparing-dbt-and-dbt-oss.md`). The comparison table there also marks "Supports optional usage-based paid features (e.g. dbt State, dbt Wizard)" as ✅ for both distributions.
- **Warehouses.** "Snowflake, Databricks, BigQuery, or Redshift. More warehouses are on the roadmap" (`docs/docs/deploy/dbt-state-setup.md`). In platform jobs, the State checkbox "is now disabled when the job's environment uses an unsupported warehouse adapter" (`docs/docs/dbt-versions/dbt-platform-release-notes-gen.md:479`).
- **Plans.** It is not available on the legacy Starter plan (`dbt-state-setup.md`). "A paid dbt platform plan is _not_ required to use dbt State locally" (`snippets/_dbt-state-trial-how-it-works.md`). "without requiring a recurring dbt platform subscription" (`docs/docs/optimize-builds.md`).
- **Platform release tracks.** Available on the Compatible, Fusion Extended, and Fusion Fallback tracks, "in addition to previously supported tracks" (`release-notes.md:96`).

### 1.4 Enabling it and where it applies

**Platform**

- Account admin path: Account settings → Billing & Usage → Usage-based features → State tab → Start free trial → enable by environment (new jobs in that environment inherit it) or by specific jobs (`docs/docs/deploy/dbt-state-setup.md`).
- Per job: Job Settings → Execution settings → **Enable dbt State**. It is available for deploy, CI, and merge jobs (`docs/docs/deploy/dbt-state-enable-jobs.md`; CI and merge support was added later, per `dbt-platform-release-notes-gen.md:406`).
- New jobs have the checkbox on by default when a subscription exists (`dbt-platform-release-notes-gen.md:525`).
- Studio IDE: an environment-level toggle, plus a per-user override (Enabled / Disabled / Reset). The user override wins (`docs/docs/deploy/dbt-state-enable-studio.md`). The IDE uses account-level State credentials (`dbt-platform-release-notes-gen.md:407`).

**CLI, v2**

- Flag `manage_state` (v2.0+), boolean, default False (`docs/reference/global-configs/about-global-configs.md:131`). It can be set in:
  - the project `flags:` block,
  - `~/.dbt/user_settings.yml`,
  - the environment variable `DBT_ENGINE_MANAGE_STATE`,
  - the CLI as `--manage-state` / `--no-manage-state`.
- Precedence: CLI > env var > `dbt_project.yml` > `user_settings.yml` (`docs/reference/global-configs/user-settings.md`).
- "activate your free trial with `dbt build --manage-state`" (`blog/2026-09-16-dbt-v2-is-ga.md`).

**CLI, v1**

- The `--manage-state` flags are not available. Use env var `DBT_ENGINE_ENABLE_STATE` or project flag `enable_state` (`docs/docs/deploy/dbt-state-setup.md`).

**Commands it applies to**

- It "will run automatically on every `dbt run` or `dbt build`" (`dbt-state-setup.md`).
- The decision tree [img] lists "unsupported command/adapter" as a bypass, so other commands are presumably bypassed. That is my inference.

**External orchestrators, dev, CI, prod**

- "across all environments and orchestrators" (`dbt-state-about.md`).
- Development in a fresh schema needs no `--defer` or `--state` (`dbt-state-about.md`, examples page).
- Debug toggle: `DBT_ENGINE_MANAGE_STATE=0 dbt run …` (`dbt-state-setup.md`).

---

## 2. Decision algorithm

### 2.1 Official three-way outcome

From `docs/reference/resource-configs/dbt-state-configs.md` and `docs/docs/deploy/dbt-state-about.md`:

- **Reuse (skip, "No-op").** The object exists in the target schema, **and** its logic is unchanged, **and** it is not due under `lag_tolerance`: either its last build is within the window, or its upstream data hasn't changed. The node is then skipped "as if it was never selected".
- **Clone.** Used when reuse isn't possible but a matching object exists elsewhere "with identical logic and fresh data".
  - The source is not limited to production: "dbt State looks across all environments and jobs … This includes schemas where a model was built before it ever ran in production. When multiple candidates exist, dbt State clones from the one with the freshest data, regardless of which environment it came from."
  - Cloned nodes are shown as **Reused**.
- **Normal build.** Used otherwise, and "automatically deferring any unselected upstream nodes".
- **Contrast with standard deferral.** "Unlike standard deferral, which always builds selected nodes and only defers unselected upstream references, dbt State can skip or clone selected nodes, too."
- **Governing principle.** "dbt State only skips work when it can prove the existing object is sufficiently equivalent for the current run. If the SQL logic, relevant config, schema, or upstream freshness means the result might be different, dbt rebuilds instead" (`dbt-state-about.md`). "dbt State prioritizes safety and precision; if it can't guarantee skipping a node is safe, then it rebuilds" (`docs/faqs/State/views-rebuilt.md`).

### 2.2 Published decision tree [img: `static/img/docs/deploy/run-cache-decision-tree.png`, embedded in `dbt-state-about.md`]

The image is titled "dbt State: Rebuild vs Clone vs Reuse — Decision made per node, before dbt would otherwise build it from scratch." Its steps, in order:

1. **Eligibility: "Is dbt State bypassed for this node?"** The bypass reasons listed are "Disabled, unsupported command/adapter, write-only mode, or untracked view." If yes, the outcome is **REBUILD** ("dbt runs it normally. Write-only mode still records the result.").
2. **Hashing: "Is evaluate_volatile_sql enabled?"**
   - No (the default): "Hash by function name. Value is ignored."
   - Yes: "Hash by function value. Value is emulated outside the warehouse."
3. **Match: "Does any known execution anywhere have this same hash?"** ("Any schema can match, not just this table's last run.") If no, the outcome is **REBUILD** ("No hash match found anywhere").
4. **Freshness: "Has fresh data arrived from a dependency, beyond tolerance?"** If yes, the outcome is **TRY CLONE OR REBUILD**: "Fresh upstream data exists. Clone if possible; otherwise rebuild." The image adds: "If yes, dbt may still clone from time travel or another schema when available."
5. **"Is that matching execution for this exact table?"** ("Same schema: the existing relation is already correct.") If yes, the outcome is **REUSE (NO-OP)**: "Already correct here — skip it. Hooks run only when run_hooks_on_no_op is true."
6. **Cloneability: "Can that match be cloned from its schema?"** ("Requires defer_to, configured source data, and clone time-travel/chain-depth limits.")
   - Yes: **CLONE**, "Copy the object and its test results. Not available for custom materializations."
   - No: **REBUILD**, "A match was found elsewhere, but cannot be cloned here."
7. The image footer lists the settings it references: `evaluate_volatile_sql, freshness tolerances, defer_to, clone_time_travel_limit, and run_hooks_on_no_op`.

**Notes on the image (my inference and labelling):**

- `run_hooks_on_no_op` appears to be an internal name for the documented `execute_hooks_on_any_reuse`.
- `defer_to` appears to correspond to `defer_to_target`.
- `clone_time_travel_limit`, "chain-depth limits", "write-only mode", "untracked view", and "clone from time travel" appear **only in this image**. They are not in any reference page. They suggest that:
  - State can clone a historical version using warehouse time travel,
  - it limits clone-of-clone chains,
  - it has a write-only (record-but-don't-decide) mode.
- The verbose run configuration printed by `dbt state explain` exposes further internal knobs (see 4.1): `tolerate nondeterminism`, `clone incremental in dev: IF_TABLE_MISSING`, and `metadata cache TTL`.

### 2.3 How "logic changed" is determined

**Parse and hash the rendered SQL**

- "dbt State decides whether to reuse a model by parsing the rendered SQL into a syntax tree and comparing the hash" (`docs/faqs/State/views-rebuilt.md`).
- "it parses the query into a syntax tree, and then calculates a hash of the query. This means it won't rebuild on a syntactically equivalent change, like removing whitespace, adding a comment, or changing a table's alias" (`blog/2026-06-17-how-dbt-state-works.md`).
- The blog image [img `query-normalisation-hash-comparison.png`] shows two files that differ only in formatting and a comment. They get different file hashes without dbt State, but produce the same "comparison hash" (`336…2d`) after normalization to an AST.
- "parsing and normalization happens after the Jinja has been rendered" (same blog).
- The FAQ says State "can see through things like whitespace and aliases" (`docs/faqs/State/model-change-calculation.md`) and ignores "table aliases" (`docs/faqs/State/state-modified-difference.md`).
- Motivating example: running `dbt lint --fix` should not trigger rebuilds.

**Lineage-aware change detection**

- The three questions the blog lists: "Is there a logic change in the node or any of its parents? Is there fresh data that exceeds the configured `lag_tolerance`? Is there a config change in the node?" (blog).
- An upstream compiled-SQL change forces downstream rebuilds regardless of `lag_tolerance` (`docs/reference/resource-configs/lag-tolerance.md`, "When does lag_tolerance apply"). The verbose output reports "upstream model queries have not changed".
- **Column-level pruning (blog claim).** "unlike `state:modified+`, downstream models can be skipped if dbt State determines they don't depend on that change. In this example, the `customers` view specifies the exact columns it uses, so adding a new column to `orders` doesn't cause an unnecessary rebuild." Inference: the hash covers the columns actually consumed from upstream.
- **`select *` rule** (`views-rebuilt.md`):
  - A view doing `select *` directly on `ref()` or `source()` is always rebuilt, because columns can't be determined at parse time.
  - `select *` from a CTE that names its columns explicitly is reusable. This was an enhancement in `release-notes.md:100`.
  - The migration page frames this as views: "dbt State can't determine which columns `select *` resolves to without querying the upstream schema."
  - Tip from the same page: exclude views with `--exclude config.materialized:view`.
- **Schema changes.** "dbt State checks all sources to see if there is any new data or if the schema has been modified" (`state-modified-difference.md`).

**Configuration changes**

- "dbt State will only pay attention to the subset of relevant configs." Configs like `meta` and `tags` are ignored. `on_schema_change`, `severity`, and `materialization` matter (blog).
- Conditional materializations (for example table in prod, view in dev) "can prevent dbt State from matching targets correctly" (`docs/docs/platform/billing/optimize-costs.md:39`).

**`compare_unrendered_code` (default `false`)**

- By default only the rendered SQL is compared, so non-deterministic Jinja such as `invocation_id()`, `env_var('AIRFLOW_RUN_ID')`, `run_started_at`, `dbt_utils.get_relations_by_pattern` ordering, or `datetime.now().month` forces rebuilds.
- With `true`, a rebuild happens only when **both** the unrendered template **and** the rendered SQL changed.
- The unrendered comparison includes "the source of any macros it calls".
- If the template is unchanged, SQL parsing is skipped entirely, so `evaluate_volatile_sql` has no effect in that case (`docs/reference/resource-configs/compare-unrendered-code.md`).

**`evaluate_volatile_sql` (default `false`)**

- By default, volatile SQL functions (`CURRENT_TIMESTAMP()`, `RANDOM()`, `UUID_STRING()`, `getdate()`) are hashed by function name, as static code.
- With `true`, "dbt State stores the _result_ of each volatile function call and uses those stored values for future comparisons". For example, `current_date()` changes after midnight and triggers a rebuild (`docs/reference/resource-configs/evaluate-volatile-sql.md`).
- The blog says State will "emulate the function's value and embed it into the hash of the SQL". The image says "Value is emulated outside the warehouse."
- Alternative: use a Jinja datetime, which changes the rendered SQL.

**Comparison with SAO and core**

- SAO used "Compiled SQL diffs that ignore non-meaningful changes like whitespace and comments" (`docs/faqs/Runs/sao-difference-core.md`).
- Core's `state:modified` "only checks if a file has changed" and has a 1 MB seed limitation. dbt State has no such seed limitation (`state-modified-difference.md`).

### 2.4 How data freshness is determined

- **Default source of timestamps.** State "fetches the last modified time for each Relation in the query. It defaults to using the warehouse's metadata … but you can also specify a `loaded_at_field`/`loaded_at_query`" (blog). The timestamps come from `INFORMATION_SCHEMA` and similar (`last-updated-timestamp.md`).
- **Propagation through views.** "If any of the input Relations are views without a `loaded_at_field`/`loaded_at_query` specified, then State will traverse further upstream until it finds a real table and will use that timestamp" (blog). "tracks data freshness across the DAG and automatically propagates it through models materialized as views" (`snippets/_dbt-state-vs-sao.md`).
- **Freshness is relative to the node's last build.** Verbose explain shows each upstream relation tagged `[FRESH]`, `[fresh]`, or `[within tolerance]`, with text like "no updates since X last executed" or "updated a moment after X last executed (tolerance: 45 minutes)" (`docs/reference/commands/state-explain.md`).
- **Warn/error thresholds are not needed.** You don't need `warn_after`/`error_after` for State to detect changes. Configure `loaded_at_field`/`loaded_at_query` for streaming data or late-arriving data (`docs/docs/build/sources.md:153-159`).
- **`loaded_at_field` / `loaded_at_query` semantics** (documented under SAO in `docs/docs/deploy/state-aware-setup.md`, and applicable to dbt State per `materializations-guide-4-incremental-models.md:171`):
  - The source counts as fresh when the max value, or the single timestamp returned by the query, changes compared with the previous run.
  - The two settings are mutually exclusive.
- **Late-arriving data.** If `loaded_at` is an event timestamp, late rows may go undetected. Align `loaded_at_query` with the incremental lookback window (`docs/best-practices/materializations/materializations-guide-4-incremental-models.md:171-183`).
- **BigQuery external sources** (for example Google Sheets) have no modification timestamp, so they always rebuild unless `loaded_at_*` is set (`views-rebuilt.md`, `dbt-state-migration.md`).
- **Snowflake `metadata_warehouse`.**
  - Default: a single consolidated metadata query on the main warehouse.
  - With `metadata_warehouse` set: one query per schema, run in parallel on the dedicated warehouse.
  - If metadata queries take more than 15 s on the main warehouse, dbt emits a warning (`docs/reference/resource-configs/metadata-warehouse.md`).
- **Views are reused on unchanged logic.** "if the view's logic is unchanged, dbt State reuses it even if new data has arrived upstream" (`dbt-state-about.md`).
- **Materialization rules in freshness configs** (`docs/reference/resource-configs/freshness.md`; this is a general v2 freshness rule, not State-specific):
  - Views and external models **require** `loaded_at_field` or `loaded_at_query` if a model freshness block is configured.
  - Ephemeral models don't support it.
  - Tables and incremental models fall back to adapter metadata.
- **Known risk: manual table edits.** Freshness is timestamp-based, so "manually editing a table outside of dbt can give it a newer modification date without actually matching what a full build would produce" (`docs/reference/resource-configs/allow-clones.md`).

### 2.5 `lag_tolerance` semantics (default `45m`)

Source: `docs/reference/resource-configs/lag-tolerance.md`.

- **Rebuild condition.** "A node rebuilds only when _both_ are true: its last build is older than the `lag_tolerance` window, and its upstream data has changed since that build."
- **It is a minimum interval.** "`lag_tolerance` sets a minimum time between rebuilds … controls how often a node can rebuild, not how fresh its upstream data has to be."
- **Worked example.** Tolerance is `45m`, the last build was at 08:00, and data arrived at 08:20:

| Run | Result | Why |
|---|---|---|
| 08:30 | Reuse | Only 30 minutes since the last build. |
| 09:00 | Rebuild | Past the window, and new data exists. |
| 09:30 | Reuse | Within the window, and no new data. |
| 10:00 | Reuse | No new data since 09:00. |

- **`0s`** means rebuild whenever upstream data changes.
- **It applies only to data freshness.** SQL changes always rebuild, including upstream compiled-SQL changes.
  - Gotcha: the first run of an incremental model is a full load. When it then becomes incremental, its compiled SQL changes, which counts as an upstream query change. All downstream models rebuild even inside their tolerance window.
- **Value types.**
  - Duration strings `<number><unit>`. Units: `s|second|seconds`, `m|minute|minutes`, `h|hour|hours`, `d|day|days`, `w|week|weeks`.
  - Or a Jinja expression with `target`, `var()`, `env_var()`, and `modules.datetime` available. Example: `"{{ '4h' if target.name == 'prod' else '7d' }}"`.
- **Where it can be set.** Project, folder, model (YAML), or SQL `config(state={...})`. The setup page says it can be set "at the project, environment, or model level" (`docs/docs/deploy/dbt-state-setup.md`).
- **The internal verbose output calls it "freshness tolerance".** For example `freshness tolerance: 2700 seconds`, and the run summary line `Freshness tolerance: 45m`.
- **Documented discrepancy.** The migration page says "`state.lag_tolerance` (for example, `4h`) skips the model unless upstream data is newer than the model's last run by at least the configured interval" (`docs/docs/deploy/dbt-state-migration.md`). This differs from the reference page, which measures time since the last build. `snippets/_dbt-state-vs-sao.md` says "`lag_tolerance` compares against the freshness of the underlying data", and the FAQ says "how much time must pass since the last upstream data change" (`docs/faqs/Runs/what-happened-to-sao.md`). **The docs are internally inconsistent on the reference point. The reference page (`lag-tolerance.md`) with its worked example is the most precise.**
- **DATT billing.** "A model inside its lag tolerance window will still be counted as a DATT if you select it for execution and it can be reused, so we recommend continuing to use selectors in development" (blog).
- **Recommended starting values.** `4h` for prod and `7d` elsewhere (`dbt-state-setup.md`, `optimize-costs.md`).

### 2.6 `require_fresh_data_from` (default `any`)

- `any`: the node becomes eligible to rebuild when any direct parent has fresh data.
- `all`: the node becomes eligible only when all direct parents have fresh data.
- "dbt State still looks for an object to reuse before triggering an actual rebuild" (`docs/reference/resource-configs/require-fresh-data-from.md`).
- It maps from SAO's `freshness.build_after.updates_on` (`dbt-state-migration.md`).

### 2.7 Deferral

- Deferral to production is automatic, with no `--defer` or `--state` needed (`dbt-state-about.md`).
- **On the platform.** State defers to the production environment by default. You can override with `defer-env-id` under `dbt-cloud:` in `dbt_project.yml`. `defer-env-id` is manifest-based, and setting it disables State-powered `state:*` selectors (`docs/docs/deploy/dbt-state-deferral.md`).
- **Self-managed.** `defer_to_target` in `profiles.yml` per output, default `prod`. It is "best-effort auto-deferral" when there is no manifest (`docs/reference/resource-configs/defer-to-target.md`).
- **Explicit manifest.** `--state` or `--defer-state` points State to a specific `manifest.json` "as the source of truth for cloning objects" (`snippets/_dbt-state-deferral-config.md`).
- **Location guessing without a manifest.** State "re-renders the database and schema names using the configured target from `defer_to_target`". It **does not re-render aliases**. This goes wrong when:
  - `generate_schema_name` depends on `env_var` or `var` values that differ between runs,
  - `generate_schema_name` derives names from paths such as `node.fqn`, and files move,
  - `generate_alias_name` is target-specific. The docs call this last case "the most likely to cause data corruption".
  - The recommendation is to provide a `manifest.json` in these cases (`defer-to-target.md`).
- **State-powered `state:*` selectors (beta).**
  - Self-managed only, and `project-id` is required.
  - Each node (models, snapshots, seeds, tests) is compared "against its own last execution in the `defer_to_target` environment (default: `prod`)" rather than against one `manifest.json`. This applies "automatically when no explicit `--state` manifest has been provided."
  - Platform jobs still use per-job manifests (`dbt-state-deferral.md`, `state-modified-difference.md`, `docs/reference/node-selection/state-selection.md`).
- **Tool integration.** dbt Wizard has a deferral mode `dbt_state` in which "dbt State or run cache handles deferral" (`docs/docs/dbt-ai/wizard-config.md:236`).

### 2.8 Cloning

- Clones come from any environment, including dev → prod. `allow_clones` (profile level, default `true`) can disable cloning into a specific target, for example in regulated prod environments. It can also be set as a platform extended attribute (`docs/reference/resource-configs/allow-clones.md`). Cloning was not configurable before this setting existed (`release-notes.md:103`).
- A clone copies "the object and its test results" and is not available for custom materializations [img].
- **Generic `dbt clone` behavior** (not specific to State): zero-copy clone where the warehouse supports it (for example Snowflake); "On other warehouses, it creates views pointing at the upstream relations" (`docs/docs/optimize-builds.md`). How State clones on BigQuery, Databricks, and Redshift is **not documented**.
- **Hooks always run on clone**, "because the clone step creates a new object in the warehouse" (`execute-hooks-on-any-reuse.md`).

### 2.9 Node types

- **Reusable.** "all node types that create relations in the database (such as SQL models, snapshots, seeds) and data tests" (`dbt-state-about.md`).
- **Data tests.** "if the nodes being tested haven't changed since the last run, the previous test result is reused without re-executing the test query." Failing test results are still surfaced when reused: "Even though the test isn't being re-executed, its warning still appears" (blog).
- **Unit tests.** In the `dbt state explain` example, a unit test shows `UNKNOWN … dbt State explain details unavailable` (`state-explain.md`). Unit tests are not documented as reused.
- **Python models.** Always built, never reused (`docs/faqs/State/python-models.md`).
- **Custom materializations.** Always built, because of possible side effects (`dbt-state-about.md`, `views-rebuilt.md`).
- **Incremental models and snapshots.**
  - `pre_clone` controls pre-population by cloning production before a dev run:

| Value | Behavior |
|---|---|
| `never` | Start from whatever exists in dev; build from scratch if nothing exists. |
| `if_missing` (default) | Clone only if the target doesn't exist in dev; later runs build incrementally on top of the clone. |
| `always` | Clone before every run. |

  - Non-incremental materializations are never pre-cloned (`docs/reference/resource-configs/pre-clone.md`).
  - When you change an incremental model in dev, State clones production and then runs the new logic on top. `--full-refresh` reverts to original dbt behavior (`docs/faqs/State/incremental-models.md`).
  - Verbose explain shows this setting as `clone incremental in dev: IF_TABLE_MISSING`.
- **Views.** Reused when logic is unchanged, even with new upstream data (see the `select *` caveat). An "untracked view" is a bypass reason in the decision tree [img]. The lag recommendation list excludes views.
- **Ephemeral models.** Nothing State-specific is documented. v2 telemetry uses `skip_reason: noop` for "Node doesn't perform work in this phase (for example, ephemeral models)" (`docs/reference/telemetry-observability.md`).
- **Microbatch.** **Not mentioned anywhere in the State docs.** This is an undocumented gap.
- **Seeds.** Reusable, and have no 1 MB limit (`state-modified-difference.md`).

### 2.10 Hooks

- **`execute_hooks_on_any_reuse`** (default `false`). On a skip (reuse in place), pre- and post-hooks are not run, matching dbt's normal behavior. With `true`, hooks run even on reuse. Hooks always run on clone (`docs/reference/resource-configs/execute-hooks-on-any-reuse.md`).
- The blog recommends it for hooks that depend on configs State ignores, such as `meta` or `tags` to tags (blog).

### 2.11 Failures, concurrency, and deleted tables

- **Canonical-state rules on failure.** How failed or partial runs update server state is **not documented** for dbt State.
- **SAO behavior (predecessor, not confirmed for dbt State).**
  - A model that failed a data test is rebuilt on the next run instead of being reused.
  - Deleted tables are detected and rebuilt (`docs/docs/deploy/state-aware-about.md`).
  - For dbt State, the Explain "table analysis" checks whether the target table exists (`TARGET_TABLE_EXISTS`). Inference: a missing table prevents an in-place reuse.
- **No concurrent-build detection in dbt State.**
  - If Job 1 finishes building a node after Job 2 starts but before Job 2 reaches that node, Job 2 still builds it.
  - "if Job 2 starts building the same snapshot or incremental model before Job 1 executing the same node, both jobs can detect the same changes in their separate transactions and commit them, which can lead to duplicate records or other data corruption."
  - Recommendation: don't run overlapping jobs that share nodes (`docs/docs/deploy/dbt-state-migration.md`).
  - SAO did have model-level queueing (`state-aware-about.md`).
- **Efficient Testing** (SAO's test reuse and test aggregation, private beta) is "not yet" available in dbt State (`dbt-state-migration.md`).

---

## 3. Configuration reference

**Node-level configs** go under a `state:` block. They can be set with `+state:` in `dbt_project.yml` (project or folder), with `config: state:` in properties YAML, or with `{{ config(state={...}) }}` in SQL (`docs/reference/resource-configs/dbt-state-configs.md` and per-key pages). The docs show them under `models:`. Whether seeds and snapshots accept the same block is not explicitly shown.

| Key | Scope / where set | Type and values | Default | Meaning | Source |
|---|---|---|---|---|---|
| `lag_tolerance` | Node, folder, or project via `state:` | Duration string (`s`, `m`, `h`, `d`, `w` plus long forms) or a Jinja string | `45m` | Minimum time since the last build before a rebuild on new upstream data. Data freshness only. | `lag-tolerance.md` |
| `require_fresh_data_from` | Node, folder, or project | `any` or `all` | `any` | How many direct parents must have fresh data for the node to be eligible to rebuild. | `require-fresh-data-from.md` |
| `compare_unrendered_code` | Node, folder, or project | bool | `false` | Rebuild only if both the unrendered template (plus called macros) and the rendered SQL changed. | `compare-unrendered-code.md` |
| `evaluate_volatile_sql` | Node, folder, or project | bool | `false` | Hash volatile functions by their emulated runtime value instead of by name. | `evaluate-volatile-sql.md` |
| `pre_clone` | Node, folder, or project | `never`, `if_missing`, or `always` | `if_missing` | Pre-clone incremental models and snapshots from production into dev before a run. | `pre-clone.md` |
| `execute_hooks_on_any_reuse` | Node, folder, or project | bool | `false` | Run pre- and post-hooks even on in-place reuse. They always run on clone. | `execute-hooks-on-any-reuse.md` |
| `defer_to_target` | `profiles.yml` output (self-managed only) | Target name | `prod` | Which target to defer to and compare against. | `defer-to-target.md` |
| `allow_clones` | `profiles.yml` output, or a platform extended attribute | bool | `true` | Whether State may clone into this target. | `allow-clones.md` |
| `metadata_warehouse` | `profiles.yml` output, or an extended attribute (Snowflake only) | Warehouse name | Falls back to `warehouse` | Separate warehouse for State metadata queries, run as parallel per-schema queries. | `metadata-warehouse.md` |

**Project, global, and environment settings:**

| Setting | Where | Values / default | Source |
|---|---|---|---|
| `manage_state` (v2.0+) | `flags:` in `dbt_project.yml`, `~/.dbt/user_settings.yml`, env var `DBT_ENGINE_MANAGE_STATE`, CLI `--manage-state` / `--no-manage-state` | bool, default False | `about-global-configs.md:131`, `user-settings.md` |
| `enable_state` (v1 plugin) | Project flag, env var `DBT_ENGINE_ENABLE_STATE` | Values not documented | `dbt-state-setup.md` |
| `dbt-cloud.project-id` | `dbt_project.yml` | Platform project ID | `dbt-state-deferral.md` |
| `dbt-cloud.defer-env-id` | `dbt_project.yml` (or `dbt_cloud.yml`) | Environment ID. Disables State-powered `state:*` selectors. | `dbt-state-deferral.md`, `about-defer.md` |
| `dbt-cloud.state-org-id` | `dbt_project.yml` | State org ID (self-managed, multiple orgs) | `dbt-state-deferral.md`, `faqs/State/multiple-projects.md` |
| `DBT_CLOUD_TOKEN`, `DBT_CLOUD_ACCOUNT_HOST`, `DBT_CLOUD_ACCOUNT_ID`, `DBT_CLOUD_PROJECT_ID` | Environment | Service-token auth for non-interactive runs | `dbt-state-cicd.md`, `login.md` |
| `DBT_ENGINE_STATE_OAUTH_CLIENT_ID`, `DBT_ENV_SECRET_STATE_OAUTH_CLIENT_SECRET` | Environment | Legacy standalone app OAuth | `dbt-state-cicd.md` |
| Source `loaded_at_field` / `loaded_at_query` | Source or model config | Column/expression, or SQL returning one timestamp; mutually exclusive | `freshness.md`, `state-aware-setup.md` |
| Platform UI | Job "Enable dbt State"; development environment "dbt State" section; per-user override (Enabled / Disabled / Reset) | — | `dbt-state-enable-jobs.md`, `dbt-state-enable-studio.md` |

**Internal names that appear only in images and verbose output (undocumented):**

- `clone_time_travel_limit`
- `run_hooks_on_no_op`
- `defer_to`
- "write-only mode"
- "tolerate nondeterminism"
- "clone incremental in dev" (`IF_TABLE_MISSING`)
- "metadata cache TTL" (`0 seconds`, or "infinite (cache never expires)")

**SAO → dbt State mapping** (`dbt-state-migration.md`):

| SAO | dbt State |
|---|---|
| `freshness.build_after.updates_on` | `state.require_fresh_data_from` |
| `build_after.count` + `build_after.period` | `state.lag_tolerance` (for example `{count:1, period:day}` becomes `1d`) |

- In Fusion/v2, if the `state` keys are unset, State falls back to `build_after` "until `build_after` is deprecated". If neither exists, the defaults are `45m` and `any`.
- SAO's `build_after` did **not** rebuild on a SQL change alone. `lag_tolerance` does.

---

## 4. CLI, output formats, artifacts, telemetry, UI

### 4.1 `dbt state explain` (v2) / `dbt-state explain` (v1 plugin)

Source: `docs/reference/commands/state-explain.md`.

- **Purpose.** Explains, after a run, why each node was executed, skipped, or cloned.
- **Default input.** "reads from the most recent execution."
- **Flags.**
  - `--log-file` / `-l <path>`: pick a state log from `logs/state/`. Example file name: `logs/state/responses_2026_08_25_11_00_15_667.jsonl`. **These JSONL "responses" files are the only documented local State artifact. Their schema is NOT documented.**
  - `--verbose`: adds a run-configuration summary and the full step-by-step analysis.
  - `-s <node>`: filter to a node.
- **v2 default output format**, one line per node: `<DECISION_CODE> <unique_id> - <reason text>`. Documented examples:
  - `SKIP_EXECUTION model.jaffle_shop.customers - model was a no-op because its query is up to date and its upstream data is within freshness tolerance`
  - `READY_TO_EXECUTE model.jaffle_shop.orders - model was executed because the view definition is newer than the cached execution`
  - `READY_TO_EXECUTE test.jaffle_shop.not_null_customers_customer_id - data test was executed because it has no prior execution or its query changed`
  - `UNKNOWN unit_test.jaffle_shop.orders.test_order_items_compute_to_bools_correctly - dbt State explain details unavailable`
- **Decision codes seen:** `SKIP_EXECUTION`, `READY_TO_EXECUTE`, `UNKNOWN`. No clone code is shown in the examples. Inference: other codes exist but are undocumented.
- **v2 verbose output.** The run configuration block has these fields:
  - `started at`
  - `profile`
  - `target`
  - `defer to target`
  - `freshness tolerance: 2700 seconds`
  - `tolerate nondeterminism: true`
  - `clone incremental in dev: IF_TABLE_MISSING`
  - `metadata cache TTL: 0 seconds`
  - `select: fqn:my_node_name`

  Per node, it then prints nested sections:
  - `table analysis ("DB"."SCHEMA"."TABLE")` → `the model table exists already [SUCCESS, TARGET_TABLE_EXISTS]`
  - `query analysis` → `the model query has not changed [SUCCESS, NODE_QUERY_UNCHANGED]` and `upstream model queries have not changed [SUCCESS]`
  - `data freshness analysis` → `upstream dependencies` → `"DB"."SCHEMA"."RAW_CUSTOMERS" [FRESH]`, with `no updates since … last executed` and `last updated: 5 days ago`, then `upstream data is up to date [SUCCESS]`
- **v1 plugin output** is tree-formatted. It begins `Last run: 2 minutes ago` and uses labels `[No-op]` and `[Execute]`. Verbose mode adds a `Run configuration` tree with "dbt State:" and "dbt:" sub-branches, where `metadata cache ttl` can be `infinite (cache never expires)`. Freshness tags are `[fresh]` and `[within tolerance]`. It ends with a line such as `decision: [No-op] model was a no-op because …`.
- **Other reason texts documented:**
  - "model was executed because either its query didn't match or its upstream data is out of date"
  - "upstream data is outdated (within tolerance)"
  - "data test was a no-op because both its query and its upstream data are up to date" [img explain-tab]
- **No JSON output flag is documented** for `state explain`.

### 4.2 Run log output (`dbt run` / `dbt build` with State)

Source: `docs/docs/deploy/dbt-state-examples.md`.

- Per-node status suffixes: `[No new changes in 2.73s]`, `[Cloned from other environment in 57.19s]`, `[SUCCESS 1 in 0.54s]`.
- Summary line: `Completed successfully. Total cache hits: 12. Estimated time saved: 17.77s. Freshness tolerance: 45m.`
- Result counts gain `REUSED=`: `Done. PASS=0 WARN=0 ERROR=0 SKIP=0 NO-OP=0 REUSED=12 TOTAL=12`.
- Observations:
  - Reused nodes are counted neither as PASS nor as NO-OP.
  - On the second run there was a partial-parse miss ("Unable to do partial parsing because of a version mismatch") and 658 macros instead of 644. Inference: the plugin injects macros.

### 4.3 Telemetry (v2)

Source: `docs/reference/telemetry-observability.md`.

- The OpenTelemetry-based system is "backed by a stable protobuf schema" and is additive-only. Formats: JSONL (`--otel-file-name`, `--log-format otel`), Parquet (`--otel-parquet-file-name`, written to `target/metadata/`), and OTLP (`--export-to-otlp`).
- `node_outcome` values: `success`, `error`, `skipped`, `canceled`. In JSONL these appear as `NODE_OUTCOME_SKIPPED`.
- `skip_reason` values: `upstream`, **`cached`** ("reused results from cache (no changes detected via dbt State)"), `phase_disabled`, `noop`. The JSONL attribute is `node_skip_reason`, with `node_skip_upstream_detail.upstream_unique_id`.
- "Fusion adds `skip_reason: cached` for nodes reused via dbt State, which has no Core equivalent."
- Record envelope fields: `record_type` (`SpanStart` / `SpanEnd` / `LogRecord`), `trace_id`, `span_id`, `parent_span_id`, `span_name`, `start_time_unix_nano`, `end_time_unix_nano`, `severity_text`, `event_type` (for example `v1.public.events.fusion.node.NodeEvaluated`), and `attributes`.
- No State-specific decision event (clone vs skip, reason chain) is documented in telemetry. The distinction between clone and in-place skip in telemetry is **not documented**.
- On the platform: `telemetry-<STEP>-otel.parquet` artifacts, available through the Admin API.

### 4.4 Platform UI

Sources: `docs/docs/deploy/dbt-state-interface.md`, `snippets/_dbt-state-explain-tab.md`, and [img explain-tab.png].

- **Explain tab.** On a run, go to Orchestration → Runs → run → **Explain**. It has an "Explain results" table with one row per resource, search, and **Download** (release notes mention both a text file and CSV). It updates live while the run is in progress. The per-row fields are:
  - **Resource name**
  - **Resource type**
  - **Decision**
  - **Run step** (for example `dbt build --exclude tag:ml_pipeline`)
  - **Table analysis**
  - **Query analysis**
  - **Data freshness analysis**

  The screenshot also shows a collapsible "Run configuration" section and a "Reused" badge.
- **Usage metrics.** Billing & Usage → Usage-based features → State tab shows:
  - "Models reused this month"
  - "Total % build reduction"
  - "Total query run time reduction"
  - a DATT chart split into Billable and Free
  - an "Asset builds" chart

  The model build chart series are Built, Reused (no-op), and Reused (cloned) (`dbt-platform-release-notes-gen.md:526`).
- **Lag tolerance recommendations.** On the dbt State page.
  - Columns: Model name, Project, Current lag, Recommended lag, % time saved, Projected 30d time savings.
  - Algorithm: take 30 days of build history. Exclude models with fewer than 10 builds. Identify builds where the definition and inputs were unchanged since the previous build. Estimate savings per candidate value. Recommend "the smallest value that would have saved more than 30 minutes."
  - Views are excluded, and the top 20 models by projected savings are shown.
- **Other surfaces.**
  - Account home chart of models built vs reused.
  - Structured logs with a **Reused** filter.
  - Catalog "Latest status" lens tag **Reused**.
  - Cost Insights: estimated warehouse cost reduction. It needs at least one full build in the last 10 days as a baseline and is model-level only for now (`docs/docs/explore/cost-insights.md`, `snippets/_cost-insights-sao.md`).

### 4.5 `dbt login` family (v2)

Source: `docs/reference/commands/login.md`.

- `dbt login`, `dbt login status` (`Status: authenticated (via METHOD)` or `Status: unauthenticated`), and `dbt license info [--json]`.
- Messages: `Congratulations! You are now signed in.` and `Authentication failed. Re-run dbt login to try again.`

### 4.6 What is NOT publicly documented

- The server API or protocol.
- The server-side storage schema.
- The hash algorithm and the normalization rules in detail.
- The `logs/state/responses_*.jsonl` schema.
- A JSON output for `state explain`.
- Retention periods (the docs point to the privacy policy).
- The complete set of decision codes.
- How clones work on non-Snowflake warehouses.
- Microbatch behavior.
- Failure and partial-run state semantics.
- The semantics of `clone_time_travel_limit` and chain depth.

---

## 5. Pricing, billing, and trial

**Unit: DATT (daily active target table)** (`snippets/_dbt-state-pricing.md`, included in `docs/docs/platform/billing/dbt-state-usage.md` and `docs/docs/deploy/dbt-state-trial.md`)

- "the number of distinct target tables … for which dbt State performs at least one of the following unique operations on a given day (based on UTC time): a skip, clone, or test reuse."
- A target table is "a database object managed by your dbt project for a given database and schema name". It includes seeds, snapshots, and models (including incremental ones), and "each distinct test (even if … `store_failures` is disabled)". Example: a model with `not_null` and `unique` tests counts as 3.
- Multiple reuses per UTC day count once.
- Builds are not billed by State. **You pay for reuse, not for builds.** A reused model "doesn't count as a Successful Model Built". It is billed as a DATT instead (`docs/docs/platform/billing/how-pricing-works.md:43`).

**Price and billing mechanics**

- Price: **USD $0.094 per DATT**, summed over all users and days in the billing period. Example: 100 DATT = $9.40. The current price is in the dbt Labs Service Consumption Table. Release notes phrase it as "$0.094 per daily unique reuse" (`release-notes.md:216`).
- Billed per table, "not per dbt platform seat" (`dbt-state-trial.md`).
- **Trial.** 30 days free, "with no usage limit", for eligible new organizations. It can't be paused. After it ends, a credit card (self-serve) or enterprise contract (managed) is required. SAO users from before 2026-06-01 get an extended trial (`snippets/_dbt-state-trial-how-it-works.md`).
- **Managed accounts.** "Allow" billing against committed consumption spend. The purchased consumption pool is shared with dbt Wizard, while the free Wizard credits can't be used for State (`snippets/_wizard-billing-faqs.md`, `docs/docs/dbt-ai/pricing-billing/overview.md:47`).
- **Spend alerts.** An email threshold in USD per month (`dbt-state-trial.md`). A separate feature-level spend limit exists, distinct from Wizard's (`trial-and-billing.md:113`).
- **Cancellation.** Usage is billed at month-end up to the cancellation date.

**Metering visibility**

- DATT chart (Billable/Free). During the trial, all DATTs are free.
- The legacy app had a Billing tab showing DATTs.

---

## 6. Limitations, caveats, gotchas, recommendations

**Scope limits**

- Warehouses: Snowflake, Databricks, BigQuery, and Redshift only (`dbt-state-setup.md`).
- Python models and custom materializations are never reused (`dbt-state-about.md`).
- The service is hosted only in a US GCP region, as a single multi-tenant instance. There is no EU or self-hosted option (`access-regions-ip-addresses.md:22`, `faqs/State/data-storage.md`).
- `metadata_warehouse` is Snowflake-only.

**Behaviors that cause rebuilds**

- `select *` views directly on `ref()` or `source()` always rebuild.
- Non-deterministic Jinja (`env_var`, `invocation_id`, ordering from introspective macros) rebuilds unless `compare_unrendered_code: true`.
- BigQuery external sources always rebuild unless `loaded_at_*` is set.
- The incremental first-run to incremental transition changes compiled SQL and forces downstream rebuilds (`views-rebuilt.md`, `lag-tolerance.md`, `dbt-state-migration.md`).

**Correctness risks**

- Concurrent jobs on the same incremental model or snapshot can cause duplicate records or data corruption (`dbt-state-migration.md`).
- Without a manifest, dynamic `generate_schema_name` or `generate_alias_name` can make State guess the wrong object. The alias case is "most likely to cause data corruption" (`defer-to-target.md`).
- Tables edited manually outside dbt: freshness is based on modification time, so a manual edit can make a table look fresh and clonable (`allow-clones.md`).
- Late-arriving data with event-time `loaded_at` may go undetected (`materializations-guide-4-incremental-models.md`).
- Volatile functions are treated as static by default, so a model using `current_date()` can go stale unless `evaluate_volatile_sql: true` (`evaluate-volatile-sql.md`).

**Operational issues**

- If `manage_state: true` locally but State isn't enabled on the account, `dbt run` and `dbt build` fail (`user-settings.md`).
- A non-interactive run without credentials disables State with a warning (`dbt-state-cicd.md`).
- Metadata queries can queue on the main warehouse; there is a 15 s warning (`metadata-warehouse.md`).

**Missing features**

- No concurrent-build detection or model-level queueing.
- No Efficient Testing (test aggregation) (`dbt-state-migration.md`).

**Recommendations from dbt Labs**

- `lag_tolerance: "{{ '4h' if target.name == 'prod' else '7d' }}"`.
- Use the lag recommendations page.
- Keep using selectors in development to avoid DATT charges and extra State activity (`optimize-costs.md:35`, blog).
- Avoid conditional materializations across environments (`optimize-costs.md:39`).
- Use explicit column lists.
- Provide a manifest when names are dynamic.
- Use a `metadata_warehouse`.
- Invite the whole team to get more clone hits.
- Roll out with `flags: manage_state: true` in `dbt_project.yml` (blog).
- Automatic `state:modified` selection in development "may be supported in a future release" (`optimize-costs.md:35`).

---

## 7. Claims and metrics

- "reduce warehouse costs by 30%+" (`docs/docs/dbt/dbt-readiness.md:93`). "💰 30%+ reduction in warehouse costs (with dbt State)" and "can reduce warehouse costs by 30% or more" (`docs/guides/dbt-upgrade.md:23,361`).
- Example numbers: a second run of 12 models was all reused, "Estimated time saved: 17.77s", although total wall time was 19.79s versus 17.77s without State. The metadata-fetch overhead is visible in this small example (`dbt-state-examples.md`).
- Lag recommendations report projected savings based on 30 days of data (`dbt-state-interface.md`).
- Cost Insights figures are retroactive estimates, "_not_ forecasts" (`cost-insights.md`).
- dbt v2 speed claims (not State-specific): 10k-node compile 70 s → 17 s; "20 minutes to parse now complete in less than one"; "2x or more faster compilation" (`blog/2026-09-16-dbt-v2-is-ga.md`).
- No independently verified or benchmarked State savings figures are published in the docs.

---

## 8. dbt v2 / dbt OSS context (licensing, artifacts)

**Renames on 2026-09-16** (`blog/2026-09-16-comparing-dbt-and-dbt-oss.md`)

- The Fusion engine became **"dbt"**: a free binary with proprietary components, under a "license designed to permit broad adoption" (`getdbt.com/licenses-faq`).
- dbt Core v2 became **"dbt OSS"**: 100% Apache 2.0, and "the same code that was initially released as dbt Core 2.0".
- Both "Support optional usage-based paid features (e.g. dbt State, dbt Wizard)".
- Only "dbt" has advanced local features (SQL comprehension, linting, LSP) and seat-based paid features (Mesh, Catalog).
- Installation: `pip install dbt` for the full distribution, or `pip install dbt-oss`. `pip install dbt-core` currently installs dbt OSS.

**Licensing history** (`blog/2026-06-01-dbt-core-v2-is-here.md`)

- On 2026-06-01, the Rust runtime previously planned as ELv2 was relicensed to Apache 2.0 as dbt Core v2 and moved into the `dbt-core` repo, later described as the `dbt` repo (`github.com/dbt-labs/dbt`). The `dbt-fusion` repo was archived.
- The Fusion binary was relicensed "more permissive than ELv2". It "can now be provided as a managed service by others", but such providers "must allow end users to enable these premium features."
- Premium features are "unlocked with a free login or payment method".

**What is proprietary**

- The State service is proprietary. The predecessor was described explicitly: "additional cloud-backed services necessary to deliver platform-specific features, such as State-Aware Orchestration. That code is proprietary" (`blog/2025-05-28-dbt-fusion-engine-components.md:90`).
- dbt State is "backed by a new paid service" (`blog/2026-06-17-how-dbt-state-works.md`).
- Inference: the State client hooks in dbt OSS may be open, but the decision logic runs server-side and is proprietary. The docs do not say which client parts are open.

**Timeline**

- 2026-06-01: dbt Labs + Fivetran announced dbt State. SAO was closed to new customers (`faqs/Runs/what-happened-to-sao.md`, `release-notes.md:214-217`).
- dbt Core 1.13 will be the final 1.x minor release, with critical patches expected for 3–5 years (`comparing-dbt-and-dbt-oss.md`).

**Artifacts in v2** (`blog/2026-09-16-dbt-v2-is-ga.md`)

- The **dbt Information Schema** is a "contracted interface" of Parquet files, queryable with `dbt show --info` or DuckDB. It covers "model lineage, execution history". The blog claims it is more than 10x smaller than JSON.
- `--no-write-json` / `DBT_ENGINE_WRITE_JSON=0` and `--write-index` switch output to Parquet.
- The blog says `manifest.json`-style information is also available in Parquet. These artifacts are open, but State's own storage is separate and undocumented.
- `dbt-autofix`, and `--use-v2-parser` in 1.12, support migration.

**Relevance to ODS (clean-room observations; my inference, not from dbt docs)**

- dbt State's public behavior is a useful checklist of conservative-default gaps ODS can close in the open:
  - make failure semantics explicit,
  - add concurrency locking or leases,
  - document storage schemas and explain output in JSON,
  - handle the `select *`, volatile-SQL, and alias-guessing edge cases,
  - offer a self-hosted, region-local store.
- Build ODS on public inputs only: `manifest.json` or v2 Parquet artifacts, warehouse `INFORMATION_SCHEMA` timestamps, and `loaded_at_*`.
