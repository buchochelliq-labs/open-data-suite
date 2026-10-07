//! Saved source-version readings (#388, ADR-0022 §7).
//!
//! Every command that reads sources' data versions from the warehouse (`ods state
//! build` and `run`, and their `--dry-run`) saves the reading it took in
//! `<state-db>.versions.json`, beside the store: the latest reading per state scope. The
//! commands that don't connect (`ods state plan`, `explain` and `ods serve`) add it to
//! what they know, so they see the versions a run would see, as of when they were read.
//!
//! The file isn't the store: nothing in it is canonical state (AGENTS.md rule 5). An
//! old reading needs no rule of its own: the planner already treats a version observed
//! before a node's last build as saying nothing about data since.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ods_core::state::{DataVersion, Timestamp};
use ods_core::{Capability, CapabilitySet, SchemaVersion};
use ods_state::{VersionAnswer, VersionReading};
use serde::{Deserialize, Serialize};

/// The version of the readings file this build writes and reads.
const READINGS_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// The file: the latest reading for each state scope.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ReadingsFile {
    schema_version: SchemaVersion,
    /// Scope (`<project>/<environment>`) → its latest reading.
    readings: BTreeMap<String, Saved>,
}

/// One reading, as saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Saved {
    /// When the reading started.
    read_at: Option<Timestamp>,
    /// The command that took it, for people, e.g. `ods state build --dry-run`.
    command: String,
    /// What the reader can do, by name, e.g. `relation_versions`.
    capabilities: Vec<String>,
    /// Source id → its answer.
    answers: BTreeMap<String, Answer>,
}

/// A source's answer, as saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Answer {
    /// Its data version.
    Version(DataVersion),
    /// Why it couldn't be read.
    Unknown(String),
}

/// A saved reading, loaded for a scope: the reading, and who took it when.
#[derive(Debug, Clone)]
pub(super) struct Loaded {
    pub reading: VersionReading,
    /// When it was read.
    pub read_at: Option<Timestamp>,
    /// The command that read it, e.g. `ods state build --dry-run`.
    pub command: String,
}

/// Where the readings for `state_db` are kept.
pub(super) fn path_for(state_db: &Path) -> PathBuf {
    let mut name = state_db.as_os_str().to_owned();
    name.push(".versions.json");
    PathBuf::from(name)
}

/// Saves `reading`, taken by `command`, as `scope`'s latest. Other scopes' readings are
/// kept, and so is a reading of this scope taken later (a slower probe that finishes
/// last mustn't replace a newer one). A file a newer ODS wrote is left alone. Commands
/// sharing a state database update the file one at a time, under a lock beside it.
///
/// # Errors
/// The file can't be written, or a newer ODS wrote it: the reading isn't saved, and
/// nothing else changes.
pub(super) fn save(
    state_db: &Path,
    scope: &str,
    command: &str,
    reading: &VersionReading,
) -> Result<(), String> {
    let path = path_for(state_db);
    // Held until it returns: read, merge and write are one step for every command.
    let _lock = lock(&path).map_err(|e| format!("can't lock {}: {e}", path.display()))?;
    let mut file = match read(&path) {
        Ok(Some(file)) => file,
        // A file that can't be read is replaced: it holds nothing this ODS can use.
        Ok(None) | Err(Unreadable::Damaged(_)) => ReadingsFile {
            schema_version: READINGS_VERSION,
            readings: BTreeMap::new(),
        },
        Err(Unreadable::Newer) => {
            return Err(format!(
                "{} was written by a newer ODS; this reading isn't saved",
                path.display()
            ));
        }
        Err(Unreadable::Io(e)) => return Err(format!("can't read {}: {e}", path.display())),
    };
    let newer_kept = file.readings.get(scope).is_some_and(|kept| {
        matches!((kept.read_at, reading.observed_at), (Some(kept), Some(this)) if kept > this)
    });
    if newer_kept {
        return Ok(());
    }
    file.schema_version = READINGS_VERSION;
    file.readings.insert(
        scope.to_owned(),
        Saved {
            read_at: reading.observed_at,
            command: command.to_owned(),
            capabilities: reading
                .capabilities
                .iter()
                .map(|c| c.name().to_owned())
                .collect(),
            answers: reading
                .answers
                .iter()
                .filter_map(|(id, answer)| {
                    let answer = match answer {
                        VersionAnswer::Version(v) => Answer::Version(v.clone()),
                        VersionAnswer::Unknown(why) => Answer::Unknown(why.clone()),
                        // An answer this build doesn't know isn't saved: it reads unknown.
                        _ => return None,
                    };
                    Some((id.clone(), answer))
                })
                .collect(),
        },
    );
    write(&path, &file).map_err(|e| format!("can't write {}: {e}", path.display()))
}

/// `scope`'s saved reading, if there is one this ODS can read. A file it can't read is
/// skipped with a warning in `warnings`: the commands that load it work without it.
pub(super) fn load(state_db: &Path, scope: &str, warnings: &mut Vec<String>) -> Option<Loaded> {
    let path = path_for(state_db);
    let file = match read(&path) {
        Ok(file) => file?,
        Err(Unreadable::Newer) => {
            warnings.push(format!(
                "the saved table versions in {} were written by a newer ODS: not used",
                path.display()
            ));
            return None;
        }
        Err(Unreadable::Damaged(e) | Unreadable::Io(e)) => {
            warnings.push(format!(
                "can't read the saved table versions in {}: {e}; not used",
                path.display()
            ));
            return None;
        }
    };
    let saved = file.readings.get(scope)?.clone();
    // A capability this build doesn't know serves no strategy it has: left out.
    let capabilities: CapabilitySet = saved
        .capabilities
        .iter()
        .filter_map(|c| c.parse::<Capability>().ok())
        .collect();
    let answers = saved
        .answers
        .into_iter()
        .map(|(id, answer)| {
            let answer = match answer {
                Answer::Version(v) => VersionAnswer::Version(v),
                Answer::Unknown(why) => VersionAnswer::Unknown(why),
            };
            (id, answer)
        })
        .collect();
    Some(Loaded {
        reading: VersionReading::new(capabilities, saved.read_at, answers),
        read_at: saved.read_at,
        command: saved.command,
    })
}

enum Unreadable {
    /// A newer ODS wrote it.
    Newer,
    /// It isn't a readings file this ODS understands.
    Damaged(String),
    Io(String),
}

/// An exclusive lock on `<path>.lock`, released when dropped. The lock file stays: it
/// holds nothing, and removing it could let two commands lock different files.
fn lock(path: &Path) -> std::io::Result<std::fs::File> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(name))?;
    file.lock()?;
    Ok(file)
}

/// Just a file's version, read before the rest.
#[derive(Deserialize)]
struct Versioned {
    schema_version: SchemaVersion,
}

/// The file at `path`; `None` when there is none.
fn read(path: &Path) -> Result<Option<ReadingsFile>, Unreadable> {
    let text = match std::fs::read(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Unreadable::Io(e.to_string())),
    };
    // The version first, so a newer file whose shape changed is reported as newer.
    let versioned: Versioned =
        serde_json::from_slice(&text).map_err(|e| Unreadable::Damaged(e.to_string()))?;
    if !READINGS_VERSION.can_read(versioned.schema_version) {
        return Err(Unreadable::Newer);
    }
    serde_json::from_slice(&text)
        .map(Some)
        .map_err(|e| Unreadable::Damaged(e.to_string()))
}

fn write(path: &Path, file: &ReadingsFile) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    // Written whole to a temporary file beside it, then renamed: a reader never sees
    // half of it, and a failed write leaves the old file as it was.
    let mut partial = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut partial, &json)?;
    partial.persist(path).map(drop).map_err(|e| e.error)
}

#[cfg(test)]
mod tests {
    use ods_core::state::Exactness;

    use super::*;

    fn reading(at: i64, version: &str) -> VersionReading {
        VersionReading::new(
            [Capability::RelationVersions].into_iter().collect(),
            Some(Timestamp::from_unix(at)),
            [
                (
                    "source.shop.app.orders".to_owned(),
                    VersionAnswer::Version(DataVersion::new(
                        version,
                        Exactness::Exact,
                        "delta_history",
                    )),
                ),
                (
                    "source.shop.app.events".to_owned(),
                    VersionAnswer::Unknown("not a Delta table".to_owned()),
                ),
            ]
            .into(),
        )
    }

    #[test]
    fn a_saved_reading_loads_as_it_was_read_for_its_scope_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        save(
            &db,
            "shop/dev",
            "ods state build --dry-run",
            &reading(100, "7"),
        )
        .unwrap();
        save(&db, "shop/prod", "ods state run", &reading(200, "9")).unwrap();
        let mut warnings = Vec::new();
        let dev = load(&db, "shop/dev", &mut warnings).unwrap();
        assert_eq!(dev.reading, reading(100, "7"));
        assert_eq!(dev.command, "ods state build --dry-run");
        assert_eq!(dev.read_at, Some(Timestamp::from_unix(100)));
        // A newer reading replaces only its own scope's.
        save(&db, "shop/dev", "ods state build", &reading(300, "8")).unwrap();
        assert_eq!(
            load(&db, "shop/dev", &mut warnings).unwrap().reading,
            reading(300, "8")
        );
        assert_eq!(
            load(&db, "shop/prod", &mut warnings).unwrap().reading,
            reading(200, "9")
        );
        assert!(load(&db, "other/dev", &mut warnings).is_none());
        assert_eq!(warnings, Vec::<String>::new());
    }

    #[test]
    fn a_later_reading_is_never_replaced_by_an_earlier_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        save(&db, "shop/dev", "ods state build", &reading(300, "8")).unwrap();
        // A slower probe, started earlier, finishing last.
        save(
            &db,
            "shop/dev",
            "ods state build --dry-run",
            &reading(100, "7"),
        )
        .unwrap();
        let kept = load(&db, "shop/dev", &mut Vec::new()).unwrap();
        assert_eq!(
            (kept.reading, kept.command.as_str()),
            (reading(300, "8"), "ods state build")
        );
    }

    #[test]
    fn commands_saving_at_once_keep_every_scope() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let scopes: Vec<String> = (0..16).map(|i| format!("shop/env{i}")).collect();
        std::thread::scope(|s| {
            for (i, scope) in scopes.iter().enumerate() {
                let db = &db;
                s.spawn(move || {
                    let at = i64::try_from(i).unwrap() + 1;
                    save(db, scope, "ods state build", &reading(at, &i.to_string())).unwrap();
                });
            }
        });
        for scope in &scopes {
            assert!(
                load(&db, scope, &mut Vec::new()).is_some(),
                "{scope} was lost"
            );
        }
    }

    #[test]
    fn a_file_it_cant_read_is_skipped_and_a_newer_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let path = path_for(&db);
        let mut warnings = Vec::new();
        assert!(
            load(&db, "shop/dev", &mut warnings).is_none(),
            "no file, nothing said"
        );
        assert_eq!(warnings, Vec::<String>::new());

        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&db, "shop/dev", &mut warnings).is_none());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        // A damaged file is replaced by the next reading.
        save(&db, "shop/dev", "ods state build", &reading(1, "1")).unwrap();
        assert!(load(&db, "shop/dev", &mut warnings).is_some());

        let newer = r#"{"schema_version":{"major":2,"minor":0},"readings":{}}"#;
        std::fs::write(&path, newer).unwrap();
        warnings.clear();
        assert!(load(&db, "shop/dev", &mut warnings).is_none());
        assert!(warnings[0].contains("newer ODS"), "{warnings:?}");
        assert!(save(&db, "shop/dev", "ods state build", &reading(2, "2")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer, "left alone");
    }
}
