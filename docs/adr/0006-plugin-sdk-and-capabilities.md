# ADR-0006: Plugin SDK, contracts and capabilities

- **Status:** Accepted (2026-10-04). Not built yet, now follow-ups: the CLI builds providers directly rather than through `Registry` and `ProviderFactory` from `[providers.*]` (§2). `SDK_VERSION` is 0.7 and the output envelope 1.0, not the 0.3 and 0.1 this text names.
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
  - Every grant has a token unique per key, even for the same owner, so a stale lease
    from a retried run can never renew or release its successor. `FencingTokens` adds
    that tokens strictly increase.
  - TTLs are a validated `LeaseTtl` (1 s to 24 h), so providers never overflow a
    timestamp or grant an already expired lease. Expiry is inclusive: at
    `granted + ttl` the key is free. An expired lease can't be renewed (the holder
    acquires again and gets a new token), and releasing it is a no-op.

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
  and factories built against an incompatible contract version, and rejects a created
  provider whose `info()` reports a different kind or instance than configured.
- The CLI (the composition root) owns the registries. Core never reads `kind`.

### 3. Capabilities (#3)
- **The vocabulary lives in `ods-core`**, per ADR-0001.
  - Well-known capabilities are enum variants: `relation_versions`,
    `relation_probe` ([ADR-0022](0022-delta-table-versions-as-source-evidence.md)),
    `relation_existence` ([ADR-0016](0016-relation-existence-before-reuse.md)), `zero_copy_clone`,
    `atomic_replace`, `change_tracking`, `query_history`, `source_freshness`,
    `schema_versioning`, `column_usage`, `constraint_metadata`, `lease_expiry`,
    `fencing_tokens`, `run_events` ([ADR-0024](0024-run-events-node-stats-and-run-journal.md))
    `error_explain` ([ADR-0025](0025-error-explanations.md)), `relation_link` (§7) and
    `run_ledger` ([ADR-0029](0029-build-timings-and-the-run-ledger.md)).
  - Third parties extend the vocabulary with `x-<namespace>.<name>`, a validated
    `CustomCapability` that can only be built by parsing, so it can never spell a
    well-known name.
  - Capabilities serialize as their names and are **ordered by name**, so a sorted
    `CapabilitySet` (and anything hashed from it) doesn't change when variants are added.
- **Strategy choice:** a planner lists `Strategy { id, requires, value }` in preference
  order, and `ods_core::choose` picks the first strategy whose requirements the
  provider's capabilities meet.
  - The list **must end with a fallback that requires nothing**, so there is always a
    conservative choice (rule 3).
  - The choice records which strategies were skipped and which capabilities each one
    lacked, so plans can explain themselves (rule 4). `Choice`, `Strategy` and `Skipped`
    serialize, so the reasons can be shown as JSON.
- **Enforcement:** `scripts/check-vendor-neutral.py` runs in CI. It fails if core,
  foundation, SDK or module source names a vendor or runtime in code or string
  literals. It tokenizes Rust (comments and `#[cfg(test)]` items are skipped) and splits
  identifiers on `_` and case changes, so `DatabricksClient` and `dbtManifest` are
  caught. Its own `--self-test` runs in CI too. Providers and the CLI are exempt.

### 4. Errors
`ProviderError` is `#[non_exhaustive]`. Its variants are `UnknownKind`,
`InvalidSettings`, `Unsupported(Capability)`, `Conflict`, `Unavailable` (the only
retryable one) and `Other`. Messages must never contain secret values (rule 9).

### 5. Conformance and fakes (#99)
- Each contract has a suite in `ods-sdk/src/conformance/`, behind the `conformance`
  feature. A provider crate runs the suite from its own tests through a small harness
  trait (for locks: a fresh provider per case, plus an optional clock control that
  moves time for every provider the harness created). The suite checks that every
  provider advertises the same capabilities.
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
- Compatibility follows from that:
  - before 1.0, a provider must be built against **exactly** the host's contract
    version (`0.p` is accepted only by a `0.p` host);
  - from 1.0, a provider built against `M.p` is accepted by a host at `M.m` when
    `p <= m`.
- In-process providers are compiled against the SDK, so the check matters most for
  out-of-process plugins (ADR-0004 §6). It is enforced at registration regardless.

### 7. Relation links (#329)
*Amended 2026-09-30.* The dashboard and the CLI link a node's relation to the
warehouse's own UI (for Unity Catalog, Catalog Explorer). This is a small, pure
contract, so it is recorded here rather than in an ADR of its own: it adds no persisted
format, no dependency and no crate.

- **Capability** `relation_link`: the provider can turn a relation's name into a link.
- **Contract** `relation_linker` 0.1 (`ods-sdk/src/contracts/relation_link.rs`):
  `RelationLinker::link(relation) -> Result<RelationLink { url, label }, NoRelationLink>`.
  - **Synchronous**, unlike the other contracts: it only formats a URL from
    configuration, with no I/O (ADR-0002: async only at I/O boundaries).
  - The relation is passed as the project's artifacts render it (quoted as the
    warehouse quotes identifiers), so each provider parses its own dialect.
  - **Conservative (rule 3):** a link is where the manifest says the relation is, never
    proof that it exists. When no link can be built the provider says why
    (`unsupported`, `not_offered`, `not_configured`, `invalid_setting`,
    `not_qualified`, `invalid_name`, `no_relation`), and never guesses one (e.g. a
    default catalog for a two-part name).
  - **Names are never repaired.** `split_relation` (in the SDK, given the warehouse's
    quote character) allows whitespace only around the `.` separators; whitespace
    inside an unquoted name, text straight after a closing quote, a stray or unclosed
    quote and an empty name are refused. `path_segment` refuses `.` and `..`, which
    browsers resolve as dot segments even percent-encoded, so a link can't leave its
    path.
  - **Private (rule 9):** a link is `https://` only, with no user part or fragment; a
    query string may carry configuration (e.g. a workspace id), never a credential. Each
    name is percent-encoded into the path. The host is configuration, not a secret.
  - The **label** comes from the provider ("Open in Catalog Explorer"), so hosts never
    write a warehouse's name.
- **Where it is used:** only the CLI maps the target's adapter to a linker, checks the
  capability with `choose` (falling back to no link), and fills neutral
  `RelationLinkFields` (`relation_url`, `relation_url_label`,
  `relation_url_unavailable`) into the lineage document, the lineage JSON and
  `ods-web`'s Catalog input. `ods-web` renders them and never imports a provider
  (ADR-0001, ADR-0009).
- **Providers:** `ods-provider-databricks::CatalogExplorer` builds
  `https://<host>/explore/data/<catalog>/<schema>/<table>` from the configured `host`
  (ADR-0021 §3; `DATABRICKS_HOST` first). The path is the one Databricks' own
  documentation uses for a table's page (the `databricksWorkspaceUrl` in [access-request
  notifications](https://learn.microsoft.com/azure/databricks/data-governance/unity-catalog/manage-privileges/access-request-destinations#access-request-examples)).
  Those links also carry `?o=<workspace id>`, which selects the workspace when a host
  serves several, and so does `CatalogExplorer`: from the provider's `workspace_id`
  setting, else from a host that names it (Azure's `adb-<id>.<n>.azuredatabricks.net`,
  GCP's `<id>.<n>.gcp.databricks.com`), else not at all. It is never guessed: an id
  that isn't one, that differs from the host's, or that differs between providers is
  refused with a reason. The conformance suite checks that a query doesn't change with
  the relation; that it holds no credential is the provider's to keep. A link may
  therefore carry a query string of configuration (never a credential, rule 9), but
  never a user part or fragment.
- **Alternatives considered:**
  - *A method on an existing contract* (`RelationInspector`, `RelationProbe`): they are
    async and implemented by executors (the dbt executor), which don't know the
    workspace's UI; a linker needs only configuration. Rejected.
  - *A URL template in configuration* (`relation_url = "https://…/{catalog}/…"`): no
    provider code at all, but every user would have to know the UI's URL scheme, and
    quoting and encoding would be left to a template. Kept as a possible later
    addition for warehouses without a provider.
- **Conformance:** `conformance::relation_link` checks the capability, a qualified
  relation's link (https, no user part or fragment, non-empty label,
  deterministic), that a missing part is never guessed, that names are encoded into
  exactly one segment each, and that an unconfigured provider says so. The fake
  (`FakeRelationLinker`) and `CatalogExplorer` both pass it.
- **Versions:** a new contract at 0.1; no existing contract changes. Adding a
  contract is an SDK minor bump (§6), and it ships under the bump to `SDK_VERSION` 0.3
  already in `[Unreleased]` for #323, beside `error_catalogue`.
- **The lineage document** (`GraphDocument`, `schema_version` 1) gains the three
  fields as optional additions, with no version change: its version is a single
  integer, which can't record a minor bump, and readers ignore fields they don't know.
  The JSON output envelope stays at 0.1 for the same additive change, as with earlier
  additions.

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
  - The vendor check is name-based. It catches `kind == "databricks"` and
    `DatabricksClient`, but not behaviour smuggled through other means; review still
    matters. Common words that are also product names (oracle, fabric) are not
    checked, to avoid false positives.
- **Follow-up work:**
  - Each owning issue adds its contract with a fake and a suite.
  - #99 generalises the harness pattern and publishes a plugin-author guide.
  - #28 implements distributed locking on top of `LockProvider`.

## References
- #2, #3, #99, #28, #329; ADR-0001 (layers), ADR-0002 (async at I/O boundaries), ADR-0005 (provider config)
- `crates/ods-core/src/{capability,strategy}.rs`, `crates/ods-sdk/src/`, `providers/ods-provider-fake/`
