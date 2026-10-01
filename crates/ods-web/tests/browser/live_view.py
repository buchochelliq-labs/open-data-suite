#!/usr/bin/env python3
"""The live run view in a browser (#322): `ods serve` on a project, a simulated run
written to its journal a step at a time (scripts/ods_live_sim.py), and Chromium on the
Lineage page, checking what the page does after each step.

    python3 crates/ods-web/tests/browser/live_view.py [-v] [-k PATTERN]

Needs Playwright for Python (scripts/requirements-record.txt), a Chromium it can drive
(`python3 -m playwright install chromium`, or ODS_CHROMIUM), bash, and `ods` (built with
cargo unless ODS_BIN_DIR names its directory). No network: only the local server.

Every step is written, then waited for on the page, so nothing depends on how fast the
machine is; the waits time out after a few seconds.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
from pathlib import Path
from urllib.parse import unquote

REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO / "scripts"))
import ods_live_sim as sim  # noqa: E402

M = sim.M
SCOPE = "jaffle_ods/default"
WAIT = 6000


def ods_bin_dir() -> Path:
    if os.environ.get("ODS_BIN_DIR"):
        return Path(os.environ["ODS_BIN_DIR"])
    subprocess.run(["cargo", "build", "--quiet", "-p", "ods-cli", "--bin", "ods"], cwd=REPO, check=True)
    meta = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=REPO,
                          check=True, capture_output=True, text=True).stdout
    return Path(json.loads(meta)["target_directory"]) / "debug"


def serve(bin_dir: Path, cwd: Path, env: dict, extra: list[str]) -> tuple[subprocess.Popen, str]:
    process = subprocess.Popen([str(bin_dir / "ods"), "serve", "--port", "0", "--no-watch", "-o", "plain", *extra],
                               cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    for line in process.stdout:
        found = re.search(r"(http://127\.0\.0\.1:\d+)/", line)
        if found:
            return process, found.group(1)
    raise SystemExit(f"ods serve didn't start: {process.stderr.read()}")


def large_manifest(target: Path) -> None:
    """Three chains of twelve models (`a_00`…`a_11`, `b_…`, `c_…`): too wide to show at a
    readable size, so running nodes at their two ends can't be framed together."""
    base = json.loads((REPO / "fixtures/dbt/jaffle-ods/artifacts/dbt-1.10/manifest.json").read_text())
    template = base["nodes"]["model.jaffle_ods.stg_customers"]
    nodes, parents, children = {}, {}, {}
    for chain in "abc":
        for i in range(12):
            name = f"{chain}_{i:02d}"
            uid = f"model.big.{name}"
            node = json.loads(json.dumps(template))
            parent = f"model.big.{chain}_{i - 1:02d}" if i else None
            node.update({
                "unique_id": uid, "name": name, "alias": name, "package_name": "big",
                "fqn": ["big", "chains", name], "path": f"chains/{name}.sql",
                "original_file_path": f"models/chains/{name}.sql",
                "relation_name": f'"big"."main"."{name}"', "database": "big", "schema": "main",
                "depends_on": {"macros": [], "nodes": [parent] if parent else []},
                "refs": [{"name": parent.split(".")[-1], "package": None, "version": None}] if parent else [],
                "raw_code": f"select * from {{{{ ref('{parent.split('.')[-1]}') }}}}" if parent else "select 1 as id",
                "compiled_code": f'select * from "big"."main"."{parent.split(".")[-1]}"' if parent else "select 1 as id",
                "columns": {}, "checksum": {"name": "sha256", "checksum": uid},
            })
            nodes[uid] = node
            parents[uid] = [parent] if parent else []
            children.setdefault(uid, [])
            if parent:
                children.setdefault(parent, []).append(uid)
    base["nodes"] = nodes
    base["parent_map"] = parents
    base["child_map"] = children
    base["sources"] = {}
    base["metadata"]["project_name"] = "big"
    target.mkdir(parents=True, exist_ok=True)
    (target / "manifest.json").write_text(json.dumps(base))


def setUpModule() -> None:
    global BIN, DEMO, DEMO_URL, DEMO_DB, BIG, BIG_URL, BIG_DB, BROWSER, PW, SCRATCH
    from playwright.sync_api import sync_playwright

    BIN = ods_bin_dir()
    SCRATCH = Path(tempfile.mkdtemp(prefix="ods-live-view-"))
    # The demo project in a folder of its own (ODS_DEMO_ROOT, else a new one), so this
    # can run beside the recordings.
    root = Path(os.environ.get("ODS_DEMO_ROOT") or SCRATCH / "demo")
    env = {"PATH": f"{BIN}{os.pathsep}{os.environ.get('PATH', '')}", "REPO": str(REPO), "TZ": "UTC",
           "LANG": "C.UTF-8", "ODS_DEMO_ROOT": str(root)}
    subprocess.run(["bash", "-c", 'source "$REPO/docs/tapes/dashboard/setup.sh" >/dev/null 2>&1'], env=env, check=True)
    env["HOME"] = str(root / "home")
    DEMO_DB = root / "jaffle_shop" / ".ods" / "state.db"
    DEMO, DEMO_URL = serve(BIN, root / "jaffle_shop", env, [])
    large_manifest(SCRATCH / "target")
    BIG_DB = SCRATCH / ".ods" / "state.db"
    BIG, BIG_URL = serve(BIN, SCRATCH, {**env, "HOME": str(SCRATCH)},
                         ["--target-dir", str(SCRATCH / "target"), "--state-db", str(BIG_DB)])
    PW = sync_playwright().start()
    BROWSER = PW.chromium.launch(executable_path=os.environ.get("ODS_CHROMIUM") or None)


def tearDownModule() -> None:
    BROWSER.close()
    PW.stop()
    for process in (DEMO, BIG):
        process.terminate()
        process.wait(timeout=10)
    shutil.rmtree(SCRATCH, ignore_errors=True)


class LiveCase(unittest.TestCase):
    """One run and one page per test."""

    url = property(lambda self: DEMO_URL)
    db = property(lambda self: DEMO_DB)
    scope = SCOPE
    reduced = "no-preference"
    run_prefix = "7f2f6c69"

    def setUp(self) -> None:
        self.run_id = f"{self.run_prefix}-{uuid.uuid4().hex[:4]}-4000-8000-{uuid.uuid4().hex[:12]}"
        self.journal = sim.Journal(self.db, self.run_id, self.scope)
        self.context = BROWSER.new_context(viewport={"width": 1440, "height": 900}, reduced_motion=self.reduced,
                                           timezone_id="UTC", locale="en-GB")
        self.context.route(re.compile(r"^(?!" + re.escape(self.url) + r")https?://"), lambda r: r.abort())
        self.page = self.context.new_page()
        self.errors: list[str] = []
        self.page.on("pageerror", lambda e: self.errors.append(str(e)))
        self.t = -1.0

    def tearDown(self) -> None:
        self.context.close()
        self.journal.remove()
        self.assertEqual(self.errors, [], "the page threw")

    # Helpers.
    def open(self, query: str = "") -> None:
        self.page.goto(f"{self.url}/lineage?live={self.run_id}{query}")
        self.page.wait_for_selector("#lin-nodes g[data-node]")

    def play(self, until: float) -> None:
        self.t = sim.play(self.journal, until=until, since=self.t, speed=0)

    def node(self, name: str, prefix: str = M):
        return self.page.locator(f'#lin-nodes g[data-node="{prefix}{name}"]')

    def choose(self, name: str, prefix: str = M) -> None:
        """Selects a node from the keyboard: it may be out of view."""
        self.node(name, prefix).locator(".hit").focus()
        self.page.keyboard.press("Enter")

    def pill(self, name: str, prefix: str = M) -> str:
        return self.node(name, prefix).locator(".lv-pill text").text_content()

    def wait_pill(self, name: str, label: str, prefix: str = M) -> None:
        self.page.wait_for_function(
            "([id, label]) => { const t = document.querySelector(`#lin-nodes g[data-node=\"${id}\"] .lv-pill text`); return t && t.textContent === label; }",
            arg=[prefix + name, label], timeout=WAIT)

    def wait(self, js: str, arg=None) -> None:
        self.page.wait_for_function(js, arg=arg, timeout=WAIT)

    def text(self, selector: str) -> str:
        return " ".join((self.page.locator(selector).first.inner_text() or "").split())

    def view(self) -> tuple[float, float, float]:
        t = self.page.get_attribute("#lin-viewport", "transform")
        x, y, k = map(float, re.match(r"translate\(([-\d.e]+),([-\d.e]+)\) scale\(([-\d.e]+)\)", t).groups())
        return x, y, k

    def in_view(self, name: str, prefix: str = M) -> bool:
        box = self.node(name, prefix).locator("rect.nbox").bounding_box()
        canvas = self.page.locator("#lin-canvas").bounding_box()
        return (box["x"] >= canvas["x"] - 1 and box["y"] >= canvas["y"] - 1
                and box["x"] + box["width"] <= canvas["x"] + canvas["width"] + 1
                and box["y"] + box["height"] <= canvas["y"] + canvas["height"] + 1)

    def settle(self) -> None:
        """Past one follow move (at most one a second) and its 700 ms animation."""
        self.page.wait_for_timeout(1900)

    def following(self) -> bool:
        return self.page.get_attribute("#lv-follow", "aria-pressed") == "true"


class SimulatedRun(LiveCase):
    def test_each_event_is_reflected_in_order_with_stats(self) -> None:
        self.play(0.0)
        self.open()
        self.wait_pill("stg_orders", "QUEUED")
        # Nodes the run doesn't touch keep their last build.
        self.assertEqual(self.pill("stg_customers"), "KEPT")
        expected = {"node_started": "RUNNING", "success": "BUILT", "error": "FAILED", "skipped": "SKIPPED"}
        for t, action, args in sim.BOARD[1:-1]:
            self.play(t)
            name = args["node"].removeprefix(M)
            label = expected[args.get("status", action)]
            self.wait_pill(name, label)
            # The event log's newest line is this event.
            word = {"RUNNING": "started", "BUILT": "built", "FAILED": "failed", "SKIPPED": "skipped"}[label]
            # Among the newest lines: two nodes can start at the same time.
            self.wait("([name, word]) => [...document.querySelectorAll('.lv-log li span:last-child')].slice(0, 2).some(e => e.textContent.startsWith(name + ' ' + word))",
                      [name, word])
            node_meta = self.node(name).locator(".lv-meta").text_content()
            if label == "BUILT":
                rows = args.get("rows")
                self.assertIn(f"{rows} rows" if rows is not None else "rows —", node_meta)
            if label == "RUNNING":
                self.assertIn(f"thread {args['thread']}", node_meta)
        self.play(99)
        self.wait("() => !document.getElementById('lv-toast').hidden")
        panel = self.text("#lin-panel")
        self.assertIn("Finished with 1 failure", panel)
        # 99 + 99 + 100 reported; stg_orders and customers_snapshot_view (views) didn't.
        self.assertIn("Rows affected at least 298", panel)
        self.assertIn("5 BUILT", panel.replace("\n", " "))
        toast = self.text("#lv-toast")
        self.assertIn("Run finished in", toast)
        self.assertIn("5 built · 1 failed · 1 skipped", toast)

    def test_node_stats_are_the_run_pages_and_missing_is_never_zero(self) -> None:
        self.play(10.9)
        self.open()
        self.wait_pill("customers_snapshot_view", "BUILT")
        self.choose("orders")
        self.wait("() => document.querySelector('#lv-card .lv-stats')")
        card = self.text("#lv-card")
        self.assertIn("Rows affected 99 from the adapter response", card)
        self.assertIn("Adapter query_id 01b2-c3", card)
        self.assertIn("Relation", card)
        # The run goes on: its tests come after it.
        self.assertIn("run after this node", self.text('#lv-card [data-stat="tests"]'))
        # A view that reported no rows: a dash with the reason, never 0.
        self.choose("stg_orders")
        self.wait("() => document.querySelector('.lp-title .nm').textContent === 'stg_orders' && document.querySelector('#lv-card .lv-stats')")
        rows = self.text('#lv-card [data-stat="rows_affected"]')
        self.assertIn("—", rows)
        self.assertIn("not reported by the adapter", rows)
        self.assertNotRegex(rows, r"\b0\b")
        # A running node counts its time.
        self.choose("customer_segments")
        self.wait("() => document.querySelector('.lp-title .nm').textContent === 'customer_segments' && document.querySelector('#lv-card .lv-so-far')")
        self.assertIn("so far", self.text('#lv-card [data-stat="time_taken"]'))
        self.assertIn("still running", self.text('#lv-card [data-stat="ended"]'))
        # When it fails, the card says why, as the Run page does, without values.
        self.play(13.1)
        self.wait("() => document.querySelector('#lv-card .lv-card[data-status=error]')")
        self.wait("() => document.querySelector('#lv-card .st-explain, #lv-card .st-error')")
        # dbt's own message is one click away, redacted.
        card = self.page.text_content("#lv-card")
        self.assertIn("KeyError", card)
        self.assertIn("[value removed]", card)
        # It built nothing: neither rows nor tests are "not reported".
        self.assertIn("the node didn't build", self.text('#lv-card [data-stat="rows_affected"]'))
        self.assertIn("not run the node did not build", self.text('#lv-card [data-stat="tests"]'))
        # A Python model, and how it materializes, as the board has them.
        pills = self.text("#lv-card .lv-card-pills")
        self.assertIn("python model", pills)
        self.assertIn("table", pills)
        # Its state alone draws its box: solid red, not the "opaque" dashes.
        dash = self.node("customer_segments").locator("rect.nbox").evaluate("e => getComputedStyle(e).strokeDasharray")
        self.assertEqual(dash, "none")

    def test_follow_frames_the_running_nodes(self) -> None:
        self.play(6.7)
        self.open()
        self.wait_pill("customer_order_rank", "RUNNING")
        self.settle()
        self.assertTrue(self.following())
        self.assertTrue(self.in_view("customers"))
        self.assertTrue(self.in_view("customer_order_rank"))
        # Then the next pair.
        self.play(9.5)
        self.wait_pill("customers_snapshot_view", "RUNNING")
        self.settle()
        self.assertTrue(self.in_view("customer_segments"))
        self.assertTrue(self.in_view("customers_snapshot_view"))
        # Never below the readable minimum.
        self.assertGreaterEqual(self.view()[2], 0.6 - 1e-6)

    def test_manual_pan_turns_follow_off_and_f_turns_it_back_on(self) -> None:
        self.play(2.4)
        self.open()
        self.wait_pill("orders", "RUNNING")
        self.settle()
        self.assertTrue(self.following())
        canvas = self.page.locator("#lin-canvas").bounding_box()
        self.page.mouse.move(canvas["x"] + 200, canvas["y"] + 120)
        self.page.mouse.down()
        self.page.mouse.move(canvas["x"] + 420, canvas["y"] + 260, steps=6)
        self.page.mouse.up()
        self.assertFalse(self.following())
        self.wait("() => !document.getElementById('lv-hint').hidden")
        self.assertIn("Follow is off: you moved the view.", self.text("#lv-hint"))
        moved = self.view()
        # Events no longer move the camera.
        self.play(6.7)
        self.wait_pill("customers", "RUNNING")
        self.settle()
        self.assertEqual(self.view(), moved)
        # F turns it back on, and it frames what runs now.
        self.page.locator("#lin-canvas").click(position={"x": 5, "y": 5})
        self.page.keyboard.press("f")
        self.assertTrue(self.following())
        self.wait("() => document.getElementById('lv-hint').hidden")
        self.settle()
        self.assertTrue(self.in_view("customers"))
        self.assertTrue(self.in_view("customer_order_rank"))
        # A zoom, Fit, or a node click turns it off too; the toggle turns it on.
        # A zoom, Fit, or a node click turns it off too, and the hint says which; the
        # toggle turns it on.
        for act, why in ((lambda: self.page.click('button[aria-label="Zoom in"]'), "you zoomed"),
                         (lambda: self.page.click("#lin-fit"), "you used Fit"),
                         (lambda: self.choose("orders"), "you selected a node")):
            act()
            self.assertFalse(self.following())
            self.assertIn(why, self.text("#lv-hint"))
            self.page.click("#lv-follow")
            self.assertTrue(self.following())

    def test_the_node_menu_works_from_the_keyboard(self) -> None:
        self.play(2.4)
        self.open()
        self.wait_pill("orders", "RUNNING")
        hit = self.node("customers").locator(".hit")
        # Shift+F10 on a focused node opens it, on its first item.
        hit.focus()
        self.page.keyboard.press("Shift+F10")
        self.wait("() => !document.getElementById('lv-menu').hidden")
        self.assertEqual(self.page.evaluate("document.activeElement.getAttribute('role')"), "menuitem")
        self.assertIn("Follow this node and its downstream", self.page.evaluate("document.activeElement.textContent"))
        self.assertIn("3 downstream nodes", self.text("#lv-menu"))
        self.page.keyboard.press("Enter")
        self.wait("() => !document.getElementById('lv-scope').hidden")
        self.assertEqual(self.text("#lv-scope span"), "Following customers + 3 downstream")
        self.assertIn("follow=customers%3Adown", self.page.url)
        # The menu key, then the arrows: upstream.
        hit.focus()
        self.page.keyboard.press("ContextMenu")
        self.wait("() => !document.getElementById('lv-menu').hidden")
        self.page.keyboard.press("ArrowDown")
        self.assertIn("upstream", self.page.evaluate("document.activeElement.textContent"))
        self.page.keyboard.press("Enter")
        self.wait("() => document.querySelector('#lv-scope span').textContent.includes('upstream')")
        # Just this node.
        hit.focus()
        self.page.keyboard.press("Shift+F10")
        self.page.keyboard.press("ArrowDown")
        self.page.keyboard.press("ArrowDown")
        self.page.keyboard.press("Enter")
        self.wait("() => document.querySelector('#lv-scope span').textContent === 'Following customers'")
        # Show node stats (End goes to the last item, Close; one up is the stats).
        hit.focus()
        self.page.keyboard.press("Shift+F10")
        self.page.keyboard.press("End")
        self.page.keyboard.press("ArrowUp")
        self.assertIn("Show node stats", self.page.evaluate("document.activeElement.textContent"))
        self.page.keyboard.press("Enter")
        self.wait("() => document.querySelector('.lp-title .nm') && document.querySelector('.lp-title .nm').textContent === 'customers'")
        # Close, and Escape, give the focus back to the node.
        hit.focus()
        self.page.keyboard.press("Shift+F10")
        self.page.keyboard.press("End")
        self.page.keyboard.press("Enter")
        self.wait("() => document.getElementById('lv-menu').hidden")
        self.assertEqual(self.page.evaluate("document.activeElement.closest('g[data-node]').dataset.node"), M + "customers")
        hit.focus()
        self.page.keyboard.press("Shift+F10")
        self.page.keyboard.press("Escape")
        self.wait("() => document.getElementById('lv-menu').hidden")
        self.assertEqual(self.page.evaluate("document.activeElement.closest('g[data-node]').dataset.node"), M + "customers")
        # The stats card's button does the same as the first item.
        self.page.click(".lv-follow-down")
        self.wait("() => document.querySelector('#lv-scope span').textContent === 'Following customers + 3 downstream'")
        # And right-click opens the menu too.
        self.node("orders").locator(".hit").click(button="right")
        self.wait("() => !document.getElementById('lv-menu').hidden")
        self.assertIn("orders", self.text("#lv-menu .lv-menu-id"))

    def test_a_downstream_scope_ignores_running_nodes_outside_it(self) -> None:
        self.play(2.4)
        self.open("&follow=customers:down")
        self.wait_pill("orders", "RUNNING")
        self.assertEqual(self.text("#lv-scope span"), "Following customers + 3 downstream")
        # Outside the scope: faded, and it never moves the camera.
        self.assertIn("lv-faded", self.node("orders").get_attribute("class"))
        self.assertNotIn("lv-faded", self.node("customers").get_attribute("class"))
        self.settle()
        before = self.view()
        self.play(2.5)
        self.page.wait_for_timeout(1500)
        self.assertEqual(self.view(), before, "orders, outside the scope, moved the camera")
        # customers (in scope) and customer_order_rank (outside) start: only customers is framed.
        self.play(6.7)
        self.wait_pill("customers", "RUNNING")
        self.settle()
        self.assertTrue(self.in_view("customers"))
        self.assertAlmostEqual(self.view()[2], 1.15, places=2)
        # Clearing the scope follows the whole run again.
        self.page.click("#lv-scope button")
        self.assertTrue(self.page.locator("#lv-scope").is_hidden())
        self.assertNotIn("follow=", self.page.url)
        self.settle()
        self.assertTrue(self.in_view("customers"))
        self.assertTrue(self.in_view("customer_order_rank"))

    def test_when_the_scope_finishes_it_is_framed_and_the_whole_run_offered(self) -> None:
        self.play(9.5)
        self.open("&follow=customers_snapshot_view:self")
        self.wait_pill("customers_snapshot_view", "RUNNING")
        self.play(10.8)
        self.wait("() => !document.getElementById('lv-toast').hidden")
        self.assertIn("customers_snapshot_view finished", self.text("#lv-toast"))
        self.page.click("#lv-toast button")
        self.assertTrue(self.page.locator("#lv-scope").is_hidden())

    def test_the_end_stops_follow_frames_the_failure_and_offers_it(self) -> None:
        self.play(99)
        self.open("&follow=customers:down")
        self.wait("() => !document.getElementById('lv-toast').hidden")
        self.assertFalse(self.following())
        self.assertTrue(self.page.locator("#lv-hint").is_hidden())
        # The final view has the failure in it.
        self.settle()
        self.assertTrue(self.in_view("customer_segments"))
        self.assertTrue(self.in_view("segment_summary"))
        # Nothing is followed any more: the scope only fades the rest.
        self.assertEqual(self.text("#lv-scope span"), "Scope: customers + 3 downstream")
        self.page.click(".lv-jump")
        self.wait("() => document.querySelector('.lp-title .nm') && document.querySelector('.lp-title .nm').textContent === 'customer_segments'")
        self.settle()
        self.assertTrue(self.in_view("customer_segments"))
        self.assertTrue(self.in_view("segment_summary"))
        # Selected and in view: the offer is gone.
        self.assertEqual(self.page.locator(".lv-jump").count(), 0)

    def test_a_link_opened_before_the_journal_exists_waits_for_it(self) -> None:
        later = f"7f2f6c69-0000-4000-8000-{uuid.uuid4().hex[:12]}"
        self.page.goto(f"{self.url}/lineage?live={later}")
        self.page.wait_for_selector("#lin-nodes g[data-node]")
        self.wait("() => /Waiting for run/.test(document.querySelector('.lin-status').textContent)")
        journal = sim.Journal(self.db, later, self.scope)
        try:
            sim.play(journal, until=2.4, speed=0)
            self.wait_pill("orders", "RUNNING")
            self.assertIn("Live", self.text(".lin-status"))
        finally:
            journal.remove()

    def test_polling_works_without_event_source(self) -> None:
        self.play(2.3)
        self.open("&poll=1")
        self.wait_pill("stg_orders", "BUILT")
        self.play(6.7)
        self.wait_pill("customers", "RUNNING")
        self.play(99)
        self.wait("() => !document.getElementById('lv-toast').hidden")

    def test_starts_and_finishes_are_announced(self) -> None:
        self.play(0.0)
        self.open()
        self.play(2.4)
        self.wait("() => /orders started/.test(document.getElementById('lv-say').textContent)")
        self.assertIn("stg_orders built", self.page.text_content("#lv-say"))

    def test_the_journal_is_shown_without_what_it_mustnt_hold(self) -> None:
        self.play(9.5)
        error = {"kind": "KeyError", "message": "KeyError: 'SENTINEL-VALUE' where token = 'SENTINEL-SECRET'"}
        self.journal.now = self.journal.t0 + sim.timedelta(seconds=13.1)
        self.journal.node_finished(M + "customer_segments", "error", error=error, thread=1,
                                   adapter={"note": "select 'SENTINEL-SQL' from x"})
        self.open()
        self.wait_pill("customer_segments", "FAILED")
        self.choose("customer_segments")
        self.wait("() => document.querySelector('#lv-card .lv-stats')")
        self.assertNotIn("SENTINEL", self.page.content())

    def test_home_and_the_run_page_link_to_the_live_view(self) -> None:
        self.play(2.4)
        self.page.goto(self.url + "/")
        self.wait("() => document.querySelector('.live-banner')")
        banner = self.text(".live-banner")
        self.assertIn("Run probably in progress", banner)
        self.assertIn("inferred", banner)
        self.assertIn("1 probably running", banner)
        self.page.click(".live-banner .live-go")
        self.page.wait_for_selector("#lin-nodes g[data-node]")
        self.assertIn(f"live={self.run_id}", self.page.url)
        self.wait_pill("orders", "RUNNING")
        self.page.goto(f"{self.url}/state/runs/{self.run_id}")
        self.page.click("a.st-live")
        self.page.wait_for_selector("#lin-nodes g[data-node]")
        self.assertIn(f"live={self.run_id}", unquote(self.page.url))


class ReducedMotion(LiveCase):
    reduced = "reduce"

    def test_the_camera_jumps(self) -> None:
        self.play(2.4)
        self.open()
        self.wait_pill("orders", "RUNNING")
        self.settle()
        before = self.view()
        self.play(6.7)
        self.wait_pill("customers", "RUNNING")
        # It moves once the second has passed, in one step: no frame in between.
        seen = set()
        for _ in range(30):
            seen.add(self.view())
            self.page.wait_for_timeout(50)
        seen.discard(before)
        self.assertEqual(len(seen), 1, f"the camera animated: {seen}")


class Animated(LiveCase):
    def test_the_camera_glides(self) -> None:
        self.play(2.4)
        self.open()
        self.wait_pill("orders", "RUNNING")
        self.settle()
        before = self.view()
        self.play(6.7)
        self.wait_pill("customers", "RUNNING")
        seen = set()
        for _ in range(40):
            seen.add(self.view())
            self.page.wait_for_timeout(40)
        seen.discard(before)
        self.assertGreater(len(seen), 2, "the camera jumped with motion allowed")


class SpreadOut(LiveCase):
    """Three chains of twelve: running nodes at their far ends can't be framed at 60%."""

    url = property(lambda self: BIG_URL)
    db = property(lambda self: BIG_DB)
    scope = "big/default"
    run_prefix = "3c91e0aa"

    def setUp(self) -> None:
        super().setUp()
        # Each event a second after the last, in the last two minutes; each running node on a
        # thread of its own.
        self.step = 0
        start = sim.datetime.now(sim.timezone.utc) - sim.timedelta(seconds=120)

        def clock():
            self.step += 1
            return start + sim.timedelta(seconds=self.step)
        self.journal.clock = clock
        self.threads: dict[str, int] = {}

    def big(self, *steps) -> None:
        for kind, name in steps:
            node = f"model.big.{name}"
            if kind == "start":
                busy = set(self.threads.values())
                thread = next(t for t in range(1, 99) if t not in busy)
                self.threads[name] = thread
                self.journal.node_started(node, thread)
            else:
                self.journal.node_finished(node, "success", thread=self.threads.pop(name, None))

    def test_follow_keeps_one_focus_shows_chips_and_moves_only_when_it_finishes(self) -> None:
        nodes = [f"model.big.{c}_{i:02d}" for c in "abc" for i in range(12)]
        self.journal.started(nodes)
        self.big(("start", "a_00"), ("finish", "a_00"), ("start", "a_01"), ("start", "b_09"), ("start", "c_10"))
        self.open()
        self.wait_pill("c_10", "RUNNING", "model.big.")
        self.settle()
        # a_01 blocks the ten queued nodes after it; b_09 two, c_10 one.
        self.wait("() => document.getElementById('lv-focus').textContent === 'Focus: a_01 · blocks 10 queued nodes'")
        self.assertTrue(self.in_view("a_01", "model.big."))
        self.assertGreaterEqual(self.view()[2], 0.6 - 1e-6)
        # A chip for each running node off screen, on its edge.
        self.wait("() => document.querySelectorAll('.lv-chip').length === 2")
        chips = self.page.eval_on_selector_all(".lv-chip", "cs => cs.map(c => [c.dataset.node, c.parentElement.id])")
        self.assertEqual(sorted(chips), [["model.big.b_09", "lv-chips-right"], ["model.big.c_10", "lv-chips-right"]])
        # A node that blocks more starts: the focus stays until a_01 finishes.
        self.big(("start", "b_00"))
        self.wait_pill("b_00", "RUNNING", "model.big.")
        self.settle()
        self.assertIn("Focus: a_01", self.text("#lv-focus"))
        self.assertTrue(self.in_view("a_01", "model.big."))
        self.big(("finish", "a_01"))
        self.wait("() => document.getElementById('lv-focus').textContent.startsWith('Focus: b_00')")
        self.settle()
        self.assertTrue(self.in_view("b_00", "model.big."))
        # A chip's node becomes the focus, and follow stays on.
        self.page.click('.lv-chip[data-node="model.big.c_10"]')
        self.assertTrue(self.following())
        self.settle()
        self.assertTrue(self.in_view("c_10", "model.big."))
        self.wait("() => document.getElementById('lv-focus').textContent.startsWith('Focus: c_10')")
        # The minimap moves the view, and turns follow off.
        self.page.locator("#lv-mini canvas").click(position={"x": 30, "y": 60})
        self.assertFalse(self.following())
        self.assertTrue(self.page.locator("#lv-mini").is_visible())


if __name__ == "__main__":
    unittest.main()
