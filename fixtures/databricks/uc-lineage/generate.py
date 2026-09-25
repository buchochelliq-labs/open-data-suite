#!/usr/bin/env python3
"""Writes a *synthetic* export of Unity Catalog's `system.access.column_lineage` for the
`jaffle-ods` fixture, in the table's public schema (a subset of its columns, in its order).

It is not captured from a workspace: it is built from `ods lineage graph` output for
`fixtures/dbt/jaffle-ods/artifacts/dbt-1.10`, then edited to exercise every case a real
export has. Regenerate with:

    ods lineage graph --target-dir fixtures/dbt/jaffle-ods/artifacts/dbt-1.10 \
        --format json --output-file /tmp/g.json
    python3 fixtures/databricks/uc-lineage/generate.py /tmp/g.json

Cases:
- every model's direct edges are observed, except `order_events` (never ran);
- `customer_order_rank.order_seq` <- `orders.order_date` is observed although ODS reports
  it as a window (indirect) input: platforms report such columns differently;
- `customers.full_name` <- `stg_customers.email` is observed but not predicted: a
  deliberate disagreement for the `misses` case;
- a table outside the project (`jaffle_ods.reporting.revenue`), a read with no target
  (a SELECT), and a file-path source, which are all skipped or counted separately;
- mixed-case names, which UC doesn't produce but readers must tolerate.
"""
import csv
import json
import sys
from pathlib import Path

HEADER = [
    "account_id", "metastore_id", "workspace_id", "entity_type", "entity_id",
    "source_table_full_name", "source_table_catalog", "source_table_schema",
    "source_table_name", "source_path", "source_type", "source_column_name",
    "target_table_full_name", "target_table_catalog", "target_table_schema",
    "target_table_name", "target_path", "target_type", "target_column_name",
    "event_time", "event_date", "statement_id",
]


def row(source, source_column, target, target_column, n, source_path="", source_type="TABLE"):
    s = source.split(".") if source else ["", "", ""]
    t = target.split(".") if target else ["", "", ""]
    return {
        "account_id": "00000000-0000-0000-0000-000000000000",
        "metastore_id": "11111111-1111-1111-1111-111111111111",
        "workspace_id": "1234567890",
        "entity_type": "JOB",
        "entity_id": "42",
        "source_table_full_name": source,
        "source_table_catalog": s[0], "source_table_schema": s[1], "source_table_name": s[2],
        "source_path": source_path, "source_type": source_type if source or source_path else "",
        "source_column_name": source_column,
        "target_table_full_name": target,
        "target_table_catalog": t[0], "target_table_schema": t[1], "target_table_name": t[2],
        "target_path": "", "target_type": "TABLE" if target else "",
        "target_column_name": target_column,
        "event_time": f"2026-09-{1 + n % 20:02d}T06:{n % 60:02d}:00.000Z",
        "event_date": f"2026-09-{1 + n % 20:02d}",
        "statement_id": f"stmt-{n:04d}",
    }


def main(graph_path):
    graph = json.loads(Path(graph_path).read_text())
    relation = {n["id"]: n["relation"] for n in graph["nodes"]}
    rows = []
    for edge in graph["column_edges"]:
        target = edge["to"]["node"]
        if target.endswith(".order_events") or edge["kind"]["type"] != "direct":
            continue
        rows.append((relation[edge["from"]["node"]], edge["from"]["column"],
                     relation[target], edge["to"]["column"]))
    rows += [
        ("jaffle_ods.main.orders", "order_date", "jaffle_ods.main.customer_order_rank", "order_seq"),
        ("jaffle_ods.main.stg_customers", "email", "jaffle_ods.main.customers", "full_name"),
        ("jaffle_ods.main.orders", "amount", "jaffle_ods.reporting.revenue", "amount"),
        ("jaffle_ods.main.orders", "status", "", ""),
        ("JAFFLE_ODS.Main.Orders", "Amount", "jaffle_ods.main.customer_order_rank", "AMOUNT"),
    ]
    rows.sort()
    out = csv.DictWriter(sys.stdout, HEADER, lineterminator="\n")
    out.writeheader()
    for n, (source, source_column, target, target_column) in enumerate(rows):
        out.writerow(row(source, source_column, target, target_column, n))
    out.writerow(row("", "raw", "jaffle_ods.main.stg_orders", "status", len(rows),
                     source_path="s3://landing/orders/", source_type="PATH"))


if __name__ == "__main__":
    main(sys.argv[1])
