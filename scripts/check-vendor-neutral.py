#!/usr/bin/env python3
"""Enforce AGENTS.md rule 1: no vendor or runtime names in core code (#3, ADR-0006).

Core, foundation, SDK and module crates must choose behaviour from capabilities, never
from which warehouse or runtime is in use. This scans their non-test Rust source and
fails if a vendor name appears in code or in a string literal. Provider crates
(`providers/`) and the composition root (`ods-cli`) may name vendors.

How it reads Rust:
- Comments (`//`, `/* */`, nested, doc comments) are ignored; string, raw string and
  char literals are scanned, since `kind == "databricks"` is the case to catch.
- Items marked `#[cfg(test)]` (usually `mod tests { … }`) are skipped, and so are crate
  `tests/` directories. Nothing else is exempt.
- Identifiers and words are split on `_`, case changes and digits, and runs of up to
  three adjacent parts are joined, so `DatabricksClient`, `dbtManifest`, `BigQuery`,
  `unity_catalog` and `PostgreSQL` all match.

Run with `--self-test` to check the scanner itself.
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
# Written without separators; parts are joined before matching. Common English words
# (oracle, fabric) are left out to avoid false positives.
VENDORS = {
    "dbt", "databricks", "unitycatalog", "snowflake", "bigquery", "redshift",
    "postgres", "postgresql", "duckdb", "sqlite", "mysql", "synapse", "spark", "pyspark",
    "trino", "clickhouse", "openai", "anthropic",
}
WORD = re.compile(r"[A-Za-z0-9_]+")
PART = re.compile(r"[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|[0-9]+")
MAX_JOIN = 3
RAW_STRING = re.compile(r'b?r(#*)"')
CHAR = re.compile(r"'(\\.[^']*|[^\\'])'")


def mask(source: str) -> tuple[str, str]:
    """Returns (text, kinds): `text` has comments blanked (newlines kept) and `kinds`
    marks each character as code `c`, string contents `s` or blank ` `."""
    text, kinds = [], []
    i, n = 0, len(source)

    def emit(chunk: str, kind: str) -> None:
        text.append(chunk)
        kinds.append("".join("\n" if ch == "\n" else kind for ch in chunk))

    def blank(chunk: str) -> None:
        emit("".join("\n" if ch == "\n" else " " for ch in chunk), " ")

    while i < n:
        c = source[i]
        if source.startswith("//", i):
            end = source.find("\n", i)
            end = n if end < 0 else end
            blank(source[i:end])
            i = end
        elif source.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if source.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif source.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(source[i:j])
            i = j
        elif raw := RAW_STRING.match(source, i):
            if i and (source[i - 1].isalnum() or source[i - 1] == "_"):
                emit(c, "c")
                i += 1
                continue
            close = '"' + raw.group(1)
            start = raw.end()
            end = source.find(close, start)
            end = n if end < 0 else end
            emit(source[i:start], "c")
            emit(source[start:end], "s")
            emit(source[end:end + len(close)], "c")
            i = end + len(close)
        elif c == '"':
            j = i + 1
            while j < n and source[j] != '"':
                j += 2 if source[j] == "\\" else 1
            emit('"', "c")
            emit(source[i + 1:j], "s")
            emit(source[j:j + 1], "c")
            i = j + 1
        elif c == "'" and (lit := CHAR.match(source, i)):
            # A char literal; a lone `'` is a lifetime or label and stays code.
            emit(lit.group(0), "s")
            i = lit.end()
        else:
            emit(c, "c")
            i += 1
    return "".join(text), "".join(kinds)


def skip_test_items(text: str, kinds: str) -> str:
    """Blanks every item that follows a `#[cfg(test)]` attribute."""
    out = list(text)
    for attr in re.finditer(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]", text):
        if kinds[attr.start()] != "c":
            continue
        depth, j = 0, attr.end()
        while j < len(text):
            if kinds[j] == "c":
                if text[j] == "{":
                    depth += 1
                elif text[j] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                elif text[j] == ";" and depth == 0:
                    break
            j += 1
        for k in range(attr.start(), min(j + 1, len(out))):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def vendor_in(word: str) -> str | None:
    parts = [p.lower() for p in PART.findall(word)]
    for size in range(1, MAX_JOIN + 1):
        for start in range(len(parts) - size + 1):
            joined = "".join(parts[start:start + size])
            if joined in VENDORS:
                return joined
    return None


def scan(source: str) -> list[tuple[int, str]]:
    """(line number, vendor) for each vendor name in non-test code."""
    text, kinds = mask(source)
    text = skip_test_items(text, kinds)
    findings = []
    for number, line in enumerate(text.splitlines(), start=1):
        for word in WORD.finditer(line):
            if vendor := vendor_in(word.group(0)):
                findings.append((number, vendor))
    return findings


def self_test() -> int:
    flagged = {
        "camel type": "struct DatabricksClient;",
        "camel field": "let dbtManifest = 1;",
        "joined parts": "fn read_big_query() {}",
        "acronym": 'const X: &str = "PostgreSQL";',
        "string compare": 'if kind == "bigquery" {}',
        "code after block comment": '/* note */ fn c() { kind == "postgres" }',
        "after a cfg(test) helper": '#[cfg(test)]\nfn helper() {}\nfn prod() { kind == "trino" }',
        "code after a multi-line string with //": 'let s = "a\n // b"; let k = "trino";',
        "raw string": 'let s = r#"snowflake"#;',
    }
    clean = {
        "line comment": "// databricks is handled by providers",
        "doc comment": "/// e.g. Databricks",
        "mid-line block comment": "let x = 1; /* databricks */ let y = 2;",
        "nested block comment": "/* outer /* databricks */ still comment */",
        "test module": '#[cfg(test)]\nmod tests {\n    fn t() { "duckdb"; }\n}',
        "common words": 'let oracle = "fabric"; let sparkline = 1;',
        "lifetimes": "fn f<'a>(x: &'a str) -> &'a str { x }",
        "char literals": "let q = '\"'; let r = '{';",
    }
    failures = []
    for name, source in flagged.items():
        if not scan(source):
            failures.append(f"not flagged: {name}")
    for name, source in clean.items():
        if found := scan(source):
            failures.append(f"false positive: {name}: {found}")
    for failure in failures:
        print(f"vendor-neutral self-test: {failure}", file=sys.stderr)
    if failures:
        return 1
    print(f"vendor-neutral self-test: ok ({len(flagged) + len(clean)} cases)")
    return 0


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    findings = []
    for crate in NEUTRAL_CRATES:
        src = ROOT / "crates" / crate / "src"
        if not src.is_dir():
            continue
        for path in sorted(src.rglob("*.rs")):
            source = path.read_text()
            lines = source.splitlines()
            for number, vendor in scan(source):
                rel = path.relative_to(ROOT)
                findings.append(f"{rel}:{number}: `{vendor}` in core code: {lines[number - 1].strip()}")
    for finding in findings:
        print(f"vendor-neutral: {finding}", file=sys.stderr)
    if findings:
        print("vendor-neutral: express the difference as a Capability (ADR-0006) instead", file=sys.stderr)
        return 1
    print(f"vendor-neutral: ok ({len(NEUTRAL_CRATES)} crate names checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
