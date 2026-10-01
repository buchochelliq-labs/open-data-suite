"use strict";
// The dashboard's only script: relative times, the search shortcut, and reloading when
// the server loads a new snapshot. The page is complete without it.
(function () {
  // The dashboard's root, relative to the page's directory (e.g. "../" from
  // state/plan), so it works under any base path.
  const rootMeta = document.querySelector('meta[name="ods-root"]');
  const base = location.pathname.replace(/[^/]*$/, "") + (rootMeta ? rootMeta.content : "");

  // "4 min ago" for every <time data-relative>; the server writes the exact time.
  function ago(iso) {
    const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
    if (!isFinite(s)) return null;
    if (s < 60) return "just now";
    if (s < 3600) return Math.floor(s / 60) + " min ago";
    if (s < 86400) return Math.floor(s / 3600) + " h ago";
    return Math.floor(s / 86400) + " d ago";
  }
  function tick() {
    for (const t of document.querySelectorAll("time[data-relative]")) {
      const text = ago(t.getAttribute("datetime"));
      if (text) t.textContent = text;
    }
  }
  tick();
  setInterval(tick, 30000);

  // "/" focuses search; Enter hands the text to the lineage explorer's search.
  const search = document.getElementById("search");
  if (search) {
    window.addEventListener("keydown", e => {
      if (e.key === "/" && document.activeElement !== search) { e.preventDefault(); search.focus(); }
    });
    search.addEventListener("keydown", e => {
      if (e.key === "Enter" && search.value.trim()) {
        location.href = base + "lineage#q=" + encodeURIComponent(search.value.trim());
      }
    });
  }

  // "Run in progress" on Home (#322): the runs that are probably going on now, kept up
  // to date. The server draws it first; this redraws it the same way.
  const slot = document.getElementById("ods-live");
  if (slot) {
    const el = (tag, cls, text, parent) => {
      const e = document.createElement(tag);
      if (cls) e.className = cls;
      if (text != null) e.textContent = text;
      if (parent) parent.appendChild(e);
      return e;
    };
    // Redrawn only when something changed, so the dot keeps its rhythm.
    let last = [...slot.children].map(c => c.textContent).join("\n");
    const draw = runs => {
      const fresh = document.createDocumentFragment();
      for (const r of runs) {
        const s = el("section", "card live-banner", null, fresh);
        s.dataset.run = r.run_id;
        s.title = r.note;
        el("span", "live-dot", null, s).setAttribute("aria-hidden", "true");
        const t = el("div", "live-text", null, s);
        el("strong", null, "Run in progress", t);
        const m = el("span", "muted", null, t);
        el("code", null, r.command || "a run", m);
        m.append(" · run ");
        el("span", "mono", r.run_id.slice(0, 8), m);
        m.append(` · ${r.finished} of ${r.nodes} nodes finished · ${r.running} running` + (r.failed ? ` · ${r.failed} failed` : "") + " · ");
        el("span", "inferred", "probably running", m);
        const go = el("a", "live-go", "Watch live on the DAG →", s);
        go.href = base + r.href;
        const page = el("a", "live-run", "Run page", s);
        page.href = base + r.run_href;
      }
      const text = [...fresh.children].map(c => c.textContent).join("\n");
      if (text !== last) { last = text; slot.replaceChildren(fresh); }
    };
    setInterval(async () => {
      try { draw((await (await fetch(base + "api/runs/live")).json()).runs || []); } catch (_) { /* keep what is shown */ }
    }, 3000);
  }

  // Reload when the server has a new snapshot (new artifacts or a new run), unless the
  // page holds it (the live run view, which would lose its place).
  const meta = document.querySelector('meta[name="ods-generation"]');
  if (!meta) return;
  const generation = Number(meta.content);
  setInterval(async () => {
    try {
      const v = await (await fetch(base + "api/version")).json();
      if (v.generation === generation) return;
      if (typeof window.odsHoldReload === "function" && window.odsHoldReload()) { window.odsReloadPending = true; return; }
      location.reload();
    } catch (_) { /* the server stopped; keep showing what we have */ }
  }, 2000);
})();
