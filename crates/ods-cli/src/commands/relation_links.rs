//! "Open in warehouse" links (#329): the project's warehouse plugin (ADR-0031 §3a)
//! turns each node's relation into a link, or a reason there is none, and this keeps
//! only a linker that advertises the `relation_link` capability. Modules and `ods-web`
//! only see the neutral [`RelationLinkFields`] (ADR-0001, rule 1).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use ods_core::{Capability, Strategy, choose};
use ods_lineage::GraphDocument;
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLinkFields, RelationLinker};

use crate::plugins::{Plugins, WarehouseSettings};

/// The configuration links are built from, read once: each warehouse plugin reads the
/// settings of its own providers from it.
#[derive(Clone)]
pub(super) struct LinkSettings {
    config: ods_config::Config,
}

impl fmt::Debug for LinkSettings {
    /// Names the providers, never their settings: a host can carry a user part.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.config.providers.keys())
            .finish()
    }
}

impl LinkSettings {
    /// From `config`.
    pub(super) fn read(config: &ods_config::Loaded) -> Self {
        Self {
            config: config.config.clone(),
        }
    }

    /// What the plugin for `warehouse` reads.
    fn for_warehouse(&self, warehouse: Option<&str>) -> WarehouseSettings {
        warehouse.map_or_else(WarehouseSettings::default, |w| {
            WarehouseSettings::from_config(&self.config, w)
        })
    }
}

/// Links for one project: its target's warehouse's linker, or why there is none.
pub(super) struct Links {
    linker: Result<Arc<dyn RelationLinker>, NoRelationLink>,
}

impl Links {
    /// Links for a project whose manifest names `adapter_type` as its warehouse.
    pub(super) fn new(adapter_type: Option<&str>, settings: &LinkSettings) -> Self {
        Self::with(
            crate::plugins::installed(),
            adapter_type,
            &settings.for_warehouse(adapter_type),
        )
    }

    /// Links by `plugins`, for a project on `adapter_type`, from `settings`.
    fn with(plugins: &Plugins, adapter_type: Option<&str>, settings: &WarehouseSettings) -> Self {
        let linker = plugins.links(adapter_type, settings);
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

    const UC: &str = "[providers.uc]\nkind = \"databricks\"\n[providers.uc.settings]\nhost = \"https://dbc-1.cloud.databricks.com/\"\n";

    fn links(adapter: Option<&str>, toml: &str) -> Links {
        let config: ods_config::Config = toml::from_str(toml).unwrap();
        let settings = adapter.map_or_else(WarehouseSettings::empty, |a| {
            WarehouseSettings::from_config(&config, a).with_env([("X", "")])
        });
        Links::with(&Plugins::builtin(), adapter, &settings)
    }

    #[test]
    fn links_databricks_relations_to_catalog_explorer() {
        let fields = links(Some("databricks"), UC).fields(Some("`main`.`jaffle`.`orders`"));
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
        assert_eq!(
            why(
                &links(Some("duckdb"), UC),
                Some("\"db\".\"main\".\"orders\"")
            ),
            "no warehouse link for `duckdb` targets"
        );
        assert!(why(&links(None, UC), Some("a.b.c")).contains("doesn't name its warehouse"));
        assert_eq!(
            why(&links(Some("databricks"), ""), Some("a.b.c")),
            "no warehouse link: `providers.databricks.settings.host` (kind \"databricks\") or `DATABRICKS_HOST` isn't configured"
        );
        let databricks = links(Some("databricks"), UC);
        assert!(
            why(&databricks, Some("`jaffle`.`orders`"))
                .ends_with("has 2 parts, and a link needs 3")
        );
        assert_eq!(
            why(&databricks, None),
            "no warehouse link: it builds no relation"
        );
    }

    #[test]
    fn settings_name_providers_never_their_values() {
        let config: ods_config::Config =
            toml::from_str(&UC.replace("https://", "https://me:secret@")).unwrap();
        let settings = LinkSettings { config };
        assert!(!format!("{settings:?}").contains("secret"), "{settings:?}");
    }
}
