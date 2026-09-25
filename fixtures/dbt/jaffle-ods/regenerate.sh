#!/usr/bin/env bash
# Regenerates the committed dbt artifacts for this fixture (#100).
#
# dbt 1.x (JSON artifacts, built against DuckDB):
#   python3 -m venv .venv && .venv/bin/pip install "dbt-core~=1.10.0" "dbt-duckdb~=1.10.0"
#   DBT=.venv/bin/dbt ./regenerate.sh
#
# dbt v2 (manifest.json plus the Parquet "dbt Information Schema"). `compile` renders SQL
# without a warehouse connection, so no database driver is needed:
#   python3 -m venv .venv2 && .venv2/bin/pip install "dbt-oss==2.0.5"
#   DBT_V2=.venv2/bin/dbt ./regenerate.sh
#
# Everything runs locally; no warehouse or network is used by dbt itself.
set -euo pipefail
cd "$(dirname "$0")"
export DBT_SEND_ANONYMOUS_USAGE_STATS=false
root="$(pwd)"

# Machine-specific paths are replaced so the artifacts are identical everywhere.
scrub() { sed "s#${root}#<project_root>#g" "$1" > "$2"; }

if [[ -n "${DBT:-}" ]]; then
  # A fresh database: the foreign key constraint on `orders` blocks replacing it.
  rm -f target/jaffle_ods.duckdb
  "$DBT" build --profiles-dir . --quiet
  "$DBT" docs generate --profiles-dir . --quiet
  version="$("$DBT" --version | sed -n 's/.*installed: *\([0-9]*\.[0-9]*\).*/\1/p' | head -n1)"
  out="artifacts/dbt-${version}"
  mkdir -p "$out"
  for artifact in manifest catalog run_results; do
    scrub "target/${artifact}.json" "${out}/${artifact}.json"
  done
  echo "wrote ${out}"

  # A real `dbt build`: its manifest and run results come from the same invocation, as
  # `ods state record` requires (`run_results.json` above is `docs generate`'s).
  rm -rf target-build
  rm -f target/jaffle_ods.duckdb
  "$DBT" build --profiles-dir . --quiet --target-path target-build
  out="artifacts/dbt-${version}-build"
  mkdir -p "$out"
  for artifact in manifest run_results; do
    scrub "target-build/${artifact}.json" "${out}/${artifact}.json"
  done
  echo "wrote ${out}"
fi

if [[ -n "${DBT_V2:-}" ]]; then
  rm -rf target-v2
  # Compiling the Python model needs dbt's DuckDB driver, which dbt-oss downloads from
  # its CDN; offline that one node fails. Its compiled code isn't needed (ODS treats
  # Python models as opaque), so carry on as long as the artifacts were written.
  "$DBT_V2" compile --profiles-dir . --generate-info-schema --target-path target-v2 || true
  test -s target-v2/manifest.json
  version="$("$DBT_V2" --version | sed -n 's/^dbt[-a-z]* \([0-9]*\.[0-9]*\).*/\1/p' | head -n1)"
  out="artifacts/dbt-${version}"
  rm -rf "$out" && mkdir -p "$out/info_schema/v1"
  scrub target-v2/manifest.json "${out}/manifest.json"
  cp target-v2/info_schema/v1/*.parquet "${out}/info_schema/v1/"
  # dbt-oss writes compiled SQL to files rather than into the Information Schema.
  (cd target-v2 && find compiled -name '*.sql' ! -path '*generic_tests*' -exec install -D {} "../${out}/{}" \;)
  rm -rf target-v2
  echo "wrote ${out}"
fi

if [[ -z "${DBT:-}${DBT_V2:-}" ]]; then
  echo "set DBT (dbt 1.x) and/or DBT_V2 (dbt v2); see the comments at the top" >&2
  exit 2
fi
