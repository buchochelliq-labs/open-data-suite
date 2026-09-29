# ADR-0019: Release and versioning strategy

- **Status:** Proposed
- **Date:** 2026-09-27 (amended 2026-09-29: first release is 0.0.1; 0.1.0 means the
  dashboard design is complete)
- **Issues:** #101, #212, #309
- **Deciders:** @n1ckyb

## Context
ODS is one binary, `ods`, built from a workspace of independently useful modules (State,
ERD, lineage, web, MCP). #101 asks how they are versioned and released:
- a SemVer policy;
- a compatibility matrix for core, the plugin SDK and the modules;
- changelog and release-note conventions;
- a deprecation policy.

#212, distribution for the first release (v0.0.1), needs the changelog format to exist
first, and AGENTS.md defers CHANGELOG entries "until #101 defines the format".

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

### 1a. Release milestones: 0.0.x until the dashboard is complete
- **The first public release is 0.0.1** (the State MVP, M1, plus the first dashboard
  screens, #310–#313).
- **Releases stay 0.0.x until every screen in the dashboard design
  (`docs/design/dashboard/`) is built.** **0.1.0 means the dashboard design is
  complete** (UX1, #309); it is not tagged before that, however much else has shipped.
  Work from later milestones that lands before then ships in a 0.0.x release.
- The milestones after it keep their order and numbers (M2 → 0.2.0, M3 → 0.3.0, …),
  each after 0.1.0.
- **In the 0.0.x series every release is treated like a pre-1.0 minor:** it may add
  features, break (listed under **Breaking**, with what to do) and migrate the state
  store. The §1 patch rule ("never breaks, never migrates") applies from 0.1.0 on.

### 2. Compatibility matrix
| Surface | Versioned by | Compatible when | Bumped |
|---|---|---|---|
| `ods` binary (product) | `workspace.package.version`, tag `vX.Y.Z` | SemVer (§1) | Each release |
| Plugin SDK | `SDK_VERSION` | ADR-0006 §6: before 1.0 exactly `0.p`; from 1.0, same major and provider minor ≤ host minor | When any contract changes |
| Each SDK contract | `Contract.version` | as the SDK | When that contract changes |
| JSON output envelope | `schema_version` in every `--output json` result | `SchemaVersion::can_read`: same major, the reader's minor ≥ the writer's. Consumers ignore fields they don't know | Minor: fields added. Major: fields removed, renamed or retyped |
| Persisted documents (snapshots, exports) | their `schema_version` | `can_read`, and a newer reader loads every earlier minor of its major | Minor: only optional or defaulted fields added (`#[serde(default)]`, or `Option`), with a test loading a document written at the previous minor. A new required field, or a removed, renamed or retyped one, is a major, read through a migration (below) |
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
- **Before 1.0:** removal is allowed in the next minor at the earliest (in the 0.0.x
  series, the next release). The warning must have shipped in at least one release.
- **From 1.0:** removal is allowed in the next major only.
- **Persisted formats are never deprecated away.** State written by any `ods` since
  0.0.1, the first release, stays readable by every later `ods`, whatever the version:
  - every earlier store schema keeps its migration;
  - every earlier document major keeps a reader that upgrades it to the current one.

  Support is never removed, so history can always be explained (AGENTS.md rule 4).
- **SDK contracts.**
  - **Before 1.0,** a provider must match the host's contract minor exactly (ADR-0006
    §6), so there is no window. A contract change is listed under **Breaking**, and
    out-of-process plugins must be rebuilt for the release. In-process providers are
    compiled with the host and need nothing.
  - **From 1.0,** the host accepts providers built against earlier minors, so a
    contract's successor ships alongside it and the deprecation window above applies.
    The old contract is removed only in a major.

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
    and bumps `workspace.package.version` for 0.0.1.
  - A CI check that a PR touching `crates/ods-cli/src/commands/`, persisted types or
    `docs/cli.md` also touches `CHANGELOG.md`, with a `no-changelog` label to opt out.
  - Report the store schema version in `ods version`, alongside SDK and output schema.

## References
- #101, #212; AGENTS.md "Workflow" and rule 3.
- ADR-0003 (output envelope), ADR-0004 (exit codes), ADR-0005 (configuration),
  ADR-0006 §6 (SDK versioning), ADR-0018 (store migrations).
- [Semantic Versioning 2.0.0](https://semver.org/), [Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/).
