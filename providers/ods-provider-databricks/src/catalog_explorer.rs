//! Links to a relation's page in Catalog Explorer, the workspace's UI for Unity Catalog
//! (#329), as a [`RelationLinker`].
//!
//! The page of table `c.s.t` is `https://<workspace-host>/explore/data/c/s/t`. That
//! shape is the one Databricks' own documentation uses for links to a table, e.g. the
//! `databricksWorkspaceUrl` of access-request notifications
//! (<https://learn.microsoft.com/azure/databricks/data-governance/unity-catalog/manage-privileges/access-request-destinations#access-request-examples>).
//! Those links also carry the workspace id as `?o=<id>`, which selects the workspace
//! when one host serves several. It is added when it is known: configured
//! (`workspace_id`), or read from a host that names it (Azure's
//! `adb-<id>.<n>.azuredatabricks.net`, GCP's `<id>.<n>.gcp.databricks.com`). A host
//! that doesn't (AWS's `dbc-…`) gets no `?o=` rather than a guessed one.
//!
//! The host and the workspace id come from configuration (`host`, ADR-0021); neither is
//! a secret, and no token ever goes into a link. Nothing is fetched: the link is where
//! the manifest says the relation is, not proof that it exists.

use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::relation_link::{
    NoRelationLink, RelationLink, RelationLinker, path_segment, split_relation,
};
use ods_sdk::{Provider, ProviderInfo};

use crate::KIND;

/// What the link is called.
pub const LABEL: &str = "Open in Catalog Explorer";

/// Links relations to Catalog Explorer in one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogExplorer {
    instance: String,
    /// `https://<host>`, or why there is none.
    base: Result<String, NoRelationLink>,
    /// The workspace id for `?o=`, if known, or why the configured one can't be used.
    workspace: Result<Option<String>, NoRelationLink>,
}

impl CatalogExplorer {
    /// Links into the workspace at `host`, as configured for provider `instance`.
    /// `host` may be `https://<host>` or a bare host name, with or without a trailing
    /// `/`; anything else (another scheme, a path, a query string, a user part) is
    /// refused, and every link then says why.
    pub fn new(instance: impl Into<String>, host: Option<&str>) -> Self {
        let base = base(host);
        let workspace = Ok(base.as_deref().ok().and_then(workspace_in_host));
        Self {
            instance: instance.into(),
            base,
            workspace,
        }
    }

    /// The same, with the workspace id configured as `workspace_id`: decimal digits,
    /// without a leading zero. `None` (not configured) keeps the id the host names, if
    /// any. A configured id that isn't one, or that differs from the id the host names,
    /// is refused: links then say why rather than pick one.
    #[must_use]
    pub fn with_workspace_id(mut self, workspace_id: Option<&str>) -> Self {
        let Some(id) = workspace_id else {
            return self;
        };
        let named = self.workspace.as_ref().ok().cloned().flatten();
        self.workspace = checked_workspace_id(id.trim()).and_then(|id| match named {
            Some(named) if named != id => Err(NoRelationLink::InvalidSetting {
                setting: "workspace_id".to_owned(),
                why: "differs from the workspace id in `host`".to_owned(),
            }),
            _ => Ok(Some(id)),
        });
        self
    }

    /// The workspace id links carry as `?o=`, if known.
    ///
    /// # Errors
    /// Why the configured `workspace_id` can't be used.
    pub fn workspace_id(&self) -> Result<Option<&str>, &NoRelationLink> {
        self.workspace.as_ref().map(Option::as_deref)
    }

    /// `https://<host>`, as links are built from.
    ///
    /// # Errors
    /// Why `host` can't be used.
    pub fn base(&self) -> Result<&str, &NoRelationLink> {
        self.base.as_deref()
    }
}

fn invalid(why: &str) -> NoRelationLink {
    NoRelationLink::InvalidSetting {
        setting: "host".to_owned(),
        why: why.to_owned(),
    }
}

/// A workspace id: a positive whole number in decimal digits, as written (no leading
/// zero, so it is never rewritten).
fn checked_workspace_id(id: &str) -> Result<String, NoRelationLink> {
    let valid = !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && !id.starts_with('0')
        && id.parse::<u64>().is_ok();
    if valid {
        Ok(id.to_owned())
    } else {
        Err(NoRelationLink::InvalidSetting {
            setting: "workspace_id".to_owned(),
            why: "must be the workspace's numeric id".to_owned(),
        })
    }
}

/// The workspace id a host names: `adb-<id>.<n>.azuredatabricks.net` (Azure) or
/// `<id>.<n>.gcp.databricks.com` (GCP). Any other host names none.
fn workspace_in_host(base: &str) -> Option<String> {
    let authority = base.strip_prefix("https://")?;
    let name = authority.split(':').next()?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (first, rest) = name.split_once('.')?;
    let (second, domain) = rest.split_once('.')?;
    let id = match domain {
        "azuredatabricks.net" => first.strip_prefix("adb-")?,
        "gcp.databricks.com" => first,
        _ => return None,
    };
    (digits(id) && digits(second))
        .then(|| checked_workspace_id(id).ok())
        .flatten()
}

/// `https://<host>` from the configured `host`: HTTPS only, no trailing `/`, and no
/// path, query, fragment or user part.
fn base(host: Option<&str>) -> Result<String, NoRelationLink> {
    let host = host
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| NoRelationLink::NotConfigured {
            setting: "host".to_owned(),
        })?;
    let rest = match host.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        Some(_) => return Err(invalid("must be an https:// workspace URL")),
        None => host,
    };
    let authority = rest.trim_end_matches('/');
    if authority.contains(['/', '?', '#']) {
        return Err(invalid(
            "must be the workspace's URL alone, with no path or query string",
        ));
    }
    if authority.contains('@') {
        // Never echoed: the part before `@` may be a credential.
        return Err(invalid("must not carry a user name or password"));
    }
    let (name, port) = match authority.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (authority, None),
    };
    let valid_name = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && !name.starts_with(['.', '-'])
        && !name.ends_with(['.', '-']);
    let valid_port = port.is_none_or(|p| !p.is_empty() && p.parse::<u16>().is_ok());
    if !valid_name || !valid_port {
        return Err(invalid("isn't a host name"));
    }
    Ok(format!("https://{}", authority.to_ascii_lowercase()))
}

impl Provider for CatalogExplorer {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            self.instance.clone(),
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::RelationLink]),
        )
    }
}

impl RelationLinker for CatalogExplorer {
    fn link(&self, relation: &str) -> Result<RelationLink, NoRelationLink> {
        let base = self.base.as_ref().map_err(Clone::clone)?;
        let workspace = self.workspace.as_ref().map_err(Clone::clone)?;
        let relation = relation.trim();
        let invalid = |why: String| NoRelationLink::InvalidName {
            relation: relation.to_owned(),
            why,
        };
        // dbt renders Databricks relations as `` `c`.`s`.`t` ``, a backtick doubled
        // inside a name. A name that isn't well formed is refused, never repaired.
        let parts = split_relation(relation, '`').map_err(invalid)?;
        // Unity Catalog's three levels. Two parts would leave the catalog to a
        // default this can't see, so no link is guessed (AGENTS rule 3).
        if parts.len() != 3 {
            return Err(NoRelationLink::NotQualified {
                relation: relation.to_owned(),
                parts: parts.len(),
                needed: 3,
            });
        }
        let path = parts
            .iter()
            .map(|p| path_segment(p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid)?;
        let query = workspace
            .as_deref()
            .map_or_else(String::new, |id| format!("?o={id}"));
        Ok(RelationLink::new(
            format!("{base}/explore/data/{}{query}", path.join("/")),
            LABEL,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(host: Option<&str>, relation: &str) -> Result<String, NoRelationLink> {
        CatalogExplorer::new("uc", host)
            .link(relation)
            .map(|l| l.url)
    }

    #[test]
    fn links_a_three_part_relation() {
        assert_eq!(
            link(
                Some("https://dbc-1.cloud.databricks.com"),
                "`main`.`sales`.`orders`"
            ),
            Ok("https://dbc-1.cloud.databricks.com/explore/data/main/sales/orders".to_owned())
        );
        assert_eq!(
            link(Some("adb-1.2.azuredatabricks.net"), "main.sales.orders"),
            Ok("https://adb-1.2.azuredatabricks.net/explore/data/main/sales/orders?o=1".to_owned())
        );
        let explorer = CatalogExplorer::new("uc", Some("h.example"));
        assert_eq!(explorer.link("a.b.c").unwrap().label, LABEL);
        assert_eq!(explorer.info().instance, "uc");
    }

    #[test]
    fn carries_the_workspace_id_when_it_is_known() {
        let url = |host: &str, id: Option<&str>| {
            CatalogExplorer::new("uc", Some(host))
                .with_workspace_id(id)
                .link("c.s.t")
                .map(|l| l.url)
        };
        // Named by the host.
        assert_eq!(
            url("adb-1234567890123456.7.azuredatabricks.net", None),
            Ok("https://adb-1234567890123456.7.azuredatabricks.net/explore/data/c/s/t?o=1234567890123456".to_owned())
        );
        assert_eq!(
            url("https://1234567890.3.gcp.databricks.com", None),
            Ok(
                "https://1234567890.3.gcp.databricks.com/explore/data/c/s/t?o=1234567890"
                    .to_owned()
            )
        );
        // Not named by the host: no `?o=` is guessed.
        for host in [
            "dbc-1234.cloud.databricks.com",
            "adb-x1.7.azuredatabricks.net",
            "adb-1.azuredatabricks.net",
            "1234.gcp.databricks.com",
            "adb-1.2.azuredatabricks.net.example",
            "h.example",
        ] {
            let got = url(host, None).unwrap();
            assert!(!got.contains('?'), "{host}: {got}");
        }
        // Configured: used; with a host that names an id, it must be the same.
        assert_eq!(
            url("dbc-1234.cloud.databricks.com", Some(" 42 ")),
            Ok("https://dbc-1234.cloud.databricks.com/explore/data/c/s/t?o=42".to_owned())
        );
        assert_eq!(
            url("adb-9.2.azuredatabricks.net", Some("9")),
            Ok("https://adb-9.2.azuredatabricks.net/explore/data/c/s/t?o=9".to_owned())
        );
        let other = url("adb-1.2.azuredatabricks.net", Some("9"));
        assert!(
            matches!(&other, Err(NoRelationLink::InvalidSetting { why, .. }) if why.contains("host")),
            "{other:?}"
        );
        // A configured id that isn't one is refused, never dropped or repaired.
        for id in [
            "",
            "  ",
            "abc",
            "12a",
            "-1",
            "0",
            "0042",
            "1&x=2",
            "99999999999999999999999",
        ] {
            let got = url("h.example", Some(id));
            assert!(
                matches!(&got, Err(NoRelationLink::InvalidSetting { setting, .. }) if setting == "workspace_id"),
                "{id}: {got:?}"
            );
        }
    }

    #[test]
    fn normalises_the_host() {
        for host in [
            "https://H.example/",
            "https://h.example//",
            "HTTPS://h.example",
            " h.example ",
            "h.example/",
        ] {
            assert_eq!(
                base(Some(host)),
                Ok("https://h.example".to_owned()),
                "{host}"
            );
        }
        assert_eq!(
            base(Some("h.example:8443")),
            Ok("https://h.example:8443".to_owned())
        );
    }

    #[test]
    fn refuses_hosts_it_cant_use() {
        assert_eq!(
            base(None),
            Err(NoRelationLink::NotConfigured {
                setting: "host".into()
            })
        );
        assert!(matches!(
            base(Some("  ")),
            Err(NoRelationLink::NotConfigured { .. })
        ));
        for host in [
            "http://h.example",
            "ftp://h.example",
            "https://h.example/some/path",
            "https://h.example/?o=123",
            "https://h.example#x",
            "https://user:secret@h.example",
            "https://",
            "h example",
            "h.example:port",
            "-h.example",
        ] {
            let got = base(Some(host));
            assert!(
                matches!(got, Err(NoRelationLink::InvalidSetting { .. })),
                "{host}: {got:?}"
            );
            // The reason never repeats the value, which may hold a credential.
            assert!(!got.unwrap_err().to_string().contains("secret"), "{host}");
        }
    }

    #[test]
    fn encodes_each_name() {
        assert_eq!(
            link(Some("h.example"), "`my cat`.`s?#`.`a/b%c`"),
            Ok("https://h.example/explore/data/my%20cat/s%3F%23/a%2Fb%25c".to_owned())
        );
        // A doubled backtick is one backtick in the name; a dot inside quotes is kept.
        assert_eq!(
            link(Some("h.example"), "`c`.`s.x`.`t``q`"),
            Ok("https://h.example/explore/data/c/s.x/t%60q".to_owned())
        );
        assert_eq!(
            link(Some("h.example"), "`café`.s.t"),
            Ok("https://h.example/explore/data/caf%C3%A9/s/t".to_owned())
        );
    }

    #[test]
    fn refuses_names_it_would_have_to_repair() {
        for relation in [
            "",
            "a..c",
            "`a`.`b",
            "my table.s.t",
            "`a`b.c.d",
            "`.`.s.t",
            "`..`.s.t",
            "c.`..`.t",
            "c.s.``",
        ] {
            let got = link(Some("h.example"), relation);
            assert!(
                matches!(got, Err(NoRelationLink::InvalidName { .. })),
                "{relation}: {got:?}"
            );
        }
        // Whitespace around the separators is fine.
        assert_eq!(
            link(Some("h.example"), " `c` . `s` . t "),
            Ok("https://h.example/explore/data/c/s/t".to_owned())
        );
        // Dots inside a longer name are ordinary characters.
        assert_eq!(
            link(Some("h.example"), "c.s.`...x`"),
            Ok("https://h.example/explore/data/c/s/...x".to_owned())
        );
    }

    #[test]
    fn needs_every_part() {
        for relation in ["`sales`.`orders`", "orders", "a.b.c.d"] {
            let got = link(Some("h.example"), relation);
            assert!(
                matches!(got, Err(NoRelationLink::NotQualified { needed: 3, .. })),
                "{relation}: {got:?}"
            );
        }
        // No host: said before anything else.
        assert!(matches!(
            link(None, "a.b.c"),
            Err(NoRelationLink::NotConfigured { .. })
        ));
    }
}
