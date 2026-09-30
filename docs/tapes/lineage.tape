# `ods lineage columns` and `ods lineage impact`: where a model's columns come from, and
# what a column change makes run.
Set Size 120x44
Set Title "ods lineage"
Output svg
Mask /\b\d+ ms\b/ "<n> ms"
Hide
Type "source $REPO/docs/tapes/setup.sh && dbt compile >/dev/null && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods lineage columns --model customers"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Screenshot lineage-columns
Type "clear"
Enter
Type "ods lineage impact --column stg_orders.status"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Screenshot lineage-impact
