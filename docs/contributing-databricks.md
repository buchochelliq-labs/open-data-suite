# Testing against Databricks

ODS's Databricks code is tested two ways:

- **Everywhere:** against fixtures in `fixtures/databricks/`, as part of `cargo test`.
- **Nightly, on demand, or when a PR is labelled `databricks`:** against a real Databricks
  Free Edition workspace, by the `databricks` workflow (#294). This page covers the
  second.

## What the workflow does
1. **It authenticates**, trying these in order:
   1. **Workload identity federation.** GitHub issues the job an OIDC token, which
      Databricks exchanges for a workspace token that lasts about an hour. The exchange
      happens under a federation policy on the CI service principal, so no secret is
      stored.
   2. **The service principal's OAuth secret**, if federation isn't set up or is
      refused.

   The job summary says which one it used. The token is masked in the logs and never
   written anywhere else.
2. **It runs a smoke check** through the SQL Statement API:
   - who the token belongs to (`current_user()`), and that the warehouse answers;
   - a schema for the run, `ods_ci_<run id>_<attempt>`, with a table written and read
     in it;
   - dropping that schema. This step runs even if an earlier one failed.

3. **It runs `ods state` on the demo project**, `.github/databricks/demo.sh`, in the
   run's schema, with the pinned dbt-databricks in
   `.github/dbt/requirements-databricks.txt`:
   - it seeds and builds everything;
   - it checks that nothing is left to build;
   - it changes `customers`, and checks that the plan is exactly `customers` and the
     view reading it;
   - it builds those.

   `.github/databricks/prepare-project.py` copies `fixtures/dbt/jaffle-ods` for
   Databricks. It leaves out the Python model, which uses DuckDB's relation API, and the
   DuckDB-typed contracts. To rehearse on DuckDB, set `ODS_CI_PROFILES_YML` to a DuckDB
   `profiles.yml` and put a dbt with dbt-duckdb on the `PATH`.
4. **It checks Delta table versions as source versions** (ADR-0022),
   `.github/databricks/source-versions.sh`, in the demo's project and the run's schema.
   It creates a Delta table, adds a dbt source for it and a model reading it, then runs
   `ods state build` three times: the model builds, then is reused (its plan evidence
   shows the `delta_history` version), then builds again after an `INSERT` into the
   table. The table goes with the run's schema.

Runs are serialised, because Free Edition has a single 2X-Small warehouse.

## Screenshots for the docs
The demo prints each command's styled output between `ods-transcript-begin NAME` and
`ods-transcript-end NAME` in the job log. To refresh the screenshots in `docs/images/`,
download the job's log and render it:

```bash
python scripts/render-transcripts.py job.log docs/images plan-after-change build-after-change
```

Download the log from the run's page (**⋯ → Download log**) or with
`gh run view <run> --log`. The renderer replaces workspace hostnames with
`<workspace>`. The job masks the hostname in its own log as well. This needs Node.js
with Playwright and its Chromium. The screenshots appear on
[State on Databricks](databricks.md). The job isn't
a required check.

## Setting up the workspace
Do this once, as a workspace admin.

1. **Service principal:** Settings → Identity and access → Service principals → Add.
   Note its **application ID**.
2. **Warehouse access:** SQL Warehouses → your warehouse → Permissions. Give the service
   principal **Can use**.
3. **Catalog access:** in the SQL editor, run:
   ```sql
   GRANT USE CATALOG, CREATE SCHEMA ON CATALOG workspace TO `<application-id>`;
   ```
   The service principal owns the schemas it creates, so it needs no other grants.
4. **Federation (preferred):** add a federation policy to the service principal. You do
   this in the account console, or with `databricks account service-principal-federation-policy create`.
   The policy needs:
   - **Issuer:** `https://token.actions.githubusercontent.com`
   - **Audience:** your Databricks account ID. If you choose another audience, set it
     as `DATABRICKS_TOKEN_AUDIENCE` below.
   - **Subject:** `repo:buchochelliq-labs@285328036/open-data-suite@1382703972:environment:databricks-free`.
     This repository's OIDC subjects include the owner's and repository's immutable
     IDs, so the plain `repo:owner/name:…` form doesn't match. When federation is
     refused, the job summary prints the exact issuer, subject and audience to use.
5. **Fallback:** Service principals → the service principal → Secrets → Generate secret.
   A secret scoped to `sql` is enough. The job asks for `all-apis` first, then `sql`;
   set `DATABRICKS_OAUTH_SCOPES` to ask for something else.
   Skip this if federation works and you'd rather store no secret at all.

### A read-only service principal for probe checks
`ods health check` runs probe checks only under a login that can do no more than read
([ADR-0030 §4c, §4e](adr/0030-configurable-and-pluggable-health-checks.md)). The job
tests that with a second service principal, which reads the run's schema while the CI
one owns it. Until it is set up, those steps are skipped.

The `workspace` catalog grants `USE CATALOG` and `CREATE SCHEMA` to every user, so a
principal there can always create objects through its groups, and ODS (correctly)
refuses to probe under it. The job therefore runs in a catalog of its own, with only
explicit grants:

1. **Catalog:** create it and give the CI service principal what it had on `workspace`:
   ```sql
   CREATE CATALOG ods_ci;
   GRANT USE CATALOG, CREATE SCHEMA ON CATALOG ods_ci TO `<ci-application-id>`;
   ```
   Then set `DATABRICKS_CATALOG` to `ods_ci` (below). Check it has nothing else:
   `SHOW GRANTS ON CATALOG ods_ci` should list only your own ownership and these.
2. **Probe service principal:** add a second one as in step 1 above, with **Can use** on
   the warehouse (step 2), and only:
   ```sql
   GRANT USE CATALOG ON CATALOG ods_ci TO `<probe-application-id>`;
   ```
   Each run's schema is granted to it by the job (`USE SCHEMA`, `SELECT`), which the CI
   principal can do because it owns the schema. Don't make it a workspace admin, an
   account admin or the owner of anything.
3. **Federation:** a policy on the probe principal like step 4's, with the same issuer,
   subject and audience. Or, as a fallback, an OAuth secret for it.
4. **GitHub environment:** set `DATABRICKS_PROBE_CLIENT_ID` (and, for the fallback,
   the secret `DATABRICKS_PROBE_CLIENT_SECRET`), below.

The step checks that a probe on `orders` passes under the probe principal with
`login_check: read_only`, and that the same probe is refused under the CI principal,
which owns the schema. The job summary shows both findings. If the first is refused,
its reason names what Unity Catalog reported: a default metastore grant, for instance
(ADR-0030 §4e, #408).

## Setting up the GitHub environment
Settings → Environments → `databricks-free`:

| Kind | Name | Value |
|---|---|---|
| Variable | `DATABRICKS_HOST` | `https://<workspace>.cloud.databricks.com` |
| Variable | `DATABRICKS_HTTP_PATH` | `/sql/1.0/warehouses/<id>`, from the warehouse's Connection details |
| Variable | `DATABRICKS_CLIENT_ID` | the service principal's application ID |
| Variable | `DATABRICKS_ACCOUNT_ID` | optional: the federation audience, if it's the account ID |
| Variable | `DATABRICKS_TOKEN_AUDIENCE` | optional: the federation audience, if it's something else |
| Variable | `DATABRICKS_CATALOG` | optional; defaults to `workspace`; `ods_ci` for probe checks (above) |
| Variable | `DATABRICKS_PROBE_CLIENT_ID` | optional: the read-only probe service principal's application ID |
| Variable | `DATABRICKS_OAUTH_SCOPES` | optional; defaults to trying `all-apis`, then `sql` |
| Secret | `DATABRICKS_CLIENT_SECRET` | optional: the fallback secret |
| Secret | `DATABRICKS_PROBE_CLIENT_SECRET` | optional: the probe principal's fallback secret |

Settings can also be stored as environment secrets: the workflow reads each one as a
variable first, then as a secret. Variables are easier to check, because secrets are
masked in the logs.

If neither audience variable is set, federation asks for the workspace's token endpoint
as its audience. The policy must then list that endpoint.

**Testing a PR against the workspace:** add the `databricks` label. Each time the
label is added, the commit the PR is at gets one run.
- **Review that commit first.** A PR run uses the PR's own workflow and scripts, and
  they get the environment's credentials.
- **A later push doesn't run again.** To test a new commit, remove the label and add it
  again.
- **Fork PRs never run the job.**

**Protecting the environment:** under **Deployment protection rules**, add yourself as
a **required reviewer**, so every run also waits for your approval. Under **Deployment
branches**, allow `main` and only the PR branches you are testing.

## Running it yourself
The script only needs Python 3.10 or later and the standard library. The `token` step
needs GitHub Actions, so outside CI, get a token with the Databricks CLI, signing in in
your browser (U2M) with your `~/.databrickscfg` profile:

```bash
databricks auth login --host https://<workspace>.cloud.databricks.com
export DATABRICKS_HOST=https://<workspace>.cloud.databricks.com
export DATABRICKS_HTTP_PATH=/sql/1.0/warehouses/<id>
export DATABRICKS_TOKEN="$(databricks auth token --host "$DATABRICKS_HOST" | jq -r .access_token)"
python .github/databricks/ci.py sql "SELECT current_user()"
```

The token stays in your shell. Don't put it in a file in the repository.
