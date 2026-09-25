//! Code fingerprints of dbt nodes (#13, ADR-0013 "Fingerprints").
//!
//! Each component is hashed separately so a change can be named:
//!
//! | Component | From |
//! |---|---|
//! | `file` | dbt's checksum of the node's source file |
//! | `compiled_sql` | the compiled SQL (models and snapshots) |
//! | `config` | the resolved config, minus settings that don't change what gets built |
//! | `macros` | every macro it calls, directly or not |
//! | `contract` | declared column types and constraints |
//! | `engine` | dbt version and adapter |

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use ods_core::state::{Fingerprint, sha256_hex};
use serde_json::Value;

use crate::{Manifest, ManifestNode, ResourceType};

/// Config keys that describe or schedule a node rather than change what it builds.
/// Changing them doesn't rebuild anything.
pub const IGNORED_CONFIG_KEYS: [&str; 5] = ["docs", "freshness", "meta", "state", "tags"];

/// JSON with object keys sorted at every level, whatever the parser kept.
fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            out.push('{');
            for (i, (key, item)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical_json(item, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn config_content(node: &ManifestNode) -> String {
    let mut config = node.config.raw.clone();
    if let Value::Object(map) = &mut config {
        for key in IGNORED_CONFIG_KEYS {
            map.remove(key);
        }
    }
    let mut out = String::new();
    canonical_json(&config, &mut out);
    out
}

/// Every macro `node` calls, directly or through other macros, as `id sha256` lines.
fn macros_content(manifest: &Manifest, node: &ManifestNode) -> String {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&str> = node.depends_on_macros.iter().map(String::as_str).collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(m) = manifest.macros.get(id) {
            stack.extend(m.depends_on.iter().map(String::as_str));
        }
    }
    let mut out = String::new();
    for id in seen {
        let digest = manifest
            .macros
            .get(id)
            .map_or_else(|| "missing".to_owned(), |m| sha256_hex(m.sql.as_bytes()));
        let _ = writeln!(out, "{id} {digest}");
    }
    out
}

fn contract_content(node: &ManifestNode) -> String {
    let mut out = String::new();
    for (column, data_type) in &node.declared_types {
        let _ = writeln!(out, "type {column} {data_type}");
    }
    let mut constraints: Vec<String> = node
        .constraints
        .iter()
        .map(|c| {
            format!(
                "constraint {} [{}] to={} [{}] expr={}",
                c.kind,
                c.columns.join(","),
                c.to.as_deref().unwrap_or(""),
                c.to_columns.join(","),
                c.expression.as_deref().unwrap_or("")
            )
        })
        .collect();
    constraints.sort();
    for line in constraints {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The fingerprint of a model, seed or snapshot, or why it can't be made completely
/// (which makes the node always build).
///
/// # Errors
/// Returns a reason when dbt didn't record enough: no file checksum, or no compiled SQL
/// for a model or snapshot (the manifest came from `dbt parse`).
pub fn fingerprint(manifest: &Manifest, node: &ManifestNode) -> Result<Fingerprint, String> {
    let file = node
        .checksum
        .as_deref()
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "dbt recorded no checksum of its file".to_owned())?;
    let mut components: Vec<(&str, String)> = vec![
        ("file", file.to_owned()),
        ("config", config_content(node)),
        ("macros", macros_content(manifest, node)),
        ("contract", contract_content(node)),
        (
            "engine",
            format!(
                "dbt {} / {}",
                manifest.dbt_version.as_deref().unwrap_or("unknown"),
                manifest.adapter_type.as_deref().unwrap_or("unknown")
            ),
        ),
    ];
    if matches!(
        node.resource_type,
        ResourceType::Model | ResourceType::Snapshot
    ) {
        let compiled = node
            .compiled_code
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| {
                "no compiled SQL in the manifest; run `dbt compile` (or `run`/`build`) first"
                    .to_owned()
            })?;
        components.push(("compiled_sql", compiled.to_owned()));
    }
    Ok(Fingerprint::from_content(components))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        let value: Value =
            serde_json::from_str(r#"{"b": {"z": 1, "a": [ {"y": 2, "x": 1} ]}, "a": null}"#)
                .unwrap();
        let mut out = String::new();
        canonical_json(&value, &mut out);
        assert_eq!(out, r#"{"a":null,"b":{"a":[{"x":1,"y":2}],"z":1}}"#);
    }
}
