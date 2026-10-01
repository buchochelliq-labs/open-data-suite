//! The live run view's server side (#322): a run's journal streamed as Server-Sent
//! Events while it is written, the `?since=` fallback, and the runs going on now, over
//! a real socket.

use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use ods_core::state::TimestampMs;
use ods_lineage::{GraphFilter, LineageNode, LineageProject, MemoryCache, NodeKind, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::executor::ExecutionMode;
use ods_sdk::contracts::run_events::{
    ErrorSummary, NodeRunStats, NodeRunStatus, RunEvent, RunEventKind, RunOutcome,
};
use ods_sdk::run_journal::Journals;
use ods_web::{Dashboard, ServeOptions, Snapshot, StreamLimits, router};
use serde_json::Value;

const SCOPE: &str = "jaffle_ods/dev";
const RUN: &str = "0e3c6a40-1b2c-4d5e-8f90-123456789abc";

fn snapshot(journals: &Path) -> Snapshot {
    let project = LineageProject::new(vec![LineageNode::new(
        "model.orders",
        ods_core::RelationName::new(["db", "orders"]).unwrap(),
        NodeKind::Model,
    )]);
    let (graph, _) = build(
        &project,
        &FakeSqlLineageAnalyzer::new(),
        &MemoryCache::default(),
    )
    .unwrap();
    let document = graph.document(&|id: &str| id.to_owned(), &GraphFilter::default());
    Snapshot::new(document, graph, "fixture").with_dashboard(
        Dashboard::new("jaffle_ods", "dev").with_journals(Journals::in_dir(journals)),
    )
}

fn limits(max_streams: usize) -> StreamLimits {
    StreamLimits::new(
        max_streams,
        Duration::from_millis(40),
        Duration::from_millis(150),
        Duration::from_secs(3600),
    )
}

fn start(snapshot: Snapshot, limits: StreamLimits) -> SocketAddr {
    let options = ServeOptions::new(([127, 0, 0, 1], 0).into()).with_stream_limits(limits);
    let app = router(snapshot, &options);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

// ------------------------------------------------------------------ the journal

fn at(ms: i64) -> TimestampMs {
    TimestampMs::from_unix_millis(1_790_000_000_000 + ms)
}

fn event(ms: i64, kind: RunEventKind) -> RunEvent {
    RunEvent::new(RUN, Some(SCOPE.to_owned()), at(ms), kind)
}

fn line(event: &RunEvent) -> String {
    format!("{}\n", serde_json::to_string(event).unwrap())
}

fn started(nodes: &[&str]) -> String {
    line(&event(
        0,
        RunEventKind::RunStarted {
            nodes: nodes.iter().map(|n| (*n).to_owned()).collect(),
            mode: ExecutionMode::Build,
            live: true,
        },
    ))
}

fn node_started(ms: i64, node: &str) -> String {
    line(&event(
        ms,
        RunEventKind::NodeStarted {
            node: node.into(),
            thread: Some("Thread-1".into()),
        },
    ))
}

fn node_finished(ms: i64, node: &str, stats: NodeRunStats) -> String {
    line(&event(
        ms,
        RunEventKind::NodeFinished {
            node: node.into(),
            stats,
        },
    ))
}

fn run_finished(ms: i64, outcome: RunOutcome) -> String {
    line(&event(ms, RunEventKind::RunFinished { outcome }))
}

/// A journal being written, as `ods state build` writes it: a line at a time.
struct Journal {
    file: File,
}

impl Journal {
    fn create(dir: &Path, run: &str) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{run}.jsonl"));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .unwrap();
        Self { file }
    }

    fn write(&mut self, text: &str) {
        self.file.write_all(text.as_bytes()).unwrap();
        self.file.flush().unwrap();
    }
}

// ------------------------------------------------------------------- the client

/// An open event stream, read as the bytes come.
struct Stream {
    reader: BufReader<TcpStream>,
    pub status: u16,
    pub head: String,
}

/// One message: its fields as sent (`event`, `id`, `data`), or a comment.
#[derive(Debug, Default)]
struct Message {
    event: String,
    id: Option<u64>,
    data: String,
    comment: Option<String>,
}

impl Message {
    fn json(&self) -> Value {
        serde_json::from_str(&self.data).unwrap()
    }
}

fn open(addr: SocketAddr, path: &str, last_event_id: Option<u64>) -> Stream {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request =
        format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\n");
    if let Some(id) = last_event_id {
        request = format!("{request}Last-Event-ID: {id}\r\n");
    }
    request.push_str("\r\n");
    (&stream).write_all(request.as_bytes()).unwrap();
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        head.push_str(&line);
    }
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    Stream {
        reader,
        status,
        head,
    }
}

impl Stream {
    /// The next message, or `None` when the server closed the stream.
    fn next(&mut self) -> Option<Message> {
        let mut message = Message::default();
        let mut any = false;
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line).unwrap() == 0 {
                return any.then_some(message);
            }
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if any {
                    return Some(message);
                }
                continue;
            }
            any = true;
            if let Some(comment) = line.strip_prefix(':') {
                message.comment = Some(comment.trim().to_owned());
            } else if let Some((field, value)) = line.split_once(':') {
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => value.clone_into(&mut message.event),
                    "id" => message.id = value.parse().ok(),
                    "data" => message.data.push_str(value),
                    _ => {}
                }
            }
        }
    }

    /// The next message that isn't a comment (the greeting, heartbeats).
    fn next_event(&mut self) -> Option<Message> {
        loop {
            let message = self.next()?;
            if message.comment.is_none() || !message.event.is_empty() {
                return Some(message);
            }
        }
    }

    fn rest(mut self) -> String {
        let mut rest = String::new();
        let _ = self.reader.read_to_string(&mut rest);
        rest
    }
}

fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let stream = open(addr, path, None);
    let status = stream.status;
    (status, stream.rest())
}

fn node_of(message: &Message) -> String {
    let data = message.json();
    format!(
        "{}:{}",
        data["kind"].as_str().unwrap(),
        data["node"].as_str().unwrap_or("-")
    )
}

// ------------------------------------------------------------------------- tests

#[test]
fn a_run_is_replayed_from_the_start_then_followed_as_written_and_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = Journal::create(dir.path(), RUN);
    journal.write(&started(&["model.orders", "model.customers"]));
    journal.write(&node_started(100, "model.orders"));
    let addr = start(snapshot(dir.path()), limits(4));

    let mut stream = open(addr, &format!("/api/runs/{RUN}/events"), None);
    assert_eq!(stream.status, 200);
    assert!(stream.head.contains("text/event-stream"), "{}", stream.head);
    // The first message says how long to wait before reconnecting.
    let hello = stream.next().unwrap();
    assert!(hello.comment.is_some());

    // What was written before the page opened, in order, by line number.
    let first = stream.next_event().unwrap();
    assert_eq!((first.event.as_str(), first.id), ("run_event", Some(1)));
    assert_eq!(node_of(&first), "run_started:-");
    let second = stream.next_event().unwrap();
    assert_eq!(second.id, Some(2));
    assert_eq!(node_of(&second), "node_started:model.orders");

    // Then each line as the run writes it.
    journal.write(&node_finished(
        900,
        "model.orders",
        NodeRunStats::new(NodeRunStatus::Success).with_rows_affected(Some(99)),
    ));
    let third = stream.next_event().unwrap();
    assert_eq!(third.id, Some(3));
    assert_eq!(third.json()["stats"]["rows_affected"], 99);
    journal.write(&node_started(1_000, "model.customers"));
    journal.write(&node_finished(
        1_500,
        "model.customers",
        NodeRunStats::new(NodeRunStatus::Success),
    ));
    let ids: Vec<Option<u64>> = (0..2).map(|_| stream.next_event().unwrap().id).collect();
    assert_eq!(ids, [Some(4), Some(5)]);

    journal.write(&run_finished(1_600, RunOutcome::Succeeded));
    let finished = stream.next_event().unwrap();
    assert_eq!(node_of(&finished), "run_finished:-");
    let end = stream.next_event().unwrap();
    assert_eq!(end.event, "end");
    assert_eq!(end.id, None, "the end has no line");
    assert_eq!(end.json()["reason"], "finished");
    assert_eq!(end.json()["outcome"], "succeeded");
    // And the server closes the stream.
    assert!(stream.next_event().is_none());
}

#[test]
fn a_client_that_reconnects_gets_only_what_it_missed() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = Journal::create(dir.path(), RUN);
    journal.write(&started(&["model.orders"]));
    journal.write(&node_started(100, "model.orders"));
    journal.write(&node_finished(
        900,
        "model.orders",
        NodeRunStats::new(NodeRunStatus::Success),
    ));
    let addr = start(snapshot(dir.path()), limits(4));
    let mut stream = open(addr, &format!("/api/runs/{RUN}/events"), Some(2));
    let next = stream.next_event().unwrap();
    assert_eq!(next.id, Some(3));
    assert_eq!(node_of(&next), "node_finished:model.orders");

    // After the end, a reconnect gets the end again and nothing else.
    journal.write(&run_finished(1_000, RunOutcome::Succeeded));
    let mut stream = open(addr, &format!("/api/runs/{RUN}/events"), Some(4));
    let end = stream.next_event().unwrap();
    assert_eq!(end.event, "end");
    assert!(stream.next_event().is_none());

    // An id that isn't a number starts over.
    let mut stream = open(addr, &format!("/api/runs/{RUN}/events"), None);
    assert_eq!(stream.next_event().unwrap().id, Some(1));
}

#[test]
fn a_line_still_being_written_is_sent_once_whole_and_a_bad_one_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = Journal::create(dir.path(), RUN);
    journal.write(&started(&["model.orders"]));
    let torn = node_started(100, "model.orders");
    journal.write(&torn[..20]);
    let addr = start(snapshot(dir.path()), limits(4));
    let mut stream = open(addr, &format!("/api/runs/{RUN}/events"), None);
    assert_eq!(stream.next_event().unwrap().id, Some(1));
    // Nothing for the half line, but heartbeats keep the connection open meanwhile.
    let heartbeat = stream.next().unwrap();
    assert_eq!(heartbeat.comment.as_deref(), Some("heartbeat"));
    journal.write(&torn[20..]);
    let whole = stream.next_event().unwrap();
    assert_eq!(
        (whole.id, node_of(&whole)),
        (Some(2), "node_started:model.orders".to_owned())
    );
    // A line of a version this ODS can't read is named, not guessed at.
    journal.write("{\"schema_version\":\"9.0\",\"kind\":\"node_queued\"}\n");
    let bad = stream.next_event().unwrap();
    assert_eq!((bad.event.as_str(), bad.id), ("unreadable", Some(3)));
    assert_eq!(bad.json()["line"], 3);
}

#[test]
fn nothing_the_journal_holds_beyond_the_redacted_fields_is_streamed() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = Journal::create(dir.path(), RUN);
    journal.write(&started(&["model.orders"]));
    // Written around the writer, as a tampered or older file could be.
    let failed = event(
        900,
        RunEventKind::NodeFinished {
            node: "model.orders".into(),
            stats: NodeRunStats::new(NodeRunStatus::Error)
                .with_error(ErrorSummary::from_message("boom")),
        },
    );
    let mut raw = serde_json::to_value(&failed).unwrap();
    raw["stats"]["error"]["message"] = "boom where password = 'SENTINEL-E'".into();
    raw["stats"]["thread"] = "t 'SENTINEL-T'".into();
    raw["stats"]["adapter"] = serde_json::json!({ "query": "select 'SENTINEL-A'" });
    journal.write(&format!("{raw}\n"));
    journal.write(&run_finished(1_000, RunOutcome::Failed));
    let addr = start(snapshot(dir.path()), limits(4));
    let (status, body) = get(addr, &format!("/api/runs/{RUN}/events"));
    assert_eq!(status, 200);
    assert!(body.contains("node_finished"), "{body}");
    assert!(!body.contains("SENTINEL"), "{body}");
    let (_, polled) = get(addr, &format!("/api/runs/{RUN}/events?since=0"));
    assert!(polled.contains("node_finished"), "{polled}");
    assert!(!polled.contains("SENTINEL"), "{polled}");
}

#[test]
fn the_polling_fallback_answers_json_lines_after_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = Journal::create(dir.path(), RUN);
    journal.write(&started(&["model.orders"]));
    journal.write(&node_started(100, "model.orders"));
    let addr = start(snapshot(dir.path()), limits(1));
    let lines = |body: &str| -> Vec<Value> {
        body.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    let (status, body) = get(addr, &format!("/api/runs/{RUN}/events?since=0"));
    assert_eq!(status, 200);
    let got = lines(&body);
    assert_eq!(got.len(), 2, "no end while the run goes: {body}");
    assert_eq!(got[0]["id"], 1);
    assert_eq!(got[1]["event"], "run_event");
    assert_eq!(got[1]["data"]["kind"], "node_started");

    journal.write(&run_finished(1_000, RunOutcome::Failed));
    let (_, body) = get(addr, &format!("/api/runs/{RUN}/events?since=2"));
    let got = lines(&body);
    assert_eq!(got.len(), 2, "{body}");
    assert_eq!(got[0]["id"], 3);
    assert_eq!(got[1]["event"], "end");
    assert_eq!(got[1]["data"]["outcome"], "failed");
}

#[test]
fn run_ids_that_arent_plain_names_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    Journal::create(dir.path(), RUN).write(&started(&[]));
    // A journal outside the journals directory, which traversal would reach.
    std::fs::write(dir.path().join("secret.jsonl"), started(&[])).unwrap();
    let addr = start(snapshot(&dir.path().join("runs")), limits(4));
    for bad in ["..%2Fsecret", "a%20b", "CON", ".hidden", "%2E%2E"] {
        let (status, body) = get(addr, &format!("/api/runs/{bad}/events"));
        assert_eq!(status, 400, "{bad}: {body}");
    }
    let (status, _) = get(addr, "/api/runs/no-such-run/events");
    assert_eq!(status, 404);

    // Without journals at all.
    let project = LineageProject::new(Vec::new());
    let (graph, _) = build(
        &project,
        &FakeSqlLineageAnalyzer::new(),
        &MemoryCache::default(),
    )
    .unwrap();
    let document = graph.document(&|id: &str| id.to_owned(), &GraphFilter::default());
    let bare = start(Snapshot::new(document, graph, "fixture"), limits(4));
    let (status, body) = get(bare, &format!("/api/runs/{RUN}/events"));
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("no run journals"), "{body}");
}

#[test]
fn streams_are_capped_and_a_closed_one_frees_its_place() {
    let dir = tempfile::tempdir().unwrap();
    Journal::create(dir.path(), RUN).write(&started(&["model.orders"]));
    let addr = start(snapshot(dir.path()), limits(1));
    let mut first = open(addr, &format!("/api/runs/{RUN}/events"), None);
    assert_eq!(first.status, 200);
    assert_eq!(first.next_event().unwrap().id, Some(1));

    let second = open(addr, &format!("/api/runs/{RUN}/events"), None);
    assert_eq!(second.status, 503);
    assert!(
        second.head.to_ascii_lowercase().contains("retry-after"),
        "{}",
        second.head
    );
    // Polling still answers: it holds nothing open.
    assert_eq!(get(addr, &format!("/api/runs/{RUN}/events?since=0")).0, 200);

    // Once the client goes, the next heartbeat can't be written, and the place is free.
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let again = open(addr, &format!("/api/runs/{RUN}/events"), None);
        if again.status == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the closed stream kept its place"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn runs_going_on_now_are_listed_as_probably_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut going = Journal::create(dir.path(), RUN);
    going.write(&started(&["model.orders", "model.customers"]));
    going.write(&node_started(100, "model.orders"));
    // A finished run, and one of another scope, aren't listed.
    let mut done = Journal::create(dir.path(), "finished-run");
    done.write(&started(&["model.orders"]));
    done.write(&run_finished(100, RunOutcome::Succeeded));
    let other = RunEvent::new(
        "other-scope",
        Some("jaffle_ods/prod".to_owned()),
        at(0),
        RunEventKind::RunStarted {
            nodes: vec![],
            mode: ExecutionMode::Build,
            live: true,
        },
    );
    Journal::create(dir.path(), "other-scope").write(&line(&other));
    // One that stopped writing long ago isn't either.
    let mut old = Journal::create(dir.path(), "old-run");
    old.write(&started(&["model.orders"]));
    old.file
        .set_modified(SystemTime::now() - Duration::from_secs(3_600))
        .unwrap();

    let addr = start(snapshot(dir.path()), limits(4));
    let (status, body) = get(addr, "/api/runs/live");
    assert_eq!(status, 200);
    let live: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(live["schema_version"], 1);
    let runs = live["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{body}");
    let run = &runs[0];
    assert_eq!(run["run_id"], RUN);
    assert_eq!(run["status"], "probably_running");
    assert_eq!(run["inferred"], true);
    assert_eq!(run["command"], "ods state build");
    assert_eq!(run["nodes"], 2);
    assert_eq!(run["running"], 1);
    assert_eq!(run["finished"], 0);
    assert_eq!(run["href"], format!("lineage?live={RUN}"));
    assert_eq!(run["run_href"], format!("state/runs/{RUN}"));

    going.write(&run_finished(900, RunOutcome::Succeeded));
    // The answer is reused for a moment, so many pages polling it share one read.
    let (_, cached) = get(addr, "/api/runs/live");
    assert_eq!(cached, body, "the same answer, within its time to live");
    std::thread::sleep(Duration::from_millis(1600));
    let (_, body) = get(addr, "/api/runs/live");
    let live: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(live["runs"].as_array().unwrap().len(), 0, "{body}");
}

#[test]
fn a_nodes_card_and_the_home_banner_come_from_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = Journal::create(dir.path(), RUN);
    journal.write(&started(&["model.orders", "model.view", "model.bad"]));
    journal.write(&node_started(100, "model.orders"));
    journal.write(&node_finished(
        900,
        "model.orders",
        NodeRunStats::new(NodeRunStatus::Success)
            .with_rows_affected(Some(99))
            .with_extra("query_id", "01b2"),
    ));
    journal.write(&node_started(1_000, "model.view"));
    journal.write(&node_finished(
        1_400,
        "model.view",
        NodeRunStats::new(NodeRunStatus::Success),
    ));
    journal.write(&node_started(1_500, "model.bad"));
    let addr = start(snapshot(dir.path()), limits(4));

    // Rows as the adapter reported them, with where they came from.
    let (status, card) = get(addr, &format!("/state/runs/{RUN}/card?node=model.orders"));
    assert_eq!(status, 200, "{card}");
    assert!(card.contains(r#"data-status="success""#), "{card}");
    assert!(card.contains(">BUILT<"), "{card}");
    assert!(card.contains("from the adapter response"), "{card}");
    assert!(card.contains(">99<"), "{card}");
    assert!(card.contains("query_id 01b2"), "{card}");
    // The relation breaks between its parts, never inside a name.
    assert!(card.contains("db.<wbr>orders"), "the relation: {card}");
    assert!(
        card.contains("run after this node"),
        "its tests are still to come: {card}"
    );
    // None reported: a dash with the reason, never 0.
    let (_, card) = get(addr, &format!("/state/runs/{RUN}/card?node=model.view"));
    assert!(card.contains("not reported by the adapter"), "{card}");
    assert!(!card.contains(">0<"), "{card}");
    // Running: the page counts the time from when it started.
    let (_, card) = get(addr, &format!("/state/runs/{RUN}/card?node=model.bad"));
    assert!(card.contains("still running"), "{card}");
    assert!(card.contains("lv-so-far"), "{card}");
    // A failure: the summary as the journal keeps it, nothing more.
    journal.write(&node_finished(
        2_000,
        "model.bad",
        NodeRunStats::new(NodeRunStatus::Error)
            .with_error(ErrorSummary::from_message("KeyError: 'lifetime_value'")),
    ));
    let (_, card) = get(addr, &format!("/state/runs/{RUN}/card?node=model.bad"));
    assert!(card.contains(">FAILED<"), "{card}");
    assert!(card.contains("[value removed]"), "{card}");
    assert!(!card.contains("lifetime_value"), "{card}");
    // It built nothing: its rows and tests say so, not "not reported".
    assert!(
        card.contains("the node didn&#x27;t build") || card.contains("the node didn't build"),
        "{card}"
    );
    assert!(card.contains("the node did not build"), "{card}");
    assert_eq!(
        get(addr, &format!("/state/runs/{RUN}/card?node=model.none")).0,
        404
    );
    assert_eq!(
        get(addr, "/state/runs/no-run/card?node=model.orders").0,
        404
    );

    // Home says a run is going on, and links to it live.
    let (status, home) = get(addr, "/");
    assert_eq!(status, 200);
    assert!(
        home.contains(r#"<section class="card live-banner""#),
        "{home}"
    );
    assert!(
        home.contains("<strong>Run probably in progress</strong>"),
        "{home}"
    );
    assert!(
        home.contains(r#"class="live-inferred""#),
        "inference is said: {home}"
    );
    assert!(
        home.contains(&format!(r#"href="lineage?live={RUN}""#)),
        "{home}"
    );
    assert!(home.contains("probably running"), "{home}");
    // The Lineage page carries the live view.
    let (_, page) = get(addr, &format!("/lineage?live={RUN}"));
    assert!(page.contains("OdsLive"), "the live view's script");
    journal.write(&run_finished(2_100, RunOutcome::Failed));
    let (_, home) = get(addr, "/");
    // (The page's script names it too, to redraw it.)
    assert!(
        !home.contains(r#"<section class="card live-banner""#),
        "{home}"
    );
}

#[test]
fn a_poll_answers_at_most_its_cap_and_the_next_goes_on_from_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = Journal::create(dir.path(), RUN);
    let mut text = started(&["model.orders"]);
    for i in 0..2_050 {
        text.push_str(&node_started(i, "model.orders"));
    }
    journal.write(&text);
    let addr = start(snapshot(dir.path()), limits(1));
    let ids = |body: &str| -> Vec<u64> {
        body.lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["id"]
                    .as_u64()
                    .unwrap()
            })
            .collect()
    };
    let (_, body) = get(addr, &format!("/api/runs/{RUN}/events?since=0"));
    let first = ids(&body);
    assert_eq!(
        first.len(),
        ods_web::live::MAX_POLLED,
        "no more than the cap"
    );
    assert_eq!(first.last(), Some(&2_000));
    let (_, body) = get(addr, &format!("/api/runs/{RUN}/events?since=2000"));
    assert_eq!(ids(&body), (2_001..=2_051).collect::<Vec<_>>());
}
