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
| `HealthCheck` | `health_check` | `HealthCheckHarness` | async | `FakeHealthCheck` |
| `ErrorCatalogue` | `error_catalogue` | `ErrorCatalogueHarness` | sync | `FakeErrorCatalogue` |
| `Executor` | `executor` | `ExecutorHarness` | async | `FakeExecutor` |
| `LockProvider` | `lock` | `LockHarness` | async | `FakeLockProvider` |
| `ObservedLineageSource` | `observed_lineage` | `ObservedLineageHarness` | sync | `FakeObservedLineageSource` |
| `RelationProbe` | `probe` | `ProbeHarness` | async | `FakeRelationProbe` |
| `RelationPrivileges` | `privileges` | `PrivilegesHarness` | async | `FakeRelationPrivileges` |
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

## A custom `ods` (in-process plugins)

A provider plugs into ODS through a **custom `ods`**: a small binary crate of your own
that runs the released CLI with your plugins added
([ADR-0031](adr/0031-plugin-registry-and-loading.md)). Two kinds of plugin go in this way:

- **Health checks.** Any `HealthCheck` (ADR-0030). `ods health check` runs it with the
  built-in, declared and probe checks, and the dashboard shows its recorded findings.
- **Warehouse plugins.** A `WarehousePlugin` serves one warehouse kind, as the project
  names it (for dbt, the manifest's `adapter_type`, e.g. `snowflake`), the way dbt picks
  its adapter. It never connects on its own. Every method is optional:
  - `changes`: a `ChangeProvider` over the connection dbt already has (a
    `RelationProbe`), which reads sources' data versions for `ods state`;
  - `privileges`: a `PrivilegedProbe` (a `RelationProbe` that is also
    `RelationPrivileges`), which reports what the probe login may do, so probe checks
    can run without `--allow-elevated-login`;
  - `links`: a `RelationLinker` for "Open in warehouse", built from the warehouse's
    `[providers.<name>]` settings (`WarehouseSettings`, with secret references left
    unresolved), or the reason there are no links;
  - `observed_lineage`: an `ObservedLineageSource` reading what the user exported
    from the warehouse, for `ods lineage compare --observed`;
  - `dialect`: the SQL dialect column lineage parses its SQL in, by the shared
    parser's name for it (a name the parser doesn't know refuses the plugin).

  What a plugin offers is **detected**, never declared: ODS calls each method, over a
  connection that refuses every statement and with empty settings, and lists what
  comes back. The released `ods` has one built-in warehouse plugin, Databricks: Delta
  table versions, Unity Catalog's login check, Catalog Explorer links, Unity Catalog's
  column lineage exports and the Databricks dialect.

```rust
// src/main.rs of your crate
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let ods = ods_cli::Ods::new()                         // the released `ods`
        .health_check(ods_cli::origin!(), Arc::new(my_checks::OwnerTagged))
        .and_then(|ods| ods.warehouse(Arc::new(my_warehouse::Plugin)));
    match ods {
        Ok(ods) => ods.run(),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
```

- **Registration is strict.** A health check needs a valid id that no other plugin
  check has. A warehouse already served, by a built-in or another plugin, is refused.
  To replace one, say so with `replacing_warehouse`. A dialect the parser doesn't know
  is refused either way.
- **Visible.** `ods version` and `ods doctor` (`capabilities.plugins`) list every
  plugin, with each contract it was detected to implement and its crate, and say
  which are built in.
- **Configured like the built-ins.** `[health.plugins.<id>]` takes `severity`
  (including `off`), `select` and `exclude` ([docs/cli.md](cli.md#plugin-checks)). An
  id no plugin check has is a configuration error.
- **Versions.** Your crate and ODS must use the same `ods-sdk`, so pin ODS to a
  release tag, as above. A plugin built against another SDK doesn't compile in.

`examples/custom-ods` is a complete example, built in this repository's CI. It adds a
health check (`custom.owner_tagged`) and a warehouse plugin for `duckdb`, passes both
conformance suites, and tests the binary end to end. Copy it to start.

## Writing a source-version provider

A source-version provider is a `ChangeProvider`, offered by a warehouse plugin's
`changes`. `ods state` asks it about every source of the project and decides, from the
answers, which models can be reused ([ADR-0022](adr/0022-delta-table-versions-as-source-evidence.md)).

- **A version is a promise.** Equal values must mean the same data. Read the version
  through the `RelationProbe` you are given: one or more read-only statements per
  source's relation, run on dbt's own connection.
- **Grade it honestly.** Each version carries an `Exactness`, and only `exact` and
  `semantic` versions let ODS reuse a node:

  | Grade | Means | Example | Reuse |
  |---|---|---|---|
  | `exact` | identifies the exact data | a Delta table's id and version | yes |
  | `semantic` | changes whenever the data changes in the sense that matters | the latest load batch of an append-only table | yes |
  | `proxy` | correlated, but can move without a data change, or miss one | a last-modified time that moves on metadata changes | no |

  Grade by what the warehouse documents, not by what is usually true. When unsure,
  pick the lower grade: a lower grade costs a rebuild, a higher one can reuse stale
  data.
- **Fail to unknown** (AGENTS rule 3). A source you can't read is `Unknown`, with the
  reason (not a table, no history, no access), and its readers build. `Err` means you
  read nothing at all, and every source is unknown. Never guess a version.
- **Run the `changes` suite** over `ods-provider-fake`'s `FakeRelationProbe`. Give it
  rows for your statements, and have `commit` change them.
  `examples/custom-ods/tests/conformance.rs` does this for its `LoadBatches`.

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
