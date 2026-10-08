//! What the login a probe runs under may do in Unity Catalog (#392, ADR-0030 §4c).
//!
//! [`UnityCatalog`] wraps any [`RelationProbe`] (e.g. the dbt executor's): it probes
//! through it unchanged, and answers [`RelationPrivileges`] with by-name statements
//! through the same probe, so the login checked is the login the probes run under. For
//! each relation it reads `system.information_schema`, one summary row per statement:
//! - that the catalog shows the relation to the login at all (and the login's name);
//! - whether the login, or a group it is in (`is_account_group_member`, `is_member`),
//!   owns the table, its schema or its catalog;
//! - any privilege it holds on the table, its schema, its catalog or the metastore
//!   beyond reading (`SELECT`, `BROWSE`, `USE CATALOG`, `USE SCHEMA`; on the metastore,
//!   only the default `USE MARKETPLACE ASSETS`), in either spelling (`USE_SCHEMA` is how
//!   `information_schema` writes it). The right to grant
//!   comes with ownership or `MANAGE`, which these find; `is_grantable` is checked too,
//!   though Unity Catalog reserves it;
//! - whether it owns the metastore (its admin), or is in the workspace's `admins` group;
//! - that the relation is a managed table, a view or a materialized view: an external
//!   table's files can be written through its storage location, which isn't checked.
//!
//! The relation is read-only for the login only when every statement answered and none
//! found anything. Whatever can't be read is unknown, never read-only (AGENTS rule 3).
//!
//! # Known limits (ADR-0030 §4c, "Known issues")
//! - An account admin isn't visible in `information_schema`, so such a login can look
//!   read-only. Run probes as a dedicated principal that isn't one.
//! - Relations outside Unity Catalog (e.g. `hive_metastore`) aren't in
//!   `system.information_schema`: unknown.
//! - A name with a quote, backslash or brace isn't put in a string literal: unknown.

use std::time::Duration;

use async_trait::async_trait;
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::privileges::{Access, PrivilegeReport, RelationPrivileges};
use ods_sdk::contracts::probe::{
    InvalidProbe, ProbeAnswer, ProbeFilter, ProbeReport, ProbeRequest, ProbeRow, ProbeStatement,
    ProbeTarget, RelationProbe,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;

/// How long reading the privileges may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The login, or a group it is in, is `{0}`.
const IS_LOGIN: &str = "({0} = current_user() or is_account_group_member({0}) or is_member({0}))";

/// The relation, matched by its parts, whatever their case.
const THE_TABLE: &str = "lower(table_catalog) = lower({database}) and lower(table_schema) = \
     lower({schema}) and lower(table_name) = lower({name})";

/// Each statement, what it finds, and what that is called when it finds something.
struct Check {
    sql: String,
    /// What the login holds when the statement counts something, e.g. `owner of the
    /// table`; for grants, `on the table`, after the privileges it lists.
    what: &'static str,
    /// Whether the statement lists privileges in `what`.
    grants: bool,
}

fn checks() -> Vec<Check> {
    let login = |column: &str| IS_LOGIN.replace("{0}", column);
    let owner = |sql: String, what| Check {
        sql,
        what,
        grants: false,
    };
    let grants = |sql: String, what| Check {
        sql,
        what,
        grants: true,
    };
    // `information_schema` spells privileges with underscores (`USE_SCHEMA`) where
    // `GRANT` takes spaces (`USE SCHEMA`); compare in the `GRANT` spelling so either reads.
    let beyond = |allowed: &str| {
        format!(
            "(replace(upper(privilege_type), '_', ' ') not in ({allowed}) or is_grantable = 'YES')"
        )
    };
    vec![
        owner(
            format!(
                "select count(*) as n from system.information_schema.tables where {THE_TABLE} and {}",
                login("table_owner")
            ),
            "owner of the table",
        ),
        owner(
            format!(
                "select count(*) as n from system.information_schema.schemata where \
                 lower(catalog_name) = lower({{database}}) and lower(schema_name) = lower({{schema}}) and {}",
                login("schema_owner")
            ),
            "owner of the schema",
        ),
        owner(
            format!(
                "select count(*) as n from system.information_schema.catalogs where \
                 lower(catalog_name) = lower({{database}}) and {}",
                login("catalog_owner")
            ),
            "owner of the catalog",
        ),
        grants(
            format!(
                "select count(*) as n, concat_ws(', ', sort_array(collect_set(privilege_type))) as what \
                 from system.information_schema.table_privileges where {THE_TABLE} and {} and {}",
                login("grantee"),
                beyond("'SELECT', 'BROWSE'")
            ),
            "on the table",
        ),
        grants(
            format!(
                "select count(*) as n, concat_ws(', ', sort_array(collect_set(privilege_type))) as what \
                 from system.information_schema.schema_privileges where lower(catalog_name) = \
                 lower({{database}}) and lower(schema_name) = lower({{schema}}) and {} and {}",
                login("grantee"),
                beyond("'SELECT', 'BROWSE', 'USE SCHEMA'")
            ),
            "on the schema",
        ),
        grants(
            format!(
                "select count(*) as n, concat_ws(', ', sort_array(collect_set(privilege_type))) as what \
                 from system.information_schema.catalog_privileges where lower(catalog_name) = \
                 lower({{database}}) and {} and {}",
                login("grantee"),
                beyond("'SELECT', 'BROWSE', 'USE CATALOG', 'USE SCHEMA'")
            ),
            "on the catalog",
        ),
        grants(
            format!(
                "select count(*) as n, concat_ws(', ', sort_array(collect_set(privilege_type))) as what \
                 from system.information_schema.metastore_privileges where {{name}} is not null and {} and {}",
                login("grantee"),
                // Granted to `account users` on every metastore by default; it lets a
                // login see Marketplace listings, not change any relation.
                beyond("'USE MARKETPLACE ASSETS'")
            ),
            "on the metastore",
        ),
        owner(
            format!(
                "select count(*) as n from system.information_schema.metastores where \
                 {{name}} is not null and {}",
                login("metastore_owner")
            ),
            "owner (admin) of the metastore",
        ),
        owner(
            "select cast(is_member('admins') as int) as n, {name} as relation_name".to_owned(),
            "a workspace admin (in `admins`)",
        ),
    ]
}

/// The statement that says whether the catalog shows the relation, and to whom.
fn seen() -> String {
    format!(
        "select current_user() as login, count(*) as found, max(table_type) as kind \
         from system.information_schema.tables where {THE_TABLE}"
    )
}

/// The kinds of relation whose data only Unity Catalog's grants govern. An external
/// table's files can be written through its storage location, which isn't checked.
const GOVERNED: [&str; 3] = ["MANAGED", "VIEW", "MATERIALIZED_VIEW"];

/// The request that reads a relation's privileges: [`seen`], then each of [`checks`].
fn request() -> Result<ProbeRequest, InvalidProbe> {
    let mut statements = vec![ProbeStatement::by_name(seen(), ["login", "found", "kind"])?];
    for check in checks() {
        statements.push(if check.grants {
            ProbeStatement::by_name(check.sql, ["n", "what"])?
        } else {
            ProbeStatement::by_name(check.sql, ["n"])?
        });
    }
    Ok(ProbeRequest::new(
        ProbeFilter::kinds(["table", "view", "materialized_view"])?,
        statements,
    )?
    .with_timeout(TIMEOUT))
}

/// A count a statement returned.
fn count(row: &ProbeRow, column: &str) -> Option<u64> {
    row.get(column)?.trim().parse().ok()
}

/// What the rows say the login may do; and its name, when shown.
fn access(rows: &[ProbeRow]) -> (Option<String>, Access) {
    let checks = checks();
    let unknown = |why: &str| Access::Unknown(why.to_owned());
    let Some((first, rest)) = rows.split_first() else {
        return (None, unknown("Unity Catalog returned nothing"));
    };
    let login = first.get("login").filter(|l| !l.trim().is_empty()).cloned();
    match count(first, "found") {
        Some(0) => {
            return (
                login,
                unknown(
                    "system.information_schema doesn't show this relation to the login (it may be outside Unity Catalog)",
                ),
            );
        }
        Some(_) => {}
        None => {
            return (
                login,
                unknown("Unity Catalog didn't say whether it shows the relation"),
            );
        }
    }
    match first.get("kind").map(|k| k.trim().to_ascii_uppercase()) {
        Some(kind) if GOVERNED.contains(&kind.as_str()) => {}
        Some(kind) => {
            return (
                login,
                Access::Unknown(format!(
                    "a {} table: who can write its files through its storage location isn't checked",
                    kind.to_ascii_lowercase()
                )),
            );
        }
        None => {
            return (
                login,
                unknown("Unity Catalog didn't say what kind of relation it is"),
            );
        }
    }
    if rest.len() != checks.len() {
        return (
            login,
            unknown("Unity Catalog didn't answer every privilege check"),
        );
    }
    let mut beyond = Vec::new();
    for (check, row) in checks.iter().zip(rest) {
        let Some(n) = count(row, "n") else {
            return (
                login,
                Access::Unknown(format!(
                    "Unity Catalog didn't say whether the login is {}",
                    check.what
                )),
            );
        };
        if n == 0 {
            continue;
        }
        if check.grants {
            let what = row.get("what").map_or("privileges", String::as_str);
            beyond.push(format!("{what} {}", check.what));
        } else {
            beyond.push(check.what.to_owned());
        }
    }
    let access = if beyond.is_empty() {
        Access::ReadOnly
    } else {
        Access::Elevated(beyond)
    };
    (login, access)
}

/// A [`RelationProbe`] that also reports, from Unity Catalog, what its login may do on
/// each relation.
#[derive(Debug, Clone)]
pub struct UnityCatalog<P> {
    probe: P,
}

impl<P: RelationProbe> UnityCatalog<P> {
    /// Probes, and reads privileges, through `probe`.
    pub fn new(probe: P) -> Self {
        Self { probe }
    }
}

impl<P: RelationProbe> Provider for UnityCatalog<P> {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "unity_catalog",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::RelationProbe, Capability::RelationPrivileges]),
        )
    }
}

#[async_trait]
impl<P: RelationProbe> RelationProbe for UnityCatalog<P> {
    async fn probe(
        &self,
        request: &ProbeRequest,
        targets: &[ProbeTarget],
    ) -> Result<ProbeReport, ProviderError> {
        self.probe.probe(request, targets).await
    }
}

#[async_trait]
impl<P: RelationProbe> RelationPrivileges for UnityCatalog<P> {
    async fn privileges(&self, targets: &[ProbeTarget]) -> Result<PrivilegeReport, ProviderError> {
        let request = request()
            .map_err(|e| ProviderError::Other(format!("the Unity Catalog privilege check: {e}")))?;
        let report = self.probe.probe(&request, targets).await?;
        let mut login = None;
        let answers = targets
            .iter()
            .map(|target| {
                let answers: Vec<&ProbeAnswer> = report
                    .targets
                    .iter()
                    .filter(|(id, _)| *id == target.id)
                    .map(|(_, answer)| answer)
                    .collect();
                let access = match answers.as_slice() {
                    [ProbeAnswer::Rows(rows)] => {
                        let (seen, access) = access(rows);
                        if login.is_none() {
                            login = seen;
                        }
                        access
                    }
                    [ProbeAnswer::Skipped(why) | ProbeAnswer::Unknown(why)] => {
                        Access::Unknown(why.clone())
                    }
                    [] => Access::Unknown("the probe didn't report on it".to_owned()),
                    [_, _, ..] => Access::Unknown("the probe answered about it twice".to_owned()),
                    _ => Access::Unknown("the probe's answer can't be read".to_owned()),
                };
                (target.id.clone(), access)
            })
            .collect();
        Ok(PrivilegeReport::new(login, answers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, &str)]) -> ProbeRow {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// The rows of a relation the login can only read, with `change` applied.
    fn rows(change: impl FnOnce(&mut Vec<ProbeRow>)) -> Vec<ProbeRow> {
        let mut rows = vec![row(&[
            ("login", "reader@example.com"),
            ("found", "1"),
            ("kind", "MANAGED"),
        ])];
        for check in checks() {
            rows.push(if check.grants {
                row(&[("n", "0"), ("what", "")])
            } else {
                row(&[("n", "0")])
            });
        }
        change(&mut rows);
        rows
    }

    #[test]
    fn every_statement_is_by_name_and_read_only_in_shape() {
        let request = request().unwrap();
        assert_eq!(request.statements().len(), checks().len() + 1);
        for statement in request.statements() {
            assert!(statement.is_by_name());
            assert!(
                statement.template().starts_with("select "),
                "{}",
                statement.template()
            );
            assert!(
                ods_sdk::contracts::probe::NAME_PARTS
                    .iter()
                    .any(|p| statement.template().contains(p)),
                "{}",
                statement.template()
            );
        }
        assert_eq!(request.timeout(), Some(TIMEOUT));
    }

    #[test]
    fn grants_match_either_spelling_and_allow_the_default_metastore_grant() {
        // A live run (#408) found `information_schema` writes `USE_SCHEMA`, which a
        // spaced allowlist never matched, and `USE_MARKETPLACE_ASSETS` on the metastore.
        let grants: Vec<_> = checks().into_iter().filter(|c| c.grants).collect();
        assert_eq!(grants.len(), 4);
        for check in &grants {
            assert!(
                check
                    .sql
                    .contains("replace(upper(privilege_type), '_', ' ') not in ("),
                "{}",
                check.sql
            );
            assert!(!check.sql.contains("'USE_"), "{}", check.sql);
        }
        let metastore = grants
            .iter()
            .find(|c| c.what == "on the metastore")
            .unwrap();
        assert!(metastore.sql.contains("not in ('USE MARKETPLACE ASSETS')"));
    }

    #[test]
    fn nothing_found_is_read_only_and_anything_found_is_named() {
        assert_eq!(
            access(&rows(|_| {})),
            (Some("reader@example.com".to_owned()), Access::ReadOnly)
        );
        let (_, found) = access(&rows(|r| {
            r[2] = row(&[("n", "1")]);
            r[4] = row(&[("n", "2"), ("what", "MODIFY, SELECT")]);
            r[9] = row(&[("n", "1")]);
        }));
        assert_eq!(
            found,
            Access::Elevated(vec![
                "owner of the schema".to_owned(),
                "MODIFY, SELECT on the table".to_owned(),
                "a workspace admin (in `admins`)".to_owned(),
            ])
        );
    }

    #[test]
    fn what_cant_be_read_is_unknown_never_read_only() {
        for change in [
            Box::new(|r: &mut Vec<ProbeRow>| {
                r[0] = row(&[("login", "x"), ("found", "0"), ("kind", "MANAGED")]);
            }) as Box<dyn FnOnce(&mut Vec<ProbeRow>)>,
            Box::new(|r: &mut Vec<ProbeRow>| r[0] = row(&[("login", "x"), ("kind", "MANAGED")])),
            Box::new(|r: &mut Vec<ProbeRow>| {
                r[0] = row(&[("login", "x"), ("found", "1"), ("kind", "EXTERNAL")]);
            }),
            Box::new(|r: &mut Vec<ProbeRow>| r[0] = row(&[("login", "x"), ("found", "1")])),
            Box::new(|r: &mut Vec<ProbeRow>| r[3] = row(&[("n", "many")])),
            Box::new(|r: &mut Vec<ProbeRow>| {
                r.pop();
            }),
            Box::new(|r: &mut Vec<ProbeRow>| r.clear()),
        ] {
            let (_, found) = access(&rows(change));
            assert!(matches!(found, Access::Unknown(_)), "{found:?}");
        }
    }

    /// The privilege checks pass their conformance suite over a fake warehouse.
    mod conformance {
        use std::sync::Arc;

        use async_trait::async_trait;
        use ods_provider_fake::FakeRelationProbe;
        use ods_sdk::conformance::privileges::{PrivilegesHarness, run};

        use super::*;

        /// A warehouse where `model.p.read` and `model.p.also` can only be read and the
        /// login owns `model.p.owned`'s schema.
        fn warehouse() -> FakeRelationProbe {
            let request = request().unwrap();
            let mut fake = FakeRelationProbe::new();
            for (id, owns_schema) in [
                ("model.p.read", false),
                ("model.p.also", false),
                ("model.p.owned", true),
            ] {
                fake = fake.with_relation(id, "table", None);
                for (i, statement) in request.statements().iter().enumerate() {
                    let values: Vec<(&str, &str)> = match i {
                        0 => vec![
                            ("login", "reader@example.com"),
                            ("found", "1"),
                            ("kind", "MANAGED"),
                        ],
                        2 if owns_schema => vec![("n", "1")],
                        _ => vec![("n", "0"), ("what", "")],
                    };
                    fake = fake.with_row(id, statement.template(), values);
                }
            }
            fake
        }

        struct Harness;

        #[async_trait]
        impl PrivilegesHarness for Harness {
            async fn provider(&self) -> Arc<dyn RelationPrivileges> {
                Arc::new(UnityCatalog::new(warehouse()))
            }

            fn read_only(&self) -> Vec<ProbeTarget> {
                vec![
                    ProbeTarget::new("model.p.read", "read"),
                    ProbeTarget::new("model.p.also", "also"),
                ]
            }

            fn elevated(&self) -> Option<ProbeTarget> {
                Some(ProbeTarget::new("model.p.owned", "owned"))
            }
        }

        #[tokio::test]
        async fn conforms() {
            let report = run(&Harness).await;
            assert!(report.skipped.is_empty(), "{report:?}");
            assert_eq!(report.passed.len(), 4, "{report:?}");
        }

        #[tokio::test]
        async fn it_names_the_login_and_probes_through_unchanged() {
            let uc = UnityCatalog::new(warehouse());
            let report = uc
                .privileges(&[ProbeTarget::new("model.p.read", "read")])
                .await
                .unwrap();
            assert_eq!(report.login.as_deref(), Some("reader@example.com"));
            let unreachable = UnityCatalog::new(warehouse().failing());
            assert!(
                unreachable
                    .privileges(&[ProbeTarget::new("model.p.read", "read")])
                    .await
                    .is_err()
            );
            assert!(
                uc.info()
                    .capabilities
                    .contains(&Capability::RelationPrivileges)
            );
        }
    }
}
