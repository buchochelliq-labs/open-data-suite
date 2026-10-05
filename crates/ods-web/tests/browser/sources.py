#!/usr/bin/env python3
"""The Freshness evidence screen in a browser (#350): `ods serve` on the demo project's
artifacts, and Chromium reaching it from the Catalog, reading every input's evidence,
and drawing it legibly in light and dark.

    python3 crates/ods-web/tests/browser/sources.py [-v] [-k PATTERN]

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

    HOME = tempfile.TemporaryDirectory(prefix="ods-sources-")
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


class Sources(unittest.TestCase):
    scheme = "light"

    def setUp(self) -> None:
        self.context = BROWSER.new_context(viewport={"width": 1440, "height": 900},
                                           color_scheme=self.scheme)
        self.page = self.context.new_page()
        self.page.set_default_timeout(WAIT)
        self.errors: list[str] = []
        self.page.on("pageerror", lambda e: self.errors.append(str(e)))

    def tearDown(self) -> None:
        self.context.close()
        self.assertEqual(self.errors, [], "no script errors")

    def api(self) -> dict:
        return json.loads(self.page.request.get(f"{URL}/api/catalog/sources").text())

    def test_the_catalog_navigation_reaches_the_screen(self) -> None:
        page = self.page
        page.goto(f"{URL}/catalog")
        page.get_by_role("link", name="Freshness evidence").click()
        page.wait_for_url(re.compile(r"/catalog/sources$"))
        self.assertEqual(page.get_by_role("heading", level=1).text_content(), "Freshness evidence")
        # The crumb leads back to the Catalog.
        page.locator("a.crumb", has_text="Catalog").click()
        page.wait_for_url(re.compile(r"/catalog$"))

    def test_every_input_is_listed_with_its_evidence(self) -> None:
        view = self.api()
        page = self.page
        page.goto(f"{URL}/catalog/sources")
        rows = page.locator("tr[data-input]")
        self.assertEqual(rows.count(), len(view["inputs"]))
        self.assertEqual(view["seeds"], 3)
        self.assertEqual(view["sources"], 0)
        # No state store: nothing compared, so every seed's grade is unknown.
        self.assertEqual(page.locator('td[data-grade="unknown"]').count(), 3)
        self.assertTrue(page.locator('[data-note="no-sources"]').is_visible())
        # Each seed links to its model page, one level up.
        link = rows.first.locator("a.mono").first
        self.assertTrue(link.get_attribute("href").startswith("../catalog/seed."))
        # The rule and every grade are in the rail.
        self.assertEqual(page.locator(".fresh-rail .rule").count(), len(view["grades"]))
        self.assertIn("Without a plan", page.locator("[data-summary]").text_content())

    def test_the_page_fits_and_grades_are_visible(self) -> None:
        page = self.page
        page.goto(f"{URL}/catalog/sources")
        overflow = page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
        self.assertFalse(overflow, "no horizontal scroll at 1440px")
        background = page.evaluate("getComputedStyle(document.body).backgroundColor")
        dot = page.locator(".legend .gdot.exact")
        self.assertNotEqual(dot.evaluate("e => getComputedStyle(e).backgroundColor"), background,
                            "the exact grade's dot stands out from the page")
        unknown = page.locator(".legend .gdot.unknown")
        self.assertEqual(unknown.evaluate("e => getComputedStyle(e).borderStyle"), "dashed")


class SourcesDark(Sources):
    """The same, in dark mode."""

    scheme = "dark"


if __name__ == "__main__":
    unittest.main()
