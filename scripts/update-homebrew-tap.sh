#!/usr/bin/env bash
# Puts a release's Homebrew formula into a checkout of the tap and opens a pull request
# for it there (the `homebrew-tap` job in .github/workflows/release.yml).
#
#   update-homebrew-tap.sh --tap DIR --formula FILE --version X.Y.Z [--repo OWNER/NAME] [--no-pr]
#   update-homebrew-tap.sh --self-test
#
# DIR is a git checkout of the tap whose `origin` the job can push to. The formula goes to
# Formula/ods.rb on a branch `ods-X.Y.Z`; the pull request (with `gh`, GH_TOKEN set) goes
# to the tap's checked-out branch. If the tap already has this formula, nothing is pushed.
# Running it again for the same version updates that branch and reuses its open pull
# request, so a re-run release job doesn't fail or open a second one.
set -euo pipefail

notice() { echo "::notice::$*"; }

update() {
  local tap="" formula="" version="" repo="" pr=1
  while [ $# -gt 0 ]; do
    case "$1" in
      --tap) tap=$2; shift 2 ;;
      --formula) formula=$2; shift 2 ;;
      --version) version=$2; shift 2 ;;
      --repo) repo=$2; shift 2 ;;
      --no-pr) pr=0; shift ;;
      *) echo "unknown argument: $1" >&2; return 2 ;;
    esac
  done
  if [ -z "$tap" ] || [ -z "$formula" ] || [ -z "$version" ]; then
    echo "usage: $0 --tap DIR --formula FILE --version X.Y.Z [--repo OWNER/NAME] [--no-pr]" >&2
    return 2
  fi
  if [ "$pr" = 1 ] && [ -z "$repo" ]; then
    echo "--repo is needed to open the pull request (or pass --no-pr)" >&2
    return 2
  fi
  [ -s "$formula" ] || { echo "no formula at $formula" >&2; return 1; }

  local base branch="ods-$version"
  base=$(git -C "$tap" rev-parse --abbrev-ref HEAD)
  mkdir -p "$tap/Formula"
  cp "$formula" "$tap/Formula/ods.rb"
  if [ -z "$(git -C "$tap" status --porcelain -- Formula/ods.rb)" ]; then
    notice "The tap's $base already has the formula for ods $version; nothing to do."
    return 0
  fi

  git -C "$tap" checkout --quiet -B "$branch"
  git -C "$tap" add Formula/ods.rb
  git -C "$tap" \
    -c user.name="github-actions[bot]" \
    -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
    commit --quiet -m "ods $version"
  # The branch is this job's own, named for the version: a re-run replaces it.
  git -C "$tap" push --quiet --force origin "HEAD:refs/heads/$branch"

  if [ "$pr" = 0 ]; then
    notice "Pushed $branch to the tap (no pull request requested)."
    return 0
  fi
  local open
  open=$(gh pr list --repo "$repo" --head "$branch" --state open --json url --jq '.[0].url // ""')
  if [ -n "$open" ]; then
    notice "Updated the tap's open pull request for ods $version: $open"
    return 0
  fi
  gh pr create --repo "$repo" --base "$base" --head "$branch" \
    --title "ods $version" \
    --body "The Homebrew formula for [ods $version](https://github.com/buchochelliq-labs/open-data-suite/releases/tag/v$version), rendered and attested by its release run. Opened by the release workflow."
}

self_test() {
  local dir
  dir=$(mktemp -d)
  trap 'rm -rf "$dir"' RETURN
  # Only this test's settings: none of the machine's git configuration.
  export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
  git init --quiet --bare --initial-branch=main "$dir/origin.git"
  git clone --quiet "$dir/origin.git" "$dir/tap" 2>/dev/null
  git -C "$dir/tap" -c user.name=t -c user.email=t@t commit --quiet --allow-empty -m init
  git -C "$dir/tap" push --quiet origin HEAD:main
  echo 'class Ods < Formula; version "1.0.0"; end' > "$dir/ods.rb"

  # A new formula is committed on ods-<version> and pushed; main is untouched.
  update --tap "$dir/tap" --formula "$dir/ods.rb" --version 1.0.0 --no-pr > /dev/null
  [ "$(git -C "$dir/origin.git" show ods-1.0.0:Formula/ods.rb)" = "$(cat "$dir/ods.rb")" ] \
    || { echo "self-test: the formula wasn't pushed to ods-1.0.0" >&2; return 1; }
  ! git -C "$dir/origin.git" cat-file -e main:Formula/ods.rb 2>/dev/null \
    || { echo "self-test: main was changed" >&2; return 1; }

  # Running again for the same version replaces the branch rather than failing.
  echo 'class Ods < Formula; version "1.0.0"; revision 1; end' > "$dir/ods.rb"
  git -C "$dir/tap" checkout --quiet main
  update --tap "$dir/tap" --formula "$dir/ods.rb" --version 1.0.0 --no-pr > /dev/null
  [ "$(git -C "$dir/origin.git" rev-list --count ods-1.0.0)" = 2 ] \
    || { echo "self-test: the re-run didn't replace the branch" >&2; return 1; }

  # A tap that already has the formula gets nothing pushed.
  git -C "$dir/tap" checkout --quiet main
  mkdir -p "$dir/tap/Formula"
  cp "$dir/ods.rb" "$dir/tap/Formula/ods.rb"
  git -C "$dir/tap" add Formula/ods.rb
  git -C "$dir/tap" -c user.name=t -c user.email=t@t commit --quiet -m "by hand"
  local out
  out=$(update --tap "$dir/tap" --formula "$dir/ods.rb" --version 1.0.0 --no-pr)
  case "$out" in *"nothing to do"*) ;; *) echo "self-test: expected nothing to do, got: $out" >&2; return 1 ;; esac

  # A missing formula or argument fails.
  ! update --tap "$dir/tap" --formula "$dir/missing.rb" --version 1.0.0 --no-pr 2>/dev/null \
    || { echo "self-test: a missing formula passed" >&2; return 1; }
  ! update --tap "$dir/tap" --formula "$dir/ods.rb" --version 1.0.0 2>/dev/null \
    || { echo "self-test: a pull request without --repo passed" >&2; return 1; }
  echo "self-test passed"
}

if [ "${1:-}" = --self-test ]; then
  self_test
else
  update "$@"
fi
