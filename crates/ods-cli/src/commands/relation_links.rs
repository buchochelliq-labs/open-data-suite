//! "Open in warehouse" links (#329): the CLI maps the target's warehouse to a provider
//! with the `relation_link` capability, and turns each node's relation into a link to
//! it, or a reason there is none. Modules and `ods-web` only see the neutral
//! [`RelationLinkFields`] (ADR-0001, rule 1). Only this module names a warehouse.
//!
//! A link is where the manifest says the relation is, not proof it exists (rule 3), and
//! the host it is built from is configuration, never a credential (rule 9).

use std::collections::BTreeMap;

use ods_core::{Capability, Strategy, choose};
use ods_lineage::GraphDocument;
use ods_provider_databricks::CatalogExplorer;
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLinkFields, RelationLinker};

/// The provider kind whose `host` Catalog Explorer links are built from (ADR-0021 §3).
const DATABRICKS: &str = ods_provider_databricks::KIND;

/// Read as the default for `host`, as ADR-0021 §3 says.
const HOST_ENV: &str = "DATABRICKS_HOST";

/// The workspace host links are built from, read once from configuration.
///
/// Only a host that passed the provider's checks is kept, as the `https://<host>` it
/// normalises to: the value as configured, which could carry a user part, is never
/// stored, so it can't reach `Debug` output or logs (rule 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LinkSettings {
    /// The provider instance and its checked `https://<host>`, or why there is none.
    host: Result<(String, String), NoRelationLink>,
}

/// `host`, checked and normalised as the provider builds links from it.
fn checked(instance: &str, host: &str) -> Result<String, NoRelationLink> {
    CatalogExplorer::new(instance, Some(host))
        .base()
        .map(str::to_owned)
        .map_err(Clone::clone)
}

impl LinkSettings {
    /// From `config` and the environment.
    pub(super) fn read(config: &ods_config::Loaded) -> Self {
        Self::read_with(&config.config, std::env::var(HOST_ENV).ok())
    }

    /// From `config`, with `env_host` as `DATABRICKS_HOST`.
    fn read_with(config: &ods_config::Config, env_host: Option<String>) -> Self {
        let instances: Vec<(&String, Option<&toml::Value>)> = config
            .providers
            .iter()
            .filter(|(_, p)| p.kind == DATABRICKS)
            .map(|(name, p)| (name, p.settings.get("host")))
            .collect();
        let instance = || {
            instances
                .first()
                .map_or_else(|| DATABRICKS.to_owned(), |(n, _)| (*n).clone())
        };
        // The environment is read before configuration (ADR-0021 §3).
        if let Some(host) = env_host.filter(|h| !h.trim().is_empty()) {
            let instance = instance();
            return Self {
                host: checked(&instance, &host).map(|base| (instance, base)),
            };
        }
        let mut hosts: Vec<(&String, String)> = Vec::new();
        for (name, host) in &instances {
            match host {
                None => {}
                Some(toml::Value::String(h)) => match checked(name, h) {
                    Ok(base) => hosts.push((name, base)),
                    Err(why) => return Self { host: Err(why) },
                },
                Some(_) => {
                    return Self {
                        host: Err(NoRelationLink::InvalidSetting {
                            setting: format!("providers.{name}.settings.host"),
                            why: "must be a plain string: a workspace URL isn't a secret"
                                .to_owned(),
                        }),
                    };
                }
            }
        }
        hosts.sort_by(|a, b| a.1.cmp(&b.1));
        hosts.dedup_by(|a, b| a.1 == b.1);
        let host = match hosts.as_slice() {
            [] => Err(NoRelationLink::NotConfigured {
                setting: format!(
                    "providers.{}.settings.host` (kind \"{DATABRICKS}\") or `{HOST_ENV}",
                    instance()
                ),
            }),
            [(name, host)] => Ok(((*name).clone(), host.clone())),
            several => Err(NoRelationLink::InvalidSetting {
                setting: "host".to_owned(),
                why: format!(
                    "differs between the {DATABRICKS} providers {}, so which workspace to link to isn't known",
                    several
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        };
        Self { host }
    }
}

/// Links for one project: its target's warehouse's linker, or why there is none.
pub(super) struct Links {
    linker: Result<Box<dyn RelationLinker>, NoRelationLink>,
}

impl Links {
    /// Links for a project whose manifest names `adapter_type` as its warehouse.
    pub(super) fn new(adapter_type: Option<&str>, settings: &LinkSettings) -> Self {
        let unsupported = || NoRelationLink::Unsupported {
            warehouse: adapter_type.map(str::to_owned),
        };
        let linker: Result<Box<dyn RelationLinker>, NoRelationLink> = match adapter_type {
            Some(DATABRICKS) => settings.host.clone().map(|(instance, host)| {
                Box::new(CatalogExplorer::new(instance, Some(&host))) as Box<dyn RelationLinker>
            }),
            _ => Err(unsupported()),
        };
        // A provider is used only for what it advertises (ADR-0006 §3).
        let linker = linker.and_then(|linker| {
            let strategies = [
                Strategy::new("relation_link", [Capability::RelationLink], true),
                Strategy::fallback("no_link", false),
            ];
            let info = linker.info();
            match choose(&info.capabilities, &strategies) {
                Ok(choice) if choice.chosen.value => Ok(linker),
                _ => Err(NoRelationLink::NotOffered {
                    provider: info.kind,
                }),
            }
        });
        Self { linker }
    }

    /// The link to `relation`, or why there is none; `None` means the node builds no
    /// relation.
    pub(super) fn fields(&self, relation: Option<&str>) -> RelationLinkFields {
        let result = match (&self.linker, relation) {
            (Err(why), _) => Err(why.clone()),
            (Ok(_), None) => Err(NoRelationLink::NoRelation),
            (Ok(linker), Some(relation)) => linker.link(relation),
        };
        RelationLinkFields::from(result)
    }

    /// Fills in each node's link in `document`, from `relations` (node id → relation as
    /// the manifest renders it).
    pub(super) fn annotate(
        &self,
        document: &mut GraphDocument,
        relations: &BTreeMap<String, String>,
    ) {
        for node in &mut document.nodes {
            node.relation_link = self.fields(relations.get(&node.id).map(String::as_str));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> ods_config::Config {
        toml::from_str(toml).unwrap()
    }

    fn settings(toml: &str, env: Option<&str>) -> LinkSettings {
        LinkSettings::read_with(&config(toml), env.map(str::to_owned))
    }

    const UC: &str = "[providers.uc]\nkind = \"databricks\"\n[providers.uc.settings]\nhost = \"https://dbc-1.cloud.databricks.com/\"\n";

    #[test]
    fn links_databricks_relations_to_catalog_explorer() {
        let links = Links::new(Some("databricks"), &settings(UC, None));
        let fields = links.fields(Some("`main`.`jaffle`.`orders`"));
        assert_eq!(
            fields.relation_url.as_deref(),
            Some("https://dbc-1.cloud.databricks.com/explore/data/main/jaffle/orders")
        );
        assert_eq!(
            fields.relation_url_label.as_deref(),
            Some("Open in Catalog Explorer")
        );
        assert_eq!(fields.relation_url_unavailable, None);
    }

    #[test]
    fn says_why_there_is_no_link() {
        let why = |links: &Links, relation: Option<&str>| {
            let fields = links.fields(relation);
            assert_eq!(fields.relation_url, None);
            fields.relation_url_unavailable.unwrap()
        };
        let duckdb = Links::new(Some("duckdb"), &settings(UC, None));
        assert_eq!(
            why(&duckdb, Some("\"db\".\"main\".\"orders\"")),
            "no warehouse link for `duckdb` targets"
        );
        let unnamed = Links::new(None, &settings(UC, None));
        assert!(why(&unnamed, Some("a.b.c")).contains("doesn't name its warehouse"));
        let no_host = Links::new(Some("databricks"), &settings("", None));
        assert_eq!(
            why(&no_host, Some("a.b.c")),
            "no warehouse link: `providers.databricks.settings.host` (kind \"databricks\") or `DATABRICKS_HOST` isn't configured"
        );
        let two_parts = Links::new(Some("databricks"), &settings(UC, None));
        assert!(
            why(&two_parts, Some("`jaffle`.`orders`")).ends_with("has 2 parts, and a link needs 3")
        );
        assert_eq!(
            why(&two_parts, None),
            "no warehouse link: it builds no relation"
        );
        let http = Links::new(
            Some("databricks"),
            &settings(&UC.replace("https://", "http://"), None),
        );
        assert!(why(&http, Some("a.b.c")).contains("https://"));
    }

    #[test]
    fn reads_the_host_from_the_environment_first() {
        let links = Links::new(Some("databricks"), &settings(UC, Some("env.example")));
        assert_eq!(
            links.fields(Some("a.b.c")).relation_url.as_deref(),
            Some("https://env.example/explore/data/a/b/c")
        );
        let only_env = Links::new(Some("databricks"), &settings("", Some("env.example")));
        assert!(only_env.fields(Some("a.b.c")).relation_url.is_some());
    }

    #[test]
    fn keeps_only_a_checked_host() {
        let with_user = settings(&UC.replace("https://", "https://me:secret@"), None);
        assert!(matches!(
            with_user.host,
            Err(NoRelationLink::InvalidSetting { .. })
        ));
        assert!(
            !format!("{with_user:?}").contains("secret"),
            "{with_user:?}"
        );
        let env = settings("", Some("https://me:secret@env.example"));
        assert!(!format!("{env:?}").contains("secret"), "{env:?}");
        // Stored as it is normalised: the same workspace written two ways is one.
        let same = settings(
            &format!(
                "{UC}[providers.b]\nkind = \"databricks\"\n[providers.b.settings]\nhost = \"DBC-1.cloud.databricks.com\"\n"
            ),
            None,
        );
        assert_eq!(
            same.host,
            Ok((
                "b".to_owned(),
                "https://dbc-1.cloud.databricks.com".to_owned()
            ))
        );
    }

    #[test]
    fn refuses_a_secret_or_ambiguous_host() {
        let secret = settings(
            "[providers.uc]\nkind = \"databricks\"\n[providers.uc.settings]\nhost = { secret = \"env:H\" }\n",
            None,
        );
        assert!(matches!(
            secret.host,
            Err(NoRelationLink::InvalidSetting { .. })
        ));
        let two = settings(
            &format!(
                "{UC}[providers.other]\nkind = \"databricks\"\n[providers.other.settings]\nhost = \"x.example\"\n"
            ),
            None,
        );
        assert!(matches!(
            two.host,
            Err(NoRelationLink::InvalidSetting { .. })
        ));
        // The same host twice is one workspace.
        let same = settings(
            &format!(
                "{UC}[providers.b]\nkind = \"databricks\"\n[providers.b.settings]\nhost = \"https://dbc-1.cloud.databricks.com/\"\n"
            ),
            None,
        );
        assert!(same.host.is_ok());
    }
}
