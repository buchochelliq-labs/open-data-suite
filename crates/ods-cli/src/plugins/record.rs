//! What an out-of-process warehouse plugin answered, kept beside the state store for the
//! surfaces that never start one (ADR-0031 §3d): the dashboard and the MCP server. The
//! CLI commands that run a project's plugin write it; the newest answers win.
//!
//! `<state db>.plugins/<scope digest>.json`, one file per scope, written whole and
//! renamed into place. Nothing in it is a secret: links are built from settings
//! without secrets, and an error summary has its values removed already.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ods_core::SchemaVersion;
use ods_core::state::Timestamp;
use ods_sdk::contracts::error_catalogue::{CatalogueInfo, Classification};
use ods_sdk::contracts::run_events::ErrorSummary;
use ods_sdk::protocol::LinkOutcome;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The record's format version.
pub const PLUGIN_RECORD_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// How many classified errors a record keeps: the newest.
pub const CLASSIFICATIONS_KEPT: usize = 500;

/// One error a plugin classified, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Classified {
    /// The error, as summarised (values removed).
    pub error: ErrorSummary,
    /// What the plugin made of it.
    pub classification: Classification,
    /// When it was asked.
    pub at: Timestamp,
}

/// What a project's out-of-process warehouse plugin answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PluginRecord {
    /// [`PLUGIN_RECORD_VERSION`] when written.
    pub schema_version: SchemaVersion,
    /// The state scope it is for.
    pub scope: String,
    /// The plugin, as `[plugins.<name>]` names it.
    pub plugin: String,
    /// The name it described itself with.
    pub name: String,
    /// The version it described itself with.
    pub version: String,
    /// The warehouse kind it serves.
    pub warehouse: String,
    /// Its SQL dialect, if it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
    /// Its error catalogue, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalogue: Option<CatalogueInfo>,
    /// The links of the project's relations, by relation, as last asked.
    #[serde(default)]
    pub links: BTreeMap<String, LinkOutcome>,
    /// When the links were asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links_at: Option<Timestamp>,
    /// The errors it classified, oldest first, at most [`CLASSIFICATIONS_KEPT`].
    #[serde(default)]
    pub classifications: Vec<Classified>,
}

impl PluginRecord {
    /// An empty record of `plugin`, which described itself as `name` at `version`, for
    /// `warehouse` in `scope`.
    pub fn new(
        scope: impl Into<String>,
        plugin: impl Into<String>,
        (name, version): (impl Into<String>, impl Into<String>),
        warehouse: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: PLUGIN_RECORD_VERSION,
            scope: scope.into(),
            plugin: plugin.into(),
            name: name.into(),
            version: version.into(),
            warehouse: warehouse.into(),
            dialect: None,
            catalogue: None,
            links: BTreeMap::new(),
            links_at: None,
            classifications: Vec::new(),
        }
    }

    /// Whether it holds a classification of `error`.
    pub fn classified(&self, error: &ErrorSummary) -> bool {
        self.classifications.iter().any(|c| &c.error == error)
    }

    /// Adds `answers`, replacing an earlier classification of the same error, and keeps
    /// the newest [`CLASSIFICATIONS_KEPT`].
    pub fn add_classifications(
        &mut self,
        answers: impl IntoIterator<Item = (ErrorSummary, Classification)>,
        at: Timestamp,
    ) {
        for (error, classification) in answers {
            self.classifications.retain(|c| c.error != error);
            self.classifications.push(Classified {
                error,
                classification,
                at,
            });
        }
        let extra = self
            .classifications
            .len()
            .saturating_sub(CLASSIFICATIONS_KEPT);
        self.classifications.drain(..extra);
    }
}

/// Where the records of the state database at `state_db` are kept.
pub fn dir_for(state_db: &Path) -> PathBuf {
    let mut name = state_db.as_os_str().to_owned();
    name.push(".plugins");
    PathBuf::from(name)
}

/// The file of `scope`'s record in `dir`: a digest, as a scope can hold any character.
fn file_for(dir: &Path, scope: &str) -> PathBuf {
    let digest = hex::encode(Sha256::digest(scope.as_bytes()));
    dir.join(format!("{}.json", &digest[..16]))
}

/// `scope`'s record in `dir`, if there is one this version reads.
pub fn read(dir: &Path, scope: &str) -> Option<PluginRecord> {
    let path = file_for(dir, scope);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(path = %path.display(), "can't read a plugin record: {e}");
            return None;
        }
    };
    let record: PluginRecord = serde_json::from_slice(&bytes)
        .inspect_err(|e| tracing::warn!(path = %path.display(), "can't read a plugin record: {e}"))
        .ok()?;
    (PLUGIN_RECORD_VERSION.can_read(record.schema_version) && record.scope == scope)
        .then_some(record)
}

/// Every record in `dir` this version reads, whatever its scope.
pub fn read_all(dir: &Path) -> Vec<PluginRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut records: Vec<PluginRecord> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            let record: PluginRecord = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
            PLUGIN_RECORD_VERSION
                .can_read(record.schema_version)
                .then_some(record)
        })
        .collect();
    records.sort_by(|a, b| a.scope.cmp(&b.scope));
    records
}

/// Writes `record` into `dir`, whole, then renamed into place: a reader never sees half
/// of it, and a failed write leaves the old one. Returns where.
///
/// # Errors
/// The directory or file can't be written.
pub fn write(dir: &Path, record: &PluginRecord) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let mut json = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    json.push(b'\n');
    let mut partial = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut partial, &json)?;
    let path = file_for(dir, &record.scope);
    partial.persist(&path).map_err(|e| e.error)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ods_core::failure::ErrorCategory;

    fn summary(message: &str) -> ErrorSummary {
        ErrorSummary::from_message(message).unwrap()
    }

    fn not_recognised() -> Classification {
        Classification::NotRecognised {
            category: ErrorCategory::Unknown,
        }
    }

    #[test]
    fn a_record_round_trips_per_scope_and_keeps_the_newest_classifications() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = PluginRecord::new("dev", "acme", ("acme", "1.2.0"), "acme_db");
        record.links.insert(
            "\"shop\".\"main\".\"orders\"".to_owned(),
            LinkOutcome::Link(ods_sdk::contracts::relation_link::RelationLink::new(
                "https://acme.example/t/orders",
                "Open in Acme",
            )),
        );
        let at = Timestamp::now();
        // Distinct messages, in letters: a summary drops numbers.
        let word = |mut i: usize| {
            let mut word = String::new();
            loop {
                word.push(char::from(b'a' + u8::try_from(i % 26).unwrap()));
                i /= 26;
                if i == 0 {
                    return word;
                }
            }
        };
        record.add_classifications(
            (0..CLASSIFICATIONS_KEPT + 2).map(|i| {
                (
                    summary(&format!("Acme Error: case {}", word(i))),
                    not_recognised(),
                )
            }),
            at,
        );
        assert_eq!(record.classifications.len(), CLASSIFICATIONS_KEPT);
        // The oldest went.
        assert!(!record.classified(&summary("Acme Error: case a")));
        // Asking again about an error replaces its classification, at the end.
        let again = record.classifications[0].error.clone();
        record.add_classifications([(again.clone(), not_recognised())], at);
        assert_eq!(record.classifications.last().unwrap().error, again);
        assert_eq!(record.classifications.len(), CLASSIFICATIONS_KEPT);

        write(&dir.path().join("x.plugins"), &record).unwrap();
        let back = read(&dir.path().join("x.plugins"), "dev").unwrap();
        assert_eq!(back, record);
        // Another scope has its own record, and none yet.
        assert!(read(&dir.path().join("x.plugins"), "prod").is_none());
    }
}
