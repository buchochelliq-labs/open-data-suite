//! `HealthCheck`: checks a set of nodes' health and reports one finding per node
//! (ADR-0030 §5).
//!
//! The built-in checks, and later declarative, probe, script and plugin checks, all
//! answer through this contract, so a host treats every check the same way.
//!
//! # Semantics
//! - [`describe`](HealthCheck::describe) names the check (its id, unique among the
//!   checks a host runs) and says what it checks and how severe its failure is unless
//!   configured.
//! - [`check`](HealthCheck::check) answers **every** node in the scope **exactly once**,
//!   and no node outside it. The order doesn't matter.
//! - A node the check can't decide about is [`Status::Unknown`], with the reason, never
//!   `Pass` (AGENTS rule 3). A node the check doesn't apply to is [`Status::Skipped`].
//! - `Err` means nothing was decided: hosts treat every node in the scope as unknown.
//! - The same scope gives the same findings: a check reads what it is given and the
//!   provider's own connections, and never changes anything (it is read-only).
//! - A check never sees, and never reports, a resolved secret (AGENTS rule 9).
//! - Providers advertise [`Capability::HealthCheck`](ods_core::Capability::HealthCheck).

use std::collections::BTreeSet;

use async_trait::async_trait;
use ods_core::SchemaVersion;
use ods_core::state::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `health_check` contract.
pub const HEALTH_CHECK: Contract = Contract {
    name: "health_check",
    version: SchemaVersion::new(0, 1),
};

/// What a host knows about a node: neutral facts from the project and the state store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeFacts {
    /// Its id, e.g. `model.shop.orders`.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its resource type, e.g. `model`, `seed`, `snapshot`.
    pub resource_type: String,
    /// Its file, relative to the project, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Its tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// How many tests (data and unit) read it.
    #[serde(default)]
    pub tests: usize,
    /// Its last successful build, if ODS recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildFacts>,
}

impl NodeFacts {
    /// A node with nothing known about it but its id, name and type.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        resource_type: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            resource_type: resource_type.into(),
            ..Self::default()
        }
    }
}

/// A node's last successful build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct BuildFacts {
    /// The run that built it.
    pub run_id: String,
    /// When the build finished.
    pub built_at: Timestamp,
    /// The run whose tests all passed on this build, and when, if recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested: Option<(String, Timestamp)>,
    /// Whether the tests that passed are the node's tests now.
    #[serde(default)]
    pub checks_current: bool,
}

impl BuildFacts {
    /// A build by `run_id` at `built_at`, its tests not recorded.
    pub fn new(run_id: impl Into<String>, built_at: Timestamp) -> Self {
        Self {
            run_id: run_id.into(),
            built_at,
            tested: None,
            checks_current: false,
        }
    }

    /// Records that its tests passed in `run_id` at `at`, and whether they are the
    /// node's tests now.
    #[must_use]
    pub fn tested(mut self, run_id: impl Into<String>, at: Timestamp, current: bool) -> Self {
        self.tested = Some((run_id.into(), at));
        self.checks_current = current;
        self
    }
}

/// What the last run's record says failed. Only from a record of the host's scope:
/// another target's run says nothing about these nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LastFailures {
    /// When the run started.
    pub started_at: Timestamp,
    /// Its command, e.g. `ods state build`.
    pub command: String,
    /// Nodes that failed, or whose tests failed.
    pub failed: BTreeSet<String>,
    /// Nodes skipped because something upstream failed.
    pub skipped: BTreeSet<String>,
}

impl LastFailures {
    /// A run started at `started_at` by `command`.
    pub fn new(
        started_at: Timestamp,
        command: impl Into<String>,
        failed: impl IntoIterator<Item = String>,
        skipped: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            started_at,
            command: command.into(),
            failed: failed.into_iter().collect(),
            skipped: skipped.into_iter().collect(),
        }
    }
}

/// How severe a check's failure is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Severity {
    /// The node is failing.
    Error,
    /// The node is a warning.
    Warn,
    /// Shown, and changes nothing.
    Info,
}

/// What a check concluded about a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Status {
    /// It passed.
    Pass,
    /// It failed.
    Fail,
    /// The check couldn't decide.
    Unknown,
    /// The check doesn't apply to the node, or had nothing to look at.
    Skipped,
}

/// A check, as it describes itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CheckInfo {
    /// Its id: lowercase letters, digits, `_`, `-` and `.`, unique among a host's
    /// checks, e.g. `acme.pii_tagged`.
    pub id: String,
    /// What it checks, for people.
    pub about: String,
    /// How severe its failure is unless configured.
    pub default_severity: Severity,
}

impl CheckInfo {
    /// A check called `id`.
    pub fn new(id: impl Into<String>, about: impl Into<String>, severity: Severity) -> Self {
        Self {
            id: id.into(),
            about: about.into(),
            default_severity: severity,
        }
    }

    /// Whether [`id`](Self::id) is well formed: non-empty, of lowercase letters,
    /// digits, `_`, `-` and `.`, starting with a letter.
    pub fn valid_id(id: &str) -> bool {
        id.starts_with(|c: char| c.is_ascii_lowercase())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-.".contains(c))
    }
}

/// What a check is asked about: the nodes in its scope, and what the last run's record
/// says, when it says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CheckScope {
    /// The nodes to answer, each once.
    pub nodes: Vec<NodeFacts>,
    /// The last run's failures, for this scope; `None` when no record says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastFailures>,
}

impl CheckScope {
    /// The scope `nodes`, with what the last run's record says.
    pub fn new(nodes: Vec<NodeFacts>, last_run: Option<LastFailures>) -> Self {
        Self { nodes, last_run }
    }
}

/// A check's conclusion about one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CheckFinding {
    /// The node's id.
    pub node: String,
    /// What the check concluded.
    pub status: Status,
    /// Why, for people.
    pub reason: String,
}

impl CheckFinding {
    /// A finding about `node`.
    pub fn new(node: impl Into<String>, status: Status, reason: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            status,
            reason: reason.into(),
        }
    }
}

/// Checks nodes' health. See the module documentation for the semantics.
#[async_trait]
pub trait HealthCheck: Provider {
    /// The check, as it describes itself.
    fn describe(&self) -> CheckInfo;

    /// One finding for every node in `scope`, and none for any other node.
    ///
    /// # Errors
    /// Returns [`ProviderError`] when nothing could be decided; hosts treat every node
    /// in the scope as unknown.
    async fn check(&self, scope: &CheckScope) -> Result<Vec<CheckFinding>, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_ids_are_plain() {
        for ok in ["tests_required", "acme.pii_tagged", "a-1"] {
            assert!(CheckInfo::valid_id(ok), "{ok}");
        }
        for bad in ["", "Tests", "1st", "a b", "a/b", ".a", "é"] {
            assert!(!CheckInfo::valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_scope_round_trips_as_json() {
        let at = Timestamp::parse("2026-09-29T09:00:00Z").unwrap();
        let mut node = NodeFacts::new("model.a", "a", "model");
        node.build = Some(BuildFacts::new("run-1", at).tested("run-1", at, true));
        let scope = CheckScope::new(
            vec![node],
            Some(LastFailures::new(
                at,
                "ods state build",
                ["model.a".to_owned()],
                [],
            )),
        );
        let json = serde_json::to_string(&scope).unwrap();
        assert_eq!(serde_json::from_str::<CheckScope>(&json).unwrap(), scope);
    }
}
