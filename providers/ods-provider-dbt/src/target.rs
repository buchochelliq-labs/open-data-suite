//! Which target dbt builds in, asked of dbt itself (#227).
//!
//! One `dbt compile --inline` renders [`QUERY`]: the non-secret fields of dbt's
//! `target` (what `dbt debug` shows), so ODS never reads `profiles.yml` or a
//! credential (AGENTS.md rule 9), and `env_var()` in profiles resolves as dbt resolves
//! it. Only named fields are rendered, never the whole `target`.

use ods_core::state::TargetIdentity;

/// Marks the answer as ODS's, not something else dbt printed.
const MARKER: &str = "ods_target";

/// The inline query. `location` is the first of the fields adapters use for where
/// the warehouse is; `database` likewise for where builds go.
pub(crate) const QUERY: &str = "{{ tojson({\"ods_target\": 1, \
\"name\": target.get(\"name\"), \
\"profile\": target.get(\"profile_name\"), \
\"type\": target.get(\"type\"), \
\"location\": target.get(\"host\") or target.get(\"account\") or target.get(\"server\") \
or target.get(\"path\") or target.get(\"project\"), \
\"database\": target.get(\"database\") or target.get(\"catalog\") or target.get(\"dbname\")}) }}";

/// Drops credentials a location might carry, e.g. `user:secret@host` or
/// `postgres://user:secret@host/db`: only what follows the last `@` is kept.
fn without_userinfo(location: &str) -> String {
    let (scheme, rest) = location
        .split_once("://")
        .map_or(("", location), |(s, r)| (s, r));
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let mut clean = String::new();
    if !scheme.is_empty() {
        clean.push_str(scheme);
        clean.push_str("://");
    }
    clean.push_str(host);
    if rest.contains('/') {
        clean.push('/');
        clean.push_str(path);
    }
    clean
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
        .location(field("location").map(|l| without_userinfo(&l)))
        .database(field("database")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As dbt 1.10 prints it (trimmed), for the jaffle-ods `DuckDB` profile.
    const COMPILED: &str = r#"{"data": {"compiled": "{\"ods_target\": 1, \"name\": \"dev\", \"profile\": \"jaffle_ods\", \"type\": \"duckdb\", \"location\": \"target/jaffle_ods.duckdb\", \"database\": \"jaffle_ods\"}", "is_inline": true}, "info": {"code": "Q042", "name": "CompiledNode"}}"#;

    #[test]
    fn reads_the_compiled_node_event() {
        let target = parse(&format!("noise\n{COMPILED}\n")).unwrap();
        assert_eq!(
            target,
            TargetIdentity::new("dev")
                .profile(Some("jaffle_ods".into()))
                .kind(Some("duckdb".into()))
                .location(Some("target/jaffle_ods.duckdb".into()))
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
                "postgres://db.example.com:5432/analytics",
            ),
            (
                "https://adb-123.azuredatabricks.net",
                "https://adb-123.azuredatabricks.net",
            ),
            ("/data/warehouse.duckdb", "/data/warehouse.duckdb"),
        ] {
            assert_eq!(without_userinfo(given), kept, "{given}");
        }
    }

    #[test]
    fn the_query_names_only_non_secret_fields() {
        for secret in ["password", "token", "private_key", "keyfile", "user"] {
            assert!(!QUERY.contains(secret), "{secret}");
        }
    }
}
