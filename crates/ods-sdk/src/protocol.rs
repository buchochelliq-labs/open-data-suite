//! The script protocol (ADR-0030 §4; ADR-0031 §5): what a host writes to a checking
//! process's stdin, and what it reads back from its stdout. One JSON document each way,
//! versioned by [`PROTOCOL`]'s major. Script checks and out-of-process plugins speak
//! it: a plugin is first asked to `describe` itself ([`DescribeRequest`]), then each
//! call is one request (`check`, `versions`) and one answer.
//!
//! These are pure serde types: the process itself is started by
//! `ods-provider-process` (ADR-0031 §6), and only there.
//!
//! A request:
//!
//! ```json
//! {"protocol": {"major": 1, "minor": 0}, "request": "check",
//!  "check": {"id": "pii.columns_tagged", "params": {}},
//!  "scope": {"nodes": [{"id": "model.shop.orders", "name": "orders", "resource_type": "model"}]},
//!  "artifacts": {"manifest": "target/manifest.json"}}
//! ```
//!
//! and its answer, one finding per node in the scope, and none for any other:
//!
//! ```json
//! {"protocol": {"major": 1, "minor": 0},
//!  "findings": [{"node": "model.shop.orders", "status": "pass", "reason": "tagged"}]}
//! ```
//!
//! A plugin's handshake, and its answer: what it is, and each contract it implements
//! with the version it was built against.
//!
//! ```json
//! {"protocol": {"major": 1, "minor": 0}, "request": "describe"}
//! ```
//!
//! ```json
//! {"protocol": {"major": 1, "minor": 0}, "name": "acme", "version": "1.2.0",
//!  "provides": [{"contract": "health_check", "version": {"major": 0, "minor": 2}},
//!               {"contract": "change_provider", "version": {"major": 0, "minor": 1}}],
//!  "checks": [{"id": "acme.pii_tagged", "about": "PII columns are tagged",
//!              "default_severity": "warn"}],
//!  "warehouse": "snowflake", "capabilities": ["relation_versions"]}
//! ```
//!
//! Source versions, asked for in one batch, and answered for every source in request
//! order:
//!
//! ```json
//! {"protocol": {"major": 1, "minor": 0}, "request": "versions",
//!  "sources": [{"id": "source.shop.raw.orders", "name": "raw.orders"}],
//!  "settings": {"warehouse": {"account": "acme", "password": {"secret": "env:SF_PASSWORD"}}}}
//! ```
//!
//! ```json
//! {"protocol": {"major": 1, "minor": 0},
//!  "sources": [{"id": "source.shop.raw.orders",
//!               "version": {"value": "42", "exactness": "exact", "source": "acme"}}]}
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use ods_core::SchemaVersion;

use crate::contracts::changes::{RequestedSource, SourceVersion};
use crate::contracts::error_catalogue::{CatalogueInfo, Classification};
use crate::contracts::health_check::{CheckFinding, CheckInfo, CheckScope};
use crate::contracts::observed_lineage::ObservedLineage;
use crate::contracts::privileges::Access;
use crate::contracts::probe::{ProbeAnswer, ProbeRequest, ProbeTarget};
use crate::contracts::relation_link::{NoRelationLink, RelationLink};
use crate::contracts::run_events::ErrorSummary;

/// A protocol version: a host refuses an answer whose major it doesn't speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProtocolVersion {
    /// Changes when a field is removed or changes meaning.
    pub major: u32,
    /// Changes when a field is added.
    pub minor: u32,
}

impl ProtocolVersion {
    /// The version `major.minor`.
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }
}

/// The protocol this SDK speaks.
pub const PROTOCOL: ProtocolVersion = ProtocolVersion::new(1, 0);

/// The check a request is for, as configured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CheckCall {
    /// The check's id.
    pub id: String,
    /// Its configured parameters (`params` in its `[[health.checks]]` entry), as given.
    #[serde(default)]
    pub params: BTreeMap<String, serde_json::Value>,
}

impl CheckCall {
    /// The check `id`, with `params`.
    pub fn new(id: impl Into<String>, params: BTreeMap<String, serde_json::Value>) -> Self {
        Self {
            id: id.into(),
            params,
        }
    }
}

/// Asks a check for its findings about the nodes in `scope`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CheckRequest {
    /// [`PROTOCOL`] when a host writes it.
    pub protocol: ProtocolVersion,
    /// What is asked: always `check` here (ADR-0031 §5 adds others).
    pub request: String,
    /// The check.
    pub check: CheckCall,
    /// The nodes to answer about, with what ODS knows of each, and the last run's
    /// failures.
    pub scope: CheckScope,
    /// The project's artifacts, by name (e.g. `manifest`), as paths a check can read.
    #[serde(default)]
    pub artifacts: BTreeMap<String, String>,
}

impl CheckRequest {
    /// A request, in this SDK's protocol, for `check`'s findings about `scope`.
    pub fn new(check: CheckCall, scope: CheckScope, artifacts: BTreeMap<String, String>) -> Self {
        Self {
            protocol: PROTOCOL,
            request: "check".to_owned(),
            check,
            scope,
            artifacts,
        }
    }
}

/// A check's answer: one finding per node it was asked about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CheckResponse {
    /// The protocol the answer is in.
    pub protocol: ProtocolVersion,
    /// Its findings.
    pub findings: Vec<CheckFinding>,
}

impl CheckResponse {
    /// An answer, in this SDK's protocol, of `findings`.
    pub fn new(findings: Vec<CheckFinding>) -> Self {
        Self {
            protocol: PROTOCOL,
            findings,
        }
    }
}

/// Asks a plugin what it is and provides: the handshake, sent before any call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct DescribeRequest {
    /// [`PROTOCOL`] when a host writes it.
    pub protocol: ProtocolVersion,
    /// Always `describe`.
    pub request: String,
}

impl DescribeRequest {
    /// The handshake, in this SDK's protocol.
    pub fn new() -> Self {
        Self {
            protocol: PROTOCOL,
            request: "describe".to_owned(),
        }
    }
}

impl Default for DescribeRequest {
    fn default() -> Self {
        Self::new()
    }
}

/// A contract a plugin implements, and the version it was built against, which the
/// host accepts or refuses (`Contract::accepts`, ADR-0006 §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Provided {
    /// The contract's name, e.g. `health_check`.
    pub contract: String,
    /// The contract version the plugin was built against.
    pub version: SchemaVersion,
}

impl Provided {
    /// `contract`, at `version`.
    pub fn new(contract: impl Into<String>, version: SchemaVersion) -> Self {
        Self {
            contract: contract.into(),
            version,
        }
    }
}

/// A plugin's answer to [`DescribeRequest`]. What it doesn't list, it isn't asked for;
/// what a host doesn't know, it lists and otherwise ignores (ADR-0031 §3c).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct DescribeResponse {
    /// The protocol the answer is in.
    pub protocol: ProtocolVersion,
    /// The plugin's own name, for people.
    #[serde(default)]
    pub name: String,
    /// The plugin's own version, for people.
    #[serde(default)]
    pub version: String,
    /// Each contract it implements.
    #[serde(default)]
    pub provides: Vec<Provided>,
    /// Its health checks, when it provides `health_check`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckInfo>,
    /// The warehouse kind it serves (e.g. a dbt adapter's), when it provides a
    /// warehouse's contracts such as `change_provider`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse: Option<String>,
    /// What it reads as a source's version, for people, e.g. `Snowflake table
    /// versions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versions_read: Option<String>,
    /// The capabilities its providers have (ADR-0006 §3), by name: for
    /// `change_provider`, which kind of version it reads, `relation_versions` (a
    /// table's own version) or `source_freshness` (its last load), which is how a
    /// planner chooses among readings. Names a host doesn't know are ignored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// The SQL dialect of its warehouse, by the shared parser's name for it (e.g.
    /// `snowflake`), for column lineage (ADR-0031 §3a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
    /// Its error catalogue's name and version, when it implements `error_catalogue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalogue: Option<CatalogueInfo>,
}

impl DescribeResponse {
    /// An answer, in this SDK's protocol, for the plugin `name` at `version`, providing
    /// nothing yet.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL,
            name: name.into(),
            version: version.into(),
            provides: Vec::new(),
            checks: Vec::new(),
            warehouse: None,
            versions_read: None,
            capabilities: Vec::new(),
            dialect: None,
            catalogue: None,
        }
    }

    /// The version of `contract` it was built against, if it provides it.
    pub fn provided(&self, contract: &str) -> Option<SchemaVersion> {
        self.provides
            .iter()
            .find(|p| p.contract == contract)
            .map(|p| p.version)
    }
}

/// Asks a plugin for the current data versions of `sources` (`change_provider`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct VersionsRequest {
    /// [`PROTOCOL`] when a host writes it.
    pub protocol: ProtocolVersion,
    /// Always `versions`.
    pub request: String,
    /// The sources, each once.
    pub sources: Vec<RequestedSource>,
    /// The `settings` of every `[providers.<name>]` of the plugin's warehouse kind, by
    /// name, as configured: a secret stays a reference, never resolved (AGENTS rule 9).
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
}

impl VersionsRequest {
    /// A request, in this SDK's protocol, for `sources`' versions.
    pub fn new(
        sources: Vec<RequestedSource>,
        settings: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        Self {
            protocol: PROTOCOL,
            request: "versions".to_owned(),
            sources,
            settings,
        }
    }
}

/// What a plugin read about one source: `{"id": …, "version": {…}}` or
/// `{"id": …, "unknown": "why"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SourceAnswer {
    /// The source's id, as asked.
    pub id: String,
    /// Its version, or why it has none.
    #[serde(flatten)]
    pub version: SourceVersion,
}

impl SourceAnswer {
    /// `id`'s answer.
    pub fn new(id: impl Into<String>, version: SourceVersion) -> Self {
        Self {
            id: id.into(),
            version,
        }
    }
}

/// A plugin's answer to [`VersionsRequest`]: every source asked about, in request order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct VersionsResponse {
    /// The protocol the answer is in.
    pub protocol: ProtocolVersion,
    /// Each source's answer.
    pub sources: Vec<SourceAnswer>,
}

impl VersionsResponse {
    /// An answer, in this SDK's protocol, of `sources`.
    pub fn new(sources: Vec<SourceAnswer>) -> Self {
        Self {
            protocol: PROTOCOL,
            sources,
        }
    }
}

/// The settings of every `[providers.<name>]` of a plugin's warehouse kind, by name, as
/// configured: a secret stays a reference, never resolved (AGENTS rule 9).
pub type WarehouseSettings = BTreeMap<String, serde_json::Value>;

/// Asks a plugin for the links to `relations`, named as the project's artifacts render
/// them (`relation_linker`, ADR-0031 §3d).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LinkRequest {
    /// [`PROTOCOL`] when a host writes it.
    pub protocol: ProtocolVersion,
    /// Always `link`.
    pub request: String,
    /// The relations, each once.
    pub relations: Vec<String>,
    /// Its warehouse's settings.
    #[serde(default)]
    pub settings: WarehouseSettings,
}

impl LinkRequest {
    /// A request, in this SDK's protocol, for `relations`' links.
    pub fn new(relations: Vec<String>, settings: WarehouseSettings) -> Self {
        Self {
            protocol: PROTOCOL,
            request: "link".to_owned(),
            relations,
            settings,
        }
    }
}

/// One relation's link, or why it has none: `{"relation": …, "link": {"url", "label"}}`
/// or `{"relation": …, "no_link": {"reason": …}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LinkAnswer {
    /// The relation, as asked.
    pub relation: String,
    /// Its link, or why there is none.
    #[serde(flatten)]
    pub outcome: LinkOutcome,
}

/// A link, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LinkOutcome {
    /// The link.
    Link(RelationLink),
    /// Why there is none.
    NoLink(NoRelationLink),
}

impl LinkAnswer {
    /// `relation`'s answer.
    pub fn new(relation: impl Into<String>, outcome: Result<RelationLink, NoRelationLink>) -> Self {
        Self {
            relation: relation.into(),
            outcome: match outcome {
                Ok(link) => LinkOutcome::Link(link),
                Err(why) => LinkOutcome::NoLink(why),
            },
        }
    }
}

/// A plugin's answer to [`LinkRequest`]: every relation asked about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LinkResponse {
    /// The protocol the answer is in.
    pub protocol: ProtocolVersion,
    /// Each relation's answer.
    pub links: Vec<LinkAnswer>,
}

impl LinkResponse {
    /// An answer, in this SDK's protocol, of `links`.
    pub fn new(links: Vec<LinkAnswer>) -> Self {
        Self {
            protocol: PROTOCOL,
            links,
        }
    }
}

/// Asks a plugin to classify failed nodes' errors, in one batch (`error_catalogue`).
/// Each is an [`ErrorSummary`], whose values are already removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ClassifyRequest {
    /// [`PROTOCOL`] when a host writes it.
    pub protocol: ProtocolVersion,
    /// Always `classify`.
    pub request: String,
    /// The errors.
    pub errors: Vec<ErrorSummary>,
}

impl ClassifyRequest {
    /// A request, in this SDK's protocol, to classify `errors`.
    pub fn new(errors: Vec<ErrorSummary>) -> Self {
        Self {
            protocol: PROTOCOL,
            request: "classify".to_owned(),
            errors,
        }
    }
}

/// A plugin's answer to [`ClassifyRequest`]: one classification per error, in request
/// order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ClassifyResponse {
    /// The protocol the answer is in.
    pub protocol: ProtocolVersion,
    /// Each error's classification.
    pub classifications: Vec<Classification>,
}

impl ClassifyResponse {
    /// An answer, in this SDK's protocol, of `classifications`.
    pub fn new(classifications: Vec<Classification>) -> Self {
        Self {
            protocol: PROTOCOL,
            classifications,
        }
    }
}

/// Asks a plugin to read the lineage the user exported from its warehouse
/// (`observed_lineage_source`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ObservedLineageRequest {
    /// [`PROTOCOL`] when a host writes it.
    pub protocol: ProtocolVersion,
    /// Always `observed_lineage`.
    pub request: String,
    /// The export, as an absolute path.
    pub export: String,
    /// Its warehouse's settings.
    #[serde(default)]
    pub settings: WarehouseSettings,
}

impl ObservedLineageRequest {
    /// A request, in this SDK's protocol, to read `export`.
    pub fn new(export: impl Into<String>, settings: WarehouseSettings) -> Self {
        Self {
            protocol: PROTOCOL,
            request: "observed_lineage".to_owned(),
            export: export.into(),
            settings,
        }
    }
}

/// A plugin's answer to [`ObservedLineageRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ObservedLineageResponse {
    /// The protocol the answer is in.
    pub protocol: ProtocolVersion,
    /// What it read.
    pub lineage: ObservedLineage,
}

impl ObservedLineageResponse {
    /// An answer, in this SDK's protocol, of `lineage`.
    pub fn new(lineage: ObservedLineage) -> Self {
        Self {
            protocol: PROTOCOL,
            lineage,
        }
    }
}

/// Asks a plugin to run read-only statements against `targets`' relations, over its own
/// connection (`relation_probe`, ADR-0031 §3d).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProbeCall {
    /// [`PROTOCOL`] when a host writes it.
    pub protocol: ProtocolVersion,
    /// Always `probe`.
    pub request: String,
    /// What to run, against which relations.
    pub probe: ProbeRequest,
    /// The nodes, each with the relation the build made: probe nothing else.
    pub targets: Vec<ProbeTarget>,
    /// Its warehouse's settings.
    #[serde(default)]
    pub settings: WarehouseSettings,
}

impl ProbeCall {
    /// A request, in this SDK's protocol, to run `probe` against `targets`.
    pub fn new(
        probe: ProbeRequest,
        targets: Vec<ProbeTarget>,
        settings: WarehouseSettings,
    ) -> Self {
        Self {
            protocol: PROTOCOL,
            request: "probe".to_owned(),
            probe,
            targets,
            settings,
        }
    }
}

/// What probing one target found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProbeTargetAnswer {
    /// The target's id, as asked.
    pub id: String,
    /// Its rows, or why it has none.
    pub answer: ProbeAnswer,
}

impl ProbeTargetAnswer {
    /// `id`'s answer.
    pub fn new(id: impl Into<String>, answer: ProbeAnswer) -> Self {
        Self {
            id: id.into(),
            answer,
        }
    }
}

/// A plugin's answer to [`ProbeCall`]: every target asked about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProbeResponse {
    /// The protocol the answer is in.
    pub protocol: ProtocolVersion,
    /// Each target's answer.
    pub targets: Vec<ProbeTargetAnswer>,
}

impl ProbeResponse {
    /// An answer, in this SDK's protocol, of `targets`.
    pub fn new(targets: Vec<ProbeTargetAnswer>) -> Self {
        Self {
            protocol: PROTOCOL,
            targets,
        }
    }
}

/// Asks a plugin what the login its probes run under may do on `targets`' relations
/// (`relation_privileges`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PrivilegesRequest {
    /// [`PROTOCOL`] when a host writes it.
    pub protocol: ProtocolVersion,
    /// Always `privileges`.
    pub request: String,
    /// The nodes.
    pub targets: Vec<ProbeTarget>,
    /// Its warehouse's settings.
    #[serde(default)]
    pub settings: WarehouseSettings,
}

impl PrivilegesRequest {
    /// A request, in this SDK's protocol, for the login's access to `targets`.
    pub fn new(targets: Vec<ProbeTarget>, settings: WarehouseSettings) -> Self {
        Self {
            protocol: PROTOCOL,
            request: "privileges".to_owned(),
            targets,
            settings,
        }
    }
}

/// What the login may do on one target's relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct AccessAnswer {
    /// The target's id, as asked.
    pub id: String,
    /// What the login may do.
    pub access: Access,
}

impl AccessAnswer {
    /// `id`'s answer.
    pub fn new(id: impl Into<String>, access: Access) -> Self {
        Self {
            id: id.into(),
            access,
        }
    }
}

/// A plugin's answer to [`PrivilegesRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PrivilegesResponse {
    /// The protocol the answer is in.
    pub protocol: ProtocolVersion,
    /// The login, as the warehouse names it, when known.
    #[serde(default)]
    pub login: Option<String>,
    /// Each target's answer.
    pub targets: Vec<AccessAnswer>,
}

impl PrivilegesResponse {
    /// An answer, in this SDK's protocol, for `login`, of `targets`.
    pub fn new(login: Option<String>, targets: Vec<AccessAnswer>) -> Self {
        Self {
            protocol: PROTOCOL,
            login,
            targets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::health_check::{NodeFacts, Status};

    #[test]
    fn a_request_and_an_answer_have_the_documented_shape() {
        let node = NodeFacts::new("model.shop.orders", "orders", "model");
        let request = CheckRequest::new(
            CheckCall::new("pii.tagged", BTreeMap::from([("min".into(), 2.into())])),
            CheckScope::new(vec![node], None),
            BTreeMap::from([("manifest".into(), "target/manifest.json".into())]),
        );
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(
            json["protocol"],
            serde_json::json!({"major": 1, "minor": 0})
        );
        assert_eq!(json["request"], "check");
        assert_eq!(json["check"]["id"], "pii.tagged");
        assert_eq!(json["check"]["params"]["min"], 2);
        assert_eq!(json["scope"]["nodes"][0]["id"], "model.shop.orders");
        assert_eq!(json["artifacts"]["manifest"], "target/manifest.json");
        let back: CheckRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back, request);

        let answer: CheckResponse = serde_json::from_str(
            r#"{"protocol": {"major": 1, "minor": 3},
                "findings": [{"node": "model.shop.orders", "status": "fail", "reason": "untagged",
                              "evidence": {"columns": "email"}}]}"#,
        )
        .unwrap();
        assert_eq!(answer.protocol, ProtocolVersion::new(1, 3));
        assert_eq!(answer.findings[0].status, Status::Fail);
        assert_eq!(answer.findings[0].evidence["columns"], "email");
    }

    #[test]
    fn a_handshake_and_a_versions_call_have_the_documented_shape() {
        use crate::contracts::changes::CHANGE_PROVIDER;
        use crate::contracts::health_check::HEALTH_CHECK;
        use ods_core::state::{DataVersion, Exactness};

        assert_eq!(
            serde_json::to_value(DescribeRequest::new()).unwrap(),
            serde_json::json!({"protocol": {"major": 1, "minor": 0}, "request": "describe"})
        );
        // A plugin that adds fields a host doesn't know yet is still understood.
        let described: DescribeResponse = serde_json::from_str(
            r#"{"protocol": {"major": 1, "minor": 0}, "name": "acme", "version": "1.2.0",
                "provides": [{"contract": "health_check", "version": {"major": 0, "minor": 2}},
                             {"contract": "change_provider", "version": {"major": 0, "minor": 1}}],
                "checks": [{"id": "acme.pii_tagged", "about": "PII columns are tagged",
                            "default_severity": "warn"}],
                "warehouse": "snowflake", "capabilities": ["relation_versions"],
                "added_later": true}"#,
        )
        .unwrap();
        assert_eq!(
            described.provided("health_check"),
            Some(HEALTH_CHECK.version)
        );
        assert_eq!(
            described.provided("change_provider"),
            Some(CHANGE_PROVIDER.version)
        );
        assert_eq!(described.provided("relation_linker"), None);
        assert_eq!(described.checks[0].id, "acme.pii_tagged");
        assert_eq!(described.warehouse.as_deref(), Some("snowflake"));
        assert_eq!(described.capabilities, ["relation_versions"]);

        let request = VersionsRequest::new(
            vec![RequestedSource::new("source.shop.raw.orders", "raw.orders")],
            BTreeMap::from([(
                "warehouse".into(),
                serde_json::json!({"account": "acme", "password": {"secret": "env:SF_PASSWORD"}}),
            )]),
        );
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["request"], "versions");
        assert_eq!(json["sources"][0]["id"], "source.shop.raw.orders");
        assert_eq!(json["sources"][0]["name"], "raw.orders");
        assert_eq!(
            json["settings"]["warehouse"]["password"]["secret"],
            "env:SF_PASSWORD"
        );

        let answer = VersionsResponse::new(vec![
            SourceAnswer::new(
                "source.shop.raw.orders",
                SourceVersion::Version(DataVersion::new("42", Exactness::Exact, "acme")),
            ),
            SourceAnswer::new(
                "source.shop.raw.payments",
                SourceVersion::Unknown("no such table".into()),
            ),
        ]);
        let json = serde_json::to_value(&answer).unwrap();
        assert_eq!(
            json["sources"],
            serde_json::json!([
                {"id": "source.shop.raw.orders",
                 "version": {"value": "42", "exactness": "exact", "source": "acme"}},
                {"id": "source.shop.raw.payments", "unknown": "no such table"}
            ])
        );
        let back: VersionsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back, answer);
    }

    #[test]
    fn warehouse_requests_have_the_documented_shape() {
        use crate::contracts::probe::{ProbeFilter, ProbeStatement};

        let links = LinkResponse::new(vec![
            LinkAnswer::new(
                "\"db\".\"main\".\"orders\"",
                Ok(RelationLink::new(
                    "https://acme.example/t/orders",
                    "Open in Acme",
                )),
            ),
            LinkAnswer::new(
                "orders",
                Err(NoRelationLink::NotConfigured {
                    setting: "account".to_owned(),
                }),
            ),
        ]);
        let json = serde_json::to_value(&links).unwrap();
        assert_eq!(
            json["links"][0]["link"]["url"],
            "https://acme.example/t/orders"
        );
        assert_eq!(json["links"][1]["no_link"]["reason"], "not_configured");
        assert_eq!(serde_json::from_value::<LinkResponse>(json).unwrap(), links);

        // A probe as a plugin reads it: checked as when built, so a statement that
        // doesn't name its relation is refused.
        let probe = ProbeRequest::new(
            ProbeFilter::kinds(["table"]).unwrap(),
            vec![ProbeStatement::new("select count(*) as n from {relation}", ["n"]).unwrap()],
        )
        .unwrap();
        let call = ProbeCall::new(
            probe,
            vec![ProbeTarget::new("model.shop.orders", "orders").expecting("db.main.orders")],
            WarehouseSettings::new(),
        );
        let json = serde_json::to_value(&call).unwrap();
        assert_eq!(json["request"], "probe");
        assert_eq!(json["targets"][0]["relation"], "db.main.orders");
        assert_eq!(
            serde_json::from_value::<ProbeCall>(json.clone()).unwrap(),
            call
        );
        let mut bad = json;
        bad["probe"]["statements"][0]["template"] = "delete from orders".into();
        assert!(serde_json::from_value::<ProbeCall>(bad).is_err());

        let answer: ProbeResponse = serde_json::from_str(
            r#"{"protocol": {"major": 1, "minor": 0},
                "targets": [{"id": "model.shop.orders", "answer": {"rows": [{"n": "3"}]}},
                            {"id": "model.shop.items", "answer": {"unknown": "no such table"}}]}"#,
        )
        .unwrap();
        assert_eq!(
            answer.targets[1].answer,
            ProbeAnswer::Unknown("no such table".to_owned())
        );
        let access: PrivilegesResponse = serde_json::from_str(
            r#"{"protocol": {"major": 1, "minor": 0}, "login": "health_ro",
                "targets": [{"id": "model.shop.orders", "access": "read_only"},
                            {"id": "model.shop.items", "access": {"elevated": ["MODIFY"]}}]}"#,
        )
        .unwrap();
        assert_eq!(access.login.as_deref(), Some("health_ro"));
        assert_eq!(access.targets[0].access, Access::ReadOnly);
        assert_eq!(
            access.targets[1].access,
            Access::Elevated(vec!["MODIFY".to_owned()])
        );
    }
}
