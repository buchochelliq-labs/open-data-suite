//! Runs a [`ProbeRequest`] against sources' relations through dbt (ADR-0022 §1).
//!
//! One `dbt show --inline` call runs the query [`query`] renders. dbt renders it with the
//! user's own profile, so ODS never handles a credential (AGENTS.md rule 9). As in the
//! relation check (ADR-0016), the query names no source: it iterates `graph.sources`
//! itself, so its size doesn't grow with the project, and only the request travels on
//! the command line. For each source it:
//! - asks the adapter for the relation (`adapter.get_relation`); none means unknown;
//! - checks the relation's kind against the filter's kinds, and, when the filter names
//!   a format `f`, that the adapter's relation says `is_<f>` is true. An adapter that
//!   doesn't say is never taken to confirm it: the relation is skipped. dbt-databricks
//!   sets `is_delta` from the catalog's `data_source_format` when it lists a Unity
//!   Catalog schema; relations it lists without a format (e.g. `hive_metastore`) say
//!   false, so they are skipped;
//! - runs each statement with `run_query`, `{relation}` replaced by the relation as
//!   the adapter renders it, and keeps the requested columns of its first row.
//!
//! Jinja can't catch an error, so one statement that fails fails the whole call: the
//! caller then treats every source as unknown.
//!
//! The answer is one row of JSON, under a marker. Its strings are percent-encoded by
//! the query (`urlencode`), so a value with a quote or backslash can't break the SQL
//! string literal that carries it, whatever the adapter's escaping rules.

use std::collections::BTreeMap;

use ods_sdk::contracts::probe::{PLACEHOLDER, ProbeRequest, ProbeRow};
use percent_encoding::percent_decode_str;

/// Marks the row as ODS's answer, not something else dbt printed.
const MARKER: &str = "ods_relation_probe";

/// The query, with `@KINDS@`, `@FORMAT@` and `@STATEMENTS@` to fill in. Relation
/// quoting is left to the adapter: `rel|string` renders the name by its own rules. `execute` is
/// false while dbt parses it, when there is no connection to ask.
const TEMPLATE: &str = "\
{%- set out = {} -%}{%- set ns = namespace(probed=0) -%}\
{%- set qs = @STATEMENTS@ -%}\
{%- if execute -%}\
{%- for s in graph.sources.values() -%}\
{%- set ns.probed = ns.probed + 1 -%}{%- set k = s.unique_id|urlencode -%}\
{%- set rel = adapter.get_relation(s.database, s.schema, s.identifier) -%}\
{%- if rel is none -%}{%- do out.update({k: {'missing': 1}}) -%}\
{%- elif rel.type not in @KINDS@ -%}{%- do out.update({k: {'kind': (rel.type or '')|string|urlencode}}) -%}\
@FORMAT@\
{%- else -%}{%- set rows = [] -%}\
{%- for q, cols in qs -%}{%- set r = run_query(q.replace('@PLACEHOLDER@', rel|string)) -%}{%- set row = {} -%}\
{%- if r.rows|length > 0 -%}\
{%- for c in cols if c in r.column_names and r.rows[0][c] is not none -%}\
{%- do row.update({c: r.rows[0][c]|string|urlencode}) -%}\
{%- endfor -%}{%- endif -%}{%- do rows.append(row) -%}\
{%- endfor -%}{%- do out.update({k: {'rows': rows}}) -%}\
{%- endif -%}\
{%- endfor -%}\
{%- endif -%}\
select '{{ tojson({\"ods_relation_probe\": 1, \"probed\": ns.probed, \"sources\": out}) }}' \
as ods_probe";

/// The inline query for `request`.
pub(crate) fn query(request: &ProbeRequest) -> String {
    // JSON arrays of strings are Jinja list literals. Kinds, formats and columns are
    // plain identifiers, and templates have no braces but the placeholder (the SDK
    // checks both), so nothing here can close a Jinja block.
    let kinds = serde_json::to_string(request.filter().relation_kinds()).unwrap_or_default();
    let statements: Vec<(&str, &[String])> = request
        .statements()
        .iter()
        .map(|s| (s.template(), s.columns()))
        .collect();
    let statements = serde_json::to_string(&statements).unwrap_or_default();
    let format = request.filter().format().map_or_else(String::new, |f| {
        format!(
            "{{%- elif not (rel.is_{f} is sameas true) -%}}{{%- do out.update({{k: {{'format': 0}}}}) -%}}"
        )
    });
    // The statements go in last, so nothing in them is taken for a marker.
    TEMPLATE
        .replace("@KINDS@", &kinds)
        .replace("@FORMAT@", &format)
        .replace("@PLACEHOLDER@", PLACEHOLDER)
        .replace("@STATEMENTS@", &statements)
}

/// What the query found about one source's relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Found {
    /// The adapter has no such relation.
    Missing,
    /// A relation of another kind, as the adapter names it.
    Kind(String),
    /// The adapter didn't confirm the filter's format.
    Format,
    /// The first row of each statement.
    Rows(Vec<ProbeRow>),
}

/// The query's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Probed {
    /// How many sources it looked at.
    pub probed: usize,
    /// By source id.
    pub sources: BTreeMap<String, Found>,
}

fn decode(text: &str) -> Result<String, String> {
    percent_decode_str(text)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|e| format!("an undecodable value in the probe: {e}"))
}

fn found(answer: &serde_json::Value) -> Result<Found, String> {
    if answer.get("missing").is_some() {
        return Ok(Found::Missing);
    }
    if let Some(kind) = answer.get("kind") {
        return kind
            .as_str()
            .ok_or_else(|| "a relation kind isn't text".to_owned())
            .and_then(decode)
            .map(Found::Kind);
    }
    if answer.get("format").is_some() {
        return Ok(Found::Format);
    }
    let rows = answer
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .ok_or("an answer is neither rows, a kind, a format nor missing")?;
    rows.iter()
        .map(|row| {
            row.as_object()
                .ok_or_else(|| "a row isn't an object".to_owned())?
                .iter()
                .map(|(column, value)| {
                    let value = value.as_str().ok_or("a value isn't text")?;
                    Ok((decode(column)?, decode(value)?))
                })
                .collect::<Result<ProbeRow, String>>()
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Found::Rows)
}

/// Reads the query's answer from what `dbt show --output json --log-format json`
/// printed: the one `ShowNode` event, whose preview is the result as JSON rows.
pub(crate) fn parse(stdout: &str) -> Result<Probed, String> {
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
        .pointer("/0/ods_probe")
        .and_then(|a| a.as_str())
        .ok_or("the query result has no `ods_probe` column")?;
    let answer: serde_json::Value =
        serde_json::from_str(answer).map_err(|e| format!("unreadable probe: {e}"))?;
    if answer.get(MARKER).and_then(serde_json::Value::as_u64) != Some(1) {
        return Err("the query result isn't ODS's relation probe".to_owned());
    }
    let probed = answer
        .get("probed")
        .and_then(serde_json::Value::as_u64)
        .and_then(|c| usize::try_from(c).ok())
        .ok_or("the probe has no count")?;
    let sources = answer
        .get("sources")
        .and_then(serde_json::Value::as_object)
        .ok_or("the probe has no answers")?
        .iter()
        .map(|(id, answer)| Ok((decode(id)?, found(answer)?)))
        .collect::<Result<_, String>>()?;
    Ok(Probed { probed, sources })
}

#[cfg(test)]
mod tests {
    use ods_sdk::contracts::probe::{ProbeFilter, ProbeStatement};

    use super::*;

    fn request(format: Option<&str>) -> ProbeRequest {
        let filter = ProbeFilter::kinds(["table", "view"]).unwrap();
        let filter = match format {
            Some(f) => filter.with_format(f).unwrap(),
            None => filter,
        };
        ProbeRequest::new(
            filter,
            vec![
                ProbeStatement::new("DESCRIBE DETAIL {relation}", ["id", "format"]).unwrap(),
                ProbeStatement::new("select \"v\" from {relation} where x = 'a\\b'", ["v"])
                    .unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    fn the_query_carries_the_request_and_names_no_source() {
        let query = query(&request(Some("columnar")));
        assert!(query.contains(MARKER));
        assert!(query.contains("graph.sources.values()"));
        assert!(
            query.contains(r#"rel.type not in ["table","view"]"#),
            "{query}"
        );
        assert!(query.contains("rel.is_columnar is sameas true"), "{query}");
        // Templates are JSON string literals, which Jinja reads the same way.
        assert!(
            query.contains(r#"["DESCRIBE DETAIL {relation}",["id","format"]]"#),
            "{query}"
        );
        assert!(
            query.contains(r#"["select \"v\" from {relation} where x = 'a\\b'",["v"]]"#),
            "{query}"
        );
        assert!(
            query.contains("q.replace('{relation}', rel|string)"),
            "{query}"
        );
        assert!(!query.contains('@'), "every marker is filled: {query}");
        // Command-line arguments are limited in size (less on Windows).
        assert!(query.len() < 2048, "{}", query.len());
    }

    #[test]
    fn without_a_format_nothing_is_confirmed() {
        let query = query(&request(None));
        assert!(!query.contains("is sameas true"), "{query}");
        assert!(!query.contains("'format'"), "{query}");
    }

    /// As dbt prints it: every string percent-encoded by the query.
    fn shown(answer: &serde_json::Value) -> String {
        let preview = serde_json::json!([{ "ods_probe": answer.to_string() }]).to_string();
        serde_json::json!({
            "data": {"preview": preview},
            "info": {"name": "ShowNode", "code": "Q041"},
        })
        .to_string()
    }

    #[test]
    fn reads_every_kind_of_answer() {
        let answer = serde_json::json!({
            "ods_relation_probe": 1,
            "probed": 4,
            "sources": {
                "source.p.raw.a": {"rows": [{"id": "a%2F1", "format": "delta"}, {"v": "it%27s%5C"}]},
                "source.p.raw.b": {"kind": "view"},
                "source.p.raw.c": {"format": 0},
                "source.p.raw.d": {"missing": 1},
            },
        });
        let probed = parse(&format!("not json\n{}\n", shown(&answer))).unwrap();
        assert_eq!(probed.probed, 4);
        assert_eq!(
            probed.sources["source.p.raw.a"],
            Found::Rows(vec![
                [("id", "a/1"), ("format", "delta")]
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .into(),
                [("v".to_owned(), "it's\\".to_owned())].into(),
            ])
        );
        assert_eq!(probed.sources["source.p.raw.b"], Found::Kind("view".into()));
        assert_eq!(probed.sources["source.p.raw.c"], Found::Format);
        assert_eq!(probed.sources["source.p.raw.d"], Found::Missing);
    }

    #[test]
    fn anything_else_is_an_error() {
        assert!(parse("").is_err());
        let error = r#"{"data": {}, "info": {"name": "MainEncounteredError"}}"#;
        assert!(parse(error).is_err());
        let other = serde_json::json!({"something_else": 1, "probed": 0, "sources": {}});
        assert!(parse(&shown(&other)).is_err());
        let odd = serde_json::json!({"ods_relation_probe": 1, "probed": 1, "sources": {"a": {"what": 1}}});
        assert!(parse(&shown(&odd)).is_err());
        let uncounted = serde_json::json!({"ods_relation_probe": 1, "sources": {}});
        assert!(parse(&shown(&uncounted)).is_err());
    }
}
