# ADR-0032: Scoped producing-code references

- **Status:** Proposed
- **Date:** 2026-11-03
- **Issues:** #462
- **Deciders:** Maintainers

## Context

A project source can be produced by code in another repository. Catalog lineage may
be unavailable or denied. Consumers need reusable metadata, with conservative
provenance and revision semantics (AGENTS rules 1, 3, 4 and 9). Elephantry explicitly
requests this shared work; its hypothesis planning remains outside ODS.

## Options considered

### Consumer-specific mapping formats

Quick to implement but duplicates identity, revision and validation semantics and
prevents providers from serving multiple consumers.

### Shared SDK contract and provider adapters

Keeps provider details outside consumers and supports multiple code locations. It
requires explicit environment/project scope and future host registration work.

## Decision

Add a versioned `ProducingCodeSource` metadata contract in ods-sdk, with a generic
`DeclaredMappings` document and a dbt manifest adapter. Reuse project node IDs,
`SchemaVersion`, `Provider` information and conformance conventions. Observed lineage
continues to use the existing `ObservedLineageSource`; an edge is not a code reference.

`AssetKey` scopes the existing node ID by host-supplied project and environment. It
must not be inferred from an unqualified physical table name. Cross-project and
logical-to-physical identity joins remain explicit host responsibilities.

References contain approved repository **aliases**, relative paths and revision
metadata, never credentials or source contents. A declaration of an exact commit
is still declared evidence. Exact revisions require the same explicitly supplied
**producing run**; a downstream test invocation is not substituted for that run.
Current-only and unknown revisions remain distinct. Multiple candidates are kept
and flagged ambiguous; consumers may not choose the first one silently.

Missing, denied, unsupported, unavailable, mismatched and conflicting evidence are
typed gaps. Fallback mappings must retain earlier provider gaps. Consumer composition
records provider identity/version alongside each result and compares differing
candidate locations rather than upgrading declarations to observations.

This increment provides in-process lookup only. It does not retrieve files, execute
code, extend the process protocol, or claim that declared paths exist. Retrieval
must enforce approved repositories, canonical paths/symlinks and immutable commit
resolution before reading. Those operations remain separate parts of #462.

## Consequences

- Positive: dbt sources can declare their producing repository without UC access;
  consumers share the same metadata and conservative revision behavior.
- Trade-offs: producing-run identity must be supplied; ambiguous candidates need
  human or additional evidence. Aliases require host configuration at retrieval time.
- Follow-up: #462 tracks retrieval, registry/process transport and catalog-derived
  code references; #170 tracks live UC lineage. No ontology dependency is introduced.

## References

- [#462](https://github.com/buchochelliq-labs/open-data-suite/issues/462)
- [ADR-0031](0031-plugin-registry-and-loading.md)
