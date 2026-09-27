# ADR-0019: Release and versioning strategy

- **Status:** Proposed
- **Date:** 2026-09-27
- **Issues:** #101, #212
- **Deciders:** @n1ckyb

## Context
ODS is one binary, `ods`, built from a workspace of independently useful modules (State,
ERD, lineage, web, MCP). #101 asks how they are versioned and released:
- a SemVer policy;
- a compatibility matrix for core, the plugin SDK and the modules;
- changelog and release-note conventions;
- a deprecation policy.

#212, distribution for v0.1.0, needs the changelog format to exist first, and AGENTS.md
defers CHANGELOG entries "until #101 defines the format".

Several things are already versioned, each by its own ADR, and a policy has to leave
them intact:
- **The plugin SDK and each contract** (ADR-0006 §6): `SDK_VERSION`, and
  `Contract { name, version }`.
- **The JSON output envelope** (ADR-0003): `schema_version`, reported by
  `ods version`.
- **Persisted formats** (AGENTS.md coding conventions): `SchemaVersion`, with
  `can_read` meaning "same major, minor not newer".
- **The SQLite state store schema** (ADR-0018): it migrates forward on open. A newer
  store is refused, with a copy kept first.
- **Configuration** (ADR-0005): `ods.toml`.

All crates have `publish = false`. Nothing is on crates.io, and only the binary reaches
users.

## Options considered
### Option A — One product version, plus a version per interface (chosen)
Every crate shares `workspace.package.version`, and that is the version of `ods`.
Each interface that other software depends on carries its own version, bumped only when
that interface changes.
- Pros:
  - One number for users, the changelog and the release tag.
  - No cross-crate version matrix to maintain while nothing is published.
  - Interfaces don't churn just because the product released: a plugin written against
    SDK 0.1 isn't invalidated by an unrelated `ods` 0.4.
- Cons:
  - Two kinds of numbers to explain (product and interface).
  - A module can't ship alone. Nobody needs that before crates are published.

### Option B — Independent SemVer per crate
Each module crate has and publishes its own version.
- Pros:
  - Modules could be consumed and released separately.
- Cons:
  - A compatibility matrix across roughly 15 crates.
  - Release tooling for every crate, and changelog fragmentation.
  - All for crates that aren't published. Premature until SDK 1.0 invites third-party
    plugins.

### Option C — Calendar versioning (e.g. 2026.12)
- Pros:
  - Signals recency.
- Cons:
  - Says nothing about compatibility, which is what users of a state store and a JSON
    API need.
  - dbt, cargo and pip users expect SemVer.

## Decision
**ODS releases one product version, the `ods` binary's, following SemVer. Every public
interface carries its own version, with the compatibility rules below. Every user-visible
change gets an entry in `CHANGELOG.md` (Keep a Changelog). Deprecated behaviour warns
for at least one minor release before it is removed.**

### 1. Product version and SemVer policy
- The version is `workspace.package.version`. A release is a `vX.Y.Z` tag on `main`,
  and the release workflow (#212) builds from the tag.
- A release PR does two things, and nothing else:
  - bumps the version;
  - moves `## [Unreleased]` in the changelog under the new version and date.
- **What counts as breaking:** any change a user or script would notice:
  - a CLI command, flag or argument removed, renamed, or given a different meaning;
  - a change to exit codes (ADR-0004);
  - a JSON output change that is incompatible (an output-schema major bump);
  - a configuration key removed or given a different meaning;
  - a state store that an older supported `ods` can no longer read, beyond ADR-0018's
    forward-only migrations;
  - a planner decision that becomes *less* conservative. More conservative is allowed
    in a minor (AGENTS.md rule 3).
- **Before 1.0:**
  - A minor may break. Every break is listed under **Breaking** in the changelog, with
    what to do.
  - A patch never breaks and never migrates the state store.
- **From 1.0:** breaking needs a major. Minors add; patches fix.

### 2. Compatibility matrix
| Surface | Versioned by | Compatible when | Bumped |
|---|---|---|---|
| `ods` binary (product) | `workspace.package.version`, tag `vX.Y.Z` | SemVer (§1) | Each release |
| Plugin SDK | `SDK_VERSION` | ADR-0006 §6: before 1.0 exactly `0.p`; from 1.0, same major and provider minor ≤ host minor | When any contract changes |
| Each SDK contract | `Contract.version` | as the SDK | When that contract changes |
| JSON output envelope | `schema_version` in every `--output json` result | `SchemaVersion::can_read`: same major, the reader's minor ≥ the writer's | Minor: fields added. Major: fields removed, renamed or retyped |
| Persisted documents (snapshots, exports) | their `schema_version` | `can_read` | As JSON output |
| SQLite state store | migration version (ADR-0018) | A newer `ods` migrates older stores forward, keeping a copy. An older `ods` refuses a newer store | Each schema migration. It is recorded in the changelog and never happens in a patch |
| `ods.toml` configuration | the product version | Unknown keys are errors (ADR-0005). Removing or changing a key's meaning is breaking | Per §1 |

`ods version --output json` reports the product, SDK and output-schema versions, so a
script or plugin host can check them. `ods state doctor` says when a store's schema is
older or newer than the binary's.

Modules are not versioned separately. A module's interface (commands, JSON, persisted
formats) is covered by the rows above.

### 3. Changelog and release notes
- `CHANGELOG.md` at the repository root, in [Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/)
  format:
  - The newest version comes first, under `## [Unreleased]`.
  - Sections, in this order: **Breaking**, Added, Changed, Deprecated, Removed, Fixed,
    Security. Empty sections are left out.
  - Each entry is a sentence written for users, not a commit subject. It ends with the
    PR and issue, e.g. `(#283, #101)`.
  - Breaking entries say what to do, e.g. "Rename `x` to `y` in `ods.toml`."
- **When to add an entry:** every PR that changes something a user can notice adds one
  under `[Unreleased]`. That includes commands and flags, JSON output, persisted formats,
  planner decisions and messages people rely on, and install. Internal refactors and
  test-only changes don't. AGENTS.md makes this a PR requirement.
- **Release notes:** the version's changelog section, copied verbatim into the GitHub
  release by the #212 workflow, followed by the checksums and install commands.

### 4. Deprecation policy
- Deprecation is announced before removal:
  - Deprecated behaviour keeps working and prints a warning on stderr, where ADR-0003
    sends warnings, so JSON on stdout stays parseable.
  - It is listed under **Deprecated** in the changelog, naming the replacement.
- **Before 1.0:** removal is allowed in the next minor at the earliest. The warning must
  have shipped in at least one release.
- **From 1.0:** removal is allowed in the next major only.
- **Persisted formats are never deprecated away.**
  - Every `ods` from 0.1.0 on keeps the migrations for every earlier store schema, and
    readers for every earlier document major it supports.
  - Dropping support for reading an old format is itself a breaking change, which needs
    a major (or, before 1.0, a minor with a Breaking entry).
- An SDK contract version is deprecated by adding its successor. The host rejects the
  old one only after the deprecation window above.

## Consequences
- Positive:
  - One number for users and one tag per release. The interfaces other software depends
    on have explicit, testable rules, mostly already enforced in code (`can_read`, SDK
    registration checks, store migrations).
  - #212 can build release notes directly from the changelog.
  - Reviewers get a concrete list of what counts as breaking.
- Negative / trade-offs:
  - Two kinds of version numbers to explain; §2's table is the reference.
  - Modules can't be released on their own. That is revisited when crates are published
    or SDK 1.0 arrives, in a superseding ADR.
  - Every user-visible PR needs a changelog entry, which is a small tax on each PR.
- Follow-up issues:
  - #212: the release workflow uses the `vX.Y.Z` tag, attaches the changelog section,
    and bumps `workspace.package.version` for 0.1.0.
  - A CI check that a PR touching `crates/ods-cli/src/commands/`, persisted types or
    `docs/cli.md` also touches `CHANGELOG.md`, with a `no-changelog` label to opt out.
  - Report the store schema version in `ods version`, alongside SDK and output schema.

## References
- #101, #212; AGENTS.md "Workflow" and rule 3.
- ADR-0003 (output envelope), ADR-0004 (exit codes), ADR-0005 (configuration),
  ADR-0006 §6 (SDK versioning), ADR-0018 (store migrations).
- [Semantic Versioning 2.0.0](https://semver.org/), [Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/).
