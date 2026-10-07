#!/usr/bin/env python3
"""The Settings page in a browser (#351): `ods serve` on the demo project's artifacts,
with an `ods.toml` holding a secret reference and a connection string with a password,
and Chromium reaching the page from the navigation, reading each part, and drawing it
legibly in light and dark without the password.

    python3 crates/ods-web/tests/browser/settings.py [-v] [-k PATTERN]

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
SENTINEL = "sk_live_SENTINEL_42"
TOML = f"""[providers.dbt]
kind = "dbt"
[providers.dbt.settings]
target = "dev"

[providers.uc]
kind = "databricks"
[providers.uc.settings]
client_secret = {{ secret = "env:DATABRICKS_CLIENT_SECRET" }}

[providers.pg]
kind = "postgres"
[providers.pg.settings]
url = "postgres://ods:{SENTINEL}@db.example.com/ods"
"""


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

    HOME = tempfile.TemporaryDirectory(prefix="ods-settings-")
    Path(HOME.name, "ods.toml").write_text(TOML)
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


class Settings(unittest.TestCase):
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
        return json.loads(self.page.request.get(f"{URL}/api/settings").text())

    def test_settings_in_the_navigation_reaches_the_page(self) -> None:
        page = self.page
        page.goto(URL + "/")
        page.locator('a[data-section="settings"]').click()
        page.wait_for_url(re.compile(r"/settings$"))
        self.assertEqual(page.get_by_role("heading", level=1).text_content(), "Settings")
        self.assertEqual(page.locator('a[data-item="configuration"]').get_attribute("aria-current"), "page")
        # About is beside it.
        page.locator('a[data-item="about"]').click()
        page.wait_for_url(re.compile(r"/settings/about$"))

    def test_every_key_and_check_is_shown_and_no_password(self) -> None:
        view = self.api()
        page = self.page
        page.goto(f"{URL}/settings")
        self.assertEqual(page.locator("tr[data-key]").count(), len(view["entries"]))
        self.assertEqual(page.locator("li[data-check]").count(), len(view["checks"]))
        self.assertEqual(page.locator("[data-provider]").count(), len(view["providers"]))
        self.assertNotIn(SENTINEL, page.content())
        self.assertIn("secret(env:DATABRICKS_CLIENT_SECRET)",
                      page.locator("section[aria-label='Secrets']").text_content())
        self.assertEqual(page.locator("form, input, button[type=submit]").count(), 0, "nothing writes")

    def test_the_page_fits_and_statuses_are_visible(self) -> None:
        page = self.page
        page.goto(f"{URL}/settings")
        overflow = page.evaluate("document.documentElement.scrollWidth > window.innerWidth")
        self.assertFalse(overflow, "no horizontal scroll at 1440px")
        ok = page.locator(".status.ok").first
        background = page.evaluate("getComputedStyle(document.body).backgroundColor")
        self.assertNotEqual(ok.evaluate("e => getComputedStyle(e).color"), background)
        planned = page.locator(".planned-card")
        self.assertEqual(planned.evaluate("e => getComputedStyle(e).borderStyle"), "dashed")
        if os.environ.get("ODS_SCREENSHOTS"):
            page.screenshot(path=str(Path(os.environ["ODS_SCREENSHOTS"]) / f"settings-{self.scheme}.png"),
                            full_page=True)


class SettingsDark(Settings):
    """The same, in dark mode."""

    scheme = "dark"


if __name__ == "__main__":
    unittest.main()
