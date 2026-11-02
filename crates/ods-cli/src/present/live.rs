//! Live progress for a run on a terminal (ADR-0003 §9, #446).
//!
//! One `rich_ext::live::LiveCoordinator` owns stderr while a run is in progress: the
//! run's dbt commands and their nodes are a `workflow::TaskTree` redrawn in place, and
//! everything else for stderr (dbt's output, notes) is printed above it, so the two
//! never interleave. When the run ends, a `workflow::CompletionSummary` is left in the
//! scrollback and the tree is cleared.
//!
//! It is only ever used on an interactive terminal; anywhere else progress stays the
//! plain lines it has always been. Drawing is best effort: an error stops the drawing,
//! never the run.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::sync::Mutex;

use rich::Console;
use rich::segment::Segment;
use rich_ext::capabilities::{Capabilities, ColorDepth, Environment, Overrides};
use rich_ext::live::{LiveCoordinator, RegionId};
use rich_ext::target::{RenderTarget, TargetKind};
use rich_ext::workflow::{CompletionSummary, State, TaskId, TaskTree, TaskTreeView};

use super::view::sanitize;

/// How a finished node ended, for the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It ran and succeeded.
    Succeeded,
    /// It failed.
    Failed,
    /// It didn't run (e.g. upstream failed).
    Skipped,
    /// Anything else.
    Unknown,
}

/// A run's live progress on stderr. Shared by the hooks that report it.
pub struct LiveProgress {
    inner: Mutex<Option<Inner>>,
}

struct Inner {
    live: LiveCoordinator<Box<dyn Write + Send>>,
    target: RenderTarget,
    region: RegionId,
    tree: TaskTree,
    step: Option<TaskId>,
    nodes: HashMap<String, TaskId>,
    title: String,
    /// How the run ended, once the caller knows: `None` until [`LiveProgress::close`].
    ended: Option<bool>,
}

/// stderr as `rich_ext::capabilities` sees a terminal: the process's environment,
/// with stderr's terminal status (its own `SystemEnvironment` looks at stdout).
struct StderrEnvironment;

impl Environment for StderrEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn is_terminal(&self) -> bool {
        std::io::stderr().is_terminal()
    }

    fn size(&self) -> Option<(usize, usize)> {
        let console = Console::builder().force_terminal(true).build();
        Some((console.width(), console.height()))
    }

    fn is_windows(&self) -> bool {
        cfg!(windows)
    }
}

impl LiveProgress {
    /// Starts drawing on stderr, under `title`; `color` is whether colour is allowed
    /// (ODS's `--color`, `NO_COLOR` and `TERM=dumb` rules, already applied). `None`
    /// when stderr isn't an interactive terminal or the region can't be drawn.
    pub fn start(title: &str, color: bool) -> Option<Self> {
        let overrides = Overrides {
            color: (!color).then_some(ColorDepth::None),
            hyperlinks: Some(false),
            animation: Some(false),
            ..Overrides::default()
        };
        Self::on(
            title,
            &StderrEnvironment,
            &overrides,
            Box::new(std::io::stderr()),
        )
    }

    /// Draws on `writer`, for a terminal as `environment` describes it.
    fn on(
        title: &str,
        environment: &dyn Environment,
        overrides: &Overrides,
        writer: Box<dyn Write + Send>,
    ) -> Option<Self> {
        let report = Capabilities::detect_with(environment, overrides);
        let capabilities = report.to_target_capabilities();
        if !capabilities.interactive {
            return None;
        }
        let target = RenderTarget::new(
            TargetKind::Terminal,
            capabilities,
            rich::theme::Theme::default(),
        );
        let mut live = LiveCoordinator::new(writer, target.clone());
        let tree = TaskTree::new().title(sanitize(title));
        let region = live.add(target.segments(&view(&tree))).ok()?;
        Some(Self {
            inner: Mutex::new(Some(Inner {
                live,
                target,
                region,
                tree,
                step: None,
                nodes: HashMap::new(),
                title: sanitize(title),
                ended: None,
            })),
        })
    }

    /// Runs `change` on the drawing, then redraws; stops drawing on any error.
    fn with(&self, change: impl FnOnce(&mut Inner) -> Result<(), rich_ext::live::LiveError>) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let Some(inner) = guard.as_mut() else {
            return;
        };
        let result = change(inner).and_then(|()| {
            let segments = inner.target.segments(&view(&inner.tree));
            // `update` only replaces the content; `refresh` draws it.
            inner.live.update(inner.region.clone(), segments)?;
            inner.live.refresh()
        });
        if result.is_err() {
            // Best effort: give the terminal back, and stop.
            if let Some(mut inner) = guard.take() {
                let _ = inner.live.finish();
            }
        }
    }

    /// A line for above the tree: dbt's output, or a note.
    pub fn print(&self, line: &str) {
        self.with(|inner| {
            let segments = vec![Segment::new(sanitize(line), None)];
            inner.live.print(&segments)
        });
    }

    /// The next dbt command starts; the one before it is done.
    pub fn step(&self, label: &str) {
        self.with(|inner| {
            if let Some(previous) = inner.step.take() {
                inner.tree.succeed(previous);
            }
            let id = inner.tree.add(None, sanitize(label));
            inner.tree.start(id);
            inner.step = Some(id);
            Ok(())
        });
    }

    /// A node waits to run, under the current command.
    pub fn queued(&self, node: &str, label: &str) {
        self.with(|inner| {
            let id = inner.tree.add(inner.step, sanitize(label));
            inner.nodes.insert(node.to_owned(), id);
            Ok(())
        });
    }

    /// A node started.
    pub fn started(&self, node: &str, label: &str) {
        self.with(|inner| {
            let id = node_id(inner, node, label);
            inner.tree.start(id);
            Ok(())
        });
    }

    /// Says how the run ended: whether it succeeded. Until then (or if it never comes,
    /// e.g. the run stopped on an error), the step in progress isn't taken to have
    /// succeeded.
    pub fn close(&self, succeeded: bool) {
        if let Ok(mut guard) = self.inner.lock()
            && let Some(inner) = guard.as_mut()
        {
            inner.ended = Some(succeeded);
        }
    }

    /// A node finished, with what to say about it (`built in 1.2s, 120 rows`).
    pub fn finished(&self, node: &str, label: &str, outcome: Outcome, said: &str) {
        self.with(|inner| {
            let id = node_id(inner, node, label);
            let said = sanitize(said);
            match outcome {
                Outcome::Succeeded => {
                    inner.tree.succeed(id).note(id, said);
                }
                Outcome::Failed => {
                    inner.tree.fail(id, said);
                }
                Outcome::Skipped => {
                    inner
                        .tree
                        .skip(id, (!said.is_empty()).then_some(said.as_str()));
                }
                Outcome::Unknown => {
                    inner.tree.warn(id, said);
                }
            }
            Ok(())
        });
    }
}

/// The tree as drawn. No timers: node events arrive as dbt reports them, so the time
/// between them isn't how long a node took (each node's note says that).
fn view(tree: &TaskTree) -> TaskTreeView<'_> {
    tree.view().show_durations(false)
}

fn node_id(inner: &mut Inner, node: &str, label: &str) -> TaskId {
    if let Some(id) = inner.nodes.get(node) {
        return *id;
    }
    let id = inner.tree.add(inner.step, sanitize(label));
    inner.nodes.insert(node.to_owned(), id);
    id
}

impl Drop for LiveProgress {
    /// Leaves a summary in the scrollback, then clears the tree and gives the terminal
    /// back.
    fn drop(&mut self) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let Some(mut inner) = guard.take() else {
            return;
        };
        if let Some(step) = inner.step.take() {
            // Conservative: only a run said to have succeeded closes its last step as
            // succeeded (AGENTS.md rule 3).
            match inner.ended {
                Some(true) => {
                    inner.tree.succeed(step);
                }
                Some(false) => {
                    inner.tree.fail(step, "the run didn't succeed");
                }
                None => {
                    inner.tree.warn(step, "didn't finish");
                }
            }
        }
        // The nodes, not dbt's commands, in the order they were first seen.
        let mut nodes: Vec<TaskId> = inner.nodes.values().copied().collect();
        nodes.sort_unstable();
        let mut counts = std::collections::BTreeMap::new();
        for id in &nodes {
            *counts.entry(inner.tree.state(*id)).or_insert(0) += 1;
        }
        let mut summary = CompletionSummary::new(inner.title.clone()).status(inner.tree.overall());
        for (state, count) in counts {
            summary = summary.count(state, count);
        }
        for id in nodes {
            if inner.tree.state(id) == State::Failed {
                summary = summary.item(State::Failed, inner.tree.label(id).to_owned());
            }
        }
        let segments = inner.target.segments(&summary);
        let _ = inner.live.remove(inner.region.clone());
        let _ = inner.live.print(&segments);
        let _ = inner.live.finish();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rich_ext::capabilities::MapEnvironment;

    use super::*;

    /// A writer the test can read back.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn no_color() -> Overrides {
        Overrides {
            color: Some(ColorDepth::None),
            ..Overrides::default()
        }
    }

    /// Off a terminal there is nothing to draw on: progress stays plain lines.
    #[test]
    fn nothing_is_drawn_off_a_terminal() {
        let piped = MapEnvironment::new().terminal(false);
        let out = Shared::default();
        assert!(
            LiveProgress::on(
                "ods state build",
                &piped,
                &no_color(),
                Box::new(out.clone())
            )
            .is_none()
        );
        assert!(out.0.lock().unwrap().is_empty());
    }

    /// On a terminal, the run is drawn, dbt's lines are printed above it, and a summary
    /// of the nodes is left behind: failed ones by name.
    #[test]
    fn a_run_is_drawn_then_summed_up() {
        let terminal = MapEnvironment::tty().size(80, 24);
        let out = Shared::default();
        let live = LiveProgress::on(
            "ods state build",
            &terminal,
            &no_color(),
            Box::new(out.clone()),
        )
        .expect("a terminal to draw on");
        live.step("1/1 dbt build: 2 nodes");
        live.print("12:00:00  Running with dbt");
        live.queued("model.p.orders", "orders");
        live.queued("model.p.customers", "customers");
        live.started("model.p.orders", "orders");
        live.finished(
            "model.p.orders",
            "orders",
            Outcome::Succeeded,
            "built in 1s",
        );
        live.finished(
            "model.p.customers",
            "customers",
            Outcome::Failed,
            "failed in 1s",
        );
        drop(live);
        let shown = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        assert!(shown.contains("12:00:00  Running with dbt"), "{shown}");
        assert!(shown.contains("1/1 dbt build: 2 nodes"), "{shown}");
        assert!(shown.contains("built in 1s"), "{shown}");
        assert!(shown.contains("1 failed, 1 succeeded"), "{shown}");
        // The summary is what stays: it comes last.
        let summary = shown.rfind("1 failed").unwrap();
        assert!(shown.rfind("built in 1s").unwrap() < summary, "{shown}");
    }

    /// A run that ends without saying it succeeded (it stopped on an error before any
    /// node ran) is never summed up as a success; one that says it failed, as failed.
    #[test]
    fn an_unconfirmed_run_is_not_summed_up_as_a_success() {
        for (close, expected) in [
            (None, "warning ods state build"),
            (Some(false), "error ods state build"),
        ] {
            let terminal = MapEnvironment::tty().size(80, 24);
            let out = Shared::default();
            let live = LiveProgress::on(
                "ods state build",
                &terminal,
                &no_color(),
                Box::new(out.clone()),
            )
            .expect("a terminal to draw on");
            live.step("2/5 dbt compile");
            if let Some(succeeded) = close {
                live.close(succeeded);
            }
            drop(live);
            let shown = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
            let summary = &shown[shown.rfind("ods state build").unwrap()..];
            assert!(!summary.contains("ok ods state build"), "{shown}");
            assert!(shown.contains(expected), "{expected} in {shown}");
        }
    }

    /// What a node or dbt says can't drive the terminal: it is sanitized first.
    #[test]
    fn nothing_in_the_progress_can_drive_the_terminal() {
        let terminal = MapEnvironment::tty().size(80, 24);
        let out = Shared::default();
        let live = LiveProgress::on(
            "ods state build",
            &terminal,
            &no_color(),
            Box::new(out.clone()),
        )
        .expect("a terminal to draw on");
        live.print("a\x1b]0;title\x07b");
        live.step("x\x1b[2Jy");
        drop(live);
        let shown = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        assert!(!shown.contains("\x1b]0;"), "{shown:?}");
        assert!(!shown.contains("\x1b[2J"), "{shown:?}");
        assert!(!shown.contains('\x07'), "{shown:?}");
    }
}
