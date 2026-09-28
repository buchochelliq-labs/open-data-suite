#!/usr/bin/env python3
"""Print one version's section of CHANGELOG.md: the release notes (ADR-0019 §3, #212).

    python3 scripts/changelog-section.py 0.1.0          # or v0.1.0, or Unreleased
    python3 scripts/changelog-section.py v0.1.0 --changelog path/to/CHANGELOG.md
    python3 scripts/changelog-section.py --self-test

The section is everything under `## [X.Y.Z]` (optionally followed by ` - YYYY-MM-DD`)
up to the next `## ` heading or the link reference definitions at the end of the file,
printed without the heading itself. It exits non-zero when the section is missing or has
no content, so a release is never published without notes.
"""
import argparse
import contextlib
import io
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# `## [0.1.0] - 2026-10-01`, `## [0.1.0]`, `## [Unreleased]`.
HEADING = re.compile(r"^## \[(?P<version>[^\]]+)\](?:\s+-\s+\S.*)?\s*$")
# Keep a Changelog ends with `[0.1.0]: https://…` link definitions.
LINK_DEFINITION = re.compile(r"^\[[^\]]+\]:\s+\S")
VERSION = re.compile(r"^(?:Unreleased|\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$")


class ChangelogError(Exception):
    """The requested section can't be produced."""


def normalise(version: str) -> str:
    """`v0.1.0` → `0.1.0`; `unreleased` → `Unreleased`."""
    version = version.strip()
    if version.lower() == "unreleased":
        return "Unreleased"
    if version.startswith("v"):
        version = version[1:]
    if not VERSION.match(version):
        raise ChangelogError(f"not a version: {version!r} (expected X.Y.Z, vX.Y.Z or Unreleased)")
    return version


def section(text: str, version: str) -> str:
    """Returns the body of `## [version]`, trimmed of blank lines at either end."""
    version = normalise(version)
    lines = text.splitlines()
    start = None
    for i, line in enumerate(lines):
        m = HEADING.match(line)
        if m and m.group("version") == version:
            if start is not None:
                raise ChangelogError(f"CHANGELOG.md has more than one [{version}] section")
            start = i + 1
    if start is None:
        raise ChangelogError(f"CHANGELOG.md has no `## [{version}]` section")
    body = []
    for line in lines[start:]:
        if line.startswith("## ") or LINK_DEFINITION.match(line):
            break
        body.append(line)
    while body and not body[0].strip():
        body.pop(0)
    while body and not body[-1].strip():
        body.pop()
    if not body:
        raise ChangelogError(f"the `## [{version}]` section of CHANGELOG.md is empty")
    return "\n".join(body) + "\n"


SAMPLE = """# Changelog

Intro.

## [Unreleased]

### Added
- Something new (#2).

## [0.2.0] - 2026-11-01

### Breaking
- Renamed `x` to `y` (#5).

### Fixed
- A bug (#4).

## [0.1.0] - 2026-10-01

### Added
- The first release (#1).

## [0.0.9]

[0.2.0]: https://example.com/compare/v0.1.0...v0.2.0
[0.1.0]: https://example.com/releases/tag/v0.1.0
"""


def self_test() -> None:
    """Checks extraction, normalisation and every failure mode against SAMPLE."""
    assert section(SAMPLE, "0.2.0") == (
        "### Breaking\n- Renamed `x` to `y` (#5).\n\n### Fixed\n- A bug (#4).\n"
    ), section(SAMPLE, "0.2.0")
    assert section(SAMPLE, "v0.1.0") == "### Added\n- The first release (#1).\n"
    assert section(SAMPLE, "unreleased") == "### Added\n- Something new (#2).\n"
    for bad, why in [
        ("0.3.0", "no `## [0.3.0]`"),
        ("0.0.9", "is empty"),
        ("0.2", "not a version"),
        ("v0.2.0; rm -rf", "not a version"),
    ]:
        try:
            section(SAMPLE, bad)
        except ChangelogError as e:
            assert why in str(e), (bad, str(e))
        else:
            raise AssertionError(f"{bad!r} should have failed")
    try:
        section(SAMPLE + "\n## [0.1.0]\n- again\n", "0.1.0")
    except ChangelogError as e:
        assert "more than one" in str(e)
    else:
        raise AssertionError("a duplicated section should have failed")
    # The real changelog always has an Unreleased section to add entries to.
    section((ROOT / "CHANGELOG.md").read_text(encoding="utf-8"), "Unreleased")
    # The command line, end to end.
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "CHANGELOG.md"
        path.write_text(SAMPLE, encoding="utf-8")
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            assert main(["v0.1.0", "--changelog", str(path)]) == 0
            assert main(["9.9.9", "--changelog", str(path)]) == 1
        assert out.getvalue() == "### Added\n- The first release (#1).\n", out.getvalue()
    print("changelog-section self-test: ok", file=sys.stderr)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("version", nargs="?", help="X.Y.Z, vX.Y.Z or Unreleased")
    parser.add_argument("--changelog", type=Path, default=ROOT / "CHANGELOG.md")
    parser.add_argument("--self-test", action="store_true", help="check this script")
    args = parser.parse_args(argv)
    if args.self_test:
        self_test()
        return 0
    if not args.version:
        parser.error("a version is required")
    try:
        text = args.changelog.read_text(encoding="utf-8")
        sys.stdout.write(section(text, args.version))
    except (ChangelogError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
