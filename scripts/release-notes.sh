#!/usr/bin/env bash
# Prints a release's notes: its CHANGELOG.md section, then how to install it.
# rab runs it as the GitHub release's `notes` (rab.yml), with VERSION
# set; `scripts/release-notes.sh 0.2.0` prints the same locally.
set -euo pipefail
cd "$(dirname "$0")/.."
version="${1:-${VERSION:?usage: release-notes.sh VERSION}}"
python3 scripts/changelog-section.py "$version"
cat <<NOTES

## Install

\`\`\`sh
pip install opendatasuite==$version        # or: pipx install / uv tool install opendatasuite
cargo binstall ods-cli@$version             # a prebuilt binary; or: cargo install ods-cli@$version --locked
\`\`\`

Homebrew, Chocolatey, direct downloads and verification (\`SHA256SUMS\`, build provenance):
https://buchochelliq-labs.github.io/open-data-suite/install/
NOTES
