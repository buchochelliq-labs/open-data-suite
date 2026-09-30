//! The run journal's format (#322, ADR-0024): where a run's journal is, and how it
//! reads back.
//!
//! A host that runs an executor appends every [`RunEvent`] of the run, one JSON line
//! each, to `<state-db>.runs/<run_id>.jsonl`. Hosts that show runs (`ods state
//! history`, `ods serve`) read them back here, so there is one reader: it skips a last
//! line cut short by a run that stopped mid-write, refuses lines of a version it can't
//! read, and redacts every event again ([`RunEvent::sanitized`]) whatever wrote the
//! file, so nothing a journal holds beyond the sanitized fields reaches a page.
//!
//! Only reading and naming live here: writing, and pruning old journals, is the host's.
//! The journal is evidence, not state (AGENTS.md rule 5): nothing reads it to decide
//! what to build.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::contracts::run_events::{RUN_EVENTS_SCHEMA_VERSION, RunEvent};

/// How many journals are kept per state database: the newest, by modification time.
pub const KEEP: usize = 50;

/// A journal changed this recently may belong to a run that is still going: it is
/// never pruned, and a reader doesn't call a run in it stopped.
pub const RECENT: Duration = Duration::from_secs(10 * 60);

/// The directory of the journals beside `state_db`: `<state-db>.runs`.
pub fn dir_for(state_db: &Path) -> PathBuf {
    let mut name = state_db.as_os_str().to_owned();
    name.push(".runs");
    PathBuf::from(name)
}

/// The journal of `run_id` beside `state_db`, if `run_id` can name a file
/// ([`usable_run_id`]).
pub fn path_for(state_db: &Path, run_id: &str) -> Option<PathBuf> {
    Journals::beside(state_db).path(run_id)
}

/// Whether a run id can be a file name as it is, on every system: 1 to 128 ASCII
/// letters, digits, `-`, `_` and `.`, not starting or ending with `.`, and not a name
/// Windows reserves for a device (`CON`, `NUL`, `COM1`, … in any case, with or
/// without an extension). No maintained crate does only this; the list is short.
pub fn usable_run_id(run_id: &str) -> bool {
    const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let stem = run_id
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let device = DEVICES.contains(&stem.as_str())
        || ["COM", "LPT"].iter().any(|p| {
            stem.strip_prefix(p)
                .is_some_and(|n| n.len() == 1 && n.bytes().all(|b| b.is_ascii_digit()))
        });
    (1..=128).contains(&run_id.len())
        && !run_id.starts_with('.')
        && !run_id.ends_with('.')
        && !device
        && run_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A journal, as read back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReadJournal {
    /// Its events, in order, each [sanitized](RunEvent::sanitized) again.
    pub events: Vec<RunEvent>,
    /// Lines that couldn't be read: a newer version, or a last line cut short by a run
    /// that stopped mid-write.
    pub unreadable: usize,
}

/// Reads journal lines from `reader`. A line that doesn't parse (or isn't UTF-8, as a
/// line cut mid-character isn't) is counted in [`ReadJournal::unreadable`], not fatal.
///
/// # Errors
/// When `reader` fails.
pub fn parse(mut reader: impl BufRead) -> std::io::Result<ReadJournal> {
    let mut journal = ReadJournal::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<RunEvent>(&line) {
            Ok(event) if RUN_EVENTS_SCHEMA_VERSION.can_read(event.schema_version) => {
                journal.events.push(event.sanitized());
            }
            _ => journal.unreadable += 1,
        }
    }
    Ok(journal)
}

/// Reads the journal at `path`; `None` if there is none.
///
/// # Errors
/// When the file exists but can't be read.
pub fn read(path: &Path) -> std::io::Result<Option<ReadJournal>> {
    match File::open(path) {
        Ok(file) => parse(BufReader::new(file)).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A journal file found in a journals directory.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JournalFile {
    /// The run it records: the file's name without `.jsonl`.
    pub run_id: String,
    /// Where it is.
    pub path: PathBuf,
    /// When it last changed.
    pub modified: SystemTime,
    /// Its size, in bytes.
    pub len: u64,
}

/// The journals beside one state database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Journals {
    dir: PathBuf,
}

impl Journals {
    /// The journals of the state database at `state_db`, in [`dir_for`] it.
    pub fn beside(state_db: &Path) -> Self {
        Self {
            dir: dir_for(state_db),
        }
    }

    /// The journals in `dir`.
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Their directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The journal of `run_id`, if `run_id` can name a file.
    pub fn path(&self, run_id: &str) -> Option<PathBuf> {
        usable_run_id(run_id).then(|| self.dir.join(format!("{run_id}.jsonl")))
    }

    /// The journal file of `run_id`, if there is one.
    pub fn file(&self, run_id: &str) -> Option<JournalFile> {
        let path = self.path(run_id)?;
        // As `list` sees entries: a symbolic link is not followed.
        let meta = std::fs::symlink_metadata(&path)
            .ok()
            .filter(std::fs::Metadata::is_file)?;
        Some(JournalFile {
            run_id: run_id.to_owned(),
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            len: meta.len(),
            path,
        })
    }

    /// Every journal there, newest (by modification time) first, then by run id. A
    /// missing directory has none; a file whose name can't be a run id is left out.
    ///
    /// # Errors
    /// When the directory exists but can't be listed.
    pub fn list(&self) -> std::io::Result<Vec<JournalFile>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut files: Vec<JournalFile> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let run_id = path
                    .file_name()?
                    .to_str()?
                    .strip_suffix(".jsonl")?
                    .to_owned();
                let meta = entry.metadata().ok().filter(std::fs::Metadata::is_file)?;
                usable_run_id(&run_id).then(|| JournalFile {
                    run_id,
                    modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    len: meta.len(),
                    path,
                })
            })
            .collect();
        files.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.run_id.cmp(&b.run_id)));
        Ok(files)
    }

    /// The journal of `run_id`; `None` if there is none or `run_id` can't name a file.
    ///
    /// # Errors
    /// When it exists but can't be read.
    pub fn read(&self, run_id: &str) -> std::io::Result<Option<ReadJournal>> {
        match self.path(run_id) {
            Some(path) => read(&path),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use ods_core::state::TimestampMs;

    use super::*;
    use crate::contracts::executor::ExecutionMode;
    use crate::contracts::run_events::{
        ErrorSummary, NodeRunStats, NodeRunStatus, RunEventKind, RunOutcome,
    };

    fn line(run_id: &str, kind: RunEventKind) -> String {
        let event = RunEvent::new(run_id, None, TimestampMs::from_unix_millis(0), kind);
        format!("{}\n", serde_json::to_string(&event).unwrap())
    }

    #[test]
    fn only_plain_run_ids_name_files() {
        assert!(usable_run_id("0e3c6a40-1b2c-4d5e-8f90-123456789abc"));
        assert!(usable_run_id("fake-run-1"));
        for bad in [
            "",
            ".hidden",
            "../x",
            "a/b",
            "a b",
            &"x".repeat(129),
            "CON",
            "nul",
            "Aux.jsonl",
            "com1",
            "LPT9.x",
            "trailing.",
        ] {
            assert!(!usable_run_id(bad), "{bad}");
        }
        assert_eq!(path_for(Path::new("s.db"), "../x"), None);
        assert_eq!(
            path_for(Path::new("s.db"), "r1"),
            Some(PathBuf::from("s.db.runs/r1.jsonl"))
        );
    }

    #[test]
    fn a_torn_or_newer_line_is_skipped_not_fatal() {
        let mut text = line(
            "r",
            RunEventKind::RunStarted {
                nodes: vec!["a".into()],
                mode: ExecutionMode::Run,
                live: true,
            },
        );
        text.push_str("\n   \n");
        text.push_str(&line(
            "r",
            RunEventKind::RunFinished {
                outcome: RunOutcome::Succeeded,
            },
        ));
        // A newer major version: refused, not guessed at.
        let mut newer: serde_json::Value = serde_json::from_str(
            line(
                "r",
                RunEventKind::RunFinished {
                    outcome: RunOutcome::Failed,
                },
            )
            .trim(),
        )
        .unwrap();
        newer["schema_version"]["major"] = 9.into();
        text.push_str(&newer.to_string());
        text.push('\n');
        let mut bytes = text.into_bytes();
        // Cut mid-character: not even UTF-8.
        bytes.extend_from_slice(b"{\"schema_version\":\"1.0\",\"run_id\":\"\xE2\x82");
        let read = parse(bytes.as_slice()).unwrap();
        assert_eq!(read.events.len(), 2);
        assert_eq!(read.unreadable, 2);
    }

    #[test]
    fn every_event_is_redacted_again_on_read() {
        // Written around the writer, as a tampered or older file could be: the error,
        // thread and extras carry values the builders would have removed.
        let event = RunEvent::new(
            "r",
            None,
            TimestampMs::from_unix_millis(0),
            RunEventKind::NodeFinished {
                node: "a".into(),
                stats: NodeRunStats::new(NodeRunStatus::Error)
                    .with_error(ErrorSummary::from_message("boom")),
            },
        );
        let mut raw = serde_json::to_value(&event).unwrap();
        raw["stats"]["error"]["message"] = "boom where secret = 'SENTINEL-E'".into();
        raw["stats"]["thread"] = "t 'SENTINEL-T'".into();
        raw["stats"]["adapter"] = serde_json::json!({ "k": "v 'SENTINEL-A'" });
        let line = format!("{raw}\n");
        assert!(line.contains("SENTINEL-E"));
        let read = parse(line.as_bytes()).unwrap();
        assert_eq!(read.events.len(), 1);
        let text = serde_json::to_string(&read.events).unwrap();
        assert!(!text.contains("SENTINEL"), "{text}");
    }

    #[test]
    fn journals_are_listed_newest_first_and_read_by_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let journals = Journals::beside(&db);
        assert!(journals.list().unwrap().is_empty());
        std::fs::create_dir_all(journals.dir()).unwrap();
        for (i, run) in ["old", "new"].iter().enumerate() {
            let path = journals.path(run).unwrap();
            let mut file = File::create(&path).unwrap();
            file.write_all(
                line(
                    run,
                    RunEventKind::RunFinished {
                        outcome: RunOutcome::Failed,
                    },
                )
                .as_bytes(),
            )
            .unwrap();
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + i as u64);
            file.set_modified(at).unwrap();
        }
        std::fs::write(journals.dir().join("notes.txt"), "").unwrap();
        std::fs::write(journals.dir().join("a b.jsonl"), "").unwrap();
        let listed: Vec<String> = journals
            .list()
            .unwrap()
            .into_iter()
            .map(|f| f.run_id)
            .collect();
        assert_eq!(listed, ["new", "old"]);
        assert_eq!(journals.read("new").unwrap().unwrap().events.len(), 1);
        assert_eq!(journals.file("old").unwrap().run_id, "old");
        assert_eq!(journals.file("missing"), None);
        assert_eq!(journals.read("missing").unwrap(), None);
        assert_eq!(journals.read("../escape").unwrap(), None);
    }
}
