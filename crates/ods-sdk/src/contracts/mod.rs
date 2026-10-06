//! Provider contracts (ADR-0006 §1).
//!
//! Each contract is a trait plus a [`Contract`](crate::Contract) constant with its name
//! and version. Contracts land with the issue that first needs them, so their
//! signatures are written against real domain types rather than guesses:
//!
//! | Contract | Issue | Status |
//! |---|---|---|
//! | [`LockProvider`](lock::LockProvider) | #28 | defined |
//! | [`SqlLineageAnalyzer`](sql_lineage::SqlLineageAnalyzer) | #73, #74 | defined (ADR-0008) |
//! | [`ObservedLineageSource`](observed_lineage::ObservedLineageSource) | #74 | defined (ADR-0008) |
//! | `LineageSink` | #74, #92 | planned (ADR-0008) |
//! | `ArtifactProvider` | #12 | planned (needs the #4 semantic graph) |
//! | `MetadataProvider` | #15 | planned |
//! | [`StateStore`](state_store::StateStore) | #11, #25, #188 | defined (ADR-0013, ADR-0018) |
//! | `FingerprintProvider` | #13 | planned |
//! | [`Executor`](executor::Executor) | #23, #24 | defined (ADR-0014) |
//! | [Run events](run_events) (`Executor::execute_with_events`) | #322 | defined (ADR-0024) |
//! | [`ErrorCatalogue`](error_catalogue::ErrorCatalogue) | #323 | defined (ADR-0025) |
//! | [`RelationInspector`](relations::RelationInspector) | #230 | defined (ADR-0016) |
//! | [`ChangeProvider`](changes::ChangeProvider) | #16, #17 | defined (ADR-0022) |
//! | [`RelationProbe`](probe::RelationProbe) | #17 | defined (ADR-0022) |
//! | [`RelationLinker`](relation_link::RelationLinker) | #329 | defined (ADR-0006 §7) |
//! | [`HealthCheck`](health_check::HealthCheck) | #392 | defined (ADR-0030) |
//! | `CloneProvider` | #29 | planned |
//! | `PolicyProvider` | #9 | planned |
//! | `EventSink` | #8 | planned |
//! | `UsageProvider` | #55 | planned |
//! | `SchemaProvider` (ERD) | #60 | planned |
//! | `SecretProvider` | #126 | planned |
//! | `LlmProvider` | #33 | planned |

pub mod changes;
pub mod error_catalogue;
pub mod executor;
pub mod health_check;
pub mod lock;
pub mod observed_lineage;
pub mod probe;
pub mod relation_link;
pub mod relations;
pub mod run_events;
pub mod sql_lineage;
pub mod state_store;
