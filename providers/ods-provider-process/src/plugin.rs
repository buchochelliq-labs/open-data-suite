//! Out-of-process plugins (ADR-0031 §5): an executable that answers the script
//! protocol's requests. ODS first asks it to `describe` itself, refuses it when a
//! contract it implements was built against a version this host can't accept, then
//! calls it once per request: `check` for each of its health checks ([`PluginCheck`]),
//! `versions` for its warehouse's source versions ([`PluginVersions`]).
//!
//! As for scripts, anything but a well-formed answer in this host's protocol major is an
//! error, which the host makes *unknown*, never a pass or a version (AGENTS rule 3).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::changes::{
    CHANGE_PROVIDER, ChangeProvider, RequestedSource, SourceVersion, VersionReport,
};
use ods_sdk::contracts::health_check::{
    CheckFinding, CheckInfo, CheckScope, HEALTH_CHECK, HealthCheck,
};
use ods_sdk::protocol::{
    CheckCall, CheckRequest, CheckResponse, DescribeRequest, DescribeResponse, VersionsRequest,
    VersionsResponse,
};
use ods_sdk::{Contract, Provider, ProviderError, ProviderInfo};

use crate::{KIND, Process};

/// The contracts an out-of-process plugin can implement in this host.
const CONTRACTS: [Contract; 2] = [HEALTH_CHECK, CHANGE_PROVIDER];

/// The kinds of source version a planner chooses among (`ods_state`'s strategies).
const VERSION_KINDS: [Capability; 2] = [Capability::RelationVersions, Capability::SourceFreshness];

/// The kinds of source version `answer` says its `change_provider` reads; names this
/// host doesn't know, or that aren't a kind of version, are ignored.
fn version_kinds(answer: &DescribeResponse) -> CapabilitySet {
    answer
        .capabilities
        .iter()
        .filter_map(|name| name.parse::<Capability>().ok())
        .filter(|c| VERSION_KINDS.contains(c))
        .collect()
}

/// A configured plugin: its command, run once per request.
#[derive(Debug, Clone)]
pub struct PluginProcess {
    name: String,
    process: Process,
}

/// What a plugin said it is and provides, once accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Described {
    /// Its answer, as given.
    pub answer: DescribeResponse,
    /// The contracts it named that this host doesn't know: listed, otherwise ignored
    /// (ADR-0031 §3c).
    pub unknown: Vec<String>,
}

impl Described {
    /// Whether it implements `contract`, at a version this host accepts.
    pub fn provides(&self, contract: &Contract) -> bool {
        self.answer.provided(contract.name).is_some()
    }
}

impl PluginProcess {
    /// The plugin `name` (its `[plugins.<name>]`), which runs `command` (its program, then
    /// its arguments) in the project's `root`, each call stopped after `timeout`.
    pub fn new(
        name: impl Into<String>,
        command: Vec<String>,
        root: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Self {
        let name = name.into();
        Self {
            process: Process {
                label: format!("plugin {name}"),
                command,
                root: root.into(),
                timeout,
            },
            name,
        }
    }

    /// Its name, as configured.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its command, for people: its items joined by spaces.
    pub fn shown(&self) -> String {
        self.process.shown()
    }

    /// The handshake: asks it what it is and provides.
    ///
    /// # Errors
    /// It didn't answer in the protocol; a contract it implements was built against a
    /// version this host doesn't accept (both are named); it lists checks without
    /// implementing `health_check`, or an id twice; or it implements `change_provider`
    /// without naming its warehouse.
    pub async fn describe(&self) -> Result<Described, ProviderError> {
        let answer: DescribeResponse = self
            .process
            .ask(&DescribeRequest::new(), |r: &DescribeResponse| r.protocol)
            .await?;
        let refuse = |why: String| {
            ProviderError::Other(format!(
                "the plugin `{}` ({}) {why}",
                self.name,
                self.shown()
            ))
        };
        let mut unknown = Vec::new();
        let mut seen = BTreeSet::new();
        for provided in &answer.provides {
            if !seen.insert(provided.contract.as_str()) {
                return Err(refuse(format!("lists `{}` twice", provided.contract)));
            }
            match CONTRACTS.iter().find(|c| c.name == provided.contract) {
                Some(contract) if !contract.accepts(provided.version) => {
                    return Err(refuse(format!(
                        "was built against {} {}.{}; this ods has {}.{}",
                        contract.name,
                        provided.version.major,
                        provided.version.minor,
                        contract.version.major,
                        contract.version.minor
                    )));
                }
                Some(_) => {}
                None => unknown.push(provided.contract.clone()),
            }
        }
        let health = answer.provided(HEALTH_CHECK.name).is_some();
        if !health && !answer.checks.is_empty() {
            return Err(refuse(format!(
                "lists checks but doesn't implement `{}`",
                HEALTH_CHECK.name
            )));
        }
        let mut ids = BTreeSet::new();
        if let Some(twice) = answer.checks.iter().find(|c| !ids.insert(c.id.as_str())) {
            return Err(refuse(format!("lists the check `{}` twice", twice.id)));
        }
        if answer.provided(CHANGE_PROVIDER.name).is_some() {
            if answer.warehouse.as_deref().is_none_or(str::is_empty) {
                return Err(refuse(format!(
                    "implements `{}` without naming its warehouse",
                    CHANGE_PROVIDER.name
                )));
            }
            // A planner uses a reading only for the kind of version it says it is: one
            // that says none would be read and never used.
            if version_kinds(&answer).is_empty() {
                return Err(refuse(format!(
                    "implements `{}` without saying which versions it reads: its capabilities name neither `relation_versions` nor `source_freshness`",
                    CHANGE_PROVIDER.name
                )));
            }
        }
        Ok(Described { answer, unknown })
    }

    /// Each of its checks, as a [`HealthCheck`] that asks it, with the project's
    /// `artifacts` by name.
    pub fn checks(
        self: &Arc<Self>,
        described: &Described,
        artifacts: &BTreeMap<String, String>,
    ) -> Vec<PluginCheck> {
        described
            .answer
            .checks
            .iter()
            .map(|info| PluginCheck {
                plugin: self.clone(),
                info: info.clone(),
                artifacts: artifacts.clone(),
            })
            .collect()
    }

    /// Its source versions, as a [`ChangeProvider`] that asks it, giving it its
    /// warehouse's `settings`; `None` unless it implements `change_provider`.
    pub fn versions(
        self: &Arc<Self>,
        described: &Described,
        settings: BTreeMap<String, serde_json::Value>,
    ) -> Option<PluginVersions> {
        described
            .provides(&CHANGE_PROVIDER)
            .then(|| PluginVersions {
                plugin: self.clone(),
                settings,
                capabilities: version_kinds(&described.answer),
            })
    }
}

/// One of a plugin's health checks: each `check` is one call to the plugin.
#[derive(Debug, Clone)]
pub struct PluginCheck {
    plugin: Arc<PluginProcess>,
    info: CheckInfo,
    artifacts: BTreeMap<String, String>,
}

impl Provider for PluginCheck {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            format!("{}/{}", self.plugin.name, self.info.id),
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::HealthCheck]),
        )
    }
}

#[async_trait]
impl HealthCheck for PluginCheck {
    fn describe(&self) -> CheckInfo {
        self.info.clone()
    }

    async fn check(&self, scope: &CheckScope) -> Result<Vec<CheckFinding>, ProviderError> {
        let request = CheckRequest::new(
            CheckCall::new(self.info.id.clone(), BTreeMap::new()),
            scope.clone(),
            self.artifacts.clone(),
        );
        let answer: CheckResponse = self
            .plugin
            .process
            .ask(&request, |r: &CheckResponse| r.protocol)
            .await?;
        Ok(answer.findings)
    }
}

/// A plugin's source versions: each `versions` is one call to the plugin.
#[derive(Debug, Clone)]
pub struct PluginVersions {
    plugin: Arc<PluginProcess>,
    settings: BTreeMap<String, serde_json::Value>,
    /// The kinds of version it said it reads.
    capabilities: CapabilitySet,
}

impl Provider for PluginVersions {
    fn info(&self) -> ProviderInfo {
        // The kind of version it said it reads, and only that: a planner chooses a
        // reading by it.
        ProviderInfo::new(
            KIND,
            self.plugin.name.clone(),
            env!("CARGO_PKG_VERSION"),
            self.capabilities.clone(),
        )
    }
}

#[async_trait]
impl ChangeProvider for PluginVersions {
    /// One call. An answer about a source it wasn't asked about, or about one twice, is
    /// an error, so every source is unknown; a source it didn't answer about is unknown.
    async fn versions(&self, sources: &[RequestedSource]) -> Result<VersionReport, ProviderError> {
        let request = VersionsRequest::new(sources.to_vec(), self.settings.clone());
        let answer: VersionsResponse = self
            .plugin
            .process
            .ask(&request, |r: &VersionsResponse| r.protocol)
            .await?;
        let asked: BTreeSet<&str> = sources.iter().map(|s| s.id.as_str()).collect();
        let mut answered: BTreeMap<String, SourceVersion> = BTreeMap::new();
        for source in answer.sources {
            if !asked.contains(source.id.as_str()) {
                return Err(ProviderError::Other(format!(
                    "the plugin `{}` answered about `{}`, which it wasn't asked about",
                    self.plugin.name, source.id
                )));
            }
            if answered.insert(source.id.clone(), source.version).is_some() {
                return Err(ProviderError::Other(format!(
                    "the plugin `{}` answered about `{}` twice",
                    self.plugin.name, source.id
                )));
            }
        }
        Ok(VersionReport::new(
            sources
                .iter()
                .map(|s| {
                    let version = answered.remove(&s.id).unwrap_or_else(|| {
                        SourceVersion::Unknown(format!(
                            "the plugin `{}` didn't answer about it",
                            self.plugin.name
                        ))
                    });
                    (s.id.clone(), version)
                })
                .collect(),
        ))
    }
}
