#!/usr/bin/env python3
"""Render the Homebrew formula for a release from its SHA256SUMS (#212).

    python3 scripts/render-homebrew-formula.py --version 0.1.0 --sums dist/SHA256SUMS > ods.rb
    python3 scripts/render-homebrew-formula.py --self-test

Fills packaging/homebrew/ods.rb.tmpl with the version, the release download URL and the
sha256 of each archive the formula installs. Fails if an archive's checksum is missing,
so a formula can't point at an asset the release doesn't have.
"""
import argparse
import re
import string
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEMPLATE = ROOT / "packaging" / "homebrew" / "ods.rb.tmpl"
REPOSITORY = "https://github.com/buchochelliq-labs/open-data-suite"
# The archives the formula installs: macOS, and static Linux binaries.
TARGETS = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
]
VERSION = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
# `sha256sum` output: `<hex>  <name>` (text mode) or `<hex> *<name>` (binary mode).
SUM_LINE = re.compile(r"^(?P<sha>[0-9a-f]{64}) [ *](?P<name>\S+)$")


class RenderError(Exception):
    """The formula can't be rendered."""


def parse_sums(text: str) -> dict[str, str]:
    """Maps file name → sha256 from `sha256sum` output."""
    sums = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        m = SUM_LINE.match(line.strip())
        if not m:
            raise RenderError(f"not a sha256sum line: {line!r}")
        sums[Path(m.group("name")).name] = m.group("sha")
    return sums


def render(template: str, version: str, sums: dict[str, str]) -> str:
    """Returns the formula for `version`, with the checksums of its archives."""
    version = version.removeprefix("v")
    if not VERSION.match(version):
        raise RenderError(f"not a version: {version!r}")
    values = {
        "version": version,
        "base_url": f"{REPOSITORY}/releases/download/v{version}",
    }
    for target in TARGETS:
        archive = f"ods-v{version}-{target}.tar.gz"
        if archive not in sums:
            raise RenderError(f"SHA256SUMS has no entry for {archive}")
        values["sha256_" + target.replace("-", "_")] = sums[archive]
    try:
        return string.Template(template).substitute(values)
    except (KeyError, ValueError) as e:
        raise RenderError(f"template placeholder without a value: {e}") from e


def self_test() -> None:
    """Renders the real template with made-up checksums and checks the failure modes."""
    template = TEMPLATE.read_text(encoding="utf-8")
    sums = {f"ods-v1.2.3-{t}.tar.gz": f"{i:x}" * 64 for i, t in enumerate(TARGETS, 1)}
    text = "\n".join(f"{sha}  {name}" for name, sha in sums.items())
    formula = render(template, "v1.2.3", parse_sums(text))
    assert 'version "1.2.3"' in formula
    assert "$" + "{" not in formula, "unfilled placeholder"
    assert f"{REPOSITORY}/releases/download/v1.2.3/ods-v1.2.3-aarch64-apple-darwin.tar.gz" in formula
    for sha in sums.values():
        assert f'sha256 "{sha}"' in formula
    assert "#{bin}/ods version" in formula, "Ruby interpolation must survive rendering"
    missing = dict(sums)
    missing.pop("ods-v1.2.3-x86_64-apple-darwin.tar.gz")
    for bad in [
        lambda: render(template, "1.2.3", missing),
        lambda: render(template, "latest", sums),
        lambda: parse_sums("not a checksum line"),
    ]:
        try:
            bad()
        except RenderError:
            pass
        else:
            raise AssertionError("should have failed")
    print("render-homebrew-formula self-test: ok", file=sys.stderr)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--version", help="X.Y.Z or vX.Y.Z")
    parser.add_argument("--sums", type=Path, help="SHA256SUMS covering the archives")
    parser.add_argument("--template", type=Path, default=TEMPLATE)
    parser.add_argument("--self-test", action="store_true", help="check this script")
    args = parser.parse_args(argv)
    if args.self_test:
        self_test()
        return 0
    if not args.version or not args.sums:
        parser.error("--version and --sums are required")
    try:
        formula = render(
            args.template.read_text(encoding="utf-8"),
            args.version,
            parse_sums(args.sums.read_text(encoding="utf-8")),
        )
    except (RenderError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    sys.stdout.write(formula)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
