//! Which target dbt builds in, asked of dbt itself (#227).
//!
//! One `dbt compile --inline` renders [`QUERY`]: named, non-secret fields of dbt's
//! `target` (what `dbt debug` shows), so ODS never reads `profiles.yml` or a
//! credential (AGENTS.md rule 9), and `env_var()` in profiles resolves as dbt resolves
//! it. The location (host, account or file) can carry a credential (`user:pw@host`,
//! `md:db?motherduck_token=…`), so the query itself renders only a cleaned form for
//! people and a digest of the whole: the raw value is never rendered, so it reaches
//! neither ODS nor dbt's logs and compiled output.

use ods_core::state::TargetIdentity;

/// Marks the answer as ODS's, not something else dbt printed.
const MARKER: &str = "ods_target";

/// The inline query. `raw` is the first of the fields adapters use for where the
/// warehouse is; `shown` drops a query string, fragment, `;` options and anything up to
/// an `@`.
pub(crate) const QUERY: &str = "\
{%- set raw = (target.get(\"host\") or target.get(\"account\") or target.get(\"server\") \
or target.get(\"path\") or target.get(\"project\") or \"\") | string -%}\
{%- set shown = raw.split(\"?\")[0].split(\"#\")[0].split(\";\")[0] -%}\
{%- if \"@\" in shown -%}{%- set shown = shown.split(\"@\")[-1] -%}{%- endif -%}\
{{ tojson({\"ods_target\": 1, \
\"name\": target.get(\"name\"), \
\"profile\": target.get(\"profile_name\"), \
\"type\": target.get(\"type\"), \
\"location\": shown, \
\"location_digest\": (local_md5(raw) if raw else none), \
\"database\": target.get(\"database\") or target.get(\"catalog\") or target.get(\"dbname\")}) }}";

/// The same cleaning as [`QUERY`]'s, again, in case a dbt renders it differently:
/// what is shown and stored never carries a user, query string or options.
fn shown(location: &str) -> String {
    let cut = location.split(['?', '#', ';']).next().unwrap_or_default();
    cut.rsplit('@').next().unwrap_or(cut).to_owned()
}

/// Reads [`QUERY`]'s answer from what `dbt compile --output json --log-format json`
/// printed: the `CompiledNode` event, whose `compiled` is the rendered query.
pub(crate) fn parse(stdout: &str) -> Result<TargetIdentity, String> {
    let compiled = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event.pointer("/info/name").and_then(|n| n.as_str()) == Some("CompiledNode"))
        .and_then(|event| {
            event
                .pointer("/data/compiled")
                .and_then(|c| c.as_str())
                .map(str::to_owned)
        })
        .ok_or("dbt printed no compiled query")?;
    let answer: serde_json::Value =
        serde_json::from_str(compiled.trim()).map_err(|e| format!("unreadable target: {e}"))?;
    if answer.get(MARKER).and_then(serde_json::Value::as_u64) != Some(1) {
        return Err("the compiled query isn't ODS's target check".to_owned());
    }
    let field = |key: &str| {
        answer
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let name = field("name").ok_or("dbt's target has no name")?;
    Ok(TargetIdentity::new(name)
        .profile(field("profile"))
        .kind(field("type"))
        .location(
            field("location")
                .map(|l| shown(&l))
                .filter(|l| !l.is_empty()),
        )
        .location_digest(field("location_digest"))
        .database(field("database")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As dbt 1.10 prints it (trimmed), for the jaffle-ods `DuckDB` profile.
    const COMPILED: &str = r#"{"data": {"compiled": "{\"ods_target\": 1, \"name\": \"dev\", \"profile\": \"jaffle_ods\", \"type\": \"duckdb\", \"location\": \"target/jaffle_ods.duckdb\", \"location_digest\": \"d04e57c88f5f224f6f03b151597e38e3\", \"database\": \"jaffle_ods\"}", "is_inline": true}, "info": {"code": "Q042", "name": "CompiledNode"}}"#;

    #[test]
    fn reads_the_compiled_node_event() {
        let target = parse(&format!("noise\n{COMPILED}\n")).unwrap();
        assert_eq!(
            target,
            TargetIdentity::new("dev")
                .profile(Some("jaffle_ods".into()))
                .kind(Some("duckdb".into()))
                .location(Some("target/jaffle_ods.duckdb".into()))
                .location_digest(Some("d04e57c88f5f224f6f03b151597e38e3".into()))
                .database(Some("jaffle_ods".into()))
        );
        assert!(parse("").is_err());
        assert!(parse(&COMPILED.replace("ods_target", "other")).is_err());
    }

    #[test]
    fn credentials_never_reach_the_location() {
        for (given, kept) in [
            ("db.example.com", "db.example.com"),
            ("user:secret@db.example.com", "db.example.com"),
            (
                "postgres://user:secret@db.example.com:5432/analytics",
                "db.example.com:5432/analytics",
            ),
            ("postgresql://h/db?password=secret", "postgresql://h/db"),
            ("md:my_db?motherduck_token=secret", "md:my_db"),
            ("host;Password=secret", "host"),
            (
                "https://adb-123.azuredatabricks.net#x",
                "https://adb-123.azuredatabricks.net",
            ),
            ("/data/warehouse.duckdb", "/data/warehouse.duckdb"),
        ] {
            assert_eq!(shown(given), kept, "{given}");
        }
    }

    #[test]
    fn the_query_never_renders_the_raw_location() {
        for secret in ["password", "token", "private_key", "keyfile", "user"] {
            assert!(!QUERY.contains(secret), "{secret}");
        }
        // `raw` is only ever hashed.
        assert_eq!(QUERY.matches("raw").count(), 4, "{QUERY}");
        assert!(QUERY.contains("local_md5(raw)"));
    }
}
