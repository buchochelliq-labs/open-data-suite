#!/usr/bin/env bash
# `ods state` against Databricks on the demo project (#294), from the `databricks`
# workflow. Builds everything, then changes one model and shows that only it and what
# reads it are rebuilt.
#
# Each command's styled output is printed between `ods-transcript` markers too, for
# the screenshots in docs/ (rendered with scripts/render-transcripts.py).
#
# Needs: ODS (the ods binary), `dbt` with dbt-databricks on the PATH, DATABRICKS_HOST,
# DATABRICKS_HTTP_PATH, DATABRICKS_TOKEN, ODS_CI_CATALOG, ODS_CI_SCHEMA.
set -euo pipefail

: "${ODS:?}" "${ODS_CI_CATALOG:?}" "${ODS_CI_SCHEMA:?}"
project="${RUNNER_TEMP:-/tmp}/jaffle-databricks"
# dbt's own Python has PyYAML, which the runner's Python doesn't.
py="$(dirname "$(command -v dbt)")/python"
"$py" .github/databricks/prepare-project.py "$project"
cd "$project"

# `dbt` from the PATH, so the output shows the command a person would type.
dbt=(--profiles-dir .)

# The styled output, as a person sees it, kept for the docs.
show() {
  local name=$1
  shift
  echo "::group::ods $*"
  echo "ods-transcript-begin $name"
  local code=0
  "$ODS" "$@" -o human --color always --width 100 2>&1 || code=$?
  echo "ods-transcript-end $name"
  echo "::endgroup::"
  return "$code"
}

# How many nodes a dry run would build.
to_build() {
  "$ODS" state build --dry-run "${dbt[@]}" --json |
    "$py" -c 'import json,sys; print(json.load(sys.stdin)["result"]["build"])'
}

show seed state seed "${dbt[@]}" --dbt-output capture
show build-first state build "${dbt[@]}" --dbt-output capture

# Everything was just built: nothing to do.
built=$(to_build)
[ "$built" = 0 ] || { echo "::error::expected nothing to build after a full build, got $built"; exit 1; }

# One model changes: only it and what reads it are planned.
sed -i 's/lifetime_value >= 20/lifetime_value >= 25/' models/marts/customers.sql
show plan-after-change state build --dry-run "${dbt[@]}" --dbt-output capture
built=$(to_build)
[ "$built" = 2 ] || { echo "::error::expected customers and customers_snapshot_view to build, got $built"; exit 1; }
show build-after-change state build "${dbt[@]}" --dbt-output capture
show history state history
