#!/usr/bin/env python3
"""Copies the demo dbt project for a run against Databricks (#294).

    prepare-project.py DEST

`fixtures/dbt/jaffle-ods` is written for DuckDB, which the rest of the test suite runs
on. The copy differs only where DuckDB and Databricks do:

- The Python model and the model reading it are left out: it uses DuckDB's relation
  API, which a Databricks Python model (PySpark) doesn't have.
- Model contracts and constraints are left out: their data types (`integer`,
  `varchar`) and the foreign key's `main.orders` are DuckDB's. Data tests stay.
- `profiles.yml` points at the workspace. The token is read from the environment when
  dbt runs, never written to the file (AGENTS.md rule 9).

The catalog and schema come from `ODS_CI_CATALOG` and `ODS_CI_SCHEMA`. To rehearse
locally on another warehouse, point `ODS_CI_PROFILES_YML` at a `profiles.yml` to use
instead.
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "fixtures" / "dbt" / "jaffle-ods"
# Only DuckDB's Python relation API builds this one; `segment_summary` reads it.
LEFT_OUT = {"customer_segments", "segment_summary"}

PROFILE = """\
jaffle_ods:
  target: databricks
  outputs:
    databricks:
      type: databricks
      host: "{{ env_var('DATABRICKS_HOST') | replace('https://', '') | replace('/', '') }}"
      http_path: "{{ env_var('DATABRICKS_HTTP_PATH') }}"
      token: "{{ env_var('DATABRICKS_TOKEN') }}"
      catalog: "{{ env_var('ODS_CI_CATALOG') }}"
      schema: "{{ env_var('ODS_CI_SCHEMA') }}"
      threads: 4
"""


def main(dest: Path) -> None:
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(
        SOURCE,
        dest,
        # Only the project's sources: no committed artifacts, targets or logs.
        ignore=shutil.ignore_patterns(
            "target", "target-*", "artifacts", "logs", "*.duckdb", "profiles.yml"
        ),
    )
    for model in (dest / "models").rglob("*"):
        if model.stem in LEFT_OUT and model.suffix in (".py", ".sql"):
            model.unlink()

    schema = dest / "models" / "schema.yml"
    doc = yaml.safe_load(schema.read_text())
    models = []
    for model in doc.get("models", []):
        if model["name"] in LEFT_OUT:
            continue
        config = model.get("config", {})
        config.pop("contract", None)
        if not config:
            model.pop("config", None)
        model.pop("constraints", None)
        for column in model.get("columns", []):
            column.pop("constraints", None)
            column.pop("data_type", None)
        models.append(model)
    doc["models"] = models
    schema.write_text(yaml.safe_dump(doc, sort_keys=False))

    override = os.environ.get("ODS_CI_PROFILES_YML")
    profile = Path(override).read_text() if override else PROFILE
    (dest / "profiles.yml").write_text(profile)
    print(f"prepared {dest} from {SOURCE.relative_to(ROOT)}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    main(Path(sys.argv[1]))
