# A failing `ods state build`: the successes are recorded, the failure is explained with
# its error redacted (#323), and `ods state retry --failed` builds only what failed or
# was skipped.
Set Size 120x50
Set Title "ods state build, then ods state retry --failed"
Set TypingDelay 30ms
Output png svg html
Mask /\b\d\d:\d\d:\d\d\b/ "HH:MM:SS"
Mask /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/ "<run-id>"
Mask /(run )[0-9a-f]{8}([.,:)\s])/ "$1<run>$2"
Mask /(totals\s+)\d+(\.\d+)?m?s/ "$1<took>"
Hide
Type "source $REPO/docs/tapes/setup.sh && ods state build -q >/dev/null 2>&1 && ods_edit customers && export FAKE_DBT_FAIL=customer_segments FAKE_DBT_FAIL_MESSAGE=$'Runtime Error in model customer_segments (models/marts/customer_segments.py)\\n  KeyError: \\'lifetime_value\\'' && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods state build -q"
Enter
Wait /ODS-E0404/ 30s
Sleep 300ms
Screenshot state-build-failed
Hide
Type "unset FAKE_DBT_FAIL && clear"
Enter
Wait /(?:^|\n)❯ *(?:\n|$)/
Show
Type "ods state retry --failed -q"
Enter
Wait /recorded/ 30s
Sleep 300ms
Screenshot state-retry-failed
