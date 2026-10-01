# Sourced, hidden, at the start of every tape in docs/tapes/ (see scripts/record-docs.sh).
#
# Makes a scratch copy of the demo dbt project (fixtures/dbt/jaffle-ods) and puts the
# repository's fake dbt (fixtures/dbt/fake-dbt) first on PATH, so recordings need no
# warehouse, no network and no Python packages, and give the same output every time.
# The fake dbt serves the committed dbt 1.10 artifacts; `ods_edit` changes a model's code
# in what it compiles, as editing the model's SQL would.
#
# The project lives at a fixed path so that paths ODS prints are the same on every run.
# ODS_DEMO_ROOT moves it (the dashboard tours and browser tests don't print paths), so
# several can run at once.

demo_root=${ODS_DEMO_ROOT:-/tmp/ods-demo}
rm -rf "$demo_root" && mkdir -p "$demo_root/home" "$demo_root/jaffle_shop/.fake"
export HOME="$demo_root/home" XDG_CONFIG_HOME="$demo_root/home/.config"
cd "$demo_root/jaffle_shop" || return
cp -R "$REPO/fixtures/dbt/jaffle-ods/"{dbt_project.yml,profiles.yml,models,seeds,macros} .
cp "$REPO/fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-build/manifest.json" .fake/
# Fixed modification times: `ods doctor` names the newest project file.
find . -exec touch -d 2026-01-01T00:00:00Z {} +
touch -d 2026-01-01T00:00:01Z dbt_project.yml
export FAKE_DBT_BASE="$PWD/.fake"
# Rows the adapter reports for these nodes; the others report none, as many adapters
# don't for views, so run totals read "at least N".
export FAKE_DBT_ROWS=raw_customers=100,raw_orders=99,raw_payments=113,stg_orders=99,orders=99,customers=100
export PATH="$REPO/fixtures/dbt/fake-dbt:$PATH"

# ods_edit NODE: change NODE's code in what the fake dbt compiles.
ods_edit() {
  python3 - "$1" <<'PY'
import json, sys
path = ".fake/manifest.json"
manifest = json.load(open(path))
node = manifest["nodes"][f"model.jaffle_ods.{sys.argv[1]}"]
node["compiled_code"] += "\n;"
node["checksum"]["checksum"] = "edited"
json.dump(manifest, open(path, "w"))
PY
}
clear
