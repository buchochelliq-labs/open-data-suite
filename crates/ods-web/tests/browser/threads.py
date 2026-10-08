#!/usr/bin/env python3
"""The Run page's Threads tab in a browser (#355): `ods serve` on the demo project, the
design board's demo run (scripts/ods_live_sim.py's BOARD) written to its journal, and
Chromium checking each bar against the journal, the critical path, the idle stretches,
and that a bar opens the replay at its node's start, in light and dark.

    python3 crates/ods-web/tests/browser/threads.py [-v] [-k PATTERN]

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
from urllib.parse import parse_qs, unquote, urlparse

REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO / "scripts"))
import ods_live_sim as sim  # noqa: E402

M = sim.M
SCOPE = "jaffle_ods/default"
WAIT = 6000
# The board's run, as the Threads tab should draw it: each thread's nodes with their
# start and finish (seconds from the run's start), from sim.BOARD.
THREADS = {
    "Thread-1 (worker)": [("stg_orders", 0.4, 2.3), ("orders", 2.4, 6.6), ("customers", 6.7, 9.4),
                          ("customer_segments", 9.5, 13.1)],
    "Thread-2 (worker)": [("customer_order_rank", 6.7, 8.6), ("customers_snapshot_view", 9.5, 10.8)],
}
SPAN = 13.3
# customer_segments finished last; it read customers, which read orders (finished after
# stg_customers, which this run didn't run), which read stg_orders.
CRITICAL = ["stg_orders", "orders", "customers", "customer_segments"]


def ods_bin_dir() -> Path:
    if os.environ.get("ODS_BIN_DIR"):
        return Path(os.environ["ODS_BIN_DIR"]).resolve()
    subprocess.run(["cargo", "build", "--quiet", "-p", "ods-cli", "--bin", "ods"], cwd=REPO, check=True)
    meta = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=REPO,
                          check=True, capture_output=True, text=True).stdout
    return Path(json.loads(meta)["target_directory"]) / "debug"


def setUpModule() -> None:
    global SERVER, URL, PW, BROWSER, SCRATCH, RUN_ID
    from playwright.sync_api import sync_playwright

    bin_dir = ods_bin_dir()
    SCRATCH = tempfile.TemporaryDirectory(prefix="ods-threads-")
    root = Path(SCRATCH.name) / "demo"
    env = {"PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}", "REPO": str(REPO), "TZ": "UTC",
           "LANG": "C.UTF-8", "ODS_DEMO_ROOT": str(root)}
    subprocess.run(["bash", "-c", 'source "$REPO/docs/tapes/dashboard/setup.sh" >/dev/null 2>&1'], env=env, check=True)
    env["HOME"] = str(root / "home")
    project = root / "jaffle_shop"
    # The board's run, written at once, as if it had just finished.
    RUN_ID = f"7f2f6c69-0000-4000-8000-{uuid.uuid4().hex[:12]}"
    journal = sim.Journal(project / ".ods" / "state.db", RUN_ID, SCOPE)
    sim.play(journal, until=99, speed=0)
    journal.close()
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
    SCRATCH.cleanup()


class Threads(unittest.TestCase):
    scheme = "light"

    def setUp(self) -> None:
        self.context = BROWSER.new_context(viewport={"width": 1440, "height": 900}, color_scheme=self.scheme)
        self.page = self.context.new_page()
        self.page.set_default_timeout(WAIT)
        self.errors: list[str] = []
        self.page.on("pageerror", lambda e: self.errors.append(str(e)))

    def tearDown(self) -> None:
        self.context.close()
        self.assertEqual(self.errors, [], "no script errors")

    def open(self) -> None:
        self.page.goto(f"{URL}/state/runs/{RUN_ID}?tab=threads")
        self.page.wait_for_selector(".st-threads")

    def bar(self, name: str):
        return self.page.locator(f'a.th-link[data-node="{M}{name}"] rect.th-bar')

    def test_the_tab_is_reached_from_the_run_page(self) -> None:
        page = self.page
        page.goto(f"{URL}/state/runs/{RUN_ID}")
        page.get_by_role("link", name="Threads").click()
        page.wait_for_url(re.compile(r"\?tab=threads$"))
        self.assertEqual(page.locator('.st-tabs a[aria-current="page"]').text_content(), "Threads")

    def test_each_bar_is_where_the_journal_says(self) -> None:
        self.open()
        page = self.page
        rows = page.locator("g.th-row")
        self.assertEqual([rows.nth(i).get_attribute("data-thread") for i in range(rows.count())], list(THREADS))
        # The time axis: x 200 at the run's start, 776 at its end (the SVG's units).
        x = lambda s: 200 + s / SPAN * 576  # noqa: E731
        for thread, nodes in THREADS.items():
            row = page.locator(f'g.th-row[data-thread="{thread}"]')
            self.assertEqual(row.locator("rect.th-bar").count(), len(nodes), thread)
            for name, start, end in nodes:
                bar = row.locator(f'a.th-link[data-node="{M}{name}"] rect.th-bar')
                left, width = float(bar.get_attribute("x")), float(bar.get_attribute("width"))
                self.assertAlmostEqual(left, x(start), delta=1.0, msg=name)
                self.assertAlmostEqual(left + width, x(end), delta=1.0, msg=name)
        # Bars by outcome; the skipped node never ran, so it has none.
        self.assertIn("failed", self.bar("customer_segments").get_attribute("class"))
        self.assertIn("built", self.bar("orders").get_attribute("class"))
        self.assertEqual(page.locator(f'a.th-link[data-node="{M}segment_summary"]').count(), 0)
        # Idle: thread 2 waited until orders finished, and after its last node.
        idle = page.locator('g.th-row[data-thread="Thread-2 (worker)"] rect.th-gap')
        first = idle.first
        self.assertAlmostEqual(float(first.get_attribute("x")), 200, delta=1.0)
        self.assertAlmostEqual(float(first.get_attribute("width")), x(6.7) - 200, delta=1.0)
        self.assertIn("busy 3.2s", page.locator('g.th-row[data-thread="Thread-2 (worker)"] .th-busy').text_content())

    def test_the_critical_path_is_outlined_and_named(self) -> None:
        self.open()
        page = self.page
        outlined = page.locator("rect.th-bar.critical")
        names = sorted(outlined.nth(i).locator("xpath=..").get_attribute("data-node").removeprefix(M)
                       for i in range(outlined.count()))
        self.assertEqual(names, sorted(CRITICAL))
        path = page.locator(".th-path [data-node]")
        self.assertEqual([path.nth(i).text_content() for i in range(path.count())], CRITICAL)
        # A failed run records no snapshot: its dependencies are the project's now.
        self.assertTrue(page.locator(".th-path .st-grade.inferred").is_visible())
        # Outlined, not only coloured: the outline is drawn, and thicker than none.
        width = self.bar("customers").evaluate("e => parseFloat(getComputedStyle(e).strokeWidth)")
        self.assertGreaterEqual(width, 2)
        self.assertEqual(self.bar("customer_order_rank").evaluate("e => getComputedStyle(e).stroke"), "none")

    def test_a_bar_opens_the_replay_at_its_start_with_the_node_selected(self) -> None:
        self.open()
        page = self.page
        link = page.locator(f'a.th-link[data-node="{M}customers"]')
        query = parse_qs(urlparse(link.get_attribute("href")).query)
        self.assertEqual(query["replay"], [RUN_ID])
        self.assertEqual(float(query["t"][0]), 6.7)
        self.assertEqual(unquote(query["node"][0]), M + "customers")
        link.click()
        page.wait_for_url(re.compile(r"/lineage\?"))
        page.wait_for_selector("body.lv-replay")
        # The run as it was when customers started, with customers selected.
        page.wait_for_function("() => (document.getElementById('lv-ptime')?.textContent || '').startsWith('0:06')")
        pill = page.locator(f'#lin-nodes g[data-node="{M}customers"] .lv-pill text')
        page.wait_for_function("e => e.textContent === 'RUNNING'", arg=pill.element_handle())
        # Selected: the explorer keeps the selection in its URL.
        page.wait_for_function("id => new URLSearchParams(location.search).get('node') === id", arg=M + "customers")

    def test_the_page_fits_and_the_bars_are_legible(self) -> None:
        self.open()
        page = self.page
        self.assertFalse(page.evaluate("document.documentElement.scrollWidth > window.innerWidth"),
                         "no horizontal scroll at 1440px")
        background = page.evaluate("getComputedStyle(document.querySelector('.st-threads')).backgroundColor")
        for name in ("orders", "customer_segments"):
            fill = self.bar(name).evaluate("e => getComputedStyle(e).fill")
            self.assertNotEqual(fill, background, name)
        built = self.bar("orders").evaluate("e => getComputedStyle(e).fill")
        failed = self.bar("customer_segments").evaluate("e => getComputedStyle(e).fill")
        self.assertNotEqual(built, failed)
        if os.environ.get("ODS_SCREENSHOTS"):
            page.screenshot(path=str(Path(os.environ["ODS_SCREENSHOTS"]) / f"threads-{self.scheme}.png"),
                            full_page=True)


class ThreadsDark(Threads):
    """The same, in dark mode."""

    scheme = "dark"


if __name__ == "__main__":
    unittest.main()
