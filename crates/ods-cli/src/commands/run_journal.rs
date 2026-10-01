//! The run journal (#322, ADR-0024): every event of a run that executes, appended as it
//! happens to `<state-db>.runs/<run_id>.jsonl`, one JSON [`RunEvent`] per line.
//!
//! The journal is evidence, not state (AGENTS.md rule 5): nothing reads it to decide
//! what to build, a failed run keeps its journal, and canonical state is still
//! committed only from the executor's report. Each line is flushed as its event
//! arrives, so a reader (`ods serve`) can follow a run while it goes. Events carry no
//! SQL, options or environment (rule 9); a failed node's error is only its redacted
//! summary.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use ods_sdk::contracts::run_events::{RunEvent, RunEventKind, RunEventSink};
// Reading and naming journals is shared with `ods serve` (ods-sdk, one reader).
use ods_sdk::run_journal::RECENT;
pub(super) use ods_sdk::run_journal::{KEEP, ReadJournal, dir_for, path_for};

/// Appends a run's events to its journal. Opens the file at `run_started`, never
/// overwriting one; if the journal can't be written, it says why once
/// ([`Self::warning`]) and the run goes on.
#[derive(Debug)]
pub(super) struct JournalSink {
    state_db: PathBuf,
    inner: Mutex<Journal>,
}

#[derive(Debug, Default)]
struct Journal {
    file: Option<File>,
    path: Option<PathBuf>,
    warning: Option<String>,
}

impl JournalSink {
    /// A journal for the next run recorded beside `state_db`.
    pub(super) fn new(state_db: &Path) -> Self {
        Self {
            state_db: state_db.to_owned(),
            inner: Mutex::default(),
        }
    }

    /// Where the journal was written, once the run started.
    pub(super) fn path(&self) -> Option<PathBuf> {
        self.lock().path.clone()
    }

    /// Why the journal couldn't be written, if it couldn't.
    pub(super) fn warning(&self) -> Option<String> {
        self.lock().warning.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Journal> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn open(&self, run_id: &str) -> Result<(File, PathBuf), String> {
        let Some(path) = path_for(&self.state_db, run_id) else {
            return Err(format!(
                "the run id `{}` can't name a file, so this run has no journal",
                run_id.escape_debug()
            ));
        };
        let dir = dir_for(&self.state_db);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("can't create `{}` for run journals: {e}", dir.display()))?;
        // Older journals go first, so the directory never holds more than `KEEP`.
        prune(&dir, KEEP.saturating_sub(1), std::time::SystemTime::now());
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("can't create the run journal `{}`: {e}", path.display()))?;
        Ok((file, path))
    }
}

impl RunEventSink for JournalSink {
    fn emit(&self, event: RunEvent) {
        let mut journal = self.lock();
        if journal.warning.is_some() {
            return;
        }
        if matches!(event.kind, RunEventKind::RunStarted { .. }) && journal.file.is_none() {
            match self.open(&event.run_id) {
                Ok((file, path)) => {
                    journal.file = Some(file);
                    journal.path = Some(path);
                }
                Err(why) => {
                    journal.warning = Some(why);
                    return;
                }
            }
        }
        let path = journal.path.clone().unwrap_or_default();
        let Some(file) = journal.file.as_mut() else {
            return;
        };
        let written = serde_json::to_string(&event)
            .map_err(std::io::Error::other)
            .and_then(|line| {
                file.write_all(format!("{line}\n").as_bytes())?;
                file.flush()
            });
        if let Err(e) = written {
            journal.warning = Some(format!(
                "the run journal `{}` couldn't be written, so it stops here: {e}",
                path.display()
            ));
            journal.file = None;
        }
    }
}

/// Deletes the oldest journals in `dir` until at most `keep` remain, never one changed
/// in the last [`RECENT`] (another run may still be writing it). Best effort: a
/// journal that can't be deleted is left.
fn prune(dir: &Path, keep: usize, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut journals: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        // The path's time, not the listing's: on Windows the listing's can lag a journal
        // another run is still writing, which would make it look old enough to delete.
        .filter_map(|e| {
            let path = e.path();
            Some((
                std::fs::symlink_metadata(&path).ok()?.modified().ok()?,
                path,
            ))
        })
        .collect();
    if journals.len() <= keep {
        return;
    }
    journals.sort();
    let excess = journals.len() - keep;
    for (modified, path) in journals.into_iter().take(excess) {
        let recent = now
            .duration_since(modified)
            .map_or(true, |age| age < RECENT);
        if !recent {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Reads the journal at `path`; `None` if there is none.
///
/// # Errors
/// When the file exists but can't be read.
pub(super) fn read(path: &Path) -> Result<Option<ReadJournal>, String> {
    ods_sdk::run_journal::read(path).map_err(|e| format!("can't read `{}`: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ods_core::state::TimestampMs;
    use ods_sdk::contracts::executor::ExecutionMode;
    use ods_sdk::contracts::run_events::RunOutcome;

    fn event(run_id: &str, kind: RunEventKind) -> RunEvent {
        RunEvent::new(run_id, None, TimestampMs::from_unix_millis(0), kind)
    }

    fn started(run_id: &str) -> RunEvent {
        event(
            run_id,
            RunEventKind::RunStarted {
                nodes: vec!["a".into()],
                mode: ExecutionMode::Run,
                live: true,
            },
        )
    }

    #[test]
    fn a_journal_is_appended_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let sink = JournalSink::new(&db);
        sink.emit(started("run-1"));
        sink.emit(event(
            "run-1",
            RunEventKind::RunFinished {
                outcome: RunOutcome::Succeeded,
            },
        ));
        let path = sink.path().unwrap();
        assert_eq!(path, dir.path().join("state.db.runs/run-1.jsonl"));
        assert_eq!(sink.warning(), None);
        // A cut-off last line (a run killed mid-write) is skipped, not fatal.
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"schema_version\":").unwrap();
        let read = read(&path).unwrap().unwrap();
        assert_eq!(read.events.len(), 2);
        assert_eq!(read.unreadable, 1);
        assert!(
            super::read(&dir.path().join("none.jsonl"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn an_existing_journal_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        JournalSink::new(&db).emit(started("run-1"));
        let again = JournalSink::new(&db);
        again.emit(started("run-1"));
        assert!(again.warning().unwrap().contains("can't create"));
        let bad = JournalSink::new(&db);
        bad.emit(started("../escape"));
        assert!(bad.warning().unwrap().contains("can't name a file"));
    }

    #[test]
    fn old_journals_are_pruned_to_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            let path = dir.path().join(format!("r{i}.jsonl"));
            std::fs::write(&path, "").unwrap();
            let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 + i);
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
        std::fs::write(dir.path().join("keep.txt"), "").unwrap();
        // One changed just now: another run may be writing it, so it stays.
        std::fs::write(dir.path().join("live.jsonl"), "").unwrap();
        prune(dir.path(), 2, std::time::SystemTime::now());
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["keep.txt", "live.jsonl", "r4.jsonl"]);
    }
}
