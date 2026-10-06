//! The health record (ADR-0030 §6): each `ods health check`'s report, kept beside the
//! state store as `<state db>.health/<time>-<n>.json`, so the dashboard and #117 read what
//! was checked without running anything. Each file is written whole, then renamed into
//! place, and only the newest [`KEEP`] are kept: a check run that fails never touches
//! the records already there (AGENTS rule 5).

use std::path::{Path, PathBuf};

use ods_core::SchemaVersion;
use ods_core::state::Timestamp;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::{CheckRun, CheckSource, Finding, HealthReport};

/// The health record's format version.
/// 1.1: a check's source can be `declarative` or `probe`, and a report holds its
/// coverage targets' verdicts.
pub const HEALTH_RECORD_VERSION: SchemaVersion = SchemaVersion::new(1, 1);

/// How many records are kept; older ones are removed after each write.
pub const KEEP: usize = 20;

/// How many records can have the same time, to the second.
const MAX_PER_SECOND: usize = 999;

/// One `ods health check`'s report, as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct HealthRecord {
    /// [`HEALTH_RECORD_VERSION`] when written.
    pub schema_version: SchemaVersion,
    /// When the checks ran.
    pub checked_at: Timestamp,
    /// The state scope checked, e.g. `shop/dev`: one directory serves every scope of
    /// its database.
    pub scope: String,
    /// What the checks found.
    pub report: HealthReport,
}

impl HealthRecord {
    /// A record of `report`, for `scope`, checked at `checked_at`.
    pub fn new(scope: impl Into<String>, checked_at: Timestamp, report: HealthReport) -> Self {
        Self {
            schema_version: HEALTH_RECORD_VERSION,
            checked_at,
            scope: scope.into(),
            report,
        }
    }
}

/// What a record says about the checks the dashboard doesn't run itself (ADR-0030 §6):
/// it works the built-in and declared checks out live, and takes every other check's
/// findings from the latest record, with that record's time.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Recorded {
    /// When the checks ran.
    pub checked_at: Timestamp,
    /// The recorded checks, in the order they ran.
    pub checks: Vec<CheckRun>,
    /// Their findings, by node.
    findings: BTreeMap<String, Vec<Finding>>,
}

impl Recorded {
    /// What `record` says about checks that aren't built in.
    pub fn of(record: &HealthRecord) -> Self {
        let recorded = |source: CheckSource| !source.is_live();
        Self {
            checked_at: record.checked_at,
            checks: record
                .report
                .checks
                .iter()
                .filter(|c| recorded(c.source))
                .cloned()
                .collect(),
            findings: record
                .report
                .badges
                .iter()
                .map(|(node, badge)| {
                    let findings = badge
                        .findings
                        .iter()
                        .filter(|f| recorded(f.source))
                        .cloned()
                        .collect();
                    (node.clone(), findings)
                })
                .collect(),
        }
    }

    /// Whether the record holds any check the dashboard doesn't run itself.
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }

    /// The recorded findings about `node`, one per recorded check. A check that has no
    /// finding about it (the node is newer than the record) is *unknown* for it, never
    /// a pass (AGENTS rule 3).
    pub(crate) fn findings_for(&self, node: &str) -> Vec<Finding> {
        let found = self.findings.get(node);
        self.checks
            .iter()
            .map(|check| {
                found
                    .and_then(|f| f.iter().find(|f| f.check == check.id))
                    .cloned()
                    .unwrap_or_else(|| Finding {
                        check: check.id.clone(),
                        source: check.source,
                        status: crate::Status::Unknown,
                        severity: check.severity,
                        reason: format!(
                            "`{}` didn't check it: the last `ods health check` ({}) didn't cover it",
                            check.id, self.checked_at
                        ),
                        evidence: BTreeMap::new(),
                    })
            })
            .collect()
    }
}

/// Where the records of the state database at `state_db` are kept.
pub fn dir_for(state_db: &Path) -> PathBuf {
    let mut name = state_db.as_os_str().to_owned();
    name.push(".health");
    PathBuf::from(name)
}

/// A file name for a record checked at `at`, that sorts by time: `2026-10-06T09-00-00Z`,
/// as `:` can't be in a file name everywhere.
fn stem(at: Timestamp) -> String {
    at.to_string().replace(':', "-")
}

/// The records in `dir`, newest first; a file whose name isn't a record's is ignored.
fn files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|e| e == "json")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.as_bytes().first().is_some_and(u8::is_ascii_digit))
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    files.sort();
    files.reverse();
    Ok(files)
}

/// Writes `record` into `dir`, then removes all but the newest [`KEEP`] records.
/// Returns where it was written.
///
/// # Errors
/// The directory or file can't be written. A record that couldn't be removed is left,
/// as it does no harm.
pub fn write(dir: &Path, record: &HealthRecord) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    let stem = stem(record.checked_at);
    // Written whole beside it, then renamed: a reader never sees half a record, and a
    // failed write leaves the others as they were.
    let mut partial = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut partial, &json)?;
    // Checks in the same second are `-001`, `-002`, …: fixed width, so they sort in
    // order. A name another check took first is skipped, never replaced.
    let mut written = None;
    for n in 1..=MAX_PER_SECOND {
        let path = dir.join(format!("{stem}-{n:03}.json"));
        match partial.persist_noclobber(&path) {
            Ok(_) => {
                written = Some(path);
                break;
            }
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => partial = e.file,
            Err(e) => return Err(e.error),
        }
    }
    let path = written.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{MAX_PER_SECOND} health records already have the time {stem}"),
        )
    })?;
    for old in files(dir)?.into_iter().skip(KEEP) {
        if let Err(e) = std::fs::remove_file(&old) {
            tracing::debug!(path = %old.display(), "can't remove an old health record: {e}");
        }
    }
    Ok(path)
}

/// The newest record in `dir` for `scope` that this version can read, if any.
pub fn latest(dir: &Path, scope: &str) -> Option<HealthRecord> {
    let files = files(dir)
        .inspect_err(|e| tracing::warn!(dir = %dir.display(), "can't list the health records: {e}"))
        .ok()?;
    files.iter().find_map(|path| {
        let text = std::fs::read(path).ok()?;
        let record: HealthRecord = serde_json::from_slice(&text)
            .inspect_err(
                |e| tracing::warn!(path = %path.display(), "can't read a health record: {e}"),
            )
            .ok()?;
        (HEALTH_RECORD_VERSION.can_read(record.schema_version) && record.scope == scope)
            .then_some(record)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn record(scope: &str, at: i64) -> HealthRecord {
        HealthRecord::new(
            scope,
            Timestamp::from_unix(1_790_000_000 + at),
            HealthReport {
                badges: BTreeMap::new(),
                checks: Vec::new(),
                coverage: Vec::new(),
                elevated_login: None,
            },
        )
    }

    #[test]
    fn records_beside_the_database() {
        assert_eq!(
            dir_for(Path::new("state/state.db")),
            Path::new("state/state.db.health")
        );
    }

    #[test]
    fn the_latest_is_per_scope_and_survives_a_same_second_write() {
        let dir = tempfile::tempdir().unwrap();
        let first = write(dir.path(), &record("shop/dev", 0)).unwrap();
        let again = write(dir.path(), &record("shop/prod", 0)).unwrap();
        assert_ne!(first, again);
        assert!(
            again
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with("-002.json")
        );
        assert_eq!(files(dir.path()).unwrap()[0], again, "newest first");
        assert_eq!(latest(dir.path(), "shop/dev").unwrap().scope, "shop/dev");
        assert_eq!(latest(dir.path(), "shop/prod").unwrap().scope, "shop/prod");
        assert!(latest(dir.path(), "shop/qa").is_none());
    }

    #[test]
    fn only_the_newest_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        for at in 0..i64::try_from(KEEP).unwrap() + 3 {
            write(dir.path(), &record("shop/dev", at * 60)).unwrap();
        }
        assert_eq!(files(dir.path()).unwrap().len(), KEEP);
        assert_eq!(
            latest(dir.path(), "shop/dev").unwrap().checked_at,
            record("shop/dev", (i64::try_from(KEEP).unwrap() + 2) * 60).checked_at
        );
    }

    #[test]
    fn unreadable_or_newer_records_are_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &record("shop/dev", 0)).unwrap();
        let mut newer = record("shop/dev", 60);
        newer.schema_version = SchemaVersion::new(2, 0);
        write(dir.path(), &newer).unwrap();
        std::fs::write(dir.path().join("9999-12-31T00-00-00Z-001.json"), "{").unwrap();
        assert_eq!(
            latest(dir.path(), "shop/dev").unwrap().checked_at,
            record("shop/dev", 0).checked_at
        );
        assert!(latest(&dir.path().join("missing"), "shop/dev").is_none());
    }
}
