//! What a plugin offers, found by asking it (ADR-0031 §3c): each factory is called
//! over a probe that refuses every statement, since building a provider runs nothing,
//! and with empty settings, so the answer never depends on where `ods` runs.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_sdk::contracts::changes::CHANGE_PROVIDER;
use ods_sdk::contracts::health_check::{HEALTH_CHECK, HealthCheck};
use ods_sdk::contracts::observed_lineage::OBSERVED_LINEAGE_SOURCE;
use ods_sdk::contracts::privileges::RELATION_PRIVILEGES;
use ods_sdk::contracts::probe::{ProbeReport, ProbeRequest, ProbeTarget, RelationProbe};
use ods_sdk::contracts::relation_link::{NoRelationLink, RELATION_LINKER};
use ods_sdk::{Contract, Provider, ProviderError, ProviderInfo};
use serde::Serialize;

use super::{Origin, WarehousePlugin, WarehouseSettings};

/// One plugin, and what it offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Detected {
    /// The check's id, or the warehouse kind.
    pub name: String,
    /// Where it comes from: a crate and its version.
    pub from: String,
    /// Whether the released `ods` has it.
    pub builtin: bool,
    /// What it offers.
    pub features: Vec<Feature>,
}

/// One thing a plugin offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Feature {
    /// The feature, e.g. `source_versions`.
    pub name: &'static str,
    /// The SDK contract it implements, if it implements one (a dialect doesn't).
    #[serde(skip)]
    pub contract: Option<Contract>,
    /// What a person needs to know: what it reads, its dialect.
    pub detail: Option<String>,
    /// Why it is offered but can't be used as configured (e.g. no host for links).
    pub unavailable: Option<String>,
}

impl Feature {
    fn new(name: &'static str, contract: Option<Contract>) -> Self {
        Self {
            name,
            contract,
            detail: None,
            unavailable: None,
        }
    }
}

/// A connection that refuses every statement.
struct Refuse;

impl Provider for Refuse {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            "detect",
            "detect",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::default(),
        )
    }
}

#[async_trait]
impl RelationProbe for Refuse {
    async fn probe(
        &self,
        _request: &ProbeRequest,
        _targets: &[ProbeTarget],
    ) -> Result<ProbeReport, ProviderError> {
        Err(ProviderError::Unavailable(
            "detection runs no statements".to_owned(),
        ))
    }
}

fn refuse() -> Arc<dyn RelationProbe> {
    Arc::new(Refuse)
}

/// Whether `plugin` reads source versions.
pub(super) fn changes(plugin: &dyn WarehousePlugin) -> bool {
    plugin.changes(refuse()).is_some()
}

/// Whether `plugin` reads observed lineage exports.
pub(super) fn observed_lineage(plugin: &dyn WarehousePlugin) -> bool {
    plugin.observed_lineage(Path::new("")).is_some()
}

/// What a warehouse plugin offers.
pub(super) fn warehouse(plugin: &dyn WarehousePlugin, builtin: bool) -> Detected {
    let origin = plugin.origin();
    let mut features = Vec::new();
    if changes(plugin) {
        let mut versions = Feature::new("source_versions", Some(CHANGE_PROVIDER));
        versions.detail = plugin.versions_read();
        features.push(versions);
    }
    if plugin.privileges(refuse()).is_some() {
        features.push(Feature::new("login_check", Some(RELATION_PRIVILEGES)));
    }
    match plugin.links(&WarehouseSettings::empty()) {
        Ok(_) => features.push(Feature::new("links", Some(RELATION_LINKER))),
        Err(NoRelationLink::Unsupported { .. } | NoRelationLink::NotOffered { .. }) => {}
        Err(why) => {
            let mut links = Feature::new("links", Some(RELATION_LINKER));
            links.unavailable = Some(why.to_string());
            features.push(links);
        }
    }
    if observed_lineage(plugin) {
        features.push(Feature::new(
            "observed_lineage",
            Some(OBSERVED_LINEAGE_SOURCE),
        ));
    }
    if let Some(dialect) = plugin.dialect() {
        let mut feature = Feature::new("dialect", None);
        feature.detail = Some(dialect.to_owned());
        features.push(feature);
    }
    Detected {
        name: plugin.warehouse().to_owned(),
        from: format!("{} {}", origin.name, origin.version),
        builtin,
        features,
    }
}

/// What a health check plugin offers: itself.
pub(super) fn check(check: &dyn HealthCheck, origin: Origin, builtin: bool) -> Detected {
    Detected {
        name: check.describe().id,
        from: format!("{} {}", origin.name, origin.version),
        builtin,
        features: vec![Feature::new("health_check", Some(HEALTH_CHECK))],
    }
}
