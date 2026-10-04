//! The live view's decisions (`assets/live.js`, #322), run under Node when it is
//! installed: how a run's events fold into node states, and where follow mode puts the
//! camera. The page around them is tested in a browser (`tests/browser/live_view.py`).

use std::process::{Command, Stdio};

/// Loads the script with no page around it and runs the checks on its `OdsLive`.
const CHECKS: &str = r#"
const assert = require("assert");
const fs = require("fs");
const L = new Function(fs.readFileSync(process.argv[1], "utf8") + "\nreturn OdsLive;")();

const at = s => new Date(Date.UTC(2026, 0, 1) + s * 1000).toISOString();
const ev = (s, kind, fields) => Object.assign({ run_id: "r", scope: "p/dev", at: at(s), kind }, fields);

// ---- folding events
{
  const run = L.newRun("r");
  L.apply(run, ev(0, "run_started", { nodes: ["a", "b", "c"], mode: "build", live: true }));
  for (const n of ["a", "b", "c"]) L.apply(run, ev(0, "node_queued", { node: n }));
  assert.deepStrictEqual(L.counts(run).queued, 3);
  L.apply(run, ev(1, "node_started", { node: "a", thread: "Thread-1 (worker)" }));
  assert.strictEqual(run.nodes.get("a").status, "running");
  L.apply(run, ev(3, "node_finished", { node: "a", stats: { status: "success", rows_affected: 10, duration_ms: 2000 } }));
  // A later correction keeps what it doesn't report.
  L.apply(run, ev(4, "node_finished", { node: "a", stats: { status: "error" } }));
  assert.strictEqual(run.nodes.get("a").status, "error");
  assert.strictEqual(run.nodes.get("a").stats.rows_affected, 10);
  assert.strictEqual(L.took(run.nodes.get("a")), 2000);
  L.apply(run, ev(4, "node_started", { node: "b" }));
  L.apply(run, ev(5, "node_finished", { node: "b", stats: { status: "success" } }));
  assert.strictEqual(L.took(run.nodes.get("b")), 1000, "end minus start without the engine's time");
  // Missing rows are never zero: the total is a lower bound, and only built nodes
  // count as "didn't report" (a failed one may have written some).
  assert.deepStrictEqual(L.rows(run), { sum: 10, reported: 1, missingBuilt: 1, missingOther: 0, finished: 2 });
  L.apply(run, ev(6, "run_finished", { outcome: "failed" }));
  // What hadn't finished never will: unknown, never success.
  assert.strictEqual(run.nodes.get("c").status, "unknown");
  assert.strictEqual(run.outcome, "failed");
  assert.deepStrictEqual(run.log.map(e => e.kind), ["run", "started", "success", "error", "started", "success", "run"]);
  assert.strictEqual(L.duration(850), "850ms");
  assert.strictEqual(L.duration(4249), "4.2s");
  assert.strictEqual(L.duration(125000), "2m 05s");
}

// ---- follow mode, on a graph laid out in a row: n0 → n1 → … → n11, plus a branch
// x0 → x1 under n0, and y under n11.
const ids = [];
const edges = [];
const pos = new Map();
for (let i = 0; i < 12; i++) {
  ids.push("n" + i);
  pos.set("n" + i, { x: i * 250, y: 0, w: 196, h: 72 });
  if (i) edges.push({ from: "n" + (i - 1), to: "n" + i });
}
for (const [id, x, y] of [["x0", 0, 200], ["x1", 250, 200], ["y", 2750, 200]]) { ids.push(id); pos.set(id, { x, y, w: 196, h: 72 }); }
edges.push({ from: "x0", to: "x1" });
const graph = L.adjacency(edges);
const vw = 856, vh = 788;
function ctx(status, extra) {
  return Object.assign({
    ids, graph, pos, vw, vh, statusOf: id => status[id] || "queued", inRun: () => true,
    view: { x: 0, y: 0, k: 1 }, focus: null, pool: null, done: false,
  }, extra);
}
{
  // Two neighbours fit: both are framed, never above the closest zoom.
  const w = L.follow(ctx({ n0: "success", n1: "running", n2: "running" }));
  assert.strictEqual(w.mode, "running");
  assert.ok(w.cam.k <= L.MAX_FOLLOW && w.cam.k >= L.MIN_READABLE, JSON.stringify(w.cam));
  // Far apart: one focus, the node blocking the most queued work (n1 blocks n2…n11 but
  // n10, which runs).
  const far = { n1: "running", n10: "running", x0: "running" };
  const f = L.follow(ctx(far));
  assert.strictEqual(f.mode, "focus");
  assert.strictEqual(f.focus, "n1");
  assert.strictEqual(f.blocks, 9);
  // It stays on its focus while that runs, even when a node blocking more starts.
  const g = L.follow(ctx(Object.assign({}, far, { n0: "running" }), { focus: "n1" }));
  assert.strictEqual(g.focus, "n1");
  // A focus that finished is replaced.
  const h = L.follow(ctx(Object.assign({}, far, { n1: "success" }), { focus: "n1" }));
  assert.strictEqual(h.focus, "x0", "n10 and x0 block one queued node each: the nearer to the view wins");
  // On a tie, the nearest to the view's centre: x0 when looking at the left.
  const left = { x: 400, y: 300, k: 1 };
  const right = { x: -2400, y: 300, k: 1 };
  const tie = { x0: "running", n10: "running", n1: "success", n0: "success" };
  assert.strictEqual(L.follow(ctx(tie, { view: left })).focus, "x0");
  assert.strictEqual(L.follow(ctx(tie, { view: right })).focus, "n10");
  // Never below the readable minimum, whatever it frames.
  for (const w2 of [f, g, h]) assert.ok(w2.cam.k >= L.MIN_READABLE);
}
{
  // A scope: running nodes outside it don't count.
  const pool = L.scopeSet(graph, { id: "x0", dir: "down" });
  assert.deepStrictEqual([...pool].sort(), ["x0", "x1"]);
  const w = L.follow(ctx({ n5: "running", x0: "success", x1: "queued" }, { pool }));
  assert.strictEqual(w.mode, "next", "x1 is ready: its upstream finished");
  assert.deepStrictEqual(w.ready, ["x1"]);
  const idle = L.follow(ctx({ n5: "running", x0: "running" }, { pool: L.scopeSet(graph, { id: "x1", dir: "self" }) }));
  assert.strictEqual(idle.mode, "idle", "nothing in scope runs or is ready: the camera stays");
  // When the scope finished, it is framed whole.
  const done = L.follow(ctx({ n5: "running", x0: "success", x1: "error" }, { pool }));
  assert.strictEqual(done.mode, "scope-done");
  // A scope too wide to read whole keeps its root in view.
  const wide = L.scopeSet(graph, { id: "n0", dir: "down" });
  const allDone = Object.fromEntries(ids.map(id => [id, "success"]));
  const w2 = L.follow(ctx(allDone, { pool: wide, root: "n0" }));
  assert.strictEqual(w2.mode, "scope-done");
  assert.ok(w2.cam.k >= L.MIN_READABLE);
  const sx = w2.cam.x + 0 * w2.cam.k, sx1 = w2.cam.x + 196 * w2.cam.k;
  assert.ok(sx >= 0 && sx1 <= vw, "the root n0 is in view: " + JSON.stringify(w2.cam));
  // The minimap's corner is kept clear: a framed box ends above it.
  const inset = L.follow(ctx({ n1: "running" }, { inset: { bottom: 190 } }));
  const bottom = inset.cam.y + 72 * inset.cam.k;
  assert.ok(bottom <= vh - 190, "above the minimap: " + bottom);
  assert.deepStrictEqual([...L.scopeSet(graph, { id: "n10", dir: "up" })].length, 11);
}
{
  // The end of a run: the whole graph if it reads at 60%, else what ran, else the
  // failure.
  const wide = ctx({ n0: "success", n11: "error" }, { inRun: id => id === "n0" || id === "n11" });
  const fin = L.finalView(wide);
  assert.ok(fin.k >= L.MIN_READABLE);
  const small = new Map([["a", { x: 0, y: 0, w: 196, h: 72 }], ["b", { x: 250, y: 0, w: 196, h: 72 }]]);
  const whole = L.finalView({ ids: ["a", "b"], pos: small, vw, vh, inRun: () => true, statusOf: () => "success" });
  assert.ok(whole.x >= 0 && whole.x + 446 * whole.k <= vw, "the whole small graph");
  const failure = L.finalView(ctx({ n11: "error" }, { inRun: id => id === "n11" || id === "n0" }));
  const fx = failure.x + 2750 * failure.k;
  assert.ok(fx >= 0 && fx + 196 * failure.k <= vw, "the failure is in view");
}
{
  // Off screen: on the edge each node is past.
  const view = { x: 0, y: 0, k: 1 };
  const off = L.offscreen(["n0", "n5", "y"], pos, view, vw, vh);
  assert.deepStrictEqual(off, [{ id: "n5", side: "right" }, { id: "y", side: "right" }]);
  assert.deepStrictEqual(L.offscreen(["n0"], pos, { x: 0, y: -900, k: 1 }, vw, vh), [{ id: "n0", side: "top" }]);
  assert.ok(!L.moved({ x: 0, y: 0, k: 1 }, { x: 2, y: 1, k: 1.001 }), "a pixel isn't a move");
  assert.ok(L.moved({ x: 0, y: 0, k: 1 }, { x: 40, y: 0, k: 1 }));
}

// ---- playback (ADR-0026): the timeline, seeking, keyframes and concurrency
{
  const evs = [
    ev(0, "run_started", { nodes: ["a", "b", "c"], mode: "build", live: true }),
    ev(0, "node_queued", { node: "a" }), ev(0, "node_queued", { node: "b" }), ev(0, "node_queued", { node: "c" }),
    ev(1, "node_started", { node: "a" }), ev(1, "node_started", { node: "b" }),
    ev(3, "node_finished", { node: "a", stats: { status: "success" } }),
    // A clock that went back: played at the time before it, never earlier.
    ev(2, "node_finished", { node: "b", stats: { status: "error" } }),
    ev(4, "node_finished", { node: "c", stats: { status: "skipped" } }),
    ev(5, "run_finished", { outcome: "failed" }),
  ].map((e, i) => ({ id: i + 1, ev: e }));
  const t0 = Date.parse(at(0));
  const tl = L.timeline("r", evs);
  assert.strictEqual(tl.t0, t0);
  assert.strictEqual(tl.t1 - tl.t0, 5000);
  assert.ok(tl.finished);
  assert.strictEqual(tl.items[7].at - t0, 3000, "never earlier than the event before it");
  // The state at a moment is the fold of what happened by then.
  const mid = L.stateAt(tl, t0 + 1500).run;
  assert.strictEqual(mid.nodes.get("a").status, "running");
  assert.strictEqual(mid.nodes.get("b").status, "running");
  assert.strictEqual(mid.nodes.get("c").status, "queued");
  assert.strictEqual(L.runningAt(tl, t0 + 1500), 2);
  assert.strictEqual(tl.peak, 2);
  // The end is the run's final state; before the start, nothing has happened but its
  // start.
  const end = L.stateAt(tl, tl.t1);
  assert.strictEqual(end.applied, evs.length);
  assert.strictEqual(end.run.outcome, "failed");
  assert.strictEqual(end.run.nodes.get("b").status, "error");
  assert.strictEqual(L.stateAt(tl, t0 - 1000).applied, 0);
  // Deterministic: the same moment gives the same picture, whatever came before.
  const a1 = JSON.stringify([...L.stateAt(tl, t0 + 3000).run.nodes]);
  L.stateAt(tl, tl.t1);
  assert.strictEqual(JSON.stringify([...L.stateAt(tl, t0 + 3000).run.nodes]), a1);
  // Seeking never changes the timeline's own copies.
  const again = L.stateAfter(tl, 0);
  L.apply(again, evs[4].ev);
  assert.strictEqual(tl.keyframes[0].run.nodes.size, 0);
  // Failures and the end are markers.
  assert.deepStrictEqual(tl.markers.map(m => [m.kind, m.node || m.outcome, m.at - t0]), [["error", "b", 3000], ["finished", "failed", 5000]]);
  // Speeds stay within the list; times read as a player's.
  assert.strictEqual(L.nextSpeed(1, 1), 2);
  assert.strictEqual(L.nextSpeed(64, 1), 64);
  assert.strictEqual(L.nextSpeed(0.25, -1), 0.25);
  assert.strictEqual(L.playTime(65000), "1:05");
  assert.strictEqual(L.playTime(3725000), "1:02:05");
}
{
  // A long journal: seeking folds from the nearest keyframe, and gives the same state
  // as folding everything.
  const n = 1000, evs = [{ id: 1, ev: ev(0, "run_started", { nodes: [], mode: "build", live: true }) }];
  for (let i = 0; i < n; i++) {
    evs.push({ id: evs.length + 1, ev: ev(i, "node_started", { node: "m" + i }) });
    evs.push({ id: evs.length + 1, ev: ev(i + 0.5, "node_finished", { node: "m" + i, stats: { status: "success", rows_affected: i } }) });
  }
  const tl = L.timeline("r", evs);
  assert.ok(tl.keyframes.length > 1 && tl.keyframes.every((k, i) => k.index === i * L.KEYFRAME_EVERY));
  const t = Date.parse(at(0)) + 700250;
  const fast = L.stateAt(tl, t).run;
  const slow = L.newRun("r");
  for (const e of evs) if (Date.parse(e.ev.at) <= t) L.apply(slow, e.ev);
  assert.strictEqual(JSON.stringify([...fast.nodes]), JSON.stringify([...slow.nodes]));
  assert.strictEqual(L.counts(fast).running, 1);
  assert.strictEqual(L.runningAt(tl, t), 1);
}
{
  // A journal rebuilt from a final report: no starts, so nothing ran in the band, and
  // no finish of the run is invented for one that has none.
  const evs = [
    ev(0, "run_started", { nodes: ["a"], mode: "build", live: false }),
    ev(9, "node_finished", { node: "a", stats: { status: "success" } }),
  ].map((e, i) => ({ id: i + 1, ev: e }));
  const tl = L.timeline("r", evs);
  assert.strictEqual(tl.live, false);
  assert.strictEqual(tl.peak, 0);
  assert.ok(!tl.finished);
  assert.strictEqual(tl.t1 - tl.t0, 9000);
}
process.stdout.write("ok");
"#;

#[test]
fn follow_mode_and_the_event_fold_decide_as_designed() {
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("skipped: node is not installed");
        return;
    }
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/live.js");
    let out = Command::new("node")
        .args(["-e", CHECKS, script])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok");
}
