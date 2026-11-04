# ADR-0032: Publish `ods-erd` to crates.io

- **Status:** Proposed
- **Date:** 2026-11-04
- **Issues:** none yet (requested by the maintainer for
  `buchochelliq-labs/open-data-suite-ontology`)
- **Deciders:** @n1ckyb

## Context
ADR-0019 says every crate has `publish = false` and nothing is on crates.io: only the
`ods` binary reaches users. That still holds for the product, but there is now a
downstream Rust consumer. `open-data-suite-ontology` (`onto`) reads ODS's ERD output
with `ods-erd`'s typed model (`Erd`, `Entity`, `Relationship`, `Key`, `Basis`), so it
doesn't re-derive ODS's types. It depends on `ods-erd` from git at a tag.

crates.io rejects packages with git dependencies. So `onto` can't be published there
while `ods-erd` isn't.

`ods-erd` is the one crate this needs, and it is a leaf in ADR-0001's layering: it
depends only on `serde` and `serde_json`. Publishing it publishes no other ODS crate and
no ODS internals.

## Options considered
### Option A — Publish `ods-erd` alone, at the product version (chosen)
- Pros:
  - The smallest change that unblocks the consumer. One crate, with no ODS dependencies.
  - Keeps ADR-0019's single product version: `ods-erd` X.Y.Z is the ERD model of
    `ods` X.Y.Z, published by the same `vX.Y.Z` tag.
  - Its serialized form is already versioned (the ERD JSON's `schema_version`), so
    consumers have a compatibility signal beyond the crate version.
- Cons:
  - A crate version that moves with the product, even when `ods-erd` didn't change.
  - The crates.io name and Trusted Publisher need one-time setup by an owner.

### Option B — Publish every crate
- Pros:
  - Anyone can depend on any part of ODS.
- Cons:
  - It is ADR-0019's Option B: the compatibility matrix and release tooling for about
    15 crates, before SDK 1.0 invites third-party plugins. Still premature.

### Option C — The consumer drops the typed dependency
`onto` would parse ODS's ERD JSON with its own types.
- Pros:
  - No change to ODS.
- Cons:
  - The consumer duplicates ODS's model and drifts from it silently. The point of
    depending on `ods-erd` is that the compiler catches a changed model.

## Decision
**`ods-erd` is published to crates.io at the product version by each `vX.Y.Z` release.**
- `ods-erd` alone sets `publish = true`. Every other crate keeps `publish = false`.
- Its dependencies must stay published, third-party crates. If it ever needs another ODS
  crate, that crate is published too, or this ADR is revisited.
- `.github/workflows/release.yml` publishes it after the GitHub release, in the
  `crates-io` environment, with crates.io Trusted Publishing. A dry run packages it with
  `cargo publish --dry-run`.
- A version already on crates.io is skipped, so re-running a release is safe.

This amends ADR-0019's "nothing is on crates.io" for this one crate. It does not change
ADR-0019's single product version.

## Consequences
- Positive:
  - `onto` can depend on `ods-erd` from crates.io and be published there itself.
  - Packaging problems surface in the release's dry run, before a tag.
- Negative / trade-offs:
  - A published crate can't be unpublished, only yanked. `ods-erd`'s public types become
    an interface. It follows ADR-0019's rules for the product (before 1.0 a minor may
    break; while 0.0.x every release may break), and breaking changes are listed under
    **Breaking**.
  - One-time setup by a crates.io owner: the first version needs a token, because
    Trusted Publishing is configured on an existing crate.
- Follow-up issues:
  - Decide, with SDK 1.0, whether `ods-sdk` and the contracts are published (ADR-0019's
    superseding ADR).

## References
- ADR-0019 (release and versioning), ADR-0001 (layering).
- crates.io Trusted Publishing: https://crates.io/docs/trusted-publishing
