//! `Executor`: runs exactly the nodes a plan says to build (#23, #24, ADR-0014).
//!
//! # Semantics
//! - [`prepare`](Executor::prepare) brings the project's metadata up to date with its
//!   code (e.g. compiles it) so a plan describes the code that would run, and, when
//!   asked, measures how new each source's data is. It builds nothing.
//! - [`execute`](Executor::execute) builds the requested nodes and no others. Whatever
//!   the provider runs alongside them (e.g. data tests) is reported as a
//!   [check](ExecutionReport::checks_failed), never as a node. A failed check is also
//!   listed on each requested node it checks ([`NodeExecution::checks_failed`]); if the
//!   executor can't tell which nodes a failed check covers, it lists it on all of them.
//! - An empty request is refused with [`ProviderError::Other`] and runs nothing: some
//!   engines read "no selection" as "everything".
//! - A node that fails is reported as [`ExecutionStatus::Failed`] in an `Ok` report;
//!   `Err` means the execution couldn't be started or its outcome can't be known. The
//!   report lists every requested node exactly once, in request order. A requested
//!   node the engine didn't report on, or didn't recognise, is
//!   [`ExecutionStatus::Skipped`] or [`ExecutionStatus::Failed`], never a success.
//! - If the engine built nodes that weren't requested anyway, the report lists them as
//!   [`unrequested`](ExecutionReport::unrequested); they are never recorded as built.
//! - Every execution has a [`run_id`](ExecutionReport::run_id) no other execution by
//!   the same provider has had.
//! - In [`ExecutionMode::Test`] nothing is built: only the requested nodes' checks run.
//!   A node is a success when all its checks passed (or it has none), and failed when
//!   any failed; the failed checks are listed on it as in the other modes.
//! - [`ExecutionRequest::full_refresh`] rebuilds incremental state from scratch, and
//!   [`ExecutionRequest::engine_args`] passes options through to the engine as they
//!   are. An executor refuses engine arguments that would change which nodes run, or
//!   where their results are written.

use async_trait::async_trait;
use ods_core::SchemaVersion;
use ods_core::state::Timestamp;
use serde::Serialize;

use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `executor` contract.
pub const EXECUTOR: Contract = Contract {
    name: "executor",
    version: SchemaVersion::new(0, 2),
};

/// What [`Executor::prepare`] should do besides refreshing metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PrepareRequest {
    /// Measure how new each source's data is.
    pub measure_sources: bool,
}

impl PrepareRequest {
    /// Refresh metadata only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Also measure sources.
    #[must_use]
    pub fn measuring_sources(mut self) -> Self {
        self.measure_sources = true;
        self
    }
}

/// What [`Executor::prepare`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PrepareReport {
    /// Whether sources were measured.
    pub sources_measured: bool,
    /// Things the user should know that didn't stop preparation, e.g. sources that
    /// couldn't be measured.
    pub warnings: Vec<String>,
}

impl PrepareReport {
    /// A report, for providers to return.
    pub fn new(sources_measured: bool, warnings: Vec<String>) -> Self {
        Self {
            sources_measured,
            warnings,
        }
    }
}

/// Whether nodes are built, checked, or both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionMode {
    /// Build the nodes and run their checks (e.g. data tests).
    #[default]
    Build,
    /// Build the nodes only.
    Run,
    /// Run the nodes' checks only; build nothing.
    Test,
}

/// A node to build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RequestedNode {
    /// Its id.
    pub id: String,
    /// The name the engine selects it by.
    pub name: String,
}

impl RequestedNode {
    /// A node to build.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

/// What to build.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExecutionRequest {
    /// The nodes, in the order the report lists them.
    pub nodes: Vec<RequestedNode>,
    /// With or without checks.
    pub mode: ExecutionMode,
    /// Rebuild incremental state from scratch (e.g. `--full-refresh`).
    pub full_refresh: bool,
    /// Options for the engine, passed through as they are.
    pub engine_args: Vec<String>,
}

impl ExecutionRequest {
    /// A request.
    pub fn new(nodes: Vec<RequestedNode>, mode: ExecutionMode) -> Self {
        Self {
            nodes,
            mode,
            full_refresh: false,
            engine_args: Vec::new(),
        }
    }

    /// Rebuilds incremental state from scratch.
    #[must_use]
    pub fn with_full_refresh(mut self, full_refresh: bool) -> Self {
        self.full_refresh = full_refresh;
        self
    }

    /// Passes options through to the engine.
    #[must_use]
    pub fn with_engine_args(mut self, args: Vec<String>) -> Self {
        self.engine_args = args;
        self
    }
}

/// How a node's execution ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionStatus {
    /// Built.
    Success,
    /// Tried and failed.
    Failed,
    /// Not tried, e.g. because a parent failed.
    Skipped,
}

/// One requested node's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeExecution {
    /// The node's id.
    pub node: String,
    /// How it ended.
    pub status: ExecutionStatus,
    /// When it finished, if known.
    pub completed_at: Option<Timestamp>,
    /// The engine's own word or message, for people.
    pub message: Option<String>,
    /// Checks on this node that failed (in [`ExecutionMode::Build`]). The node was
    /// built, but its result isn't validated: consumers must not treat it as a
    /// success. A check on several nodes is listed on each.
    pub checks_failed: Vec<String>,
    /// Checks on this node the engine skipped (e.g. it stopped at a first failure).
    /// In [`ExecutionMode::Build`] the node was built but isn't fully checked; in
    /// [`ExecutionMode::Test`] it isn't tested, and its status isn't `success`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks_skipped: Vec<String>,
}

impl NodeExecution {
    /// An outcome, for providers to return.
    pub fn new(
        node: impl Into<String>,
        status: ExecutionStatus,
        completed_at: Option<Timestamp>,
        message: Option<String>,
    ) -> Self {
        Self {
            node: node.into(),
            status,
            completed_at,
            message,
            checks_failed: Vec::new(),
            checks_skipped: Vec::new(),
        }
    }

    /// Lists checks on this node that failed.
    #[must_use]
    pub fn with_checks_failed(mut self, checks: Vec<String>) -> Self {
        self.checks_failed = checks;
        self
    }

    /// Lists checks on this node that the engine skipped.
    #[must_use]
    pub fn with_checks_skipped(mut self, checks: Vec<String>) -> Self {
        self.checks_skipped = checks;
        self
    }

    /// Whether the node was built (or, in a test run, tested) and every check on it
    /// ran and passed.
    #[must_use]
    pub fn fully_checked(&self) -> bool {
        self.status == ExecutionStatus::Success
            && self.checks_failed.is_empty()
            && self.checks_skipped.is_empty()
    }
}

/// What an execution did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ExecutionReport {
    /// Identifies this execution; unique per provider.
    pub run_id: String,
    /// When it started, if known.
    pub started_at: Option<Timestamp>,
    /// When it finished.
    pub finished_at: Timestamp,
    /// Every requested node, once, in request order.
    pub nodes: Vec<NodeExecution>,
    /// Checks that failed (in [`ExecutionMode::Build`]), by id.
    pub checks_failed: Vec<String>,
    /// Nodes the engine built although they weren't requested, e.g. because a name
    /// selected more than one node. Executors must avoid this; when the engine does it
    /// anyway, they say so here instead of hiding it.
    pub unrequested: Vec<String>,
    /// Whether everything, nodes and checks, succeeded.
    pub succeeded: bool,
    /// The command it ran, for people to reproduce it.
    pub command: Option<String>,
}

impl ExecutionReport {
    /// A report, for providers to return. `succeeded` is derived: every node succeeded
    /// and no check failed. Use [`Self::failed`] if the engine reported failure anyway.
    pub fn new(
        run_id: impl Into<String>,
        started_at: Option<Timestamp>,
        finished_at: Timestamp,
        nodes: Vec<NodeExecution>,
        checks_failed: Vec<String>,
    ) -> Self {
        let succeeded =
            checks_failed.is_empty() && nodes.iter().all(|n| n.status == ExecutionStatus::Success);
        Self {
            run_id: run_id.into(),
            started_at,
            finished_at,
            nodes,
            checks_failed,
            unrequested: Vec::new(),
            succeeded,
            command: None,
        }
    }

    /// Marks the execution failed, e.g. because the engine exited with an error.
    #[must_use]
    pub fn failed(mut self) -> Self {
        self.succeeded = false;
        self
    }

    /// Lists nodes built without being requested.
    #[must_use]
    pub fn with_unrequested(mut self, nodes: Vec<String>) -> Self {
        self.unrequested = nodes;
        self
    }

    /// Records the command it ran.
    #[must_use]
    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }
}

/// Builds nodes. See the [module documentation](self) for the semantics every
/// implementation must follow.
#[async_trait]
pub trait Executor: Provider {
    /// Refreshes the project's metadata and, if asked, measures its sources.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the metadata couldn't be refreshed.
    async fn prepare(&self, request: &PrepareRequest) -> Result<PrepareReport, ProviderError>;

    /// Builds exactly the requested nodes.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the request is empty, or the execution couldn't be
    /// started or its outcome can't be read. Failed nodes are not errors.
    async fn execute(&self, request: &ExecutionRequest) -> Result<ExecutionReport, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(status: ExecutionStatus) -> NodeExecution {
        NodeExecution::new("model.a", status, None, None)
    }

    #[test]
    fn success_needs_every_node_and_check() {
        let at = Timestamp::from_unix(0);
        assert!(
            ExecutionReport::new("r", None, at, vec![node(ExecutionStatus::Success)], vec![])
                .succeeded
        );
        assert!(
            !ExecutionReport::new("r", None, at, vec![node(ExecutionStatus::Skipped)], vec![])
                .succeeded
        );
        assert!(
            !ExecutionReport::new(
                "r",
                None,
                at,
                vec![node(ExecutionStatus::Success)],
                vec!["test.a".into()]
            )
            .succeeded
        );
        assert!(
            !ExecutionReport::new("r", None, at, vec![node(ExecutionStatus::Success)], vec![])
                .failed()
                .succeeded
        );
    }
}
