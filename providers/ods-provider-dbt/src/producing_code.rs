//! Explicit producing-code references from public manifest metadata.
use crate::Manifest;
use ods_core::CapabilitySet;
use ods_sdk::contracts::producing_code::{
    AssetKey, CodeMapping, CodeResolution, DeclaredMappings, ProducingCodeSource,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

/// An in-memory source over declared mappings, independent of catalog access.
#[derive(Debug, Clone)]
pub struct DbtProducingCode(DeclaredMappings);
impl DbtProducingCode {
    /// Reads `meta.ods.producing_code`, or `config.meta.ods.producing_code`.
    /// Both forms, when present, must agree. Values are arrays of code locations.
    /// # Errors
    /// Malformed metadata or conflicting forms fail explicitly.
    pub fn from_manifest(
        manifest: &Manifest,
        project: &str,
        environment: &str,
    ) -> Result<Self, ProviderError> {
        let mut entries = Vec::new();
        for node in &manifest.nodes {
            let top = node.meta.pointer("/ods/producing_code");
            let config = node.config.raw.pointer("/meta/ods/producing_code");
            if let (Some(a), Some(b)) = (top, config)
                && a != b
            {
                return Err(ProviderError::Other(
                    "conflicting producing-code metadata".into(),
                ));
            }
            if let Some(value) = top.or(config) {
                let locations: Vec<ods_sdk::contracts::producing_code::CodeLocation> =
                    serde_json::from_value(value.clone()).map_err(|_| {
                        ProviderError::Other("invalid producing-code metadata".into())
                    })?;
                for code in locations {
                    let mapping = CodeMapping::new(
                        AssetKey::new(project, environment, &node.unique_id),
                        code,
                    );
                    entries.push(mapping);
                }
            }
        }
        let mappings = DeclaredMappings::new(entries);
        // Validate the entire document even if no caller requests a mapped node.
        mappings.resolve(&AssetKey::new(project, environment, "validation"), None)?;
        Ok(Self(mappings))
    }
    /// Wraps a generic mapping document with the same source contract.
    /// Validation errors are returned when resolving an asset.
    #[must_use]
    pub fn from_mappings(mappings: DeclaredMappings) -> Self {
        Self(mappings)
    }
}
impl Provider for DbtProducingCode {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            "dbt",
            "producing_code",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}
impl ProducingCodeSource for DbtProducingCode {
    fn producing_code(
        &self,
        asset: &AssetKey,
        producing_run: Option<&str>,
    ) -> Result<CodeResolution, ProviderError> {
        self.0.resolve(asset, producing_run)
    }
}
