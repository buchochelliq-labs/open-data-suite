//! The built-in Databricks plugin (ADR-0031 §3, §3a): Delta table versions
//! (ADR-0022), Unity Catalog's login check (ADR-0030 §4e), Catalog Explorer links
//! (#329), Unity Catalog's exported column lineage, dbt-databricks's error messages
//! (ADR-0025), and the Databricks dialect. The
//! only place the CLI names Databricks' providers.
//!
//! A link is where the manifest says the relation is, not proof it exists (rule 3), and
//! the host it is built from is configuration, never a credential (rule 9).

use std::path::Path;
use std::sync::Arc;

use ods_provider_databricks::{
    CatalogExplorer, DatabricksErrors, DeltaVersions, UcColumnLineage, UnityCatalog,
};
use ods_sdk::ProviderError;
use ods_sdk::contracts::changes::ChangeProvider;
use ods_sdk::contracts::error_catalogue::ErrorCatalogue;
use ods_sdk::contracts::observed_lineage::ObservedLineageSource;
use ods_sdk::contracts::privileges::PrivilegedProbe;
use ods_sdk::contracts::probe::RelationProbe;
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLinker};

use super::{Origin, WarehousePlugin, WarehouseSettings};

/// The warehouse kind dbt calls Databricks, and the provider kind its `host` is read
/// from (ADR-0021 §3).
const DATABRICKS: &str = ods_provider_databricks::KIND;

/// Read as the default for `host`, as ADR-0021 §3 says.
const HOST_ENV: &str = "DATABRICKS_HOST";

/// The Databricks plugin.
pub(super) struct Databricks;

impl WarehousePlugin for Databricks {
    fn origin(&self) -> Origin {
        Origin {
            name: "ods-provider-databricks",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn warehouse(&self) -> &str {
        DATABRICKS
    }

    fn versions_read(&self) -> Option<String> {
        Some("table version from the Delta history".to_owned())
    }

    fn changes(&self, probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn ChangeProvider>> {
        Some(Arc::new(DeltaVersions::new(probe)))
    }

    fn privileges(&self, probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn PrivilegedProbe>> {
        Some(Arc::new(UnityCatalog::new(probe)))
    }

    fn links(
        &self,
        settings: &WarehouseSettings,
    ) -> Result<Arc<dyn RelationLinker>, NoRelationLink> {
        let workspace = workspace(settings)?;
        Ok(Arc::new(
            CatalogExplorer::new(workspace.instance, Some(&workspace.base))
                .with_workspace_id(workspace.id.as_deref()),
        ))
    }

    fn observed_lineage(
        &self,
        export: &Path,
    ) -> Option<Result<Arc<dyn ObservedLineageSource>, ProviderError>> {
        Some(
            UcColumnLineage::from_path(export)
                .map(|source| Arc::new(source) as Arc<dyn ObservedLineageSource>),
        )
    }

    fn errors(&self) -> Option<Arc<dyn ErrorCatalogue>> {
        Some(Arc::new(DatabricksErrors::new()))
    }

    fn dialect(&self) -> Option<&str> {
        Some("databricks")
    }
}

/// A workspace links can be built for, as the provider checked it.
///
/// Only a host that passed the provider's checks is kept, as the `https://<host>` it
/// normalises to: the value as configured, which could carry a user part, is never
/// stored, so it can't reach `Debug` output or logs (rule 9).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Workspace {
    /// Its checked `https://<host>`.
    base: String,
    /// Its workspace id, configured or named by the host, if known.
    id: Option<String>,
    /// The provider instance it is configured on.
    instance: String,
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

/// The workspace to link to, from `DATABRICKS_HOST` first, then the configured
/// providers (ADR-0021 §3), or why there is none.
fn workspace(settings: &WarehouseSettings) -> Result<Workspace, NoRelationLink> {
    let instances = settings.providers();
    let instance = || {
        instances
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| DATABRICKS.to_owned())
    };
    // The environment is read before configuration (ADR-0021 §3). Its workspace id
    // is the one the providers configure; if they configure different ones, which
    // belongs to this host isn't known.
    if let Some(host) = settings.env(HOST_ENV).filter(|h| !h.trim().is_empty()) {
        let mut ids = Vec::new();
        for (name, provider) in instances {
            if let Some(id) = workspace_id(name, provider.get("workspace_id"))? {
                ids.push((id.trim().to_owned(), name.clone()));
            }
        }
        ids.sort();
        ids.dedup_by(|a, b| a.0 == b.0);
        let id = match ids.as_slice() {
            [] => None,
            [(id, _)] => Some(id.as_str()),
            several => {
                return Err(differs(
                    "workspace_id",
                    several.iter().map(|(_, n)| n.as_str()),
                ));
            }
        };
        return checked(&instance(), &host, id);
    }
    let mut hosts: Vec<Workspace> = Vec::new();
    for (name, provider) in instances {
        let id = workspace_id(name, provider.get("workspace_id"))?;
        match provider.get("host") {
            None => {}
            Some(toml::Value::String(h)) => hosts.push(checked(name, h, id.as_deref())?),
            Some(_) => {
                return Err(NoRelationLink::InvalidSetting {
                    setting: format!("providers.{name}.settings.host"),
                    why: "must be a plain string: a workspace URL isn't a secret".to_owned(),
                });
            }
        }
    }
    hosts.sort();
    hosts.dedup_by(|a, b| a.base == b.base && a.id == b.id);
    match hosts.as_slice() {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(toml: &str, env: Option<&str>) -> WarehouseSettings {
        let config: ods_config::Config = toml::from_str(toml).unwrap();
        WarehouseSettings::from_config(&config, DATABRICKS)
            .with_env(env.map(|h| (HOST_ENV, h.to_owned())))
    }

    /// The link to `relation`, or why there is none.
    fn link(settings: &WarehouseSettings, relation: &str) -> Result<String, String> {
        Databricks
            .links(settings)
            .and_then(|l| l.link(relation))
            .map(|l| l.url)
            .map_err(|why| why.to_string())
    }

    const UC: &str = "[providers.uc]\nkind = \"databricks\"\n[providers.uc.settings]\nhost = \"https://dbc-1.cloud.databricks.com/\"\n";

    #[test]
    fn refuses_an_insecure_host() {
        let http = settings(&UC.replace("https://", "http://"), None);
        assert!(link(&http, "a.b.c").unwrap_err().contains("https://"));
    }

    #[test]
    fn reads_the_host_from_the_environment_first() {
        assert_eq!(
            link(&settings(UC, Some("env.example")), "a.b.c").unwrap(),
            "https://env.example/explore/data/a/b/c"
        );
        assert!(link(&settings("", Some("env.example")), "a.b.c").is_ok());
    }

    #[test]
    fn adds_the_workspace_id() {
        let url = |toml: &str, env: Option<&str>| link(&settings(toml, env), "a.b.c");
        for id in ["workspace_id = \"1234\"\n", "workspace_id = 1234\n"] {
            assert_eq!(
                url(&format!("{UC}{id}"), None).unwrap(),
                "https://dbc-1.cloud.databricks.com/explore/data/a/b/c?o=1234",
                "{id}"
            );
        }
        // With the host from the environment, the configured id still applies.
        assert_eq!(
            url(&format!("{UC}workspace_id = 7\n"), Some("env.example")).unwrap(),
            "https://env.example/explore/data/a/b/c?o=7"
        );
        // Named by an Azure host, with nothing configured.
        assert_eq!(
            url("", Some("adb-55.3.azuredatabricks.net")).unwrap(),
            "https://adb-55.3.azuredatabricks.net/explore/data/a/b/c?o=55"
        );
        // Not an id: no link, and the reason names the setting.
        assert!(
            url(&format!("{UC}workspace_id = \"12x\"\n"), None)
                .unwrap_err()
                .contains("workspace_id")
        );
        let invalid = |toml: &str, env: Option<&str>| {
            matches!(
                workspace(&settings(toml, env)),
                Err(NoRelationLink::InvalidSetting { .. })
            )
        };
        assert!(invalid(
            &format!("{UC}workspace_id = {{ secret = \"env:W\" }}\n"),
            None
        ));
        // Different ids with the host from the environment: which one isn't known.
        let env_two = workspace(&settings(
            &format!(
                "{UC}workspace_id = 1\n[providers.b]\nkind = \"databricks\"\n[providers.b.settings]\nworkspace_id = 2\n"
            ),
            Some("env.example"),
        ));
        assert!(
            matches!(&env_two, Err(NoRelationLink::InvalidSetting { setting, .. }) if setting == "workspace_id"),
            "{env_two:?}"
        );
        // An id that isn't the one an Azure host names: refused, not overridden.
        assert!(invalid(
            &format!("{UC}workspace_id = 7\n"),
            Some("adb-55.3.azuredatabricks.net")
        ));
        // An empty id is a configured value that isn't one.
        assert!(invalid(&format!("{UC}workspace_id = \"\"\n"), None));
        // One host with two ids is two workspaces: which one isn't known.
        assert!(invalid(
            &format!(
                "{UC}workspace_id = 1\n[providers.b]\nkind = \"databricks\"\n[providers.b.settings]\nhost = \"dbc-1.cloud.databricks.com\"\nworkspace_id = 2\n"
            ),
            None
        ));
    }

    #[test]
    fn keeps_only_a_checked_host() {
        let with_user = workspace(&settings(
            &UC.replace("https://", "https://me:secret@"),
            None,
        ));
        assert!(matches!(
            with_user,
            Err(NoRelationLink::InvalidSetting { .. })
        ));
        assert!(
            !format!("{with_user:?}").contains("secret"),
            "{with_user:?}"
        );
        let env = workspace(&settings("", Some("https://me:secret@env.example")));
        assert!(!format!("{env:?}").contains("secret"), "{env:?}");
        // Stored as it is normalised: the same workspace written two ways is one.
        let same = workspace(&settings(
            &format!(
                "{UC}[providers.b]\nkind = \"databricks\"\n[providers.b.settings]\nhost = \"DBC-1.cloud.databricks.com\"\n"
            ),
            None,
        ))
        .unwrap();
        assert_eq!(same.base, "https://dbc-1.cloud.databricks.com");
        assert_eq!(same.instance, "b");
    }

    #[test]
    fn refuses_a_secret_or_ambiguous_host() {
        let secret = workspace(&settings(
            "[providers.uc]\nkind = \"databricks\"\n[providers.uc.settings]\nhost = { secret = \"env:H\" }\n",
            None,
        ));
        assert!(matches!(secret, Err(NoRelationLink::InvalidSetting { .. })));
        let two = workspace(&settings(
            &format!(
                "{UC}[providers.other]\nkind = \"databricks\"\n[providers.other.settings]\nhost = \"x.example\"\n"
            ),
            None,
        ));
        assert!(matches!(two, Err(NoRelationLink::InvalidSetting { .. })));
        // The same host twice is one workspace.
        let same = workspace(&settings(
            &format!(
                "{UC}[providers.b]\nkind = \"databricks\"\n[providers.b.settings]\nhost = \"https://dbc-1.cloud.databricks.com/\"\n"
            ),
            None,
        ));
        assert!(same.is_ok());
    }

    #[test]
    fn only_databricks_providers_are_read() {
        // Another kind's host is never a Databricks workspace.
        let other = settings(
            "[providers.sf]\nkind = \"snowflake\"\n[providers.sf.settings]\nhost = \"x.example\"\n",
            None,
        );
        assert!(matches!(
            workspace(&other),
            Err(NoRelationLink::NotConfigured { .. })
        ));
    }
}
