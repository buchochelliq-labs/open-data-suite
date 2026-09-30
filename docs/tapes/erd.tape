# `ods erd generate`: keys and relationships from tests, constraints and joins, as a
# Mermaid erDiagram.
Set Size 110x40
Set Title "ods erd generate"
Output svg
Hide
Type "source $REPO/docs/tapes/setup.sh && dbt compile >/dev/null && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods erd generate"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Screenshot erd
