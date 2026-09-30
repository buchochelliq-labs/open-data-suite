//! Links to a relation's page in Catalog Explorer, the workspace's UI for Unity Catalog
//! (#329), as a [`RelationLinker`].
//!
//! The page of table `c.s.t` is `https://<workspace-host>/explore/data/c/s/t`. That
//! shape is the one Databricks' own documentation uses for links to a table, e.g. the
//! `databricksWorkspaceUrl` of access-request notifications
//! (<https://learn.microsoft.com/azure/databricks/data-governance/unity-catalog/manage-privileges/access-request-destinations#access-request-examples>).
//! The workspace id (`?o=`) that some of those links carry is left out: a link never
//! has a query string (AGENTS rule 9), and a workspace's own host already names it.
//!
//! The host comes from configuration (`host`, ADR-0021); it isn't a secret, and no
//! token ever goes into a link. Nothing is fetched: the link is where the manifest says
//! the relation is, not proof that it exists.

use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLink, RelationLinker};
use ods_sdk::{Provider, ProviderInfo};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::KIND;

/// What the link is called.
pub const LABEL: &str = "Open in Catalog Explorer";

/// Kept as they are in a path segment (RFC 3986's unreserved characters); everything
/// else is percent-encoded, so a name is always exactly one segment.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Links relations to Catalog Explorer in one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogExplorer {
    instance: String,
    /// `https://<host>`, or why there is none.
    base: Result<String, NoRelationLink>,
}

impl CatalogExplorer {
    /// Links into the workspace at `host`, as configured for provider `instance`.
    /// `host` may be `https://<host>` or a bare host name, with or without a trailing
    /// `/`; anything else (another scheme, a path, a query string, a user part) is
    /// refused, and every link then says why.
    pub fn new(instance: impl Into<String>, host: Option<&str>) -> Self {
        Self {
            instance: instance.into(),
            base: base(host),
        }
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

/// The parts of a relation as dbt renders it for Databricks: `` `c`.`s`.`t` ``, where a
/// backtick in a name is doubled, or unquoted `c.s.t`. `None` if a quote isn't closed
/// or a part is empty.
fn parts(relation: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = relation.trim().chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '`' if quoted && chars.peek() == Some(&'`') => {
                chars.next();
                current.push('`');
            }
            '`' => quoted = !quoted,
            '.' if !quoted => out.push(std::mem::take(&mut current)),
            c if !quoted && c.is_whitespace() => {}
            c => current.push(c),
        }
    }
    out.push(current);
    (!quoted && out.iter().all(|p| !p.is_empty())).then_some(out)
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
        let parts = parts(relation).unwrap_or_default();
        // Unity Catalog's three levels. Two parts would leave the catalog to a
        // default this can't see, so no link is guessed (AGENTS rule 3).
        if parts.len() != 3 {
            return Err(NoRelationLink::NotQualified {
                relation: relation.trim().to_owned(),
                parts: parts.len(),
                needed: 3,
            });
        }
        let path: Vec<String> = parts
            .iter()
            .map(|p| utf8_percent_encode(p, SEGMENT).to_string())
            .collect();
        Ok(RelationLink::new(
            format!("{base}/explore/data/{}", path.join("/")),
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
            Ok("https://adb-1.2.azuredatabricks.net/explore/data/main/sales/orders".to_owned())
        );
        let explorer = CatalogExplorer::new("uc", Some("h.example"));
        assert_eq!(explorer.link("a.b.c").unwrap().label, LABEL);
        assert_eq!(explorer.info().instance, "uc");
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
    fn needs_every_part() {
        for relation in [
            "`sales`.`orders`",
            "orders",
            "",
            "a..c",
            "`a`.`b",
            "a.b.c.d",
        ] {
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
