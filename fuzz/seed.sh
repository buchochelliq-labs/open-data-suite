#!/usr/bin/env bash
# Seeds each fuzz target's corpus from the repository's fixtures, so fuzzing starts
# from real files rather than from nothing. Run from the repository root.
set -euo pipefail
cd "$(dirname "$0")/.."
seed() { mkdir -p "fuzz/corpus/$1"; shift; for f in "$@"; do [ -f "$f" ] && cp "$f" "fuzz/corpus/$target/$(echo "$f" | tr / _)"; done; }
target=dbt_manifest; seed "$target" $(find fixtures -name manifest.json)
target=dbt_run_results; seed "$target" $(find fixtures -name run_results.json)
target=dbt_messages; seed "$target" $(find fixtures -path '*errors*' -name '*.json')
target=sql_analyzer; seed "$target" $(find fixtures -name '*.sql' | head -200)
target=uc_lineage; seed "$target" $(find fixtures/databricks -name 'column_lineage.*')
target=redact; seed "$target" $(find fixtures -path '*errors*' -name '*.json')
