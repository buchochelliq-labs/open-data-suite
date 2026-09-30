# `ods -h`: every command, and what is available today.
Set Size 120x40
Set Title "ods -h"
Output svg
Hide
Type "source $REPO/docs/tapes/setup.sh"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods -h"
Enter
Wait /-V, --version/
Sleep 200ms
Screenshot help
