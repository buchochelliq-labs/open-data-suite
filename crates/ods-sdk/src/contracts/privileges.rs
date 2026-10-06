//! `RelationPrivileges`: what the login a provider connects as may do on nodes'
//! relations and the containers they are in (ADR-0030 §4c).
//!
//! A probe check runs only under a login that can do no more than read what it probes:
//! a login that can't write can't be made to write, whatever a SQL check missed. The
//! engine asks this contract before every probe run. Privilege names differ by
//! warehouse, so the provider, not the engine, decides what counts as reading.
//!
//! # Semantics
//! - [`privileges`](RelationPrivileges::privileges) only reads (e.g. the catalog's
//!   grants); it changes nothing.
//! - The report lists every requested target exactly once, in request order.
//! - [`Access::ReadOnly`] only when the provider showed, for the target's relation and
//!   every container a reader needs (e.g. its schema and catalog), that the login holds
//!   nothing beyond reading them, owns none of them, and isn't an administrator where
//!   the warehouse says so. Anything it couldn't show is [`Access::Unknown`], never
//!   `ReadOnly` (AGENTS rule 3).
//! - [`Access::Elevated`] names what the login holds beyond reading, as the warehouse
//!   names it (e.g. `MODIFY on schema shop.raw`, `owner of catalog shop`), never empty.
//! - A target the provider didn't recognise is [`Access::Unknown`].
//! - `Err` means nothing was read: callers treat every target as unknown.
//! - The report names the login when the provider knows it, as the warehouse names it
//!   (a user or principal name), never a credential (AGENTS rule 9).

use async_trait::async_trait;
use ods_core::SchemaVersion;
use serde::Serialize;

use crate::contracts::probe::ProbeTarget;
use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `relation_privileges` contract.
pub const RELATION_PRIVILEGES: Contract = Contract {
    name: "relation_privileges",
    version: SchemaVersion::new(0, 1),
};

/// What the login may do on one target's relation and its containers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Access {
    /// It can only read.
    ReadOnly,
    /// It can do more than read: each privilege, ownership or role beyond reading, as
    /// the warehouse names it. Never empty.
    Elevated(Vec<String>),
    /// It couldn't be told, and why.
    Unknown(String),
}

/// What [`RelationPrivileges::privileges`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PrivilegeReport {
    /// The login, as the warehouse names it, when known.
    pub login: Option<String>,
    /// Every requested target, by id, in request order.
    pub targets: Vec<(String, Access)>,
}

impl PrivilegeReport {
    /// A report, for implementations to return.
    pub fn new(login: Option<String>, targets: Vec<(String, Access)>) -> Self {
        Self { login, targets }
    }
}

/// Reports what the provider's login may do on nodes' relations.
#[async_trait]
pub trait RelationPrivileges: Provider {
    /// What the login may do on the relation of each of `targets`, and its containers.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if nothing could be read.
    async fn privileges(&self, targets: &[ProbeTarget]) -> Result<PrivilegeReport, ProviderError>;
}
