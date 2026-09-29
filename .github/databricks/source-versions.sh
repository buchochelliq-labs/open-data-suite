#!/usr/bin/env bash
# Delta table versions as source versions (ADR-0022, #17), from the `databricks`
# workflow, after demo.sh, in its project copy and the run's schema. A Delta source
# table and a model reading it are added; then `ods state build` three times:
#   1. the model builds (never built), and the table's version is recorded;
#   2. it is reused, and its plan evidence shows the `delta_history` version;
#   3. after an INSERT into the source, it builds again (new upstream data).
#
# Needs what demo.sh needs; the source table is dropped with the run's schema.
set -euo pipefail

: "${ODS:?}" "${ODS_CI_CATALOG:?}" "${ODS_CI_SCHEMA:?}"
ci=(python3 "$PWD/.github/databricks/ci.py")
project="${RUNNER_TEMP:-/tmp}/jaffle-databricks"
[ -d "$project" ] || { echo "::error::run demo.sh first: $project is its project"; exit 1; }
py="$(dirname "$(command -v dbt)")/python"
table="\`$ODS_CI_CATALOG\`.\`$ODS_CI_SCHEMA\`.ods_source_events"

"${ci[@]}" sql "CREATE TABLE $table (id INT, amount INT) USING DELTA"
"${ci[@]}" sql "INSERT INTO $table VALUES (1, 10), (2, 20)"

cd "$project"
cat > models/ods_ci_sources.yml <<'YAML'
version: 2
sources:
  - name: ods_ci
    schema: "{{ env_var('ODS_CI_SCHEMA') }}"
    tables:
      - name: events
        identifier: ods_source_events
YAML
cat > models/marts/events_total.sql <<'SQL'
select count(*) as events, sum(amount) as amount from {{ source('ods_ci', 'events') }}
SQL

# `ods state build --json`, then checks on the result: `check <python expression>`,
# where `entry` is events_total's plan entry and `result` the whole result.
build() {
  echo "::group::ods state build ($1)"
  "$ODS" state build --profiles-dir . --dbt-output capture --json > "build-$1.json"
  echo "::endgroup::"
}
check() {
  local run=$1 what=$2 expr=$3
  "$py" - "build-$run.json" "$expr" <<'PY' || { echo "::error::run $run: expected $what"; exit 1; }
import json, sys
result = json.load(open(sys.argv[1]))["result"]
entry = next(e for e in result["plan"]["entries"] if e["name"] == "events_total")
ok = eval(sys.argv[2], {"result": result, "entry": entry})
print(json.dumps({k: entry[k] for k in ("action", "reasons", "evidence")}, indent=1))
sys.exit(0 if ok else 1)
PY
}

build 1
check 1 "events_total to build" 'entry["action"] == "build"'
check 1 "the source version recorded" 'result["record"]["sources_recorded"]'

# ODS compares times to the second.
sleep 2
build 2
check 2 "events_total to be reused" 'entry["action"] == "reuse"'
check 2 "delta_history evidence" 'any(e["kind"] == "source_version_origin" and e["value"] == "delta_history" for e in entry["evidence"])'
check 2 "a table version as the data version" 'any(e["kind"] == "source_data_version" and "/" in (e["value"] or "") for e in entry["evidence"])'

"${ci[@]}" sql "INSERT INTO $table VALUES (3, 30)"
sleep 2
build 3
check 3 "events_total to build on new data" 'entry["action"] == "build" and entry["reasons"][0]["code"] == "new_upstream_data"'
echo "source versions: build, reuse, build after an INSERT"
