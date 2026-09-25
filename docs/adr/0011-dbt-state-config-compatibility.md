# ADR-0011: Read dbt State configuration as-is

- **Status:** Proposed
- **Date:** 2026-09-25
- **Issues:** #168, #19
- **Deciders:** @n1ckyb

(ADR-0009 is the hostable explorer, #166. ADR-0010 is reserved for the MCP server, #169.)

## Context
Teams that use dbt State (or SAO before it) already describe how fresh each model must
be:
- `state.lag_tolerance`;
- `state.require_fresh_data_from`;
- SAO's `freshness.build_after`;
- source `loaded_at_field` / `loaded_at_query`.

These can be set in `dbt_project.yml`, properties YAML, or SQL `config()`. To switch to
ODS, or to run both, those projects should need no edits.

We checked with the fixture `fixtures/dbt/jaffle-ods-state`: dbt writes the resolved
settings for each node into every format we read.
- **dbt 1.10 `manifest.json`:** `config.state` as written (`"4h"`) and
  `config.freshness`. Sources carry `loaded_at_*`.
- **dbt v2 `manifest.json`:** `lag_tolerance` rewritten to `{count, period}`.
- **dbt v2 Information Schema:** the same values, in the JSON `config` column. Sources
  also have `loaded_at_*` columns.

The two versions resolve the block differently:
- **dbt v2 merges it key by key.** A model setting only `lag_tolerance` inherits
  `require_fresh_data_from` from its folder.
- **dbt 1.x replaces the whole block.** A more specific block discards a less specific
  one.

## Options considered
- **Parse `dbt_project.yml` and YAML ourselves.** This would give exact provenance
  (which file). But it re-implements dbt's config resolution, including Jinja, which is
  fragile and duplicates dbt.
- **Read the resolved per-node config from the artifacts (chosen).** This behaves
  exactly like the dbt version the user runs. The cost: provenance is "dbt config", not
  a file and line.
- **Require ODS-specific config.** This forces users to maintain two configs; rejected.

## Decision
- **Neutral types.** `ods-core::freshness` defines `FreshnessPolicy`
  (`lag_tolerance_secs`, a `Quorum` of any/all, a `PolicyOrigin`, unapplied and unknown
  settings) and `LoadedAt`. The planner sees only these (rule 1).
- **Reading.** `ods-provider-dbt::state_config::resolve` reads the resolved config for
  every model and snapshot, and `loaded_at_*` for sources. The resolution is used as
  given: no re-merging.
- **Precedence** follows dbt: `state.*`, then SAO `freshness.build_after` for any keys
  still unset, then defaults.
- **Defaults (decided with the maintainer).**
  - If any node configures `state:` or `build_after`, the project relies on dbt State,
    so unset nodes get dbt State's defaults: `45m` and `any`.
  - Otherwise the tolerance is `0`: rebuild on any new data (rule 3).
- **Settings understood but not applied yet:**
  - `compare_unrendered_code` and `pre_clone` are ignored safely (ODS rebuilds more).
  - `evaluate_volatile_sql: true` and `execute_hooks_on_any_reuse: true` would make
    reuse unsafe, so those nodes are never reused.
- **Unknown keys and unreadable values** are reported, and the node is never reused.
- **`ods state policies`** shows every node's policy with its origin, and how sources
  report new data (rule 4). The other `ods state` subcommands remain planned.

## Consequences
- Positive:
  - dbt State projects work unchanged;
  - behaviour matches the user's dbt version;
  - every decision can be explained;
  - new dbt keys never cause unsafe reuse.
- Negative / trade-offs:
  - Provenance is per setting, not per file.
  - The same YAML behaves differently on dbt 1.x and v2 (dbt's own behaviour, and
    documented).
  - A later dbt 1.x release could warn about `state` as a custom config key.
- Follow-up:
  - apply `evaluate_volatile_sql` in fingerprinting (#13);
  - honour hooks on reuse in the executor (#23);
  - `pre_clone` with clone strategies (#29, #30);
  - file-level provenance, if users ask for it.

## References
- dbt's public documentation of State configuration.
- Fixture: `fixtures/dbt/jaffle-ods-state/README.md`.
