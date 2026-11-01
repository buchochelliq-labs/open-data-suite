#!/usr/bin/env bash
# Regenerates the committed artifacts of this fixture (#352): a project that declares a
# semantic layer (semantic models and metrics), so the dashboard's Semantic layer page
# is tested on a real dbt manifest. `parse` needs no warehouse connection.
#
#   python3 -m venv .venv && .venv/bin/pip install "dbt-core~=1.10.0" "dbt-duckdb~=1.10.0"
#   DBT=.venv/bin/dbt ./regenerate.sh
set -euo pipefail
cd "$(dirname "$0")"
export DBT_SEND_ANONYMOUS_USAGE_STATS=false
root="$(pwd)"
"$DBT" parse --profiles-dir . --quiet
version="$("$DBT" --version | sed -n 's/.*installed: *\([0-9]*\.[0-9]*\).*/\1/p' | head -n1)"
out="artifacts/dbt-${version}"
mkdir -p "$out"
# Machine-specific paths are replaced so the artifacts are identical everywhere.
for artifact in manifest; do
  sed "s#${root}#<project_root>#g" "target/${artifact}.json" > "${out}/${artifact}.json"
done
echo "wrote ${out}"
