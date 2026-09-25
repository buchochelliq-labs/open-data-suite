#!/usr/bin/env bash
# Regenerates the committed dbt artifacts for this fixture (#100).
#
# Needs a Python environment with dbt-core and dbt-duckdb installed, e.g.
#   python3 -m venv .venv && .venv/bin/pip install "dbt-core~=1.10.0" "dbt-duckdb~=1.10.0"
#   DBT=.venv/bin/dbt ./regenerate.sh
# Everything runs locally against DuckDB; no network or warehouse is used.
set -euo pipefail
cd "$(dirname "$0")"
DBT="${DBT:-dbt}"
export DBT_SEND_ANONYMOUS_USAGE_STATS=false

"$DBT" build --profiles-dir . --quiet
"$DBT" docs generate --profiles-dir . --quiet

version="$("$DBT" --version | sed -n 's/.*installed: *\([0-9]*\.[0-9]*\).*/\1/p' | head -n1)"
out="artifacts/dbt-${version}"
mkdir -p "$out"
root="$(pwd)"
for artifact in manifest catalog run_results; do
  # Machine-specific paths are replaced so the artifacts are identical everywhere.
  sed "s#${root}#<project_root>#g" "target/${artifact}.json" > "${out}/${artifact}.json"
done
echo "wrote ${out}"
