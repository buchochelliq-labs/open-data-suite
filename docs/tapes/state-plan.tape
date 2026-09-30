# `ods state plan`: first with nothing recorded, then after a code change, with each
# node's BUILD or REUSE and the reason.
Set Size 120x40
Set Title "ods state plan"
Output png svg
Mask /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/ "<run-id>"
Hide
Type "source $REPO/docs/tapes/setup.sh && dbt compile >/dev/null && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods state plan"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Sleep 300ms
Screenshot state-plan-first
Hide
Type "ods state build -q >/dev/null 2>&1 && ods_edit stg_orders && dbt compile >/dev/null && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods state plan"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Sleep 300ms
Screenshot state-plan-after-change
