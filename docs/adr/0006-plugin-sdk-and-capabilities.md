# ADR-0006: Plugin SDK, contracts and capabilities

- **Status:** Proposed
- **Date:** 2026-09-24
- **Issues:** #2 (plugin SDK), #3 (capability negotiation); related #99 (conformance), #28 (locking)
- **Deciders:** @n1ckyb

## Context
Every ODS module talks to the outside world through providers: dbt artifacts,
warehouse metadata, state stores, executors, locks, LLMs and so on. ADR-0001 fixes the
dependency direction: modules depend on `ods-sdk` contracts, and only the CLI wires
concrete providers in. AGENTS.md rule 1 forbids core code that branches on vendor
identity.

Issue #2 lists twelve contracts. Most of them take domain types that don't exist yet:
- `ArtifactProvider` needs the #4 semantic graph;
- `StateStore` needs the #11 state model;
- `SchemaProvider` needs the #60 ERD model.

Defining all twelve now would freeze guessed signatures that the owning issues would
then have to break.

## Options considered

### Contract rollout
- **Framework now, one complete reference contract, the rest with their owning issues
  (chosen).** Every mechanism is built and exercised end to end now: versioning,
  capabilities, factories, conformance and fakes. Each later contract follows a proven
  pattern and is written against real types.
- *All twelve traits now, with placeholder types.* The acceptance list would be ticked
  off, but most signatures would change when the domain types arrive, which is churn
  for every provider author.

### Sync or async contracts
- **Async traits via `async-trait` (chosen).** Providers do network and database I/O
  (ADR-0002: async at I/O boundaries). `async-trait` keeps the traits object-safe
  (`Box<dyn LockProvider>`), which the registry needs. Native `async fn` in traits is
  not yet dyn-compatible. We'll switch when it is, as a minor bump that providers
  won't notice.
- *Sync traits.* Simpler, but real providers (`sqlx`, HTTP clients) would each have to
  block on their own runtime.

## Decision

### 1. Contracts
- A contract is a trait in `ods-sdk/src/contracts/<name>.rs` plus a
  `Contract { name, version }` constant. Its documentation states the semantics
  precisely enough for a conformance suite to test.
- Every contract trait extends `Provider`, which requires `Send + Sync` and
  `fn info() -> ProviderInfo`.
- **`LockProvider` (#28) is the reference contract**:
  - `acquire`, `renew` and `release` on leases;
  - optional expiry (`LeaseExpiry`) and fencing tokens (`FencingTokens`).

  It has no dependencies on unfinished domain types, and non-trivial semantics.
- The other contracts are listed in `contracts/mod.rs` with the issue that will add each
  one: `ArtifactProvider` #12, `MetadataProvider` #15, `StateStore` #25,
  `FingerprintProvider` #13, `Executor` #23, `ChangeProvider` #16, `CloneProvider` #29,
  `PolicyProvider` #9, `EventSink` #8, `UsageProvider` #55, `SchemaProvider` #60,
  `SecretProvider` #126, `LlmProvider` #33. Each lands with a fake and a conformance
  suite in the same PR.

### 2. Provider identity, factories and configuration
- `ProviderInfo { kind, instance, version, capabilities }` is how a provider describes
  itself, in diagnostics and to planners.
- `ProviderFactory<dyn Contract>` creates a provider from `[providers.<name>]`
  configuration (ADR-0005):
  - it validates the provider-specific `settings`;
  - unknown keys and wrong types are `ProviderError::InvalidSettings`, which never
    echoes values.
- `Registry<dyn Contract>` holds factories keyed by `kind`. It rejects duplicate kinds
  and factories built against an incompatible contract version.
- The CLI (the composition root) owns the registries. Core never reads `kind`.

### 3. Capabilities (#3)
- **The vocabulary lives in `ods-core`**, per ADR-0001.
  - Well-known capabilities are enum variants: `relation_versions`, `zero_copy_clone`,
    `atomic_replace`, `change_tracking`, `query_history`, `source_freshness`,
    `schema_versioning`, `column_usage`, `constraint_metadata`, `lease_expiry` and
    `fencing_tokens`.
  - Third parties extend the vocabulary with `x-<namespace>.<name>`.
  - Capabilities serialize as their names, in a sorted `CapabilitySet`.
- **Strategy choice:** a planner lists `Strategy { id, requires, value }` in preference
  order, and `ods_core::choose` picks the first strategy whose requirements the
  provider's capabilities meet.
  - The list **must end with a fallback that requires nothing**, so there is always a
    conservative choice (rule 3).
  - The choice records which strategies were skipped and which capabilities each one
    lacked, so plans can explain themselves (rule 4).
- **Enforcement:** `scripts/check-vendor-neutral.py` runs in CI. It fails if core,
  foundation, SDK or module source names a vendor or runtime outside comments and
  tests. Providers and the CLI are exempt.

### 4. Errors
`ProviderError` is `#[non_exhaustive]`. Its variants are `UnknownKind`,
`InvalidSettings`, `Unsupported(Capability)`, `Conflict`, `Unavailable` (the only
retryable one) and `Other`. Messages must never contain secret values (rule 9).

### 5. Conformance and fakes (#99)
- Each contract has a suite in `ods-sdk/src/conformance/`, behind the `conformance`
  feature. A provider crate runs the suite from its own tests through a small harness
  trait (for locks: a fresh provider, plus an optional clock control).
- Cases that need a capability the provider doesn't advertise are **skipped and
  reported**, not failed. Providers are tested exactly for what they claim, and a test
  can assert which cases were skipped.
- `providers/ods-provider-fake` has reference in-memory implementations with a
  controllable clock and switchable capabilities. They must pass the full suites, and
  modules use them in tests instead of real services.

### 6. Versioning (SemVer policy)
- `SDK_VERSION` versions the SDK, and each `Contract` has its own version.
  - While major is 0, any minor bump may break providers.
  - From 1.0, removing or changing a method is a major bump.
  - Adding a method with a default implementation, or adding a contract, is a minor
    bump.
- A provider built against contract `M.p` is accepted by a host at `M.m` when `p <= m`.
- In-process providers are compiled against the SDK, so the check matters most for
  out-of-process plugins (ADR-0004 §6). It is enforced at registration regardless.

## Consequences
- **Positive:**
  - Every provider mechanism (versioning, capabilities, factories, conformance, fakes)
    is exercised end to end now.
  - Rule 1 is enforced by CI, not just by review.
  - Later contracts get real signatures and a template to copy.
- **Negative / trade-offs:**
  - #2's full contract list is spread over later issues. Until then, `contracts/mod.rs`
    is the tracker.
  - `async-trait` boxes every call's future, which is negligible next to provider I/O.
  - The vendor check is name-based. It catches `kind == "databricks"`, but not
    behaviour smuggled through other means; review still matters.
- **Follow-up work:**
  - Each owning issue adds its contract with a fake and a suite.
  - #99 generalises the harness pattern and publishes a plugin-author guide.
  - #28 implements distributed locking on top of `LockProvider`.

## References
- #2, #3, #99, #28; ADR-0001 (layers), ADR-0002 (async at I/O boundaries), ADR-0005 (provider config)
- `crates/ods-core/src/{capability,strategy}.rs`, `crates/ods-sdk/src/`, `providers/ods-provider-fake/`
