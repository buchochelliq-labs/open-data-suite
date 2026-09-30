#!/usr/bin/env bash
# Captures real dbt error messages for the error catalogue's tests (#323).
#
#   python3 -m venv .venv && .venv/bin/pip install "dbt-core~=1.10.0" "dbt-duckdb~=1.10.0"
#   DBT=.venv/bin/dbt ./capture-errors.sh
#
# Each scenario breaks a fresh copy of this project in one way, runs dbt, and keeps the
# message dbt gave: from `run_results.json` when a node failed, or from dbt's output when
# the whole project failed (e.g. at parse). Machine-specific paths become <project_root>.
# The messages are written to artifacts/dbt-<version>-errors/errors.json, with a secret
# sentinel in some broken code so tests can check it never reaches an explanation.
#
# Two scenarios also keep dbt's JSON log (as artifacts/dbt-1.10-events does: lines at
# info and above, `NodeStart`, `NodeFinished` and `RunResultError`), and the manifest's
# node for the broken model, for end-to-end tests:
#   - missing-column: `stg_customers` renames `first_name` to `given_name`, so
#     `customers` fails in `dbt build`;
#   - unknown-macro: `stg_payments` calls `cent_to_dollars`, so `dbt compile` fails.
#
# Everything runs locally against DuckDB; no network is used by dbt itself.
set -euo pipefail
cd "$(dirname "$0")"
: "${DBT:?set DBT to a dbt 1.x executable with dbt-duckdb (see the top of this file)}"
export DBT_SEND_ANONYMOUS_USAGE_STATS=false
here="$(pwd)"
version="$("$DBT" --version | sed -n 's/.*installed: *\([0-9]*\.[0-9]*\).*/\1/p' | head -n1)"
out="${here}/artifacts/dbt-${version}-errors"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
: > "$work/messages.jsonl"

# Runs one scenario: $1 name, $2 dbt command (build or compile), $3 a shell snippet
# that breaks the copy (run inside it).
scenario() {
  local name="$1" command="$2" edit="$3" dir="$work/$1"
  rm -rf "$dir" && mkdir -p "$dir"
  cp -r dbt_project.yml profiles.yml models macros seeds "$dir/"
  (cd "$dir" && eval "$edit")
  (cd "$dir" && "$DBT" "$command" --profiles-dir . --log-format json --log-level debug \
    > dbt-stdout.jsonl 2> dbt-stderr.txt) || true
  python3 - "$dir" "$name" "$command" >> "$work/messages.jsonl" <<'PY'
import json, pathlib, sys
dir, name, command = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
root = str(dir)
found = []
results = dir / "target" / "run_results.json"
lines = [json.loads(l) for l in (dir / "dbt-stdout.jsonl").read_text().splitlines() if l.startswith("{")]
invocation = next((l["info"].get("invocation_id") for l in lines), None)
if results.exists():
    run = json.loads(results.read_text())
    if run.get("metadata", {}).get("invocation_id") == invocation:
        for r in run["results"]:
            if r["status"] in ("error", "fail") and r.get("message"):
                found.append({"node": r["unique_id"], "message": r["message"]})
if not found:
    # A project-level failure: dbt's own error lines.
    for l in lines:
        if l["info"]["level"] == "error" and l["info"]["name"] in ("MainEncounteredError", "MainStackTrace"):
            found.append({"node": None, "message": l["info"]["msg"]})
            break
import re
for f in found:
    m = f["message"].replace(root, "<project_root>")
    # A Python model's traceback names the interpreter's files and a temporary file.
    m = re.sub(r'File "[^"]*/site-packages/', 'File "<site-packages>/', m)
    m = re.sub(r'/tmp/tmp\w+\.py', '<temporary file>.py', m)
    f["message"] = m
    print(json.dumps({"name": name, "command": "dbt " + command, **f}))
PY
}

keep_log() {
  local name="$1" dir="$work/$1"
  python3 - "$dir" "$out/${name}.jsonl" <<'PY'
import json, pathlib, sys
dir, dest = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
root = str(dir)
kept = []
for raw in (dir / "dbt-stdout.jsonl").read_text().splitlines():
    if not raw.startswith("{"):
        continue
    line = json.loads(raw)
    info = line["info"]
    if info["level"] in ("info", "warn", "error") or info["name"] in ("NodeStart", "NodeFinished", "RunResultError"):
        if info["name"] in ("MainReportArgs",):
            continue
        kept.append(raw.replace(root, "<project_root>"))
dest.write_text("\n".join(kept) + "\n")
PY
}

scenario missing-column build \
  "sed -i 's/^    first_name,/    first_name as given_name,/' models/staging/stg_customers.sql"
keep_log missing-column
scenario unknown-macro compile \
  "sed -i 's/cents_to_dollars(/cent_to_dollars(/' models/staging/stg_payments.sql"
keep_log unknown-macro
python3 - "$work/unknown-macro/target/manifest.json" "$out/unknown-macro-node.json" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
node = m["nodes"]["model.jaffle_ods.stg_payments"]
keep = {k: node[k] for k in ("unique_id", "name", "resource_type", "original_file_path", "path", "raw_code", "depends_on")}
open(sys.argv[2], "w").write(json.dumps(keep, indent=2, sort_keys=True) + "\n")
PY
scenario unqualified-column build \
  "sed -i 's/count(order_id) as number_of_orders,/count(order_ref) as number_of_orders,/' models/marts/customers.sql"
scenario missing-relation build \
  "printf 'select * from main.no_such_table\n' > models/marts/broken.sql"
scenario missing-ref build \
  "printf \"select * from {{ ref('no_such_model') }}\n\" > models/marts/broken.sql"
scenario template-syntax build \
  "printf \"select * from {{ ref('orders' }}\n\" > models/marts/broken.sql"
scenario type-mismatch build \
  "printf \"select cast('sk_live_SENTINEL_42' as integer) as id\n\" > models/marts/broken.sql"
scenario missing-function build \
  "printf \"select no_such_function(order_id) as x from {{ ref('orders') }}\n\" > models/marts/broken.sql"
scenario packages-missing build \
  "printf 'packages:\n  - package: dbt-labs/dbt_utils\n    version: 1.3.0\n' > packages.yml"
scenario profile-missing build \
  "sed -i 's/^profile: jaffle_ods/profile: no_such_profile/' dbt_project.yml"
scenario python-exception build \
  "sed -i 's/return customers.project(/raise KeyError(\"sk_live_SENTINEL_42\")\n    return customers.project(/' models/marts/customer_segments.py"
scenario dependent-objects build \
  "true"
# `dependent-objects` needs a database that already has the tables: build twice.
(cd "$work/dependent-objects" && "$DBT" build --profiles-dir . --log-format json --log-level debug > dbt-stdout.jsonl 2>&1) || true
python3 - "$work/dependent-objects" >> "$work/messages.jsonl" <<'PY'
import json, pathlib, sys
dir = pathlib.Path(sys.argv[1])
run = json.loads((dir / "target" / "run_results.json").read_text())
for r in run["results"]:
    if r["status"] == "error":
        print(json.dumps({"name": "dependent-objects", "command": "dbt build", "node": r["unique_id"], "message": r["message"].replace(str(dir), "<project_root>")}))
PY
scenario test-failure build \
  "mkdir -p tests && printf \"select * from {{ ref('stg_orders') }} where status <> 'placed'\n\" > tests/only_placed_orders.sql"

python3 - "$work/messages.jsonl" "$out/errors.json" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
open(sys.argv[2], "w").write(json.dumps(rows, indent=2, sort_keys=True) + "\n")
PY
echo "wrote ${out}"
