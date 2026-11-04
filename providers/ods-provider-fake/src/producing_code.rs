//! Mapping-backed reference implementation of producing-code lookup.
use ods_core::CapabilitySet;
use ods_sdk::contracts::producing_code::{
    AssetKey, CodeResolution, DeclaredMappings, ProducingCodeSource,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

/// Returns declared references scoped by project, environment and node.
#[derive(Debug, Clone)]
pub struct FakeProducingCodeSource(pub DeclaredMappings);
impl Provider for FakeProducingCodeSource {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            crate::KIND,
            "producing_code",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}
impl ProducingCodeSource for FakeProducingCodeSource {
    fn producing_code(
        &self,
        asset: &AssetKey,
        run: Option<&str>,
    ) -> Result<CodeResolution, ProviderError> {
        self.0.resolve(asset, run)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conforms() {
        let mappings = serde_json::from_value(serde_json::json!({"schema_version":{"major":0,"minor":1}, "entries":[{"asset":{"project":"p","environment":"prod","node":"raw"},"code":{"repository":"ingestion","path":"load.py","revision":{"kind":"unknown"}}}]})).unwrap();
        ods_sdk::conformance::producing_code::check(
            &FakeProducingCodeSource(mappings),
            &AssetKey::new("p", "prod", "raw"),
        );
    }
}
