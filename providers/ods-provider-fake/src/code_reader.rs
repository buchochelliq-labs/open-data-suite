//! Immutable in-memory source document for retrieval conformance.
use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_sdk::contracts::{
    code_reader::{CodeDocument, CodeReadError, CodeReader},
    producing_code::{CodeLocation, Revision},
};
use ods_sdk::{Provider, ProviderInfo};

/// One approved document, never backed by a file or network request.
pub struct FakeCodeReader(pub CodeDocument);
impl Provider for FakeCodeReader {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            crate::KIND,
            "code_reader",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}
#[async_trait]
impl CodeReader for FakeCodeReader {
    async fn verify_read_access(
        &self,
        _location: &CodeLocation,
    ) -> Result<ods_sdk::contracts::code_reader::CodeReadAccess, CodeReadError> {
        Ok(ods_sdk::contracts::code_reader::CodeReadAccess::Unauthenticated)
    }

    async fn read_code(
        &self,
        location: &CodeLocation,
        run: Option<&str>,
    ) -> Result<CodeDocument, CodeReadError> {
        location
            .validate()
            .map_err(|_| CodeReadError::InvalidRequest)?;
        let Revision::Exact { run: expected, .. } = &location.revision else {
            return Err(CodeReadError::RevisionRequired);
        };
        if Some(expected.as_str()) != run {
            return Err(CodeReadError::RevisionMismatch);
        }
        if location != self.0.location() {
            return Err(CodeReadError::PathMissing);
        }
        Ok(self.0.clone())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn conforms() {
        let location = CodeLocation::new(
            "repo",
            "load.sql",
            Revision::Exact {
                commit: "a".repeat(40),
                run: "r".into(),
            },
        );
        let reader = FakeCodeReader(CodeDocument::new(
            location.clone(),
            "a".repeat(40),
            "select 1".into(),
        ));
        ods_sdk::conformance::code_reader::check(&reader, &location, "r", "select 1").await;
    }
}
