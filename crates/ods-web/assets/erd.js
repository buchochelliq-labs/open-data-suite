// The ERD page (#64): draws the embedded ERD (ods-erd's JSON, schema_version 1) as
// entities with their keys and the relationships between them, laid out by dagre.
// Keys and relationships only: never lineage (AGENTS rule 6). Every edge says how it
// is known, and inferred ones can be hidden (rule 3). Nothing here writes anything.
(function () {
  "use strict";
  const $ = id => document.getElementById(id);
  const dataEl = $("erd-data");
  if (!dataEl) return;
  const data = JSON.parse(dataEl.textContent);
  const erd = data.erd;
  const NS = "http://www.w3.org/2000/svg";
  const W = 240, HEAD = 30, ROW = 20, PAD = 8, GAP = 18;
  const canvas = $("erd-canvas"), svg = $("erd-svg"), viewport = $("erd-viewport"), detail = $("erd-detail");
  for (const el of document.querySelectorAll(".erd-js")) el.classList.add("on");

  // What the toggles say, kept in the URL so a view can be shared.
  const params = new URLSearchParams(location.search);
  const state = {
    inferred: params.get("inferred") !== "0",
    columns: params.get("columns") !== "keys",
    sel: null,
  };
  const inferredBox = $("erd-inferred"), columnsBox = $("erd-columns");
  inferredBox.checked = state.inferred;
  columnsBox.checked = state.columns;
  function remember() {
    const p = new URLSearchParams(location.search);
    if (state.inferred) p.delete("inferred"); else p.set("inferred", "0");
    if (state.columns) p.delete("columns"); else p.set("columns", "keys");
    const q = p.toString();
    try { history.replaceState(null, "", location.pathname + (q ? "?" + q : "")); } catch (_) { /* file:// */ }
  }
  inferredBox.addEventListener("change", () => {
    state.inferred = inferredBox.checked;
    remember();
    // A selected edge that is now hidden isn't shown in the panel either.
    if (state.sel && state.sel.edge != null && !state.inferred && rels[state.sel.edge].basis === "inferred") clear();
    render();
  });
  columnsBox.addEventListener("change", () => { state.columns = columnsBox.checked; remember(); render(); });

  const byId = new Map(erd.entities.map(e => [e.id, e]));
  const nameOf = id => (byId.get(id) || { name: id }).name;
  const rels = erd.relationships.map((r, i) => Object.assign({ i, number: data.numbers[i], proven: !!data.proven[i] }, r));
  const visibleRels = () => rels.filter(r => state.inferred || r.basis !== "inferred");

  // A column is a key column if it is in the primary key, a unique key, or a reference.
  function isKey(e, c) {
    if (c.primary_key || c.foreign_key) return true;
    return (e.unique_keys || []).some(k => k.columns.includes(c.name));
  }
  function columnsOf(e) {
    const shown = state.columns ? e.columns : e.columns.filter(c => isKey(e, c));
    return { shown, hidden: e.columns.length - shown.length };
  }

  // ---------- drawing helpers
  function el(name, attrs, parent) {
    const node = document.createElementNS(NS, name);
    for (const [k, v] of Object.entries(attrs || {})) node.setAttribute(k, v);
    if (parent) parent.appendChild(node);
    return node;
  }
  function txt(parent, x, y, s, cls) {
    const t = el("text", { x, y, class: cls || "" }, parent);
    t.textContent = s;
    return t;
  }

  const BASIS = { tested: "tested", declared: "declared constraint", joined: "joined in SQL", inferred: "inferred from names" };
  // Only what keys prove: the server says which relationships are proven (rule 3).
  function cardinalityText(r) {
    if (!r.proven && r.cardinality !== "unknown") return "Cardinality unproven: the referenced columns only look like a key; no test or constraint says so.";
    if (r.cardinality === "many_to_one") return `Many to ${r.optional ? "zero or one" : "exactly one"}: the referenced columns are a key.`;
    if (r.cardinality === "one_to_one") return "One to one: both sides are keys.";
    return "Cardinality unknown: the referenced columns aren't a tested or declared key.";
  }

  // ---------- layout: dagre, referenced entities to the left of what references them
  let pos = new Map();
  function layout(entities, edges) {
    const g = new dagre.graphlib.Graph({ multigraph: true });
    g.setGraph({ rankdir: "LR", nodesep: 36, ranksep: 110, marginx: 20, marginy: 20 });
    g.setDefaultEdgeLabel(() => ({}));
    const sizes = new Map();
    for (const e of entities) {
      const { shown, hidden } = columnsOf(e);
      const rows = Math.max(1, shown.length) + (hidden > 0 ? 1 : 0);
      const h = HEAD + rows * ROW + PAD;
      sizes.set(e.id, h);
      g.setNode(e.id, { width: W, height: h });
    }
    for (const r of edges) if (r.from !== r.to) g.setEdge(r.to, r.from, {}, "r" + r.i);
    const out = new Map();
    try {
      dagre.layout(g);
      for (const e of entities) { const n = g.node(e.id); out.set(e.id, { x: n.x - W / 2, y: n.y - n.height / 2, h: n.height }); }
    } catch (err) {
      // A grid, so the page still shows every entity.
      console.warn("dagre layout failed; using a grid", err);
      const per = Math.max(1, Math.ceil(Math.sqrt(entities.length)));
      entities.forEach((e, i) => out.set(e.id, { x: 20 + (i % per) * (W + 80), y: 20 + Math.floor(i / per) * 260, h: sizes.get(e.id) }));
    }
    return out;
  }

  // ---------- render
  function render() {
    viewport.replaceChildren();
    const edges = visibleRels();
    pos = layout(erd.entities, edges);
    const gEdges = el("g", { class: "edges" }, viewport);
    const gEntities = el("g", { class: "entities" }, viewport);
    // Edges that cross the same gap between columns each get a lane of their own, so
    // their vertical runs don't overlap.
    const routes = edges.map(route).filter(Boolean);
    // Several relationships of an entity to itself: one loop each, further out.
    const loops = new Map();
    for (const rt of routes) {
      if (rt.r.from !== rt.r.to) continue;
      const n = loops.get(rt.r.from) || 0;
      rt.xm += n * 16;
      loops.set(rt.r.from, n + 1);
    }
    const gaps = new Map();
    for (const rt of routes) {
      if (!rt.between) continue;
      const k = rt.gap;
      if (!gaps.has(k)) gaps.set(k, []);
      gaps.get(k).push(rt);
    }
    for (const lane of gaps.values()) {
      lane.sort((a, b) => (a.y1 + a.y2) - (b.y1 + b.y2));
      lane.forEach((rt, i) => { rt.xm += (i - (lane.length - 1) / 2) * Math.min(12, 80 / lane.length); });
    }
    for (const rt of routes) drawEdge(gEdges, rt);
    for (const e of erd.entities) drawEntity(gEntities, e);
    highlight();
  }

  // The y of a column's row, or of the header when the column isn't shown.
  function rowY(e, column) {
    const p = pos.get(e.id);
    const i = columnsOf(e).shown.findIndex(c => c.name === column);
    return i < 0 ? p.y + HEAD / 2 : p.y + HEAD + i * ROW + ROW / 2;
  }

  function drawEntity(parent, e) {
    const p = pos.get(e.id);
    const { shown, hidden } = columnsOf(e);
    const pk = e.primary_key;
    const label = `${e.name}, ${e.kind}, ${e.columns.length} column${e.columns.length === 1 ? "" : "s"}` +
      (pk ? `, key ${pk.columns.join(" + ")} (${pk.basis})` : ", no known key");
    const g = el("g", { class: `ent kind-${e.kind}`, transform: `translate(${p.x},${p.y})`, tabindex: 0, role: "button", "aria-label": label, "data-entity": e.id }, parent);
    el("rect", { class: "ent-box", width: W, height: p.h, rx: 6 }, g);
    el("rect", { class: "ent-head", width: W, height: HEAD, rx: 6 }, g);
    el("rect", { class: "ent-head-fix", y: HEAD - 6, width: W, height: 6 }, g);
    txt(g, 12, 20, e.name, "ent-name");
    txt(g, W - 10, 19, e.kind, "ent-kind");
    if (e.columns.length === 0) {
      txt(g, 12, HEAD + 14, "columns unknown", "ent-none");
    } else if (shown.length === 0) {
      txt(g, 12, HEAD + 14, `no key columns (${e.columns.length} hidden)`, "ent-none");
    }
    shown.forEach((c, i) => {
      const y = HEAD + i * ROW;
      const row = el("g", { class: "ent-row", transform: `translate(0,${y})` }, g);
      let x = 12;
      if (c.primary_key && pk) {
        el("rect", { class: `badge pk ${pk.basis}`, x, y: 4, width: 22, height: 13, rx: 3 }, row);
        txt(row, x + 11, 14, "PK", "badge-t");
        x += 26;
      }
      if (c.foreign_key) {
        el("rect", { class: "badge fk", x, y: 4, width: 22, height: 13, rx: 3 }, row);
        txt(row, x + 11, 14, "FK", "badge-t");
        x += 26;
      }
      txt(row, Math.max(x, 40), 14, c.name, "col-name");
    });
    if (hidden > 0 && shown.length > 0) txt(g, 40, HEAD + shown.length * ROW + 14, `+${hidden} more`, "ent-none");
    g.addEventListener("click", ev => { ev.stopPropagation(); select({ entity: e.id }); });
    g.addEventListener("keydown", ev => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); select({ entity: e.id }); } });
  }

  // Where an edge runs: from the referencing column to the referenced one, out of the
  // sides that face each other.
  function route(r) {
    const from = byId.get(r.from), to = byId.get(r.to);
    if (!from || !to || !pos.has(r.from) || !pos.has(r.to)) return null;
    const pf = pos.get(r.from), pt = pos.get(r.to);
    const y1 = rowY(from, r.from_columns[0]), y2 = rowY(to, r.to_columns[0]);
    let x1, x2, d1, d2, xm, between = false;
    if (r.from === r.to) {
      x1 = pf.x + W; x2 = pf.x + W; d1 = 1; d2 = 1; xm = pf.x + W + 34;
    } else if (pt.x + W + GAP <= pf.x) {
      x1 = pf.x; d1 = -1; x2 = pt.x + W; d2 = 1; xm = (x1 + x2) / 2; between = true;
    } else if (pf.x + W + GAP <= pt.x) {
      x1 = pf.x + W; d1 = 1; x2 = pt.x; d2 = -1; xm = (x1 + x2) / 2; between = true;
    } else {
      // The same column of the layout: route around the right of both.
      x1 = pf.x + W; x2 = pt.x + W; d1 = 1; d2 = 1; xm = Math.max(x1, x2) + 30;
    }
    return { r, from, to, x1, x2, y1, y2, d1, d2, xm, between, gap: Math.round(Math.min(x1, x2)) + ":" + Math.round(Math.max(x1, x2)) };
  }

  // An orthogonal edge, with the cardinality drawn at each end.
  function drawEdge(parent, rt) {
    const { r, from, to, x1, x2, y1, y2, d1, d2, xm } = rt;
    const sx1 = x1 + d1 * 18, sx2 = x2 + d2 * 18;
    const path = `M${x1},${y1} H${sx1} H${xm} V${y2} H${sx2} H${x2}`;
    const label = `${from.name}.${r.from_columns.join(", ")} references ${to.name}.${r.to_columns.join(", ")}, ${BASIS[r.basis] || r.basis}, ${cardinalityText(r)}` +
      (r.number ? ` Missing relationship ${r.number}.` : "");
    const g = el("g", { class: `edge e-${r.basis}`, tabindex: 0, role: "button", "aria-label": label, "data-edge": r.i }, parent);
    el("path", { class: "edge-hit", d: path }, g);
    el("path", { class: "edge-line", d: path }, g);
    // The referencing end: many, one, or unknown; unproven counts as unknown.
    const card = r.proven ? r.cardinality : "unknown";
    glyph(g, x1, y1, d1, card === "many_to_one" ? "many" : card === "one_to_one" ? "one" : "unknown");
    // The referenced end: a key (exactly one, or zero or one when the reference may be null), or unknown.
    glyph(g, x2, y2, d2, card === "unknown" ? "unknown" : r.optional ? "zero_or_one" : "exactly_one");
    if (r.number) {
      const my = (y1 + y2) / 2;
      el("circle", { class: "edge-num", cx: xm, cy: my, r: 9 }, g);
      txt(g, xm, my + 4, String(r.number), "edge-num-t");
    }
    g.addEventListener("click", ev => { ev.stopPropagation(); select({ edge: r.i }); });
    g.addEventListener("keydown", ev => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); select({ edge: r.i }); } });
  }

  // (x, y) is where the edge meets the entity; d points away from it.
  function glyph(g, x, y, d, kind) {
    const line = (ax, ay, bx, by) => el("line", { class: "glyph", x1: ax, y1: ay, x2: bx, y2: by }, g);
    if (kind === "many") {
      line(x + d * 12, y, x, y - 6); line(x + d * 12, y, x, y); line(x + d * 12, y, x, y + 6);
    } else if (kind === "one") {
      line(x + d * 8, y - 6, x + d * 8, y + 6);
    } else if (kind === "exactly_one") {
      line(x + d * 6, y - 6, x + d * 6, y + 6); line(x + d * 11, y - 6, x + d * 11, y + 6);
    } else if (kind === "zero_or_one") {
      line(x + d * 6, y - 6, x + d * 6, y + 6);
      el("circle", { class: "glyph-o", cx: x + d * 14, cy: y, r: 4 }, g);
    } else {
      el("circle", { class: "glyph-q", cx: x + d * 12, cy: y, r: 6 }, g);
      txt(g, x + d * 12, y + 3.5, "?", "glyph-q-t");
    }
  }

  // ---------- selection: an edge's evidence, or an entity's keys
  function select(sel) {
    state.sel = sel;
    highlight();
    detail.replaceChildren();
    detail.hidden = false;
    const h = (tag, text, cls, parent) => { const n = document.createElement(tag); if (text != null) n.textContent = text; if (cls) n.className = cls; (parent || detail).appendChild(n); return n; };
    const close = h("button", "Close", "btn erd-close");
    close.type = "button";
    close.addEventListener("click", clear);
    if (sel.edge != null) {
      const r = rels[sel.edge];
      h("h2", "Relationship");
      h("p", `${nameOf(r.from)}.${r.from_columns.join(", ")} → ${nameOf(r.to)}.${r.to_columns.join(", ")}`, "mono");
      const basis = h("p");
      h("span", r.basis, `basis ${r.basis}`, basis);
      basis.append(" " + (BASIS[r.basis] || r.basis));
      h("p", cardinalityText(r));
      h("p", r.optional ? "The reference may be null: nothing says it never is." : "The reference is never null.", "muted");
      h("h3", "Evidence");
      const ul = h("ul", null, "erd-evidence");
      for (const ev of r.evidence) h("li", ev, "mono", ul);
      if (r.number) h("p", `Missing relationship ${r.number}: see how to test it below.`, "muted");
    } else {
      const e = byId.get(sel.entity);
      h("h2", e.name);
      h("p", `${e.kind}${e.relation ? " · " + e.relation : ""}`, "muted mono");
      if (e.description) h("p", e.description);
      const pk = e.primary_key;
      h("p", pk ? `Key: ${pk.columns.join(" + ")} (${pk.basis})` : "No known key: no unique and not-null columns are tested or declared.");
      for (const k of e.unique_keys || []) h("p", `Unique: ${k.columns.join(" + ")} (${k.basis})`, "muted");
      const n = visibleRels().filter(r => r.from === e.id || r.to === e.id).length;
      h("p", `${n} relationship${n === 1 ? "" : "s"} shown.`, "muted");
      if (e.kind !== "source") {
        const a = h("a", "Open its model page", null, h("p"));
        a.href = "catalog/" + encodeURIComponent(e.id);
      }
    }
    close.focus({ preventScroll: true });
  }
  function clear() {
    state.sel = null;
    detail.hidden = true;
    detail.replaceChildren();
    highlight();
  }
  function highlight() {
    const sel = state.sel;
    viewport.classList.toggle("dim", !!sel);
    for (const g of viewport.querySelectorAll(".edge")) {
      const r = rels[Number(g.dataset.edge)];
      const on = sel && (sel.edge === r.i || (sel.entity && (r.from === sel.entity || r.to === sel.entity)));
      g.classList.toggle("on", !!on);
    }
    for (const g of viewport.querySelectorAll(".ent")) {
      const id = g.dataset.entity;
      const on = sel && (sel.entity === id || (sel.edge != null && (rels[sel.edge].from === id || rels[sel.edge].to === id)) ||
        (sel.entity && visibleRels().some(r => (r.from === sel.entity && r.to === id) || (r.to === sel.entity && r.from === id))));
      g.classList.toggle("on", !!on);
    }
  }
  window.addEventListener("keydown", e => { if (e.key === "Escape" && state.sel) clear(); });

  // ---------- pan and zoom
  const view = { x: 0, y: 0, k: 1 };
  const apply = () => viewport.setAttribute("transform", `translate(${view.x},${view.y}) scale(${view.k})`);
  function bounds() {
    let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
    for (const p of pos.values()) { x0 = Math.min(x0, p.x); y0 = Math.min(y0, p.y); x1 = Math.max(x1, p.x + W + 40); y1 = Math.max(y1, p.y + p.h); }
    return x0 === Infinity ? null : [x0, y0, x1, y1];
  }
  function fit() {
    const b = bounds(); if (!b) return;
    const r = canvas.getBoundingClientRect();
    const w = Math.max(1, r.width - 32), hh = Math.max(1, r.height - 32);
    view.k = Math.min(1.25, Math.max(0.2, Math.min(w / (b[2] - b[0]), hh / (b[3] - b[1]))));
    view.x = 16 + (w - (b[2] - b[0]) * view.k) / 2 - b[0] * view.k;
    view.y = 16 + Math.max(0, (hh - (b[3] - b[1]) * view.k) / 2) - b[1] * view.k;
    apply();
  }
  $("erd-fit").addEventListener("click", fit);
  let drag = null;
  canvas.addEventListener("mousedown", e => {
    if (e.button !== 0) return;
    drag = { x: e.clientX - view.x, y: e.clientY - view.y, x0: e.clientX, y0: e.clientY, moved: false };
  });
  window.addEventListener("mousemove", e => {
    if (!drag) return;
    if (!drag.moved && Math.abs(e.clientX - drag.x0) + Math.abs(e.clientY - drag.y0) < 4) return;
    drag.moved = true; canvas.classList.add("dragging");
    view.x = e.clientX - drag.x; view.y = e.clientY - drag.y; apply();
  });
  window.addEventListener("mouseup", () => { setTimeout(() => { drag = null; }, 0); canvas.classList.remove("dragging"); });
  canvas.addEventListener("click", () => { if (state.sel && !(drag && drag.moved)) clear(); });
  canvas.addEventListener("wheel", e => {
    e.preventDefault();
    const r = canvas.getBoundingClientRect(), mx = e.clientX - r.left, my = e.clientY - r.top;
    const k = Math.min(3, Math.max(0.1, view.k * Math.exp(-e.deltaY * 0.0015)));
    view.x = mx - (mx - view.x) * (k / view.k); view.y = my - (my - view.y) * (k / view.k); view.k = k; apply();
  }, { passive: false });

  // ---------- export: the diagram as it is drawn now, with its colours resolved
  $("erd-export").addEventListener("click", () => {
    const b = bounds(); if (!b) return;
    const css = getComputedStyle(document.documentElement);
    const v = name => css.getPropertyValue(name).trim();
    const out = svg.cloneNode(true);
    out.setAttribute("xmlns", NS);
    out.setAttribute("viewBox", `${b[0] - 10} ${b[1] - 10} ${b[2] - b[0] + 20} ${b[3] - b[1] + 20}`);
    out.setAttribute("width", Math.round(b[2] - b[0] + 20));
    out.setAttribute("height", Math.round(b[3] - b[1] + 20));
    out.removeAttribute("aria-label");
    out.querySelector("#erd-viewport").removeAttribute("transform");
    const style = document.createElementNS(NS, "style");
    style.textContent = EXPORT_CSS.replace(/var\((--[a-z-]+)\)/g, (_, n) => v(n) || "#888");
    out.insertBefore(style, out.firstChild);
    const blob = new Blob([new XMLSerializer().serializeToString(out)], { type: "image/svg+xml" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = "erd.svg";
    document.body.appendChild(a);
    a.click();
    setTimeout(() => { URL.revokeObjectURL(a.href); a.remove(); }, 0);
  });
  // The drawing's own rules, for the exported file (the page's stylesheet doesn't go).
  const EXPORT_CSS = `
    text { font-family: 'IBM Plex Mono', ui-monospace, monospace; font-size: 12px; fill: var(--fg); }
    .ent-box { fill: var(--surface); stroke: var(--border); }
    .ent-head, .ent-head-fix { fill: var(--erd-head); }
    .ent-name { font-weight: 600; } .ent-kind { font-size: 10px; fill: var(--muted); text-anchor: end; }
    .ent-none { fill: var(--muted); font-size: 11px; }
    .badge.pk.tested { fill: var(--erd-pk-tested); } .badge.pk.declared { fill: var(--erd-pk-declared); }
    .badge.pk.inferred { fill: none; stroke: var(--unknown); stroke-dasharray: 2 2; } .badge.fk { fill: var(--erd-fk); }
    .badge-t { font-size: 9px; font-weight: 700; text-anchor: middle; }
    .edge-hit { fill: none; stroke: none; }
    .edge-line, .glyph { fill: none; stroke-width: 1.5; }
    .e-tested .edge-line, .e-tested .glyph { stroke: var(--erd-tested); }
    .e-declared .edge-line, .e-declared .glyph { stroke: var(--erd-declared); }
    .e-joined .edge-line, .e-joined .glyph { stroke: var(--erd-joined); } .e-joined .edge-line { stroke-dasharray: 6 4; }
    .e-inferred .edge-line, .e-inferred .glyph { stroke: var(--erd-inferred); } .e-inferred .edge-line { stroke-dasharray: 2 3; }
    .glyph-o, .glyph-q { fill: var(--surface); stroke: var(--muted); }
    .glyph-q-t, .edge-num-t { font-size: 10px; text-anchor: middle; }
    .edge-num { fill: var(--surface); stroke: var(--fg); }`;

  render();
  fit();
  window.addEventListener("resize", () => { if (!state.sel) fit(); });

  // ---------- copy buttons for the suggested tests
  for (const b of document.querySelectorAll(".erd-copy")) {
    if (!navigator.clipboard) { b.hidden = true; continue; }
    b.addEventListener("click", () => navigator.clipboard.writeText(b.dataset.copy).then(() => {
      b.textContent = "Copied";
      setTimeout(() => { b.textContent = "Copy"; }, 1500);
    }));
  }
})();
