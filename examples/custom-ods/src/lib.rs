//! Plugins written outside ODS, for a custom `ods` (ADR-0031 §2).
//!
//! - [`OwnerTagged`], a health check: every node says who owns it, with an `owner:` tag.
//! - [`LoadBatchPlugin`], a warehouse plugin for `duckdb`: a source's data version is the
//!   latest `batch_id` its loader wrote ([`LoadBatches`]).
//!
//! Both are illustrations: copy the shape, not the rules.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use ods_cli::plugins::{Origin, WarehousePlugin};
use ods_core::state::{DataVersion, Exactness};
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::changes::{
    CHANGE_PROVIDER, ChangeProvider, RequestedSource, SourceVersion, VersionReport,
};
use ods_sdk::contracts::health_check::{
    CheckFinding, CheckInfo, CheckScope, HealthCheck, Severity, Status,
};
use ods_sdk::contracts::probe::{
    InvalidProbe, ProbeAnswer, ProbeFilter, ProbeRequest, ProbeStatement, ProbeTarget,
    RelationProbe,
};
use ods_sdk::{Contract, Provider, ProviderError, ProviderInfo};

/// The provider kind this crate's providers report.
pub const KIND: &str = "custom-ods";

// ---------------------------------------------------------------- a health check

/// Every node has an `owner:<who>` tag, so someone answers for it.
#[derive(Debug, Clone, Copy, Default)]
pub struct OwnerTagged;

/// [`OwnerTagged`]'s id. Namespaced, so it can't clash with ODS's own checks.
pub const OWNER_TAGGED: &str = "custom.owner_tagged";

impl Provider for OwnerTagged {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            OWNER_TAGGED,
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::HealthCheck]),
        )
    }
}

#[async_trait]
impl HealthCheck for OwnerTagged {
    fn describe(&self) -> CheckInfo {
        CheckInfo::new(
            OWNER_TAGGED,
            "the node has an `owner:<who>` tag",
            Severity::Warn,
        )
    }

    async fn check(&self, scope: &CheckScope) -> Result<Vec<CheckFinding>, ProviderError> {
        // It reads only what it is given: one finding per node, and no other.
        Ok(scope
            .nodes
            .iter()
            .map(|node| {
                let owners: Vec<&str> = node
                    .tags
                    .iter()
                    .filter_map(|t| t.strip_prefix("owner:"))
                    .filter(|who| !who.trim().is_empty())
                    .collect();
                if owners.is_empty() {
                    CheckFinding::new(&node.id, Status::Fail, "no `owner:<who>` tag")
                } else {
                    let owners = owners.join(", ");
                    CheckFinding::new(&node.id, Status::Pass, format!("owned by {owners}"))
                        .with_evidence("owner", owners)
                }
            })
            .collect())
    }
}

// ---------------------------------------------------------------- a change provider

/// Where versions come from, as evidence shows it.
pub const ORIGIN: &str = "load_batch";

/// The statement each source's relation gets: the latest batch its loader wrote.
pub const LATEST_BATCH: &str = "select max(batch_id) as batch_id from {relation}";

/// Reads each source's latest load batch through a relation probe.
///
/// **The grade is the point of the example.** A batch id moves on every load, and only
/// on loads, *if* the loader only ever appends a new batch. That shows the data changed
/// in the sense that matters, so it is `semantic` and may let ODS reuse a model. A loader
/// that also updates rows in place would make it a `proxy`, which never allows reuse: grade
/// what your warehouse documents, not what you hope (ADR-0022).
#[derive(Debug, Clone)]
pub struct LoadBatches<P> {
    probe: P,
}

impl<P: RelationProbe> LoadBatches<P> {
    /// Reads through `probe`.
    pub fn new(probe: P) -> Self {
        Self { probe }
    }
}

/// The probe request: one read-only statement against tables.
///
/// # Errors
/// Never in practice; the statement is fixed.
pub fn request() -> Result<ProbeRequest, InvalidProbe> {
    ProbeRequest::new(
        ProbeFilter::kinds(["table"])?,
        vec![ProbeStatement::new(LATEST_BATCH, ["batch_id"])?],
    )
}

/// A source's version from what the probe found: unknown, with why, unless there is a
/// batch id.
fn version(answer: ProbeAnswer) -> SourceVersion {
    match answer {
        ProbeAnswer::Rows(rows) => match rows
            .first()
            .and_then(|row| row.get("batch_id"))
            .map(|v| v.trim())
            // A null `max` (no rows yet) arrives as a missing column.
            .filter(|v| !v.is_empty())
        {
            Some(batch) => {
                SourceVersion::Version(DataVersion::new(batch, Exactness::Semantic, ORIGIN))
            }
            None => SourceVersion::Unknown("no load batch yet".to_owned()),
        },
        ProbeAnswer::Skipped(why) => SourceVersion::Unknown(format!("not a table: {why}")),
        ProbeAnswer::Unknown(why) => SourceVersion::Unknown(why),
        // `ProbeAnswer` may grow: whatever this version doesn't know is unknown.
        _ => SourceVersion::Unknown("an answer this provider doesn't know".to_owned()),
    }
}

impl<P: RelationProbe> Provider for LoadBatches<P> {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "load_batches",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::RelationVersions]),
        )
    }
}

#[async_trait]
impl<P: RelationProbe> ChangeProvider for LoadBatches<P> {
    async fn versions(&self, sources: &[RequestedSource]) -> Result<VersionReport, ProviderError> {
        let request =
            request().map_err(|e| ProviderError::Other(format!("the batch probe: {e}")))?;
        let targets: Vec<ProbeTarget> = sources.iter().map(ProbeTarget::from).collect();
        let report = self.probe.probe(&request, &targets).await?;
        let mut answers: BTreeMap<String, Option<ProbeAnswer>> = BTreeMap::new();
        for (id, answer) in report.targets {
            // Answered twice: trust neither (AGENTS rule 3).
            answers
                .entry(id)
                .and_modify(|seen| *seen = None)
                .or_insert(Some(answer));
        }
        Ok(VersionReport::new(
            sources
                .iter()
                .map(|s| {
                    let found = match answers.remove(&s.id) {
                        Some(Some(answer)) => version(answer),
                        Some(None) => SourceVersion::Unknown("the probe answered twice".to_owned()),
                        None => SourceVersion::Unknown("the probe didn't report on it".to_owned()),
                    };
                    (s.id.clone(), found)
                })
                .collect(),
        ))
    }
}

// ---------------------------------------------------------------- the warehouse plugin

/// The warehouse kind this plugin serves, as dbt names it.
pub const WAREHOUSE: &str = "duckdb";

/// `duckdb`'s providers: [`LoadBatches`] over dbt's own connection.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoadBatchPlugin;

impl WarehousePlugin for LoadBatchPlugin {
    fn origin(&self) -> Origin {
        ods_cli::origin!()
    }

    fn warehouse(&self) -> &str {
        WAREHOUSE
    }

    fn provides(&self) -> Vec<Contract> {
        vec![CHANGE_PROVIDER]
    }

    fn versions_read(&self) -> Option<String> {
        Some("latest load batch, `max(batch_id)`".to_owned())
    }

    fn changes(&self, probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn ChangeProvider>> {
        Some(Arc::new(LoadBatches::new(probe)))
    }
}
