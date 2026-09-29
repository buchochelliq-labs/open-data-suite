# ADR-0017: State per dbt target, with a non-secret target identity

- **Status:** Proposed
- **Date:** 2026-09-26
- **Issues:** #227
- **Deciders:** @n1ckyb

## Context
State is scoped by project and `--environment` (ADR-0013), which defaulted to
`default`, not by the dbt target builds go to. Fingerprints include relation names,
so most target switches rebuild anyway. But two targets with the same catalog and
schema on another host or account shared state. So did a profile whose default
target changed. ODS could reuse a build that never happened in the target being run,
which breaks AGENTS.md rule 3.

Knowing the target means reading the profile. That needs care:
- `profiles.yml` holds credentials (rule 9);
- it resolves `env_var()` in Jinja;
- and ODS has no YAML parser.

## Options considered
### Option A: parse `profiles.yml`
Rejected. It needs a new dependency and a Jinja renderer. It would read a file full
of secrets, and could still disagree with what dbt resolves.

### Option B: ask dbt (chosen)
Run one `dbt compile --inline` of the non-secret fields of dbt's `target` (the
fields `dbt debug` shows), rendered with `tojson`:
- `name`;
- `profile_name`;
- `type`;
- the location: the first of `host`, `account`, `server`, `path`, `project`;
- the first of `database`, `catalog`, `dbname`.

It resolves the profile exactly as the build will. It names no secret field, and
never renders the whole `target`. It passes `--no-populate-cache --no-introspect`,
since no warehouse metadata is needed.

A location can still carry a credential: `user:pw@host`, `?password=…`, or
MotherDuck's `md:db?motherduck_token=…`. So the query itself renders only:
- a cleaned form for people, with the query string, fragment, `;` options and
  anything up to an `@` removed;
- and `local_md5` of the raw value, which tells targets apart.

The raw value is never rendered, so it reaches neither ODS nor dbt's logs and
compiled output. ODS cleans the shown form again, in case a dbt renders it
differently.

### Option C: identity only, environment stays `default`
Switching between targets with `--target` would then rebuild everything on every
switch.

## Decision
1. **`--environment` defaults to the dbt target name** when one is given (`--target`,
   or `DBT_TARGET` as its default, ADR-0015). Otherwise it is `default`, as before.
   An explicit `--environment` wins. `--target` is on every State command, including
   `plan`, `record` and `history`, which don't run dbt, so all of them agree on the
   scope.
2. **Every snapshot records a `TargetIdentity`**:
   - name, profile, kind, location and database;
   - it is neutral data in `ods-core`;
   - `StateSnapshot.target` is optional and additive, so the state schema goes from
     1.0 to 1.1, and 1.0 documents still read.
   
   Commands that run dbt (`run`, `seed`, `snapshot`, `build`, `compile`, `test`) ask
   dbt for it after compiling. `ods state record` records none: the previous
   snapshot's target is no evidence of where a dbt build ODS didn't run went, so the
   next run rebuilds once.
3. **A build is only reused in the target it went to.**
   - When the latest snapshot's identity differs from the current one, or is missing,
     nothing in it is reused. Each node is BUILT with the new reason `target_changed`,
     and the run warns with both identities.
   - The next snapshot still follows it, so history and the compare-and-swap are
     unchanged. It holds only this run's builds, under the new identity.
   - `ods state test` refuses to test builds made in another target, or in one it
     can't tell (exit 1).
   - `ods state plan` doesn't run dbt, so it can't ask. It shows the recorded target
     as not checked. It plans state that names another target than `--target` as
     `run` would: nothing in it is reused. State that doesn't name a target (from
     `ods state record`) is planned as recorded, with a note: the offline
     `record` + `plan` workflow never has one. `ods state run` still rebuilds it once.
4. **The target check is required to record.** If dbt can't render the profile, a run
   that would record stops, since every other dbt command would fail the same way.
   A dry run or `ods state compile` records nothing, so it warns and plans as if
   nothing were built.

## Consequences
- Positive:
  - State recorded for one target is never reused for another: another host or
    account, another database, another profile, or a changed default target.
  - `--target prod` and `--target dev` each keep their own state.
  - No credential is read or stored, and no dependency is added.
- Negative / trade-offs:
  - One more dbt invocation per run (a parse, which partial parsing keeps quick).
  - Everything builds once after the upgrade: 1.0 snapshots have no identity. This is
    also true after a run recorded only with `ods state record` into an empty scope.
  - Without `--target`, the scope is `default` whatever the profile's default target
    is. Changing that default target is still caught by the identity, but as a
    rebuild, not as a separate scope.
  - Older ODS versions refuse 1.1 snapshots.
- Identity is only as good as the fields adapters expose. An adapter without any of
  the location fields (e.g. one configured only by region) is told apart by name,
  profile, type and database alone. The schema is left out, since fingerprints
  include relation names.
- Follow-up issues: the check is an inherent method of the dbt executor, not an SDK
  contract with a fake and a conformance suite. It becomes one when a second engine
  needs it.

## References
- #227; [ADR-0013](0013-state-snapshots-fingerprints-and-store.md) (scopes,
  snapshots); [ADR-0014](0014-executor-contract-and-state-run.md) (`ods state run`);
  [ADR-0015](0015-cli-compatibility-front-ends.md) (`DBT_*` defaults).
