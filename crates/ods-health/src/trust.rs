//! The trust store (ADR-0030 §4b, §4d): which of a project's probe (and, later, script)
//! definitions the user has reviewed and allowed to run. A repository's `ods.toml` can
//! name queries and commands, and cloning it mustn't be enough to run them, so each
//! project's definitions are trusted one by one, by digest: a changed definition is
//! untrusted again until it is reviewed.
//!
//! The store lives in the user's own configuration directory, beside `config.toml`, never
//! in a repository. It is written whole, then renamed into place, and only by
//! `ods health trust`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ods_core::SchemaVersion;
use ods_core::state::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ProbeDefinition;

/// The trust store's format version.
pub const TRUST_STORE_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// The store's file name, in the user's configuration directory.
pub const FILE_NAME: &str = "trust.json";

/// Where the store is, beside the user's own `config.toml`.
pub fn path_beside(user_config: &Path) -> PathBuf {
    user_config.with_file_name(FILE_NAME)
}

/// The store couldn't be read or written.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// It couldn't be read.
    #[error("the trust store {path} can't be read: {why}")]
    Unreadable {
        /// Where it is.
        path: PathBuf,
        /// Why.
        why: String,
    },
    /// A newer ODS wrote it.
    #[error(
        "the trust store {path} is version {}.{}, newer than this ODS reads ({}.{})",
        found.major,
        found.minor,
        TRUST_STORE_VERSION.major,
        TRUST_STORE_VERSION.minor
    )]
    Newer {
        /// Where it is.
        path: PathBuf,
        /// Its version.
        found: SchemaVersion,
    },
    /// It couldn't be written.
    #[error("the trust store {path} can't be written: {why}")]
    Unwritable {
        /// Where it is.
        path: PathBuf,
        /// Why.
        why: String,
    },
}

/// One project's trusted definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProjectTrust {
    /// When they were trusted.
    pub trusted_at: Timestamp,
    /// Each trusted check's digest, by id.
    pub checks: BTreeMap<String, String>,
}

/// The trust store: each project's trusted definitions, by the canonical path of the
/// directory its configuration is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TrustStore {
    /// [`TRUST_STORE_VERSION`] when written.
    pub schema_version: SchemaVersion,
    /// Each project's entry.
    pub projects: BTreeMap<String, ProjectTrust>,
}

impl Default for TrustStore {
    fn default() -> Self {
        Self {
            schema_version: TRUST_STORE_VERSION,
            projects: BTreeMap::new(),
        }
    }
}

/// How a definition stands against the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Standing {
    /// Trusted as it is now.
    Trusted,
    /// Trusted before, but it has changed since.
    Changed,
    /// Never trusted for this project.
    New,
}

impl TrustStore {
    /// Reads the store at `path`; an empty store when there is none yet.
    ///
    /// # Errors
    /// It exists but can't be read, isn't a trust store, or a newer ODS wrote it.
    pub fn read(path: &Path) -> Result<Self, TrustError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(TrustError::Unreadable {
                    path: path.to_owned(),
                    why: e.to_string(),
                });
            }
        };
        let store: Self = serde_json::from_slice(&bytes).map_err(|e| TrustError::Unreadable {
            path: path.to_owned(),
            why: e.to_string(),
        })?;
        if !TRUST_STORE_VERSION.can_read(store.schema_version) {
            return Err(TrustError::Newer {
                path: path.to_owned(),
                found: store.schema_version,
            });
        }
        Ok(store)
    }

    /// Writes the store to `path`, whole, then renamed into place: a reader never sees
    /// half of it, and a failed write leaves the old one.
    ///
    /// # Errors
    /// The directory or file can't be written.
    pub fn write(&self, path: &Path) -> Result<(), TrustError> {
        let unwritable = |why: String| TrustError::Unwritable {
            path: path.to_owned(),
            why,
        };
        let dir = path
            .parent()
            .ok_or_else(|| unwritable("it has no directory".to_owned()))?;
        std::fs::create_dir_all(dir).map_err(|e| unwritable(e.to_string()))?;
        let mut json = serde_json::to_vec_pretty(self).map_err(|e| unwritable(e.to_string()))?;
        json.push(b'\n');
        let mut partial =
            tempfile::NamedTempFile::new_in(dir).map_err(|e| unwritable(e.to_string()))?;
        std::io::Write::write_all(&mut partial, &json).map_err(|e| unwritable(e.to_string()))?;
        partial
            .persist(path)
            .map_err(|e| unwritable(e.error.to_string()))?;
        Ok(())
    }

    /// How each of `definitions` stands for the project at `root`.
    pub fn standing(
        &self,
        root: &str,
        definitions: &[ProbeDefinition],
    ) -> BTreeMap<String, Standing> {
        let entry = self.projects.get(root);
        definitions
            .iter()
            .map(|d| {
                let standing = match entry.and_then(|e| e.checks.get(&d.id)) {
                    Some(digest) if *digest == d.digest => Standing::Trusted,
                    Some(_) => Standing::Changed,
                    None => Standing::New,
                };
                (d.id.clone(), standing)
            })
            .collect()
    }

    /// The ids of `definitions` trusted as they are now for the project at `root`.
    pub fn trusted(&self, root: &str, definitions: &[ProbeDefinition]) -> BTreeSet<String> {
        self.standing(root, definitions)
            .into_iter()
            .filter(|(_, s)| *s == Standing::Trusted)
            .map(|(id, _)| id)
            .collect()
    }

    /// Trusts `definitions` as they are now for the project at `root`, replacing what it
    /// trusted before: a definition no longer configured is no longer trusted.
    pub fn trust(&mut self, root: &str, definitions: &[ProbeDefinition], at: Timestamp) {
        self.projects.insert(
            root.to_owned(),
            ProjectTrust {
                trusted_at: at,
                checks: definitions
                    .iter()
                    .map(|d| (d.id.clone(), d.digest.clone()))
                    .collect(),
            },
        );
        self.schema_version = TRUST_STORE_VERSION;
    }

    /// Forgets the project at `root`; whether it was there.
    pub fn revoke(&mut self, root: &str) -> bool {
        self.projects.remove(root).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(id: &str, digest: &str) -> ProbeDefinition {
        ProbeDefinition {
            id: id.to_owned(),
            sql: "select 1 as n from {relation}".into(),
            digest: digest.to_owned(),
        }
    }

    fn at() -> Timestamp {
        Timestamp::parse("2026-10-06T09:00:00Z").unwrap()
    }

    #[test]
    fn trust_is_per_project_and_per_digest() {
        let mut store = TrustStore::default();
        let defs = [definition("a", "sha256:1"), definition("b", "sha256:2")];
        assert!(
            store.trusted("/p", &defs).is_empty(),
            "nothing is trusted at first"
        );
        store.trust("/p", &defs, at());
        assert_eq!(store.trusted("/p", &defs).len(), 2);
        assert!(
            store.trusted("/other", &defs).is_empty(),
            "another project isn't"
        );
        let changed = [
            definition("a", "sha256:9"),
            definition("b", "sha256:2"),
            definition("c", "sha256:3"),
        ];
        assert_eq!(
            store.standing("/p", &changed),
            BTreeMap::from([
                ("a".to_owned(), Standing::Changed),
                ("b".to_owned(), Standing::Trusted),
                ("c".to_owned(), Standing::New),
            ])
        );
        assert!(store.revoke("/p"));
        assert!(!store.revoke("/p"));
        assert!(store.trusted("/p", &defs).is_empty());
    }

    #[test]
    fn it_round_trips_and_a_missing_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ods").join(FILE_NAME);
        assert_eq!(TrustStore::read(&path).unwrap(), TrustStore::default());
        let mut store = TrustStore::default();
        store.trust("/p", &[definition("a", "sha256:1")], at());
        store.write(&path).unwrap();
        assert_eq!(TrustStore::read(&path).unwrap(), store);
        assert_eq!(
            path_beside(&dir.path().join("ods/config.toml")),
            dir.path().join("ods/trust.json")
        );
    }

    #[test]
    fn an_unreadable_or_newer_store_is_an_error_never_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "{").unwrap();
        assert!(matches!(
            TrustStore::read(&path),
            Err(TrustError::Unreadable { .. })
        ));
        let newer = TrustStore {
            schema_version: SchemaVersion::new(2, 0),
            projects: BTreeMap::new(),
        };
        std::fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();
        assert!(matches!(
            TrustStore::read(&path),
            Err(TrustError::Newer { .. })
        ));
    }
}
