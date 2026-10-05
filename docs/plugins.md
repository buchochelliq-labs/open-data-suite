# Writing a provider

A provider implements one or more of `ods-sdk`'s contracts: reading a warehouse's
relations, analyzing SQL, storing state, running a build. Everything vendor-specific
lives in a provider; ODS's core and modules only see the contract
([ADR-0006](adr/0006-plugin-sdk-and-capabilities.md)).

To prove a provider keeps a contract, `ods-sdk` ships a **conformance suite** for each
one. The providers in this repository run them, and a provider written elsewhere runs
the same suites from its own tests (#99).

## Contracts and their suites

| Contract (trait) | Suite (`ods_sdk::conformance::…`) | Harness | `run` | Reference fake |
|---|---|---|---|---|
| `ChangeProvider` | `changes` | `ChangeHarness` | async | `FakeChangeProvider` |
| `ErrorCatalogue` | `error_catalogue` | `ErrorCatalogueHarness` | sync | `FakeErrorCatalogue` |
| `Executor` | `executor` | `ExecutorHarness` | async | `FakeExecutor` |
| `LockProvider` | `lock` | `LockHarness` | async | `FakeLockProvider` |
| `ObservedLineageSource` | `observed_lineage` | `ObservedLineageHarness` | sync | `FakeObservedLineageSource` |
| `RelationProbe` | `probe` | `ProbeHarness` | async | `FakeRelationProbe` |
| `RelationLinker` | `relation_link` | `RelationLinkHarness` | sync | `FakeRelationLinker` |
| `RelationInspector` | `relations` | `RelationHarness` | async | `FakeExecutor` |
| `SqlLineageAnalyzer` | `sql_lineage` | `SqlLineageHarness` | sync | `FakeSqlLineageAnalyzer` |
| `StateStore` | `state_store` | `StateStoreHarness` | async | `FakeStateStore` |

The fakes are in `ods-provider-fake`; read one next to its suite to see the smallest
implementation that passes.

## Running a suite

A suite needs a **harness**: a small type in your tests that hands the suite fresh
instances of your provider, plus whatever the contract needs to be exercised without a
live platform (a fixture, a temp directory, a way to move time forward). The suite
runs every case and returns a `Report`:

```rust
// tests/conformance.rs in your provider crate
use std::sync::Arc;

use ods_sdk::conformance::sql_lineage::{SqlLineageHarness, run};
use ods_sdk::contracts::sql_lineage::SqlLineageAnalyzer;

struct Harness;

impl SqlLineageHarness for Harness {
    fn analyzer(&self) -> Arc<dyn SqlLineageAnalyzer> {
        Arc::new(my_provider::MyAnalyzer::new())
    }
}

#[test]
fn conforms() {
    let report = run(&Harness);
    assert!(report.skipped.is_empty(), "{report:?}");
}
```

For an async suite, make the test `#[tokio::test]` and `.await` the `run`.

- **A failing case panics** with the contract and what it expected, so the test output
  says which rule was broken.
- **Capabilities decide what runs.** A case that needs a capability your provider
  doesn't advertise is skipped and listed in `report.skipped` with the reason, so a
  provider is never tested for behaviour it doesn't claim. Assert on the skips you
  expect, so that a capability you meant to advertise and didn't shows up as a failure.
- **No network.** Suites run on fixtures and fakes. A harness for a warehouse-backed
  provider runs on recorded responses or a local stand-in, never a live account.

## Depending on the SDK

The suites are behind `ods-sdk`'s `conformance` feature, so only tests pull them in.
ODS's crates are not published to crates.io yet, so depend on the repository, pinned to
a release tag:

```toml
[dependencies]
ods-sdk = { git = "https://github.com/buchochelliq-labs/open-data-suite", tag = "v0.0.1" }

[dev-dependencies]
ods-sdk = { git = "https://github.com/buchochelliq-labs/open-data-suite", tag = "v0.0.1", features = ["conformance"] }
```

The `sql_lineage` and `observed_lineage` suites are newer than v0.0.1: until the next
release, pin a commit on `main` with `rev = "…"` instead of `tag`.

Before 1.0 a contract accepts only its exact minor version, and `SDK_VERSION` changes
whenever a contract does; the [changelog](https://github.com/buchochelliq-labs/open-data-suite/blob/main/CHANGELOG.md)
lists each change under **Breaking** with what to do. Move to a new tag and rerun the
suites.

## In CI

A job that runs the suites on every push:

```yaml
name: conformance
on: [push, pull_request]
jobs:
  conformance:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --test conformance
```
