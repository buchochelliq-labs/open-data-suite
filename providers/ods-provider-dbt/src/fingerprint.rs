//! Code fingerprints of dbt nodes (#13, ADR-0013 "Fingerprints").
//!
//! Each component is hashed separately so a change can be named:
//!
//! | Component | From |
//! |---|---|
//! | `scheme` | [`SCHEME`]: changes whenever this table does |
//! | `sql` | SQL models and snapshots: the compiled SQL, [normalised](crate::normalize) so formatting doesn't count |
//! | `file` | dbt's checksum of the source file: seeds, Python models, and SQL models whose Jinja isn't [pure](crate::normalize::jinja_is_pure) (or whose raw code isn't recorded) |
//! | `compiled_code` | Python models: the compiled code, as is |
//! | `config` | the resolved config, minus settings that don't change what gets built |
//! | `macros` | every macro it calls, directly or not, plus its materialization and the `generate_*_name` macros that place it |
//! | `contract` | declared column types and constraints |
//! | `relation` | the relation it builds (database, schema, alias) |
//! | `engine` | dbt version and adapter |
//!
//! SQL models and snapshots usually have no `file` component: the compiled SQL, config
//! and macros are what gets built, so an edit to the file that changes none of them (a
//! comment, a reformat, Jinja that renders the same SQL) doesn't rebuild the model. The
//! exception is Jinja that could run SQL itself: anything beyond `ref`, `source`,
//! `config`, `var`, `is_incremental`, `if`/`for`/`set`. What that runs isn't in the
//! compiled SQL, so the file still counts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use ods_core::state::{Fingerprint, sha256_hex};
use serde_json::Value;

use crate::normalize::{jinja_is_pure, normalize_sql};
use crate::{Manifest, ManifestNode, ResourceType};

/// The fingerprint scheme. Bump it whenever a component's meaning changes, so snapshots
/// fingerprinted the old way are never compared as equal.
///
/// 2: SQL is normalised and SQL models no longer hash their file (#209).
pub const SCHEME: &str = "dbt/2";

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

fn config_content(node: &ManifestNode) -> Result<String, String> {
    if !node.config.raw.is_object() {
        return Err("dbt recorded no config for it".to_owned());
    }
    let mut config = node.config.raw.clone();
    if let Value::Object(map) = &mut config {
        for key in IGNORED_CONFIG_KEYS {
            map.remove(key);
        }
    }
    let mut out = String::new();
    canonical_json(&config, &mut out);
    Ok(out)
}

/// Macros that shape a node without appearing in its `depends_on`: the materialization
/// that builds it, and the `generate_*_name` macros that decide where it goes.
fn implicit_macros<'m>(manifest: &'m Manifest, node: &ManifestNode) -> Vec<&'m str> {
    let materialization = node
        .materialized
        .as_deref()
        .map(|m| format!("materialization_{m}_"));
    manifest
        .macros
        .keys()
        .filter(|id| {
            let name = id.rsplit('.').next().unwrap_or(id);
            materialization
                .as_deref()
                .is_some_and(|m| name.starts_with(m))
                || [
                    "generate_schema_name",
                    "generate_alias_name",
                    "generate_database_name",
                ]
                .iter()
                .any(|g| name.ends_with(g))
        })
        .map(String::as_str)
        .collect()
}

/// Every macro `node` calls, directly or through other macros, as `id sha256` lines.
fn macros_content(manifest: &Manifest, node: &ManifestNode) -> Result<String, String> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&str> = node.depends_on_macros.iter().map(String::as_str).collect();
    stack.extend(implicit_macros(manifest, node));
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
        let m = manifest
            .macros
            .get(id)
            .ok_or_else(|| format!("it calls macro `{id}`, which the artifacts don't include"))?;
        let _ = writeln!(out, "{id} {}", sha256_hex(m.sql.as_bytes()));
    }
    Ok(out)
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
    let mut components: Vec<(&str, String)> = vec![
        (Fingerprint::SCHEME, SCHEME.to_owned()),
        ("config", config_content(node)?),
        ("macros", macros_content(manifest, node)?),
        ("contract", contract_content(node)),
        ("relation", node.relation_name.clone().unwrap_or_default()),
        (
            "engine",
            format!(
                "dbt {} / {}",
                manifest.dbt_version.as_deref().unwrap_or("unknown"),
                manifest.adapter_type.as_deref().unwrap_or("unknown")
            ),
        ),
    ];
    let compiled = || {
        node.compiled_code
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| {
                "no compiled SQL in the manifest; run `dbt compile` (or `run`/`build`) first"
                    .to_owned()
            })
    };
    let file = || {
        node.checksum
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                "dbt recorded no checksum of its content (e.g. a seed over 1 MiB)".to_owned()
            })
    };
    let code = matches!(
        node.resource_type,
        ResourceType::Model | ResourceType::Snapshot
    );
    let python = node.language.as_deref() == Some("python");
    if code && !python {
        let sql = compiled()?;
        // Unnormalisable SQL is hashed as is: formatting then counts, which only ever
        // rebuilds more.
        components.push(("sql", normalize_sql(sql).unwrap_or_else(|| sql.to_owned())));
        if !node.raw_code.as_deref().is_some_and(jinja_is_pure) {
            components.push(("file", file()?));
        }
        return Ok(Fingerprint::from_content(components).with_cosmetic("sql", sql));
    }
    components.push(("file", file()?));
    if code {
        components.push(("compiled_code", compiled()?.to_owned()));
    }
    Ok(Fingerprint::from_content(components))
}

/// The project and package macros a check calls, directly or through other macros,
/// as `id sha256` lines. dbt's own and the adapter's macros are left out (the engine
/// version stands for them): `dbt test`/`dbt build` add ones they resolve at run time
/// (e.g. `get_limit_subquery`) to a test's `depends_on`, which `dbt compile` doesn't,
/// and hashing them would make a test look changed from one command to the next.
fn check_macros_content(manifest: &Manifest, test: &ManifestNode) -> Result<String, String> {
    let adapter = manifest
        .adapter_type
        .as_deref()
        .map(|a| format!("macro.dbt_{a}."));
    let engine = |id: &str| {
        id.starts_with("macro.dbt.") || adapter.as_deref().is_some_and(|a| id.starts_with(a))
    };
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&str> = test.depends_on_macros.iter().map(String::as_str).collect();
    while let Some(id) = stack.pop() {
        if engine(id) || !seen.insert(id) {
            continue;
        }
        if let Some(m) = manifest.macros.get(id) {
            stack.extend(m.depends_on.iter().map(String::as_str));
        }
    }
    let mut out = String::new();
    for id in seen {
        let m = manifest
            .macros
            .get(id)
            .ok_or_else(|| format!("it calls macro `{id}`, which the artifacts don't include"))?;
        let _ = writeln!(out, "{id} {}", sha256_hex(m.sql.as_bytes()));
    }
    Ok(out)
}

/// The scheme [`checks_digest`] uses; changing it makes every node untested once.
pub const CHECKS_SCHEME: &str = "dbt-checks/2";

/// The checks that cover `node`: the data tests and unit tests that read it, which
/// `dbt test`/`dbt build` run with it by default.
pub fn checks_of<'m>(manifest: &'m Manifest, node: &str) -> Vec<&'m str> {
    let mut checks: Vec<&str> = manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == ResourceType::Test && n.depends_on.iter().any(|d| d == node))
        .map(|n| n.unique_id.as_str())
        .chain(
            manifest
                .unit_tests
                .iter()
                .filter(|t| t.depends_on.iter().any(|d| d == node))
                .map(|t| t.unique_id.as_str()),
        )
        .collect();
    checks.sort_unstable();
    checks
}

/// A digest of the checks that cover `node` and their definitions (#220): a build
/// tested against these checks is tested; against others, it isn't. `None` when it
/// has no checks, or one can't be fingerprinted, so it is never marked tested.
pub fn checks_digest(manifest: &Manifest, node: &str) -> Option<String> {
    let checks = checks_of(manifest, node);
    if checks.is_empty() {
        return None;
    }
    let mut components = vec![
        (Fingerprint::SCHEME.to_owned(), CHECKS_SCHEME.to_owned()),
        // dbt's own and the adapter's macros aren't hashed below: they are this.
        (
            "engine".to_owned(),
            format!(
                "dbt {} / {}",
                manifest.dbt_version.as_deref().unwrap_or("unknown"),
                manifest.adapter_type.as_deref().unwrap_or("unknown")
            ),
        ),
    ];
    for id in checks {
        let content = if let Some(test) = manifest.nodes.iter().find(|n| n.unique_id == id) {
            // A generic test is its macro and arguments; a singular test its SQL.
            let mut arguments = String::new();
            if let Some(t) = &test.test {
                canonical_json(&t.arguments, &mut arguments);
            }
            format!(
                "config {}\nmacros {}\ncode {}\narguments {}\ndepends_on {}\n",
                config_content(test).ok()?,
                check_macros_content(manifest, test).ok()?,
                test.raw_code.as_deref().unwrap_or(""),
                arguments,
                test.depends_on.join(","),
            )
        } else {
            let unit = manifest.unit_tests.iter().find(|t| t.unique_id == id)?;
            let mut definition = unit.definition.clone();
            if let Value::Object(map) = &mut definition {
                // When it was parsed, not what it checks.
                map.remove("created_at");
            }
            let mut out = String::new();
            canonical_json(&definition, &mut out);
            out
        };
        components.push((id.to_owned(), content));
    }
    Some(Fingerprint::from_content(components).digest)
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
