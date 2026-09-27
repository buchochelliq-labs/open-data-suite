# Changelog

All notable, user-visible changes to ODS. The format follows
[Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/), and ODS follows
[Semantic Versioning](https://semver.org/). [ADR-0019](docs/adr/0019-release-and-versioning.md)
defines what counts as breaking, the compatibility rules for each interface, and the
deprecation policy.

Before 1.0, a minor release may break. Every break is listed under **Breaking**, with
what to do.

## [Unreleased]

Everything so far is pre-release. The first public release, 0.1.0, will summarise the
State MVP. Entries below record changes since the changelog was introduced.

### Added
- A release and versioning policy: one version for `ods`, a version for each interface,
  this changelog, and a deprecation window (#101).
- Release builds of `ods` for Linux (x86_64 and arm64, static), macOS (Intel and Apple
  silicon) and Windows, from the first release on. Install with `pip install
  opendatasuite`, Homebrew, `cargo binstall` or a direct download; each archive comes
  with `SHA256SUMS`, a build provenance attestation, the licence and third-party licence
  notices. See the Install page of the documentation (#212).

### Changed
- `Timestamp` values are parsed with `jiff`. Impossible dates such as `2026-02-30` are
  now rejected (#283).
- Dependency cycles are reported as the cycle itself, e.g. `a → b → a`, instead of every
  node that couldn't be ordered (#281).
- The lineage explorer lays out graphs with dagre, which shortens edges and reduces
  crossings (#282).

### Fixed
- `ods state` could fail on Windows to set a damaged state database aside, because a
  database connection was still open after the store closed (#279).
- A lag tolerance longer than the last representable date is now reported as
  "never due", not as a date in the year 9999 (#283).
