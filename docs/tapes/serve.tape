# `ods serve`: the dashboard's startup line (the pages themselves are in the dashboard
# design, docs/design/dashboard/).
Set Size 110x12
Set Title "ods serve"
Output svg
Mask /\b\d+ ms\b/ "<n> ms"
Hide
Type "source $REPO/docs/tapes/setup.sh && ods state build -q >/dev/null 2>&1 && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods serve --port 18765"
Enter
Wait /18765/
Sleep 500ms
Screenshot serve
Ctrl+C
