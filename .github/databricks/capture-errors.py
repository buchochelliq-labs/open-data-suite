#!/usr/bin/env python3
"""Captures real Databricks error messages for the dbt error catalogue's tests (#349).

    capture-errors.py OUT.json

Run by the `databricks` workflow, with dbt-databricks's `dbt` on the PATH (whose own
Python runs this, for PyYAML) and the workspace settings `demo.sh` needs. Each scenario
breaks a copy of the demo project in one way, runs dbt on just what it needs, and keeps
the message dbt reported for the failed node, as `capture-errors.sh` does for DuckDB:

- missing-column, unresolved-column: a column that doesn't exist (`[UNRESOLVED_COLUMN…]`);
- missing-relation: a table that doesn't exist (`[TABLE_OR_VIEW_NOT_FOUND]`);
- missing-schema: a schema that doesn't exist (`[SCHEMA_NOT_FOUND]`);
- missing-function: a SQL function that doesn't exist (`[UNRESOLVED_ROUTINE]`);
- type-mismatch: a cast that can't be made, with a secret sentinel as the value
  (`[CAST_INVALID_INPUT]`);
- datatype-mismatch: an operator on types it doesn't take (`[DATATYPE_MISMATCH…]`);
- not-null-constraint, check-constraint: a model contract's constraints violated by
  its rows (Delta's constraint errors);
- test-failure: a singular test that returns rows.

The catalog, the run's schema and the workspace host become `<catalog>`, `<schema>` and
`<host>`; no message may hold the host or a token afterwards. The messages are written
to OUT.json and printed between `ods-errors-begin` and `ods-errors-end` markers, so they
can be read from the job's log and committed as
`fixtures/dbt/jaffle-ods/artifacts/dbt-databricks-errors/errors.json`.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SENTINEL = "sk_live_SENTINEL_42"

# A contract on its own model, so the demo's DuckDB types stay out of it.
CONSTRAINED = """\
version: 2
models:
  - name: {name}
    config:
      contract: {{enforced: true}}
    columns:
      - name: id
        data_type: int
        constraints:
{constraint}
"""

# name, dbt command, models to select, files to write (path -> text), edits
# (path -> (old, new)).
SCENARIOS = [
    (
        "missing-column",
        "run",
        "stg_customers customers",
        {},
        {"models/staging/stg_customers.sql": ("    first_name,", "    first_name as given_name,")},
    ),
    (
        "unresolved-column",
        "run",
        "stg_orders broken",
        {"models/marts/broken.sql": "select no_such_column from {{ ref('stg_orders') }}\n"},
        {},
    ),
    (
        "missing-relation",
        "run",
        "broken",
        {"models/marts/broken.sql": "select * from {{ target.catalog }}.{{ target.schema }}.no_such_table\n"},
        {},
    ),
    (
        "missing-schema",
        "run",
        "broken",
        {"models/marts/broken.sql": "select * from {{ target.catalog }}.ods_no_such_schema.no_such_table\n"},
        {},
    ),
    (
        "missing-function",
        "run",
        "broken",
        {"models/marts/broken.sql": "select no_such_function(1) as x\n"},
        {},
    ),
    (
        "type-mismatch",
        "run",
        "broken",
        {"models/marts/broken.sql": f"select cast('{SENTINEL}' as int) as id\n"},
        {},
    ),
    (
        "datatype-mismatch",
        "run",
        "broken",
        {"models/marts/broken.sql": "select array(1) + 1 as x\n"},
        {},
    ),
    (
        "not-null-constraint",
        "run",
        "broken",
        {
            "models/marts/broken.sql": "select cast(null as int) as id\n",
            "models/marts/broken.yml": CONSTRAINED.format(
                name="broken", constraint="          - type: not_null"
            ),
        },
        {},
    ),
    (
        "check-constraint",
        "run",
        "broken",
        {
            "models/marts/broken.sql": "select -1 as id\n",
            "models/marts/broken.yml": CONSTRAINED.format(
                name="broken",
                constraint="          - type: check\n            name: positive_id\n            expression: id > 0",
            ),
        },
        {},
    ),
    (
        "test-failure",
        "build",
        "stg_orders only_placed_orders",
        {"tests/only_placed_orders.sql": "select * from {{ ref('stg_orders') }} where status <> 'placed'\n"},
        {},
    ),
]


def run_dbt(project: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["dbt", *args, "--profiles-dir", ".", "--log-format", "json"],
        cwd=project,
        capture_output=True,
        text=True,
        check=False,
    )


def redact(text: str, project: Path) -> str:
    for value, name in (
        (os.environ["ODS_CI_CATALOG"], "<catalog>"),
        (os.environ["ODS_CI_SCHEMA"], "<schema>"),
        (os.environ["DATABRICKS_HOST"].removeprefix("https://").strip("/"), "<host>"),
        (str(project), "<project_root>"),
    ):
        if value:
            text = text.replace(value, name)
    return text


def failed(project: Path, invocation_from: str) -> list[dict]:
    """The failed nodes' messages in this invocation's run_results.json."""
    results = project / "target" / "run_results.json"
    if not results.exists():
        return []
    run = json.loads(results.read_text())
    invocation = None
    for line in invocation_from.splitlines():
        if line.startswith("{"):
            invocation = json.loads(line)["info"].get("invocation_id")
            break
    if run.get("metadata", {}).get("invocation_id") != invocation:
        return []
    return [
        {"node": r["unique_id"], "message": r["message"]}
        for r in run["results"]
        if r["status"] in ("error", "fail") and r.get("message")
    ]


def main(out: Path) -> None:
    for var in ("DATABRICKS_HOST", "DATABRICKS_TOKEN", "ODS_CI_CATALOG", "ODS_CI_SCHEMA"):
        if not os.environ.get(var):
            sys.exit(f"capture-errors.py: {var} is not set")
    work = Path(os.environ.get("RUNNER_TEMP", "/tmp")) / "jaffle-errors"
    base = work / "base"
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    subprocess.run(
        [sys.executable, str(ROOT / ".github/databricks/prepare-project.py"), str(base)],
        check=True,
    )
    # The seeds the scenarios read, once.
    seeded = run_dbt(base, "seed")
    if seeded.returncode != 0:
        print(seeded.stdout[-4000:], seeded.stderr[-4000:], sep="\n")
        sys.exit("capture-errors.py: dbt seed failed")

    rows = []
    for name, command, select, files, edits in SCENARIOS:
        project = work / name
        shutil.copytree(base, project, ignore=shutil.ignore_patterns("target", "logs"))
        for path, text in files.items():
            (project / path).parent.mkdir(parents=True, exist_ok=True)
            (project / path).write_text(text)
        for path, (old, new) in edits.items():
            file = project / path
            source = file.read_text()
            if old not in source:
                sys.exit(f"capture-errors.py: {name}: `{old}` isn't in {path}")
            file.write_text(source.replace(old, new))
        print(f"::group::{name}: dbt {command} --select {select}")
        done = run_dbt(project, command, "--select", *select.split())
        found = failed(project, done.stdout)
        print(f"exit {done.returncode}; {len(found)} failed node(s)")
        print("::endgroup::")
        if not found:
            print(f"::warning::{name}: dbt reported no failed node; nothing recorded")
        for f in found:
            rows.append(
                {
                    "name": name,
                    "command": f"dbt {command}",
                    "node": f["node"],
                    "message": redact(f["message"], project),
                }
            )

    text = json.dumps(rows, indent=2, sort_keys=True) + "\n"
    host = os.environ["DATABRICKS_HOST"].removeprefix("https://").strip("/")
    token = os.environ["DATABRICKS_TOKEN"]
    for secret in (host, token):
        if secret and secret in text:
            sys.exit("capture-errors.py: a message still holds the host or a token")
    if re.search(r"dapi[0-9a-f]{20,}", text):
        sys.exit("capture-errors.py: a message holds something like an access token")
    out.write_text(text)
    print("ods-errors-begin databricks")
    print(text, end="")
    print("ods-errors-end databricks")
    print(f"recorded {len(rows)} message(s) from {len(SCENARIOS)} scenarios")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    main(Path(sys.argv[1]))
