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

  // Reload when the server has a new snapshot (new artifacts or a new run).
  const meta = document.querySelector('meta[name="ods-generation"]');
  if (!meta) return;
  const generation = Number(meta.content);
  setInterval(async () => {
    try {
      const v = await (await fetch(base + "api/version")).json();
      if (v.generation !== generation) location.reload();
    } catch (_) { /* the server stopped; keep showing what we have */ }
  }, 2000);
})();
