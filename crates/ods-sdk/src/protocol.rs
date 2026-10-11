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
use crate::contracts::health_check::{CheckFinding, CheckInfo, CheckScope};

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
}
