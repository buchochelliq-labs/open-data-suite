//! Reference in-memory providers (ADR-0006 §5).
//!
//! These fakes implement SDK contracts with no I/O and a controllable clock. They are
//! the executable specification of each contract: they pass its conformance suite, and
//! modules use them in tests instead of real warehouses (AGENTS.md: no network in
//! tests). Capabilities can be switched off to test planners' fallbacks.

mod clock;
mod lock;
mod observed_lineage;
mod sql_lineage;

pub use clock::FakeClock;
pub use lock::{FakeLockFactory, FakeLockProvider};
pub use observed_lineage::FakeObservedLineageSource;
pub use sql_lineage::FakeSqlLineageAnalyzer;

/// The `kind` fakes are registered under in configuration.
pub const KIND: &str = "fake";
