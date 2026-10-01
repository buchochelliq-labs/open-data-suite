"""A simulated run for the live run view (#322): writes a run's journal the way
`ods state build` does, one event at a time, so `ods serve` streams it.

Used by the browser tests of the live view (crates/ods-web/tests/browser/live_view.py)
and by the `live` dashboard tour (scripts/record-dashboard.py). It writes only what the
journal format (ADR-0024) holds, and no values: it is a test tool, not part of ODS.

    journal = Journal(state_db, "7f2f6c69-…", "jaffle_ods/default")
    journal.started(["model.jaffle_ods.orders"])
    journal.node_started("model.jaffle_ods.orders")
    journal.node_finished("model.jaffle_ods.orders", "success", rows=99)
    journal.finished("succeeded")

`BOARD` is the design board's demo run (docs/design/dashboard/boards/live-run/Main):
two threads, a failure and a skipped node, as a script of timed steps.
"""

from __future__ import annotations

import json
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path


def _iso(at: datetime) -> str:
    return at.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.") + f"{at.microsecond // 1000:03d}Z"


class Journal:
    """`<state-db>.runs/<run_id>.jsonl`, appended and flushed an event at a time."""

    def __init__(self, state_db: Path, run_id: str, scope: str | None, clock=None):
        self.dir = Path(str(state_db) + ".runs")
        self.dir.mkdir(parents=True, exist_ok=True)
        self.path = self.dir / f"{run_id}.jsonl"
        self.run_id = run_id
        self.scope = scope
        # Times in the events: real ones, or a fixed clock for repeatable pages.
        self.clock = clock or (lambda: datetime.now(timezone.utc))
        self.started_at: dict[str, datetime] = {}
        # Lines written, as the stream numbers them.
        self.lines = 0
        self.file = open(self.path, "x", encoding="utf-8")

    def close(self) -> None:
        self.file.close()

    def remove(self) -> None:
        self.close()
        self.path.unlink(missing_ok=True)

    def raw(self, text: str) -> None:
        """Writes `text` as it is: a torn line, or a line of another version."""
        self.file.write(text)
        self.file.flush()
        self.lines += text.count("\n")

    def event(self, kind: str, **fields) -> None:
        line = {
            "schema_version": {"major": 1, "minor": 0},
            "run_id": self.run_id,
            "scope": self.scope,
            "at": _iso(self.clock()),
            "kind": kind,
            **fields,
        }
        self.raw(json.dumps(line) + "\n")

    def started(self, nodes: list[str], mode: str = "build") -> None:
        self.event("run_started", nodes=nodes, mode=mode, live=True)
        for node in nodes:
            self.event("node_queued", node=node)

    def node_started(self, node: str, thread: int = 1) -> None:
        self.started_at[node] = self.clock()
        self.event("node_started", node=node, thread=f"Thread-{thread} (worker)")

    def node_finished(self, node: str, status: str, rows: int | None = None, compile_ms: int | None = None,
                      error: dict | None = None, blocked_by: list[str] | None = None,
                      adapter: dict | None = None, thread: int | None = None) -> None:
        end = self.clock()
        start = self.started_at.get(node)
        stats = {
            "status": status,
            "started_at": _iso(start) if start else None,
            "finished_at": _iso(end) if start else None,
            "duration_ms": round((end - start).total_seconds() * 1000) if start else None,
            "compile_ms": compile_ms,
            "execute_ms": max(0, round((end - start).total_seconds() * 1000) - compile_ms) if start and compile_ms is not None else None,
            "rows_affected": rows,
            "thread": f"Thread-{thread} (worker)" if thread else None,
            "error": error,
            "tests": None,
        }
        if adapter:
            stats["adapter"] = adapter
        if blocked_by:
            stats["blocked_by"] = sorted(blocked_by)
        self.event("node_finished", node=node, stats=stats)

    def check(self, check: str, covers: list[str], status: str) -> None:
        self.event("check_finished", check=check, covers=covers, status=status)

    def finished(self, outcome: str) -> None:
        self.event("run_finished", outcome=outcome)


# ------------------------------------------------------------------ the board's run

M = "model.jaffle_ods."
ERROR = {
    "kind": "KeyError",
    "message": "KeyError: [value removed]",
    "details_at": "dbt's log file (logs/dbt.log in the project, unless --log-path)",
}
# (seconds from the start, what happens): the Main board's NODES, as events.
BOARD: list[tuple[float, str, dict]] = [
    (0.0, "start", {"nodes": [M + n for n in ["stg_orders", "orders", "customers", "customer_order_rank",
                                               "customer_segments", "customers_snapshot_view", "segment_summary"]]}),
    (0.4, "node_started", {"node": M + "stg_orders", "thread": 1}),
    (2.3, "node_finished", {"node": M + "stg_orders", "status": "success", "compile_ms": 300, "thread": 1}),
    (2.4, "node_started", {"node": M + "orders", "thread": 1}),
    (6.6, "node_finished", {"node": M + "orders", "status": "success", "rows": 99, "compile_ms": 400, "thread": 1,
                            "adapter": {"query_id": "01b2-c3"}}),
    (6.7, "node_started", {"node": M + "customers", "thread": 1}),
    (6.7, "node_started", {"node": M + "customer_order_rank", "thread": 2}),
    (8.6, "node_finished", {"node": M + "customer_order_rank", "status": "success", "rows": 99, "compile_ms": 200, "thread": 2}),
    (9.4, "node_finished", {"node": M + "customers", "status": "success", "rows": 100, "compile_ms": 300, "thread": 1}),
    (9.5, "node_started", {"node": M + "customer_segments", "thread": 1}),
    (9.5, "node_started", {"node": M + "customers_snapshot_view", "thread": 2}),
    (10.8, "node_finished", {"node": M + "customers_snapshot_view", "status": "success", "compile_ms": 200, "thread": 2}),
    (13.1, "node_finished", {"node": M + "customer_segments", "status": "error", "compile_ms": 200, "thread": 1, "error": ERROR}),
    (13.15, "node_finished", {"node": M + "segment_summary", "status": "skipped", "blocked_by": [M + "customer_segments"]}),
    (13.3, "finish", {"outcome": "failed"}),
]


def apply(journal: Journal, action: str, args: dict) -> None:
    if action == "start":
        journal.started(args["nodes"])
    elif action == "node_started":
        journal.node_started(args["node"], args.get("thread", 1))
    elif action == "node_finished":
        journal.node_finished(**args)
    elif action == "finish":
        journal.finished(args["outcome"])
    else:
        raise ValueError(action)


def play(journal: Journal, script=BOARD, until: float | None = None, since: float = -1.0,
         speed: float = 1.0, after=None) -> float:
    """Writes the script's steps after `since` up to `until` (seconds from the run's
    start), waiting between them at `speed`; `speed=0` writes them at once. `after(t)`
    is called after each step. Each event
    carries its time in the script, from when the run started (the first call sets it:
    now, or at `speed=0` as long ago as `until`, so a node started at 6.7 s and shown at
    9 s has run for 2.3 s). Returns the time of the last step written."""
    now = datetime.now(timezone.utc)
    if not hasattr(journal, "t0"):
        journal.t0 = now - timedelta(seconds=(until or 0.0) if speed == 0 else 0.0)
    journal.clock = lambda: journal.now
    last = since
    for t, action, args in script:
        if t <= since:
            continue
        if until is not None and t > until:
            break
        if speed > 0 and last >= 0:
            time.sleep(max(0.0, (t - last) / speed))
        journal.now = journal.t0 + timedelta(seconds=t)
        apply(journal, action, args)
        last = t
        if after:
            after(t)
    return last
