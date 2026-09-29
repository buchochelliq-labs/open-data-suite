//! dbt's progress as ODS run events (#322, ADR-0024).
//!
//! With `--log-format json`, dbt prints one JSON object per line: `info` (the event's
//! `name`, `level`, `ts`, `thread`, `invocation_id` and the human `msg`) and `data`.
//! The [`Bridge`] reads two of dbt's structured events, both public in dbt-core's event
//! definitions:
//! - `NodeStart`: a node started (`data.node_info.unique_id`, on `info.thread`);
//! - `NodeFinished`: it finished, with `data.run_result`: `status`, `message`,
//!   `timing_info` (the `compile` and `execute` steps), `thread`, `execution_time` and
//!   `adapter_response`.
//!
//! Both are debug-level, so the executor asks for `--log-level debug` and prints only
//! the lines at `info` and above, as dbt would have. Nothing else in a line is read or
//! kept: debug events carry the SQL dbt runs and the `--vars` it was given.
//!
//! When dbt has exited, [`Bridge::finish`] fills in what the log didn't say from
//! `run_results.json` and the report: every requested node finishes exactly once, with
//! the report's status. Unknown or missing fields are missing stats, never zero or
//! success.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::state::TimestampMs;
use ods_sdk::contracts::executor::{ExecutionMode, ExecutionReport, ExecutionRequest};
use ods_sdk::contracts::run_events::{
    CheckStatus, ErrorSummary, NodeRunStats, NodeRunStatus, RunEvent, RunEventKind, RunEventSink,
    RunOutcome,
};
use serde_json::Value;

use crate::runs::{ResultDetails, RunResults, RunStatus, seconds_to_ms};

/// dbt's error headers: `<kind> Error in <resource type> <name> (<path>)`.
const HEADER_KINDS: [&str; 6] = [
    "Runtime Error",
    "Database Error",
    "Compilation Error",
    "Dependency Error",
    "Parsing Error",
    "Python Error",
];

/// Where an error's full text is.
const DETAILS_AT: &str = "dbt's log file (logs/dbt.log in the project, unless --log-path)";

/// How severe a dbt log line is, and the least severe a person sees (dbt's
/// `--log-level`, or `DBT_LOG_LEVEL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum LogLevel {
    /// dbt's `debug`.
    Debug,
    /// dbt's `info`, the default.
    Info,
    /// dbt's `warn`.
    Warn,
    /// dbt's `error`.
    Error,
    /// Nothing is shown (`none`).
    None,
}

impl LogLevel {
    /// dbt's name for a level, in any case; `None` for anything else.
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// A line of dbt's output for people, as the bridge read it. The caller renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DbtLine {
    /// When dbt logged it, `HH:MM:SS` in UTC, if it said.
    pub time: Option<String>,
    /// What dbt said.
    pub text: String,
}

/// What a line that can't be read shows as: it may be a structured line that holds SQL
/// or options, so it isn't shown.
pub const UNREADABLE_LINE: &str = "[an unreadable dbt log line is hidden]";

/// Turns dbt's JSON log lines, and its run results at the end, into run events.
pub struct Bridge<'a> {
    sink: &'a dyn RunEventSink,
    scope: Option<String>,
    mode: ExecutionMode,
    requested: Vec<String>,
    requested_set: BTreeSet<String>,
    /// Check id → the nodes it reads, from the manifest.
    coverage: BTreeMap<String, Vec<String>>,
    /// The least severe line shown.
    show_from: LogLevel,
    run_id: Option<String>,
    last: Option<TimestampMs>,
    /// Nodes finished from the log, with the stats they finished with.
    finished: BTreeMap<String, NodeRunStats>,
    checks: BTreeSet<String>,
    ended: bool,
}

impl<'a> Bridge<'a> {
    /// A bridge for `request`'s run, reporting to `sink`. `coverage` maps each check
    /// (test) to the nodes it reads; lines at `show_from` and above are shown.
    pub fn new(
        sink: &'a dyn RunEventSink,
        request: &ExecutionRequest,
        coverage: BTreeMap<String, Vec<String>>,
        show_from: LogLevel,
    ) -> Self {
        let requested: Vec<String> = request.nodes.iter().map(|n| n.id.clone()).collect();
        Self {
            sink,
            scope: request.scope.clone(),
            mode: request.mode,
            requested_set: requested.iter().cloned().collect(),
            requested,
            coverage,
            show_from,
            run_id: None,
            last: None,
            finished: BTreeMap::new(),
            checks: BTreeSet::new(),
            ended: false,
        }
    }

    /// Whether the log has started the run: dbt printed a structured line.
    pub fn started(&self) -> bool {
        self.run_id.is_some()
    }

    fn emit(&mut self, at: Option<TimestampMs>, kind: RunEventKind) {
        // Times never go backwards: a line's own time, or the last one's.
        let at = match (at, self.last) {
            (Some(at), Some(last)) => at.max(last),
            (Some(at), None) => at,
            (None, Some(last)) => last,
            (None, None) => TimestampMs::now(),
        };
        self.last = Some(at);
        let run_id = self.run_id.clone().unwrap_or_default();
        self.sink
            .emit(RunEvent::new(run_id, self.scope.clone(), at, kind));
    }

    fn start(&mut self, run_id: String, at: Option<TimestampMs>, live: bool) {
        self.run_id = Some(run_id);
        self.emit(
            at,
            RunEventKind::RunStarted {
                nodes: self.requested.clone(),
                mode: self.mode,
                live,
            },
        );
        // In a test run nothing is built: nodes finish with their checks' outcome.
        if self.mode != ExecutionMode::Test {
            for node in self.requested.clone() {
                self.emit(at, RunEventKind::NodeQueued { node });
            }
        }
    }

    /// Reads one line of dbt's standard output. Returns what people should see for it,
    /// failing closed (AGENTS.md rule 9), since dbt's debug lines hold the SQL it runs
    /// and the options it was given:
    /// - one of dbt's JSON lines shows its `msg` only when its `level` is known and at
    ///   least the level shown; a line without a level shows nothing;
    /// - a line that starts with `{` but can't be read as one (cut, merged with other
    ///   output, a number out of range) shows as [`UNREADABLE_LINE`];
    /// - any other line (e.g. a Python model's `print`) is shown as it is, unless it
    ///   holds a `{`, which could be a JSON line merged into it: then it shows as
    ///   [`UNREADABLE_LINE`] too.
    pub fn line(&mut self, line: &str) -> Option<DbtLine> {
        let hidden = || {
            Some(DbtLine {
                time: None,
                text: UNREADABLE_LINE.to_owned(),
            })
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        if !trimmed.starts_with('{') {
            return if trimmed.contains('{') {
                hidden()
            } else {
                Some(DbtLine {
                    time: None,
                    text: line.to_owned(),
                })
            };
        }
        let Ok(Value::Object(event)) = serde_json::from_str::<Value>(trimmed) else {
            return hidden();
        };
        let Some(info) = event.get("info").and_then(Value::as_object) else {
            return hidden();
        };
        let text = |key: &str| info.get(key).and_then(Value::as_str);
        let ts = text("ts").and_then(|t| TimestampMs::parse(t).ok());
        if !self.started()
            && !self.ended
            && let Some(id) = text("invocation_id").filter(|id| !id.is_empty())
        {
            self.start(id.to_owned(), ts, true);
        }
        let data = event.get("data");
        match text("name") {
            Some("NodeStart") => self.node_start(data, ts, text("thread")),
            Some("NodeFinished") => self.node_finished(data, ts),
            _ => {}
        }
        let level = text("level").and_then(LogLevel::parse)?;
        if level == LogLevel::None || level < self.show_from || self.show_from == LogLevel::None {
            return None;
        }
        let msg = text("msg")?;
        Some(DbtLine {
            time: text("ts")
                .and_then(|t| t.get(11..19))
                .filter(|t| t.bytes().all(|b| b.is_ascii_digit() || b == b':'))
                .map(str::to_owned),
            text: msg.to_owned(),
        })
    }

    fn node_start(&mut self, data: Option<&Value>, ts: Option<TimestampMs>, thread: Option<&str>) {
        let Some(id) = unique_id(data) else { return };
        if self.mode == ExecutionMode::Test
            || !self.requested_set.contains(id)
            || self.finished.contains_key(id)
            || !self.started()
        {
            return;
        }
        let node = id.to_owned();
        self.emit(
            ts,
            RunEventKind::NodeStarted {
                node,
                thread: thread.map(str::to_owned),
            },
        );
    }

    fn node_finished(&mut self, data: Option<&Value>, ts: Option<TimestampMs>) {
        let Some(id) = unique_id(data).map(str::to_owned) else {
            return;
        };
        if !self.started() {
            return;
        }
        let result = data.and_then(|d| d.get("run_result"));
        let status = result
            .and_then(|r| r.get("status"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if is_check(&id) {
            if self.checks.insert(id.clone()) {
                let covers = self.covers(&id);
                self.emit(
                    ts,
                    RunEventKind::CheckFinished {
                        check: id,
                        covers,
                        status: check_status(status),
                    },
                );
            }
            return;
        }
        if self.mode == ExecutionMode::Test
            || !self.requested_set.contains(&id)
            || self.finished.contains_key(&id)
        {
            return;
        }
        let details = result.map(log_details).unwrap_or_default();
        let finished = node_stats(node_status(status), &details);
        self.finished.insert(id.clone(), finished.clone());
        self.emit(
            ts,
            RunEventKind::NodeFinished {
                node: id,
                stats: finished,
            },
        );
    }

    fn covers(&self, check: &str) -> Vec<String> {
        let mut covers = self.coverage.get(check).cloned().unwrap_or_default();
        covers.sort();
        covers.dedup();
        covers
    }

    /// After dbt exited with results: every requested node that hasn't finished
    /// finishes as the report says, with what `run_results.json` says about it; checks
    /// the log didn't show finish; then the run. If the log never started the run,
    /// it starts now, not live.
    pub fn finish(&mut self, report: &ExecutionReport, run: &RunResults) {
        if self.ended {
            return;
        }
        let at = TimestampMs::from(report.finished_at);
        if !self.started() {
            self.start(
                report.run_id.clone(),
                Some(report.started_at.map_or(at, TimestampMs::from)),
                false,
            );
        }
        let results: BTreeMap<&str, &crate::runs::NodeResult> = run
            .results
            .iter()
            .map(|r| (r.unique_id.as_str(), r))
            .collect();
        for node in &report.nodes {
            let status = NodeRunStatus::from(node.status);
            let finished = match self.finished.get(&node.node) {
                // The log finished it as the report says: nothing to add.
                Some(live) if live.status == status => continue,
                // The report's status wins (ADR-0024): the node finishes again, with the
                // log's stats and the report's status.
                Some(live) => {
                    let mut corrected = live.clone();
                    corrected.status = status;
                    if status != NodeRunStatus::Error {
                        corrected.error = None;
                    }
                    corrected
                }
                None => {
                    // A test run built nothing: its nodes have no execution of their own.
                    let details = match results.get(node.node.as_str()) {
                        Some(r) if self.mode != ExecutionMode::Test => r.details.clone(),
                        _ => ResultDetails::default(),
                    };
                    node_stats(status, &details)
                }
            };
            self.finished.insert(node.node.clone(), finished.clone());
            self.emit(
                Some(at),
                RunEventKind::NodeFinished {
                    node: node.node.clone(),
                    stats: finished,
                },
            );
        }
        for result in &run.results {
            if is_check(&result.unique_id) && !self.checks.contains(&result.unique_id) {
                self.checks.insert(result.unique_id.clone());
                let covers = self.covers(&result.unique_id);
                self.emit(
                    Some(at),
                    RunEventKind::CheckFinished {
                        check: result.unique_id.clone(),
                        covers,
                        status: check_status(&result.raw_status),
                    },
                );
            }
        }
        self.end(
            Some(at),
            if report.succeeded {
                RunOutcome::Succeeded
            } else {
                RunOutcome::Failed
            },
        );
    }

    /// The execution ended without results it could read: the run, if the log
    /// started it, finishes with an unknown outcome, and so do its unfinished nodes.
    pub fn abort(&mut self) {
        if self.started() && !self.ended {
            self.end(None, RunOutcome::Unknown);
        }
        self.ended = true;
    }

    fn end(&mut self, at: Option<TimestampMs>, outcome: RunOutcome) {
        self.emit(at, RunEventKind::RunFinished { outcome });
        self.ended = true;
    }
}

/// Every check (data test and unit test) in `manifest`, with the nodes it reads: what
/// a [`Bridge`] needs to tell which nodes a check covers.
pub fn coverage(manifest: &crate::Manifest) -> BTreeMap<String, Vec<String>> {
    manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == crate::ResourceType::Test)
        .map(|n| (n.unique_id.clone(), n.depends_on.clone()))
        .chain(
            manifest
                .unit_tests
                .iter()
                .map(|t| (t.unique_id.clone(), t.depends_on.clone())),
        )
        .collect()
}

fn is_check(id: &str) -> bool {
    id.starts_with("test.") || id.starts_with("unit_test.")
}

fn unique_id(data: Option<&Value>) -> Option<&str> {
    data?
        .get("node_info")?
        .get("unique_id")?
        .as_str()
        .filter(|id| !id.is_empty())
}

/// As the report reads dbt's status for a requested node, so the two always agree.
/// dbt's status word in a log event, as a node's status. A missing or unknown word is
/// `unknown` (rule 3); the report's status, from `run_results.json`, wins at the end.
fn node_status(status: &str) -> NodeRunStatus {
    match RunStatus::parse(status) {
        RunStatus::Success => NodeRunStatus::Success,
        RunStatus::Skipped => NodeRunStatus::Skipped,
        RunStatus::Failed => NodeRunStatus::Error,
        _ => NodeRunStatus::Unknown,
    }
}

fn check_status(status: &str) -> CheckStatus {
    match status {
        "pass" | "success" => CheckStatus::Passed,
        "warn" => CheckStatus::Warned,
        "fail" | "error" | "runtime error" => CheckStatus::Failed,
        "skipped" => CheckStatus::Skipped,
        _ => CheckStatus::Unknown,
    }
}

/// A log event's `run_result`, read as `run_results.json`'s fields are.
fn log_details(result: &Value) -> ResultDetails {
    let text = |key: &str| {
        result
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let timing = result
        .get("timing_info")
        .and_then(Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .filter_map(|t| {
                    let field = |k: &str| t.get(k).and_then(Value::as_str).map(str::to_owned);
                    Some((field("name")?, field("started_at"), field("completed_at")))
                })
                .collect()
        })
        .unwrap_or_default();
    ResultDetails {
        timing,
        execution_ms: result
            .get("execution_time")
            .and_then(Value::as_f64)
            .and_then(seconds_to_ms),
        thread: text("thread"),
        adapter_response: result.get("adapter_response").cloned(),
        message: text("message"),
    }
}

/// A node's stats from what dbt reported. Timing, rows and extras count only for a
/// node that ran (succeeded or failed): a skipped node's zero execution time is not a
/// duration.
pub fn node_stats(outcome: NodeRunStatus, details: &ResultDetails) -> NodeRunStats {
    let mut stats = NodeRunStats::new(outcome);
    if let Some(thread) = &details.thread {
        stats = stats.with_thread(thread.clone());
    }
    if !matches!(outcome, NodeRunStatus::Success | NodeRunStatus::Error) {
        return stats;
    }
    let parse = |t: &Option<String>| t.as_deref().and_then(|t| TimestampMs::parse(t).ok());
    let step = |name: &str| {
        details
            .timing
            .iter()
            .find(|(n, ..)| n == name)
            .and_then(|(_, start, end)| parse(end)?.millis_since(parse(start)?))
    };
    let started = details.timing.iter().filter_map(|(_, s, _)| parse(s)).min();
    let finished = details.timing.iter().filter_map(|(_, _, e)| parse(e)).max();
    stats = stats.with_times(started, finished).with_durations(
        details.execution_ms,
        step("compile"),
        step("execute"),
    );
    if let Some(Value::Object(response)) = &details.adapter_response {
        stats = stats.with_rows_affected(response.get("rows_affected").and_then(whole_number));
        for (key, value) in response {
            // `_message` is free text (it can echo a statement); rows are a stat.
            if key.starts_with('_') || key == "rows_affected" {
                continue;
            }
            let value = match value {
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => {
                    whole_number(value).map_or_else(|| n.to_string(), |i| i.to_string())
                }
                _ => continue,
            };
            stats = stats.with_extra(key.clone(), value);
        }
    }
    if outcome == NodeRunStatus::Error {
        stats = stats.with_error(details.message.as_deref().and_then(error_summary));
    }
    stats
}

/// A JSON number that is a whole number (log events write `4.0`).
fn whole_number(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        let f = value.as_f64()?;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "checked whole and within i64's exactly representable range"
        )]
        (f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15).then_some(f as i64)
    })
}

/// The kind of a dbt error header line, `Runtime Error in model orders
/// (models/orders.sql)`: one of [`HEADER_KINDS`], then `in`, a resource type, a name
/// and a parenthesised path ending the line. `None` for any other line.
fn header_kind(line: &str) -> Option<&'static str> {
    let kind = HEADER_KINDS.iter().find(|k| line.starts_with(*k))?;
    let rest = line[kind.len()..].strip_prefix(" in ")?;
    let mut words = rest.splitn(3, ' ');
    let (resource, name, path) = (words.next()?, words.next()?, words.next()?);
    let word = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_alphanumeric() || c == '_');
    (word(resource) && !name.is_empty() && path.starts_with('(') && path.ends_with(')'))
        .then_some(*kind)
}

/// Whether a line is the SQL echo some adapters add (`LINE 35: where …`).
fn is_sql_echo(line: &str) -> bool {
    line.strip_prefix("LINE ").is_some_and(|rest| {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with(':')
    })
}

/// dbt's error message, summarised. dbt starts it with a header naming the kind and the
/// node (`Runtime Error in model orders (models/orders.sql)`) and puts the engine's own
/// message on the next line; the summary is that line, with the header's kind when
/// the line has none. SQL echo lines (`LINE 35: …`) are never used.
pub fn error_summary(message: &str) -> Option<ErrorSummary> {
    let mut lines = message
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !is_sql_echo(l));
    let first = lines.next()?;
    let summary = match (header_kind(first), lines.next()) {
        (Some(kind), Some(detail)) => {
            let summary = ErrorSummary::from_message(detail)?;
            if summary.kind().is_none() {
                summary.with_kind(kind)
            } else {
                summary
            }
        }
        (Some(kind), None) => ErrorSummary::from_message(kind)?.with_kind(kind),
        (None, _) => ErrorSummary::from_message(first)?,
    };
    Some(summary.with_details_at(DETAILS_AT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbt_error_headers_give_the_kind_and_the_next_line_the_message() {
        let s = error_summary(
            "Runtime Error in model orders (models/marts/orders.sql)\n  Dependency Error: Cannot alter entry \"orders\" because there are entries that depend on it.",
        )
        .unwrap();
        assert_eq!(s.kind(), Some("Dependency Error"));
        assert_eq!(
            s.message(),
            "Dependency Error: Cannot alter entry [value removed] because there are entries that depend on it."
        );
        let s = error_summary(
            "Compilation Error in model x (models/x.sql)\n  column 'sk_live' is not there",
        )
        .unwrap();
        assert_eq!(s.kind(), Some("Compilation Error"));
        assert_eq!(s.message(), "column [value removed] is not there");
        assert!(s.details_at().is_some());
        let s = error_summary("KeyError: 'segment'").unwrap();
        assert_eq!(s.message(), "KeyError: [value removed]");
        assert_eq!(error_summary("  \n"), None);
        // Not a dbt header, though it has " in " and ends in `)`; and the SQL echo
        // line is never used (#322 review).
        let s = error_summary(
            "Binder Error: column not found in table orders (did you mean total)\nLINE 35: where cast(secret_col as integer) = ssn_col",
        )
        .unwrap();
        assert_eq!(s.kind(), Some("Binder Error"));
        assert!(!s.message().contains("secret_col") && !s.message().contains("ssn_col"));
        let s = error_summary(
            "Database Error in model x (models/x.sql)\n  LINE 3: where token = 'sk_1'\n  002003 (42S02): SQL compilation error:\n  Object does not exist",
        )
        .unwrap();
        assert_eq!(s.kind(), Some("Database Error"));
        assert!(s.message().ends_with("SQL compilation error:"), "{s:?}");
        assert!(!format!("{s:?}").contains("sk_1"));
    }

    #[test]
    fn whole_numbers_read_from_floats_and_ints() {
        assert_eq!(whole_number(&serde_json::json!(4.0)), Some(4));
        assert_eq!(whole_number(&serde_json::json!(-1)), Some(-1));
        assert_eq!(whole_number(&serde_json::json!(1.5)), None);
        assert_eq!(whole_number(&serde_json::json!("4")), None);
    }
}
