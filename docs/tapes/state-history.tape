# `ods state history`: every recorded snapshot with its run's time and rows, then one
# run's per-node stats from its journal with `--run`.
Set Size 150x40
Set Title "ods state history"
Output png svg
Mask /\d{4}-\d\d-\d\d[T ]\d\d:\d\d(:\d\d(\.\d+)?)?Z?/ "<time>"
Mask /\b\d\d:\d\d:\d\d\b/ "HH:MM:SS"
Mask /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/ "<run-id>"
Mask /\b\d+(\.\d+)?m?s\b/ "<took>"
Mask /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f-]*…/ "<run-id>…"
# A run's duration comes from dbt's event times, so its column's width varies: compare
# the tables with their rules and padding collapsed (the images keep them).
Mask /[━─]+/ "─"
Mask / {2,}/ " "
Hide
Type "source $REPO/docs/tapes/setup.sh && ods state build -q >/dev/null 2>&1 && ods_edit stg_orders && ods state build -q >/dev/null 2>&1; run=$(ods state history --json | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"result\"][\"snapshots\"][0][\"run_id\"])') && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods state history"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Screenshot state-history
Type "ods state history --run $run"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Screenshot state-history-run
