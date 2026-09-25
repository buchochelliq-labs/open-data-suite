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
//! | [`StateStore`](state_store::StateStore) | #11, #25 | defined (ADR-0013) |
//! | `FingerprintProvider` | #13 | planned |
//! | `Executor` | #23 | planned |
//! | `ChangeProvider` | #16 | planned |
//! | `CloneProvider` | #29 | planned |
//! | `PolicyProvider` | #9 | planned |
//! | `EventSink` | #8 | planned |
//! | `UsageProvider` | #55 | planned |
//! | `SchemaProvider` (ERD) | #60 | planned |
//! | `SecretProvider` | #126 | planned |
//! | `LlmProvider` | #33 | planned |

pub mod lock;
pub mod observed_lineage;
pub mod sql_lineage;
pub mod state_store;
