//! Content-addressed cache of per-model lineage.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use ods_core::RelationName;
use ods_sdk::contracts::sql_lineage::QueryLineage;
use sha2::{Digest, Sha256};

/// Stores analysis results by [`cache_key`]. Implementations must be safe to share
/// between the analysis threads.
pub trait LineageCache: Sync {
    /// The cached result for `key`.
    fn get(&self, key: &str) -> Option<QueryLineage>;
    /// Stores a result.
    fn put(&self, key: String, lineage: QueryLineage);
}

/// An in-memory cache.
#[derive(Debug, Default)]
pub struct MemoryCache(Mutex<HashMap<String, QueryLineage>>);

impl MemoryCache {
    /// Number of cached results.
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl LineageCache for MemoryCache {
    fn get(&self, key: &str) -> Option<QueryLineage> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    fn put(&self, key: String, lineage: QueryLineage) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, lineage);
    }
}

/// The key a model's lineage is cached under: everything the result depends on.
///
/// A change to the analyzer, the SQL, or the columns of any upstream relation produces a
/// new key, so a stale result is never reused. `upstream` must be sorted by relation
/// (a `BTreeMap` iteration is).
pub fn cache_key<'a>(
    analyzer_version: &str,
    sql: &str,
    upstream: impl IntoIterator<Item = (&'a RelationName, Option<&'a [String]>)>,
) -> String {
    let mut hasher = Sha256::new();
    // Length-prefix every field so different splits of the same bytes can't collide.
    let mut field = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    field(b"ods-lineage/1");
    field(analyzer_version.as_bytes());
    field(sql.as_bytes());
    for (relation, columns) in upstream {
        field(relation.to_string().as_bytes());
        match columns {
            None => field(b"<unknown>"),
            Some(columns) => {
                field(&(columns.len() as u64).to_le_bytes());
                for column in columns {
                    field(column.as_bytes());
                }
            }
        }
    }
    hex(&hasher.finalize())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_change_with_every_input() {
        let rel = RelationName::new(["db", "orders"]).unwrap();
        let cols = vec!["id".to_owned()];
        let base = cache_key("a1", "select 1", [(&rel, Some(cols.as_slice()))]);
        assert_eq!(
            base,
            cache_key("a1", "select 1", [(&rel, Some(cols.as_slice()))])
        );
        assert_ne!(
            base,
            cache_key("a2", "select 1", [(&rel, Some(cols.as_slice()))])
        );
        assert_ne!(
            base,
            cache_key("a1", "select 2", [(&rel, Some(cols.as_slice()))])
        );
        assert_ne!(base, cache_key("a1", "select 1", [(&rel, None)]));
        let more = vec!["id".to_owned(), "x".to_owned()];
        assert_ne!(
            base,
            cache_key("a1", "select 1", [(&rel, Some(more.as_slice()))])
        );
        assert_eq!(base.len(), 64);
    }
}
