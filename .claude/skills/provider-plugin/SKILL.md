---
name: provider-plugin
description: Implement an OpenDataSuite SDK contract (ArtifactProvider, MetadataProvider, ChangeProvider, StateStore, Executor, CloneProvider, UsageProvider, ERDProvider, LLMProvider, …) for a concrete vendor such as dbt, Databricks, SQLite or PostgreSQL. Use when an issue says "implement <X> provider/store".
---

# Implement a provider

1. **Find the contract** in `crates/ods-sdk`. If it does not exist yet, stop: define the trait
   first (with a fake in `ods-provider-fake` and conformance tests), ideally as its own PR.
2. **Declare capabilities** honestly. Advertise only what the provider can prove at runtime;
   detect optional features (e.g. Delta CDF, query history, shallow clone) and degrade
   gracefully. Core chooses strategies from capabilities — never from the provider's name.
3. **Evidence.** Every observation returned (versions, timestamps, usage counts, inferred
   relationships) carries provenance and a confidence/explicit-vs-inferred marker.
4. **Conservative failure.** When metadata is missing or ambiguous, return "unknown" so the
   planner falls back to BUILD; never fabricate values (e.g. cost estimates, versions).
5. **No leakage.** Vendor types stay inside the provider crate; map to `ods-core` types at the boundary.
6. **Secrets** come from the config/secret-reference layer; redact in `Debug`, errors and events.
7. **Tests:**
   - Run the SDK conformance suite for the contract.
   - Fixture-based tests (recorded JSON responses / dbt artifacts in `fixtures/`), no live network.
   - Optional live integration tests behind a feature flag + env var, skipped by default.
8. **dbt specifics:** parse artifacts against the public schemas for each supported
   `dbt_schema_version`; the Executor passes **exact** node selectors (no `+` graph
   expansion) so reused nodes are not re-run.
9. Document capabilities and limitations in the crate README. Run `verify`.
