#!/usr/bin/env python3
"""Records scripted tours of the `ods serve` dashboard for the docs.

    scripts/record-dashboard.py [--check] [--webm] [--chromium PATH] [TOUR ...]

Each tour in docs/tapes/dashboard/*.toml is a list of steps (open a page, move the
pointer to an element and click it, scroll, show a caption, pause, take a still). The
script prepares the demo project with docs/tapes/dashboard/setup.sh (the fake dbt, a
few recorded runs, one of them partial), starts `ods serve` on it, and plays each tour
in Chromium at 1440x900, writing into docs/assets/recordings/dashboard/<tour>/:

- `<tour>.webp`: an animated WebP of the tour (960 px wide), for README and docs pages;
- `<still>.png`: the stills the tour names;
- `<still>.txt`: each still's visible text, with run ids, times and durations masked;
- `<tour>.webm`: Playwright's own video of the tour, only with `--webm` (not committed:
  large, and the WebP shows the same).

`--check` plays every tour without writing images: it fails if a step's element is
missing, if an `expect`ed text isn't on the page, or if a still's masked text differs
from the committed `.txt`. Nothing is timed, so it is quick.

Needs Python 3.11+, Playwright for Python and Pillow (scripts/requirements-record.txt),
a Chromium Playwright can drive (`python3 -m playwright install chromium`, or name one
with --chromium or ODS_CHROMIUM), and `ods` (built with cargo unless ODS_BIN_DIR names a
directory holding it). No network: the tours only talk to the local `ods serve`.

These are docs tools, not dependencies of any ODS crate.
"""

from __future__ import annotations

import argparse
import difflib
import io
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOURS = REPO / "docs" / "tapes" / "dashboard"
OUT = REPO / "docs" / "assets" / "recordings" / "dashboard"
VIEWPORT = {"width": 1440, "height": 900}
ANIMATION_WIDTH = 960
# Frames of the animation: the pointer moves in MOVE_FRAMES steps, each shown FRAME_MS.
MOVE_FRAMES = 14
FRAME_MS = 55

# What varies between recordings, masked in the stills' text (as `Mask` does in tapes).
MASKS = [
    (re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"), "<run-id>"),
    (re.compile(r"\b(?:run |runs/|· |snapshot \d+ · )?[0-9a-f]{8}\b"), "<run>"),
    (re.compile(r"\d{4}-\d\d-\d\d[T ]\d\d:\d\d(?::\d\d(?:\.\d+)?)?Z?"), "<time>"),
    (re.compile(r"\b\d\d:\d\d(?::\d\d(?:\.\d+)?)?Z?(?!\w)"), "<clock>"),
    # A day on its own, e.g. the Runs page's "today" group: recordings run on any day.
    (re.compile(r"\b\d{4}-\d\d-\d\d\b"), "<date>"),
    (re.compile(r"\b\d+(?:\.\d+)?\s?(?:ms|s)\b"), "<took>"),
    (re.compile(r"\b(?:just now|\d+ (?:second|minute|hour)s? ago)\b"), "<when>"),
]

# A pointer and a click ring drawn into the page, and a caption bar: the page's own
# markup is left alone, and everything is removed before a still is taken.
OVERLAY_JS = r"""
(() => {
  if (window.__odsTour) return;
  const css = document.createElement('style');
  css.textContent = `
    #ods-tour-cursor { position: fixed; z-index: 2147483647; width: 22px; height: 22px;
      margin: -3px 0 0 -3px; pointer-events: none; left: -40px; top: -40px;
      background: no-repeat url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 22 22'%3E%3Cpath d='M3 2l15 9-7 1.5L8 20z' fill='%2318202A' stroke='white' stroke-width='1.6' stroke-linejoin='round'/%3E%3C/svg%3E"); }
    #ods-tour-ring { position: fixed; z-index: 2147483646; width: 36px; height: 36px;
      margin: -18px 0 0 -18px; border-radius: 50%; pointer-events: none; opacity: 0;
      border: 3px solid #3F51D8; background: rgba(63, 81, 216, .18); }
    #ods-tour-caption { position: fixed; z-index: 2147483647; left: 50%; bottom: 28px;
      transform: translateX(-50%); max-width: 900px; padding: 10px 18px; border-radius: 8px;
      background: rgba(24, 32, 42, .92); color: #fff; pointer-events: none;
      font: 500 17px/1.4 'IBM Plex Sans', system-ui, sans-serif; text-align: center;
      box-shadow: 0 6px 24px rgba(0, 0, 0, .25); }
    #ods-tour-caption:empty { display: none; }`;
  document.head.appendChild(css);
  for (const id of ['ods-tour-cursor', 'ods-tour-ring', 'ods-tour-caption']) {
    const el = document.createElement('div');
    el.id = id;
    document.body.appendChild(el);
  }
  window.__odsTour = {
    move(x, y) { const c = document.getElementById('ods-tour-cursor'); c.style.left = x + 'px'; c.style.top = y + 'px'; },
    ring(x, y, on) { const r = document.getElementById('ods-tour-ring'); r.style.left = x + 'px'; r.style.top = y + 'px'; r.style.opacity = on ? 1 : 0; },
    caption(text) { document.getElementById('ods-tour-caption').textContent = text || ''; },
    hide(hidden) { for (const id of ['ods-tour-cursor', 'ods-tour-ring', 'ods-tour-caption']) document.getElementById(id).style.visibility = hidden ? 'hidden' : 'visible'; },
  };
})();
"""


def mask(text: str) -> str:
    for pattern, replacement in MASKS:
        text = pattern.sub(replacement, text)
    lines = [line.rstrip() for line in text.splitlines()]
    return "\n".join(line for line in lines if line) + "\n"


def ods_bin_dir() -> Path:
    if os.environ.get("ODS_BIN_DIR"):
        return Path(os.environ["ODS_BIN_DIR"])
    subprocess.run(["cargo", "build", "--quiet", "-p", "ods-cli", "--bin", "ods"], cwd=REPO, check=True)
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=REPO, check=True, capture_output=True, text=True,
    ).stdout
    import json

    return Path(json.loads(out)["target_directory"]) / "debug"


class Server:
    """The demo project, prepared by setup.sh, served by `ods serve` on a free port."""

    def __init__(self, bin_dir: Path):
        env = {
            "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
            "REPO": str(REPO),
            "TZ": "UTC",
            "LANG": "C.UTF-8",
        }
        subprocess.run(
            ["bash", "-c", 'source "$REPO/docs/tapes/dashboard/setup.sh" >/dev/null 2>&1'],
            env=env, check=True,
        )
        # setup.sh's project and HOME (see docs/tapes/setup.sh).
        project = Path("/tmp/ods-demo/jaffle_shop")
        env["HOME"] = "/tmp/ods-demo/home"
        self.process = subprocess.Popen(
            [str(bin_dir / "ods"), "serve", "--port", "0", "--no-watch", "-o", "plain"],
            cwd=project, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.url = None
        assert self.process.stdout is not None
        for line in self.process.stdout:
            found = re.search(r"(http://127\.0\.0\.1:\d+)/", line)
            if found:
                self.url = found.group(1)
                break
        if self.url is None:
            raise SystemExit(f"ods serve didn't start: {self.process.stderr.read() if self.process.stderr else ''}")

    def close(self) -> None:
        self.process.terminate()
        self.process.wait(timeout=10)


class Tour:
    """Plays one tour: in check mode it only asserts; otherwise it also collects the
    frames of the animation and the stills."""

    def __init__(self, page, base: str, name: str, check: bool, out: Path):
        self.page, self.base, self.name, self.check, self.out = page, base, name, check, out
        self.frames: list[tuple[bytes, int]] = []
        self.pointer = (VIEWPORT["width"] * 0.6, VIEWPORT["height"] * 0.55)
        self.caption_text = ""
        self.problems: list[str] = []
        self.stills: list[str] = []

    # Page helpers.
    def overlay(self) -> None:
        self.page.evaluate(OVERLAY_JS)
        self.page.evaluate("([x, y]) => window.__odsTour.move(x, y)", list(self.pointer))
        self.page.evaluate("t => window.__odsTour.caption(t)", self.caption_text)

    def settle(self) -> None:
        self.page.wait_for_load_state("networkidle")
        # Fonts and the lineage layout draw after load.
        self.page.evaluate("document.fonts.ready.then(() => true)")
        self.page.wait_for_timeout(150)
        self.overlay()

    def frame(self, ms: int) -> None:
        if self.check:
            return
        self.frames.append((self.page.screenshot(type="png"), ms))

    def locate(self, selector: str):
        locator = self.page.locator(selector).first
        try:
            locator.wait_for(state="visible", timeout=5000)
        except Exception:
            self.problems.append(f"{self.name}: no visible element for {selector!r} on {self.page.url}")
            return None
        return locator

    # Steps.
    def goto(self, path: str) -> None:
        self.page.goto(self.base + path)
        self.settle()

    def move_to(self, locator) -> tuple[float, float]:
        locator.scroll_into_view_if_needed()
        box = locator.bounding_box()
        target = (box["x"] + min(box["width"] / 2, 60), box["y"] + box["height"] / 2)
        start = self.pointer
        steps = 1 if self.check else MOVE_FRAMES
        for i in range(1, steps + 1):
            t = i / steps
            t = t * t * (3 - 2 * t)  # ease in and out
            x = start[0] + (target[0] - start[0]) * t
            y = start[1] + (target[1] - start[1]) * t
            self.page.mouse.move(x, y)
            self.page.evaluate("([x, y]) => window.__odsTour.move(x, y)", [x, y])
            self.frame(FRAME_MS)
        self.pointer = target
        return target

    def click(self, selector: str) -> None:
        locator = self.locate(selector)
        if locator is None:
            return
        x, y = self.move_to(locator)
        self.page.evaluate("([x, y]) => window.__odsTour.ring(x, y, true)", [x, y])
        self.frame(250)
        navigating = self.page.url
        self.page.mouse.click(x, y)
        self.page.wait_for_timeout(100)
        if self.page.url != navigating:
            self.settle()
        else:
            self.page.wait_for_timeout(250)
            self.overlay()
        self.page.evaluate("([x, y]) => window.__odsTour.ring(x, y, false)", [x, y])

    def hover(self, selector: str) -> None:
        locator = self.locate(selector)
        if locator is not None:
            self.move_to(locator)

    def scroll(self, selector: str) -> None:
        locator = self.locate(selector)
        if locator is None:
            return
        locator.evaluate("e => e.scrollIntoView({block: 'center'})")
        self.page.wait_for_timeout(150)
        self.frame(FRAME_MS)

    def caption(self, text: str) -> None:
        self.caption_text = text
        self.page.evaluate("t => window.__odsTour.caption(t)", text)

    def expect(self, text: str) -> None:
        # innerText follows CSS (e.g. upper-cased labels), so compare case- and
        # whitespace-insensitively.
        body = " ".join(self.page.evaluate("document.body.innerText").split()).casefold()
        if " ".join(text.split()).casefold() not in body:
            self.problems.append(f"{self.name}: expected {text!r} on {self.page.url}")

    def still(self, name: str) -> None:
        self.stills.append(name)
        self.page.evaluate("window.__odsTour.hide(true)")
        text = mask(self.page.evaluate("document.body.innerText"))
        committed = self.out / f"{name}.txt"
        if self.check:
            want = committed.read_text() if committed.exists() else ""
            if text != want:
                diff = "".join(difflib.unified_diff(
                    want.splitlines(True), text.splitlines(True), "committed", "this run"))
                self.problems.append(f"{self.name}: still {name} differs\n{diff}")
        else:
            self.page.screenshot(path=str(self.out / f"{name}.png"))
            committed.write_text(text)
        self.page.evaluate("window.__odsTour.hide(false)")

    def play(self, steps: list[dict]) -> None:
        for step in steps:
            if "goto" in step:
                self.goto(step["goto"])
            if "caption" in step:
                self.caption(step["caption"])
            if "scroll" in step:
                self.scroll(step["scroll"])
            if "hover" in step:
                self.hover(step["hover"])
            if "click" in step:
                self.click(step["click"])
            for text in step.get("expect", []):
                self.expect(text)
            self.frame(int(step.get("pause", 1600)))
            if "still" in step:
                self.still(step["still"])


def write_animation(frames: list[tuple[bytes, int]], path: Path) -> None:
    from PIL import Image

    images, durations = [], []
    for png, ms in frames:
        image = Image.open(io.BytesIO(png)).convert("RGB")
        height = round(image.height * ANIMATION_WIDTH / image.width)
        image = image.resize((ANIMATION_WIDTH, height), Image.LANCZOS)
        # Consecutive identical frames become one longer frame.
        if images and image.tobytes() == images[-1].tobytes():
            durations[-1] += ms
            continue
        images.append(image)
        durations.append(ms)
    images[0].save(
        path, save_all=True, append_images=images[1:], duration=durations, loop=0,
        quality=72, method=6,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("tours", nargs="*", help="tour names (default: every tour)")
    parser.add_argument("--check", action="store_true", help="assert steps and compare stills' text; write nothing")
    parser.add_argument("--webm", action="store_true", help="also keep Playwright's .webm video of each tour")
    parser.add_argument("--chromium", default=os.environ.get("ODS_CHROMIUM"), help="Chromium executable")
    args = parser.parse_args()

    from playwright.sync_api import sync_playwright

    names = args.tours or sorted(p.stem for p in TOURS.glob("*.toml"))
    tours = {name: tomllib.loads((TOURS / f"{name}.toml").read_text()) for name in names}
    server = Server(ods_bin_dir())
    problems: list[str] = []
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(executable_path=args.chromium)
            for name, tour in tours.items():
                out = OUT / name
                out.mkdir(parents=True, exist_ok=True)
                video_dir = Path(tempfile.mkdtemp(prefix="ods-tour-")) if args.webm and not args.check else None
                context = browser.new_context(
                    viewport=VIEWPORT, device_scale_factor=1,
                    color_scheme=tour.get("theme", "light"), reduced_motion="reduce",
                    timezone_id="UTC", locale="en-GB",
                    record_video_dir=str(video_dir) if video_dir else None,
                    record_video_size=VIEWPORT if video_dir else None,
                )
                # Only the local server: anything else would be a network call.
                context.route(re.compile(r"^(?!" + re.escape(server.url) + r")https?://"), lambda route: route.abort())
                page = context.new_page()
                player = Tour(page, server.url, name, args.check, out)
                print(f"{name}: {tour.get('title', '')}", file=sys.stderr)
                player.play(tour["step"])
                context.close()
                problems += player.problems
                if not args.check:
                    for old in out.glob("*.txt"):
                        if old.stem not in player.stills:
                            old.unlink()
                            old.with_suffix(".png").unlink(missing_ok=True)
                    write_animation(player.frames, out / f"{name}.webp")
                    if video_dir:
                        for video in video_dir.glob("*.webm"):
                            shutil.move(video, out / f"{name}.webm")
                        shutil.rmtree(video_dir, ignore_errors=True)
                    for written in sorted(out.iterdir()):
                        print(f"  {written.relative_to(REPO)} ({written.stat().st_size} bytes)", file=sys.stderr)
                else:
                    print(f"  {len(player.stills)} stills, {len(tour['step'])} steps: "
                          f"{'ok' if not player.problems else 'FAILED'}", file=sys.stderr)
            browser.close()
    finally:
        server.close()
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
