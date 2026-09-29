"use strict";
// The ODS lineage explorer (ADR-0008, ADR-0009, #312). One script for both pages:
// - served by `ods serve` inside the dashboard (meta ods-source "api"): the graph, the
//   State overlay and a deep-linked selection are embedded as JSON by the server, and
//   impact is asked of the API;
// - offline (`ods lineage view`): the graph is embedded, or fetched from graph.json on a
//   static site; there is no overlay and no impact, which need the server.
// It renders a GraphDocument (schema_version 1) with dagre. Model-to-model edges are the
// DAG's (a node reads another), not keys between tables: those belong to the ERD.
(async function () {
  const meta = document.querySelector('meta[name="ods-source"]');
  const source = meta ? meta.content : "embedded";
  const served = source === "api";
  // Relative to the page's directory, so it works under any base path.
  const baseUrl = location.pathname.replace(/[^/]*$/, "");
  const PANEL = document.getElementById("lin-panel");

  function h(tag, text, cls, parent) {
    const e = document.createElement(tag);
    if (text != null) e.textContent = text;
    if (cls) e.className = cls;
    if (parent) parent.appendChild(e);
    return e;
  }
  // Text with `code` spans, as the planner and these messages write commands.
  function rich(tag, text, cls, parent) {
    const e = h(tag, null, cls, parent);
    String(text).split("`").forEach((part, i) => { if (i % 2) h("code", part, null, e); else if (part) e.append(part); });
    return e;
  }
  function embedded(id) {
    const e = document.getElementById(id);
    const text = e ? e.textContent.trim() : "";
    return text && !text.startsWith("/*") ? JSON.parse(text) : null;
  }

  let doc;
  try {
    doc = embedded("ods-graph") || await (await fetch(baseUrl + (served ? "api/graph" : "graph.json"))).json();
  } catch (err) {
    PANEL.replaceChildren(h("p", "Could not load the lineage graph: " + err, "lp-body"));
    return;
  }
  const overlay = served ? embedded("ods-overlay") : null;
  const SVG = "http://www.w3.org/2000/svg";
  const W = 140, HEAD = 52, ROW = 20, FOOT = 6, GAPX = 34, GAPY = 34;
  const byId = new Map(doc.nodes.map(n => [n.id, n]));
  const large = doc.nodes.length > 300;
  const state = {
    indirect: true,
    // The dashboard opens on the model graph, as the design does; the offline page keeps
    // its column view unless the graph is large.
    columns: !served && !large,
    focus: false,
    overlay: overlay ? "state" : "none",
    sel: null, up: new Set(), down: new Set(), edgesUp: new Set(), edgesDown: new Set(),
    tab: "why",
  };
  const $ = id => document.getElementById(id);
  $("lin-columns").checked = state.columns;

  // ---------- the overlay
  const LABEL = { build: "BUILD", reuse: "REUSE", never_built: "NEVER BUILT", unknown: "UNKNOWN" };
  const MEANS = {
    build: "will run",
    reuse: "unchanged, relation checked",
    never_built: "no build recorded",
    unknown: "no evidence: builds",
  };
  const decisionOf = id => (overlay && state.overlay === "state" && overlay.nodes[id]) || null;
  const builds = d => d && d.decision !== "reuse";
  const sentence = s => s ? s[0].toUpperCase() + s.slice(1) : s;
  // Percent-encoded as the server does it (all but A-Z a-z 0-9 - . _ ~), for links.
  const enc = s => Array.from(new TextEncoder().encode(s), b =>
    /[A-Za-z0-9._~-]/.test(String.fromCharCode(b)) ? String.fromCharCode(b) : "%" + b.toString(16).toUpperCase().padStart(2, "0")).join("");

  // ---------- indexes
  const key = (node, col) => col == null ? node + "\u0000" : node + "\u0000" + col;
  const inEdges = new Map(), outEdges = new Map(), nodeIn = new Map(), nodeOut = new Map();
  const push = (m, k, v) => { if (!m.has(k)) m.set(k, []); m.get(k).push(v); };
  doc.column_edges.forEach((e, i) => {
    push(outEdges, key(e.from.node, e.from.column), i);
    push(inEdges, key(e.to.node, e.to.column), i);
  });
  doc.node_edges.forEach(e => { push(nodeOut, e.from, e.to); push(nodeIn, e.to, e.from); });

  // ---------- traversal (conservative: a row-shaping edge affects every column)
  function traceColumn(node, col) {
    const up = new Set(), down = new Set(), edgesUp = new Set(), edgesDown = new Set();
    const walkUp = (n, c) => {
      for (const k of [key(n, c), key(n, null)]) {
        for (const i of inEdges.get(k) || []) {
          const e = doc.column_edges[i];
          if (!state.indirect && e.kind.type === "indirect") continue;
          edgesUp.add(i);
          const kk = key(e.from.node, e.from.column);
          if (!up.has(kk)) { up.add(kk); walkUp(e.from.node, e.from.column); }
        }
      }
    };
    const walkDown = (n, c) => {
      for (const i of outEdges.get(key(n, c)) || []) {
        const e = doc.column_edges[i];
        if (!state.indirect && e.kind.type === "indirect") continue;
        edgesDown.add(i);
        if (e.to.column == null) {
          const node = byId.get(e.to.node);
          for (const col of (node ? node.columns : [])) {
            const kk = key(e.to.node, col);
            if (!down.has(kk)) { down.add(kk); walkDown(e.to.node, col); }
          }
        } else {
          const kk = key(e.to.node, e.to.column);
          if (!down.has(kk)) { down.add(kk); walkDown(e.to.node, e.to.column); }
        }
      }
    };
    walkUp(node, col); walkDown(node, col);
    return { up, down, edgesUp, edgesDown };
  }
  function traceNode(id) {
    const up = new Set(), down = new Set();
    const walk = (m, set, n) => { for (const x of m.get(n) || []) if (!set.has(x)) { set.add(x); walk(m, set, x); } };
    walk(nodeIn, up, id); walk(nodeOut, down, id);
    return { up, down };
  }

  // ---------- layout: dagre (layered, left to right). Nodes and edges go in sorted
  // order so the same graph always gets the same picture.
  let pos = new Map(), routes = new Map();
  const height = id => HEAD + (state.columns && byId.get(id).columns.length ? byId.get(id).columns.length * ROW + FOOT : 0);
  function layout(visible) {
    const ids = [...visible].filter(id => byId.has(id)).sort();
    const edges = doc.node_edges.filter(e => visible.has(e.from) && visible.has(e.to))
      .map(e => [e.from, e.to]).sort((a, b) => a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0);
    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: "LR", ranksep: state.columns ? 90 : GAPX, nodesep: GAPY, marginx: 0, marginy: 0 });
    g.setDefaultEdgeLabel(() => ({}));
    for (const id of ids) g.setNode(id, { width: W, height: height(id) });
    for (const [from, to] of edges) g.setEdge(from, to);
    try {
      dagre.layout(g);
    } catch (e) {
      // dagre recurses, so a very long chain can overflow the stack: lay out by each
      // node's exported layer instead, one column per layer, in id order.
      console.warn("dagre layout failed; using a simple layered layout", e);
      return layered(ids);
    }
    const out = new Map();
    // dagre gives centres; the renderer works from top-left corners.
    for (const id of ids) { const n = g.node(id); out.set(id, { x: n.x - W / 2, y: n.y - n.height / 2, h: n.height }); }
    routes = new Map();
    for (const [from, to] of edges) { const e = g.edge(from, to); if (e && e.points) routes.set(key(from, to), e.points); }
    return out;
  }
  function layered(ids) {
    const out = new Map(), next = new Map();
    for (const id of ids) {
      const layer = byId.get(id).layer || 0, y = next.get(layer) || 0;
      out.set(id, { x: layer * (W + GAPX), y, h: height(id) });
      next.set(layer, y + height(id) + GAPY);
    }
    routes = new Map();
    return out;
  }

  // ---------- rendering
  const el = (tag, attrs, parent) => {
    const e = document.createElementNS(SVG, tag);
    for (const [k, v] of Object.entries(attrs || {})) e.setAttribute(k, v);
    if (parent) parent.appendChild(e);
    return e;
  };
  const gNodes = $("lin-nodes"), gEdges = $("lin-edges");

  function visibleNodes() {
    if (!state.focus || !state.sel) return new Set(doc.nodes.map(n => n.id));
    const keep = new Set([state.sel.node]);
    for (const k of [...state.up, ...state.down]) keep.add(k.split("\u0000")[0]);
    return keep;
  }
  function anchor(node, col, side) {
    const p = pos.get(node), n = byId.get(node);
    if (!p) return null;
    const x = side === "out" ? p.x + W : p.x;
    if (col == null || !state.columns) return { x, y: p.y + HEAD / 2 };
    const i = n.columns.indexOf(col);
    return { x, y: i < 0 ? p.y + HEAD / 2 : p.y + HEAD + i * ROW + ROW / 2 };
  }
  const curve = (a, b) => {
    const dx = Math.max(24, (b.x - a.x) / 2);
    return `M${a.x},${a.y} C${a.x + dx},${a.y} ${b.x - dx},${b.y} ${b.x},${b.y}`;
  };
  // A model edge follows dagre's route (which bends around the nodes in between) from
  // anchor `a` to anchor `b`, smoothed as a Catmull-Rom spline, level at both ends.
  const route = (from, to, a, b) => {
    const bends = routes.get(key(from, to));
    if (!bends || bends.length < 3) return curve(a, b);
    const pts = [a, ...bends.slice(1, -1), b];
    let d = `M${a.x},${a.y}`;
    for (let i = 0; i < pts.length - 1; i++) {
      const p1 = pts[i], p2 = pts[i + 1];
      const p0 = pts[i - 1] || { x: 2 * p1.x - p2.x, y: p2.y }, p3 = pts[i + 2] || { x: 2 * p2.x - p1.x, y: p1.y };
      d += ` C${p1.x + (p2.x - p0.x) / 6},${p1.y + (p2.y - p0.y) / 6} ${p2.x - (p3.x - p1.x) / 6},${p2.y - (p3.y - p1.y) / 6} ${p2.x},${p2.y}`;
    }
    return d;
  };
  // Mono 12px is 7.2px a character; long names end in an ellipsis, as in the design.
  const fit = (text, px) => { const max = Math.floor(px / 7.2); return text.length > max ? text.slice(0, max - 1) + "…" : text; };

  function render() {
    const visible = visibleNodes();
    pos = layout(visible);
    gNodes.replaceChildren(); gEdges.replaceChildren();
    const sel = state.sel;
    const columnSel = sel && sel.column != null;

    if (state.columns) {
      doc.column_edges.forEach((e, i) => {
        if (!state.indirect && e.kind.type === "indirect") return;
        const a = anchor(e.from.node, e.from.column, "out"), b = anchor(e.to.node, e.to.column, "in");
        if (!a || !b) return;
        let cls = "edge " + e.kind.type;
        if (columnSel) {
          cls += state.edgesUp.has(i) ? " up" : state.edgesDown.has(i) ? " down" : " dimmed";
        } else if (sel) {
          const onPath = (e.to.node === sel.node && state.up.has(e.from.node)) || (e.from.node === sel.node && state.down.has(e.to.node))
            || (state.up.has(e.from.node) && state.up.has(e.to.node)) || (state.down.has(e.from.node) && state.down.has(e.to.node));
          if (!onPath) cls += " dimmed";
        }
        const path = el("path", { d: curve(a, b), class: cls }, gEdges);
        el("title", {}, path).textContent =
          `${e.from.node}.${e.from.column} → ${e.to.node}${e.to.column ? "." + e.to.column : " (rows)"}: ${e.kind.type} ${e.kind.subtype}`;
      });
      // A DAG edge no column edge follows (e.g. into a Python model, whose columns
      // aren't known) is still drawn, between the nodes.
      const linked = new Set(doc.column_edges.map(e => key(e.from.node, e.to.node)));
      for (const e of doc.node_edges) {
        if (linked.has(key(e.from, e.to))) continue;
        const a = anchor(e.from, null, "out"), b = anchor(e.to, null, "in");
        if (!a || !b) continue;
        const onPath = sel && ((state.up.has(e.from) || e.from === sel.node) && (state.up.has(e.to) || e.to === sel.node)
          || (state.down.has(e.from) || e.from === sel.node) && (state.down.has(e.to) || e.to === sel.node));
        const path = el("path", { d: curve(a, b), class: "edge model" + (columnSel ? " dimmed" : sel && onPath ? " path" : "") }, gEdges);
        el("title", {}, path).textContent = `${nameOf(e.to)} reads ${nameOf(e.from)}`;
      }
    } else {
      for (const e of doc.node_edges) {
        const a = anchor(e.from, null, "out"), b = anchor(e.to, null, "in");
        if (!a || !b) continue;
        let cls = "edge model";
        if (sel) {
          const on = id => id === sel.node;
          const inUp = (state.up.has(e.from) || on(e.from)) && (state.up.has(e.to) || on(e.to));
          const inDown = (state.down.has(e.from) || on(e.from)) && (state.down.has(e.to) || on(e.to));
          if (inUp || inDown) cls += " path";
        }
        const path = el("path", { d: route(e.from, e.to, a, b), class: cls }, gEdges);
        el("title", {}, path).textContent = `${nameOf(e.to)} reads ${nameOf(e.from)}`;
      }
    }

    for (const id of [...visible].sort()) {
      const n = byId.get(id), p = pos.get(id);
      if (!p) continue;
      const d = decisionOf(id);
      let cls = `node ${n.kind}`;
      if (n.opaque) cls += " opaque";
      if (sel && sel.node === id && sel.column == null) cls += " sel";
      // Readers of the selection that build are outlined, as in the design.
      if (sel && !columnSel && state.down.has(id) && builds(d)) cls += " builds";
      if (columnSel && id !== sel.node && ![...state.up, ...state.down].some(k => k.startsWith(id + "\u0000"))) cls += " dimmed";
      const g = el("g", { class: cls, transform: `translate(${p.x},${p.y})` }, gNodes);
      el("rect", { class: "nbox", width: W, height: p.h, rx: 6 }, g);
      el("rect", { class: `stripe ${n.kind}`, x: 0.5, y: 0.5, width: 4, height: p.h - 1 }, g);
      el("text", { class: "name", x: 12, y: 21 }, g).textContent = fit(n.name, W - 22);
      if (d) {
        const label = LABEL[d.decision] || d.decision.toUpperCase();
        const w = Math.round(label.length * 6.4 + 16);
        el("rect", { class: `pill-bg ${d.decision}`, x: 12, y: 29, width: w, height: 16, rx: 8 }, g);
        el("text", { class: `pill-text ${d.decision}`, x: 20, y: 41 }, g).textContent = label;
      } else {
        el("text", { class: "kindtag", x: 12, y: 41 }, g).textContent = n.kind + (n.opaque ? " · opaque" : "");
      }
      const hit = el("rect", { class: "hit", width: W, height: HEAD, tabindex: 0, role: "button" }, g);
      const tip = [n.name, `${n.kind} · ${n.relation}`];
      if (d) tip.push(`${LABEL[d.decision] || d.decision} · ${d.summary}`);
      if (n.opaque) tip.push((d && d.opaque) || "opaque: column lineage unknown");
      el("title", {}, hit).textContent = tip.join("\n");
      hit.addEventListener("click", ev => { ev.stopPropagation(); select(id, null); });
      hit.addEventListener("keydown", ev => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); select(id, null); } });
      if (!state.columns || !n.columns.length) continue;
      el("line", { class: "rule", x1: 4, x2: W, y1: HEAD, y2: HEAD }, g);
      n.columns.forEach((c, i) => {
        const k = key(id, c);
        let rcls = "row";
        if (sel) {
          if (sel.node === id && sel.column === c) rcls += " sel";
          else if (state.up.has(k)) rcls += " up";
          else if (state.down.has(k)) rcls += " down";
        }
        const row = el("g", { class: rcls, transform: `translate(0,${HEAD + i * ROW})` }, g);
        el("rect", { x: 4, width: W - 4, height: ROW }, row);
        el("text", { x: 14, y: 14 }, row).textContent = fit(c, W - 24);
        row.addEventListener("click", ev => { ev.stopPropagation(); select(id, c); });
      });
    }
    const plan = overlay && state.overlay === "state" && overlay.based_on != null ? `plan against snapshot ${overlay.based_on} · ` : "";
    $("lin-stats").textContent = `${plan}${visible.size} of ${doc.nodes.length} nodes · ${doc.column_edges.length} column edges`;
    legend();
  }

  function legend() {
    const box = $("lin-legend");
    box.replaceChildren();
    if (overlay && state.overlay === "state") {
      // Build and reuse always; the others when some node has them.
      for (const d of ["build", "reuse", "never_built", "unknown"].filter(d => d === "build" || d === "reuse" || overlay.counts[d])) {
        const s = h("span", null, null, box);
        h("span", LABEL[d], "pill " + d, s);
        s.append(MEANS[d]);
      }
    }
    const kinds = new Set(doc.nodes.map(n => n.kind));
    for (const k of ["seed", "source", "snapshot", "model"]) {
      if (!kinds.has(k)) continue;
      const s = h("span", null, null, box);
      h("span", null, "stripe", s).style.background = `var(--kind-${k})`;
      s.append(k);
    }
    if (doc.nodes.some(n => n.opaque)) {
      const s = h("span", null, null, box);
      h("span", null, "box", s);
      s.append("opaque: column lineage unknown");
    }
    if (state.columns) {
      for (const [cls, text] of [["line", "direct"], ["line indirect", "indirect"], ["line up", "upstream"], ["line down", "downstream"]]) {
        const s = h("span", null, null, box);
        h("span", null, cls, s);
        s.append(text);
      }
    }
  }

  // ---------- selection
  function select(node, column, opts) {
    state.sel = { node, column };
    if (column != null && !state.columns) { state.columns = true; $("lin-columns").checked = true; }
    if (column == null) {
      const t = traceNode(node);
      state.up = t.up; state.down = t.down; state.edgesUp = new Set(); state.edgesDown = new Set();
    } else {
      const t = traceColumn(node, column);
      state.up = t.up; state.down = t.down; state.edgesUp = t.edgesUp; state.edgesDown = t.edgesDown;
      state.tab = "columns";
    }
    render();
    panel();
    if (opts && opts.center) center(node);
    remember();
    $("lin-impact") && ($("lin-impact").disabled = false);
  }
  function clearSel() {
    state.sel = null; state.up = new Set(); state.down = new Set(); state.edgesUp = new Set(); state.edgesDown = new Set();
    render();
    panel();
    remember();
  }
  // The selection is in the URL, so a reload (the server reloads open pages when the
  // project or the state changes) or a copied link keeps it: `?node=` when served,
  // `#node=` offline.
  function remember() {
    try {
      const q = new URLSearchParams();
      if (state.sel) { q.set("node", state.sel.node); if (state.sel.column != null) q.set("column", state.sel.column); }
      const text = q.toString();
      history.replaceState(null, "", served ? location.pathname + (text ? "?" + text : "") : location.pathname + location.search + (text ? "#" + text : ""));
    } catch (_) { /* e.g. a sandboxed file:// page */ }
  }
  const nameOf = id => (byId.get(id) || { name: id }).name;
  // A node by id, else by unique name.
  function resolve(wanted) {
    if (byId.has(wanted)) return wanted;
    const named = doc.nodes.filter(n => n.name === wanted);
    return named.length === 1 ? named[0].id : null;
  }

  // ---------- side panel
  function linkItem(ul, label, node, column, note) {
    const li = h("li", null, null, ul);
    const a = h("button", label, "link", li);
    a.type = "button";
    a.addEventListener("click", () => select(node, column, { center: true }));
    if (note) h("span", " " + note, "note", li);
  }
  const CUBE = '<path d="M21 8 12 3 3 8v8l9 5 9-5z"></path><path d="M3 8l9 5 9-5M12 13v8"></path>';
  function icon(kind) {
    const s = document.createElementNS(SVG, "svg");
    for (const [k, v] of Object.entries({ width: 18, height: 18, viewBox: "0 0 24 24", fill: "none", "stroke-width": 1.75, "stroke-linejoin": "round", "aria-hidden": "true" })) s.setAttribute(k, v);
    s.style.stroke = `var(--kind-${kind})`;
    s.innerHTML = CUBE;
    return s;
  }

  function panel(notice) {
    PANEL.replaceChildren();
    if (!state.sel) return emptyPanel(notice);
    const { node, column } = state.sel;
    const n = byId.get(node), d = decisionOf(node);
    const head = h("div", null, "lp-head", PANEL);
    const title = h("div", null, "lp-title", head);
    title.appendChild(icon(n.kind));
    h("span", column != null ? `${n.name}.${column}` : n.name, "nm", title);
    if (served) {
      const open = h("a", "Open", null, title);
      open.href = d ? d.model_href : "catalog/" + enc(node);
      open.title = "The model page";
    }
    const pills = h("div", null, "lp-pills", head);
    if (d) h("span", d.decision === "never_built" ? "NEVER BUILT" : `${LABEL[d.decision]} · ${d.summary}`, "pill decision " + d.decision, pills);
    h("span", n.kind, "pill plain", pills);
    if (n.confidence) h("span", `lineage ${n.confidence}`, "pill plain", pills);
    if (n.opaque) h("span", "opaque", "pill plain dashed", pills);

    const tabs = [];
    if (overlay && state.overlay === "state") tabs.push(["why", "Why"]);
    tabs.push(["general", "General"], ["columns", "Columns"]);
    if (served) tabs.push(["impact", "Impact"]);
    if (!tabs.some(([k]) => k === state.tab)) state.tab = tabs[0][0];
    const bar = h("div", null, "lp-tabs", PANEL);
    bar.setAttribute("role", "tablist");
    for (const [k, label] of tabs) {
      const b = h("button", label, null, bar);
      b.type = "button";
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(k === state.tab));
      b.addEventListener("click", () => { state.tab = k; panel(); });
    }
    const body = h("div", null, "lp-body", PANEL);
    body.setAttribute("role", "tabpanel");
    ({ why, general, columns: columnsTab, impact: impactTab })[state.tab](body, node, column, n, d);

    const foot = h("div", null, "lin-foot", PANEL);
    const done = h("span", "", "done");
    const copy = (label, text) => {
      const b = h("button", label, null, foot);
      b.type = "button";
      b.addEventListener("click", async () => {
        try { await navigator.clipboard.writeText(text()); done.textContent = "Copied"; }
        catch (_) { done.textContent = "Copy isn't allowed here"; }
        setTimeout(() => { done.textContent = ""; }, 2000);
      });
    };
    copy("Copy as JSON", () => JSON.stringify({ node: n, column: column == null ? undefined : column, decision: d || undefined }, null, 2));
    copy("Copy selector", () => `+${n.name}+`);
    foot.appendChild(done);
  }

  function why(body, node, column, n, d) {
    if (!d) {
      h("p", n.kind === "source" ? "Sources are read, never built, so the plan has no decision for them."
        : "The plan has no decision for this node.", "note", body);
      return;
    }
    const ol = h("ol", null, "why", body);
    const item = (strong, sub) => {
      const li = h("li", null, null, ol);
      h("strong", strong, null, li);
      if (sub != null) { li.appendChild(document.createElement("br")); rich("span", sub, "sub", li); }
      return li;
    };
    if (d.last_built) {
      item("Last successful build", `run ${d.last_built.run} · snapshot ${d.last_built.snapshot}` + (d.last_built.target ? ` · target ${d.last_built.target}` : ""));
    } else if (d.decision === "never_built") {
      item("Last successful build", "none recorded");
    }
    for (const r of d.reasons) item(sentence(r.label), r.message);
    if (d.changed_components.length) item("Fingerprint compared", `${d.changed_components.join(", ")} changed`);
    const readers = [...traceNode(node).down].filter(id => builds(decisionOf(id))).sort((a, b) => nameOf(a) < nameOf(b) ? -1 : 1);
    const last = item(`Decision: ${LABEL[d.decision] || d.decision}`, null);
    last.appendChild(document.createElement("br"));
    const sub = h("span", null, "sub", last);
    if (d.decision === "reuse") sub.textContent = "Its last successful build is kept.";
    else if (readers.length) {
      sub.append(readers.length === 1 ? "A reader builds too: " : "Readers build too: ");
      readers.forEach((id, i) => {
        if (i) sub.append(", ");
        const a = h("button", nameOf(id), "link", sub);
        a.type = "button";
        a.addEventListener("click", () => select(id, null, { center: true }));
      });
    } else sub.textContent = "No reader builds.";
    if (d.opaque) rich("p", `${d.opaque}. Any change to what it reads makes it run.`, "warn more", body);
    const more = h("p", null, "more", body);
    const a = h("a", "Full reason chain and evidence in State · Why →", null, more);
    a.href = d.why_href;
    warnings(body);
  }

  function warnings(body) {
    if (!overlay || !overlay.warnings.length) return;
    h("h3", "What qualifies the plan", null, body);
    const ul = h("ul", null, null, body);
    for (const w of overlay.warnings) rich("li", w, "note", ul);
  }

  function general(body, node, column, n) {
    h("p", `${n.kind} · ${n.relation}`, "sub", body);
    if (n.confidence) h("p", `Lineage confidence: ${n.confidence}`, "note", body);
    if (n.opaque) h("p", "Opaque: the SQL could not be analyzed, so every input is assumed to affect every column.", "warn", body);
    for (const note of n.diagnostics || []) h("p", note, "warn", body);
    const t = column == null ? state : traceNode(node);
    h("h3", `Upstream (${t.up.size})`, null, body);
    const up = h("ul", null, null, body);
    for (const id of [...t.up].sort()) linkItem(up, nameOf(id), id, null);
    h("h3", `Downstream (${t.down.size})`, null, body);
    const down = h("ul", null, null, body);
    for (const id of [...t.down].sort()) linkItem(down, nameOf(id), id, null);
  }

  function columnsTab(body, node, column, n) {
    if (column == null) {
      h("h3", `Columns (${n.columns.length})`, null, body);
      if (!n.columns.length) { rich("p", "Unknown: no catalog lists them (`dbt docs generate`).", "note", body); return; }
      h("p", "Pick a column to trace it through the graph.", "note", body);
      const cols = h("ul", null, null, body);
      for (const c of n.columns) linkItem(cols, c, node, c);
      return;
    }
    const direct = [];
    for (const k of [key(node, column), key(node, null)]) for (const i of inEdges.get(k) || []) direct.push(doc.column_edges[i]);
    h("h3", "Inputs", null, body);
    const ul = h("ul", null, null, body);
    if (!direct.length) h("li", "none (a literal, or a source column)", "note", ul);
    for (const e of direct) {
      linkItem(ul, `${nameOf(e.from.node)}.${e.from.column}`, e.from.node, e.from.column,
        `${e.kind.type} ${e.kind.subtype}${e.to.column == null ? " (shapes rows)" : ""}`);
    }
    h("h3", `Upstream columns (${state.up.size})`, null, body);
    const up = h("ul", null, null, body);
    for (const k of [...state.up].sort()) { const [nn, cc] = k.split("\u0000"); linkItem(up, `${nameOf(nn)}.${cc}`, nn, cc); }
    h("h3", `Downstream columns (${state.down.size}): would change`, null, body);
    const down = h("ul", null, null, body);
    for (const k of [...state.down].sort()) { const [nn, cc] = k.split("\u0000"); linkItem(down, `${nameOf(nn)}.${cc}`, nn, cc); }
  }

  // Impact (served only): which models must run if this changes. Nothing is run.
  function impactTab(body, node, column) {
    h("p", column == null ? "What must run if its rows change?" : `What must run if ${nameOf(node)}.${column} changes?`, null, body);
    const row = h("div", null, "lin-impact", body);
    const out = h("div", null, null, body);
    const kinds = column == null ? [["rows", "Its rows change"]] : [["modified", "Modified"], ["removed", "Removed"]];
    for (const [kind, label] of kinds) {
      const b = h("button", label, null, row);
      b.type = "button";
      b.addEventListener("click", async () => {
        const q = new URLSearchParams({ node });
        if (column != null) { q.set("column", column); q.set("kind", kind); }
        out.replaceChildren(h("p", "…", "note"));
        try {
          const r = await (await fetch(baseUrl + "api/impact?" + q)).json();
          out.replaceChildren();
          if (r.error) { h("p", r.error, "warn", out); return; }
          const nodes = Object.keys(r.impact.nodes).sort();
          h("p", `${nodes.length} model(s) must run · ${new Set(r.impact.pruned.map(p => p.node)).size} skipped`, "note", out);
          const ul = h("ul", null, null, out);
          for (const id of nodes) {
            const imp = r.impact.nodes[id];
            linkItem(ul, nameOf(id), id, null, imp.rows_changed ? "all rows may change" : (imp.changed_columns || []).join(", "));
          }
        } catch (err) { out.replaceChildren(h("p", "Impact failed: " + err, "warn")); }
      });
    }
    rich("p", "Computed from column lineage; nothing runs. `ods lineage impact` gives the same answer in a terminal.", "note more", body);
  }

  function emptyPanel(notice) {
    const body = h("div", null, "lp-body", PANEL);
    if (notice) h("p", notice, "warn", body);
    if (overlay && state.overlay === "state") {
      h("h3", "State overlay", null, body);
      const text = {
        no_store: "No state store yet, so no build is recorded: every node shows as never built. Record a first run with `ods state build`.",
        no_runs: "No run is recorded for this project yet: every node shows as never built. Record a first run with `ods state build`.",
        unreadable: "The state store can't be read, so the plan couldn't be made: every node shows as unknown, and would build.",
        project_unreadable: "The project can't be read, so the plan couldn't be made: every node shows as unknown, and would build.",
        recorded: overlay.based_on != null ? `Each node shows what \`ods state plan\` decides against snapshot ${overlay.based_on}.` : "Each node shows what `ods state plan` decides.",
      }[overlay.state] || "";
      rich("p", text, null, body);
      if (overlay.error) rich("p", sentence(overlay.error), "warn", body);
      const counts = h("div", null, "counts", body);
      for (const d of ["build", "never_built", "unknown", "reuse"]) {
        const n = overlay.counts[d];
        if (!n) continue;
        const s = h("span", null, null, counts);
        h("span", LABEL[d], "pill " + d, s);
        s.append(`${n} node${n === 1 ? "" : "s"}`);
      }
      warnings(body);
    }
    h("p", "Click a node to see why it builds or is reused, and a column to trace it upstream and downstream.", "note more", body);
    if (!served) rich("p", "This offline page has the graph only: the State overlay and impact need `ods serve`.", "note", body);
  }

  // ---------- pan and zoom
  const canvas = $("lin-canvas"), viewport = $("lin-viewport");
  const view = { x: 16, y: 16, k: 1 };
  const apply = () => viewport.setAttribute("transform", `translate(${view.x},${view.y}) scale(${view.k})`);
  function fitAll() {
    let maxX = 0, maxY = 0;
    for (const p of pos.values()) { maxX = Math.max(maxX, p.x + W); maxY = Math.max(maxY, p.y + p.h); }
    const r = canvas.getBoundingClientRect();
    // Room for the stats above and the legend below.
    const room = { w: r.width - 18, h: r.height - 40 - 72 };
    view.k = Math.min(1, Math.max(0.05, Math.min(room.w / (maxX || 1), room.h / (maxY || 1))));
    view.x = 16;
    view.y = 40 + Math.max(0, (room.h - maxY * view.k) / 2);
    apply();
  }
  // Brings `node` into view, unless it already is.
  function center(node) {
    const p = pos.get(node); if (!p) return;
    const r = canvas.getBoundingClientRect();
    const sx = view.x + p.x * view.k, sy = view.y + p.y * view.k;
    if (sx >= 0 && sy >= 0 && sx + W * view.k <= r.width && sy + p.h * view.k <= r.height - 72) return;
    view.k = Math.max(view.k, 0.6);
    view.x = r.width / 2 - (p.x + W / 2) * view.k;
    view.y = r.height / 3 - p.y * view.k;
    apply();
  }
  let drag = null;
  canvas.addEventListener("mousedown", e => {
    if (e.target.closest(".lin-legend, .lin-corner")) return;
    drag = { x: e.clientX - view.x, y: e.clientY - view.y, moved: false }; canvas.classList.add("dragging");
  });
  window.addEventListener("mousemove", e => { if (!drag) return; drag.moved = true; view.x = e.clientX - drag.x; view.y = e.clientY - drag.y; apply(); });
  window.addEventListener("mouseup", () => { setTimeout(() => { drag = null; }, 0); canvas.classList.remove("dragging"); });
  canvas.addEventListener("click", e => { if (state.sel && !(drag && drag.moved) && !e.target.closest(".lin-legend, .lin-corner")) clearSel(); });
  canvas.addEventListener("wheel", e => {
    e.preventDefault();
    const r = canvas.getBoundingClientRect(), mx = e.clientX - r.left, my = e.clientY - r.top;
    const k = Math.min(3, Math.max(0.03, view.k * Math.exp(-e.deltaY * 0.0015)));
    view.x = mx - (mx - view.x) * (k / view.k); view.y = my - (my - view.y) * (k / view.k); view.k = k; apply();
  }, { passive: false });

  // ---------- search
  const search = $("lin-search"), results = $("lin-results");
  const index = [];
  for (const n of doc.nodes) {
    index.push({ label: n.name, hay: (n.name + " " + n.id).toLowerCase(), node: n.id, column: null, kind: n.kind });
    for (const c of n.columns) index.push({ label: `${n.name}.${c}`, hay: `${n.name}.${c}`.toLowerCase(), node: n.id, column: c, kind: "column" });
  }
  let active = 0, matches = [];
  function showResults() {
    const q = search.value.trim().toLowerCase();
    results.replaceChildren();
    if (!q) { results.style.display = "none"; return; }
    const terms = q.split(/\s+/);
    matches = index.filter(x => terms.every(t => x.hay.includes(t)))
      .sort((a, b) => (a.hay.startsWith(q) ? 0 : 1) - (b.hay.startsWith(q) ? 0 : 1) || a.label.length - b.label.length)
      .slice(0, 50);
    active = 0;
    matches.forEach((m, i) => {
      const div = h("div", m.label, i === 0 ? "active" : null, results);
      div.setAttribute("role", "option");
      h("span", m.kind, "kind", div);
      div.addEventListener("mousedown", ev => { ev.preventDefault(); choose(m); });
    });
    results.style.display = matches.length ? "block" : "none";
  }
  function choose(m) { results.style.display = "none"; search.blur(); select(m.node, m.column, { center: true }); }
  search.addEventListener("input", showResults);
  search.addEventListener("keydown", e => {
    const items = results.children;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!items.length) return;
      items[active].classList.remove("active");
      active = (active + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length;
      items[active].classList.add("active");
    } else if (e.key === "Enter" && matches[active]) { choose(matches[active]); }
    else if (e.key === "Escape") { search.value = ""; showResults(); search.blur(); }
  });
  search.addEventListener("blur", () => setTimeout(() => { results.style.display = "none"; }, 100));
  window.addEventListener("keydown", e => {
    const typing = /^(INPUT|SELECT|TEXTAREA)$/.test((document.activeElement || {}).tagName || "");
    // The page's own search; served, the shell's is hidden here (lineage.css).
    if (e.key === "/" && !typing) { e.preventDefault(); search.focus(); }
    else if (e.key === "Escape" && !typing && state.sel) clearSel();
  });
  function searchFor(text) { search.value = text; search.focus(); showResults(); }

  // ---------- toggles
  $("lin-indirect").addEventListener("change", e => { state.indirect = e.target.checked; state.sel ? select(state.sel.node, state.sel.column) : render(); });
  $("lin-columns").addEventListener("change", e => {
    state.columns = e.target.checked;
    if (!state.columns && state.sel && state.sel.column != null) select(state.sel.node, null);
    else render();
    fitAll();
  });
  $("lin-focus").addEventListener("change", e => { state.focus = e.target.checked; render(); state.sel ? center(state.sel.node) : fitAll(); });
  $("lin-fit").addEventListener("click", fitAll);
  if ($("lin-overlay")) {
    if (!overlay) $("lin-overlay").disabled = true;
    $("lin-overlay").addEventListener("change", e => { state.overlay = e.target.value; render(); panel(); });
  }
  if ($("lin-impact")) {
    $("lin-impact").addEventListener("click", () => {
      if (!state.sel) { searchFor(search.value); panel("Pick a node or a column first, then simulate what must run if it changes."); return; }
      state.tab = "impact"; panel();
    });
  }

  // ---------- first paint, and deep links: `?node=<id>&column=<name>` (served) or
  // `#node=…&column=…` (offline, and Home's links); `#q=<text>` hands over a search.
  render(); fitAll();
  function follow(params, fallback) {
    const wanted = params.get("node") || fallback;
    if (wanted) {
      const id = resolve(wanted);
      if (!id) { panel(`No node \`${wanted}\` in this graph.`); return; }
      const column = params.get("column");
      const n = byId.get(id);
      select(id, column != null && n.columns.includes(column) ? column : null, { center: true });
      return;
    }
    if (params.get("q")) searchFor(params.get("q"));
    else panel();
  }
  const hash = new URLSearchParams(location.hash.slice(1));
  const query = new URLSearchParams(location.search);
  if (served && (query.get("node") || embedded("ods-selected"))) follow(query, embedded("ods-selected"));
  else follow(hash);
  window.addEventListener("hashchange", () => follow(new URLSearchParams(location.hash.slice(1))));
  if ($("lin-impact") && !state.sel) $("lin-impact").disabled = false;
})();
