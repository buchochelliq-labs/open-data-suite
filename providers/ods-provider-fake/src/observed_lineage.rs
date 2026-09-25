//! A scripted [`ObservedLineageSource`].

use ods_core::CapabilitySet;
use ods_sdk::contracts::observed_lineage::{ObservedLineage, ObservedLineageSource};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;

/// Returns the lineage it was given.
#[derive(Debug, Clone, Default)]
pub struct FakeObservedLineageSource(pub ObservedLineage);

impl Provider for FakeObservedLineageSource {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "fake",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

impl ObservedLineageSource for FakeObservedLineageSource {
    fn observed_lineage(&self) -> Result<ObservedLineage, ProviderError> {
        Ok(self.0.clone())
    }
}
