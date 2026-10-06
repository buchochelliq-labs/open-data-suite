//! In-memory [`HealthCheck`] (ADR-0030): fails the nodes it is told to, passes the rest,
//! and can be made to fail as a whole, or to break the contract on purpose so hosts can
//! test how they handle a check that misbehaves.

use std::collections::BTreeSet;

use async_trait::async_trait;
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::health_check::{
    CheckFinding, CheckInfo, CheckScope, HealthCheck, Severity, Status,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;

/// How the fake misbehaves, for host tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Misbehaviour {
    /// Answers as the contract says.
    None,
    /// Returns an error: nothing decided.
    Fails,
    /// Leaves the first node unanswered.
    SkipsANode,
    /// Answers the first node twice.
    AnswersTwice,
    /// Also answers a node that isn't in the scope.
    AnswersAStranger,
    /// Never answers: hosts must give up after their timeout.
    Hangs,
}

/// A check that fails the named nodes and passes the rest.
#[derive(Debug, Clone)]
pub struct FakeHealthCheck {
    info: CheckInfo,
    failing: BTreeSet<String>,
    unknown: BTreeSet<String>,
    misbehaviour: Misbehaviour,
}

impl FakeHealthCheck {
    /// A check called `id`, at `severity` unless configured, that passes every node.
    pub fn new(id: &str, severity: Severity) -> Self {
        Self {
            info: CheckInfo::new(id, "A fake check, for tests.", severity),
            failing: BTreeSet::new(),
            unknown: BTreeSet::new(),
            misbehaviour: Misbehaviour::None,
        }
    }

    /// Fails `node`.
    #[must_use]
    pub fn failing(mut self, node: impl Into<String>) -> Self {
        self.failing.insert(node.into());
        self
    }

    /// Can't decide about `node`.
    #[must_use]
    pub fn unknown(mut self, node: impl Into<String>) -> Self {
        self.unknown.insert(node.into());
        self
    }

    /// Misbehaves as `how`.
    #[must_use]
    pub fn misbehaving(mut self, how: Misbehaviour) -> Self {
        self.misbehaviour = how;
        self
    }
}

impl Provider for FakeHealthCheck {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "fake",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::HealthCheck]),
        )
    }
}

#[async_trait]
impl HealthCheck for FakeHealthCheck {
    fn describe(&self) -> CheckInfo {
        self.info.clone()
    }

    async fn check(&self, scope: &CheckScope) -> Result<Vec<CheckFinding>, ProviderError> {
        if self.misbehaviour == Misbehaviour::Hangs {
            std::future::pending::<()>().await;
        }
        if self.misbehaviour == Misbehaviour::Fails {
            return Err(ProviderError::Unavailable(
                "the fake check was told to fail".to_owned(),
            ));
        }
        let mut findings: Vec<CheckFinding> = scope
            .nodes
            .iter()
            .map(|node| {
                if self.failing.contains(&node.id) {
                    CheckFinding::new(&node.id, Status::Fail, "the fake check fails it")
                } else if self.unknown.contains(&node.id) {
                    CheckFinding::new(&node.id, Status::Unknown, "the fake check can't tell")
                } else {
                    CheckFinding::new(&node.id, Status::Pass, "the fake check passes it")
                }
            })
            .collect();
        match self.misbehaviour {
            Misbehaviour::SkipsANode if !findings.is_empty() => {
                findings.remove(0);
            }
            Misbehaviour::AnswersTwice if !findings.is_empty() => {
                let first = findings[0].clone();
                findings.push(CheckFinding::new(first.node, Status::Pass, "again"));
            }
            Misbehaviour::AnswersAStranger => {
                findings.push(CheckFinding::new("model.stranger", Status::Fail, "who?"));
            }
            _ => {}
        }
        Ok(findings)
    }
}
