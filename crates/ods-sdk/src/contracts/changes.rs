//! `ChangeProvider`: reads the current data version of sources, the inputs nothing
//! builds, so the planner can tell whether they have new data (#16, ADR-0022).
//!
//! # Semantics
//! - [`versions`](ChangeProvider::versions) is read-only: it builds, writes and changes
//!   nothing.
//! - The report lists every requested source exactly once, in request order, as a
//!   [`SourceVersion::Version`] or [`SourceVersion::Unknown`] with the reason.
//! - A source the provider couldn't read, or didn't recognise, is `Unknown`, never a
//!   version: only a version the provider read is one (AGENTS.md rule 3).
//! - Equal versions mean the same data: a provider never reports the version it read
//!   before for data that changed since. It may report a new version for unchanged data
//!   (e.g. after maintenance), which only costs a rebuild.
//! - `Err` means nothing was read. Callers treat every requested source as unknown.
//! - Providers answer in one batch whatever the number of sources.
//! - The provider's [`capabilities`](crate::ProviderInfo::capabilities) say which kind
//!   of version it reads, e.g. `relation_versions` for a table's own version, so a
//!   planner can prefer one kind over another.

use async_trait::async_trait;
use ods_core::SchemaVersion;
use ods_core::state::DataVersion;
use serde::Serialize;

use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `change_provider` contract.
pub const CHANGE_PROVIDER: Contract = Contract {
    name: "change_provider",
    version: SchemaVersion::new(0, 1),
};

/// A source whose data is asked about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct RequestedSource {
    /// Its id, e.g. `source.shop.raw.orders`.
    pub id: String,
    /// Its display name, e.g. `raw.orders`.
    pub name: String,
}

impl RequestedSource {
    /// A source to ask about.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

/// What a provider read about one source's data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceVersion {
    /// Its current data version.
    Version(DataVersion),
    /// The provider couldn't tell, and why.
    Unknown(String),
}

/// What [`ChangeProvider::versions`] read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct VersionReport {
    /// Every requested source, in request order.
    pub sources: Vec<(String, SourceVersion)>,
}

impl VersionReport {
    /// A report, for providers to return.
    pub fn new(sources: Vec<(String, SourceVersion)>) -> Self {
        Self { sources }
    }
}

/// Reads sources' current data versions.
#[async_trait]
pub trait ChangeProvider: Provider {
    /// Reports each requested source's current data version.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if nothing could be read.
    async fn versions(&self, sources: &[RequestedSource]) -> Result<VersionReport, ProviderError>;
}
