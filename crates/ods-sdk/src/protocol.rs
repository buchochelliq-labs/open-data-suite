//! The script protocol (ADR-0030 §4; ADR-0031 §5): what a host writes to a checking
//! process's stdin, and what it reads back from its stdout. One JSON document each way,
//! versioned by [`PROTOCOL`]'s major.
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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contracts::health_check::{CheckFinding, CheckScope};

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
}
