"use strict";
// The live run view on the Lineage page (#322, docs/design/dashboard/boards/live-run).
// Two parts:
// - OdsLive: what the view decides, as plain functions with no page around them, so
//   they are tested on their own (tests/live_follow.rs): folding run events, follow
//   mode's camera and focus node, follow scopes, and which running nodes are off screen.
// - a plugin of the explorer (lineage.js): it streams the run's events from
//   `api/runs/<id>/events` (Server-Sent Events, or polling `?since=` without them), and
//   draws each node's state, the run's side panel, follow mode, the node menu, edge
//   chips and the minimap.
// - playback (ADR-0026): the same view for a run's whole journal, replayed in the page
//   at any speed, paused, stepped event by event or scrubbed to any moment
//   (`?replay=<run_id>&t=<seconds>`).
// It only reads: nothing here starts, stops or changes a run.

const OdsLive = (function () {
  // Follow never zooms out below this: past it, names can't be read.
  const MIN_READABLE = 0.6;
  // Nor in closer than this, framing a few nodes.
  const MAX_FOLLOW = 1.15;
  const PAD = 80;
  // The camera moves at most this often, so parallel threads don't make it jitter.
  const MOVE_EVERY = 1000;

  // ---------- the run, folded from its events (as RunSummary::from_events does)
  function newRun(id) {
    return {
      id, scope: null, mode: null, live: null, startedAt: null, finishedAt: null, outcome: null,
      requested: [], nodes: new Map(), log: [], unreadable: 0, lastId: 0, ended: null,
    };
  }
  const FINISHED = new Set(["success", "error", "skipped", "unknown"]);
  const ms = at => { const t = Date.parse(at); return isFinite(t) ? t : null; };
  function node(run, id, requested) {
    let n = run.nodes.get(id);
    if (!n) {
      n = { id, requested: !!requested, status: "queued", startedAt: null, finishedAt: null, thread: null, stats: null, tests: null };
      run.nodes.set(id, n);
    }
    return n;
  }
  // Applies one event; returns the ids of the nodes it changed and what the log says.
  function apply(run, ev) {
    const changed = [];
    let said = null;
    if (!run.scope && ev.scope) run.scope = ev.scope;
    const at = ms(ev.at);
    switch (ev.kind) {
      case "run_started":
        run.mode = ev.mode; run.live = ev.live; run.startedAt = at;
        run.requested = ev.nodes.slice();
        for (const id of ev.nodes) { node(run, id, true).requested = true; changed.push(id); }
        said = { at, kind: "run", text: `Run started: ${ev.nodes.length} node${ev.nodes.length === 1 ? "" : "s"}` };
        break;
      case "node_queued": {
        const n = node(run, ev.node);
        if (!FINISHED.has(n.status)) n.status = "queued";
        changed.push(ev.node);
        break;
      }
      case "node_started": {
        const n = node(run, ev.node);
        n.status = "running"; n.startedAt = at; n.thread = ev.thread || null;
        changed.push(ev.node);
        said = { at, kind: "started", node: ev.node };
        break;
      }
      case "node_finished": {
        const n = node(run, ev.node);
        const s = ev.stats || {};
        // A second finish corrects the status; what it doesn't report is kept.
        const before = n.stats || {};
        const keep = k => s[k] != null ? s[k] : before[k];
        n.stats = Object.assign({}, before, s, {
          started_at: keep("started_at"), finished_at: keep("finished_at"), duration_ms: keep("duration_ms"),
          compile_ms: keep("compile_ms"), execute_ms: keep("execute_ms"), rows_affected: keep("rows_affected"),
          thread: keep("thread"),
        });
        n.status = s.status || "unknown";
        if (n.startedAt == null && s.started_at) n.startedAt = ms(s.started_at);
        n.finishedAt = ms(s.finished_at) || at;
        if (!n.thread && s.thread) n.thread = s.thread;
        changed.push(ev.node);
        said = { at, kind: n.status, node: ev.node, rows: n.stats.rows_affected };
        break;
      }
      case "check_finished":
        for (const id of ev.covers || []) {
          const n = run.nodes.get(id);
          if (!n) continue;
          n.tests = n.tests || { passed: 0, failed: 0, warned: 0, skipped: 0, unknown: 0 };
          const k = { passed: "passed", failed: "failed", warned: "warned", skipped: "skipped" }[ev.status] || "unknown";
          n.tests[k] += 1;
          changed.push(id);
        }
        // `check` is the check's handle (#323), never its id: name it by what it covers.
        if (ev.status === "failed") {
          const on = (ev.covers || []).map(c => c.split(".").pop()).join(", ");
          said = { at, kind: "check", text: `A test${on ? ` on ${on}` : ""} failed (${ev.check})` };
        }
        break;
      case "run_finished":
        run.outcome = ev.outcome; run.finishedAt = at;
        // Whatever hadn't finished by now never will: unknown, never success.
        for (const n of run.nodes.values()) if (!FINISHED.has(n.status)) { n.status = "unknown"; changed.push(n.id); }
        said = { at, kind: "run", text: `Run finished: ${ev.outcome}` };
        break;
      default:
        break;
    }
    if (said) run.log.push(said);
    return changed;
  }
  function counts(run) {
    const c = { queued: 0, running: 0, success: 0, error: 0, skipped: 0, unknown: 0 };
    for (const n of run.nodes.values()) c[n.status] = (c[n.status] || 0) + 1;
    return c;
  }
  // How long a node took: the engine's time, else start to finish; null when not known.
  function took(n) {
    const s = n.stats || {};
    if (s.duration_ms != null) return s.duration_ms;
    if (n.startedAt != null && n.finishedAt != null) return Math.max(0, n.finishedAt - n.startedAt);
    return null;
  }
  // Rows so far: the sum of what was reported; built nodes that didn't report theirs;
  // and failed or unknown nodes that didn't, which may have written some too.
  function rows(run) {
    let sum = 0, reported = 0, missingBuilt = 0, missingOther = 0, finished = 0;
    for (const n of run.nodes.values()) {
      if (!["success", "error", "unknown"].includes(n.status)) continue;
      finished += 1;
      const r = n.stats && n.stats.rows_affected;
      if (r != null) { sum += r; reported += 1; }
      else if (n.status === "success") missingBuilt += 1;
      else missingOther += 1;
    }
    return { sum, reported, missingBuilt, missingOther, finished };
  }
  // `850ms`, `4.2s`, `2m 05s`, as the Run pages say it.
  function duration(msv) {
    if (msv == null) return null;
    if (msv < 1000) return `${Math.round(msv)}ms`;
    if (msv < 60000) return `${(Math.round(msv / 100) / 10).toFixed(1)}s`;
    const s = Math.round(msv / 1000);
    return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
  }

  // ---------- playback (ADR-0026): a run's journal, replayed from any moment
  const SPEEDS = [0.25, 0.5, 1, 2, 4, 8, 16, 64];
  // A copy of the folded state every this many events, so seeking folds at most this
  // many from the nearest one.
  const KEYFRAME_EVERY = 256;
  function cloneRun(run) {
    const c = Object.assign({}, run, { requested: run.requested.slice(), log: run.log.slice(), nodes: new Map() });
    for (const [id, n] of run.nodes) {
      c.nodes.set(id, Object.assign({}, n, { stats: n.stats && Object.assign({}, n.stats), tests: n.tests && Object.assign({}, n.tests) }));
    }
    return c;
  }
  // The timeline of a run's events (`[{ id, ev }]`, in journal order, this run's only):
  // when it starts and ends, each event's time, keyframes, failures, and how many nodes
  // were running at each moment. Times never go backwards: an event dated before the
  // one before it is played at that one's time; one without a time, too. Such an event
  // is marked `adjusted` and counted, so the page can say its time is inferred.
  function timeline(runId, events) {
    const items = [];
    let last = null;
    for (const e of events) {
      const recorded = ms(e.ev.at);
      const adjusted = recorded == null || (last != null && recorded < last);
      const at = adjusted ? last : recorded;
      items.push({ id: e.id, ev: e.ev, at, adjusted });
      if (at != null) last = at;
    }
    const firstAt = items.find(i => i.at != null);
    const started = items.find(i => i.ev.kind === "run_started" && i.at != null);
    const t0 = started ? started.at : firstAt ? firstAt.at : 0;
    for (const i of items) if (i.at == null || i.at < t0) { i.at = t0; i.adjusted = true; }
    const finishedItem = items.find(i => i.ev.kind === "run_finished");
    const t1 = items.length ? Math.max(t0, items[items.length - 1].at) : t0;
    const run = newRun(runId);
    const keyframes = [{ index: 0, run: cloneRun(run) }];
    const markers = [];
    // The last event that says something about each node: until the playhead passes
    // it, the node's story isn't over.
    const lastIndex = new Map();
    const touch = (id, i) => { if (id) lastIndex.set(id, i + 1); };
    items.forEach((item, i) => {
      apply(run, item.ev);
      touch(item.ev.node, i);
      for (const id of item.ev.kind === "check_finished" ? item.ev.covers || [] : []) touch(id, i);
      if (item.ev.kind === "node_finished" && item.ev.stats && item.ev.stats.status === "error") {
        markers.push({ at: item.at, kind: "error", node: item.ev.node, index: i + 1, inferred: item.adjusted });
      }
      if ((i + 1) % KEYFRAME_EVERY === 0) keyframes.push({ index: i + 1, run: cloneRun(run) });
    });
    if (finishedItem) markers.push({ at: finishedItem.at, kind: "finished", outcome: finishedItem.ev.outcome, index: items.indexOf(finishedItem) + 1, inferred: finishedItem.adjusted });
    const adjusted = items.filter(i => i.adjusted).length;
    // Running at each moment: from each node's start to its finish (or the end).
    const edges = [];
    for (const n of run.nodes.values()) {
      if (n.startedAt == null) continue;
      // A finish dated before its start (a clock that went back) ends where it began.
      const from = Math.max(t0, n.startedAt);
      edges.push([from, 1], [Math.max(from, n.finishedAt != null ? n.finishedAt : t1), -1]);
    }
    edges.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    const steps = [[t0, 0]];
    let now = 0, peak = 0;
    for (const [t, d] of edges) {
      now += d;
      peak = Math.max(peak, now);
      if (steps[steps.length - 1][0] === t) steps[steps.length - 1][1] = now; else steps.push([t, now]);
    }
    return { runId, items, t0, t1, keyframes, markers, steps, peak, finished: !!finishedItem, live: run.live, final: run, lastIndex, adjusted };
  }
  // How many events happened at or before `t`.
  function indexAt(tl, t) {
    let lo = 0, hi = tl.items.length;
    while (lo < hi) { const mid = (lo + hi) >> 1; if (tl.items[mid].at <= t) lo = mid + 1; else hi = mid; }
    return lo;
  }
  // The run as it was after its first `n` events: from the nearest keyframe, folded on.
  function stateAfter(tl, n) {
    let kf = tl.keyframes[0];
    for (const k of tl.keyframes) { if (k.index <= n) kf = k; else break; }
    const run = cloneRun(kf.run);
    for (let i = kf.index; i < n; i++) apply(run, tl.items[i].ev);
    return run;
  }
  // The run as it was at `t`: the same picture for the same `t`, always.
  function stateAt(tl, t) { const n = indexAt(tl, t); return { run: stateAfter(tl, n), applied: n }; }
  // How many nodes were running at `t`.
  function runningAt(tl, t) {
    let v = 0;
    for (const [at, n] of tl.steps) { if (at <= t) v = n; else break; }
    return v;
  }
  // The next speed up (`dir` 1) or down (-1), staying within the list.
  function nextSpeed(speed, dir) {
    const i = SPEEDS.indexOf(speed);
    const at = i < 0 ? SPEEDS.indexOf(1) : i;
    return SPEEDS[Math.max(0, Math.min(SPEEDS.length - 1, at + dir))];
  }
  // `1:05`, or `1:02:05` past an hour: a player's time.
  function playTime(msv) {
    const s = Math.max(0, Math.floor(msv / 1000));
    const hh = Math.floor(s / 3600), mm = Math.floor((s % 3600) / 60), ss = String(s % 60).padStart(2, "0");
    return hh ? `${hh}:${String(mm).padStart(2, "0")}:${ss}` : `${mm}:${ss}`;
  }

  // ---------- the graph: who is downstream or upstream of whom
  function adjacency(edges) {
    const down = new Map(), up = new Map();
    const push = (m, k, v) => { if (!m.has(k)) m.set(k, []); m.get(k).push(v); };
    for (const e of edges) { push(down, e.from, e.to); push(up, e.to, e.from); }
    return { down, up };
  }
  // `id` and everything reachable from it along `adj`.
  function reach(adj, id) {
    const out = new Set([id]), stack = [id];
    while (stack.length) for (const x of adj.get(stack.pop()) || []) if (!out.has(x)) { out.add(x); stack.push(x); }
    return out;
  }
  // The nodes a follow scope covers: `down` (the node and its downstream), `up`, or
  // `self`; null for the whole run.
  function scopeSet(graph, scope) {
    if (!scope) return null;
    if (scope.dir === "self") return new Set([scope.id]);
    return reach(scope.dir === "up" ? graph.up : graph.down, scope.id);
  }
  // Queued nodes downstream of `id`: the work it blocks.
  function blocking(graph, statusOf, id) {
    let n = 0;
    for (const x of reach(graph.down, id)) if (x !== id && statusOf(x) === "queued") n += 1;
    return n;
  }

  // ---------- the camera: screen = view.x + graph.x * view.k
  function box(ids, pos) {
    let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
    for (const id of ids) {
      const p = pos.get(id); if (!p) continue;
      x0 = Math.min(x0, p.x); y0 = Math.min(y0, p.y); x1 = Math.max(x1, p.x + p.w); y1 = Math.max(y1, p.y + p.h);
    }
    return x0 === Infinity ? null : { x0, y0, x1, y1 };
  }
  // The part of the view to frame in: all of it but `inset` (e.g. the minimap's
  // corner), as { cx, cy, w, h }.
  function region(vw, vh, inset) {
    const i = Object.assign({ top: 0, right: 0, bottom: 0, left: 0 }, inset);
    const w = Math.max(1, vw - i.left - i.right), h = Math.max(1, vh - i.top - i.bottom);
    return { cx: i.left + w / 2, cy: i.top + h / 2, w, h };
  }
  // The camera that shows `b` in a `vw`×`vh` view (less `inset`), no closer than `max`.
  function fit(b, vw, vh, max, inset) {
    const r = region(vw, vh, inset);
    const pad = Math.min(PAD, r.w / 6, r.h / 6);
    const bw = Math.max(1, b.x1 - b.x0), bh = Math.max(1, b.y1 - b.y0);
    const k = Math.min(max, (r.w - 2 * pad) / bw, (r.h - 2 * pad) / bh);
    return { k, x: r.cx - (b.x0 + bw / 2) * k, y: r.cy - (b.y0 + bh / 2) * k };
  }
  // The same, but never below the readable minimum: centred on `b` (or on `on`, a box
  // that must stay in view) if it doesn't fit.
  function fitReadable(b, vw, vh, inset, on) {
    const c = fit(b, vw, vh, MAX_FOLLOW, inset);
    if (c.k >= MIN_READABLE) return c;
    const k = MIN_READABLE, r = region(vw, vh, inset), at = on || b;
    return { k, x: r.cx - ((at.x0 + at.x1) / 2) * k, y: r.cy - ((at.y0 + at.y1) / 2) * k };
  }
  // The running node follow should keep in view when they don't all fit: the one that
  // blocks the most queued nodes downstream; on a tie the nearest to the view's centre,
  // so the camera travels least; then by id, so the choice is the same every time.
  function chooseFocus(ids, ctx) {
    const cx = (ctx.vw / 2 - ctx.view.x) / ctx.view.k, cy = (ctx.vh / 2 - ctx.view.y) / ctx.view.k;
    const dist = id => { const p = ctx.pos.get(id); return p ? Math.hypot(p.x + p.w / 2 - cx, p.y + p.h / 2 - cy) : Infinity; };
    const scored = ids.map(id => ({ id, blocks: blocking(ctx.graph, ctx.statusOf, id), d: dist(id) }));
    scored.sort((a, b) => b.blocks - a.blocks || a.d - b.d || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    return scored[0];
  }
  // Where follow mode wants the camera now, and why:
  // - `running`: every running node fits at a readable size;
  // - `focus`: they don't, so one focus node is kept until it finishes (`ctx.focus`);
  // - `scope-done`: every node of the scope finished: the whole scope is framed;
  // - `next`: nothing runs yet: the queued nodes whose upstream all finished;
  // - `done` / `idle`: nothing to move to.
  function follow(ctx) {
    const pool = ctx.pool;
    const inPool = id => !pool || pool.has(id);
    const running = ctx.ids.filter(id => inPool(id) && ctx.statusOf(id) === "running").sort();
    if (running.length) {
      const b = box(running, ctx.pos);
      if (b) {
        const c = fit(b, ctx.vw, ctx.vh, MAX_FOLLOW, ctx.inset);
        if (c.k >= MIN_READABLE) return { mode: "running", cam: c, focus: null, running };
      }
      const kept = ctx.focus && running.includes(ctx.focus) ? { id: ctx.focus } : chooseFocus(running, ctx);
      const fb = box([kept.id], ctx.pos);
      return {
        mode: "focus", focus: kept.id, running,
        blocks: blocking(ctx.graph, ctx.statusOf, kept.id),
        cam: fb ? fit(fb, ctx.vw, ctx.vh, MAX_FOLLOW, ctx.inset) : null,
      };
    }
    if (pool) {
      const inRun = [...pool].filter(id => ctx.inRun(id));
      if (inRun.length && inRun.every(id => !["queued", "running"].includes(ctx.statusOf(id)))) {
        // The whole scope; if it can't be read whole, its root stays in view.
        const b = box([...pool], ctx.pos), root = ctx.root ? box([ctx.root], ctx.pos) : null;
        return { mode: "scope-done", cam: b ? fitReadable(b, ctx.vw, ctx.vh, ctx.inset, root) : null, focus: null, running };
      }
    }
    if (ctx.done) return { mode: "done", cam: null, focus: null, running };
    const ready = ctx.ids.filter(id => inPool(id) && ctx.statusOf(id) === "queued"
      && (ctx.graph.up.get(id) || []).every(u => !ctx.inRun(u) || FINISHED.has(ctx.statusOf(u)))).sort();
    const b = ready.length ? box(ready, ctx.pos) : null;
    if (b) return { mode: "next", cam: fitReadable(b, ctx.vw, ctx.vh, ctx.inset), focus: null, running, ready };
    return { mode: "idle", cam: null, focus: null, running };
  }
  // The view at the end of a run: the whole graph if it reads at the minimum, else every
  // node that ran, else the failed and skipped ones; null when none of that applies.
  function finalView(ctx) {
    const all = box(ctx.pos.keys(), ctx.pos);
    if (all) { const c = fit(all, ctx.vw, ctx.vh, 1, ctx.inset); if (c.k >= MIN_READABLE) return c; }
    const ran = box(ctx.ids.filter(ctx.inRun), ctx.pos);
    if (ran) { const c = fit(ran, ctx.vw, ctx.vh, 1, ctx.inset); if (c.k >= MIN_READABLE) return c; }
    const bad = box(ctx.ids.filter(id => ["error", "skipped"].includes(ctx.statusOf(id))), ctx.pos);
    return bad ? fitReadable(bad, ctx.vw, ctx.vh, ctx.inset) : null;
  }
  // Whether the camera moved enough to be worth moving.
  function moved(a, b) {
    return !a || !b || Math.abs(a.k - b.k) > 0.01 * a.k || Math.abs(a.x - b.x) > 4 || Math.abs(a.y - b.y) > 4;
  }
  // Running nodes outside the view, and which edge each is past.
  function offscreen(ids, pos, view, vw, vh) {
    const out = [];
    for (const id of ids) {
      const p = pos.get(id); if (!p) continue;
      const x0 = view.x + p.x * view.k, y0 = view.y + p.y * view.k;
      const x1 = x0 + p.w * view.k, y1 = y0 + p.h * view.k;
      if (x1 > 0 && x0 < vw && y1 > 0 && y0 < vh) continue;
      // The edge it is furthest past.
      const past = { left: -x1, right: x0 - vw, top: -y1, bottom: y0 - vh };
      const side = Object.keys(past).reduce((a, b) => past[b] > past[a] ? b : a);
      out.push({ id, side });
    }
    return out;
  }

  return {
    MIN_READABLE, MAX_FOLLOW, MOVE_EVERY, FINISHED,
    newRun, apply, counts, took, rows, duration, adjacency, reach, scopeSet, blocking,
    SPEEDS, KEYFRAME_EVERY, cloneRun, timeline, indexAt, stateAfter, stateAt, runningAt, nextSpeed, playTime,
    box, fit, fitReadable, chooseFocus, follow, finalView, moved, offscreen,
  };
})();

if (typeof window !== "undefined") (window.OdsExplorerPlugins = window.OdsExplorerPlugins || []).push(function liveRunView(x) {
  if (!x.served) return;
  const L = OdsLive;
  const $ = id => document.getElementById(id);
  const h = x.h;
  const SVG = "http://www.w3.org/2000/svg";
  const params = new URLSearchParams(location.search);
  const reduced = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)");
  const still = () => !!(reduced && reduced.matches);
  const graph = L.adjacency(x.doc.node_edges);
  const overlayData = (() => { try { return JSON.parse(($("ods-overlay") || {}).textContent || "null"); } catch (_) { return null; } })();
  const buildable = x.doc.nodes.filter(n => n.kind !== "source").map(n => n.id);
  const resolve = wanted => x.byId.has(wanted) ? wanted : ((x.doc.nodes.filter(n => n.name === wanted).length === 1 ? x.doc.nodes.find(n => n.name === wanted).id : null));

  // ---------- state
  const S = {
    on: false, runId: null, run: null, source: null, done: false,
    follow: true, hint: false, focus: null, scope: null, lastCam: null, lastMove: 0, tween: null,
    dirty: new Set(), frame: 0, liveIds: [], menu: null, card: { node: null, status: null, html: null },
    toast: null, scopeDoneShown: false, say: [], saidAt: 0, pollTimer: 0,
    // Playback (ADR-0026): null live; else the timeline, the playhead and the transport.
    replay: null,
  };
  // The view's clock: the playhead in playback, the wall clock live.
  const now = () => (S.replay && S.replay.tl ? S.replay.p : Date.now());
  const statusOf = id => {
    const n = S.run && S.run.nodes.get(id);
    if (n) return n.status;
    const node = x.byId.get(id);
    return node && node.kind === "source" ? "source" : "kept";
  };
  const inRun = id => !!(S.run && S.run.nodes.has(id));
  // The scope's nodes, worked out once per scope.
  let poolFor = null, poolSet = null;
  const pool = () => {
    if (poolFor !== S.scope) { poolFor = S.scope; poolSet = L.scopeSet(graph, S.scope); }
    return poolSet;
  };

  // ---------- the page's parts, made once
  const bar = document.querySelector(".lin-bar");
  const select = $("lin-overlay");
  const liveOption = document.createElement("option");
  liveOption.value = "live";
  liveOption.textContent = "Live run";
  select.insertBefore(liveOption, select.firstChild);
  // In the picker only while a run is replayed (ADR-0026).
  const replayOption = document.createElement("option");
  replayOption.value = "replay";
  replayOption.textContent = "Replay";
  const showReplay = on => { if (on) liveOption.insertAdjacentElement("afterend", replayOption); else replayOption.remove(); };
  const offer = h("button", "Watch the run live", "lv-offer lin-ui", null);
  offer.type = "button";
  offer.hidden = true;
  select.insertAdjacentElement("afterend", offer);
  const liveBar = h("span", null, "lv-bar", null);
  offer.insertAdjacentElement("afterend", liveBar);
  const followBtn = h("button", null, "lv-follow", liveBar);
  followBtn.type = "button";
  followBtn.id = "lv-follow";
  followBtn.innerHTML = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="7"></circle><circle cx="12" cy="12" r="2"></circle><path d="M12 1v4M12 19v4M1 12h4M19 12h4"></path></svg><span></span>';
  followBtn.title = "Keep the running nodes in view (F)";
  const scopeChip = h("span", null, "lv-scope", liveBar);
  scopeChip.id = "lv-scope";
  const scopeText = h("span", null, null, scopeChip);
  const scopeClear = h("button", "×", null, scopeChip);
  scopeClear.type = "button";
  scopeClear.setAttribute("aria-label", "Stop following this scope; follow the whole run");
  const focusLine = h("span", null, "lv-focus", liveBar);
  focusLine.id = "lv-focus";
  const zoomOut = h("button", "−", "lv-zoom", null), zoomIn = h("button", "+", "lv-zoom", null);
  zoomOut.type = zoomIn.type = "button";
  zoomOut.setAttribute("aria-label", "Zoom out");
  zoomIn.setAttribute("aria-label", "Zoom in");
  $("lin-fit").insertAdjacentElement("beforebegin", zoomOut);
  $("lin-fit").insertAdjacentElement("beforebegin", zoomIn);

  const canvas = x.canvas;
  const hint = h("div", null, "lv-hint lin-ui", canvas);
  hint.id = "lv-hint";
  const hintText = h("span", "Follow is off: you moved the view.", null, hint);
  const hintBtn = h("button", "Follow run", null, hint);
  hintBtn.type = "button";
  const toast = h("div", null, "lv-toast lin-ui", canvas);
  toast.id = "lv-toast";
  toast.setAttribute("role", "status");
  const chips = {};
  for (const side of ["right", "left", "top", "bottom"]) {
    chips[side] = h("div", null, `lv-chips ${side} lin-ui`, canvas);
    chips[side].id = "lv-chips-" + side;
  }
  const mini = h("div", null, "lv-mini lin-ui", canvas);
  mini.id = "lv-mini";
  mini.setAttribute("role", "group");
  mini.setAttribute("aria-label", "Minimap of the whole run; click to move the view there (turns follow off)");
  h("span", "WHOLE RUN", "lv-mini-label", mini);
  const miniCanvas = h("canvas", null, null, mini);
  miniCanvas.width = 300 * (window.devicePixelRatio || 1);
  miniCanvas.height = 170 * (window.devicePixelRatio || 1);
  const menu = h("div", null, "lv-menu lin-ui", x.canvas.parentElement);
  menu.id = "lv-menu";
  menu.setAttribute("role", "menu");
  menu.hidden = true;
  const say = h("div", null, "sr-only lv-say", document.body);
  say.id = "lv-say";
  say.setAttribute("role", "status");
  say.setAttribute("aria-live", "polite");
  const status = document.querySelector(".lin-status");
  const statusBefore = status ? status.innerHTML : "";

  // ---------- turning the view on and off
  function params_(q) {
    if (!S.on) return;
    const R = S.replay;
    if (R) {
      q.set("replay", S.runId);
      if (R.tl && R.p > R.tl.t0) q.set("t", ((R.p - R.tl.t0) / 1000).toFixed(1));
      if (R.speed !== 1) q.set("speed", String(R.speed));
    } else q.set("live", S.runId);
    if (S.scope) {
      const n = x.byId.get(S.scope.id);
      const named = n && x.doc.nodes.filter(m => m.name === n.name).length === 1 ? n.name : S.scope.id;
      q.set("follow", `${named}:${S.scope.dir}`);
    }
  }
  function start(runId, opts) {
    opts = opts || {};
    stop(true);
    S.on = true; S.runId = runId; S.run = L.newRun(runId); S.done = false; S.jumped = false;
    S.waitFrom = 0; S.backoff = 0; S.waiting = false;
    document.body.classList.remove("lv-done");
    S.follow = true; S.hint = false; S.focus = null; S.lastCam = null; S.lastMove = 0; S.scopeDoneShown = false;
    S.scope = opts.scope || null;
    S.card = { node: null, status: null, html: null };
    S.replay = opts.replay ? { tl: null, p: 0, applied: 0, playing: false, speed: opts.speed || 1, at: opts.at || 0, autoplay: !!opts.autoplay, raf: 0 } : null;
    document.body.classList.toggle("lv-replay", !!S.replay);
    showReplay(!!S.replay);
    select.value = S.replay ? "replay" : "live";
    x.state.overlay = "live";
    x.state.columns = false;
    const cols = $("lin-columns"); if (cols) cols.checked = false;
    document.body.classList.add("lv-on");
    x.setBox(196, 72, 54, 48);
    x.render();
    x.fitAll();
    x.panel();
    x.remember();
    if (S.replay) load(); else connect();
    draw();
  }
  function stop(quiet) {
    if (S.source) { S.source.close(); S.source = null; }
    clearTimeout(S.pollTimer);
    if (S.replay) { cancelAnimationFrame(S.replay.raf); S.replay = null; }
    showReplay(false);
    if (!S.on) return;
    S.on = false;
    document.body.classList.remove("lv-on", "lv-done", "lv-replay", "lv-playing");
    closeMenu();
    if (status) status.innerHTML = statusBefore;
    x.setBox(140, 52, 28, 34);
    if (quiet) return;
    x.render(); x.fitAll(); x.panel(); x.remember();
    // A reload held while the run was shown happens now.
    if (window.odsReloadPending) location.reload();
  }
  window.odsHoldReload = () => S.on;

  // ---------- the stream
  function connect() {
    const url = x.baseUrl + "api/runs/" + encodeURIComponent(S.runId) + "/events";
    if (typeof EventSource === "undefined" || params.get("poll") === "1") return poll(url, 0);
    const es = new EventSource(url);
    S.source = es;
    es.addEventListener("run_event", m => receive(Number(m.lastEventId), JSON.parse(m.data)));
    es.addEventListener("unreadable", m => { S.run.unreadable += 1; S.run.lastId = Number(m.lastEventId) || S.run.lastId; schedule(); });
    es.addEventListener("end", m => { es.close(); S.source = null; ended(JSON.parse(m.data)); });
    es.onerror = () => {
      // Refused (too many streams) or gone: poll instead; a dropped connection
      // reconnects by itself and resumes from the last id.
      if (es.readyState === EventSource.CLOSED && S.on && !S.done) { S.source = null; poll(url, S.run.lastId); }
      else schedule();
    };
  }
  async function poll(url, since) {
    if (!S.on || S.done) return;
    try {
      const r = await fetch(url + "?since=" + since);
      if (r.status === 404 && Date.now() - (S.waitFrom || (S.waitFrom = Date.now())) < 60000) {
        // Opened from a link before the run wrote its journal: wait for it, a while.
        S.waiting = true;
        S.backoff = Math.min(8000, (S.backoff || 500) * 2);
        schedule();
        S.pollTimer = setTimeout(() => poll(url, S.run.lastId), S.backoff);
        return;
      }
      if (r.status === 400 || r.status === 404) {
        S.waiting = false;
        ended({ reason: "missing", outcome: null, inferred: false, note: `No journal for run ${S.runId}: it may have been pruned (the newest 50 are kept), or the id is wrong.` });
        return;
      }
      if (r.ok && S.waiting && params.get("poll") !== "1" && typeof EventSource !== "undefined") {
        // It's there now: stream it.
        S.waiting = false;
        connect();
        return;
      }
      S.waiting = false;
      if (r.ok) {
        for (const line of (await r.text()).split("\n")) {
          if (!line.trim()) continue;
          const m = JSON.parse(line);
          if (m.event === "run_event") receive(m.id, m.data);
          else if (m.event === "unreadable") { S.run.unreadable += 1; S.run.lastId = m.id; }
          else if (m.event === "end") { ended(m.data); return; }
        }
      }
    } catch (_) { /* try again */ }
    S.pollTimer = setTimeout(() => poll(url, S.run.lastId), 1000);
  }
  function receive(id, ev) {
    if (id && id <= S.run.lastId) return;
    if (id) S.run.lastId = id;
    // Another run's event in this journal: counted, never drawn.
    if (ev.run_id && ev.run_id !== S.runId) { S.run.unreadable += 1; schedule(); return; }
    for (const changed of L.apply(S.run, ev)) S.dirty.add(changed);
    const said = S.run.log[S.run.log.length - 1];
    if (said && said !== S.lastSaid) { S.lastSaid = said; announce(said); }
    schedule();
  }
  function ended(end) {
    const following = S.follow;
    S.done = true;
    document.body.classList.add("lv-done");
    S.run.ended = end;
    // Follow stops on the final view, without the "you moved it" hint.
    S.follow = false; S.hint = false; S.focus = null;
    for (const id of S.run.nodes.keys()) S.dirty.add(id);
    if (end.reason !== "finished") announce({ kind: "run", text: end.note });
    // The final view shows what ran, the failure included, unless the person was
    // looking elsewhere.
    if (following && end.reason !== "missing") {
      requestAnimationFrame(() => {
        const { vw, vh } = viewport();
        const cam = L.finalView({ ids: buildable, inRun, statusOf, pos: positions(), vw, vh });
        if (cam) moveTo(cam);
      });
    }
    schedule();
  }
  function schedule() {
    if (S.frame) return;
    S.frame = requestAnimationFrame(() => { S.frame = 0; draw(); });
  }

  // ---------- playback (ADR-0026): the journal, loaded once and replayed in the page
  async function load() {
    const R = S.replay;
    const url = x.baseUrl + "api/runs/" + encodeURIComponent(S.runId) + "/events";
    const events = [];
    let since = 0, unreadable = 0, missing = false, failed = null;
    R.error = null; R.retry = false; R.offerLive = false;
    schedule();
    try {
      for (;;) {
        const r = await fetch(url + "?since=" + since);
        if (S.replay !== R) return;
        if (r.status === 400 || r.status === 404) { missing = true; break; }
        // E.g. 503 when too many streams are open: worth trying again.
        if (!r.ok) { failed = `the server answered ${r.status}`; break; }
        let got = 0, end = false;
        for (const line of (await r.text()).split("\n")) {
          if (!line.trim()) continue;
          const m = JSON.parse(line);
          if (m.id) since = Math.max(since, m.id);
          if (m.event === "run_event") {
            got += 1;
            // Another run's event in this journal: counted, never drawn.
            if (m.data.run_id && m.data.run_id !== S.runId) unreadable += 1; else events.push({ id: m.id, ev: m.data });
          } else if (m.event === "unreadable") { got += 1; unreadable += 1; }
          else if (m.event === "end") end = true;
        }
        // A finished journal ends; one still being written has nothing more for now.
        if (end || !got) break;
      }
    } catch (_) { failed = failed || "the connection failed or an answer couldn't be read"; }
    if (S.replay !== R) return;
    // Only a journal that isn't there is said to be missing; one that couldn't be
    // loaded, or read, says so, and what to do.
    if (missing) {
      R.error = `No journal for run ${S.runId}: it may have been pruned (the newest 50 are kept), or the id is wrong.`;
      ended({ reason: "missing", outcome: null, inferred: false, note: R.error });
      return;
    }
    if (failed) {
      R.error = `The run's journal couldn't be loaded: ${failed}.`;
      R.retry = true;
      schedule();
      return;
    }
    if (!events.length) {
      R.error = unreadable
        ? `The run's journal is there, but none of its ${unreadable} line${unreadable === 1 ? "" : "s"} could be read (a newer version, or cut short).`
        : "The run's journal has no events yet: the run may be starting.";
      R.offerLive = !unreadable;
      schedule();
      return;
    }
    R.tl = L.timeline(S.runId, events);
    R.unreadable = unreadable;
    S.run = L.stateAfter(R.tl, 0);
    R.applied = 0;
    R.p = R.tl.t0;
    seek(R.tl.t0 + R.at, { quiet: true });
    for (const id of buildable) S.dirty.add(id);
    if (R.autoplay) play(); else announce({ kind: "run", text: `Replay of run ${shortRun(S.runId)} ready, paused at ${L.playTime(R.p - R.tl.t0)}. Space plays.` });
    schedule();
  }
  // Moves the playhead to `t`: forward by applying what happened since, backward by
  // folding again from the nearest keyframe. Every node redraws; nothing is announced.
  function seek(t, how) {
    const R = S.replay;
    if (!R || !R.tl) return;
    const tl = R.tl;
    const p = Math.max(tl.t0, Math.min(tl.t1, t));
    const n = L.indexAt(tl, p);
    if (n >= R.applied) {
      for (let i = R.applied; i < n; i++) for (const id of L.apply(S.run, tl.items[i].ev)) S.dirty.add(id);
    } else {
      S.run = L.stateAfter(tl, n);
      for (const id of buildable) S.dirty.add(id);
    }
    R.applied = n; R.p = p;
    S.run.unreadable = R.unreadable;
    for (const id of buildable) if (statusOf(id) === "running") S.dirty.add(id);
    const atEnd = tl.finished && n === tl.items.length && p >= tl.t1;
    if (atEnd && !S.done) {
      S.done = true;
      document.body.classList.add("lv-done");
      S.run.ended = { reason: "finished", outcome: S.run.outcome, inferred: false, note: "" };
      S.follow = false; S.hint = false; S.focus = null;
      for (const id of S.run.nodes.keys()) S.dirty.add(id);
    } else if (!atEnd && S.done) {
      S.done = false; S.jumped = false;
      document.body.classList.remove("lv-done");
      S.run.ended = null;
    }
    S.chipsKey = null; S.panelKey = null; S.edgesStale = true;
    if (how && how.user) { R.userMoved = true; rememberSoon(); }
    schedule();
  }
  let rememberTimer = 0;
  // The URL follows the playhead when it stops moving, not every frame.
  function rememberSoon() { clearTimeout(rememberTimer); rememberTimer = setTimeout(() => { if (S.on && S.replay && !S.replay.playing) x.remember(); }, 250); }
  function play() {
    const R = S.replay;
    if (!R || !R.tl || R.playing) return;
    // At the end, play starts again from the beginning, following the run again.
    if (R.p >= R.tl.t1) { seek(R.tl.t0); S.follow = true; S.lastCam = null; S.focus = null; }
    R.playing = true;
    R.last = performance.now();
    R.ticked = 0;
    document.body.classList.add("lv-playing");
    announce({ kind: "run", text: `Playing at ${R.speed}×` });
    R.raf = requestAnimationFrame(frame);
    schedule();
  }
  function pause(quiet) {
    const R = S.replay;
    if (!R || !R.playing) return;
    R.playing = false;
    cancelAnimationFrame(R.raf);
    document.body.classList.remove("lv-playing");
    if (!quiet) announce({ kind: "run", text: `Paused at ${L.playTime(R.p - R.tl.t0)}` });
    rememberSoon();
    schedule();
  }
  function frame(t) {
    const R = S.replay;
    if (!R || !R.playing) return;
    const dt = Math.max(0, Math.min(250, t - R.last));
    R.last = t;
    seek(R.p + dt * R.speed);
    // Running nodes' times, and the chips', move with the playhead.
    if (t - R.ticked > 100) { R.ticked = t; soFar(); }
    if (R.p >= R.tl.t1) {
      pause(true);
      announce({ kind: "run", text: R.tl.finished ? "The replay reached the end of the run" : "The replay reached the end of the journal" });
      // As live: the final view shows what ran, the failure included.
      if (R.tl.finished) requestAnimationFrame(() => {
        const { vw, vh } = viewport();
        const cam = L.finalView({ ids: buildable, inRun, statusOf, pos: positions(), vw, vh });
        if (cam) moveTo(cam);
      });
      return;
    }
    R.raf = requestAnimationFrame(frame);
  }
  const toggle = () => { const R = S.replay; if (R && R.tl) { if (R.playing) pause(); else play(); } };
  function seekBy(dms) { const R = S.replay; if (R && R.tl) seek(R.p + dms, { user: true }); }
  // The previous or next moment something happened, whatever the gap.
  function step(dir) {
    const R = S.replay;
    if (!R || !R.tl) return;
    pause(true);
    const items = R.tl.items;
    if (dir > 0) {
      const n = L.indexAt(R.tl, R.p);
      if (n < items.length) seek(items[n].at, { user: true });
    } else {
      let lo = 0, hi = items.length;
      while (lo < hi) { const mid = (lo + hi) >> 1; if (items[mid].at < R.p) lo = mid + 1; else hi = mid; }
      seek(lo > 0 ? items[lo - 1].at : R.tl.t0, { user: true });
    }
    const said = S.run.log[S.run.log.length - 1];
    if (said) announce(said);
  }
  function setSpeed(speed) {
    const R = S.replay;
    if (!R || !L.SPEEDS.includes(speed)) return;
    R.speed = speed;
    announce({ kind: "run", text: `Speed ${speed}×` });
    rememberSoon();
    schedule();
  }
  function goLive() { const scope = S.scope; start(S.runId, { scope }); }

  // The play bar, made once.
  const player = h("div", null, "lv-player lin-ui", canvas);
  player.id = "lv-player";
  player.setAttribute("role", "group");
  player.setAttribute("aria-label", "Replay controls");
  const ICON = {
    play: '<svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M7 4.5v15l12.5-7.5z"/></svg>',
    pause: '<svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M6 4h4.5v16H6zM13.5 4H18v16h-4.5z"/></svg>',
    back: '<svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M5 4h2.5v16H5zM20 4.5v15L9 12z"/></svg>',
    next: '<svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M16.5 4H19v16h-2.5zM4 4.5v15L15 12z"/></svg>',
  };
  const btn = (cls, label, title) => { const b = h("button", null, "lv-pbtn " + cls, player); b.type = "button"; b.setAttribute("aria-label", label); b.title = title; return b; };
  const playBtn = btn("lv-play", "Play", "Play or pause (Space or K)");
  playBtn.id = "lv-play";
  const prevBtn = btn("lv-prev", "Previous event", "Previous event (,)");
  prevBtn.innerHTML = ICON.back;
  const nextBtn = btn("lv-next", "Next event", "Next event (.)");
  nextBtn.innerHTML = ICON.next;
  const timeEl = h("span", "0:00 / 0:00", "mono lv-ptime", player);
  timeEl.id = "lv-ptime";
  const track = h("div", null, "lv-track", player);
  const band = h("canvas", null, "lv-band", track);
  band.setAttribute("aria-hidden", "true");
  const marks = h("div", null, "lv-marks", track);
  const scrub = h("input", null, "lv-scrub", track);
  scrub.type = "range";
  scrub.id = "lv-scrub";
  scrub.min = "0"; scrub.step = "1"; scrub.value = "0";
  scrub.setAttribute("aria-label", "Playhead: time since the run started");
  const speedSel = h("select", null, "lv-speed", player);
  speedSel.id = "lv-speed";
  speedSel.setAttribute("aria-label", "Playback speed");
  speedSel.title = "Playback speed (Shift+< slower, Shift+> faster)";
  for (const v of L.SPEEDS) { const o = h("option", `${v}×`, null, speedSel); o.value = String(v); }
  const pnote = h("span", null, "lv-pnote", player);
  pnote.id = "lv-pnote";
  const liveBtn = btn("lv-golive", "Go live", "Watch the run live instead");
  liveBtn.textContent = "Go live";
  const retryBtn = btn("lv-golive lv-retry", "Try again", "Load the run's journal again");
  retryBtn.textContent = "Try again";
  retryBtn.hidden = true;
  retryBtn.addEventListener("click", () => { if (S.replay && !S.replay.tl) load(); });
  playBtn.addEventListener("click", toggle);
  prevBtn.addEventListener("click", () => step(-1));
  nextBtn.addEventListener("click", () => step(1));
  liveBtn.addEventListener("click", goLive);
  speedSel.addEventListener("change", () => setSpeed(Number(speedSel.value)));
  // Dragging the playhead pauses; letting go plays again if it was playing.
  scrub.addEventListener("pointerdown", () => { const R = S.replay; if (R) { R.resume = R.playing; pause(true); } });
  scrub.addEventListener("input", () => { const R = S.replay; if (R && R.tl) seek(R.tl.t0 + Number(scrub.value), { user: true }); });
  scrub.addEventListener("change", () => { const R = S.replay; if (R && R.resume) { R.resume = false; play(); } });
  function playerDraw() {
    const R = S.replay;
    player.hidden = !R;
    if (!R) return;
    const tl = R.tl;
    for (const b of [playBtn, prevBtn, nextBtn, scrub, speedSel]) b.disabled = !tl;
    const playing = !!R.playing;
    if (playBtn.dataset.state !== String(playing)) {
      playBtn.dataset.state = String(playing);
      playBtn.innerHTML = playing ? ICON.pause : ICON.play;
      playBtn.setAttribute("aria-label", playing ? "Pause" : "Play");
    }
    if (speedSel.value !== String(R.speed)) speedSel.value = String(R.speed);
    retryBtn.hidden = !R.retry;
    if (!tl) {
      timeEl.textContent = R.error ? "—" : "loading…";
      pnote.textContent = R.error || "";
      liveBtn.hidden = !R.offerLive;
      return;
    }
    const len = tl.t1 - tl.t0, at = R.p - tl.t0;
    const text = `${L.playTime(at)} / ${L.playTime(len)}`;
    if (timeEl.textContent !== text) timeEl.textContent = text;
    timeEl.title = `At ${new Date(R.p).toISOString().replace("T", " ").slice(0, 19)} UTC`;
    scrub.max = String(len);
    if (scrub.value !== String(Math.round(at))) scrub.value = String(Math.round(at));
    scrub.setAttribute("aria-valuetext", `${L.playTime(at)} of ${L.playTime(len)}, ${L.runningAt(tl, R.p)} running`);
    const notes = [];
    if (tl.live === false) notes.push("Times from the run's final report: when each node finished, not when it started.");
    if (!tl.finished) notes.push("The journal ends here: the run hadn't finished when it was loaded, or it stopped.");
    if (tl.adjusted) notes.push(`${tl.adjusted} event${tl.adjusted === 1 ? " has" : "s have"} no usable time (missing, or earlier than the event before): played at the time of the event before, so ${tl.adjusted === 1 ? "its" : "their"} time is inferred.`);
    const note = notes.join(" ");
    if (pnote.textContent !== note) pnote.textContent = note;
    liveBtn.hidden = tl.finished;
    if (R.drawnFor !== tl) { R.drawnFor = tl; bandDraw(); }
  }
  // The concurrency band (how many nodes ran at each moment, to the run's peak) and the
  // failures and the end as markers: where to look, by eye.
  function bandDraw() {
    const tl = S.replay && S.replay.tl;
    if (!tl) return;
    const W = Math.max(1, track.clientWidth), H = 22, dpr = window.devicePixelRatio || 1;
    band.width = W * dpr; band.height = H * dpr;
    const ctx = band.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    const len = Math.max(1, tl.t1 - tl.t0), peak = Math.max(1, tl.peak);
    ctx.fillStyle = getComputedStyle(player).getPropertyValue("--lv-band").trim() || "#9DB8E8";
    tl.steps.forEach(([t, n], i) => {
      const next = i + 1 < tl.steps.length ? tl.steps[i + 1][0] : tl.t1;
      const x0 = (t - tl.t0) / len * W, x1 = (next - tl.t0) / len * W, hh = n / peak * H;
      if (n) ctx.fillRect(x0, H - hh, Math.max(1, x1 - x0), hh);
    });
    band.title = `Nodes running over the run: at most ${tl.peak} at once`;
    marks.replaceChildren();
    for (const m of tl.markers) {
      const b = h("button", null, "lv-mark " + m.kind + (m.inferred ? " inferred" : ""), marks);
      b.type = "button";
      b.style.left = `${(m.at - tl.t0) / len * 100}%`;
      const when = `${m.inferred ? "at about" : "at"} ${L.playTime(m.at - tl.t0)}${m.inferred ? " (inferred: its time wasn't recorded usably)" : ""}`;
      const label = m.kind === "error" ? `${x.nameOf(m.node)} failed ${when}` : `The run finished (${m.outcome || "outcome not said"}) ${when}`;
      b.setAttribute("aria-label", label + ": go there");
      b.title = label;
      b.addEventListener("click", () => {
        pause(true);
        seek(m.at, { user: true });
        if (m.node) { x.user("select"); x.select(m.node, null, { center: true }); }
      });
    }
  }
  window.addEventListener("resize", () => { if (S.replay && S.replay.tl) bandDraw(); });
  // A video player's keys, while the page isn't typing or on a control of its own.
  window.addEventListener("keydown", e => {
    const R = S.replay;
    if (!S.on || !R || !R.tl || e.ctrlKey || e.metaKey || e.altKey || S.menu) return;
    const el = document.activeElement || document.body;
    if (/^(INPUT|SELECT|TEXTAREA)$/.test(el.tagName) && el !== scrub) return;
    const onControl = el !== document.body && !!el.closest && !!el.closest("button, a, [role=button], [role=tab], .hit, .lin-panel");
    const len = R.tl.t1 - R.tl.t0;
    const k = e.key;
    let done = true;
    if ((k === " " && !onControl && el !== scrub) || k === "k" || k === "K") toggle();
    else if (k === "j" || k === "J") seekBy(-10000);
    else if (k === "l" || k === "L") seekBy(10000);
    else if ((k === "ArrowLeft" || k === "ArrowRight") && !onControl && el !== scrub) seekBy(k === "ArrowLeft" ? -5000 : 5000);
    else if ((k === "Home" || k === "End") && !onControl && el !== scrub) seek(k === "Home" ? R.tl.t0 : R.tl.t1, { user: true });
    else if (/^[0-9]$/.test(k) && !onControl) seek(R.tl.t0 + len * Number(k) / 10, { user: true });
    else if (k === ",") step(-1);
    else if (k === ".") step(1);
    else if (k === "<") setSpeed(L.nextSpeed(R.speed, -1));
    else if (k === ">") setSpeed(L.nextSpeed(R.speed, 1));
    else done = false;
    if (done) e.preventDefault();
  });

  // ---------- announcements, throttled: at most one every two seconds
  function announce(said) {
    const name = said.node ? x.nameOf(said.node) : "";
    const text = said.text || ({ started: `${name} started`, success: `${name} built`, error: `${name} failed`, skipped: `${name} skipped`, unknown: `${name}: outcome not reported` })[said.kind];
    if (!text) return;
    S.say.push(text);
  }
  // Whether it has nothing left to say: for tests and recordings that wait for quiet.
  const quiet = () => { document.body.dataset.liveQuiet = !S.say.length && !S.saying ? "1" : "0"; };
  setInterval(() => {
    if (!S.say.length) return;
    S.saying = true;
    quiet();
    const all = S.say.splice(0);
    const text = all.length > 3 ? `${all.slice(-3).join(". ")}. And ${all.length - 3} more.` : all.join(". ") + ".";
    say.textContent = "";
    setTimeout(() => { say.textContent = text; }, 30);
    // Cleared once read out, so it is never read twice (or left on the page).
    setTimeout(() => { if (say.textContent === text) say.textContent = ""; S.saying = false; quiet(); }, 1500);
  }, 2000);

  // ---------- drawing, once per frame at most
  const PILL = { queued: "QUEUED", running: "RUNNING", success: "BUILT", error: "FAILED", skipped: "SKIPPED", kept: "KEPT", unknown: "UNKNOWN", source: "SOURCE", notrun: "NOT RUN" };
  const WORD = { queued: "queued", running: "running", success: "built", error: "failed", skipped: "skipped", kept: "kept earlier build", unknown: "outcome unknown", source: "source, read not built", notrun: "not in this run" };
  const pillOf = s => s === "success" && S.run && S.run.mode === "test" ? "TESTED" : PILL[s];
  // A node the run doesn't touch keeps its last build, if it has one; else it isn't run.
  const shownStatus = id => {
    const s = statusOf(id);
    if (s !== "kept") return s;
    const d = overlayData && overlayData.nodes[id];
    return d && d.decision !== "never_built" && (d.last_built || d.decision === "reuse") ? "kept" : "notrun";
  };
  const shortRun = id => (id || "").slice(0, 8);
  function meta(id) {
    const s = shownStatus(id), n = S.run && S.run.nodes.get(id);
    switch (s) {
      case "kept": { const d = overlayData && overlayData.nodes[id]; return d && d.last_built ? `build of run ${d.last_built.run}` : "not selected"; }
      case "notrun": return "not selected";
      case "source": return "read, not built";
      case "queued": return "waiting on upstream";
      case "running": return `${L.duration(Math.max(0, now() - (n.startedAt || now())))}` + (n.thread ? ` · ${threadName(n.thread)}` : "");
      case "success": { const r = n.stats && n.stats.rows_affected; return `${L.duration(L.took(n)) || "time —"} · ${r == null ? "rows —" : `${r} rows`}`; }
      case "error": return L.took(n) != null ? `after ${L.duration(L.took(n))}` : "failed";
      case "skipped": return "upstream failed";
      default: return "outcome not reported";
    }
  }
  // Sans 11px is about 5.9px a character.
  const fitSans = (text, px) => { const max = Math.floor(px / 5.9); return text.length > max ? text.slice(0, max - 1) + "…" : text; };
  // "Thread-1 (worker)" → "thread 1"; other names as they are.
  const threadName = t => { const m = /^Thread-(\d+)/.exec(t || ""); return m ? `thread ${m[1]}` : t; };

  function decorate(g, id, hit, box) {
    if (!S.on) return;
    g.setAttribute("data-node", id);
    const n = x.byId.get(id);
    const kind = n.kind === "model" && n.opaque ? "py" : n.kind;
    const k = document.createElementNS(SVG, "text");
    k.setAttribute("class", "lv-kind");
    k.setAttribute("x", box.w - 10); k.setAttribute("y", 21);
    k.textContent = kind;
    g.appendChild(k);
    const name = g.querySelector(".name");
    if (name) name.textContent = x.fit(n.name, box.w - 30 - kind.length * 6.2);
    const halo = document.createElementNS(SVG, "rect");
    halo.setAttribute("class", "lv-halo");
    halo.setAttribute("x", -6); halo.setAttribute("y", -6); halo.setAttribute("width", box.w + 12); halo.setAttribute("height", box.h + 12); halo.setAttribute("rx", 12);
    g.insertBefore(halo, g.firstChild);
    const pill = document.createElementNS(SVG, "g");
    pill.setAttribute("class", "lv-pill");
    const pr = document.createElementNS(SVG, "rect");
    pr.setAttribute("x", 12); pr.setAttribute("y", 40); pr.setAttribute("height", 17); pr.setAttribute("rx", 4);
    const pt = document.createElementNS(SVG, "text");
    pt.setAttribute("x", 18); pt.setAttribute("y", 52);
    pill.append(pr, pt);
    g.appendChild(pill);
    const mt = document.createElementNS(SVG, "text");
    mt.setAttribute("class", "lv-meta");
    mt.setAttribute("y", 52);
    g.appendChild(mt);
    // The hit area covers the whole box, above what was drawn.
    hit.setAttribute("height", box.h);
    g.appendChild(hit);
    paintNode(id, g);
  }
  function paintNode(id, g) {
    g = g || x.nodeEl(id);
    if (!g) return;
    const s = shownStatus(id);
    const scoped = pool();
    let cls = g.getAttribute("class").replace(/ lv-\S+/g, "");
    cls += ` lv-node lv-${s}`;
    if (scoped && !scoped.has(id)) cls += " lv-faded";
    if (S.focus === id && S.follow) cls += " lv-focused";
    g.setAttribute("class", cls);
    const label = pillOf(s);
    const pt = g.querySelector(".lv-pill text"), pr = g.querySelector(".lv-pill rect"), mt = g.querySelector(".lv-meta");
    if (!pt) return;
    pt.textContent = label;
    const w = Math.round(label.length * 6.6 + 12);
    pr.setAttribute("width", w);
    const m = meta(id);
    mt.setAttribute("x", 12 + w + 7);
    mt.textContent = fitSans(m, 196 - 12 - w - 7 - 8);
    const hit = g.querySelector(".hit");
    if (hit) {
      hit.setAttribute("aria-label", `${x.nameOf(id)}, ${WORD[s]}, ${m}`);
      hit.setAttribute("aria-haspopup", "menu");
      const t = hit.querySelector("title");
      if (t) t.textContent = `${x.nameOf(id)}\n${WORD[s]} · ${m}\nRight-click, Shift+F10 or the menu key: follow options`;
    }
  }
  function paintEdges() {
    for (const p of document.querySelectorAll("#lin-edges path[data-to]")) {
      const a = shownStatus(p.dataset.from), b = shownStatus(p.dataset.to);
      let cls = p.getAttribute("class").replace(/ lv-\S+/g, "");
      if (b === "running") cls += " lv-into-running";
      else if (b === "skipped") cls += " lv-into-skipped";
      else if ((a === "kept" || a === "notrun" || a === "source") && (b === "kept" || b === "notrun")) cls += " lv-quiet";
      const scoped = pool();
      if (scoped && !(scoped.has(p.dataset.from) && scoped.has(p.dataset.to))) cls += " lv-faded";
      p.setAttribute("class", cls);
    }
  }

  function draw() {
    if (!S.on) return;
    // How far the page has read, and whether it has something left to say: for tests
    // and recordings that wait for the page to catch up.
    document.body.dataset.liveLine = String(S.run.lastId);
    quiet();
    for (const id of S.dirty) paintNode(id);
    if (S.dirty.size || S.edgesStale) { paintEdges(); S.edgesStale = false; }
    S.dirty.clear();
    tick(false);
    header();
    controls();
    if (!x.state.sel) runPanel(); else refreshCard();
    if (S.legendFor !== S.run.mode) { S.legendFor = S.run.mode; x.legend(); }
    playerDraw();
  }

  // ---------- follow mode
  // The minimap's corner, kept clear while framing.
  const PLAYER = 96;
  const inset = () => {
    const bottom = (S.done ? 0 : 190) + (S.replay ? PLAYER : 0);
    return bottom ? { bottom } : {};
  };
  function viewport() { const r = canvas.getBoundingClientRect(); return { vw: r.width, vh: r.height }; }
  function positions() {
    const out = new Map(), { w } = x.size();
    for (const [id, p] of x.pos()) out.set(id, { x: p.x, y: p.y, w, h: p.h });
    return out;
  }
  function wanted() {
    const { vw, vh } = viewport();
    return L.follow({
      ids: buildable, statusOf, inRun, graph, pos: positions(), vw, vh,
      view: { x: x.view.x, y: x.view.y, k: x.view.k }, focus: S.focus, pool: pool(), done: S.done,
      root: S.scope && S.scope.id, inset: inset(),
    });
  }
  // Moves the camera where follow wants it, at most once a second unless `now`.
  function tick(now) {
    const w = wanted();
    S.want = w;
    if (S.follow) {
      if (w.focus !== S.focus) { const was = S.focus; S.focus = w.focus; if (was) S.dirty.add(was); if (w.focus) S.dirty.add(w.focus); }
      const t = performance.now();
      if (w.cam && L.moved(S.lastCam || x.view, w.cam) && (now || t - S.lastMove >= L.MOVE_EVERY)) {
        S.lastMove = t; S.lastCam = w.cam;
        moveTo(w.cam);
      }
    }
    chipsDraw();
    miniDraw();
    for (const id of S.dirty) paintNode(id);
    S.dirty.clear();
  }
  function moveTo(cam) {
    if (S.tween) cancelAnimationFrame(S.tween);
    const from = { x: x.view.x, y: x.view.y, k: x.view.k };
    if (still()) { Object.assign(x.view, { x: cam.x, y: cam.y, k: cam.k }); x.apply(); return; }
    const t0 = performance.now(), T = 700;
    const step = t => {
      const u = Math.min(1, (t - t0) / T), e = u < 0.5 ? 2 * u * u : 1 - Math.pow(-2 * u + 2, 2) / 2;
      x.view.x = from.x + (cam.x - from.x) * e; x.view.y = from.y + (cam.y - from.y) * e; x.view.k = from.k + (cam.k - from.k) * e;
      x.apply();
      S.tween = u < 1 ? requestAnimationFrame(step) : null;
      if (!S.tween) { chipsDraw(); miniDraw(); }
    };
    S.tween = requestAnimationFrame(step);
  }
  function setFollow(on, why) {
    if (!S.on) return;
    if (!on && S.follow && S.tween) { cancelAnimationFrame(S.tween); S.tween = null; }
    S.follow = on;
    S.hint = !on && why === "user" && !S.done;
    if (on) { S.lastCam = null; S.focus = null; tick(true); }
    for (const id of S.run.nodes.keys()) S.dirty.add(id);
    controls();
    schedule();
  }
  const WHY_OFF = { pan: "You moved the view", zoom: "You zoomed", fit: "You used Fit", select: "You selected a node" };
  x.hooks.user.push(kind => {
    if (S.on && S.follow) { S.offWhy = WHY_OFF[kind] || "You moved the view"; setFollow(false, "user"); }
    if (kind !== "select") closeMenu();
  });
  followBtn.addEventListener("click", () => setFollow(!S.follow, S.follow ? "toggle" : null));
  hintBtn.addEventListener("click", () => setFollow(true));
  zoomIn.addEventListener("click", () => zoom(1.25));
  zoomOut.addEventListener("click", () => zoom(0.8));
  function zoom(f) {
    x.user("zoom");
    const { vw, vh } = viewport();
    const k = Math.max(0.2, Math.min(2, x.view.k * f));
    const cx = (vw / 2 - x.view.x) / x.view.k, cy = (vh / 2 - x.view.y) / x.view.k;
    Object.assign(x.view, { k, x: vw / 2 - cx * k, y: vh / 2 - cy * k });
    x.apply();
    chipsDraw(); miniDraw();
  }
  window.addEventListener("keydown", e => {
    if (!S.on || e.ctrlKey || e.metaKey || e.altKey) return;
    const typing = /^(INPUT|SELECT|TEXTAREA)$/.test((document.activeElement || {}).tagName || "");
    if (typing) return;
    if ((e.key === "f" || e.key === "F") && !S.menu) { e.preventDefault(); setFollow(!S.follow, S.follow ? "toggle" : null); }
  });
  // Pans and zooms also redraw the chips and the minimap's view.
  new MutationObserver(() => { if (S.on && !S.chipsQueued) { S.chipsQueued = true; requestAnimationFrame(() => { S.chipsQueued = false; chipsDraw(); miniDraw(); }); } })
    .observe($("lin-viewport"), { attributes: true, attributeFilter: ["transform"] });

  // ---------- edge chips: running nodes out of view
  function chipsDraw() {
    if (!S.on) return;
    const { vw, vh } = viewport();
    const scoped = pool();
    const running = buildable.filter(id => statusOf(id) === "running" && (!scoped || scoped.has(id))).sort();
    const off = L.offscreen(running, positions(), x.view, vw, vh);
    const arrows = { right: "→", left: "←", top: "↑", bottom: "↓" };
    const where = { right: "to the right", left: "to the left", top: "above", bottom: "below" };
    const focusMode = S.follow && S.want && S.want.mode === "focus";
    const key = off.map(o => `${o.id}:${o.side}`).join("|") + (focusMode ? "|focus" : "");
    if (key === S.chipsKey) return;
    S.chipsKey = key;
    for (const side of Object.keys(chips)) {
      const list = off.filter(o => o.side === side);
      chips[side].replaceChildren();
      const note = side === "right" && focusMode;
      chips[side].hidden = !list.length && !note;
      if (note) h("div", "When the focus finishes, follow moves to the running node blocking the most queued work, then the nearest.", "lv-chips-note", chips[side]);
      if (!list.length) continue;
      if (side === "right" || side === "left") h("div", `${list.length} RUNNING OFF SCREEN`, "lv-chips-head", chips[side]);
      for (const o of list) {
        const n = S.run.nodes.get(o.id);
        const since = n.startedAt || now();
        const secs = L.duration(Math.max(0, now() - since));
        const b = h("button", null, "lv-chip", chips[side]);
        b.type = "button";
        b.dataset.node = o.id;
        b.setAttribute("aria-label", `Show ${x.nameOf(o.id)}, running ${secs}, off screen ${where[side]}`);
        if (side === "left") h("span", arrows[side], "lv-arrow", b).setAttribute("aria-hidden", "true");
        h("span", x.nameOf(o.id), "lv-chip-name", b);
        const t = h("span", secs, "lv-chip-time", b);
        t.dataset.since = since;
        if (side !== "left") h("span", arrows[side], "lv-arrow", b).setAttribute("aria-hidden", "true");
        b.addEventListener("click", () => {
          // The chip's node becomes the focus, and follow stays on.
          S.follow = true; S.hint = false; S.focus = o.id; S.lastCam = null;
          const { vw: w2, vh: h2 } = viewport();
          const bx = L.box([o.id], positions());
          if (bx) { const c = L.fit(bx, w2, h2, L.MAX_FOLLOW, inset()); S.lastCam = c; S.lastMove = performance.now(); moveTo(c); }
          for (const id of S.run.nodes.keys()) S.dirty.add(id);
          schedule();
        });
      }
    }
  }

  // ---------- the minimap: the whole run, running nodes pulsing, the view as a box
  let miniScale = null;
  function miniDraw() {
    if (!S.on) return;
    const ctx = miniCanvas.getContext("2d");
    const dpr = window.devicePixelRatio || 1;
    const W = 300, H = 170, top = 26, pad = 10;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    const pos = positions();
    const b = L.box(pos.keys(), pos);
    if (!b) return;
    const k = Math.min((W - 2 * pad) / (b.x1 - b.x0), (H - top - pad) / (b.y1 - b.y0));
    const ox = pad + ((W - 2 * pad) - (b.x1 - b.x0) * k) / 2 - b.x0 * k, oy = top + ((H - top - pad) - (b.y1 - b.y0) * k) / 2 - b.y0 * k;
    miniScale = { k, ox, oy };
    const css = getComputedStyle(mini);
    const color = s => css.getPropertyValue(`--mini-${s}`).trim() || "#C9CFD8";
    const pulse = still() ? 0.5 : 0.5 + 0.5 * Math.sin(performance.now() / 260);
    for (const [id, p] of pos) {
      const s = shownStatus(id);
      const x0 = ox + p.x * k, y0 = oy + p.y * k, w = Math.max(3, p.w * k), hh = Math.max(2, p.h * k);
      if (s === "running") {
        ctx.fillStyle = color("halo");
        ctx.globalAlpha = 0.25 + 0.35 * pulse;
        ctx.fillRect(x0 - 3, y0 - 3, w + 6, hh + 6);
        ctx.globalAlpha = 1;
      }
      ctx.fillStyle = color(s);
      ctx.fillRect(x0, y0, w, hh);
    }
    const { vw, vh } = viewport();
    const vx0 = (0 - x.view.x) / x.view.k, vy0 = (0 - x.view.y) / x.view.k;
    ctx.strokeStyle = color("view");
    ctx.lineWidth = 2;
    // The view's box, kept inside the minimap so all four sides show.
    const rx0 = Math.max(2, ox + vx0 * k), ry0 = Math.max(2, oy + vy0 * k);
    const rx1 = Math.min(W - 2, ox + (vx0 + vw / x.view.k) * k), ry1 = Math.min(H - 2, oy + (vy0 + vh / x.view.k) * k);
    if (rx1 > rx0 && ry1 > ry0) ctx.strokeRect(rx0, ry0, rx1 - rx0, ry1 - ry0);
  }
  setInterval(() => { if (S.on && !S.done && buildable.some(id => statusOf(id) === "running")) miniDraw(); }, still() ? 1000 : 120);
  miniCanvas.addEventListener("click", e => {
    if (!miniScale) return;
    const r = miniCanvas.getBoundingClientRect();
    const gx = (e.clientX - r.left - miniScale.ox) / miniScale.k, gy = (e.clientY - r.top - miniScale.oy) / miniScale.k;
    x.user("pan");
    const { vw, vh } = viewport();
    x.view.x = vw / 2 - gx * x.view.k; x.view.y = vh / 2 - gy * x.view.k;
    x.apply();
    chipsDraw(); miniDraw();
  });

  // ---------- the toolbar, header, hint and toast
  // "· 36 nodes" after the page's name, as the large-project board has it.
  const crumb = document.querySelector("header.top .crumb-here");
  const count = crumb ? h("span", null, "lv-node-count", null) : null;
  if (crumb) crumb.insertAdjacentElement("afterend", count);
  function header() {
    if (count) count.textContent = S.on ? ` · ${buildable.length.toLocaleString("en")} nodes` : "";
    if (!status) return;
    const running = !S.done;
    const c = L.counts(S.run);
    const requested = S.run.requested.length || S.run.nodes.size;
    const finished = c.success + c.error + c.skipped + c.unknown;
    const R = S.replay;
    const word = R ? (R.tl ? `Replay · run ${shortRun(S.runId)} · ${R.playing ? "playing" : "paused"} · ${finished} of ${requested} finished · ${c.running} running`
      : R.error ? `Replay · run ${shortRun(S.runId)} · couldn't load` : `Loading run ${shortRun(S.runId)}'s journal`)
      : S.waiting ? `Waiting for run ${shortRun(S.runId)}'s journal`
      : running ? `Live · run ${shortRun(S.runId)} · ${finished} of ${requested} finished · ${c.running} running` : `Finished · run ${shortRun(S.runId)}`;
    const ending = S.run.ended && S.run.ended.reason;
    const text = ending === "missing" ? `No journal · run ${shortRun(S.runId)}` : ending && ending !== "finished" ? `Stopped? · run ${shortRun(S.runId)}` : word;
    if (status.dataset.live !== text) {
      status.dataset.live = text;
      status.innerHTML = "";
      const pulse = running && !R;
      status.className = "pill-snap lin-status lv-pill-status" + (pulse ? " running" : "");
      h("span", null, pulse ? "dot lv-dot" : "dot lv-dot-done", status);
      status.append(text);
      status.title = R ? "A replay of the run's journal: nothing is running" : running ? "Probably running: its journal is still being written" : (S.run.ended ? S.run.ended.note : "");
    }
  }
  const elapsed = msv => { const s = Math.max(0, Math.floor(msv / 1000)); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`; };
  function controls() {
    followBtn.setAttribute("aria-pressed", String(S.follow));
    followBtn.classList.toggle("on", S.follow);
    followBtn.querySelector("span").textContent = S.follow ? "Following run" : "Follow run";
    followBtn.disabled = S.done;
    followBtn.title = S.done ? "The run finished" : "Keep the running nodes in view (F)";
    scopeChip.hidden = !S.scope;
    if (S.scope) {
      const set = L.scopeSet(graph, S.scope), n = set.size - 1;
      // Once the run is over nothing is followed: the scope only fades the rest.
      const lead = S.done ? "Scope:" : "Following";
      scopeText.textContent = S.scope.dir === "self" ? `${lead} ${x.nameOf(S.scope.id)}` : `${lead} ${x.nameOf(S.scope.id)} + ${n} ${S.scope.dir === "down" ? "downstream" : "upstream"}`;
      scopeChip.classList.toggle("static", S.done);
      scopeClear.setAttribute("aria-label", S.done ? "Clear the scope" : "Stop following this scope; follow the whole run");
    }
    const w = S.want;
    focusLine.replaceChildren();
    if (S.follow && w && w.mode === "focus") {
      focusLine.append("Focus: ");
      h("span", x.nameOf(w.focus), "mono", focusLine);
      focusLine.append(` · blocks ${w.blocks} queued node${w.blocks === 1 ? "" : "s"}`);
      focusLine.title = "Running nodes are too far apart to show at a readable size, so follow keeps one in focus until it finishes, then moves to the running node blocking the most queued work (the nearest one on a tie).";
    } else focusLine.title = "";
    hint.hidden = !(S.hint && !S.follow && !S.done);
    hintText.textContent = `Follow is off: ${(S.offWhy || "You moved the view").replace(/^You/, "you")}.`;
    toastDraw();
  }
  function toastDraw() {
    const w = S.want;
    let key = "", build = null;
    if (S.done) {
      const c = L.counts(S.run);
      const kept = buildable.filter(id => !inRun(id) && shownStatus(id) === "kept").length;
      const took = S.run.finishedAt && S.run.startedAt ? L.duration(S.run.finishedAt - S.run.startedAt) : null;
      const failed = [...S.run.nodes.values()].filter(n => n.status === "error").map(n => n.id);
      key = `done:${c.success}:${c.error}:${c.skipped}:${kept}:${took}:${S.run.ended && S.run.ended.reason}:${!!S.jumped}:${!!S.replay}`;
      build = () => {
        toast.className = "lv-toast lin-ui" + (failed.length ? " failed" : "");
        const stopped = S.run.ended && S.run.ended.reason !== "finished";
        h("strong", S.run.ended.reason === "missing" ? "No journal for this run" : stopped ? "The run's journal stopped" : (took ? `Run finished in ${took}` : "Run finished"), null, toast);
        const parts = [`${c.success} ${S.run.mode === "test" ? "tested" : "built"}`];
        if (c.error) parts.push(`${c.error} failed`);
        if (c.skipped) parts.push(`${c.skipped} skipped`);
        if (c.unknown) parts.push(`${c.unknown} unknown`);
        parts.push(`${kept} kept`);
        h("span", stopped ? S.run.ended.note : parts.join(" · "), "lv-toast-sub", toast);
        // Gone once the failure is selected and in view.
        if (failed.length && !S.jumped) {
          const b = h("button", failed.length === 1 ? "Jump to failure" : `Jump to failures (${failed.length})`, "lv-jump", toast);
          b.type = "button";
          b.addEventListener("click", () => jumpToFailure(failed));
        }
        // Watch it again (ADR-0026); in a replay, the play bar does that.
        if (!S.replay && S.run.ended.reason !== "missing") {
          const b = h("button", "Replay", "lv-replay-btn", toast);
          b.type = "button";
          b.title = "Play the run again from its journal, at any speed";
          b.addEventListener("click", () => start(S.runId, { replay: true, autoplay: true, scope: S.scope }));
        }
      };
    } else if (S.scope && w && w.mode === "scope-done") {
      key = "scope-done:" + S.scope.id;
      build = () => {
        toast.className = "lv-toast lin-ui";
        h("strong", `${x.nameOf(S.scope.id)}${S.scope.dir === "self" ? "" : S.scope.dir === "down" ? " and its downstream" : " and its upstream"} finished`, null, toast);
        const b = h("button", "Follow whole run", "lv-whole", toast);
        b.type = "button";
        b.addEventListener("click", () => setScope(null));
      };
    }
    if (key === S.toastKey) return;
    S.toastKey = key;
    toast.replaceChildren();
    toast.hidden = !build;
    if (build) build();
  }
  function jumpToFailure(failed) {
    const first = failed.slice().sort((a, b) => x.nameOf(a) < x.nameOf(b) ? -1 : 1)[0];
    const around = [...L.reach(graph.down, first)].filter(id => id === first || statusOf(id) === "skipped");
    S.follow = false; S.hint = false; S.jumped = true;
    x.select(first, null);
    const { vw, vh } = viewport();
    const b = L.box(around, positions());
    if (b) moveTo(L.fitReadable(b, vw, vh));
    const t = x.target(first); if (t) t.focus({ preventScroll: true });
  }

  // ---------- scopes
  function setScope(scope) {
    S.scope = scope; S.scopeDoneShown = false; S.focus = null; S.lastCam = null;
    if (scope) S.follow = true;
    S.hint = false;
    for (const id of buildable) S.dirty.add(id);
    S.edgesStale = true;
    x.remember();
    tick(true);
    // The chip is the click's own answer, so it changes now, not on the next frame.
    controls();
    schedule();
  }
  scopeClear.addEventListener("click", () => setScope(null));

  // ---------- the node menu: right-click, the menu key, or Shift+F10
  function openMenu(id, anchor) {
    closeMenu();
    S.menu = { id, anchor };
    const downN = L.reach(graph.down, id).size - 1, upN = L.reach(graph.up, id).size - 1;
    menu.setAttribute("aria-label", `Actions for ${x.nameOf(id)}`);
    menu.replaceChildren();
    h("div", x.nameOf(id), "lv-menu-id", menu).setAttribute("role", "presentation");
    const item = (label, sub, act, cls) => {
      const b = h("button", null, "lv-item" + (cls ? " " + cls : ""), menu);
      b.type = "button";
      b.setAttribute("role", "menuitem");
      b.tabIndex = -1;
      h("span", label, "lv-item-label", b);
      if (sub) h("span", sub, "lv-item-sub", b);
      b.addEventListener("click", () => { closeMenu(); act(); backTo(id); });
      return b;
    };
    item("Follow this node and its downstream", `${downN} downstream node${downN === 1 ? "" : "s"}; the rest of the run fades`, () => setScope({ id, dir: "down" }), "first");
    item("Follow this node and its upstream", `${upN} upstream node${upN === 1 ? "" : "s"}`, () => setScope({ id, dir: "up" }));
    item("Follow just this node", null, () => setScope({ id, dir: "self" }));
    h("div", null, "lv-menu-sep", menu).setAttribute("role", "separator");
    item("Show node stats", null, () => x.select(id, null));
    item("Close", null, () => {}, "muted");
    menu.hidden = false;
    // Beside the node, inside the canvas.
    const host = x.canvas.parentElement.getBoundingClientRect();
    const r = anchor.getBoundingClientRect();
    const left = Math.min(host.width - 284, Math.max(8, r.right - host.left + 8));
    const top = Math.min(host.height - menu.offsetHeight - 8, Math.max(60, r.top - host.top));
    menu.style.left = left + "px";
    menu.style.top = top + "px";
    menu.querySelector("[role=menuitem]").focus({ preventScroll: true });
  }
  // The focus goes back to the node the menu was for (as drawn now: a render may have
  // replaced the element it opened on).
  function backTo(id) {
    if (document.activeElement && document.activeElement !== document.body && !menu.contains(document.activeElement)) return;
    const t = x.target(id);
    if (t) t.focus({ preventScroll: true });
  }
  function closeMenu() {
    if (!S.menu) return;
    S.menu = null;
    menu.hidden = true;
    menu.replaceChildren();
  }
  menu.addEventListener("keydown", e => {
    const items = [...menu.querySelectorAll("[role=menuitem]")];
    const i = items.indexOf(document.activeElement);
    const go = j => { e.preventDefault(); e.stopPropagation(); items[(j + items.length) % items.length].focus(); };
    if (e.key === "ArrowDown") go(i + 1);
    else if (e.key === "ArrowUp") go(i - 1);
    else if (e.key === "Home") go(0);
    else if (e.key === "End") go(items.length - 1);
    else if (e.key === "Escape" || e.key === "Tab") {
      // Handled here: the page's own Escape would clear the selection.
      e.preventDefault();
      e.stopPropagation();
      const id = S.menu && S.menu.id;
      closeMenu();
      if (id) backTo(id);
    }
  });
  document.addEventListener("mousedown", e => { if (S.menu && !menu.contains(e.target)) closeMenu(); }, true);
  x.hooks.nodeMenu = (id, hit) => { if (!S.on) return false; openMenu(id, hit); return true; };
  x.hooks.nodeKey = (e, id, hit) => {
    if (!S.on) return;
    if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) { e.preventDefault(); openMenu(id, hit); }
  };

  // ---------- the side panel: the run, or the selected node's stats
  function runPanel() {
    const P = x.panelEl;
    const run = S.run;
    const c = L.counts(run);
    const requested = run.requested.length || run.nodes.size;
    const finished = [...run.nodes.values()].filter(n => L.FINISHED.has(n.status)).length;
    const notSelected = buildable.filter(id => !inRun(id)).length;
    const kept = buildable.filter(id => !inRun(id) && shownStatus(id) === "kept").length;
    const running = [...run.nodes.values()].filter(n => n.status === "running").sort((a, b) => (a.startedAt || 0) - (b.startedAt || 0));
    const r = L.rows(run);
    const key = JSON.stringify([c, finished, kept, running.map(n => [n.id, n.thread]), r, run.log.length, S.done, run.unreadable, run.outcome, run.startedAt,
      S.replay && S.replay.tl ? Math.floor(S.replay.p / 1000) : 0]);
    if (key === S.panelKey && P.querySelector(".lv-run")) return;
    S.panelKey = key;
    P.replaceChildren();
    const body = h("div", null, "lp-body lv-run", P);
    const head = h("div", null, "lv-run-head", body);
    h("div", ({ build: "ods state build", run: "ods state run", test: "ods state test" })[run.mode] || "a run", "lv-cmd", head);
    const sub = h("div", null, "lv-sub", head);
    sub.append(`run ${shortRun(run.id)}`);
    if (run.scope) sub.append(` · scope ${run.scope}`);
    if (run.startedAt) sub.append(` · started ${clock(run.startedAt)}`);
    const prog = h("div", null, "lv-progress", body);
    const line = h("div", null, "lv-progress-line", prog);
    const verdict = !S.done ? (run.startedAt ? "Running" : "Starting") : run.ended && run.ended.reason !== "finished" ? "Probably stopped" : c.error ? `Finished with ${c.error} failure${c.error === 1 ? "" : "s"}` : run.outcome === "succeeded" ? "Succeeded" : `Finished: ${run.outcome || "outcome not said"}`;
    h("strong", verdict, null, line);
    const el = h("span", run.startedAt ? elapsed((S.done && run.finishedAt ? run.finishedAt : now()) - run.startedAt) : "—", "mono lv-run-time", line);
    if (run.startedAt && !S.done) { el.dataset.since = run.startedAt; el.dataset.fmt = "clock"; }
    const barEl = h("div", null, "lv-bar-track", prog);
    const fill = h("div", null, "lv-bar-fill" + (c.error ? " failed" : ""), barEl);
    fill.style.width = `${requested ? Math.round(100 * finished / requested) : 0}%`;
    barEl.setAttribute("role", "progressbar");
    barEl.setAttribute("aria-valuemin", "0");
    barEl.setAttribute("aria-valuemax", String(requested));
    barEl.setAttribute("aria-valuenow", String(finished));
    barEl.setAttribute("aria-label", "Selected nodes finished");
    h("div", `${finished} of ${requested} selected nodes finished · ${notSelected} not selected`, "lv-note", prog);
    const grid = h("div", null, "lv-counts", body);
    const tile = (n, label, cls) => { const t = h("div", null, "lv-count " + cls, grid); h("div", String(n), "lv-n", t); h("div", label, "lv-l", t); };
    tile(c.running, "RUNNING", "running");
    tile(c.success, run.mode === "test" ? "TESTED" : "BUILT", "success");
    tile(c.error, "FAILED", "error");
    tile(c.queued, "QUEUED", "queued");
    tile(c.skipped, "SKIPPED", "skipped");
    tile(kept, "KEPT", "kept");
    if (c.unknown) tile(c.unknown, "UNKNOWN", "unknown");
    h("h3", "Now running", "lv-h", body);
    if (!running.length) h("p", S.done ? "Nothing: the run has finished." : run.startedAt ? "Waiting for the next node to start." : "Starting: checking relations…", "lv-note", body);
    for (const n of running) {
      const b = h("button", null, "lv-running", body);
      b.type = "button";
      h("span", x.nameOf(n.id), "mono", b);
      h("span", n.thread ? threadName(n.thread) : "", "lv-thread", b);
      const t = h("span", L.duration(Math.max(0, now() - (n.startedAt || now()))), "mono lv-elapsed", b);
      if (n.startedAt) t.dataset.since = n.startedAt;
      b.addEventListener("click", () => { x.user("select"); x.select(n.id, null, { center: true }); });
    }
    h("h3", "So far", "lv-h", body);
    const rowsLine = h("div", null, "lv-kv", body);
    h("span", "Rows affected", null, rowsLine);
    const short = r.missingBuilt + r.missingOther;
    h("span", r.reported ? (short ? `at least ${r.sum}` : String(r.sum)) : "—", "mono", rowsLine);
    let note;
    if (!r.finished) note = "No node has finished yet.";
    // Nothing reported: no total, so no claim about one.
    else if (!r.reported) note = "No node reported rows yet.";
    else if (short) {
      const parts = [];
      const s = n => (n === 1 ? "" : "s");
      if (r.missingBuilt) parts.push(`${r.missingBuilt} built node${s(r.missingBuilt)} didn't report rows (many adapters report none for views or merges)`);
      if (r.missingOther) parts.push(`${r.missingOther} failed or unknown node${s(r.missingOther)} may have written some`);
      note = `${parts.join("; ")}, so the total is a lower bound.`;
    } else note = "Every node that ran reported its rows.";
    h("p", note, "lv-note", body);
    if (run.unreadable) h("p", `${run.unreadable} line${run.unreadable === 1 ? "" : "s"} of the journal couldn't be read (a newer version, or cut short).`, "warn", body);
    if (run.live === false) h("p", "This run's events came from its final results only: no times, rows or threads while it ran.", "warn", body);
    h("h3", "Events", "lv-h", body);
    const log = h("ol", null, "lv-log", body);
    for (const e of run.log.slice(-7).reverse()) {
      const li = h("li", null, "lv-ev " + e.kind, log);
      h("span", e.at ? clock(e.at) : "", "mono lv-t", li);
      const name = e.node ? x.nameOf(e.node) : "";
      h("span", e.text || ({
        started: `${name} started`, success: `${name} ${run.mode === "test" ? "tested" : "built"}` + (e.rows != null ? `, ${e.rows} rows` : ""),
        error: `${name} failed`, skipped: `${name} skipped: upstream failed`, unknown: `${name}: outcome not reported`,
      })[e.kind], null, li);
    }
    const more = h("p", null, "lv-more", body);
    const a = h("a", "The run's page →", null, more);
    a.href = x.baseUrl + "state/runs/" + encodeURIComponent(run.id);
  }
  const clock = t => new Date(t).toISOString().slice(11, 19);

  function cardPanel(P, sel) {
    const id = sel.node, n = x.byId.get(id);
    P.replaceChildren();
    const head = h("div", null, "lp-head", P);
    const title = h("div", null, "lp-title", head);
    h("span", n.name, "nm", title);
    const close = h("button", "×", "lp-close", title);
    close.type = "button";
    close.setAttribute("aria-label", "Close node stats");
    close.addEventListener("click", () => { x.clearSel(); P.focus(); });
    const body = h("div", null, "lp-body lv-card-body", P);
    body.id = "lv-card";
    const slot = h("div", null, "lv-card-slot", body);
    S.card.slot = slot;
    fillCard(true);
    const actions = h("div", null, "lv-card-actions", body);
    const follow = h("button", "Follow this node and its downstream", "lv-follow-down", actions);
    follow.type = "button";
    follow.addEventListener("click", () => setScope({ id, dir: "down" }));
    const links = h("p", null, "lv-links", body);
    const d = overlayData && overlayData.nodes[id];
    if (d && d.why_href) { const a = h("a", "Why this decision", null, links); a.href = x.baseUrl + d.why_href; }
    const m = h("a", "Model page", null, links); m.href = x.baseUrl + "catalog/" + x.enc(id);
    const rp = h("a", "Run page", null, links); rp.href = x.baseUrl + "state/runs/" + encodeURIComponent(S.runId) + "?tab=nodes";
    return true;
  }
  // The card, from the server (the Run page's view of the node), fetched again when the
  // node's status changes; the page counts a running node's time itself.
  async function fillCard(force) {
    const id = x.state.sel && x.state.sel.node;
    if (!id || !S.card.slot) return;
    const s = shownStatus(id);
    // In a replay the server's card is the run's final word on the node: shown only
    // once the playhead has passed the node's last event, never before (no future).
    const R = S.replay;
    const settled = !R || (!!R.tl && (R.tl.lastIndex.get(id) || 0) <= R.applied);
    const key = s + (settled ? "" : ":at-playhead");
    if (!force && S.card.node === id && S.card.status === key) return;
    S.card.node = id; S.card.status = key;
    const slot = S.card.slot;
    if (!inRun(id) || !settled) { slot.replaceChildren(localCard(id)); return; }
    try {
      const r = await fetch(x.baseUrl + "state/runs/" + encodeURIComponent(S.runId) + "/card?node=" + encodeURIComponent(id));
      if (!r.ok) throw new Error(String(r.status));
      const html = await r.text();
      if (S.card.slot !== slot || S.card.node !== id) return;
      // The server escaped every value in it.
      const t = document.createElement("template");
      t.innerHTML = html;
      slot.replaceChildren(t.content);
      whyItRan(slot, id);
      soFar();
    } catch (_) {
      if (S.card.slot === slot) slot.replaceChildren(localCard(id));
    }
  }
  // A run that recorded no snapshot has no recorded reason: the plan's, as the page
  // loaded it (the plan the run is carrying out while it goes), said as such.
  function whyItRan(slot, id) {
    const dl = slot.querySelector(".lv-stats");
    const d = overlayData && overlayData.nodes[id];
    if (!dl || !d || dl.querySelector('[data-stat="why_it_ran"]')) return;
    const row = document.createElement("div");
    row.className = "lv-row";
    row.dataset.stat = "why_it_ran";
    h("dt", "Planned because", null, row);
    const dd = h("dd", null, null, row);
    const first = d.reasons && d.reasons[0];
    h("span", first ? first.message : d.summary, "lv-v", dd);
    h("span", "the plan's reason when this page opened; the run records its own once it commits", "lv-sub", dd);
    const tests = dl.querySelector('[data-stat="tests"]');
    dl.insertBefore(row, tests || null);
  }
  // What the events alone say, when the server has no card (e.g. not in the run).
  function localCard(id) {
    const s = shownStatus(id), n = S.run.nodes.get(id);
    const wrap = document.createElement("section");
    wrap.className = "lv-card";
    wrap.dataset.status = s;
    const pills = h("div", null, "lv-card-pills", wrap);
    h("span", pillOf(s), "lv-pill " + s, pills);
    const dl = h("dl", null, "lv-stats", wrap);
    const row = (k, v, sub) => { const d = h("div", null, "lv-row", dl); h("dt", k, null, d); const dd = h("dd", null, null, d); h("span", v, "lv-v", dd); if (sub) h("span", sub, "lv-sub", dd); };
    row("Status", WORD[s]);
    if (!n) {
      const d = overlayData && overlayData.nodes[id];
      row("This run", s === "kept" ? "Not selected: its last build is kept" : s === "source" ? "A source: read, not built" : "Not selected", d && d.last_built ? `build of run ${d.last_built.run}` : "");
    } else {
      row("Started", n.startedAt ? clock(n.startedAt) : "—", n.startedAt ? "" : "not recorded yet");
      row("Time taken", L.duration(L.took(n)) || "—", L.took(n) == null ? "not timed yet" : "");
      const r = n.stats && n.stats.rows_affected;
      if (n.stats) row("Rows affected", r == null ? "—" : String(r), r == null ? "not reported" : "");
    }
    if (S.replay && n) h("p", "As at the playhead: the run's full card for this node shows once the replay passes its last event.", "lv-note", wrap);
    return wrap;
  }
  // Times that count up while the run goes, updated in place (focus stays put).
  function soFar() {
    const t = now();
    for (const e of document.querySelectorAll("[data-since]")) {
      const d = Math.max(0, t - Number(e.dataset.since));
      e.textContent = e.classList.contains("lv-so-far") ? `${L.duration(d)} so far` : e.dataset.fmt === "clock" ? elapsed(d) : L.duration(d);
    }
  }
  function refreshCard() { if (S.on && x.state.sel) fillCard(false); }
  // Copy buttons in the card (the Run page's explanation card has them).
  x.panelEl.addEventListener("click", async e => {
    const b = e.target.closest("button[data-copy]");
    if (!b) return;
    try { await navigator.clipboard.writeText(b.getAttribute("data-copy")); b.classList.add("copied"); setTimeout(() => b.classList.remove("copied"), 1200); } catch (_) { /* shown as text */ }
  });

  // ---------- the legend
  const LEGEND = {
    queued: "waiting to run", running: "running now", success: "built in this run", error: "failed in this run",
    skipped: "skipped: something upstream failed", kept: "not selected: its last build is kept",
    notrun: "not selected, and no build of it is recorded", source: "a source: read, never built",
  };
  function legendDraw(boxEl) {
    if (!S.on) return false;
    const shown = ["queued", "running", "success", "error", "skipped", "kept"];
    for (const s of ["notrun", "source"]) if (x.doc.nodes.some(n => shownStatus(n.id) === s)) shown.push(s);
    for (const s of shown) {
      const pill = h("span", pillOf(s), "lv-pill " + s, boxEl);
      pill.title = LEGEND[s];
    }
    return true;
  }

  // ---------- hooks into the explorer
  x.hooks.decorate = (g, id, hit, box) => { if (S.on) decorate(g, id, hit, box); };
  x.hooks.afterRender = x.hooks.afterRender || [];
  x.hooks.afterRender.push(() => { if (S.on) { paintEdges(); S.chipsKey = null; } });
  x.hooks.legend = legendDraw;
  x.hooks.params = params_;
  x.hooks.panel = (P, sel) => {
    if (!S.on) return false;
    if (sel) return cardPanel(P, sel);
    S.panelKey = null;
    runPanel();
    return true;
  };
  x.hooks.overlay = value => {
    if (value === "replay") { if (!S.replay) select.value = x.state.overlay; return true; }
    if (value === "live") {
      const run = S.runId || S.liveIds[0];
      if (run) start(run); else select.value = x.state.overlay;
      return true;
    }
    if (S.on) { x.state.overlay = value; stop(false); return true; }
    return false;
  };
  offer.addEventListener("click", () => { if (S.liveIds[0]) start(S.liveIds[0]); });

  // Runs going on now: the picker's "Live run", and an offer to watch.
  async function discover() {
    try {
      const r = await (await fetch(x.baseUrl + "api/runs/live")).json();
      S.liveIds = (r.runs || []).map(run => run.run_id);
    } catch (_) { /* keep what we had */ }
    const any = S.liveIds.length > 0 || !!S.runId;
    liveOption.disabled = !any;
    liveOption.textContent = any ? "Live run" : "Live run (none running)";
    offer.hidden = S.on || !S.liveIds.length;
  }
  liveOption.disabled = true;
  liveOption.textContent = "Live run (none running)";
  discover();
  setInterval(() => { if (!S.on) discover(); }, 3000);

  // The seconds tick: running nodes' time, the header, the chips, and follow.
  setInterval(() => {
    if (!S.on) return;
    for (const id of buildable) if (statusOf(id) === "running") S.dirty.add(id);
    S.chipsKey = null;
    soFar();
    schedule();
  }, 1000);

  // Opened from a live link: `?live=<run_id>`, with `&follow=<node>:down|up|self`; or a
  // replay: `?replay=<run_id>`, with `&t=<seconds>` and `&speed=<n>` (ADR-0026).
  const replayRun = params.get("replay");
  const wantedRun = params.get("live") || replayRun;
  if (wantedRun) {
    let scope = null;
    const f = params.get("follow");
    if (f) {
      const [who, dir] = f.split(":");
      const id = resolve(who);
      if (id && ["down", "up", "self"].includes(dir || "down")) scope = { id, dir: dir || "down" };
    }
    // Set up before the explorer's first paint, so the graph is laid out once.
    S.on = true; S.runId = wantedRun; S.run = L.newRun(wantedRun); S.scope = scope;
    if (replayRun) {
      const at = Number(params.get("t")), speed = Number(params.get("speed"));
      S.replay = { tl: null, p: 0, applied: 0, playing: false, raf: 0, autoplay: false,
        at: isFinite(at) && at > 0 ? at * 1000 : 0, speed: L.SPEEDS.includes(speed) ? speed : 1 };
      document.body.classList.add("lv-replay");
      showReplay(true);
    }
    liveOption.disabled = false;
    liveOption.textContent = "Live run";
    select.value = S.replay ? "replay" : "live";
    x.state.overlay = "live";
    x.state.columns = false;
    document.body.classList.add("lv-on");
    x.setBox(196, 72, 54, 48);
    setTimeout(() => { if (S.on && S.runId === wantedRun && !S.source) { if (S.replay) load(); else connect(); schedule(); } }, 0);
  }
});
