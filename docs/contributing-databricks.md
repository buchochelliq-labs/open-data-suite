# Testing against Databricks

ODS's Databricks code is tested two ways:

- **Everywhere:** against fixtures in `fixtures/databricks/`, as part of `cargo test`.
- **Nightly, on demand, or on a PR labelled `databricks`:** against a real Databricks
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
   - who the token belongs to, and that the warehouse answers;
   - a schema for the run, `ods_ci_<run id>_<attempt>`, with a table written and read
     in it;
   - dropping that schema. This step runs even if an earlier one failed.

Runs are serialised, because Free Edition has a single 2X-Small warehouse. The job isn't
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
   - **Subject:** `repo:buchochelliq-labs/open-data-suite:environment:databricks-free`
5. **Fallback:** Service principals → the service principal → Secrets → Generate secret.
   Skip this if federation works and you'd rather store no secret at all.

## Setting up the GitHub environment
Settings → Environments → `databricks-free`:

| Kind | Name | Value |
|---|---|---|
| Variable | `DATABRICKS_HOST` | `https://<workspace>.cloud.databricks.com` |
| Variable | `DATABRICKS_HTTP_PATH` | `/sql/1.0/warehouses/<id>`, from the warehouse's Connection details |
| Variable | `DATABRICKS_CLIENT_ID` | the service principal's application ID |
| Variable | `DATABRICKS_ACCOUNT_ID` | optional: the federation audience, if it's the account ID |
| Variable | `DATABRICKS_TOKEN_AUDIENCE` | optional: the federation audience, if it's something else |
| Variable | `DATABRICKS_CATALOG` | optional; defaults to `workspace` |
| Secret | `DATABRICKS_CLIENT_SECRET` | optional: the fallback secret |

If neither audience variable is set, federation asks for the workspace's token endpoint
as its audience. The policy must then list that endpoint.

**Deployment branches:** allow `main`. To test a PR against the workspace, label it
`databricks`; if the environment then refuses the PR's branch, allow that branch too.
Fork PRs never run the job.

## Running it yourself
The script only needs Python 3.10 or later and the standard library. The `token` step
needs GitHub Actions, so outside CI, get a token with the Databricks CLI, signing in in
your browser (U2M) with your `~/.databrickscfg` profile:

```bash
databricks auth login --host https://<workspace>.cloud.databricks.com
export DATABRICKS_HOST=https://<workspace>.cloud.databricks.com
export DATABRICKS_HTTP_PATH=/sql/1.0/warehouses/<id>
export DATABRICKS_TOKEN="$(databricks auth token --host "$DATABRICKS_HOST" | jq -r .access_token)"
python .github/databricks/ci.py whoami
python .github/databricks/ci.py sql "SELECT current_user()"
```

The token stays in your shell. Don't put it in a file in the repository.
