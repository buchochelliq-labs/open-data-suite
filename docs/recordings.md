# Recording the docs

The terminal screenshots and the dashboard tours in these docs are recordings of the
real `ods`, made by two scripts and checked in CI, so they can't drift from what ODS
prints. Everything runs offline, on a scratch copy of the demo dbt project
(`fixtures/dbt/jaffle-ods`), with the repository's fake dbt (`fixtures/dbt/fake-dbt`)
standing in for dbt: no warehouse, no network and no Python packages for dbt.

| What | Written by | Source | Output |
|---|---|---|---|
| Terminal sessions (`ods state build`, `plan`, `history`, …) | `scripts/record-docs.sh` | `docs/tapes/*.tape` | `docs/assets/recordings/<tape>/` |
| Dashboard tours (`ods serve`) | `scripts/record-dashboard.py` | `docs/tapes/dashboard/*.toml` | `docs/assets/recordings/dashboard/<tour>/` |

Both are docs tools, not dependencies of any ODS crate. The terminal recorder,
[rs-rich-record](https://crates.io/crates/rs-rich-record) (MIT), embeds fonts under the
Bitstream Vera and CC BY 4.0 licences, which is why it isn't linked into the workspace.

## Terminal recordings

Install the `rich` CLI once, outside the repository, then record:

```sh
cargo install rs-rich-cli --version 0.0.13 --locked
scripts/record-docs.sh                    # every tape
scripts/record-docs.sh state-build        # one tape
scripts/record-docs.sh --check            # compare with what is committed; writes nothing
```

The script builds `ods` (debug) and runs `rich record` from the repository root, so a
tape reads the repository as `$REPO`. Set `ODS_BIN_DIR` to use an `ods` you've built,
and `RICH` to use another `rich`.

A **tape** is a scripted terminal session, one step per line (the full language is in
rs-rich-record's README). Each tape here starts with a hidden step that sources
`docs/tapes/setup.sh`, which copies the demo project to `/tmp/ods-demo/jaffle_shop`
(a fixed path, so the paths ODS prints are the same on every run), puts the fake dbt on
`PATH`, and defines `ods_edit NODE` to change a model's code as editing it would:

```text
# One line on what the tape shows.
Set Size 120x40
Set Title "ods state plan"
Output png svg                     # what to write: png svg cast gif mp4 html
Mask /[0-9a-f]{8}-[0-9a-f-]{27}/ "<run-id>"
Hide
Type "source $REPO/docs/tapes/setup.sh && dbt compile >/dev/null && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/          # the prompt is back: the command finished
Show
Type "ods state plan"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Screenshot state-plan              # <name>.png / .svg / .txt
```

Every `Screenshot` also writes `<name>.txt`, its text. `--check` runs each tape again
and compares that text, after the tape's `Mask`s, with the committed file, so an
`ods` change that alters what a page shows fails CI until the recordings are made
again. Mask what varies from run to run: run ids, times, and durations measured by the
clock (the fake dbt reports fixed node times). If a varying value changes the width
of a table column, collapse the table in the text with masks (see
`state-history.tape`) or pick a terminal wide enough that nothing wraps.

To add a tape: write `docs/tapes/<name>.tape`, record it, look at the images, run
`--check` twice to see that it is stable, embed the SVG (or PNG) in the docs, and
commit the tape with everything in `docs/assets/recordings/<name>/`, including
`provenance.json` (it lists the screenshots, so a screenshot a tape stops taking is
removed on the next recording). MP4 isn't committed; prefer SVG in pages (small, with
selectable text), and PNG or GIF where an image must look the same everywhere.

## Dashboard tours

```sh
pip install -r scripts/requirements-record.txt   # Playwright for Python, Pillow
python3 -m playwright install chromium
scripts/record-dashboard.py                      # every tour
scripts/record-dashboard.py runs --webm          # one tour, also keeping a .webm video
scripts/record-dashboard.py --check              # steps and stills' text; writes nothing
```

`--chromium PATH` (or `ODS_CHROMIUM`) picks a Chromium when Playwright's own isn't
installed. `ODS_DEMO_ROOT` moves the scratch project from `/tmp/ods-demo`, so
several recordings or test runs can go at once. `docs/tapes/dashboard/setup.sh` prepares the project: the same scratch copy,
then a few runs of the fake dbt (a full build, a partial build whose failed node's
error is redacted, a `retry --failed`) and a code change, so every page has something
to show. The script then starts `ods serve` on a free port and plays each tour in
Chromium at 1440×900; every request other than to that server is blocked.

A **tour** is a TOML list of steps. Each step may `goto` a path, show a `caption`,
`scroll` to, `hover` or `click` an element (a Playwright selector; the pointer moves
there and a ring marks the click), assert texts with `expect`, `pause` (milliseconds),
and take a `still`:

```toml
title = "Runs: from Home to a failed node"
theme = "light"                 # or "dark": the page follows prefers-color-scheme

[[step]]
goto = "/"
caption = "Home: the project's health, the recent runs, what needs attention"
expect = ["Project health"]
pause = 3800
still = "home"
```

A step may also `focus` an element and `press` keys (e.g. `Shift+F10`), or `drag`
(`{ from = "<selector>", by = [dx, dy] }`). A tour with `live_run = "<run id>"` (and
`live_scope`) plays a simulated run (#322): each `run = <seconds>` step writes the
live-run board's demo run (`scripts/ods_live_sim.py`) into the run's journal up to that
time, then waits until the page has read it and announced it, so its stills are the same
every time; the journal is removed when the tour ends, so later tours don't list it.

It writes, per tour, an animated WebP (960 px wide) for pages to embed, a PNG per still,
and each still's visible text (`<still>.txt`, with run ids, dates, times and durations
masked).
`--check` plays the tours without images: a step whose element is missing, an
`expect` that isn't on the page, or a still whose text differs fails. The `.webm`
video (`--webm`) isn't committed.

CI (`docs-media` in `.github/workflows/ci.yml`) runs both checks on Linux, then the
live run view's browser tests (`crates/ods-web/tests/browser/live_view.py`), which use
the same Playwright, Chromium and simulated run.
