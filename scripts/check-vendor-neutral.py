#!/usr/bin/env python3
"""Enforce AGENTS.md rule 1: no vendor or runtime names in core code (#3, ADR-0006).

Core, foundation, SDK and module crates must choose behaviour from capabilities, never
from which warehouse or runtime is in use. This scans their non-test Rust source for
vendor names outside comments and fails if any appear. Provider crates (`providers/`)
and the composition root (`ods-cli`) may name vendors.

Test code is exempt: everything from a `#[cfg(test)]` line to the end of the file (the
convention for unit-test modules), and crate `tests/` directories.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NEUTRAL_CRATES = [
    "ods-core", "ods-events", "ods-config", "ods-policy", "ods-sdk",
    "ods-state", "ods-erd", "ods-usage", "ods-ci", "ods-lsp", "ods-agent",
    "ods-mesh", "ods-synthetic",
]
VENDORS = [
    "dbt", "databricks", "unity_catalog", "unitycatalog", "snowflake", "bigquery",
    "redshift", "postgres", "postgresql", "duckdb", "sqlite", "mysql", "synapse",
    "fabric", "spark", "trino", "clickhouse", "oracle", "openai", "anthropic",
]
PATTERN = re.compile(r"(?<![A-Za-z0-9])(" + "|".join(VENDORS) + r")(?![A-Za-z0-9])", re.IGNORECASE)


def code_without_comments(line: str) -> str:
    """Drops `//` comments (including doc comments) that are not inside a string."""
    in_string = False
    escaped = False
    for i, c in enumerate(line):
        if in_string:
            if escaped:
                escaped = False
            elif c == "\\":
                escaped = True
            elif c == '"':
                in_string = False
        elif c == '"':
            in_string = True
        elif line.startswith("//", i):
            return line[:i]
    return line


def main() -> int:
    findings = []
    for crate in NEUTRAL_CRATES:
        src = ROOT / "crates" / crate / "src"
        if not src.is_dir():
            continue
        for path in sorted(src.rglob("*.rs")):
            in_block_comment = False
            for number, line in enumerate(path.read_text().splitlines(), start=1):
                if line.strip().startswith("#[cfg(test)]"):
                    break
                if in_block_comment:
                    if "*/" in line:
                        in_block_comment = False
                    continue
                if line.strip().startswith("/*"):
                    in_block_comment = "*/" not in line
                    continue
                code = code_without_comments(line)
                for match in PATTERN.finditer(code):
                    rel = path.relative_to(ROOT)
                    findings.append(f"{rel}:{number}: `{match.group(1)}` in core code: {line.strip()}")
    for finding in findings:
        print(f"vendor-neutral: {finding}", file=sys.stderr)
    if findings:
        print("vendor-neutral: express the difference as a Capability (ADR-0006) instead", file=sys.stderr)
        return 1
    print(f"vendor-neutral: ok ({len(NEUTRAL_CRATES)} crate names checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
