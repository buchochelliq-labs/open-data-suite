# ADR-0020: dbt state interop: export a state that `--favor-state` can trust, and import dbt runs

- **Status:** Proposed
- **Date:** 2026-09-28 (amended 2026-09-29 with the reproduction results, #296)
- **Issues:** #296 (related: #292, #293, #294, #227, #230)
- **Deciders:** @n1ckyb

## Context
Many teams develop against production with dbt's deferral:

```sh
dbt build --defer --favor-state --state prod/     # prod/ holds prod's manifest.json
# model b fails; model a, which b reads, built fine in this target
dbt retry
```

On the retry, `b` reads **prod's** `a`, not the `a` this target just built. The same
happens in any follow-up run with `--favor-state` that doesn't select `a`. Environments
mix silently. The results look fine and are wrong.

### How dbt resolves refs under deferral
This is our reading of dbt-core's OSS source (Apache-2.0, 1.x), which rule 8 allows. We
have not read dbt's v2 engine, and don't rely on it.

1. **Merging the state.** With `--defer`, dbt loads the state manifest (from
   `--defer-state`, else `--state`). `Manifest.merge_from_artifact` then gives every
   node that is in both manifests, is refable (a model, seed or snapshot) and isn't
   ephemeral, a `defer_relation`. It is built from the **state** node's `database`,
   `schema`, `alias` and `relation_name`.
2. **Resolving a ref.** `RuntimeRefResolver.create_relation` uses the
   `defer_relation` when deferral is on and either:
   - `--favor-state` is set and the referenced node is **not selected** in this
     invocation; or
   - the adapter's relation cache has no relation for the node in this target.

   Otherwise it uses this target's relation.
3. **`dbt retry`** reads the previous `run_results.json` (from `--state` when given,
   else the target path). It reruns the same command with the saved arguments, and
   selects only the nodes that errored, failed or were skipped. The nodes that
   succeeded are therefore unselected. With `--favor-state` still in the saved
   arguments, rule 2's first branch sends every ref to them to the state manifest.

So `--favor-state` does what it says. The problem is its input: the state manifest
doesn't know what this target has built since. ODS does. Every successful build is
recorded per target, with a target identity (ADR-0013, ADR-0017), and ODS can check
that a relation still exists (ADR-0016).

### What we were unsure of
The reproduction test (Decision §7) settles each of these before the export ships,
and the docs describe only what it shows. The first is settled: see "What the
reproduction showed" below.
- **Which flags `dbt retry` honours.** We believe it reuses the saved arguments and
  reads `run_results.json` from `--state`. We don't know whether `--defer-state` on
  the retry's command line overrides a saved `--state`, or whether the export must be
  passed as `--state`.
- **Which fields build the deferred relation.** We believe `Relation.create_from` uses
  `database`, `schema` and `alias`, and some adapters use `relation_name`. The export
  sets all four consistently, so either way works.
- **Whether dbt validates `manifest.json` strictly.** We believe it checks
  `dbt_schema_version` and then deserializes, ignoring unknown keys. We don't depend
  on that: the export adds no keys to dbt's files.
- **Whether `state:modified` looks at rendered relation names.** We believe it
  compares unrendered config, so rewriting rendered `database`/`schema`/`alias` doesn't
  change the selection. To be safe, the documented use keeps `--state prod/` for
  selection and passes the export as `--defer-state`, if the test shows that dbt
  honours it (see above).
- **"Selected" under `dbt build`.** We believe tests are selected resources too, but
  only refable nodes can be deferred, so this shouldn't matter.

### What the reproduction showed
`crates/ods-cli/tests/dbt_favor_state.rs` runs real dbt on DuckDB, in the `real-dbt`
CI job, on the fixture `fixtures/dbt/favor-state`. Model `a` records which target built
it, and `b` reads `a`. It gave the same results with dbt 1.11.15 and 1.12.5:
- **The problem is real.** `a` builds in dev and `b` fails. `dbt retry` then builds
  `b` on **prod's** `a`: its compiled SQL and its data both say prod.
- **`dbt retry --defer-state <dir>` works.** With `<dir>` holding prod's manifest in
  which `a` points at dev, the retry builds `b` on dev's `a`. dbt honours
  `--defer-state` over the original run's saved `--state`, and reads the previous run
  results from the target path as usual.
- **`dbt retry --state <dir>` doesn't.** With the run results copied into `<dir>`, the
  retry reads them from there, but still defers to the saved `--state` (prod). So the
  export doesn't carry run results, and the docs never suggest this form.
- **Later deferred runs work too.** `dbt build -s b --defer --favor-state --state prod/
  --defer-state <dir>` keeps prod for `state:modified` selection and resolves `a` to
  dev.
- Changing only `schema` and `relation_name` was enough on DuckDB. The export still
  sets all four relation fields, as §3 says.

### Constraints
- **Rule 8, clean-room:** only the public artifact schemas and dbt-core's OSS source.
  No artifact field is invented.
- **Rule 3, conservative:** when ODS can't prove a relation exists in this target,
  it must do what dbt would have done: point at the upstream state.
- **Rule 4, explainable:** every node's choice carries a reason.
- **Rule 9:** nothing secret is written.
- **Rule 1:** the decision is provider-neutral; dbt's file format lives in
  `ods-provider-dbt`.

## Options considered
### Option A — `--defer` without `--favor-state`
dbt then prefers this target's relation whenever one exists, so the retry reads the
`a` it just built.
- Pros: no ODS involvement; it works today.
- Cons: *any* relation in this target wins, however stale: a table built last week
  from other code is used instead of prod. That is why teams use `--favor-state`. It
  also relies on the adapter's relation cache. It stays documented as a workaround.

### Option B — Clone prod into this target first (`dbt clone`)
Clone every unselected relation from prod, then build without deferral.
- Pros: every ref resolves to this target; no manifest is rewritten.
- Cons: writes to the warehouse for every node, which is cheap only with zero-copy
  clones (the `zero_copy_clone` capability, #29, #30). `dbt clone` skips relations
  that already exist, so it has the same staleness problem as Option A. It
  complements the export later; it doesn't replace it.

### Option C — Patch the prod manifest in place
Rewrite `prod/manifest.json` so built nodes point at this target.
- Pros: no new directory; users keep their command lines.
- Cons: it corrupts an artifact other people and jobs read, including prod's own
  `state:modified` comparisons. Two developers would overwrite each other. There is no
  record of what changed. Rejected.

### Option D — `ods state export --dbt-state <dir>` (chosen)
Write a new state directory that points each node at the right place, and explain
every choice.
- Pros: dbt is unchanged, and so is the upstream artifact. The choice uses what ODS
  already records. It is explainable and conservative per node. dbt reads it with
  `--defer-state <dir>` on `dbt retry` and later deferred runs; a retry without that
  flag still defers to the original run's state and reads prod.
- Cons: one more command in the workflow, and a second copy of the manifest to keep in
  step. The directory is only as current as the last export.

## Decision
**`ods state export --dbt-state <dir> --upstream <state dir>` writes a dbt state
directory. It is the upstream `manifest.json` with one change: nodes that ODS recorded as
successfully built in this target, and can show still exist there, point at this
target's relation. Every other node keeps the upstream pointer. dbt reads it with
`--defer-state <dir>`, including on `dbt retry`. `ods state import --from <target dir>`
records a plain dbt run.**

### 1. The export command
```sh
ods state export --dbt-state .ods/dbt-state --upstream prod/ --target dev
dbt retry --defer-state .ods/dbt-state
# or any later deferred run: selection still compares with prod
dbt build -s b --defer --favor-state --state prod/ --defer-state .ods/dbt-state
```
- **Option names.** `--dbt PROGRAM` already means the dbt executable on every
  `ods state` command (`docs/cli.md`), and it keeps that meaning here. The export's
  directory is `--dbt-state <dir>`, named after dbt's `--state`. The import's source is
  `--from <target dir>`.
- `--upstream` names the directory holding the upstream `manifest.json`, e.g. prod's.
  It is required: ODS doesn't guess where prod is.
- The scope is the usual one (`--target`, `--environment`, ADR-0017).
- The upstream manifest must be for the same project (`metadata.project_name` and
  `metadata.project_id` match the current manifest's). Otherwise the export is refused.
- It never writes into the upstream directory.

**How the directory is refreshed.** Renaming a whole directory over a non-empty one
isn't atomic on any platform, and swapping a symlink may need admin rights on Windows.
So the directory stays put, and each file in it is replaced atomically:
1. ODS takes a lock file, `.ods-export.lock`, in the directory. A second export to the
   same directory waits, then fails. dbt never reads that file.
2. It writes every new file in full to a temporary file in the same directory (so on
   the same filesystem), and flushes it. A maintained crate does this (`tempfile`'s
   `persist`, already a dev-dependency).
3. It renames the files over the old ones in this order: `ods-export.json`, then
   **`manifest.json` last**. A rename is atomic on POSIX. On Windows, `MoveFileEx`
   with `REPLACE_EXISTING` replaces the file in one step, but fails while another
   process holds it open. ODS retries briefly, then fails and names the file.

What a dbt reader can see during a refresh:
- **No file is ever partly written.** dbt reads each file whole when it starts, so it
  gets either the old or the new version of each.
- **The manifest decides where refs resolve, and it changes last,** in a single step.
  A dbt run that starts during a refresh either uses the whole previous export or the
  whole new one.
- `ods-export.json` records the SHA-256 of the manifest it describes, so ODS (and the
  docs' troubleshooting) can tell when a manifest was written by another export.
- If any step fails, the export exits non-zero and says which files were replaced.
  Before step 3 nothing has changed. Rerunning the export repairs a partial refresh.
  The upstream directory and ODS state are never touched (rule 5).

### 2. The per-node rule
It applies to each node in the upstream manifest's `nodes`. First match wins. Each
outcome has a reason code.

| # | Condition | Points at | Reason |
|---|---|---|---|
| 1 | Not a model, seed or snapshot, or ephemeral: dbt never defers it | unchanged | `not_deferrable` |
| 2 | Not in this target's current manifest | upstream | `not_in_project` |
| 3 | The scope's latest snapshot is for another target, or names none (ADR-0017) | upstream | `target_changed` / `target_unknown` |
| 4 | No successful build recorded in the scope | upstream | `not_built_here` |
| 5 | The recorded fingerprint's `relation` component differs from the relation this target's manifest names | upstream | `relation_changed` |
| 6 | Any other fingerprint component differs: the build is of other code | upstream | `code_changed_since_build` |
| 7 | The relation check (ADR-0016) says it's missing | upstream | `relation_missing` |
| 8 | The check couldn't tell, wasn't run (`--no-check-relations`), or checked another relation than the manifest names | upstream | `relation_unverified` |
| 9 | Otherwise | this target | `built_here` (run id, build time) |

- "This target" means the `database`, `schema`, `alias` and `relation_name` of the node
  in the current manifest, compiled for this target. ODS doesn't store relation names;
  rule 5 ties them to the recorded build.
- Rule 6 is deliberate. When ODS can't vouch for a build, the export does what dbt
  would have done with `--favor-state`. It never swaps prod for a stale build.
- Nodes that exist only in this target aren't added: dbt gives them no
  `defer_relation`, so refs to them already resolve to this target.
- The relation check runs by default, through the `relation_inspector` contract and
  the `relation_existence` capability (ADR-0016). It is one dbt call for all nodes.
- `--output json` returns every node's choice, reason and evidence. Plain output
  summarises counts and lists the nodes that point at this target (ADR-0003).

### 3. What is written
- **`manifest.json`:** the upstream document, parsed as generic JSON. For nodes chosen
  by rule 9, and only for them, the values of `database`, `schema`, `alias` and
  `relation_name` are replaced. Nothing is added, removed or reordered on purpose,
  including `metadata`. A test diffs the output against the upstream and allows only
  those four fields to differ.
- **No `run_results.json`.** `dbt retry` reads the previous run's results from the
  target path, and defers to `--defer-state` (see "What the reproduction showed").
  `dbt retry --state <dir>` would read results from the export but still defer to the
  original run's state, so the export writes none. ODS never writes run results.
- **`ods-export.json`:** ODS's own sidecar, which dbt doesn't read. It holds
  `schema_version`, when and from which snapshot the export was made, the target
  identity's non-secret form (ADR-0017), the upstream manifest's `invocation_id`, the
  SHA-256 of the `manifest.json` it was written with, and each node's choice and
  reason.
- **Supported versions:** manifest **v12**, the version the
  fixtures in `fixtures/dbt/` use (dbt 1.10.23 and 2.0.5 both write manifest v12) and
  that dbt 1.11 and 1.12 in the `real-dbt` job write. The export keeps the upstream's
  `dbt_schema_version`, since it is the upstream's document. Other versions are
  refused with an error that names them. ODS *reads* manifest v11 too, but exporting
  one waits until a test covers it.
- Why no ODS fields inside `manifest.json`: `metadata.env` is typed as a string map,
  but dbt fills it from `DBT_ENV_CUSTOM_ENV_*` variables, and tools show it as such.
  Node `meta` is user data. Neither is ours to write, so the sidecar holds everything.

### 4. Versioning (ADR-0019)
- `manifest.json` is written for dbt to read. Its schema is dbt's, and it is versioned
  by dbt's `dbt_schema_version`. ODS doesn't version it.
- Supporting a new manifest version is **Added** in the changelog.
  Dropping one is **Breaking**.
- `ods-export.json` is an ODS persisted document: `schema_version` 1.0, with
  ADR-0019's rules (minor: optional fields only; major: through a reader that
  upgrades).
- The command's JSON output uses the output envelope's `schema_version` (ADR-0003).

### 5. What is never exported (rule 9)
- Nothing from `profiles.yml`, no resolved `env_var()` value, and no credential. ODS
  holds none of these (ADR-0005, ADR-0017).
- The target identity appears only in its cleaned form, as ADR-0017 shows it: never
  the raw location, which can carry a token.
- The export adds no values to dbt's files. What it copies is what the upstream
  artifact already holds, e.g. rendered SQL and `metadata.env`. dbt already scrubs
  `DBT_ENV_SECRET_*` values from its artifacts. The docs say to treat the export
  directory like the upstream state directory it came from.

### 6. Importing a plain dbt run
`ods state import --from <target dir>` records a `dbt build`, `run`, `seed`,
`snapshot` or `retry` made outside ODS.
- It is `ods state record` (ADR-0013) given a directory. It reads `manifest.json`,
  `run_results.json` and, if present, `sources.json`, with the same refusals: commands
  that build nothing, `--empty` runs, mismatched invocations, runs already recorded,
  and runs older than the recorded state.
- **Target identity:** with `--target`, ODS asks dbt for the target's identity as
  ADR-0017 does. It records it only if every node's relation in the imported manifest
  is the relation that target resolves to now. Otherwise it records no identity and
  warns, so the next export points everything upstream (`target_unknown`) and the next
  `ods state run` rebuilds once.
- The planner then uses the import like any recorded run.

### 7. Test plan
1. **Reproduce dbt's behaviour first** (real dbt, DuckDB, in the existing `real-dbt`
   job, for each dbt in its matrix):
   - A fixture project with `prod` and `dev` targets, in separate DuckDB databases.
     Model `a` writes a marker that differs by target; `b` reads `a` and can be made
     to fail with an environment variable (`dbt retry` replays the original vars).
   - Build prod and keep its `manifest.json`.
   - In dev: `dbt build --defer --favor-state --state prod/` with `b` failing, then fix
     it and `dbt retry`.
   - Assert that `b` read **prod's** `a` (its marker, and the relation in its compiled
     SQL). The test pins today's behaviour, so a dbt release that changes it is
     noticed.
   - The same tests settle the retry form: `dbt retry --defer-state <export>` reads
     dev's `a`, and `dbt retry --state <export>` doesn't.
   - **Done:** `crates/ods-cli/tests/dbt_favor_state.rs`, with the results under
     "What the reproduction showed".
2. **Then the fix:** the same scenario through `ods state build` (or
   `ods state import --from`), then `ods state export --dbt-state`, then
   `dbt retry --defer-state`.
   Assert that `b` read **dev's** `a`.
3. **Unit and snapshot tests** with no dbt:
   - each rule in §2, with a fake store and inspector;
   - the four-field diff against the fixture manifests;
   - refusal of other schema versions and of another project's manifest;
   - the refresh: file order, a failure before and during the renames, and a second
     export blocked by the lock;
   - `insta` snapshots of the JSON and plain output;
   - a check that no ODS key appears in `manifest.json`.
4. **Databricks:** the same scenario in the `databricks` workflow (#294), once dbt runs
   there.

## Consequences
- Positive:
  - `--favor-state` and `dbt retry` read what this target built, without changing dbt
    or the upstream artifact.
  - Every pointer is explained. Anything ODS can't vouch for behaves exactly as dbt
    would.
  - Teams can adopt ODS state from plain dbt runs (`import`), and use ODS's knowledge
    from plain dbt commands (`export`).
- Negative / trade-offs:
  - One more command before a deferred dbt run, and a second manifest to keep current.
    A stale export is still conservative: nodes built after it point upstream.
  - The fix depends on dbt internals that aren't a documented contract (§ "How dbt
    resolves refs"). The reproduction test is what tells us when they change.
  - The relation check costs one dbt call per export.
  - Only manifest v12 for the export (and run results v6 for the import), until tests
    cover more.
- Follow-up issues:
  - `ods state retry --failed` after an import. The last-run file holds an ODS
    command line, and a dbt invocation's saved arguments don't map to one exactly.
  - Docs: "Retrying with favor-state", with the before and after (#296).
  - Clone-based preparation (Option B) where `zero_copy_clone` is available (#29, #30).
  - Raise the retry behaviour with dbt upstream, with the reproduction as evidence.

## References
- #296; #292 / #293 (`ods state retry --failed`); #294 (Databricks CI).
- [ADR-0003](0003-cli-presentation-boundary.md) (output),
  [ADR-0011](0011-dbt-state-config-compatibility.md) (dbt State config),
  [ADR-0013](0013-state-snapshots-fingerprints-and-store.md) (snapshots, `record`),
  [ADR-0016](0016-relation-existence-before-reuse.md) (relation checks),
  [ADR-0017](0017-state-per-target.md) (state per target),
  [ADR-0019](0019-release-and-versioning.md) (versioning).
- dbt-core (Apache-2.0): `Manifest.merge_from_artifact` in
  `core/dbt/contracts/graph/manifest.py`; `RuntimeRefResolver.create_relation` in
  `core/dbt/context/providers.py`; the retry task in `core/dbt/task/retry.py`.
- dbt artifact schemas: `manifest` v12, `run-results` v6 (Apache-2.0).
- Fixtures: `fixtures/dbt/jaffle-ods/artifacts/`, `fixtures/dbt/jaffle-ods-state/artifacts/`.
