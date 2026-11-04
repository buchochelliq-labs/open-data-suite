#!/usr/bin/env bash
# rab's test for each wheel (rab.yml), run in its build job with DIST
# (the wheel's directory) and VERSION set: the wheel ships the licence and the notices,
# and `pip install` puts an `ods` on PATH that reports the version (#212).
set -euo pipefail
wheel=("$DIST"/*.whl)
py="$(command -v python3 || command -v python)"
"$py" -m zipfile -l "${wheel[0]}" > "$RUNNER_TEMP/wheel-contents.txt"
grep -q 'licenses/LICENSE' "$RUNNER_TEMP/wheel-contents.txt"
grep -q 'licenses/THIRD-PARTY-LICENSES.html' "$RUNNER_TEMP/wheel-contents.txt"
venv="$RUNNER_TEMP/wheel-venv"
"$py" -m venv "$venv"
if [ -d "$venv/Scripts" ]; then bin="$venv/Scripts"; else bin="$venv/bin"; fi
"$bin/python" -m pip install --quiet --no-index --find-links "$DIST" "opendatasuite==$VERSION"
export PATH="$bin:$PATH"
command -v ods
ods version
ods version --output json |
  "$bin/python" -c 'import json, sys; got = json.load(sys.stdin)["ods_version"]; sys.exit(0 if got == sys.argv[1] else f"ods_version {got}, expected {sys.argv[1]}")' "$VERSION"
