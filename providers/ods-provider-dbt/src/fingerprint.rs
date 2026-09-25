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
//! | `hook_env` | a digest of each environment variable a hook reads with `env_var` (only when one does) |
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

/// Environment variables dbt treats as secrets: not even a digest of them is kept.
const SECRET_ENV_PREFIX: &str = "DBT_ENV_SECRET_";

/// The SQL of a node's pre- and post-hooks, unrendered, as dbt records them.
fn hook_sqls(node: &ManifestNode) -> Vec<&str> {
    let mut sqls = Vec::new();
    for key in ["pre-hook", "post-hook", "pre_hook", "post_hook"] {
        let hooks = match node.config.raw.get(key) {
            Some(Value::Array(items)) => items.iter().collect(),
            Some(other) => vec![other],
            None => Vec::new(),
        };
        for hook in hooks {
            match hook {
                Value::String(sql) => sqls.push(sql.as_str()),
                Value::Object(map) => sqls.extend(map.get("sql").and_then(Value::as_str)),
                _ => {}
            }
        }
    }
    sqls
}

/// Whether `text` calls `name(` as a whole word.
fn calls(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].trim_start();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && after.starts_with('(')
    })
}

/// The first argument of each `name(...)` call in `text`: `Some(literal)` for a quoted
/// string, `None` for anything else.
fn call_args(text: &str, name: &str) -> Vec<Option<String>> {
    let mut args = Vec::new();
    for (at, _) in text.match_indices(name) {
        let before = text[..at].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        let Some(rest) = text[at + name.len()..].trim_start().strip_prefix('(') else {
            continue;
        };
        let rest = rest.trim_start();
        let literal = rest
            .chars()
            .next()
            .filter(|q| matches!(q, '\'' | '"'))
            .and_then(|q| {
                let body = &rest[1..];
                body.find(q).map(|end| body[..end].to_owned())
            });
        args.push(literal);
    }
    args
}

/// What a node's hooks can read that dbt only resolves when they run (#218): the
/// `env_var` values, as a `hook_env` component, or why they can't be fingerprinted.
///
/// dbt records the macros hooks call in `depends_on.macros` (so their source is in
/// `macros`) and the hook SQL in the config (so it is in `config`). What neither shows
/// is the value of `var(...)` and `env_var(...)` in a hook, or in a macro a hook calls.
fn hook_env_content(
    manifest: &Manifest,
    node: &ManifestNode,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<String>, String> {
    let hooks = hook_sqls(node);
    if hooks.is_empty() {
        return Ok(None);
    }
    // The hooks, and every macro they call, directly or not.
    let mut texts: Vec<&str> = hooks.clone();
    let mut stack: Vec<&str> = manifest
        .macros
        .keys()
        .filter(|id| {
            let name = id.rsplit('.').next().unwrap_or(id);
            hooks.iter().any(|h| calls(h, name))
        })
        .map(String::as_str)
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(m) = manifest.macros.get(id) {
            texts.push(&m.sql);
            stack.extend(m.depends_on.iter().map(String::as_str));
        }
    }
    let mut names = BTreeSet::new();
    for text in &texts {
        if calls(text, "var") {
            let which = call_args(text, "var")
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "its hooks read `var({which})`, whose value ODS doesn't see yet (#218), so it is always built"
            ));
        }
        for arg in call_args(text, "env_var") {
            match arg {
                Some(name) if name.starts_with(SECRET_ENV_PREFIX) => {
                    return Err(format!(
                        "its hooks read the secret `{name}`, which ODS doesn't fingerprint, so it is always built"
                    ));
                }
                Some(name) => {
                    names.insert(name);
                }
                None => {
                    return Err(
                        "its hooks read an environment variable ODS can't name, so it is always built"
                            .to_owned(),
                    );
                }
            }
        }
    }
    if names.is_empty() {
        return Ok(None);
    }
    let mut out = String::new();
    for name in names {
        // Only a digest of each value is kept (AGENTS rule 9).
        let value = env(&name).map_or_else(|| "unset".to_owned(), |v| sha256_hex(v.as_bytes()));
        let _ = writeln!(out, "{name} {value}");
    }
    Ok(Some(out))
}

/// The fingerprint of a model, seed or snapshot, or why it can't be made completely
/// (which makes the node always build). Environment variables that hooks read come
/// from this process's environment, which `ods state run` passes on to dbt.
///
/// # Errors
/// Returns a reason when dbt didn't record enough: no file checksum, or no compiled SQL
/// for a model or snapshot (the manifest came from `dbt parse`); or when its hooks read
/// values ODS can't see.
pub fn fingerprint(manifest: &Manifest, node: &ManifestNode) -> Result<Fingerprint, String> {
    fingerprint_with_env(manifest, node, &|name| std::env::var(name).ok())
}

/// [`fingerprint`], reading environment variables from `env`.
///
/// # Errors
/// As [`fingerprint`].
pub fn fingerprint_with_env(
    manifest: &Manifest,
    node: &ManifestNode,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Fingerprint, String> {
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
    if let Some(hook_env) = hook_env_content(manifest, node, env)? {
        components.push(("hook_env", hook_env));
    }
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
