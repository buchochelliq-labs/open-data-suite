//! Which nodes' relations are still in the warehouse, asked through dbt (#230).
//!
//! One `dbt show --inline` call runs [`QUERY`]. dbt renders it with the user's own
//! profile, so ODS never handles a credential (AGENTS.md rule 9), and asks the adapter
//! for every relation, so it works on every adapter with its own quoting and case
//! rules. The query takes no node list, so its size doesn't grow with the project:
//! it checks every model, seed and snapshot that has a relation, and its one row
//! carries the count it checked and the ids of the missing ones. Nothing is written
//! to the user's project (AGENTS.md rule 8).

use std::collections::BTreeSet;

use crate::{Manifest, ResourceType};

/// Marks the row as ODS's answer, not something else dbt printed.
const MARKER: &str = "ods_relation_check";

/// The inline query. `execute` is false while dbt parses it, when there is no
/// connection to ask.
pub(crate) const QUERY: &str = "\
{%- set missing = [] -%}{%- set ns = namespace(checked=0) -%}\
{%- if execute -%}\
{%- for n in graph.nodes.values() if n.resource_type in ('model', 'seed', 'snapshot') \
and n.config.materialized != 'ephemeral' -%}\
{%- set ns.checked = ns.checked + 1 -%}\
{%- if adapter.get_relation(n.database, n.schema, n.alias) is none -%}\
{%- do missing.append(n.unique_id) -%}\
{%- endif -%}\
{%- endfor -%}\
{%- endif -%}\
select '{{ tojson({\"ods_relation_check\": 1, \"checked\": ns.checked, \"missing\": missing}) }}' \
as ods_relations";

/// What the query found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    /// How many relations it checked.
    pub checked: usize,
    /// The nodes whose relation doesn't exist.
    pub missing: BTreeSet<String>,
}

/// The nodes in `manifest` that [`QUERY`] checks, the same way it picks them.
pub(crate) fn checkable(manifest: &Manifest) -> BTreeSet<&str> {
    manifest
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.resource_type,
                ResourceType::Model | ResourceType::Seed | ResourceType::Snapshot
            ) && n.materialized.as_deref() != Some("ephemeral")
        })
        .map(|n| n.unique_id.as_str())
        .collect()
}

/// Reads [`QUERY`]'s answer from what `dbt show --output json --log-format json`
/// printed: the one `ShowNode` event, whose preview is the result as JSON rows.
pub(crate) fn parse(stdout: &str) -> Result<Found, String> {
    let preview = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event.pointer("/info/name").and_then(|n| n.as_str()) == Some("ShowNode"))
        .and_then(|event| {
            event
                .pointer("/data/preview")
                .and_then(|p| p.as_str())
                .map(str::to_owned)
        })
        .ok_or("dbt printed no query result")?;
    let rows: serde_json::Value =
        serde_json::from_str(&preview).map_err(|e| format!("unreadable query result: {e}"))?;
    let answer = rows
        .pointer("/0/ods_relations")
        .and_then(|a| a.as_str())
        .ok_or("the query result has no `ods_relations` column")?;
    let answer: serde_json::Value =
        serde_json::from_str(answer).map_err(|e| format!("unreadable relation check: {e}"))?;
    if answer.get(MARKER).and_then(serde_json::Value::as_u64) != Some(1) {
        return Err("the query result isn't ODS's relation check".to_owned());
    }
    let checked = answer
        .get("checked")
        .and_then(serde_json::Value::as_u64)
        .and_then(|c| usize::try_from(c).ok())
        .ok_or("the relation check has no count")?;
    let missing = answer
        .get("missing")
        .and_then(|m| m.as_array())
        .ok_or("the relation check has no list of missing relations")?
        .iter()
        .map(|id| id.as_str().map(str::to_owned))
        .collect::<Option<BTreeSet<_>>>()
        .ok_or("the list of missing relations isn't a list of ids")?;
    Ok(Found { checked, missing })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As dbt 1.10 prints it (trimmed), with `segment_summary` and `stg_payments` dropped.
    const SHOWN: &str = r#"{"data": {"is_inline": true, "node_name": "inline_query", "output_format": "json", "preview": "[{\"ods_relations\": \"{\\\"ods_relation_check\\\": 1, \\\"checked\\\": 13, \\\"missing\\\": [\\\"model.jaffle_ods.segment_summary\\\", \\\"model.jaffle_ods.stg_payments\\\"]}\"}]", "quiet": true, "unique_id": "sql_operation.jaffle_ods.inline_query"}, "info": {"category": "", "code": "Q041", "level": "info", "name": "ShowNode"}}"#;

    #[test]
    fn reads_the_show_node_event() {
        let found = parse(&format!("not json\n{SHOWN}\n")).unwrap();
        assert_eq!(found.checked, 13);
        assert_eq!(
            found.missing.into_iter().collect::<Vec<_>>(),
            [
                "model.jaffle_ods.segment_summary",
                "model.jaffle_ods.stg_payments"
            ]
        );
    }

    #[test]
    fn anything_else_is_an_error() {
        assert!(parse("").is_err());
        let error = r#"{"data": {}, "info": {"name": "MainEncounteredError"}}"#;
        assert!(parse(error).is_err());
        let other = SHOWN.replace("ods_relation_check", "something_else");
        assert!(parse(&other).is_err());
    }

    #[test]
    fn the_query_is_small_and_names_no_nodes() {
        // Command-line arguments are limited in size (less on Windows).
        assert!(QUERY.len() < 1024, "{}", QUERY.len());
        assert!(QUERY.contains(MARKER));
    }
}
