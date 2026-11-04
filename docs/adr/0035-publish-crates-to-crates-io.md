# ADR-0035: Publish the crates to crates.io at the product version

- **Status:** Proposed
- **Date:** 2026-11-04
- **Issues:** none yet (requested by the maintainer; unblocks
  `buchochelliq-labs/open-data-suite-ontology` on crates.io)
- **Deciders:** @n1ckyb

## Context
ADR-0019 says every crate has `publish = false` and nothing is on crates.io: only the
`ods` binary reaches users. Two things have changed:
- **A downstream Rust consumer.** `open-data-suite-ontology` (`onto`) reads ODS's ERD
  output with `ods-erd`'s types (`Erd`, `Entity`, `Relationship`, `Key`, `Basis`), so
  it doesn't re-derive ODS's model. It depends on `ods-erd` from git, which crates.io
  rejects, so `onto` can't be published there either.
- **Plugins.** ADR-0031 lets third parties write plugins against `ods-sdk`'s
  contracts, and ADR-0006 gives each contract a fake and a conformance suite. Without
  published crates, a plugin author can only depend on ODS from git.

ADR-0019 rejected publishing (its Option B) for independent per-crate versions: a
compatibility matrix and release tooling for every crate. Publishing every crate at the
**one product version** avoids both.

Nothing else blocks publishing:
- every crate name is free on crates.io;
- no crate embeds files from outside its directory;
- the one git dependency, `rs-rich` and `rs-rich-ext` (for `ods-cli`'s rendering), is
  on crates.io with the features ODS uses (`rs-rich` 0.0.14, `rs-rich-ext` 0.0.21 with
  `sql` and `markdown`).

## Options considered
### Option A — Publish every crate at the product version (chosen)
- Pros:
  - One rule, and ADR-0019's single version kept: `ods-*` X.Y.Z are the crates of
    `ods` X.Y.Z, published by the `vX.Y.Z` tag. No compatibility matrix.
  - `cargo install ods-cli` installs `ods`, alongside pip, Homebrew and Chocolatey.
  - Consumers (`onto`) and plugin authors (`ods-sdk`, `ods-provider-fake`) depend on
    released code.
- Cons:
  - A crate's version moves with the product even when it didn't change.
  - Every internal path dependency carries the version, so a release bumps it in one
    more place. Cargo refuses a mismatch, so it can't be forgotten silently.
  - Names and published versions are permanent: a crate can be yanked, never removed.

### Option B — Publish only `ods-erd`
`ods-erd` is a leaf crate (serde only).
- Pros:
  - The smallest change that unblocks `onto`.
- Cons:
  - It answers one consumer, not the plugin story. The next consumer, or the first
    plugin author, needs this decision again.

### Option C — Independent SemVer per crate
- Cons:
  - ADR-0019's Option B. Still premature before SDK 1.0.

## Decision
**Every workspace crate is published to crates.io at the product version by each `vX.Y.Z`
release.**
- `workspace.package.publish = true`. Internal dependencies in
  `[workspace.dependencies]` carry `version` equal to `workspace.package.version`, and
  the release PR bumps both.
- No crate may depend on git. Every dependency must be on crates.io, or be another
  workspace crate.
- `.github/workflows/release.yml` publishes them after the GitHub release, from the
  `crates-io` environment, in one `cargo publish` of every crate whose version
  crates.io doesn't have yet. Cargo orders them by dependency. Trusted Publishing is
  used once each crate exists; a crate's first version uses an owner's token.
- CI packages every crate on each PR (`cargo publish --workspace --dry-run`).
- Interfaces keep ADR-0019's own versions (SDK, contracts, JSON schema, store schema).
  The crate version is the product version.

This amends ADR-0019's "nothing is on crates.io". It keeps ADR-0019's single version.

## Consequences
- Positive:
  - `onto` and plugin authors depend on released crates.
  - `cargo install ods-cli` is one more way to install.
  - Packaging problems show on PRs, not at release.
- Negative / trade-offs:
  - Every public item of every crate is now visible API. ADR-0019's rules apply to the
    product as a whole: before 1.0 a minor may break, and breaks are listed under
    **Breaking**. Crate APIs get no guarantee beyond that.
  - One-time setup by a crates.io owner (a first-version token, then Trusted
    Publishers). A release that adds a new crate needs the token again.
  - The release takes a few minutes longer.
- Follow-up issues:
  - With SDK 1.0, decide whether `ods-sdk` gets its own version (ADR-0019's superseding
    ADR).

## References
- ADR-0019 (release and versioning), ADR-0001 (layering), ADR-0006 and ADR-0031
  (contracts and plugins), ADR-0003 (rs-rich).
- crates.io Trusted Publishing: https://crates.io/docs/trusted-publishing
