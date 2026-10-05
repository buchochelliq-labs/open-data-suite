#!/usr/bin/env python3
"""The ERD page in a browser (#64): `ods serve` on the demo project's artifacts, and
Chromium drawing the diagram, toggling inferred edges and columns, showing a
relationship's evidence, scoping to a selection and exporting SVG.

    python3 crates/ods-web/tests/browser/erd.py [-v] [-k PATTERN]

Needs Playwright for Python (scripts/requirements-record.txt), a Chromium it can drive
(`python3 -m playwright install chromium`, or ODS_CHROMIUM), and `ods` (built with
cargo unless ODS_BIN_DIR names its directory). No network: only the local server.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
TARGET = REPO / "fixtures/dbt/jaffle-ods/artifacts/dbt-1.10"
WAIT = 6000


def ods_bin_dir() -> Path:
    if os.environ.get("ODS_BIN_DIR"):
        return Path(os.environ["ODS_BIN_DIR"]).resolve()
    subprocess.run(["cargo", "build", "--quiet", "-p", "ods-cli", "--bin", "ods"], cwd=REPO, check=True)
    meta = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=REPO,
                          check=True, capture_output=True, text=True).stdout
    return Path(json.loads(meta)["target_directory"]) / "debug"


def setUpModule() -> None:
    global SERVER, URL, PW, BROWSER, HOME
    from playwright.sync_api import sync_playwright

    HOME = tempfile.TemporaryDirectory(prefix="ods-erd-")
    env = {"PATH": os.environ.get("PATH", ""), "HOME": HOME.name, "XDG_CONFIG_HOME": HOME.name,
           "TZ": "UTC", "LANG": "C.UTF-8"}
    SERVER = subprocess.Popen([str(ods_bin_dir() / "ods"), "serve", "--target-dir", str(TARGET), "--port", "0",
                               "--no-watch", "-o", "plain"],
                              cwd=HOME.name, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
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
    HOME.cleanup()


class Erd(unittest.TestCase):
    def setUp(self) -> None:
        self.context = BROWSER.new_context(viewport={"width": 1440, "height": 900}, accept_downloads=True)
        self.page = self.context.new_page()
        self.page.set_default_timeout(WAIT)
        self.errors: list[str] = []
        self.page.on("pageerror", lambda e: self.errors.append(str(e)))

    def tearDown(self) -> None:
        self.context.close()
        self.assertEqual(self.errors, [], "no script errors")

    def api(self, query: str = "") -> dict:
        return json.loads(self.page.request.get(f"{URL}/api/erd{query}").text())

    def test_the_diagram_draws_every_entity_and_relationship(self) -> None:
        view = self.api()
        page = self.page
        page.goto(f"{URL}/erd")
        page.wait_for_selector("g.ent")
        self.assertEqual(page.locator("g.ent").count(), len(view["erd"]["entities"]))
        self.assertEqual(page.locator("g.edge").count(), len(view["erd"]["relationships"]))
        # Each missing relationship is numbered on its edge and in the side panel.
        self.assertEqual(page.locator("circle.edge-num").count(), len(view["missing"]))
        self.assertEqual(page.locator(".erd-missing > li").count(), len(view["missing"]))

    def test_inferred_edges_and_columns_can_be_hidden_and_the_url_keeps_it(self) -> None:
        page = self.page
        page.goto(f"{URL}/erd")
        page.wait_for_selector("g.edge.e-inferred")
        rows = page.locator("g.ent-row").count()
        page.get_by_label("Inferred edges").uncheck()
        self.assertEqual(page.locator("g.edge.e-inferred").count(), 0)
        self.assertGreater(page.locator("g.edge.e-tested").count(), 0, "tested edges stay")
        page.get_by_label("All columns").uncheck()
        self.assertLess(page.locator("g.ent-row").count(), rows, "only key columns")
        self.assertIn("inferred=0", page.url)
        self.assertIn("columns=keys", page.url)
        page.reload()
        page.wait_for_selector("g.ent")
        self.assertFalse(page.get_by_label("Inferred edges").is_checked())
        self.assertEqual(page.locator("g.edge.e-inferred").count(), 0)

    def test_a_relationship_shows_its_evidence_from_the_keyboard(self) -> None:
        page = self.page
        page.goto(f"{URL}/erd")
        edge = page.locator("g.edge.e-tested").first
        edge.focus()
        page.keyboard.press("Enter")
        detail = page.locator("#erd-detail")
        self.assertTrue(detail.is_visible())
        self.assertIn("Evidence", detail.text_content())
        self.assertIn("test.", detail.text_content(), "the tests behind it")
        self.assertIn("tested", detail.text_content())
        page.keyboard.press("Escape")
        self.assertFalse(detail.is_visible())
        # An entity says its key and links to its model page.
        entity = page.locator('g.ent[data-entity="model.jaffle_ods.orders"]')
        entity.click()
        self.assertIn("Key: order_id", detail.text_content())
        self.assertEqual(detail.get_by_role("link", name="Open its model page").get_attribute("href"),
                         "catalog/model.jaffle_ods.orders")

    def test_the_scope_is_a_selection_in_the_url(self) -> None:
        page = self.page
        page.goto(f"{URL}/erd")
        page.locator('input[name="select"]').fill("stg_payments")
        page.locator('input[name="depth"]').fill("1")
        page.get_by_role("button", name="Apply").click()
        page.wait_for_url(re.compile(r"select=stg_payments"))
        page.wait_for_selector("g.ent")
        names = sorted(page.locator("g.ent").evaluate_all("els => els.map(e => e.dataset.entity)"))
        self.assertEqual(names, ["model.jaffle_ods.stg_orders", "model.jaffle_ods.stg_payments"])

    def test_export_svg_downloads_the_current_view(self) -> None:
        page = self.page
        page.goto(f"{URL}/erd")
        page.wait_for_selector("g.ent")
        with page.expect_download() as info:
            page.get_by_role("button", name="Export SVG").click()
        download = info.value
        self.assertEqual(download.suggested_filename, "erd.svg")
        svg = Path(download.path()).read_text()
        self.assertTrue(svg.startswith("<svg"), svg[:80])
        self.assertIn("customers", svg)
        self.assertIn("<style>", svg)
        self.assertNotIn("var(--", svg, "colours are resolved for the file")


if __name__ == "__main__":
    unittest.main()
