//! The State planner: what to build, what to reuse, and why (#18, #20, ADR-0013).
//!
//! Everything here is pure and synchronous. The caller describes the project as it is
//! now ([`Project`]), and passes the last committed
//! [`StateSnapshot`](ods_core::state::StateSnapshot):
//! - [`plan`] decides, per node, BUILD or REUSE, with reasons and evidence;
//! - [`record`] turns a finished run into the next snapshot, advancing only the nodes
//!   that succeeded (AGENTS.md rule 5);
//! - [`select`] resolves dbt-style `+name+` selectors.
//!
//! Missing or uncertain evidence always means BUILD (AGENTS.md rule 3).

mod planner;
mod recorder;
mod selection;

pub use planner::{PlanError, plan};
pub use recorder::{Outcome, Recorded, RunResult, record};
pub use selection::select;

use ods_core::FreshnessPolicy;
use ods_core::state::{DataVersion, Fingerprint, Timestamp};

/// A node ODS plans: a model, seed or snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Node {
    /// Unique id, e.g. `model.shop.orders`.
    pub id: String,
    /// Display name, e.g. `orders`.
    pub name: String,
    /// What it is, e.g. `model`.
    pub kind: String,
    /// Direct parents: other nodes or sources.
    pub parents: Vec<String>,
    /// Its code fingerprint, or why one can't be made completely.
    pub fingerprint: Result<Fingerprint, String>,
    /// When new upstream data makes it due.
    pub policy: FreshnessPolicy,
    /// Its output depends only on its own code (e.g. a file of static rows), so having
    /// no parents doesn't leave its data unexplained.
    pub self_contained: bool,
}

impl Node {
    /// A node.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: impl Into<String>,
        parents: Vec<String>,
        fingerprint: Result<Fingerprint, String>,
        policy: FreshnessPolicy,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            parents,
            fingerprint,
            policy,
            self_contained: false,
        }
    }

    /// Marks the node's output as depending only on its own code.
    #[must_use]
    pub fn self_contained(mut self) -> Self {
        self.self_contained = true;
        self
    }
}

/// An upstream source of data that ODS doesn't build.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Source {
    /// Unique id, e.g. `source.shop.raw.orders`.
    pub id: String,
    /// Display name, e.g. `raw.orders`.
    pub name: String,
    /// Its current data version, if anything reports one.
    pub version: Option<DataVersion>,
    /// When `version` was observed. A version only says something about data a node
    /// hasn't seen if it was observed after the node was built.
    pub observed_at: Option<Timestamp>,
}

impl Source {
    /// A source.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        version: Option<DataVersion>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version,
            observed_at: None,
        }
    }

    /// Sets when the version was observed.
    #[must_use]
    pub fn observed_at(mut self, at: Option<Timestamp>) -> Self {
        self.observed_at = at;
        self
    }
}

/// The project as it is now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Project {
    /// Buildable nodes.
    pub nodes: Vec<Node>,
    /// Sources.
    pub sources: Vec<Source>,
}

impl Project {
    /// A project.
    pub fn new(nodes: Vec<Node>, sources: Vec<Source>) -> Self {
        Self { nodes, sources }
    }
}
