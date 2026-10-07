//! A run by thread (#355): one row per thread the engine ran nodes on, one bar per node,
//! the idle gaps between them, and the critical path, the chain of dependencies that
//! set the run's length. Built from the run's journal (ADR-0024) and the parents each
//! build recorded (else the project's now, as inferred).
//!
//! Only what the journal says is drawn (AGENTS.md rule 3): a node without a start or a
//! finish gets no bar, and a journal rebuilt from a final report (`live: false`), which
//! has no times, gets none at all.

use std::collections::{BTreeMap, BTreeSet};

use ods_sdk::contracts::run_events::NodeRunStatus;
use serde::Serialize;

use super::journal::{NodeStatsView, duration};
use super::state::NodeRef;

/// The run by thread, and `/api/state/runs/<run>`'s `threads`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ThreadsView {
    /// The run's length in milliseconds, start to its last finish, when timed.
    pub span_ms: Option<u64>,
    /// One row per thread, by name; a node whose thread wasn't reported is under
    /// [`UNREPORTED`].
    pub threads: Vec<ThreadRow>,
    /// The critical path, first node first: from the node that finished last, back
    /// through the dependency that finished last, until a node with none in this run.
    pub critical_path: Vec<NodeRef>,
    /// How long the critical path's nodes took together, for people.
    pub critical_took: Option<String>,
    /// Whether the path rests on dependencies the run didn't record (it recorded no
    /// snapshot, or not these nodes), taken from the project as it is now: inferred,
    /// not known.
    pub critical_inferred: bool,
    /// Why there are no bars, when there are none.
    pub untimed: Option<String>,
    /// Nodes with a finish but no start: only when they finished is known, so they get
    /// a mark, never a bar.
    pub finishes_only: Vec<Finish>,
}

/// What a node's thread is called when the engine didn't say.
pub const UNREPORTED: &str = "thread not reported";

/// One thread's nodes, in the order they started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ThreadRow {
    /// Its name, e.g. `Thread-1 (worker)`.
    pub thread: String,
    /// Its nodes.
    pub bars: Vec<Bar>,
    /// Milliseconds it spent running nodes.
    pub busy_ms: u64,
    /// Milliseconds of the run it spent idle, from the run's start to its last finish.
    pub idle_ms: u64,
    /// Its idle stretches, as start and end offsets in milliseconds.
    pub gaps: Vec<(u64, u64)>,
}

/// One node on its thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Bar {
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
    /// How it ended.
    pub status: NodeRunStatus,
    /// The same, for people.
    pub status_label: &'static str,
    /// Milliseconds from the run's start to its start.
    pub start_ms: u64,
    /// Milliseconds from the run's start to its finish.
    pub end_ms: u64,
    /// How long it took, for people.
    pub took: String,
    /// Whether it is on the critical path.
    pub critical: bool,
}

/// A node only known to have finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Finish {
    /// Its id.
    pub node: String,
    /// Its name.
    pub name: String,
    /// How it ended.
    pub status: NodeRunStatus,
    /// Milliseconds from the run's start to its finish, if the run's start is known.
    pub end_ms: Option<u64>,
}

/// A node's bar, for the critical path: its id, start and finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timed<'a> {
    /// Its id.
    pub node: &'a str,
    /// Its start, in milliseconds from the run's start.
    pub start_ms: u64,
    /// Its finish, likewise.
    pub end_ms: u64,
}

/// The critical path through `bars`, first node first: the node that finished last,
/// preceded by its parent (from `parents`) that finished last, and so on until a node
/// has no parent among `bars`. Ties go to the earlier start, then the smaller id, so the
/// path is the same every time.
pub fn critical_path<'a>(
    bars: &[Timed<'a>],
    parents: &dyn Fn(&str) -> Vec<String>,
) -> Vec<&'a str> {
    let by_id: BTreeMap<&str, &Timed<'a>> = bars.iter().map(|b| (b.node, b)).collect();
    let last = |candidates: &mut dyn Iterator<Item = &Timed<'a>>| {
        candidates
            .max_by(|a, b| {
                a.end_ms
                    .cmp(&b.end_ms)
                    .then_with(|| b.start_ms.cmp(&a.start_ms))
                    .then_with(|| b.node.cmp(a.node))
            })
            .map(|b| b.node)
    };
    let Some(mut at) = last(&mut bars.iter()) else {
        return Vec::new();
    };
    let mut path = vec![at];
    let mut seen = BTreeSet::from([at]);
    loop {
        let parents = parents(at);
        let mut timed = parents
            .iter()
            .filter_map(|p| by_id.get(p.as_str()).copied())
            .filter(|b| !seen.contains(b.node));
        let Some(next) = last(&mut timed) else {
            break;
        };
        path.push(next);
        seen.insert(next);
        at = next;
    }
    path.reverse();
    path
}

/// A node's parents, and whether the run recorded them (else they are the project's
/// now).
pub(crate) type Parents<'a> = &'a dyn Fn(&str) -> (Vec<String>, bool);

/// The run by thread, from its `nodes` (with offsets from its start), each node's
/// `parents`, the run's length `span_ms` if known, and whether its journal was written
/// as it ran (`live`).
pub(crate) fn threads(
    nodes: &[NodeStatsView],
    parents: Parents<'_>,
    span_ms: Option<u64>,
    live: bool,
) -> ThreadsView {
    // A journal rebuilt from final results gets no bar, whatever times it holds; nor
    // does a node that never ran, whatever times its engine gave it.
    let has_bar = |n: &NodeStatsView| {
        live && n.start_offset_ms.is_some()
            && n.end_offset_ms.is_some()
            && !matches!(n.status, NodeRunStatus::Skipped | NodeRunStatus::Queued)
    };
    let timed: Vec<&NodeStatsView> = nodes.iter().filter(|n| has_bar(n)).collect();
    let finishes_only: Vec<Finish> = nodes
        .iter()
        .filter(|n| !has_bar(n) && n.finished_at.is_some())
        .filter(|n| !matches!(n.status, NodeRunStatus::Skipped | NodeRunStatus::Queued))
        .map(|n| Finish {
            node: n.node.clone(),
            name: n.name.clone(),
            status: n.status,
            end_ms: n.end_offset_ms,
        })
        .collect();
    if timed.is_empty() {
        let untimed = if nodes.is_empty() {
            "No node ran in this run, or it has no journal.".to_owned()
        } else if !live {
            "This run's journal was rebuilt from its final results, which have no start times: no bar is drawn. The Nodes tab lists each node's outcome.".to_owned()
        } else {
            "Its journal gives no node both a start and a finish: no bar is drawn.".to_owned()
        };
        return ThreadsView {
            untimed: Some(untimed),
            finishes_only,
            ..ThreadsView::default()
        };
    }
    let bars: Vec<Timed<'_>> = timed
        .iter()
        .map(|n| Timed {
            node: &n.node,
            start_ms: n.start_offset_ms.unwrap_or(0),
            end_ms: n
                .end_offset_ms
                .unwrap_or(0)
                .max(n.start_offset_ms.unwrap_or(0)),
        })
        .collect();
    let path = critical_path(&bars, &|id| parents(id).0);
    let critical_inferred = path.iter().any(|id| !parents(id).1);
    let on_path: BTreeSet<&str> = path.iter().copied().collect();
    let span = bars
        .iter()
        .map(|b| b.end_ms)
        .max()
        .into_iter()
        .chain(span_ms)
        .max()
        .unwrap_or(0);
    let threads = rows(&timed, &bars, &on_path, span);
    let names: BTreeMap<&str, &str> = timed
        .iter()
        .map(|n| (n.node.as_str(), n.name.as_str()))
        .collect();
    let critical_ms: u64 = path
        .iter()
        .filter_map(|id| bars.iter().find(|b| b.node == *id))
        .map(|b| b.end_ms - b.start_ms)
        .sum();
    ThreadsView {
        span_ms: Some(span),
        threads,
        critical_took: (!path.is_empty()).then(|| duration(critical_ms)),
        critical_inferred,
        critical_path: path
            .iter()
            .map(|id| NodeRef {
                node: (*id).to_owned(),
                name: names.get(id).copied().unwrap_or(id).to_owned(),
            })
            .collect(),
        untimed: None,
        finishes_only,
    }
}

/// The `timed` nodes (with their `bars`) by thread, each thread's bars by start.
fn rows(
    timed: &[&NodeStatsView],
    bars: &[Timed<'_>],
    on_path: &BTreeSet<&str>,
    span: u64,
) -> Vec<ThreadRow> {
    let mut rows: BTreeMap<String, Vec<Bar>> = BTreeMap::new();
    for (n, b) in timed.iter().zip(bars) {
        rows.entry(n.thread.clone().unwrap_or_else(|| UNREPORTED.to_owned()))
            .or_default()
            .push(Bar {
                node: n.node.clone(),
                name: n.name.clone(),
                status: n.status,
                status_label: n.status_label,
                start_ms: b.start_ms,
                end_ms: b.end_ms,
                took: duration(b.end_ms - b.start_ms),
                critical: on_path.contains(n.node.as_str()),
            });
    }
    rows.into_iter()
        .map(|(thread, mut bars)| {
            bars.sort_by(|a, b| {
                (a.start_ms, a.end_ms, &a.node).cmp(&(b.start_ms, b.end_ms, &b.node))
            });
            let (busy_ms, gaps) = busy_and_gaps(&bars, span);
            ThreadRow {
                thread,
                idle_ms: gaps.iter().map(|(s, e)| e - s).sum(),
                busy_ms,
                gaps,
                bars,
            }
        })
        .collect()
}

/// How long `bars` (sorted by start) kept their thread busy, and the stretches of
/// `0..span` it was idle. Overlapping bars (a thread the engine reused) count once.
fn busy_and_gaps(bars: &[Bar], span: u64) -> (u64, Vec<(u64, u64)>) {
    let mut busy = 0;
    let mut gaps = Vec::new();
    let mut covered = 0;
    for bar in bars {
        if bar.start_ms > covered {
            gaps.push((covered, bar.start_ms));
        }
        if bar.end_ms > covered {
            busy += bar.end_ms - bar.start_ms.max(covered);
            covered = bar.end_ms;
        }
    }
    if span > covered {
        gaps.push((covered, span));
    }
    (busy, gaps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(node: &str, start_ms: u64, end_ms: u64) -> Timed<'_> {
        Timed {
            node,
            start_ms,
            end_ms,
        }
    }

    fn parents_of(edges: &[(&str, &str)]) -> impl Fn(&str) -> Vec<String> {
        let edges: Vec<(String, String)> = edges
            .iter()
            .map(|(c, p)| ((*c).to_owned(), (*p).to_owned()))
            .collect();
        move |child: &str| {
            edges
                .iter()
                .filter(|(c, _)| c == child)
                .map(|(_, p)| p.clone())
                .collect()
        }
    }

    #[test]
    fn the_path_follows_the_dependency_that_finished_last() {
        // a and b feed c; b finished later, so the run waited on it, not on a. d ran
        // beside them and finished before c.
        let bars = [
            bar("a", 0, 100),
            bar("b", 0, 400),
            bar("c", 400, 900),
            bar("d", 100, 600),
        ];
        let parents = parents_of(&[("c", "a"), ("c", "b")]);
        assert_eq!(critical_path(&bars, &parents), ["b", "c"]);
    }

    #[test]
    fn a_long_chain_wins_over_a_long_node() {
        // x is the longest node, but the run ended with z, which waited on y and w.
        let bars = [
            bar("w", 0, 300),
            bar("x", 0, 700),
            bar("y", 300, 600),
            bar("z", 600, 1000),
        ];
        let parents = parents_of(&[("y", "w"), ("z", "y")]);
        assert_eq!(critical_path(&bars, &parents), ["w", "y", "z"]);
    }

    #[test]
    fn parents_that_didnt_run_end_the_path_and_cycles_dont_loop() {
        // `src` is a source, never a bar; a recorded cycle can't make it loop.
        let bars = [bar("a", 0, 100), bar("b", 100, 300)];
        let parents = parents_of(&[("a", "src"), ("b", "a"), ("a", "b")]);
        assert_eq!(critical_path(&bars, &parents), ["a", "b"]);
        assert_eq!(critical_path(&[], &parents), Vec::<&str>::new());
    }

    #[test]
    fn ties_are_broken_the_same_way_every_time() {
        let bars = [bar("b", 0, 500), bar("a", 0, 500), bar("c", 100, 500)];
        let none = |_: &str| Vec::new();
        assert_eq!(
            critical_path(&bars, &none),
            ["a"],
            "earlier start, then smaller id"
        );
    }

    fn timed_bar(node: &str, start_ms: u64, end_ms: u64) -> Bar {
        Bar {
            node: node.to_owned(),
            name: node.to_owned(),
            status: NodeRunStatus::Success,
            status_label: "built",
            start_ms,
            end_ms,
            took: String::new(),
            critical: false,
        }
    }

    #[test]
    fn idle_stretches_cover_the_run_outside_the_bars() {
        let bars = [timed_bar("a", 100, 300), timed_bar("b", 500, 600)];
        assert_eq!(
            busy_and_gaps(&bars, 1000),
            (300, vec![(0, 100), (300, 500), (600, 1000)])
        );
        // Overlaps count once.
        let bars = [timed_bar("a", 0, 400), timed_bar("b", 200, 500)];
        assert_eq!(busy_and_gaps(&bars, 500), (500, vec![]));
    }
}
