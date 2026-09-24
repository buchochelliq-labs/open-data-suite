#!/usr/bin/env bash
# Apply .github/labels.json and .github/milestones.json to GitHub.
#
# Idempotent: creates missing labels/milestones, updates descriptions/due dates,
# and sets each listed issue's milestone. Requires an authenticated `gh` and `jq`.
#
# Usage:
#   scripts/sync-milestones.sh [--repo owner/name] [--dry-run] [--close-duplicates]
set -euo pipefail

REPO="buchochelliq-labs/open-data-suite"
DRY_RUN=0
CLOSE_DUPES=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) REPO="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --close-duplicates) CLOSE_DUPES=1; shift ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MILESTONES="$ROOT/.github/milestones.json"
LABELS="$ROOT/.github/labels.json"

command -v gh >/dev/null || { echo "gh CLI is required" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }

run() {
  if [[ $DRY_RUN -eq 1 ]]; then echo "[dry-run] $*"; else "$@"; fi
}

echo "==> labels"
jq -c '.[]' "$LABELS" | while read -r label; do
  name=$(jq -r .name <<<"$label")
  run gh label create "$name" --repo "$REPO" --force \
    --color "$(jq -r .color <<<"$label")" \
    --description "$(jq -r .description <<<"$label")" >/dev/null
  echo "    $name"
done

echo "==> milestones"
existing=$(gh api "repos/$REPO/milestones?state=all&per_page=100" --paginate)
jq -c '.milestones[]' "$MILESTONES" | while read -r m; do
  title=$(jq -r .title <<<"$m")
  number=$(jq -r --arg t "$title" '.[] | select(.title == $t) | .number' <<<"$existing")
  args=(-f "title=$title" -f "description=$(jq -r .description <<<"$m")")
  due=$(jq -r '.due_on // empty' <<<"$m")
  [[ -n $due ]] && args+=(-f "due_on=$due")

  if [[ -z $number ]]; then
    if [[ $DRY_RUN -eq 1 ]]; then
      echo "[dry-run] create milestone '$title'"; number="<new>"
    else
      number=$(gh api "repos/$REPO/milestones" -X POST "${args[@]}" --jq .number)
      echo "    created #$number $title"
    fi
  else
    run gh api "repos/$REPO/milestones/$number" -X PATCH "${args[@]}" >/dev/null
    echo "    updated #$number $title"
  fi

  for issue in $(jq -r '.issues[]' <<<"$m"); do
    if [[ $number == "<new>" ]]; then
      echo "[dry-run]   #$issue -> '$title'"
    else
      run gh api "repos/$REPO/issues/$issue" -X PATCH -F "milestone=$number" >/dev/null
      echo "      #$issue"
    fi
  done
done

if [[ $CLOSE_DUPES -eq 1 ]]; then
  echo "==> duplicates"
  jq -r '.duplicates | to_entries[] | select(.key != "$comment") | "\(.key) \(.value)"' "$MILESTONES" |
    while read -r dup canonical; do
      run gh issue comment "$dup" --repo "$REPO" --body "Duplicate of #$canonical (see docs/ROADMAP.md §5)."
      run gh api "repos/$REPO/issues/$dup" -X PATCH -f state=closed -f state_reason=duplicate >/dev/null
      echo "    closed #$dup as duplicate of #$canonical"
    done
fi

echo "done."
