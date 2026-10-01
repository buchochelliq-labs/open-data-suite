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

/// The workspace links are built from, read once from configuration.
///
/// Only a host that passed the provider's checks is kept, as the `https://<host>` it
/// normalises to: the value as configured, which could carry a user part, is never
/// stored, so it can't reach `Debug` output or logs (rule 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LinkSettings {
    /// The workspace, or why there is none.
    host: Result<Workspace, NoRelationLink>,
}

/// A workspace links can be built for, as the provider checked it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Workspace {
    /// Its checked `https://<host>`.
    base: String,
    /// Its workspace id, configured or named by the host, if known.
    id: Option<String>,
    /// The provider instance it is configured on.
    instance: String,
}

impl Workspace {
    fn explorer(&self) -> CatalogExplorer {
        CatalogExplorer::new(self.instance.clone(), Some(&self.base))
            .with_workspace_id(self.id.as_deref())
    }
}

/// `host` and `workspace_id`, checked as the provider builds links from them.
fn checked(
    instance: &str,
    host: &str,
    workspace_id: Option<&str>,
) -> Result<Workspace, NoRelationLink> {
    let explorer = CatalogExplorer::new(instance, Some(host)).with_workspace_id(workspace_id);
    let base = explorer.base().map_err(Clone::clone)?.to_owned();
    let id = explorer
        .workspace_id()
        .map_err(Clone::clone)?
        .map(str::to_owned);
    Ok(Workspace {
        base,
        id,
        instance: instance.to_owned(),
    })
}

/// A provider's `workspace_id` setting: a string or a whole number, as TOML allows.
fn workspace_id(
    instance: &str,
    value: Option<&toml::Value>,
) -> Result<Option<String>, NoRelationLink> {
    match value {
        None => Ok(None),
        Some(toml::Value::String(id)) => Ok(Some(id.clone())),
        Some(toml::Value::Integer(id)) => Ok(Some(id.to_string())),
        Some(_) => Err(NoRelationLink::InvalidSetting {
            setting: format!("providers.{instance}.settings.workspace_id"),
            why: "must be the workspace's numeric id".to_owned(),
        }),
    }
}

impl LinkSettings {
    /// From `config` and the environment.
    pub(super) fn read(config: &ods_config::Loaded) -> Self {
        Self::read_with(&config.config, std::env::var(HOST_ENV).ok())
    }

    /// From `config`, with `env_host` as `DATABRICKS_HOST`.
    fn read_with(config: &ods_config::Config, env_host: Option<String>) -> Self {
        let instances: Vec<(&String, &ods_config::ProviderConfig)> = config
            .providers
            .iter()
            .filter(|(_, p)| p.kind == DATABRICKS)
            .collect();
        let instance = || {
            instances
                .first()
                .map_or_else(|| DATABRICKS.to_owned(), |(n, _)| (*n).clone())
        };
        // The environment is read before configuration (ADR-0021 §3). Its workspace id
        // is the one the providers configure; if they configure different ones, which
        // belongs to this host isn't known.
        if let Some(host) = env_host.filter(|h| !h.trim().is_empty()) {
            let instance = instance();
            let mut ids = Vec::new();
            for (name, provider) in &instances {
                match workspace_id(name, provider.settings.get("workspace_id")) {
                    Ok(Some(id)) => ids.push((id.trim().to_owned(), (*name).clone())),
                    Ok(None) => {}
                    Err(why) => return Self { host: Err(why) },
                }
            }
            ids.sort();
            ids.dedup_by(|a, b| a.0 == b.0);
            let id = match ids.as_slice() {
                [] => None,
                [(id, _)] => Some(id.as_str()),
                several => {
                    return Self {
                        host: Err(differs(
                            "workspace_id",
                            several.iter().map(|(_, n)| n.as_str()),
                        )),
                    };
                }
            };
            return Self {
                host: checked(&instance, &host, id),
            };
        }
        let mut hosts: Vec<Workspace> = Vec::new();
        for (name, provider) in &instances {
            let id = match workspace_id(name, provider.settings.get("workspace_id")) {
                Ok(id) => id,
                Err(why) => return Self { host: Err(why) },
            };
            match provider.settings.get("host") {
                None => {}
                Some(toml::Value::String(h)) => match checked(name, h, id.as_deref()) {
                    Ok(workspace) => hosts.push(workspace),
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
        hosts.sort();
        hosts.dedup_by(|a, b| a.base == b.base && a.id == b.id);
        let host = match hosts.as_slice() {
            [] => Err(NoRelationLink::NotConfigured {
                setting: format!(
                    "providers.{}.settings.host` (kind \"{DATABRICKS}\") or `{HOST_ENV}",
                    instance()
                ),
            }),
            [workspace] => Ok(workspace.clone()),
            // One host with different (or some missing) ids is still ambiguous: say
            // which setting differs.
            several if several.iter().all(|w| w.base == several[0].base) => Err(differs(
                "workspace_id",
                several.iter().map(|w| w.instance.as_str()),
            )),
            several => Err(differs("host", several.iter().map(|w| w.instance.as_str()))),
        };
        Self { host }
    }
}

/// `setting` differs between the providers `instances`, so the workspace isn't known.
fn differs<'a>(setting: &str, instances: impl Iterator<Item = &'a str>) -> NoRelationLink {
    NoRelationLink::InvalidSetting {
        setting: setting.to_owned(),
        why: format!(
            "differs between the {DATABRICKS} providers {}, so which workspace to link to isn't known",
            instances.collect::<Vec<_>>().join(", ")
        ),
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
            Some(DATABRICKS) => settings
                .host
                .as_ref()
                .map(|workspace| Box::new(workspace.explorer()) as Box<dyn RelationLinker>)
                .map_err(Clone::clone),
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
    fn adds_the_workspace_id() {
        let url = |toml: &str, env: Option<&str>| {
            Links::new(Some("databricks"), &settings(toml, env))
                .fields(Some("a.b.c"))
                .relation_url
        };
        for id in ["workspace_id = \"1234\"\n", "workspace_id = 1234\n"] {
            assert_eq!(
                url(&format!("{UC}{id}"), None).as_deref(),
                Some("https://dbc-1.cloud.databricks.com/explore/data/a/b/c?o=1234"),
                "{id}"
            );
        }
        // With the host from the environment, the configured id still applies.
        assert_eq!(
            url(&format!("{UC}workspace_id = 7\n"), Some("env.example")).as_deref(),
            Some("https://env.example/explore/data/a/b/c?o=7")
        );
        // Named by an Azure host, with nothing configured.
        assert_eq!(
            url("", Some("adb-55.3.azuredatabricks.net")).as_deref(),
            Some("https://adb-55.3.azuredatabricks.net/explore/data/a/b/c?o=55")
        );
        // Not an id: no link, and the reason names the setting.
        let bad = Links::new(
            Some("databricks"),
            &settings(&format!("{UC}workspace_id = \"12x\"\n"), None),
        );
        assert!(
            bad.fields(Some("a.b.c"))
                .relation_url_unavailable
                .unwrap()
                .contains("workspace_id"),
        );
        let table = settings(
            &format!("{UC}workspace_id = {{ secret = \"env:W\" }}\n"),
            None,
        );
        assert!(matches!(
            table.host,
            Err(NoRelationLink::InvalidSetting { .. })
        ));
        // Different ids with the host from the environment: which one isn't known.
        let env_two = settings(
            &format!(
                "{UC}workspace_id = 1\n[providers.b]\nkind = \"databricks\"\n[providers.b.settings]\nworkspace_id = 2\n"
            ),
            Some("env.example"),
        );
        assert!(
            matches!(&env_two.host, Err(NoRelationLink::InvalidSetting { setting, .. }) if setting == "workspace_id"),
            "{env_two:?}"
        );
        // An id that isn't the one an Azure host names: refused, not overridden.
        let azure = settings(
            &format!("{UC}workspace_id = 7\n"),
            Some("adb-55.3.azuredatabricks.net"),
        );
        assert!(matches!(
            azure.host,
            Err(NoRelationLink::InvalidSetting { .. })
        ));
        // An empty id is a configured value that isn't one.
        let empty = settings(&format!("{UC}workspace_id = \"\"\n"), None);
        assert!(matches!(
            empty.host,
            Err(NoRelationLink::InvalidSetting { .. })
        ));
        // One host with two ids is two workspaces: which one isn't known.
        let two = settings(
            &format!(
                "{UC}workspace_id = 1\n[providers.b]\nkind = \"databricks\"\n[providers.b.settings]\nhost = \"dbc-1.cloud.databricks.com\"\nworkspace_id = 2\n"
            ),
            None,
        );
        assert!(matches!(
            two.host,
            Err(NoRelationLink::InvalidSetting { .. })
        ));
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
        let same = same.host.unwrap();
        assert_eq!(same.base, "https://dbc-1.cloud.databricks.com");
        assert_eq!(same.instance, "b");
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
