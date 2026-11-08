#!/usr/bin/env bash
# Writes THIRD-PARTY-LICENSES.html (the licence notices of every dependency, from
# about.toml and about.hbs) at the repository root, where the release archives and
# pyproject.toml's license-files pick it up. rab runs it as each build's
# `prepare` (rab.yml), so every platform ships the notices.
set -euo pipefail
cd "$(dirname "$0")/.."
# Pinned, with the `cli` feature: from 0.9 the `cargo-about` binary is built only with it,
# and without it `cargo install` succeeds while `cargo about` then doesn't exist (the
# v0.1.3 release run failed on this). Packaging's `scripts` job runs this install.
ABOUT_VERSION=0.9.3
if ! command -v cargo-about >/dev/null 2>&1; then
  cargo install --locked "cargo-about@$ABOUT_VERSION" --features cli
fi
cargo about generate --fail --locked about.hbs -o THIRD-PARTY-LICENSES.html
test -s THIRD-PARTY-LICENSES.html
