# Prepares the demo project the dashboard tours are recorded on: the same scratch copy
# as the terminal tapes (../setup.sh), with a few runs of the fake dbt recorded as state
# and journals, then a code change so the plan has something to build.
#
# Sourced by scripts/record-dashboard.py with REPO set and `ods` on PATH.

source "$REPO/docs/tapes/setup.sh"

# 1. Everything builds.
ods state build -q >/dev/null 2>&1
# 2. customers changes; customer_segments (a Python model) fails, so segment_summary is
#    skipped: a partial run, whose journal keeps the redacted error.
ods_edit customers
FAKE_DBT_FAIL=customer_segments \
FAKE_DBT_FAIL_MESSAGE=$'Runtime Error in model customer_segments (models/marts/customer_segments.py)\n  KeyError: \'lifetime_value\'' \
  ods state build -q >/dev/null 2>&1
# 3. The fix: only what failed is retried.
ods state retry --failed -q >/dev/null 2>&1
# 4. orders changes, and dbt compiles it: the plan now builds orders and what reads it.
ods_edit orders
dbt compile >/dev/null 2>&1
