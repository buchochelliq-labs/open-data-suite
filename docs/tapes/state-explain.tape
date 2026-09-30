# `ods state explain`: why a node would be built, traced upstream to the root cause.
Set Size 110x30
Set Title "ods state explain customers"
Output svg
Mask /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/ "<run-id>"
Mask /\d{4}-\d\d-\d\d[T ]\d\d:\d\d(:\d\d(\.\d+)?)?Z?/ "<time>"
Hide
Type "source $REPO/docs/tapes/setup.sh && ods state build -q >/dev/null 2>&1 && ods_edit stg_orders && dbt compile >/dev/null && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods state explain customers"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Screenshot state-explain
