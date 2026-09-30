#!/usr/bin/env bash
# Records the terminal sessions in docs/tapes/*.tape into docs/assets/recordings/<tape>/
# with rs-rich-record (`rich record`, from the rs-rich-cli crate), or checks that the
# committed recordings still match what `ods` prints.
#
#   scripts/record-docs.sh [--check] [TAPE_NAME ...]
#
#   --check    record again, but only compare each screenshot's text with the committed
#              `<shot>.txt` (after the tape's `Mask`s); write nothing; exit 1 on a difference
#   TAPE_NAME  only these tapes (e.g. `state-build`); default: every tape
#
# Needs `rich` 0.0.13 or later on PATH (`cargo install rs-rich-cli --locked`, or set
# RICH=/path/to/rich), bash and python3. It builds `ods` (debug) with cargo; set ODS_BIN_DIR
# to a directory holding an `ods` binary to skip that. The tapes run against a scratch copy
# of fixtures/dbt/jaffle-ods with the fake dbt in fixtures/dbt/fake-dbt: no warehouse, no
# network. See docs/tapes/setup.sh and CONTRIBUTING.md ("Docs recordings").
#
# rs-rich-record is a docs tool, not a dependency of any ODS crate: its embedded fonts
# (Bitstream Vera, CC BY 4.0) stay out of the workspace's licence set.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"

check=()
names=()
for arg in "$@"; do
  case "$arg" in
    --check) check=(--check) ;;
    -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "record-docs.sh: unknown option $arg" >&2; exit 2 ;;
    *) names+=("$arg") ;;
  esac
done

rich=${RICH:-rich}
if ! command -v "$rich" >/dev/null 2>&1; then
  echo "record-docs.sh: \`rich\` not found; install it with \`cargo install rs-rich-cli --locked\`" >&2
  exit 2
fi

if [[ -z "${ODS_BIN_DIR:-}" ]]; then
  cargo build --quiet -p ods-cli --bin ods
  ODS_BIN_DIR="$(cargo metadata --format-version 1 --no-deps \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/debug"
fi

tapes=()
if ((${#names[@]})); then
  for name in "${names[@]}"; do tapes+=("docs/tapes/$name.tape"); done
else
  tapes=(docs/tapes/*.tape)
fi

# The tapes' own shell environment is pinned by the recorder (TZ=UTC, LANG=C.UTF-8,
# TERM=xterm-256color, a scratch HOME, the tape's `Set Size` as the terminal size);
# `rich record` runs them from the repository root, which the tapes read as $REPO.
exec "$rich" record "${check[@]}" --bin-dir "$ODS_BIN_DIR" --output docs/assets/recordings "${tapes[@]}"
