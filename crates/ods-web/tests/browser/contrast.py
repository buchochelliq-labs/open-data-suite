#!/usr/bin/env python3
"""Contrast on every dashboard screen, light and dark (#353): `ods serve` on the demo
project with the design board's demo run, and Chromium measuring, on each screen,
every piece of visible text against what is painted behind it (WCAG 2.2 AA: 4.5:1,
3:1 for large text) and the status marks and bars against theirs (3:1, non-text
contrast).

    python3 crates/ods-web/tests/browser/contrast.py [-v] [-k PATTERN]

Inactive components (the navigation's *Planned* items, anything disabled) are exempt,
as WCAG exempts them. The background is what the browser paints at the text's centre,
layer by layer, so text on a bar, a pill or a tinted row is measured on that.

Needs Playwright for Python (scripts/requirements-record.txt), a Chromium it can drive
(`python3 -m playwright install chromium`, or ODS_CHROMIUM), bash, and `ods` (built with
cargo unless ODS_BIN_DIR names its directory). No network: only the local server.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO / "scripts"))
import ods_live_sim as sim  # noqa: E402

M = sim.M
SCOPE = "jaffle_ods/default"
WAIT = 8000
WIDTH = 1440

# What WCAG 2.2 AA asks: 4.5:1 for text, 3:1 for large text and for graphics.
MEASURE = r"""
(marks) => {
  // Hit-test everything, so text that ignores the pointer is still found at its point.
  const hit = document.createElement("style");
  hit.textContent = "* { pointer-events: auto !important; }";
  document.head.appendChild(hit);
  const parse = s => {
    const m = s && s.match(/rgba?\(([^)]+)\)/);
    if (!m) return null;
    const p = m[1].split(/[\s,\/]+/).filter(Boolean).map(Number);
    return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 };
  };
  const lin = v => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); };
  const lum = c => 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
  const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
  const over = (top, bottom) => ({
    r: top.r * top.a + bottom.r * (1 - top.a), g: top.g * top.a + bottom.g * (1 - top.a),
    b: top.b * top.a + bottom.b * (1 - top.a), a: 1,
  });
  const opacity = el => {
    let o = 1;
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
      const v = parseFloat(getComputedStyle(e).opacity);
      if (!isNaN(v)) o *= v;
    }
    return o;
  };
  const SHAPES = new Set(["rect", "circle", "ellipse", "path", "polygon"]);
  // The colour an element paints over what is behind it, if any.
  const paint = e => {
    const cs = getComputedStyle(e);
    let c;
    if (e instanceof SVGElement) {
      if (!SHAPES.has(e.tagName)) return null;
      c = parse(cs.fill);
      if (!c) return null;
      c.a *= parseFloat(cs.fillOpacity || "1");
    } else {
      c = parse(cs.backgroundColor);
      if (!c || c.a === 0) return null;
    }
    c.a *= opacity(e);
    return c.a > 0 ? c : null;
  };
  // What is painted at (x, y) under `el` (with `el` itself when `inclusive`).
  const behind = (x, y, el, inclusive) => {
    const stack = document.elementsFromPoint(x, y);
    let i = stack.indexOf(el);
    if (i < 0) {
      for (let a = el.parentElement; a && i < 0; a = a.parentElement) i = stack.indexOf(a);
      if (i < 0) return null; // covered by something else: not visible here
    } else if (!inclusive) i += 1;
    const layers = [];
    for (; i < stack.length; i++) {
      if (stack[i] === el && !inclusive) continue;
      const c = paint(stack[i]);
      if (c) { layers.push(c); if (c.a >= 1) break; }
    }
    let bg = { r: 255, g: 255, b: 255, a: 1 };
    for (let j = layers.length - 1; j >= 0; j--) bg = over(layers[j], bg);
    return bg;
  };
  // Inactive components (planned items and tabs, anything disabled), as WCAG exempts them,
  // and what is never drawn.
  const EXEMPT = ".planned, .tab-planned, [aria-disabled='true'], :disabled, option, title, script, style, noscript, .sr-only, [hidden]";
  const describe = el => {
    const parts = [];
    for (let e = el; e && e.nodeType === 1 && parts.length < 4; e = e.parentElement) {
      let p = e.tagName.toLowerCase();
      if (e.id) p += "#" + e.id;
      else if (typeof e.className === "string" && e.className.trim()) p += "." + e.className.trim().split(/\s+/).slice(0, 2).join(".");
      else if (e.className && e.className.baseVal) p += "." + e.className.baseVal.trim().split(/\s+/).slice(0, 2).join(".");
      parts.unshift(p);
    }
    return parts.join(" > ");
  };
  const hex = c => "#" + [c.r, c.g, c.b].map(v => Math.round(v).toString(16).padStart(2, "0")).join("");
  const failures = [];
  let measured = 0;
  // Text: each run of visible text, at its own centre.
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  const seen = new Set();
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (!n.nodeValue.trim()) continue;
    const el = n.parentElement;
    if (!el || el.closest(EXEMPT)) continue;
    const range = document.createRange();
    range.selectNodeContents(n);
    const rect = [...range.getClientRects()].find(r => r.width > 1 && r.height > 1);
    if (!rect) continue;
    const cs = getComputedStyle(el);
    // Not drawn: hidden, or fully transparent (e.g. a form's invisible default button).
    if (cs.visibility !== "visible" || opacity(el) < 0.05) continue;
    const x = rect.left + rect.width / 2, y = rect.top + rect.height / 2;
    if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) continue;
    const bg = behind(x, y, el, true);
    if (!bg) continue;
    const raw = parse(el instanceof SVGElement ? cs.fill : cs.color);
    if (!raw) continue;
    raw.a *= el instanceof SVGElement ? parseFloat(cs.fillOpacity || "1") : 1;
    raw.a *= opacity(el);
    const fg = over(raw, bg);
    const size = parseFloat(cs.fontSize), bold = parseInt(cs.fontWeight, 10) >= 700;
    const need = size >= 24 || (bold && size >= 18.66) ? 3 : 4.5;
    const r = ratio(fg, bg);
    measured += 1;
    const key = describe(el) + "|" + n.nodeValue.trim().slice(0, 30);
    if (r + 0.005 < need && !seen.has(key)) {
      seen.add(key);
      failures.push({ kind: "text", what: n.nodeValue.trim().slice(0, 40), where: describe(el),
                      fg: hex(fg), bg: hex(bg), ratio: +r.toFixed(2), need });
    }
  }
  // Marks that carry meaning: 3:1 against what they sit on.
  for (const el of document.querySelectorAll(marks)) {
    if (el.closest(EXEMPT)) continue;
    const rect = el.getBoundingClientRect();
    if (rect.width < 1 || rect.height < 1) continue;
    const x = rect.left + rect.width / 2, y = rect.top + rect.height / 2;
    if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) continue;
    const cs = getComputedStyle(el);
    // A mark is seen by its fill or its outline (a hollow dot): the stronger counts.
    const svg = el instanceof SVGElement;
    const paints = [svg ? cs.fill : cs.backgroundColor,
                    svg ? (cs.stroke !== "none" ? cs.stroke : null)
                        : (parseFloat(cs.borderTopWidth) > 0 ? cs.borderTopColor : null)]
      .map(parse).filter(c => c && c.a > 0);
    if (!paints.length) continue;
    const bg = behind(x, y, el, false);
    if (!bg) continue;
    let best = 0, shown = null;
    for (const c of paints) {
      c.a *= opacity(el);
      const r = ratio(over(c, bg), bg);
      if (r > best) { best = r; shown = over(c, bg); }
    }
    measured += 1;
    if (best + 0.005 < 3) {
      failures.push({ kind: "mark", what: describe(el), where: describe(el.parentElement || el),
                      fg: hex(shown), bg: hex(bg), ratio: +best.toFixed(2), need: 3 });
    }
  }
  hit.remove();
  return { measured, failures };
}
"""

# Marks whose colour carries meaning, every kind the stylesheets draw: status, health,
# confidence and grade dots; the live dots; run, timeline, thread and coverage bars;
# glyphs and legend swatches. Chips that say what they mean in words (e.g. `.st-conf`)
# are text, measured as such.
MARKS = ", ".join([
    ".dot:not(.none)", ".hdot", ".cdot", ".gdot", ".th-dot", ".live-dot", ".lv-dot", ".lv-dot-done",
    ".st-glyph", ".st-bar", ".st-sw", ".tl-kind",
    ".tl-bar:not(.unknown):not(.untimed)", ".th-bar:not(.unknown)", ".bar .b", ".bar .r", ".track > span",
])


def ods_bin_dir() -> Path:
    if os.environ.get("ODS_BIN_DIR"):
        return Path(os.environ["ODS_BIN_DIR"]).resolve()
    subprocess.run(["cargo", "build", "--quiet", "-p", "ods-cli", "--bin", "ods"], cwd=REPO, check=True)
    meta = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=REPO,
                          check=True, capture_output=True, text=True).stdout
    return Path(json.loads(meta)["target_directory"]) / "debug"


def setUpModule() -> None:
    global SERVER, URL, PW, BROWSER, SCRATCH, RUN_ID, LIVE_ID, LIVE
    from playwright.sync_api import sync_playwright

    bin_dir = ods_bin_dir()
    SCRATCH = tempfile.TemporaryDirectory(prefix="ods-contrast-")
    root = Path(SCRATCH.name) / "demo"
    env = {"PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}", "REPO": str(REPO), "TZ": "UTC",
           "LANG": "C.UTF-8", "ODS_DEMO_ROOT": str(root)}
    subprocess.run(["bash", "-c", 'source "$REPO/docs/tapes/dashboard/setup.sh" >/dev/null 2>&1'], env=env, check=True)
    env["HOME"] = str(root / "home")
    project = root / "jaffle_shop"
    db = project / ".ods" / "state.db"
    # The board's run, finished; and the same run still going, at 7 s, for the live view.
    RUN_ID = f"7f2f6c69-0000-4000-8000-{uuid.uuid4().hex[:12]}"
    journal = sim.Journal(db, RUN_ID, SCOPE)
    sim.play(journal, until=99, speed=0)
    journal.close()
    LIVE_ID = f"7f2f6c69-0000-4000-8000-{uuid.uuid4().hex[:12]}"
    LIVE = sim.Journal(db, LIVE_ID, SCOPE)
    sim.play(LIVE, until=7.0, speed=0)
    SERVER = subprocess.Popen([str(bin_dir / "ods"), "serve", "--port", "0", "--no-watch", "-o", "plain"],
                              cwd=project, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    for line in SERVER.stdout:
        found = re.search(r"(http://127\.0\.0\.1:\d+)/", line)
        if found:
            URL = found.group(1)
            break
    else:
        raise SystemExit(f"ods serve didn't start: {SERVER.stderr.read()}")
    PW = sync_playwright().start()
    BROWSER = PW.chromium.launch(executable_path=os.environ.get("ODS_CHROMIUM") or None)


def tearDownModule() -> None:
    BROWSER.close()
    PW.stop()
    SERVER.terminate()
    SERVER.wait(timeout=10)
    LIVE.close()
    SCRATCH.cleanup()



def screens() -> list[tuple[str, str, str | None, list[str]]]:
    """(name, path, what to wait for, what to click first) for every screen."""
    model = f"/catalog/{M}customers"
    run = f"/state/runs/{RUN_ID}"
    return [
        ("home", "/", ".live-banner", []),
        ("catalog", "/catalog", None, []),
        ("model-overview", model, None, []),
        ("model-columns", f"{model}?tab=columns", None, []),
        ("model-code", f"{model}?tab=code", None, []),
        ("model-lineage", f"{model}?tab=lineage", None, []),
        ("model-state", f"{model}?tab=state", None, []),
        ("model-tests", f"{model}?tab=tests", None, []),
        ("freshness", "/catalog/sources", None, []),
        ("semantic", "/catalog/semantic", None, []),
        ("lineage", "/lineage", "rect.hit", []),
        ("lineage-why", "/lineage", "rect.hit", ["rect.hit[aria-label^='customers, model']", "[role=tab]:has-text('Why')"]),
        ("impact", "/lineage/impact?column=orders.amount&change-0=drop&add=1", None, []),
        ("live", f"/lineage?live={LIVE_ID}", ".lv-pill", []),
        ("replay", f"/lineage?replay={RUN_ID}&t=7", "body.lv-replay .lv-pill", []),
        ("replay-failed", f"/lineage?replay={RUN_ID}&t=13.3&node={M}customer_segments", "body.lv-replay .lv-pill", []),
        ("plan", "/state/plan", None, []),
        ("plan-why", "/state/plan", None, [f"tr[data-node='{M}customers'] a.st-node"]),
        ("runs", "/state/runs", None, []),
        ("run-timeline", run, None, []),
        ("run-nodes", f"{run}?tab=nodes", None, []),
        ("run-threads", f"{run}?tab=threads", None, []),
        ("erd", "/erd", None, []),
        ("settings", "/settings", None, []),
        ("about", "/settings/about", None, []),
    ]


class Contrast(unittest.TestCase):
    scheme = "light"

    def check(self, name: str, path: str, wait: str | None, clicks: list[str]) -> list[dict]:
        context = BROWSER.new_context(viewport={"width": WIDTH, "height": 900}, color_scheme=self.scheme,
                                      reduced_motion="reduce")
        page = context.new_page()
        page.set_default_timeout(WAIT)
        errors: list[str] = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        try:
            page.goto(URL + path)
            page.wait_for_load_state("networkidle")
            if wait:
                page.wait_for_selector(wait, state="attached")
            for selector in clicks:
                page.locator(selector).first.click()
                page.wait_for_load_state("networkidle")
            page.evaluate("document.fonts.ready.then(() => true)")
            # The whole page in view, so nothing below the fold goes unmeasured.
            height = min(6000, page.evaluate("document.documentElement.scrollHeight"))
            page.set_viewport_size({"width": WIDTH, "height": max(900, height)})
            page.wait_for_timeout(250)
            if os.environ.get("ODS_SCREENSHOTS"):
                page.screenshot(path=str(Path(os.environ["ODS_SCREENSHOTS"]) / f"contrast-{name}-{self.scheme}.png"))
            result = page.evaluate(MEASURE, MARKS)
        finally:
            context.close()
        self.assertEqual(errors, [], f"{name}: script errors")
        self.assertGreater(result["measured"], 20, f"{name}: measured almost nothing; did the page load?")
        return result["failures"]

    def test_every_screen_meets_wcag_aa(self) -> None:
        problems = []
        for name, path, wait, clicks in screens():
            with self.subTest(screen=name):
                failures = self.check(name, path, wait, clicks)
                for f in failures:
                    problems.append(f"{self.scheme} {name}: {f['kind']} {f['what']!r} at {f['where']}: "
                                    f"{f['fg']} on {f['bg']} is {f['ratio']}:1, needs {f['need']}:1")
        self.assertEqual(problems, [], "\n" + "\n".join(problems))


class ContrastDark(Contrast):
    """The same, in dark mode."""

    scheme = "dark"


if __name__ == "__main__":
    unittest.main()
