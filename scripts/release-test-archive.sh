#!/usr/bin/env bash
# release-kit's test for each `ods` archive (release-kit.yml), run in its build job with
# FILES (the archive), VERSION and TARGET set: the archive holds the binary, the licence,
# the README, the changelog and the notices, and the binary reports the version.
set -euo pipefail
name="ods-v$VERSION-$TARGET"
work="$(mktemp -d)"
py="$(command -v python3 || command -v python)"
case "$FILES" in
  *.zip) "$py" -m zipfile -e "$FILES" "$work"; exe=.exe ;;
  *) tar -xzf "$FILES" -C "$work"; exe= ;;
esac
for file in "ods$exe" LICENSE README.md CHANGELOG.md THIRD-PARTY-LICENSES.html; do
  test -s "$work/$name/$file" || { echo "::error::$name lacks $file"; exit 1; }
done
"$work/$name/ods$exe" version
"$work/$name/ods$exe" version --output json |
  "$py" -c 'import json, sys; got = json.load(sys.stdin)["ods_version"]; sys.exit(0 if got == sys.argv[1] else f"ods_version {got}, expected {sys.argv[1]}")' "$VERSION"
