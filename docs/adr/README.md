# Architecture Decision Records

Use the `adr` skill (`.claude/skills/adr`) or copy [`0000-template.md`](0000-template.md).

| ADR | Title | Status | Issues |
|---|---|---|---|
| 0000 | Template | — | — |
| [0001](0001-monorepo-architecture-and-module-boundaries.md) | Monorepo architecture and module boundaries | Accepted | #1 |
| [0002](0002-rust-first-backend-and-technology-stack.md) | Rust-first backend and technology stack | Accepted | #105 |
| [0003](0003-cli-presentation-boundary.md) | CLI presentation boundary and rs-rich-cli | Accepted | #108 |
| [0004](0004-cli-framework-and-exit-codes.md) | CLI framework, module registration and exit codes | Accepted | #6 |
| [0005](0005-configuration-and-profiles.md) | Configuration and profiles | Accepted | #7 |
| [0006](0006-plugin-sdk-and-capabilities.md) | Plugin SDK, contracts and capabilities | Accepted | #2, #3 |
| [0008](0008-column-level-lineage.md) | Open, fast column-level lineage | Accepted | #73, #74, #31, #92 |
| [0009](0009-hostable-explorer-ods-web.md) | A hostable explorer (`ods-web`) and an EDGE layer | Accepted | #95, #96, #102, #107 |
| [0010](0010-mcp-server.md) | `ods mcp`, a read-only, local MCP server | Accepted | #169 |
| [0011](0011-dbt-state-config-compatibility.md) | Read dbt State configuration as-is | Accepted | #168, #19 |
| [0012](0012-erd-from-tests-and-constraints.md) | An ERD from tests and constraints, with evidence | Accepted | #60–#65, #169 |
| [0013](0013-state-snapshots-fingerprints-and-store.md) | State snapshots, fingerprints and the state store | Accepted | #11, #13, #16, #18, #20, #22, #25 |
| [0014](0014-executor-contract-and-state-run.md) | The Executor contract and `ods state run` | Accepted | #23, #24, #211 |
| [0015](0015-cli-compatibility-front-ends.md) | A dbt-shaped CLI, and compatibility front-ends for other tools' CLIs | Proposed | #224, #225, #226 |
| [0016](0016-relation-existence-before-reuse.md) | Check that a relation still exists before reusing it | Accepted | #230 |
| [0017](0017-state-per-target.md) | State per dbt target, with a non-secret target identity | Accepted | #227 |
| [0018](0018-state-store-migrations-and-recovery.md) | State store migrations, integrity and recovery | Accepted | #188 |
| [0019](0019-release-and-versioning.md) | Release and versioning strategy | Accepted | #101, #212 |
| [0020](0020-dbt-state-interop-and-favor-state.md) | dbt state interop: export a state that `--favor-state` can trust, and import dbt runs | Accepted | #296 |
| [0021](0021-databricks-authentication.md) | Databricks authentication: U2M first, M2M for CI, tokens never persisted | Proposed | #297 |
| [0022](0022-delta-table-versions-as-source-evidence.md) | Delta table versions as source change evidence, read through dbt | Accepted | #17, #16 |
| [0023](0023-ods-doctor-diagnostics.md) | `ods doctor`: typed health checks, stable codes and exit semantics | Accepted | #181 |
| [0024](0024-run-events-node-stats-and-run-journal.md) | Run events, per-node run stats and the run journal | Accepted | #322 |
| [0025](0025-error-explanations.md) | Explaining failed nodes: a neutral taxonomy, provider pattern catalogues and evidence joins | Accepted | #323 |
| [0026](0026-run-playback.md) | Run playback: replaying a run's journal on the Lineage page | Accepted | #322 (follow-up) |
| [0027](0027-federated-build-graph-across-project-formats.md) | A federated build graph across project formats (dbt, SQLMesh, …) | Proposed (exploratory) | #12, #86, #226 |

ADR-0007 (the clean-room and licensing policy, #10) is planned and not written yet; until it is, the rule is `AGENTS.md` rule 8, and `deny.toml` checks dependency licences. Each ADR's status line notes what of its decision isn't built yet.
