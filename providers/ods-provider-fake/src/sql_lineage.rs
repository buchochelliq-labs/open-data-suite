//! A scripted [`SqlLineageAnalyzer`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use ods_core::{CapabilitySet, RelationName};
use ods_sdk::contracts::sql_lineage::{AnalyzeRequest, QueryLineage, SqlLineageAnalyzer};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;

/// Returns scripted lineage for known SQL texts and an opaque result for anything else,
/// so tests of lineage consumers don't depend on a real parser.
///
/// Relation names are dot-separated and compared as written; column names are used as
/// written.
#[derive(Debug, Default)]
pub struct FakeSqlLineageAnalyzer {
    scripted: BTreeMap<String, QueryLineage>,
    calls: AtomicUsize,
}

impl FakeSqlLineageAnalyzer {
    /// An analyzer with no scripted queries.
    pub fn new() -> Self {
        Self::default()
    }

    /// Scripts the result for `sql`.
    #[must_use]
    pub fn with(mut self, sql: impl Into<String>, lineage: QueryLineage) -> Self {
        self.scripted.insert(sql.into(), lineage);
        self
    }

    /// How many times [`SqlLineageAnalyzer::analyze`] was called.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Provider for FakeSqlLineageAnalyzer {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "fake",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

impl SqlLineageAnalyzer for FakeSqlLineageAnalyzer {
    fn analyzer_version(&self) -> String {
        "fake/1".to_owned()
    }

    fn relation_name(&self, qualified: &str) -> Result<RelationName, ProviderError> {
        RelationName::new(qualified.split('.')).map_err(ProviderError::Other)
    }

    fn column_name(&self, name: &str) -> String {
        name.to_owned()
    }

    fn analyze(&self, request: &AnalyzeRequest<'_>) -> Result<QueryLineage, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.scripted.get(request.sql).cloned().unwrap_or_else(|| {
            QueryLineage::opaque(
                std::collections::BTreeSet::new(),
                "the fake has no script for this SQL",
            )
        }))
    }
}
