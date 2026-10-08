#!/usr/bin/env python3
"""The Semantic layer page in a browser (#352): `ods serve` on a project that declares
semantic models and metrics (fixtures/dbt/jaffle-metrics) and on one that declares
neither (the demo project), and Chromium reaching the page from the Catalog, reading
every definition the API gives, and drawing it legibly in light and dark.

    python3 crates/ods-web/tests/browser/semantic.py [-v] [-k PATTERN]

Needs Playwright for Python (scripts/requirements-record.txt), a Chromium it can drive
(`python3 -m playwright install chromium`, or ODS_CHROMIUM), and `ods` (built with
cargo unless ODS_BIN_DIR names its directory). No network: only the local servers.
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
WITH = REPO / "fixtures/dbt/jaffle-metrics/artifacts/dbt-1.10"
WITHOUT = REPO / "fixtures/dbt/jaffle-ods/artifacts/dbt-1.10"
WAIT = 6000


def ods_bin_dir() -> Path:
    if os.environ.get("ODS_BIN_DIR"):
        return Path(os.environ["ODS_BIN_DIR"]).resolve()
    subprocess.run(["cargo", "build", "--quiet", "-p", "ods-cli", "--bin", "ods"], cwd=REPO, check=True)
    meta = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=REPO,
                          check=True, capture_output=True, text=True).stdout
    return Path(json.loads(meta)["target_directory"]) / "debug"


def serve(bin_dir: Path, target: Path, home: str) -> tuple[subprocess.Popen, str]:
    env = {"PATH": os.environ.get("PATH", ""), "HOME": home, "XDG_CONFIG_HOME": home, "TZ": "UTC",
           "LANG": "C.UTF-8"}
    server = subprocess.Popen([str(bin_dir / "ods"), "serve", "--target-dir", str(target), "--port", "0",
                               "--no-watch", "-o", "plain"],
                              cwd=home, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    for line in server.stdout:
        found = re.search(r"(http://127\.0\.0\.1:\d+)/", line)
        if found:
            return server, found.group(1)
    raise SystemExit(f"ods serve didn't start: {server.stderr.read()}")


def setUpModule() -> None:
    global SERVERS, URL, EMPTY_URL, PW, BROWSER, HOMES
    from playwright.sync_api import sync_playwright

    bin_dir = ods_bin_dir()
    HOMES = [tempfile.TemporaryDirectory(prefix="ods-semantic-") for _ in range(2)]
    with_layer, URL = serve(bin_dir, WITH, HOMES[0].name)
    without, EMPTY_URL = serve(bin_dir, WITHOUT, HOMES[1].name)
    SERVERS = [with_layer, without]
    PW = sync_playwright().start()
    BROWSER = PW.chromium.launch(executable_path=os.environ.get("ODS_CHROMIUM") or None)


def tearDownModule() -> None:
    BROWSER.close()
    PW.stop()
    for server in SERVERS:
        server.terminate()
        server.wait(timeout=10)
    for home in HOMES:
        home.cleanup()


class Semantic(unittest.TestCase):
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

    def api(self, url: str = "") -> dict:
        return json.loads(self.page.request.get(f"{url or URL}/api/catalog/semantic").text())

    def test_the_catalog_reaches_the_page(self) -> None:
        page = self.page
        page.goto(URL + "/catalog")
        page.locator('a[data-item="semantic"]').click()
        page.wait_for_url(re.compile(r"/catalog/semantic$"))
        self.assertEqual(page.get_by_role("heading", level=1).text_content().split()[:2], ["Semantic", "layer"])
        self.assertEqual(page.locator('a[data-item="semantic"]').get_attribute("aria-current"), "page")

    def test_every_definition_is_shown(self) -> None:
        view = self.api()
        page = self.page
        page.goto(URL + "/catalog/semantic")
        self.assertEqual(len(view["models"]), 2)
        self.assertEqual(len(view["metrics"]), 4)
        for model in view["models"]:
            card = page.locator(f'[data-semantic-model="{model["id"]}"]')
            text = card.inner_text()
            for f in model["measures"] + model["dimensions"] + model["entities"]:
                self.assertIn(f["name"], text, model["id"])
            # On its model, linked to that model's page.
            card.locator(".on a").click()
            page.wait_for_url(re.compile(r"/catalog/model\.jaffle_metrics\."))
            page.go_back()
        for metric in view["metrics"]:
            row = page.locator(f'tr[data-metric="{metric["id"]}"]')
            text = row.inner_text()
            self.assertIn(metric["name"], text)
            self.assertIn(metric["computed_from"], text)
            for model in metric["depends_on"]:
                self.assertIn(model["name"], text)
        aov = page.locator('tr[data-metric="metric.jaffle_metrics.average_order_value"]').inner_text()
        self.assertIn("revenue / orders_placed", aov)
        self.assertTrue(page.get_by_text("Read-only placeholder.").is_visible())

    def test_a_project_without_a_semantic_layer_says_so(self) -> None:
        view = self.api(EMPTY_URL)
        self.assertEqual((view["models"], view["metrics"]), ([], []))
        page = self.page
        page.goto(EMPTY_URL + "/catalog/semantic")
        self.assertTrue(page.get_by_role("heading", name="No semantic layer").is_visible())
        self.assertEqual(page.locator("table").count(), 0)

    def test_the_page_fits_and_is_legible(self) -> None:
        page = self.page
        page.goto(URL + "/catalog/semantic")
        self.assertFalse(page.evaluate("document.documentElement.scrollWidth > window.innerWidth"),
                         "no horizontal scroll at 1440px")
        body = page.evaluate("getComputedStyle(document.body).backgroundColor")
        text = page.evaluate("getComputedStyle(document.body).color")
        self.assertNotEqual(body, text)
        # The source is an active choice; other build tools are planned and look it.
        source = page.locator(".sem-pill").first
        planned = page.locator(".sem-pill.planned")
        self.assertEqual(planned.evaluate("e => getComputedStyle(e).borderStyle"), "dashed")
        self.assertNotEqual(source.evaluate("e => getComputedStyle(e).color"),
                            planned.evaluate("e => getComputedStyle(e).color"))
        self.assertNotEqual(page.locator(".sem-table-wrap").evaluate("e => getComputedStyle(e).backgroundColor"),
                            body)
        if os.environ.get("ODS_SCREENSHOTS"):
            out = Path(os.environ["ODS_SCREENSHOTS"])
            page.screenshot(path=str(out / f"semantic-{self.scheme}.png"), full_page=True)
            page.goto(EMPTY_URL + "/catalog/semantic")
            page.screenshot(path=str(out / f"semantic-empty-{self.scheme}.png"), full_page=True)


class SemanticDark(Semantic):
    """The same, in dark mode."""

    scheme = "dark"


if __name__ == "__main__":
    unittest.main()
