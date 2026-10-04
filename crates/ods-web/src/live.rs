//! The live run view's server side (#322, ADR-0024): a run's events as they are
//! written, and which runs are going on now.
//!
//! - `GET /api/runs/<id>/events` streams a run's journal as Server-Sent Events: every
//!   line from the start (so a page opened mid-run is right), then each new line as the
//!   run writes it, then an `end` event once the run finishes. Each message's id is its
//!   line number, so a client that reconnects with `Last-Event-ID` gets only what it
//!   missed. `?since=<n>` answers the same messages once, as JSON lines, for clients
//!   without `EventSource`.
//! - `GET /api/runs/live` lists the runs whose journal doesn't say they finished and
//!   changed recently: *probably* running, which is only inferred.
//!
//! The journal is tailed by polling its size, on a blocking thread, at most a bounded
//! chunk at a time, so no stream holds more than one chunk and one line. Every line is
//! read through ods-sdk's journal reader, which redacts each event again whatever wrote
//! the file (AGENTS rule 9); a check is sent by its handle, never its id (#323); and
//! beyond loopback, where an error's full text is (a local path) is left out too, as
//! the Run page does. The dashboard stays read-only: nothing
//! here starts, stops or writes anything.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use ods_core::state::Timestamp;
use ods_sdk::contracts::executor::ExecutionMode;
use ods_sdk::contracts::run_events::{NodeRunStatus, RunEvent, RunEventKind, RunOutcome};
use ods_sdk::run_journal::{RECENT, usable_run_id};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::server::Shared;

/// Version of the stream's messages and of [`LiveRuns`]. Additive fields don't change
/// it; a removed or retyped one does. Every message's data carries it as
/// `live_schema_version`, beside a run event's own `schema_version`, which is the
/// journal's. 2 since a `check_finished` names its check by handle (#323).
pub const LIVE_SCHEMA_VERSION: u32 = 2;

/// The most a stream reads from a journal at once, in bytes; also the longest line it
/// reads (a longer one is counted as unreadable, never held whole).
pub const CHUNK_BYTES: usize = 256 * 1024;

/// The most messages one `?since=` answer carries; the client asks again from the last.
pub const MAX_POLLED: usize = 2_000;

/// The most of a journal one `?since=` answer reads, in bytes, skipped lines included.
pub const MAX_POLL_BYTES: u64 = 32 * 1024 * 1024;

/// How long `GET /api/runs/live`'s answer is reused, so many pages polling it don't each
/// read the journals again.
const LIVE_TTL: Duration = Duration::from_millis(1500);

/// How many journals, newest first, `GET /api/runs/live` looks into.
const LIVE_CANDIDATES: usize = 8;

/// How the event streams behave: how many may be open, how often a journal is looked
/// at, when a heartbeat is sent, and when a run that writes nothing is given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct StreamLimits {
    /// Streams open at once, across clients; one more is refused with 503.
    pub max_streams: usize,
    /// How often a journal is checked for new lines.
    pub poll: Duration,
    /// How often a comment is sent when nothing else is, so proxies keep the
    /// connection open.
    pub heartbeat: Duration,
    /// A journal that hasn't changed for this long and doesn't say the run finished
    /// ends its stream as `stopped` (only inferred: a long node writes nothing).
    pub idle_end: Duration,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_streams: 16,
            poll: Duration::from_millis(300),
            heartbeat: Duration::from_secs(15),
            idle_end: RECENT,
        }
    }
}

impl StreamLimits {
    /// Limits with `max_streams` open at once, a journal checked every `poll`, a
    /// heartbeat every `heartbeat`, and a silent run given up on after `idle_end`.
    pub fn new(
        max_streams: usize,
        poll: Duration,
        heartbeat: Duration,
        idle_end: Duration,
    ) -> Self {
        Self {
            max_streams: max_streams.max(1),
            poll: poll.max(Duration::from_millis(10)),
            heartbeat: heartbeat.max(Duration::from_millis(10)),
            idle_end,
        }
    }
}

/// The streams' limits, the permits for open streams and for polls being answered, and
/// the last list of live runs, shared by every request.
#[derive(Debug)]
pub(crate) struct Streams {
    pub(crate) limits: StreamLimits,
    permits: Arc<Semaphore>,
    /// `?since=` answers being read at once: as many as streams.
    polls: Arc<Semaphore>,
    /// The last `/api/runs/live` answer: when, for which reload, and what. Its lock is
    /// held while one is made, so callers at the same time wait for that one.
    live: tokio::sync::Mutex<Option<(Instant, u64, Arc<LiveRuns>)>>,
}

impl Streams {
    pub(crate) fn new(limits: StreamLimits) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limits.max_streams)),
            polls: Arc::new(Semaphore::new(limits.max_streams)),
            live: tokio::sync::Mutex::new(None),
            limits,
        }
    }
}

/// 503, with when to try again.
fn busy(message: &str) -> Response {
    let mut response = error(StatusCode::SERVICE_UNAVAILABLE, message);
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

// ------------------------------------------------------------------------ tailing

/// One message of the stream.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Message {
    /// A journal line's event, by its line number.
    Event { line: u64, event: Box<RunEvent> },
    /// A journal line that couldn't be read: a newer version, a line cut short, or one
    /// longer than [`CHUNK_BYTES`].
    Unreadable { line: u64 },
    /// The stream is over.
    End(End),
}

/// Why a stream ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct End {
    /// `finished` (the journal says so), `stopped` (it doesn't, and hasn't changed for
    /// long: inferred), or `replaced` (the path names another file now, or the file got
    /// shorter: it isn't the journal the stream was reading).
    pub reason: &'static str,
    /// How the run ended, when the journal says.
    pub outcome: Option<RunOutcome>,
    /// Whether the reason is only inferred.
    pub inferred: bool,
    /// The reason in words.
    pub note: String,
}

/// How a quiet period reads in a note: `10 minutes`, `45 seconds`.
fn span(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        return format!("{secs} second{}", if secs == 1 { "" } else { "s" });
    }
    let mins = secs / 60;
    format!("{mins} minute{}", if mins == 1 { "" } else { "s" })
}

/// Opens a journal for reading without following a symbolic link and without
/// blocking (a FIFO swapped in would otherwise hold a thread), and only if it is a
/// regular file.
fn open_journal(path: &FsPath) -> std::io::Result<File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = {
        if !std::fs::symlink_metadata(path)?.is_file() {
            return Err(not_a_journal());
        }
        File::open(path)?
    };
    if !file.metadata()?.is_file() {
        return Err(not_a_journal());
    }
    Ok(file)
}

fn not_a_journal() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file")
}

/// What identifies a file while it is open: device and inode on Unix; elsewhere only
/// that it is a regular file (the handle kept open is what is read either way).
#[cfg(unix)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the same signature as on other systems, where there is no identity"
)]
fn identity(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((meta.dev(), meta.ino()))
}
#[cfg(not(unix))]
fn identity(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Whether the path's file and the open handle's file look like one file. On Unix the
/// identity above decides it; elsewhere there is none, so a file put in the journal's
/// place is told apart by a size or time outside what the handle read just before and
/// after (an append-only journal only grows between the two).
#[cfg(unix)]
fn same_file(
    _path: &std::fs::Metadata,
    _before: &std::fs::Metadata,
    _after: &std::fs::Metadata,
) -> bool {
    true
}
#[cfg(not(unix))]
fn same_file(
    path: &std::fs::Metadata,
    before: &std::fs::Metadata,
    after: &std::fs::Metadata,
) -> bool {
    let within = |p: Option<std::time::SystemTime>, a, b| match (p, a, b) {
        (Some(p), Some(a), Some(b)) => a <= p && p <= b,
        _ => false,
    };
    (before.len()..=after.len()).contains(&path.len())
        && within(
            path.modified().ok(),
            before.modified().ok(),
            after.modified().ok(),
        )
}

/// Reads a journal from where it last stopped: complete lines only, a bounded chunk at
/// a time, from one handle opened once. A line still being written is left for the next
/// read.
#[derive(Debug)]
pub(crate) struct Tail {
    path: PathBuf,
    /// The run asked for: an event of another run is flagged, not passed on.
    run_id: String,
    /// The journal, opened on the first read and kept.
    file: Option<File>,
    /// Its identity when opened: a path that names another file now was replaced.
    ident: Option<(u64, u64)>,
    /// Bytes read so far: always the end of a complete line, or of an overlong one.
    offset: u64,
    /// The file's length at the last read, to tell when it grew.
    len: u64,
    /// When it last grew, by this server's own clock (a file's time can be wrong).
    grew: Instant,
    /// Lines read so far.
    line: u64,
    /// Lines up to this one were sent before (`Last-Event-ID`, `?since=`): scanned for
    /// `run_finished` only, not parsed.
    since: u64,
    /// Inside a line longer than a chunk, which is skipped.
    overlong: bool,
    /// How the run ended, once a `run_finished` was read (sent or not).
    finished: Option<RunOutcome>,
    /// Whether the stream is over.
    done: bool,
}

impl Tail {
    pub(crate) fn new(path: PathBuf, run_id: &str, since: u64) -> Self {
        Self {
            path,
            run_id: run_id.to_owned(),
            file: None,
            ident: None,
            offset: 0,
            len: 0,
            grew: Instant::now(),
            line: 0,
            since,
            overlong: false,
            finished: None,
            done: false,
        }
    }

    /// Bytes read so far.
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads what was written since the last read, at most [`CHUNK_BYTES`] and at most
    /// `budget` messages, ending with [`Message::End`] when the run finished, stopped or
    /// the file was replaced. Returns whether more is already there to read.
    ///
    /// # Errors
    /// When the file can't be opened (or isn't a regular file) or read.
    pub(crate) fn read(
        &mut self,
        idle_end: Duration,
        budget: usize,
        out: &mut VecDeque<Message>,
    ) -> std::io::Result<bool> {
        if self.done {
            return Ok(false);
        }
        if self.file.is_none() {
            let file = open_journal(&self.path)?;
            self.ident = identity(&file.metadata()?);
            self.file = Some(file);
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(false);
        };
        // The path must still name the file this stream opened. The handle is read on
        // both sides of the path, so an append in between can't look like a new file.
        let before = file.metadata()?;
        let at_path = std::fs::symlink_metadata(&self.path);
        let meta = file.metadata()?;
        let same = at_path.is_ok_and(|m| {
            m.is_file() && identity(&m) == self.ident && same_file(&m, &before, &meta)
        });
        let len = meta.len();
        if !same || len < self.offset {
            self.end(
                out,
                End {
                    reason: "replaced",
                    outcome: None,
                    inferred: false,
                    note: "The journal was replaced or got shorter, so this stream stopped. \
                           Reload to read it again."
                        .to_owned(),
                },
            );
            return Ok(false);
        }
        if len > self.len {
            self.len = len;
            self.grew = Instant::now();
        }
        let mut more = false;
        if len > self.offset && budget > 0 {
            file.seek(SeekFrom::Start(self.offset))?;
            let want = usize::try_from(len - self.offset)
                .unwrap_or(usize::MAX)
                .min(CHUNK_BYTES);
            let mut buf = vec![0; want];
            file.read_exact(&mut buf)?;
            let full = self.offset + (want as u64) < len;
            more = self.take(&buf, budget, out) || full;
        }
        if more {
            return Ok(true);
        }
        let trailing = len > self.offset;
        if self.finished.is_some() {
            let outcome = self.finished;
            self.trailing(trailing, out);
            self.end(
                out,
                End {
                    reason: "finished",
                    outcome,
                    inferred: false,
                    note: "The run finished.".to_owned(),
                },
            );
        } else {
            // Quiet by the file's time, or by this server's clock: a time in the future
            // can't keep a stream open for ever.
            let quiet_file = meta
                .modified()
                .ok()
                .and_then(|m| SystemTime::now().duration_since(m).ok())
                .unwrap_or_default();
            if quiet_file >= idle_end || self.grew.elapsed() >= idle_end {
                self.trailing(trailing, out);
                self.end(
                    out,
                    End {
                        reason: "stopped",
                        outcome: None,
                        inferred: true,
                        note: format!(
                            "Probably stopped: its journal doesn't say the run finished, and \
                             nothing was written for {} or more. A node that runs longer \
                             writes nothing meanwhile, so it may still be running.",
                            span(idle_end)
                        ),
                    },
                );
            }
        }
        Ok(false)
    }

    /// A last line cut short by a run that stopped mid-write is unreadable, as the
    /// journal reader counts it for the Run page.
    fn trailing(&mut self, partial: bool, out: &mut VecDeque<Message>) {
        if partial || self.overlong {
            self.line += 1;
            let line = self.line;
            self.push(out, Message::Unreadable { line });
        }
    }

    fn end(&mut self, out: &mut VecDeque<Message>, end: End) {
        self.done = true;
        out.push_back(Message::End(end));
    }

    /// Takes the complete lines at the start of `buf`, until `budget` messages were
    /// made; keeps the rest for later, unless it fills the chunk: then it is the start
    /// of an overlong line, which is skipped. Returns whether the budget stopped it.
    fn take(&mut self, buf: &[u8], budget: usize, out: &mut VecDeque<Message>) -> bool {
        let complete = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let mut used = 0;
        let mut made = 0;
        for raw in buf[..complete].split_inclusive(|b| *b == b'\n') {
            used += raw.len();
            self.line += 1;
            let line = self.line;
            if std::mem::take(&mut self.overlong) {
                made += self.push(out, Message::Unreadable { line });
            } else if line <= self.since {
                // Sent before: only whether the run finished matters, so only a line
                // that may say so is parsed.
                if contains(raw, b"run_finished")
                    && let Some(event) = ods_sdk::run_journal::parse(raw)
                        .unwrap_or_default()
                        .events
                        .into_iter()
                        .next()
                    && let RunEventKind::RunFinished { outcome } = event.kind
                    && event.run_id == self.run_id
                {
                    self.finished = Some(outcome);
                }
            } else {
                // The shared reader: parses, checks the version, redacts again.
                let read = ods_sdk::run_journal::parse(raw).unwrap_or_default();
                match read.events.into_iter().next() {
                    // Another run's event in this run's journal is flagged, not shown.
                    Some(event) if event.run_id == self.run_id => {
                        if let RunEventKind::RunFinished { outcome } = &event.kind {
                            self.finished = Some(*outcome);
                        }
                        made += self.push(
                            out,
                            Message::Event {
                                line,
                                event: Box::new(event),
                            },
                        );
                    }
                    Some(_) => made += self.push(out, Message::Unreadable { line }),
                    None if read.unreadable > 0 => {
                        made += self.push(out, Message::Unreadable { line });
                    }
                    None => {}
                }
            }
            if made >= budget {
                self.offset += used as u64;
                return used < complete;
            }
        }
        self.offset += complete as u64;
        let rest = buf.len() - complete;
        if complete == 0 && rest >= CHUNK_BYTES {
            // A whole chunk without a line end: too long to be an event.
            self.overlong = true;
            self.offset += rest as u64;
        }
        false
    }

    /// Queues `message` unless it was sent before; returns how many were queued.
    fn push(&self, out: &mut VecDeque<Message>, message: Message) -> usize {
        let line = match &message {
            Message::Event { line, .. } | Message::Unreadable { line } => *line,
            Message::End(_) => u64::MAX,
        };
        if line > self.since {
            out.push_back(message);
            1
        } else {
            0
        }
    }
}

/// Whether `needle` is in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// An event as the stream sends it: the sanitized event, a check by its
/// [handle](ods_core::failure::check_handle) rather than its id (#323: an engine may
/// build a check's id from the test's arguments; the journal on disk keeps the id),
/// and beyond loopback without where an error's full text is.
fn event_json(event: &RunEvent, details: bool) -> serde_json::Value {
    let mut value = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    if let RunEventKind::CheckFinished { check, .. } = &event.kind
        && let Some(shown) = value.get_mut("check")
    {
        *shown = ods_core::failure::check_handle(check).into();
    }
    if !details
        && let Some(error) = value
            .get_mut("stats")
            .and_then(|s| s.get_mut("error"))
            .and_then(serde_json::Value::as_object_mut)
    {
        error.remove("details_at");
    }
    value
}

impl Message {
    fn name(&self) -> &'static str {
        match self {
            Message::Event { .. } => "run_event",
            Message::Unreadable { .. } => "unreadable",
            Message::End(_) => "end",
        }
    }

    fn id(&self) -> Option<u64> {
        match self {
            Message::Event { line, .. } | Message::Unreadable { line } => Some(*line),
            Message::End(_) => None,
        }
    }

    fn data(&self, details: bool) -> serde_json::Value {
        let mut data = match self {
            Message::Event { event, .. } => event_json(event, details),
            Message::Unreadable { line } => serde_json::json!({ "line": line }),
            Message::End(end) => serde_json::to_value(end).unwrap_or(serde_json::Value::Null),
        };
        // A client tells the stream's meaning by this, not by the event's version,
        // which is the journal's and didn't change when the stream did.
        if let Some(object) = data.as_object_mut() {
            object.insert("live_schema_version".into(), LIVE_SCHEMA_VERSION.into());
        }
        data
    }

    fn sse(&self, details: bool) -> Event {
        let event = Event::default()
            .event(self.name())
            .data(self.data(details).to_string());
        match self.id() {
            Some(id) => event.id(id.to_string()),
            None => event,
        }
    }

    /// The message as one JSON line of a `?since=` answer.
    fn json_line(&self, details: bool) -> String {
        let mut line = serde_json::json!({
            "event": self.name(),
            "data": self.data(details),
        });
        if let Some(id) = self.id() {
            line["id"] = id.into();
        }
        let mut text = line.to_string();
        text.push('\n');
        text
    }
}

// ----------------------------------------------------------------------- handlers

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The journal of `run_id`, or why there is none to read.
fn journal_of(state: &Shared, run_id: &str) -> Result<PathBuf, (StatusCode, &'static str)> {
    if !usable_run_id(run_id) {
        return Err((StatusCode::BAD_REQUEST, "not a run id"));
    }
    let snapshot = state.current();
    let dashboard = snapshot.dashboard();
    let Some(journals) = dashboard
        .journal_source()
        .and_then(|j| j.journals())
        .cloned()
    else {
        return Err((
            StatusCode::NOT_FOUND,
            "no run journals here: `ods serve` wasn't given a state database",
        ));
    };
    // As the Runs pages see journals: a symbolic link isn't one.
    match journals.file(run_id) {
        Some(file) => Ok(file.path),
        None => Err((StatusCode::NOT_FOUND, "no journal for this run")),
    }
}

#[derive(Deserialize)]
pub(crate) struct EventsQuery {
    /// Answer once, as JSON lines, with the messages after this line.
    since: Option<u64>,
}

/// `GET /api/runs/<id>/events`: the run's journal as Server-Sent Events, or with
/// `?since=<n>` as JSON lines.
pub(crate) async fn events(
    State(state): State<Shared>,
    Path(run_id): Path<String>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Response {
    let found = {
        let state = state.clone();
        let run_id = run_id.clone();
        tokio::task::spawn_blocking(move || journal_of(&state, &run_id)).await
    };
    let path = match found {
        Ok(Ok(path)) => path,
        Ok(Err((status, message))) => return error(status, message),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let details = state.details;
    if let Some(since) = query.since {
        let Ok(permit) = Arc::clone(&state.streams.polls).try_acquire_owned() else {
            return busy("too many polls are being answered; try again");
        };
        let response = polled(path, &run_id, since, details, state.streams.limits.idle_end).await;
        drop(permit);
        return response;
    }
    let Ok(permit) = Arc::clone(&state.streams.permits).try_acquire_owned() else {
        return busy("too many live streams are open; try again, or poll with `?since=`");
    };
    // EventSource sends back the last id it got; anything else starts over.
    let since = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let limits = state.streams.limits;
    let stream = Streaming {
        tail: Some(Tail::new(path, &run_id, since)),
        queue: VecDeque::new(),
        limits,
        first: true,
        details,
        _permit: permit,
    };
    let body = futures_util::stream::unfold(stream, Streaming::next);
    let mut response = Sse::new(body)
        .keep_alive(
            KeepAlive::new()
                .interval(limits.heartbeat)
                .text("heartbeat"),
        )
        .into_response();
    // A proxy that buffers (nginx does by default) would hold the events back.
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

/// A stream's state between messages. Dropped, with its permit, when the client goes.
struct Streaming {
    /// `None` while a read is on the blocking pool, or once the stream failed.
    tail: Option<Tail>,
    queue: VecDeque<Message>,
    limits: StreamLimits,
    first: bool,
    details: bool,
    _permit: OwnedSemaphorePermit,
}

impl Streaming {
    async fn next(mut self) -> Option<(Result<Event, std::convert::Infallible>, Self)> {
        if self.first {
            self.first = false;
            // How long EventSource waits before reconnecting.
            let hello = Event::default()
                .retry(Duration::from_secs(2))
                .comment("ods run events");
            return Some((Ok(hello), self));
        }
        loop {
            if let Some(message) = self.queue.pop_front() {
                let event = message.sse(self.details);
                return Some((Ok(event), self));
            }
            let mut tail = self.tail.take()?;
            if tail.done {
                return None;
            }
            let idle_end = self.limits.idle_end;
            let read = tokio::task::spawn_blocking(move || {
                let mut out = VecDeque::new();
                let more = tail.read(idle_end, usize::MAX, &mut out);
                (tail, out, more)
            })
            .await;
            let Ok((tail, out, more)) = read else {
                return None;
            };
            let more = match more {
                Ok(more) => more,
                Err(e) => {
                    tracing::warn!(error = %e, "live: a run journal can't be read");
                    return None;
                }
            };
            self.tail = Some(tail);
            self.queue = out;
            if self.queue.is_empty() && !more {
                tokio::time::sleep(self.limits.poll).await;
            }
        }
    }
}

/// `?since=<n>`: the messages after line `n`, once, as JSON lines, ending with `end`
/// when the run is over.
/// At most [`MAX_POLLED`] messages and [`MAX_POLL_BYTES`] of journal read.
async fn polled(
    path: PathBuf,
    run_id: &str,
    since: u64,
    details: bool,
    idle_end: Duration,
) -> Response {
    let run_id = run_id.to_owned();
    let read = tokio::task::spawn_blocking(move || {
        let mut tail = Tail::new(path, &run_id, since);
        let mut out = VecDeque::new();
        let mut text = String::new();
        let mut sent = 0;
        loop {
            let more = tail.read(idle_end, MAX_POLLED - sent, &mut out)?;
            while let Some(message) = out.pop_front() {
                text.push_str(&message.json_line(details));
                sent += 1;
            }
            if !more || sent >= MAX_POLLED || tail.offset() >= MAX_POLL_BYTES {
                break;
            }
        }
        Ok::<_, std::io::Error>(text)
    })
    .await;
    match read {
        Ok(Ok(text)) => (
            [(header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8")],
            text,
        )
            .into_response(),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "live: a run journal can't be read");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the journal can't be read",
            )
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// --------------------------------------------------------------------- live runs

/// The runs that are probably going on now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LiveRuns {
    /// Format version ([`LIVE_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Newest first.
    pub runs: Vec<LiveRun>,
}

/// A run whose journal doesn't say it finished, and changed recently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LiveRun {
    /// The run's id.
    pub run_id: String,
    /// Its state scope, if its events say.
    pub scope: Option<String>,
    /// Build, run or test.
    pub mode: Option<ExecutionMode>,
    /// The command, for people: `ods state build`.
    pub command: Option<&'static str>,
    /// When it started.
    pub started_at: Option<Timestamp>,
    /// When its journal last changed.
    pub updated_at: Timestamp,
    /// Nodes in the run so far.
    pub nodes: usize,
    /// How many of them finished.
    pub finished: usize,
    /// How many are running now.
    pub running: usize,
    /// How many failed so far.
    pub failed: usize,
    /// Always `probably_running`: nothing says a run is alive but its journal changing.
    pub status: &'static str,
    /// Always true: see `status`.
    pub inferred: bool,
    /// Why it is listed, in words.
    pub note: String,
    /// The live view, relative to the dashboard's root.
    pub href: String,
    /// The run's page, relative to the dashboard's root.
    pub run_href: String,
}

/// What each mode's command is called.
fn command(mode: ExecutionMode) -> Option<&'static str> {
    match mode {
        ExecutionMode::Build => Some("ods state build"),
        ExecutionMode::Run => Some("ods state run"),
        ExecutionMode::Test => Some("ods state test"),
        _ => None,
    }
}

impl crate::Dashboard {
    /// The runs that are probably going on now, as of `now`: the newest journals that
    /// changed within [`RECENT`] and don't say they finished, of this dashboard's scope
    /// (or of none). Read through the journals' cache, so a poll reads a file only
    /// once it changes.
    pub fn live_runs(&self, now: SystemTime) -> LiveRuns {
        let mut runs = Vec::new();
        let Some(source) = self.journal_source() else {
            return LiveRuns {
                schema_version: LIVE_SCHEMA_VERSION,
                runs,
            };
        };
        for file in source.list().into_iter().take(LIVE_CANDIDATES) {
            let age = now.duration_since(file.modified).unwrap_or_default();
            if age >= RECENT {
                // Listed newest first: the rest are older still.
                break;
            }
            let Some(run) = source.run(&file) else {
                continue;
            };
            let summary = &run.summary;
            if summary.outcome.is_some()
                || summary
                    .scope
                    .as_deref()
                    .is_some_and(|scope| scope != self.scope)
            {
                continue;
            }
            let count = |s| summary.totals.count(s);
            let finished = summary
                .nodes
                .iter()
                .filter(|n| n.stats.status.is_finished())
                .count();
            let updated = file
                .modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            runs.push(LiveRun {
                href: format!("lineage?live={}", file.run_id),
                run_href: format!("state/runs/{}", file.run_id),
                run_id: file.run_id,
                scope: summary.scope.clone(),
                mode: summary.mode,
                command: summary.mode.and_then(command),
                started_at: summary
                    .started_at
                    .map(ods_core::state::TimestampMs::to_seconds),
                updated_at: Timestamp::from_unix(updated),
                nodes: summary.nodes.len(),
                finished,
                running: count(NodeRunStatus::Running),
                failed: count(NodeRunStatus::Error),
                status: "probably_running",
                inferred: true,
                note: format!(
                    "Probably running: its journal doesn't say it finished, and changed in \
                     the last {} minutes.",
                    RECENT.as_secs() / 60
                ),
            });
        }
        LiveRuns {
            schema_version: LIVE_SCHEMA_VERSION,
            runs,
        }
    }
}

/// `GET /api/runs/live`.
pub(crate) async fn live(State(state): State<Shared>) -> Response {
    let generation = state.generation.load(std::sync::atomic::Ordering::SeqCst);
    // One answer at a time; the others wait for it and reuse it while it is fresh.
    let mut last = state.streams.live.lock().await;
    if let Some((at, made_for, runs)) = last.as_ref()
        && *made_for == generation
        && at.elapsed() < LIVE_TTL
    {
        return Json(runs.as_ref().clone()).into_response();
    }
    let reading = state.clone();
    let made = tokio::task::spawn_blocking(move || {
        reading.current().dashboard().live_runs(SystemTime::now())
    })
    .await;
    match made {
        Ok(runs) => {
            let runs = Arc::new(runs);
            *last = Some((Instant::now(), generation, Arc::clone(&runs)));
            Json(runs.as_ref().clone()).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use ods_core::state::TimestampMs;
    use ods_sdk::contracts::run_events::{ErrorSummary, NodeRunStats};

    use super::*;

    fn line(kind: RunEventKind) -> String {
        let event = RunEvent::new("r", None, TimestampMs::from_unix_millis(0), kind);
        format!("{}\n", serde_json::to_string(&event).unwrap())
    }

    fn queued(node: &str) -> String {
        line(RunEventKind::NodeQueued { node: node.into() })
    }

    fn read_all(tail: &mut Tail) -> Vec<Message> {
        let mut out = VecDeque::new();
        while tail
            .read(Duration::from_secs(3600), usize::MAX, &mut out)
            .unwrap()
        {}
        out.into()
    }

    fn lines(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .map(|m| match m {
                Message::Event { line, event } => format!("{line}:{}", event.node().unwrap_or("-")),
                Message::Unreadable { line } => format!("{line}:?"),
                Message::End(end) => format!("end:{}", end.reason),
            })
            .collect()
    }

    #[test]
    fn a_line_being_written_waits_for_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let mut file = File::create(&path).unwrap();
        let second = queued("b");
        write!(file, "{}{}", queued("a"), &second[..10]).unwrap();
        let mut tail = Tail::new(path.clone(), "r", 0);
        assert_eq!(lines(&read_all(&mut tail)), ["1:a"]);
        write!(file, "{}", &second[10..]).unwrap();
        file.write_all(b"\n{not json}\n").unwrap();
        // A blank line counts as a line, and says nothing.
        assert_eq!(lines(&read_all(&mut tail)), ["2:b", "4:?"]);
    }

    #[test]
    fn resuming_skips_what_was_sent_but_still_ends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let text = [
            queued("a"),
            queued("b"),
            line(RunEventKind::RunFinished {
                outcome: RunOutcome::Failed,
            }),
        ]
        .concat();
        std::fs::write(&path, text).unwrap();
        let mut tail = Tail::new(path, "r", 3);
        let got = read_all(&mut tail);
        assert_eq!(lines(&got), ["end:finished"]);
        let Message::End(end) = &got[0] else { panic!() };
        assert_eq!(end.outcome, Some(RunOutcome::Failed));
    }

    #[test]
    fn an_overlong_line_is_skipped_without_holding_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let mut text = queued("a");
        text.push_str(&"x".repeat(CHUNK_BYTES * 2 + 17));
        text.push('\n');
        text.push_str(&queued("b"));
        std::fs::write(&path, text).unwrap();
        let mut tail = Tail::new(path, "r", 0);
        assert_eq!(lines(&read_all(&mut tail)), ["1:a", "2:?", "3:b"]);
    }

    #[test]
    fn a_silent_unfinished_journal_ends_as_stopped_and_a_shorter_one_as_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        std::fs::write(&path, queued("a")).unwrap();
        let mut tail = Tail::new(path.clone(), "r", 0);
        let mut out = VecDeque::new();
        tail.read(Duration::ZERO, usize::MAX, &mut out).unwrap();
        assert_eq!(lines(&Vec::from(out)), ["1:a", "end:stopped"]);

        let mut tail = Tail::new(path.clone(), "r", 0);
        read_all(&mut tail);
        std::fs::write(&path, "").unwrap();
        assert_eq!(lines(&read_all(&mut tail)), ["end:replaced"]);
    }

    #[test]
    fn beyond_loopback_an_errors_log_path_is_left_out() {
        let stats = NodeRunStats::new(NodeRunStatus::Error).with_error(
            ErrorSummary::from_message("boom").map(|e| e.with_details_at("/home/me/logs/dbt.log")),
        );
        let event = RunEvent::new(
            "r",
            None,
            TimestampMs::from_unix_millis(0),
            RunEventKind::NodeFinished {
                node: "a".into(),
                stats,
            },
        );
        assert!(event_json(&event, true).to_string().contains("dbt.log"));
        assert!(!event_json(&event, false).to_string().contains("dbt.log"));
    }

    #[test]
    fn a_file_put_in_its_place_ends_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        std::fs::write(&path, queued("a")).unwrap();
        let mut tail = Tail::new(path.clone(), "r", 0);
        assert_eq!(lines(&read_all(&mut tail)), ["1:a"]);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, [queued("a"), queued("b")].concat()).unwrap();
        assert_eq!(lines(&read_all(&mut tail)), ["end:replaced"]);
    }

    #[cfg(unix)]
    #[test]
    fn links_and_fifos_are_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.jsonl");
        std::fs::write(&real, queued("a")).unwrap();
        let link = dir.path().join("r.jsonl");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut out = VecDeque::new();
        assert!(
            Tail::new(link, "r", 0)
                .read(Duration::ZERO, 10, &mut out)
                .is_err()
        );
        // A FIFO would block a reader until something writes to it: refused at once.
        let fifo = dir.path().join("f.jsonl");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if made.is_ok_and(|s| s.success()) {
            assert!(
                Tail::new(fifo, "r", 0)
                    .read(Duration::ZERO, 10, &mut out)
                    .is_err()
            );
        }
        assert!(out.is_empty());
    }

    #[test]
    fn a_torn_last_line_of_a_stopped_run_is_unreadable_as_the_run_page_counts_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let torn = queued("b");
        std::fs::write(&path, format!("{}{}", queued("a"), &torn[..12])).unwrap();
        let mut tail = Tail::new(path.clone(), "r", 0);
        let mut out = VecDeque::new();
        tail.read(Duration::ZERO, usize::MAX, &mut out).unwrap();
        assert_eq!(lines(&Vec::from(out)), ["1:a", "2:?", "end:stopped"]);
        let read = ods_sdk::run_journal::read(&path).unwrap().unwrap();
        assert_eq!(read.unreadable, 1);
    }

    #[test]
    fn a_journal_dated_in_the_future_still_stops_by_the_servers_clock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        std::fs::write(&path, queued("a")).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(86_400))
            .unwrap();
        let mut tail = Tail::new(path, "r", 0);
        let quiet = Duration::from_millis(80);
        let mut out = VecDeque::new();
        tail.read(quiet, usize::MAX, &mut out).unwrap();
        assert_eq!(lines(&Vec::from(std::mem::take(&mut out))), ["1:a"]);
        std::thread::sleep(Duration::from_millis(120));
        tail.read(quiet, usize::MAX, &mut out).unwrap();
        let got = Vec::from(out);
        assert_eq!(lines(&got), ["end:stopped"]);
        let Message::End(end) = &got[0] else { panic!() };
        assert!(end.note.contains("0 seconds"), "{}", end.note);
        assert_eq!(span(Duration::from_secs(600)), "10 minutes");
        assert_eq!(span(Duration::from_secs(45)), "45 seconds");
    }

    #[test]
    fn a_budget_stops_at_a_line_and_the_next_read_goes_on_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        std::fs::write(&path, [queued("a"), queued("b"), queued("c")].concat()).unwrap();
        let mut tail = Tail::new(path, "r", 0);
        let mut out = VecDeque::new();
        assert!(tail.read(Duration::from_secs(3600), 2, &mut out).unwrap());
        assert_eq!(lines(&Vec::from(std::mem::take(&mut out))), ["1:a", "2:b"]);
        assert!(!tail.read(Duration::from_secs(3600), 2, &mut out).unwrap());
        assert_eq!(lines(&Vec::from(out)), ["3:c"]);
    }

    #[test]
    fn another_runs_event_is_flagged_not_shown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let other = RunEvent::new(
            "other",
            None,
            TimestampMs::from_unix_millis(0),
            RunEventKind::NodeQueued { node: "x".into() },
        );
        let text = format!(
            "{}{}\n",
            queued("a"),
            serde_json::to_string(&other).unwrap()
        );
        std::fs::write(&path, text).unwrap();
        let mut tail = Tail::new(path, "r", 0);
        assert_eq!(lines(&read_all(&mut tail)), ["1:a", "2:?"]);
    }
}
