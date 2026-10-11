//! Out-of-process plugins in the plugin set (ADR-0031 §5): a warehouse that a
//! `[plugins.<name>]` serves, through what it described.

use std::sync::Arc;

use ods_sdk::contracts::changes::ChangeProvider;
use ods_sdk::contracts::probe::RelationProbe;

use super::{Origin, WarehousePlugin};

/// A warehouse an out-of-process plugin serves: its source versions, through its own
/// connection, never dbt's (rule 9: ODS hands it no credential).
pub(crate) struct ProcessWarehouse {
    pub(crate) origin: Origin,
    pub(crate) warehouse: String,
    pub(crate) changes: Option<Arc<dyn ChangeProvider>>,
    pub(crate) versions_read: Option<String>,
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
}
