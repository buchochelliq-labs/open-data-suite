# `ods doctor --project`: is the dbt project and its artifacts ready for ODS?
Set Size 110x27
Set Title "ods doctor"
Output svg
Mask /\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ/ "<time>"
Hide
Type "source $REPO/docs/tapes/setup.sh && dbt compile >/dev/null && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods doctor --project"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Screenshot doctor
