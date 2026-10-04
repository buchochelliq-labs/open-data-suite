#!/usr/bin/env python3
"""Render the Chocolatey package source for a release from its SHA256SUMS (#212).

    python3 scripts/render-chocolatey-package.py --version 0.1.0 --sums SHA256SUMS --out choco
    python3 scripts/render-chocolatey-package.py --self-test

Fills the templates in packaging/chocolatey/ with the version, the Windows archive's
download URL and its sha256, and writes `opendatasuite.nuspec` and
`tools/chocolateyinstall.ps1` under --out, ready for `choco pack`. Fails if the archive's
checksum is missing, so a package can't point at an asset the release doesn't have.

The templates use `${name}` placeholders only: PowerShell's own `$variables` are left
alone, and a `${name}` without a value is an error rather than left in the output.
"""
import argparse
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEMPLATES = ROOT / "packaging" / "chocolatey"
# Template (relative to TEMPLATES) → rendered file (relative to --out).
FILES = {
    "opendatasuite.nuspec.tmpl": "opendatasuite.nuspec",
    "tools/chocolateyinstall.ps1.tmpl": "tools/chocolateyinstall.ps1",
}
REPOSITORY = "https://github.com/buchochelliq-labs/open-data-suite"
TARGET = "x86_64-pc-windows-msvc"
VERSION = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
# `sha256sum` output: `<hex>  <name>` (text mode) or `<hex> *<name>` (binary mode).
SUM_LINE = re.compile(r"^(?P<sha>[0-9a-f]{64}) [ *](?P<name>\S+)$")
PLACEHOLDER = re.compile(r"\$\{(?P<name>[a-z0-9_]+)\}")


class RenderError(Exception):
    """The package can't be rendered."""


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


def values_for(version: str, sums: dict[str, str]) -> dict[str, str]:
    """The placeholder values for `version`."""
    version = version.removeprefix("v")
    if not VERSION.match(version):
        raise RenderError(f"not a version: {version!r}")
    archive = f"ods-v{version}-{TARGET}.zip"
    if archive not in sums:
        raise RenderError(f"SHA256SUMS has no entry for {archive}")
    return {
        "version": version,
        "url": f"{REPOSITORY}/releases/download/v{version}/{archive}",
        "sha256": sums[archive],
    }


def fill(template: str, values: dict[str, str]) -> str:
    """Replaces each `${name}` with its value; an unknown name is an error."""

    def one(m: re.Match) -> str:
        name = m.group("name")
        if name not in values:
            raise RenderError(f"template placeholder without a value: ${{{name}}}")
        return values[name]

    return PLACEHOLDER.sub(one, template)


def render(version: str, sums: dict[str, str], templates: Path = TEMPLATES) -> dict[str, str]:
    """Returns rendered file (relative path) → contents."""
    values = values_for(version, sums)
    return {
        out: fill((templates / src).read_text(encoding="utf-8"), values)
        for src, out in FILES.items()
    }


def write(files: dict[str, str], out: Path) -> None:
    """Writes the rendered files under `out`."""
    for rel, text in files.items():
        path = out / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")


def self_test() -> None:
    """Renders the real templates with a made-up checksum and checks the failure modes."""
    sha = "ab" * 32
    sums = parse_sums(f"{sha}  ods-v1.2.3-{TARGET}.zip\n{'cd' * 32} *ods-v1.2.3-x86_64-apple-darwin.tar.gz\n")
    files = render("v1.2.3", sums)
    nuspec, script = files["opendatasuite.nuspec"], files["tools/chocolateyinstall.ps1"]
    assert "<version>1.2.3</version>" in nuspec
    assert "<id>opendatasuite</id>" in nuspec
    assert f"{REPOSITORY}/blob/v1.2.3/LICENSE" in nuspec
    assert f"url64bit       = '{REPOSITORY}/releases/download/v1.2.3/ods-v1.2.3-{TARGET}.zip'" in script
    assert f"checksum64     = '{sha}'" in script
    for text in files.values():
        assert PLACEHOLDER.search(text) is None, "unfilled placeholder"
    assert "$toolsDir" in script and "$env:ChocolateyPackageName" in script, (
        "PowerShell variables must survive rendering"
    )
    import xml.etree.ElementTree as ET

    ET.fromstring(nuspec.encode("utf-8"))  # well-formed XML
    for bad in [
        lambda: render("1.2.3", {}),
        lambda: render("latest", sums),
        lambda: parse_sums("not a checksum line"),
        lambda: fill("${nope}", {}),
    ]:
        try:
            bad()
        except RenderError:
            pass
        else:
            raise AssertionError("should have failed")
    with tempfile.TemporaryDirectory() as tmp:
        write(files, Path(tmp))
        assert (Path(tmp) / "tools" / "chocolateyinstall.ps1").read_text(encoding="utf-8") == script
    print("render-chocolatey-package self-test: ok", file=sys.stderr)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--version", help="X.Y.Z or vX.Y.Z")
    parser.add_argument("--sums", type=Path, help="SHA256SUMS covering the Windows archive")
    parser.add_argument("--out", type=Path, help="directory to write the package source to")
    parser.add_argument("--self-test", action="store_true", help="check this script")
    args = parser.parse_args(argv)
    if args.self_test:
        self_test()
        return 0
    if not args.version or not args.sums or not args.out:
        parser.error("--version, --sums and --out are required")
    try:
        files = render(args.version, parse_sums(args.sums.read_text(encoding="utf-8")))
        write(files, args.out)
    except (RenderError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
