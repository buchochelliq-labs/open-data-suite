# ADR-0025: Explaining failed nodes: a neutral taxonomy, provider pattern catalogues and evidence joins

- **Status:** Proposed
- **Date:** 2026-09-30
- **Issues:** #323 (explain dbt errors), #322 (run events and journal), #320 (values removed from messages)
- **Deciders:** @n1ckyb

## Context
When a node fails, people get dbt's (or the warehouse's) text: a kind, an adapter
message, a compiled path, and no hint of what changed. ODS knows things the engine
doesn't: what changed since the last successful build (the plan and fingerprints),
which columns each model reads and produces (column lineage, [ADR-0008](0008-column-level-lineage.md)),
the project's macros and files (the manifest), the versions of upstream data
([ADR-0022](0022-delta-table-versions-as-source-evidence.md)), and how each node did in
earlier runs (the run journal, [ADR-0024](0024-run-events-node-stats-and-run-journal.md)).
#323 asks for a plain-language explanation of each failed node, built on that evidence,
in the terminal, the JSON output and the dashboard (board 9 of the live-run canvas).

The constraints:
- **No vendor logic in core** (AGENTS.md rule 1): core can't match dbt's, DuckDB's or
  Databricks' text. Whether a provider can explain its engine's errors is a capability
  ([ADR-0006](0006-plugin-sdk-and-capabilities.md)).
- **Conservative** (rule 3): an error ODS doesn't recognise must not get a guessed
  cause, and inference must not read as fact.
- **Explainability** (rule 4): each explanation carries its reasons and their sources,
  as text and JSON.
- **Presentation apart from logic** (rule 7): view models in the CLI and the web layer.
- **Clean room** (rule 8): patterns come from public text: dbt-core's messages, the
  adapters' documented errors, Apache Spark's and Delta Lake's error classes.
- **Secrets** (rule 9): engine messages quote SQL and values. ADR-0024 keeps only a
  redacted summary; explanations must not bring anything back.

## Options considered
### Option A: classify in core with regular expressions over the raw message
Pros: one place. Cons: vendor text in core (rule 1); reads the raw message, which
holds values (rule 9); every adapter change is a core change.

### Option B: an LLM reads the message and explains it
Pros: covers everything. Cons: guesses (rule 3), needs the raw text (rule 9) and a
key, and isn't deterministic or testable. Kept for later (M6, "Ask the ODS agent"),
clearly labelled, grounded in the same evidence.

### Option C: a neutral model in core, pattern catalogues in providers, evidence joins in a module (chosen)
Providers classify the redacted summary into a neutral taxonomy; a pure function in
`ods-state` joins that classification with ODS's own evidence and derives how sure it
is. Pros: rule 1 and rule 9 hold by construction; deterministic; each pattern is tested
on a recorded message. Cons: coverage grows one pattern at a time; unrecognised errors
stay unexplained (by design).

## Decision
### The model (`ods_core::failure`)
An `ErrorExplanation` (schema version 1.0) has: the node; a **category**; a
**symptom** when a pattern recognised the error; the **confidence**; the pattern that
matched (catalogue, version, id); a **headline** and a sentence under it; the
**evidence** (each item with its source and whether it *confirms* the pattern); the
**location**; **suggestions** (text and commands to copy); the **impact** (blocked
nodes, and which keep an earlier build); and the engine's own (redacted) message.

**Categories** (the chip on a failed node): `compilation`, `dependency` (dependency or
ref), `database` (database or SQL), `permission`, `timeout` (timeout or lock),
`python_model`, `test_failure`, `configuration` (configuration or profile),
`internal`, `unknown`.

**Symptoms** (what a recognised error means, whatever engine said it):
`missing_column`, `missing_relation`, `unknown_macro`, `missing_ref`,
`template_syntax`, `packages_missing`, `permission_denied`, `type_mismatch`,
`constraint_violation`, `dependent_objects`, `query_timeout`, `lock_conflict`,
`warehouse_unavailable`, `profile_not_found`, `credentials_missing`,
`python_exception`, `test_failed`. Each has a category by default and a neutral
headline.

**Confidence**, exactly as designed:
- `known_pattern_with_evidence`: a pattern recognised it and ODS's own evidence
  confirms it;
- `known_pattern`: a pattern recognised it, nothing of ODS's confirms it;
- `not_recognised`: no pattern did.

The confidence is **derived, never set**: only `ExplanationBuilder::build` makes an
explanation, and it computes the confidence from whether a pattern matched and whether
an evidence item *confirms*. An unrecognised error gets no symptom, no confirming
evidence and its category's neutral headline ("The warehouse rejected the query"),
whatever the caller passes: what ODS knows is shown as context ("What ODS knows"), never
as a cause. Its detail and suggestions are the caller's (the joins below only add the
fixed "ODS doesn't recognise this error…" sentence and generic steps for it). An
explanation read back from JSON has its confidence derived again, so edited JSON
can't claim more.

**Text is narrow.** Explanation text is fixed wording, numbers and *code spans*
(backticks in JSON) that only hold ASCII letters, digits and `_ . - /`, at most 200
characters; anything else (so `:`, `@`, `=`, `+`, `$`: URLs with passwords,
`key=value`) reads `[name hidden]`. A command is a fixed template of the caller's own
(which may hold `--flag`, `=` and `&&`, never `$`) whose placeholders take arguments
of the same narrow characters; any other command is dropped. The node id and the
engine's name are checked the same way. The names come from ODS's own evidence (the
project's node, column and macro names, run ids, paths), never from the engine's
message, whose only trace is its redacted summary. This narrows what can get in; it
isn't a proof: a long token made only of those characters could still pass as a name,
which is why names never come from an engine's text. Evidence items may also carry
their facts as data (`data`: the missing columns, or the undefined macros) for machines
(rule 4).

### The provider contract (`ods_sdk::contracts::error_catalogue`, 0.1)
A provider with the new **`error_explain` capability** implements `ErrorCatalogue`:
- `catalogue()`: its name, version and the engine's name (who "said" a message);
- `classify(&ErrorSummary) -> Classification`: `Recognised(PatternMatch)` (a stable
  pattern id, the symptom, the category, the engine's own steps, and optionally an
  identifier-shaped subject such as a Python exception's type), or
  `NotRecognised { category }` with only the category the engine's kind implies.

`classify` reads only the `ErrorSummary` of ADR-0024 (kind and redacted first line),
never the raw message or SQL, so a pattern can neither match on nor leak a value. It is
pure and deterministic. The catalogue's **version** changes with any pattern, and every
explanation names the catalogue and version that made it.

A provider also describes the project for explanations as a `ProjectIndex`: each node's
source and compiled files, its language, the macros its code calls that the project and
its packages don't define (with their lines), and every macro the project defines. The
dbt provider counts a call only when it is a plain name that isn't a macro or part of
dbt's documented Jinja context (`ref`, `set`, `zip_strict`, `load_relation`, …), or a
dotted name whose namespace is a package that defines macros; a method on a value
(`cols.append(…)`) says nothing about macros. A Python exception is named only when it
is one of Python's built-in exceptions.

`ErrorSummary` gains one optional field, `line`: the line the engine reported in the
code it ran (e.g. DuckDB's `LINE 25:`). A line number isn't a value; the SQL echo line
itself is still never kept. Older journals read as before.

The **fake** (`ods-provider-fake::FakeErrorCatalogue`) and the **conformance suite**
(`ods_sdk::conformance::error_catalogue`, five cases) come with the contract: the
catalogue advertises the capability; the provider's recorded samples classify as
declared; classification is deterministic; unknown text is never recognised, and
without a kind the category is `unknown`; and no classification quotes anything, with
a sentinel from the raw message never coming back.

### The dbt catalogue (`ods-provider-dbt::error_catalogue`, version 1)
Patterns over the summary's kind and lowercased message, each grounded in public text
and, for dbt and DuckDB, recorded from a real dbt 1.10 + DuckDB run
(`fixtures/dbt/jaffle-ods/capture-errors.sh`, `artifacts/dbt-1.10-errors/`):
- dbt-core: an undefined macro (`'x' is undefined. This can happen when calling a macro
  that does not exist`), a ref to a missing node, packages not installed, a missing
  profile or target, Jinja syntax errors, a Python model's failure (the exception type
  and its line are kept from the traceback's last lines), a failed test;
- DuckDB: missing columns (`Binder Error`), missing tables and views (`Catalog Error`),
  conversion, constraint, dependent entries, write-write conflicts, file locks,
  permission and interrupt errors;
- PostgreSQL's documented messages (missing column or relation, permission denied,
  statement timeout, invalid input, unique and not-null violations, dependent objects,
  password authentication, connection);
- Apache Spark's public error conditions (`UNRESOLVED_COLUMN`,
  `TABLE_OR_VIEW_NOT_FOUND`, `CAST_INVALID_INPUT`, `DATATYPE_MISMATCH`) and Delta Lake's
  `DELTA_CONCURRENT_*` classes, which Databricks reports.

Anything else is not recognised. A missing scalar function (DuckDB's `Scalar Function
with name … does not exist`) is deliberately left unrecognised: it isn't a macro, and
ODS has no evidence to say more.

### Evidence joins (`ods_state::explain_failure`)
A pure, synchronous function of `FailureFacts`: the classification, the node's
summary and stats, the run, the plan, the state committed before the run, earlier runs
(newest first), the project index, and column lineage facts. Evidence **confirms** a
pattern only when it shows the same thing independently:
- **missing column** ← column lineage: the node reads a column its upstream no longer
  produces, and the upstream's column copied from an input of that name (the rename:
  "`stg_customers` now outputs `given_name`, not `first_name`"). `ods-lineage` answers
  this (`ColumnGraph::missing_columns`); the SQL analyzer now names the columns it
  couldn't resolve against a known table (`QueryLineage::unresolved`, never a column
  alias of the select list) instead of only giving up. It never guesses: an upstream
  whose columns aren't known says nothing. It confirms only with **exactly one**
  candidate whose upstream was rebuilt successfully in this run, or whose committed
  build is the code there is now; several candidates, or one from a stale upstream,
  are listed as context;
- **undefined macro** ← the project index: **exactly one** call in the node's code
  isn't among the project's macros; with several, they are listed as context and
  none is named in the headline. Did-you-mean from macros within two edits, and then
  first, before installing packages;
- **missing relation** ← state and run: an upstream that has never been built and
  didn't build in this run.

Context for any failure, recognised or not: how long it ran and in which phase; whether
its code changed since its last successful build (plan and fingerprints); whether an
upstream changed in this run and was rebuilt; new upstream data since its last build,
with the versions; and its history ("it failed the same way in 2 of its last 5 runs",
"it built fine in its last N runs"). **Impact:** the nodes that the engine said it
blocked, or skipped nodes it said nothing about that the plan puts downstream; and which
of them keep their last good build (rule 5).

**Suggestions** are real commands only: the ones in ODS's CLI reference
(`ods lineage impact --column <upstream unique id>.<column>=removed`, which resolves
whatever the names; `ods state retry --failed`, with `--state-db` when the run used a
state database from a flag or the environment, and only for the last run, which is
what it retries; `ods doctor`) and the engine's own from the provider (`dbt deps`,
`dbt debug`). `ods lineage impact --column …=removed` now accepts a column that is
already gone when some node still reads it, so the suggestion works after the failure.

### Computed on read, never stored
Explanations are computed when shown, from what ODS keeps anyway (the journal, the
state, the manifest, lineage), never written into the journal or the state. Older runs
are explained with the current catalogue, and a better catalogue explains them better.
Right after a run, the plan it used is known; for `ods state history --run`, a plan is
rebuilt from the states before and after the run (what the run changed), and the
project as it is now provides the manifest and lineage. That evidence confirms only
when the manifest was written by that run (dbt's invocation id is the run id); for any
other run it is context, worded "as the project is now", since the code may have
changed since. A command that fails before
any node runs (e.g. `dbt compile` in the prepare step) is explained from the error dbt
printed, found by its header, with the manifest that command wrote. Only errors from
that step are: once dbt started building, an error isn't shown as "nothing was built",
and its hint names the run's journal instead.

### Surfaces
- **Terminal:** `ods state run`, `build` and `test` show "Why it failed" for each failed
  node (headline, category, confidence, evidence with its source, where, impact, what to
  try with commands, and dbt's redacted message); `ods state history --run` too. A
  command that failed before running shows the explanation with outcome
  `failed_before_running`.
- **JSON:** a `failures` array of `ErrorExplanation`s in the same results.
- **Dashboard:** the Run page's Nodes tab and the Runs side panel render the same model
  as board 9 (#323 part 2).

```mermaid
graph LR
  dbt[dbt / warehouse message] --> summary[ErrorSummary, redacted - ADR-0024]
  summary --> catalogue[ods-provider-dbt: ErrorCatalogue]
  catalogue -->|Classification| join[ods-state: explain_failure]
  plan[plan + fingerprints] --> join
  state[state before the run] --> join
  journal[earlier journals] --> join
  index[ProjectIndex from the manifest] --> join
  lineage[ods-lineage: missing columns] --> join
  join --> explanation[ods-core: ErrorExplanation]
  explanation --> cli[ods-cli: terminal and JSON]
  explanation --> web[ods-web: Run page]
```

## Consequences
- Positive:
  - A failed node says what went wrong, why ODS thinks so and what to try, in the same
    words in every surface, and never more than the evidence supports.
  - Vendor text stays in providers; a new adapter adds patterns and a fixture, not core
    code.
  - Old runs benefit from new patterns, since nothing is stored.
- Negative / trade-offs:
  - `ods-state` now depends on `ods-sdk` (for the run events and catalogue types it
    reads), as modules may (ADR-0001).
  - Coverage is only as good as the catalogue; many errors stay `not_recognised` until
    someone records them.
  - Explaining a past run uses the project as it is now: evidence from the manifest or
    lineage may describe a later version of the code. The text says "now" where it
    matters, and a rebuilt plan says only what the states show.
  - The reported line is a line of the code the engine ran (which adapters wrap), not
    of the source file; ODS shows it as such and maps back to the source only where it
    knows the line itself (an undefined macro's call).
- Follow-up issues:
  - Patterns for more adapters (Snowflake, BigQuery, Databricks SQL warehouses) with
    recorded fixtures. (dbt 1.11 and 1.12 are now recorded as well as 1.10, and every
    recorded message is checked on each: 1.12 rewords the missing-packages error, which
    the catalogue's version 2 recognises.)
  - Test failures explained per check (which test, on which column, failing rows).
  - `ods doctor` checks as evidence for configuration errors.
  - "Ask the ODS agent to investigate" (M6) and an MCP tool `explain_failure`.

## References
- #323, #322, #320; [ADR-0006](0006-plugin-sdk-and-capabilities.md),
  [ADR-0008](0008-column-level-lineage.md), [ADR-0013](0013-state-snapshots-fingerprints-and-store.md),
  [ADR-0024](0024-run-events-node-stats-and-run-journal.md).
- The design: `docs/design/dashboard/README.md`, "Failed node: the error explained
  (#323)", and `docs/design/dashboard/boards/live-run/ErrorExplained.dc.html`.
- dbt-core's public error messages; DuckDB's error types; PostgreSQL's error messages;
  Apache Spark's `error-conditions.json` and Delta Lake's `delta-error-classes.json`.
