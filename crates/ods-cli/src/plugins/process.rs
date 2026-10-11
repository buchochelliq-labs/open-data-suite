//! Out-of-process plugins in the plugin set (ADR-0031 §5, §3d): a warehouse that a
//! `[plugins.<name>]` serves, through what it described, what it answers live (source
//! versions, observed lineage), and what it answered before, as recorded (links and
//! error classifications).

use std::path::Path;
use std::sync::Arc;

use ods_provider_process::{AnsweredCatalogue, AnsweredLineage, AnsweredLinker, PluginProcess};
use ods_sdk::ProviderError;
use ods_sdk::contracts::changes::ChangeProvider;
use ods_sdk::contracts::error_catalogue::ErrorCatalogue;
use ods_sdk::contracts::observed_lineage::ObservedLineageSource;
use ods_sdk::contracts::probe::RelationProbe;
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLinker};
use ods_sdk::protocol::WarehouseSettings as Settings;

use super::{Origin, WarehousePlugin, WarehouseSettings};

/// A warehouse an out-of-process plugin serves. It never uses dbt's connection (rule 9:
/// ODS hands it no credential): what it reads, it reads over its own.
pub(crate) struct ProcessWarehouse {
    pub(crate) origin: Origin,
    pub(crate) warehouse: String,
    pub(crate) changes: Option<Arc<dyn ChangeProvider>>,
    pub(crate) versions_read: Option<String>,
    pub(crate) dialect: Option<String>,
    /// Whether it implements `relation_linker`.
    pub(crate) offers_links: bool,
    /// The links it gave, as recorded; `None` before any command asked.
    pub(crate) links: Option<Arc<AnsweredLinker>>,
    /// The errors it classified, as recorded, when it implements `error_catalogue`.
    pub(crate) errors: Option<Arc<AnsweredCatalogue>>,
    /// The plugin, to read an export with, and its warehouse's settings, when it
    /// implements `observed_lineage_source` and runs here (never on the dashboard).
    pub(crate) lineage: Option<(Arc<PluginProcess>, Settings)>,
    /// The plugin's name, for reasons.
    pub(crate) plugin: String,
}

impl WarehousePlugin for ProcessWarehouse {
    fn origin(&self) -> Origin {
        self.origin.clone()
    }

    fn warehouse(&self) -> &str {
        &self.warehouse
    }

    fn changes(&self, _probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn ChangeProvider>> {
        self.changes.clone()
    }

    fn versions_read(&self) -> Option<String> {
        self.versions_read.clone()
    }

    fn links(
        &self,
        _settings: &WarehouseSettings,
    ) -> Result<Arc<dyn RelationLinker>, NoRelationLink> {
        if !self.offers_links {
            return Err(NoRelationLink::Unsupported {
                warehouse: Some(self.warehouse.clone()),
            });
        }
        match &self.links {
            Some(links) => Ok(links.clone()),
            None => Err(NoRelationLink::NotAnswered {
                plugin: self.plugin.clone(),
                why: "no command that runs it has recorded links yet (`ods state build` or `ods health check` does)".to_owned(),
            }),
        }
    }

    fn observed_lineage(
        &self,
        export: &Path,
    ) -> Option<Result<Arc<dyn ObservedLineageSource>, ProviderError>> {
        let (process, settings) = self.lineage.as_ref()?;
        let export = std::path::absolute(export).unwrap_or_else(|_| export.to_owned());
        let export = export.display().to_string();
        // A process call from a synchronous method: on a thread of its own, with its own
        // runtime, so it works whether or not the caller is already in one.
        let read = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| ProviderError::Other(e.to_string()))?
                        .block_on(process.observed_lineage(&export, settings.clone()))
                })
                .join()
                .unwrap_or_else(|_| {
                    Err(ProviderError::Other(
                        "reading the export stopped unexpectedly".to_owned(),
                    ))
                })
        });
        Some(read.map(|lineage| {
            Arc::new(AnsweredLineage::new(process.name(), lineage))
                as Arc<dyn ObservedLineageSource>
        }))
    }

    fn errors(&self) -> Option<Arc<dyn ErrorCatalogue>> {
        self.errors
            .clone()
            .map(|errors| errors as Arc<dyn ErrorCatalogue>)
    }

    fn dialect(&self) -> Option<&str> {
        self.dialect.as_deref()
    }
}
