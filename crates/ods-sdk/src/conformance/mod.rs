//! Reusable conformance suites (ADR-0006 §5).
//!
//! A provider crate runs the suite for each contract it implements, from its own tests:
//!
//! ```ignore
//! #[tokio::test]
//! async fn conforms() {
//!     let report = ods_sdk::conformance::lock::run(&MyHarness::new()).await;
//!     assert!(report.skipped.is_empty(), "{report:?}");
//! }
//! ```
//!
//! Cases that need a capability the provider does not advertise are skipped and listed
//! in the [`Report`], so a provider is never tested for behaviour it doesn't claim.
//! A failing case panics with the case name.

pub mod lock;

/// What a suite run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Cases that ran and passed.
    pub passed: Vec<&'static str>,
    /// Cases that were skipped, with the reason.
    pub skipped: Vec<(&'static str, String)>,
}
