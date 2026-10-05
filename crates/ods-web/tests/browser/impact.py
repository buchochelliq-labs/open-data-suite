#!/usr/bin/env python3
"""The Impact simulator in a browser (#347): `ods serve` on the demo project's
artifacts, and Chromium opening the simulator from a column of the Model page,
choosing a change, and reading what it reaches.

    python3 crates/ods-web/tests/browser/impact.py [-v] [-k PATTERN]

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

    HOME = tempfile.TemporaryDirectory(prefix="ods-impact-")
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


class Simulator(unittest.TestCase):
    def setUp(self) -> None:
        self.context = BROWSER.new_context(viewport={"width": 1440, "height": 900})
        self.page = self.context.new_page()
        self.page.set_default_timeout(WAIT)
        self.errors: list[str] = []
        self.page.on("pageerror", lambda e: self.errors.append(str(e)))

    def tearDown(self) -> None:
        self.context.close()
        self.assertEqual(self.errors, [], "no script errors")

    def verdicts(self) -> dict[str, str]:
        rows = self.page.locator("tr[data-verdict]")
        return {rows.nth(i).get_attribute("data-node").rsplit(".", 1)[-1]: rows.nth(i).get_attribute("data-verdict")
                for i in range(rows.count())}

    def test_from_a_column_drop_it_and_see_what_breaks(self) -> None:
        page = self.page
        page.goto(f"{URL}/catalog/model.jaffle_ods.orders?tab=columns")
        page.locator('tr[data-column="amount"] a.simulate').click()
        page.wait_for_url(re.compile(r"/lineage/impact\?column=model\.jaffle_ods\.orders\.amount$"))
        self.assertEqual(page.locator('input[name="column"]').input_value(), "orders.amount")
        self.assertEqual(page.locator("tr[data-verdict]").count(), 0, "nothing until a change is chosen")
        page.get_by_role("radio", name="drop").check()
        page.get_by_role("button", name="Simulate").click()
        page.wait_for_selector("tr[data-verdict]")
        verdicts = self.verdicts()
        self.assertEqual(verdicts["orders"], "changed")
        self.assertEqual(verdicts["customers"], "breaks")
        self.assertEqual(verdicts["customer_order_rank"], "breaks")
        # The Python model's lineage is unknown. What reads it is only affected: it reads
        # `customers`, which doesn't pass `amount` on, so no removed column reaches it.
        self.assertEqual(verdicts["customer_segments"], "unknown")
        self.assertEqual(verdicts["segment_summary"], "affected")
        self.assertIn("1 unknown", page.locator(".imp-results-head").text_content())
        self.assertIn(" -s customers ", page.locator("#imp-selector").text_content())
        # The URL is the simulation: reloading it shows the same.
        page.reload()
        self.assertEqual(self.verdicts(), verdicts)

    def test_a_type_change_breaks_nothing_and_keyboard_works(self) -> None:
        page = self.page
        page.goto(f"{URL}/lineage/impact")
        page.locator('input[name="column"]').fill("orders.amount")
        page.get_by_role("radio", name="type change").focus()
        page.keyboard.press("Space")
        page.locator('input[name="to"]').fill("decimal(18,2)")
        page.locator('input[name="to"]').press("Enter")
        page.wait_for_selector("tr[data-verdict]")
        verdicts = self.verdicts()
        self.assertNotIn("breaks", verdicts.values())
        self.assertEqual(verdicts["customers"], "affected")
        self.assertIn("decimal(18,2)", page.locator('tr[data-verdict="changed"]').text_content())

    def test_enter_simulates_even_with_several_rows(self) -> None:
        page = self.page
        page.goto(f"{URL}/lineage/impact?column=orders.amount&change-0=drop&add=1")
        page.locator('input[name="column"]').nth(1).fill("orders.status")
        page.get_by_role("radio", name="type change").nth(1).check()
        page.locator('input[name="column"]').nth(1).press("Enter")
        page.wait_for_selector("tr[data-verdict]")
        self.assertEqual(page.locator('input[name="column"]').count(), 2, "Enter didn't remove a row")
        self.assertEqual(self.verdicts()["customers"], "breaks")

    def test_a_mistake_is_said_and_nothing_is_simulated(self) -> None:
        page = self.page
        page.goto(f"{URL}/lineage/impact?column=orders.nope&change-0=drop")
        alert = page.get_by_role("alert")
        self.assertIn("has no column `nope`", alert.text_content())
        self.assertEqual(page.locator('input[name="column"]').get_attribute("aria-invalid"), "true")
        self.assertEqual(page.locator("tr[data-verdict]").count(), 0)


if __name__ == "__main__":
    unittest.main()
