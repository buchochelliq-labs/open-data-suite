#!/usr/bin/env python3
"""Renders the styled `ods` output of a CI run as terminal screenshots for the docs.

    render-transcripts.py LOG OUT_DIR [NAME ...]

LOG is a job log from the `databricks` workflow (#294), in which
`.github/databricks/demo.sh` prints each command's output between
`ods-transcript-begin NAME` and `ods-transcript-end NAME`. Every transcript, or only
the NAMEs given, becomes `OUT_DIR/state-databricks-NAME.png`.

Needs Node.js with Playwright (`npm install -g playwright`, then
`npx playwright install chromium`), which screenshots exactly the terminal element.

Databricks workspace hostnames are replaced with `<workspace>`: they identify the
workspace and add nothing to the docs.

The ANSI handling is a few lines for the SGR codes rs-rich emits (bold, dim,
underline, the 16 colours). The converters on PyPI are LGPL or pull in more than this
needs.
"""

from __future__ import annotations

import html
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

# GitHub prefixes each log line with an ISO timestamp.
TIMESTAMP = re.compile(r"^\d{4}-\d\d-\d\dT[\d:.]+Z ")
SGR = re.compile(r"\x1b\[([\d;]*)m")
WORKSPACE_HOST = re.compile(
    r"[\w-]+(?=\.(?:cloud\.databricks\.com|azuredatabricks\.net|gcp\.databricks\.com)\b)"
)
PALETTE = {
    30: "#45475a", 31: "#f38ba8", 32: "#a6e3a1", 33: "#f9e2af",
    34: "#89b4fa", 35: "#f5c2e7", 36: "#94e2d5", 37: "#bac2de",
    90: "#585b70", 91: "#f38ba8", 92: "#a6e3a1", 93: "#f9e2af",
    94: "#89b4fa", 95: "#f5c2e7", 96: "#94e2d5", 97: "#a6adc8",
}
LINE_PX = 17


def transcripts(log: str) -> dict[str, list[str]]:
    found: dict[str, list[str]] = {}
    current = None
    for raw in log.splitlines():
        line = TIMESTAMP.sub("", raw, count=1)
        if line.startswith("ods-transcript-begin "):
            current = line.split(maxsplit=1)[1].strip()
            found[current] = []
        elif line.startswith("ods-transcript-end "):
            current = None
        elif current is not None:
            found[current].append(WORKSPACE_HOST.sub("<workspace>", line))
    return found


def to_html(lines: list[str]) -> str:
    out = []
    for line in lines:
        style: dict[str, str] = {}
        parts = []
        pos = 0
        for m in SGR.finditer(line):
            parts.append(span(line[pos : m.start()], style))
            codes = [int(c) for c in m.group(1).split(";") if c] or [0]
            for code in codes:
                if code == 0:
                    style = {}
                elif code == 1:
                    style["font-weight"] = "bold"
                elif code == 2:
                    style["opacity"] = "0.6"
                elif code == 4:
                    style["text-decoration"] = "underline"
                elif code in PALETTE:
                    style["color"] = PALETTE[code]
                elif code == 39:
                    style.pop("color", None)
            pos = m.end()
        parts.append(span(line[pos:], style))
        out.append("".join(parts))
    return "\n".join(out)


def span(text: str, style: dict[str, str]) -> str:
    if not text:
        return ""
    escaped = html.escape(text)
    if not style:
        return escaped
    css = ";".join(f"{k}:{v}" for k, v in style.items())
    return f'<span style="{css}">{escaped}</span>'


def page(body: str, title: str) -> str:
    return f"""<!doctype html><meta charset="utf-8">
<style>
body {{ margin: 0; background: #11111b; }}
.term {{ display: inline-block; margin: 0; background: #1e1e2e; color: #cdd6f4; border-radius: 8px;
  font: 14px/{LINE_PX}px "DejaVu Sans Mono", Menlo, Consolas, monospace; }}
.bar {{ padding: 8px 12px; color: #6c7086; font-size: 12px; }}
.bar i {{ display: inline-block; width: 11px; height: 11px; border-radius: 50%;
  margin-right: 6px; }}
pre {{ margin: 0; padding: 4px 16px 16px; white-space: pre; font: inherit; }}
</style>
<div class="term"><div class="bar"><i style="background:#f38ba8"></i><i
style="background:#f9e2af"></i><i style="background:#a6e3a1"></i> {html.escape(title)}</div>
<pre>{body}</pre></div>"""


SHOOT = """
const { chromium } = require('playwright');
(async () => {
  const [page_url, out] = process.argv.slice(1);
  const browser = await chromium.launch();
  const page = await browser.newPage({ deviceScaleFactor: 2, viewport: { width: 2000, height: 800 } });
  await page.goto(page_url);
  await page.locator('.term').screenshot({ path: out });
  await browser.close();
})().catch((e) => { console.error(e); process.exit(1); });
"""


def render(name: str, lines: list[str], out: Path) -> Path:
    while lines and not lines[-1].strip():
        lines.pop()
    target = out / f"state-databricks-{name}.png"
    with tempfile.TemporaryDirectory() as tmp:
        doc = Path(tmp) / "t.html"
        doc.write_text(page(to_html(lines), f"ods · {name}"))
        root = subprocess.run(
            ["npm", "root", "-g"], check=True, capture_output=True, text=True
        ).stdout.strip()
        subprocess.run(
            ["node", "-e", SHOOT, doc.as_uri(), str(target.resolve())],
            check=True,
            env={**os.environ, "NODE_PATH": root},
        )
    return target


def main(argv: list[str]) -> None:
    if len(argv) < 2:
        sys.exit(__doc__)
    log, out, *names = argv
    found = transcripts(Path(log).read_text(errors="replace"))
    if not found:
        sys.exit(f"no ods-transcript markers in {log}")
    Path(out).mkdir(parents=True, exist_ok=True)
    for name in names or sorted(found):
        if name not in found:
            sys.exit(f"no transcript named {name}; found {', '.join(sorted(found))}")
        print(render(name, found[name], Path(out)))


if __name__ == "__main__":
    main(sys.argv[1:])
