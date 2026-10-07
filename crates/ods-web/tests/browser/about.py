#!/usr/bin/env python3
"""The About page in a browser (ADR-0031 §3c): `ods serve` on the demo project's
artifacts, and Chromium reaching it from Settings, reading each plugin and what it
offers, and drawing it legibly in light and dark.

    python3 crates/ods-web/tests/browser/about.py [-v] [-k PATTERN]

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

    HOME = tempfile.TemporaryDirectory(prefix="ods-about-")
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


class About(unittest.TestCase):
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
        return json.loads(self.page.request.get(f"{URL}/api/settings/about").text())

    def test_settings_in_the_navigation_reaches_the_page(self) -> None:
        page = self.page
        page.goto(URL + "/")
        page.locator('a[data-section="settings"]').click()
        page.wait_for_url(re.compile(r"/settings/about$"))
        self.assertEqual(page.get_by_role("heading", level=1).text_content(), "About")
        # Settings' pages: About is current, Configuration is planned.
        self.assertEqual(page.locator('a[data-item="about"]').get_attribute("aria-current"), "page")
        self.assertTrue(page.locator('span.planned[data-item="configuration"]').is_visible())
        # Home is one level up.
        page.locator('a[data-section="home"]').click()
        page.wait_for_url(re.compile(r"/$"))

    def test_every_plugin_is_shown_with_what_it_offers(self) -> None:
        view = self.api()
        page = self.page
        page.goto(f"{URL}/settings/about")
        plugins = view["warehouses"] + view["health_checks"]
        self.assertEqual(page.locator("[data-plugin]").count(), len(plugins))
        for plugin in plugins:
            row = page.locator(f'[data-plugin="{plugin["name"]}"]')
            self.assertEqual(row.locator(".feature").count(), len(plugin["features"]), plugin["name"])
        # The demo project builds on DuckDB, which the built-in plugin serves.
        self.assertEqual(view["warehouse"], "duckdb")
        self.assertTrue(page.locator('[data-plugin="duckdb"] .this-project').is_visible())
        self.assertEqual(page.locator(".this-project").count(), 1)
        self.assertIn(view["ods_version"], page.locator("section[aria-label='This ods']").text_content())

    def test_the_page_fits_and_unusable_features_look_different(self) -> None:
        page = self.page
        page.goto(f"{URL}/settings/about")
        overflow = page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
        self.assertFalse(overflow, "no horizontal scroll at 1440px")
        # Without a host, Databricks' links are offered but not usable here.
        unusable = page.locator('[data-plugin="databricks"] .feature.unusable')
        self.assertEqual(unusable.count(), 1)
        self.assertEqual(unusable.evaluate("e => getComputedStyle(e).borderStyle"), "dashed")
        usable = page.locator('[data-plugin="databricks"] .feature:not(.unusable)').first
        self.assertNotEqual(usable.evaluate("e => getComputedStyle(e).color"),
                            unusable.evaluate("e => getComputedStyle(e).color"))
        background = page.evaluate("getComputedStyle(document.body).backgroundColor")
        self.assertNotEqual(usable.evaluate("e => getComputedStyle(e).backgroundColor"), background)
        if os.environ.get("ODS_SCREENSHOTS"):
            page.screenshot(path=str(Path(os.environ["ODS_SCREENSHOTS"]) / f"about-{self.scheme}.png"),
                            full_page=True)


class AboutDark(About):
    """The same, in dark mode."""

    scheme = "dark"


if __name__ == "__main__":
    unittest.main()
