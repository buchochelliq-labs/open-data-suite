# `ods state build` after a code change: live per-node progress, then the run report
# with each node's result, time taken, rows and why it ran.
Set Size 120x62
Set Title "ods state build"
Set Caption "After an edit to stg_orders: 8 nodes build, 5 are reused"
Set TypingDelay 30ms
Output png svg gif
# dbt's log lines carry the wall-clock time; run ids and the run's own duration vary.
Mask /\b\d\d:\d\d:\d\d\b/ "HH:MM:SS"
Mask /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/ "<run-id>"
Mask /(totals\s+)\d+(\.\d+)?m?s/ "$1<took>"
Hide
Type "source $REPO/docs/tapes/setup.sh && ods state build -q >/dev/null 2>&1 && ods_edit stg_orders && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods state build"
Enter
Wait /recorded/ 30s
Sleep 300ms
Screenshot state-build
