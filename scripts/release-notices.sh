#!/usr/bin/env bash
# Writes THIRD-PARTY-LICENSES.html (the licence notices of every dependency, from
# about.toml and about.hbs) at the repository root, where the release archives and
# pyproject.toml's license-files pick it up. release-kit runs it as each build's
# `prepare` (release-kit.yml), so every platform ships the notices.
set -euo pipefail
cd "$(dirname "$0")/.."
if ! command -v cargo-about >/dev/null 2>&1; then
  cargo install --locked cargo-about
fi
cargo about generate --fail --locked about.hbs -o THIRD-PARTY-LICENSES.html
test -s THIRD-PARTY-LICENSES.html
