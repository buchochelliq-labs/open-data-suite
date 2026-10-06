#!/usr/bin/env bash
# `ods health check`'s probe checks on Databricks, under a read-only login (#392,
# ADR-0030 §4c, §4e), from the `databricks` workflow, after demo.sh built the project.
#
# 1. Under the probe service principal, which can only read the run's schema, a probe
#    on `orders` must run and pass, its login checked as read-only in Unity Catalog.
# 2. Under the CI service principal, which owns the run's schema, the same probe must
#    be refused before it runs, naming the ownership found.
#
# Needs: ODS, `dbt` with dbt-databricks on the PATH, DATABRICKS_HOST,
# DATABRICKS_HTTP_PATH, DATABRICKS_TOKEN, DATABRICKS_PROBE_TOKEN, ODS_CI_CATALOG,
# ODS_CI_SCHEMA.
set -euo pipefail

: "${ODS:?}" "${DATABRICKS_TOKEN:?}" "${DATABRICKS_PROBE_TOKEN:?}"
project="${RUNNER_TEMP:-/tmp}/jaffle-databricks"
py="$(dirname "$(command -v dbt)")/python"
cd "$project"

# The probe lives in a configuration directory of its own, so it is the user's and needs
# no trust entry; `[health.probes]` names the read-only target in profiles.yml.
config="$(mktemp -d)"
mkdir -p "$config/ods"
cat > "$config/ods/config.toml" <<'TOML'
[health.probes]
target = "health_ro"

[[health.checks]]
id = "orders.has_rows"
kind = "probe"
select = { name = ["orders"] }
sql = "select count(*) as n from {relation}"
pass = "n > 0"
severity = "error"
TOML

# `ods health check` as JSON, with `$1` as the probe target's token; exit code ignored,
# the finding is what is checked.
check() {
  DATABRICKS_PROBE_TOKEN="$1" XDG_CONFIG_HOME="$config" "$ODS" health check \
    --profiles-dir . --target databricks --dbt-output capture --no-record --json || true
}

# The finding of `orders.has_rows` on `orders`, checked by `$1` (a Python expression
# over `f`, the finding), and shown in the job summary under `$2`.
expect() {
  "$py" -c '
import json, os, sys
envelope = json.load(sys.stdin)
nodes = (envelope.get("result") or {}).get("nodes") or []
found = [f for n in nodes if n["id"] == "model.jaffle_ods.orders"
         for f in n["findings"] if f["check"] == "orders.has_rows"]
if len(found) != 1:
    sys.exit(f"no orders.has_rows finding on orders: {json.dumps(envelope)[:2000]}")
f = found[0]
ev = f.get("evidence", {})
line = "**%s:** `%s` (login check `%s`): %s" % (sys.argv[2], f["status"], ev.get("login_check"), f["reason"])
print(line)
summary = os.environ.get("GITHUB_STEP_SUMMARY")
if summary:
    with open(summary, "a") as out:
        out.write(line + "\n")
if not eval(sys.argv[1], {"f": f, "ev": ev}):
    sys.exit(f"unexpected finding: {json.dumps(f)}")
' "$1" "$2"
}

echo "::group::probe under the read-only service principal"
check "$DATABRICKS_PROBE_TOKEN" | expect \
  'f["status"] == "pass" and ev.get("login_check") == "read_only" and ev.get("row.n", "0") != "0"' \
  "Probe under the read-only principal"
echo "::endgroup::"

echo "::group::probe under the CI service principal (owns the schema)"
check "$DATABRICKS_TOKEN" | expect \
  'f["status"] == "unknown" and ev.get("login_check") == "refused" and "owner of the schema" in f["reason"]' \
  "Same probe under the schema's owner"
echo "::endgroup::"
