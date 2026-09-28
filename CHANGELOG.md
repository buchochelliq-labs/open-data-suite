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

### Breaking
- The executor contract is now version 0.3: an `ExecutionRequest` carries the sources
  whose tests to run, and an `ExecutionReport` returns their outcomes. Out-of-process
  executor plugins must be rebuilt against the current SDK, whose `EXECUTOR` contract
  is 0.3 (#288, #232).

### Added
- `ods state build` (with tests) and `ods state test` run the tests defined on sources,
  as `dbt build` does, but only when they could find something new: the source has new
  data, its data version is unknown, its tests changed, or they haven't passed yet. A
  failing source test fails the command and skips the models that read the source.
  Sources can be tested before anything is built. JSON output gains `source_tests`,
  `execution.sources` and `record.source_tests`; `based_on` is `null` when nothing was
  recorded yet (#288, #232).
- `ods state test` runs `dbt source freshness` first, as the documentation said;
  `--no-source-freshness` skips it (#288).
- CI runs the real dbt + DuckDB integration tests against dbt 1.11 and 1.12 on every
  pull request (#287, #233).
- A release and versioning policy: one version for `ods`, a version for each interface,
  this changelog, and a deprecation window (#101).
- Release builds of `ods` for Linux (x86_64 and arm64, static), macOS (Intel and Apple
  silicon) and Windows, from the first release on. Install with `pip install
  opendatasuite`, `cargo binstall` or a direct download; each archive comes
  with `SHA256SUMS`, a build provenance attestation, the licence and third-party licence
  notices. See the Install page of the documentation (#212).

### Changed
- State snapshots are now schema version 1.2: they record each source's last passing
  tests against its data version. Snapshots written at 1.0 and 1.1 still read (#288).
- `ods state test` names failed checks by test name rather than by their hash (#288).
- `Timestamp` values are parsed with `jiff`. Impossible dates such as `2026-02-30` are
  now rejected (#283).
- Dependency cycles are reported as the cycle itself, e.g. `a → b → a`, instead of every
  node that couldn't be ordered (#281).
- The lineage explorer lays out graphs with dagre, which shortens edges and reduces
  crossings (#282).

### Fixed
- From dbt 1.11, every dbt setting can also be spelled `DBT_ENGINE_<name>`, and dbt
  prefers that spelling. ODS now treats those names as the setting they spell, so
  e.g. `DBT_ENGINE_DEFER` and `DBT_ENGINE_SAMPLE` no longer get past its checks. The
  `DBT_ENGINE_` spelling of a variable ODS reads as a default for its own options
  (`DBT_TARGET`, `DBT_PROFILE`, …) is refused with a message (#287).
- The settings dbt 1.11 and 1.12 added are classified (#287).
- `ods state` could fail on Windows to set a damaged state database aside, because a
  database connection was still open after the store closed (#279).
- A lag tolerance longer than the last representable date is now reported as
  "never due", not as a date in the year 9999 (#283).
