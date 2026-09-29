//! `RelationInspector`: checks that nodes' relations are still in the warehouse before
//! they are reused (#230, ADR-0016).
//!
//! # Semantics
//! - [`inspect`](RelationInspector::inspect) is read-only: it builds, drops and changes
//!   nothing.
//! - The report lists every requested node exactly once, in request order.
//! - A node whose relation the provider couldn't resolve is
//!   [`RelationPresence::Unknown`], never [`RelationPresence::Present`]: only a relation
//!   the provider saw is present.
//! - `Err` means nothing was verified. Callers treat every requested node as unknown.
//! - Providers answer in one batch (e.g. one metadata query) whatever the number of
//!   nodes, and may look at more nodes than were requested to do so.

use async_trait::async_trait;
use ods_core::SchemaVersion;
use serde::Serialize;

use crate::contracts::executor::RequestedNode;
use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `relation_inspector` contract.
pub const RELATION_INSPECTOR: Contract = Contract {
    name: "relation_inspector",
    version: SchemaVersion::new(0, 1),
};

/// Whether a node's relation is in the warehouse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RelationPresence {
    /// It exists.
    Present {
        /// What it is, e.g. `table` or `view`, if the provider knows.
        kind: Option<String>,
    },
    /// It doesn't exist.
    Missing,
    /// The provider couldn't tell, and why.
    Unknown(String),
}

/// What [`RelationInspector::inspect`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RelationReport {
    /// Every requested node, in request order.
    pub nodes: Vec<(String, RelationPresence)>,
}

impl RelationReport {
    /// A report, for providers to return.
    pub fn new(nodes: Vec<(String, RelationPresence)>) -> Self {
        Self { nodes }
    }
}

/// Checks that nodes' relations exist.
#[async_trait]
pub trait RelationInspector: Provider {
    /// Reports whether each requested node's relation exists.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the check couldn't run; nothing was verified.
    async fn inspect(&self, nodes: &[RequestedNode]) -> Result<RelationReport, ProviderError>;
}
