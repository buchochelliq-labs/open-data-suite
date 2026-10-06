# OpenDataSuite (ODS) — Roadmap, Milestones & Release Plan

Status: **proposed** · Last updated: 2026-10-04

> **Plans, not promises.** Target dates are proposals for a small team and are revisited
> at the end of each milestone. Scope, order and dates will change.

This document turns the initial backlog (issues #1–#157) into an ordered set of
milestones and releases. It is the planning source of truth until the vision
document (#103) lands; milestone membership is mirrored in
[`.github/milestones.json`](https://github.com/buchochelliq-labs/open-data-suite/blob/main/.github/milestones.json) and applied to GitHub with
[`scripts/sync-milestones.sh`](https://github.com/buchochelliq-labs/open-data-suite/blob/main/scripts/sync-milestones.sh).

## 1. Product thesis (one paragraph)

ODS is a **Rust-first, provider-neutral control plane for analytics engineering**.
A shared semantic core (`ods-core`) and a versioned plugin SDK power independently
useful modules — **State** (incremental, explainable "what needs to run"),
**ERD**, **Usage**, **CI**, **LSP**, **Agent**, **Mesh** and **Synthetic**. dbt is
the first project format and Databricks/Unity Catalog the first warehouse, but
neither is allowed to leak into core. Every decision ODS makes must be
**explainable with evidence**, and unknown evidence always resolves to the
**conservative** action (BUILD, deny, flag as inferred).

## 2. Guiding principles (release gates)

Every milestone must preserve these; reviewers reject PRs that break them.

1. **Core is provider-neutral** — no `if warehouse == "databricks"` in core; use capabilities (#3).
2. **Contracts before implementations** — providers depend on the SDK (#2), never the reverse.
3. **Conservative by default** — insufficient evidence ⇒ BUILD / fail closed (#9, #20).
4. **Explainability** — every plan/finding carries a reason chain and evidence (#21, #43).
5. **Failed runs never replace canonical state** (#11, #24).
6. **Machine-readable output everywhere** — human, `--json`, `--plain` (#6, #108).
7. **Public formats only** — interoperate through public/OSS dbt contracts; no proprietary code or binaries (#10, #67).
8. **ERD ≠ lineage** — separate domain types (#5).

## 3. Release train

| Release | Theme | Milestone(s) | Target | Exit criteria (summary) |
|---|---|---|---|---|
| pre-release (internal) | Foundations | M0 | 2026-10-30 | Workspace builds in CI; ADRs for architecture, Rust stack, CLI presentation; plugin SDK + semantic core compile with fake providers; demo dbt fixture project. |
| **v0.0.1** — *first public release* | **ODS State MVP (code- and data-aware) + first dashboard screens** | M1 | **2026-10-30** | `ods state plan / run / explain / diff / history` on a dbt project using `manifest.json`, SQLite state, exact-selection dbt executor; skips models whose code **and upstream data** are unchanged (dbt source freshness + Delta table versions), with per-node staleness tolerance; failed runs leave prior state authoritative; JSON output; installable binary for Linux/macOS/Windows. `ods serve` dashboard: shell and Home (#310), State Plan and Why, Runs, Run (#311), Lineage with the State overlay (#312), Catalog and the Model page (#313). |
| **v0.0.2** | **ERD, Impact & hardening** | R2 | 2026-11-27 | The ERD page (#64) and the Impact simulator (#347) on the dashboard, both over engines already built; failure explanations for agents and the CLI (#348); `ods state savings` (#210); Databricks error messages recorded from a real run (#349); the plugin conformance suite (#99) and property tests (#192). |
| v0.0.3, … | Incremental releases | UX1, and any later-milestone work that lands early | as ready | Public releases stay v0.0.x until the whole dashboard design is built. Work from M2 onwards that is done before then ships in a v0.0.x release. |
| **v0.1.0** | **ODS Dashboard complete** | UX1 | 2027-01-29 | Every screen in the [dashboard design](design/dashboard/README.md) is built (#309): the v0.0.1 screens plus Freshness evidence, Semantic layer placeholder, Impact simulator, ERD, Settings and dark mode. |
| v0.2.0 (after v0.1.0) | Databricks depth & reuse strategies | M2 | 2027-02-26 | Unity Catalog metadata; full Delta change providers (CDF, watermarks, partitions); remaining freshness/trigger policies incl. WAIT; REUSE/DEFER/CLONE strategies; PostgreSQL store with locking; secrets providers. |
| v0.3.0 (after v0.1.0) | Schema intelligence: ERD + Usage | M3 | 2027-04-30 | `ods erd generate/inspect/validate` (dbt provider, Mermaid/DOT/PlantUML); `ods usage …` backed by Unity Catalog. |
| v0.4.0 (after v0.1.0) | ODS CI | M4 | 2027-06-30 | Change-impact engine, selective CI planner, PR report (Markdown + JSON), usage-aware risk; SQL parser + column lineage. |
| v0.5.0 (after v0.1.0) | Developer experience | M5 | 2027-08-31 | Clean-room LSP (index, completion, diagnostics, navigation, rename) + VS Code extension. |
| v0.6.0 (after v0.1.0) | ODS Agent | M6 | 2027-10-29 | Agent runtime, BYOK LLM providers, policy/approval, patch engine, self-validation, first skills (tests/docs/investigate). |
| later | Platform & ecosystem | M7–M9 | — | Mesh, server mode/API/RBAC/audit, integrations, synthetic data, dev envs, ops/cost/governance. |

**Versioning rule:** releases stay v0.0.x until every screen in the dashboard design
(`docs/design/dashboard/`) is built; **v0.1.0 ships only when the dashboard design is
complete** (UX1, #309). The milestones after it keep their order and their version
numbers (M2 → v0.2.0, M3 → v0.3.0, …); each of those releases comes after v0.1.0.

Target dates are proposals for a small team and should be revisited at the end of M0.
Versioning follows SemVer with a `0.x` "anything may break between minors" caveat; in
the v0.0.x series any release may break.
The formal policy (#101) is [ADR-0019](adr/0019-release-and-versioning.md): one product
version, a version per interface, `CHANGELOG.md` and a deprecation window.

## 4. Milestones

Issue numbers in **bold** are on the critical path for the milestone's release.

### M0 — Foundations (internal, before v0.0.1)
Goal: a compiling Rust workspace with the architectural skeleton every other module builds on.

| Issue | Title |
|---|---|
| **#1** | Monorepo architecture & module boundaries (ADR) |
| **#105** | Rust-first backend architecture (ADR) |
| **#2** | Versioned plugin SDK |
| **#3** | Provider capability negotiation |
| **#4** | Shared semantic graph/domain core |
| #5 | ERD and lineage as distinct graph domains |
| **#6** | Composable `ods` CLI framework |
| #7 | Unified configuration & profiles |
| #8 | Structured events & observability model |
| #108 | rs-rich-cli as the CLI presentation layer (ADR + PoC) |
| **#100** | Representative dbt demo project & fixtures |
| #10 | Clean-room & third-party licensing policy |
| #101 | Release/versioning strategy |
| #103 | Vision/roadmap document in repo |
| #104 | Naming/package/domain availability research |

Definition of done: `cargo build/test/clippy/fmt` + `cargo deny` green in GitHub Actions;
ADR-0001 (module boundaries), ADR-0002 (Rust stack), ADR-0003 (CLI presentation) merged;
`ods --version` and `ods state --help` run; a fake provider passes a contract smoke test.

### M1 — State MVP (→ **v0.0.1**, first public release)
Goal: prove the core value — *"ODS decides WHAT runs, dbt decides HOW"* — locally and explainably.
v0.0.1 also carries the first dashboard screens (#310–#313, listed below and under UX1).

| Issue | Title |
|---|---|
| **#11** | State domain model & lifecycle |
| **#12** | dbt artifact provider (manifest/run_results/catalog) |
| **#13** | Deterministic code fingerprinting |
| **#14** | Provider-neutral RelationState & data fingerprints |
| **#18** | DAG invalidation & impact propagation |
| **#20** | State execution planner |
| **#22** | `ods state plan` and dry-run |
| **#21** | State explain/diff/history/why commands |
| **#23** | dbt execution provider (exact node selection) |
| **#25** | SQLite StateStore |
| **#24** | `ods state run` end-to-end MVP flow |
| #26 | Filesystem JSON StateStore (debugging) |
| **#16** | Source/upstream change detection contract; includes dbt `sources.json` freshness (`max_loaded_at`) as change evidence for any warehouse |
| **#19** *(M1 slice)* | Freshness policies, minimal: `AnyDependencyChanged` (default), `MaxStaleness` (per-node/group tolerance, **default 0**: rebuild on any new data) and forced rebuild; the policy used appears in `explain` |
| **#17** *(M1 slice)* | Delta change detection, minimal: latest table version + commit timestamp for sources, behind the `relation_versions` capability, read through dbt (ADR-0022) |
| #99 | Plugin conformance test suite (State-related contracts) |
| **#168** | Read dbt State configs (`state:`, `freshness.build_after`, `loaded_at_*`) so existing dbt State projects work unchanged; dbt defaults (45m/`any`) only when the project already uses State, otherwise tolerance 0 |
| **#211** | dbt executor: exact selection that can't widen (`fqn:`/selector file), no command-line length limit |
| **#212** | Distribution: release binaries, Homebrew, `pip`-installable wrapper |
| #209 | Formatting-insensitive SQL fingerprints: comment/whitespace edits reuse |
| #214 | `ods.toml` settings for `ods state run` (dbt path, dirs, target, environment) |
| **#220** | `ods state run` without tests by default; `--test`, `ods state test`, `--full-refresh`, `--resource-type`, `--exclude`, dbt args after `--` |
| **#227** | State scope includes the dbt target; dbt settings read from the environment (`DBT_TARGET`, `DBT_PROFILE`, `DBT_FULL_REFRESH`) are honoured or refused |
| **#229** | dbt-shaped commands: `ods state compile`, `run` (models, `dbt run`), `seed`, `snapshot`, `build` (with tests); `--full-refresh` and `--vars` as in dbt |
| **#230** | Don't reuse a node whose table no longer exists in the warehouse |
| #232 | Run source tests in `build`/`test` when the source has new or unknown data |
| #233 | CI job running the real-dbt integration tests (dbt + DuckDB) |
| **#310** | Dashboard: app shell and Home (health overview) |
| **#311** | Dashboard: State pages (Plan and Why, Runs, Run) |
| **#312** | Dashboard: Lineage with State overlay (restyle `ods serve` lineage) |
| **#313** | Dashboard: Catalog (models) and Model page |
| #73, #74 *(preview)* | SQL parser and open column-level lineage: `ods lineage`, `ods serve`, OpenLineage export, observed lineage from Unity Catalog (#164–#167). Delivered early; the CI integration stays in M4 |

**Status (2026-10-04): released as [v0.0.1](https://github.com/buchochelliq-labs/open-data-suite/releases/tag/v0.0.1).**
- Closed on GitHub: #11, #18, #20, #21, #22, #23, #24, #74, #168, #181, #188, #209, #211,
  #212, #214, #220, #227, #229, #230, #232, #233 and the four dashboard screens
  #310–#313 (merged in #316, #318, #319, #320).
- Delivered beyond the table: run events, the run journal, the live view and run
  playback (#322, [ADR-0024](adr/0024-run-events-node-stats-and-run-journal.md),
  [ADR-0026](adr/0026-run-playback.md)), and failed nodes and tests explained (#323,
  [ADR-0025](adr/0025-error-explanations.md)).
- Moved out of M1 on 2026-10-04, with their v0.0.1 slice merged and in use; what's
  left of each goes to M2: #12 (the formal `ArtifactProvider` contract), #13 (the
  pluggable `FingerprintProvider`), #14 (`RelationState`), #16 (the `ChangeSet` type
  and `ChangeProvider` contract), #17 and #19 (the remainders below) and #25 (plan
  history). #73's preview is done too, and its syntax-tree and symbol API with source
  ranges goes to M5, which needs it.
- Moved out of M1 with no work merged, since v0.0.1 doesn't need them: #26 (a JSON
  store for debugging) goes to M2, and #99 and #192 (the conformance suite and property
  tests) go to R2.
- The release itself (#356): v0.0.1 is on GitHub Releases and PyPI (#360), and the
  ADRs that shipped are accepted (#357, merged in #359).

**Why data awareness moved into M1:** skipping only on code changes still rebuilds
models whose upstream data hasn't changed. Skipping on unchanged code *and* data is the
minimum useful behaviour for the State MVP.

**What the #17 slice pulls in:** table history is read through dbt's own connection
(`dbt show --inline`), with the Delta query supplied by `ods-provider-databricks`
([ADR-0022](adr/0022-delta-table-versions-as-source-evidence.md)). ODS still handles no
warehouse credential, so neither a Databricks SQL connection (#15) nor a secret
resolver (#126) is needed for M1. Both stay in M2.

**Staleness is propagated through views:** a view is only as fresh as its inputs, so
#18 carries upstream freshness through view-materialised models.

Out of scope for v0.0.1: the remaining dashboard screens (UX1, #309), Unity Catalog metadata beyond table history, Change Data Feed,
watermark/partition triggers, WAIT decisions, clone/defer, distributed locking, server mode.
The M1 slices keep #17 and #19 open; their remainder ships in M2.

### R2 — ERD, Impact & hardening (→ **v0.0.2**)
Goal: put on the dashboard what ODS already computes but only shows in the CLI, and
harden what v0.0.1 ships. The two screens are part of the v0.1.0 design (#309) and
come first because their engines exist.

| Issue | Title |
|---|---|
| **#64** | Dashboard: the ERD page, over `ods-erd` (from M3) |
| **#347** | Dashboard: the Impact simulator, over `ods lineage impact` |
| **#348** | Explain a failure from MCP and the CLI (`explain_failure`, `ods state explain-failure`) |
| #210 | `ods state savings`: builds skipped and time avoided (from M2) |
| #349 | Record the dbt catalogue's Databricks error messages from a real run |
| #99 | Plugin conformance test suite |
| #192 | Property and fuzz tests for the graph, SQL lineage and State invariants |

### UX1 — ODS Dashboard (→ **v0.1.0**)
Goal: build every screen of the [dashboard design](design/dashboard/README.md) in
`ods serve` (server-rendered, read-only, [ADR-0009](adr/0009-hostable-explorer-ods-web.md)).
**v0.1.0 ships only when every screen in the design is done**; until then releases are
v0.0.x.

| Issue | Title |
|---|---|
| **#310** *(in v0.0.1, tracked in M1)* | App shell and Home (health overview) |
| **#311** *(in v0.0.1, tracked in M1)* | State pages (Plan and Why, Runs, Run) |
| **#312** *(in v0.0.1, tracked in M1)* | Lineage with State overlay |
| **#313** *(in v0.0.1, tracked in M1)* | Catalog (models) and Model page |
| **#309** | Remaining screens for v0.1.0 (the umbrella; one issue per screen below) |
| *#64, #347* *(in v0.0.2, tracked in R2)* | ERD, Impact simulator |
| **#350** | Freshness evidence (Catalog → Sources) |
| **#351** | Settings (read-only), with the Server mode placeholders |
| **#352** | Semantic layer (Catalog → Metrics), read-only placeholder |
| **#353** | Dark mode checked on every screen |
| **#354** | Health badges and coverage from real signals (the dashboard slice of #117) |
| #355 | Run timeline: a per-thread Gantt view (playback follow-up, ADR-0026) |
| #388 | Freshness evidence: the source versions the last dry run read, without the server connecting |
| #392 | Configurable and pluggable health checks: `[health]` config, declarative, probe and script checks, plugins ([ADR-0030](adr/0030-configurable-and-pluggable-health-checks.md)) |

#310–#313 are in the M1 GitHub milestone because they ship in v0.0.1, and #64 and #347
are in R2 because they ship in v0.0.2. #309 closes when every screen is built.

### M2 — Databricks & data-aware State (→ v0.2.0, after v0.1.0)
| #15 Databricks/UC metadata provider · #12 dbt `ArtifactProvider` contract · #13 `FingerprintProvider` contract · #14 `RelationState` · #16 `ChangeSet` and `ChangeProvider` · #25 plan history in the SQLite store · #26 JSON StateStore for debugging · #169 `ods mcp` read-only MCP server (ADR-0010) · #173 ODS skills pack for existing coding agents · #170 live Unity Catalog lineage reader · #17 Delta change providers (remainder: CDF, commit history, watermark, partition arrival) · #19 freshness & trigger policies (remainder: AllDependenciesChanged, MinInterval, WatermarkReached, PartitionAvailable, WAIT) · #29 REUSE/DEFER/CLONE abstraction · #30 Databricks shallow clone · #27 PostgreSQL StateStore · #28 locking/leases/fencing · #126 secrets & external config · #31 column-aware invalidation (research) · #215 public benchmark of `ods state run` · #221 reuse hooked models safely (keyed digests of run-time values) · #224 dbt selector parity (`-s`, `--selector`, methods, graph operators; ADR-0015) · #225 `ods dbt …` front-end, also run as `dbt` (ADR-0015) · #231 rebuild or adopt seeds by comparing the table with the CSV (row count + hash) · #387 pluggable source-version (freshness) providers: a registry instead of the hard-coded Databricks mapping, external providers, author guide, per-warehouse research (Databricks stays the only built-in) |

### M3 — ERD & Usage (→ v0.3.0, after v0.1.0)
| #172 ODS metadata index (queryable lineage/State/ERD/usage) · #60 ERD domain model · #61 dbt ERD provider · #62 relationship inference · #63 render/export · #65 `ods erd` CLI · #55 Usage domain & UsageProvider · #56 Unity Catalog usage provider · #59 `ods usage` CLI · #66 warehouse-native ERD providers (stretch) |

### M4 — ODS CI (→ v0.4.0, after v0.1.0)
| #75 change-impact engine (on the column lineage delivered in M1 preview) · #171 column lineage for Python models · #84 selective CI planner · #85 PR report/check output · #58 usage in CI risk · #109 data diff · #110 data diff in CI · #213 GitHub Action PR impact comment |

### M5 — LSP & VS Code (→ v0.5.0, after v0.1.0)
| #67 clean-room LSP architecture · #73 syntax-tree and symbol API with source ranges (the parser's M1 preview is done) · #68 indexing · #69 completion · #70 diagnostics · #71 navigation/hover · #72 semantic rename · #107 VS Code extension · #116 SQL scratch/REPL |

### M6 — ODS Agent (→ v0.6.0, after v0.1.0)
Core: #32 architecture & tool runtime · #33 LLMProvider/BYOK · #34 context planner · #9 policy & approval (pulled forward if State needs it) · #50 patch engine · #51 self-validation · #52 Git-aware review · #53 skills SDK.
First skills: #36 missing tests · #37 test review · #38 test priority · #57 usage-driven tests · #39/#40 docs · #41 contracts · #35 investigate.
Stretch: #42–#49 review skills, #54 explore mode, #123 diagnostics.

**Direction:** ODS won't build its own chat interface. The engines answer and the model
proposes:
- M2 makes ODS agent-ready in the agents teams already use (#169, #173).
- M6 adds a headless agent for CI and review whose output is a proof-carrying evidence
  bundle (#50–#52), governed by declarative policy (#9, #98).
- The working product name is **ODS Steward** (`ods steward` as an alias of `ods agent`),
  pending the #104 name check.

### M7 — Platform: Mesh & Server (unscheduled)
#86 Mesh resolver · #89 registry/contracts · #90 change propagation · #91 cross-platform mapping · #95 server mode · #96 REST API · #97 RBAC/OIDC · #98 audit log · #106 language bindings · #102 docs site · #124 multi-repo discovery. Idea, no issue yet: one federated build graph across project formats (dbt + SQLMesh first), with Mesh as the same mechanism for dbt only ([ADR-0027](adr/0027-federated-build-graph-across-project-formats.md)). Open data environments for any build tool, unifying #29, #30, #114, #120 and #195 ([ADR-0028](adr/0028-open-data-environments.md)).

### M8 — Ecosystem integrations & operations (unscheduled)
#92 OpenMetadata · #93 Elementary · #94 MetricFlow · #111–#113 cost & optimisation · #117 health scoring · #118 observability store · #119 alert routing · #120 deployment/promotion · #121 package governance · #122 ownership/SLA governance · #125 snapshot/SCD review · #114 dev env cloning · #115 dev sampling · #226 SQLMesh project provider and `ods sqlmesh` front-end (ADR-0015).

### M9 — ods-synthetic (unscheduled)
#127 module & ADR · #147 engine contract · #129 profiling · #130 planner · #131 ERD-aware generation · #132 dbt constraints · #133 statistical engine · #134 copula engine · #135 rule/Faker engine · #136 SDV adapter · #137 engine selection · #148 sensitive-column policy · #139 privacy-risk eval · #141 quality eval · #142 validate with dbt tests · #143 writers · #144 CLI · #146 realistic mode · #150 row counts · #151 edge-case mode · #152 ods-dev integration · #153 ods-ci integration · #154 usage-prioritised fidelity · #155 explainability · #156 benchmarks · #149 differential privacy · #157 licensing & privacy docs.

Synthetic depends on M3 (ERD, usage) and M1 (core/state). It is a strong v1.x candidate
and should get its own ADR (#127) before any engine work.

## 5. Backlog hygiene — suspected duplicates

The backlog was created in several passes and contains duplicates. Recommend closing the
**newer/older copy listed on the right as `duplicate`** of the canonical issue on the left
(canonical = the copy with the richer description or lower number used above):

| Canonical | Duplicate(s) | Topic |
|---|---|---|
| #147 | #128 | SyntheticEngine contract |
| #148 | #138 | Sensitive-column policy |
| #149 | #140 | Differential privacy |
| #150 | #145 | Scale/row-count generation |
| #85 | #88 (closed) | CI PR report |
| #84 | #87 (closed) | Selective CI planner |
| #60 | #83 (closed) | ERD domain model |
| #59 | #82 (closed) | `ods usage` CLI |
| #58 | #78, #81 (closed) | Usage in CI risk |
| #57 | #77, #80 (closed) | Usage-driven test recommendations |
| #56 | #76, #79 (closed) | Unity Catalog usage provider |

Note: several closed copies (#76–#88) look like they were closed as duplicates without
`state_reason: duplicate`; the open copies remain the canonical ones above.

## 6. Proposed labels

`area:core` `area:sdk` `area:cli` `area:state` `area:erd` `area:usage` `area:ci`
`area:lsp` `area:vscode` `area:agent` `area:mesh` `area:server` `area:synthetic`
`area:dev` `area:ops` `area:docs` · `provider:dbt` `provider:databricks`
`provider:postgres` · `type:adr` `type:feature` `type:research` `type:docs`
`type:chore` · `priority:critical-path` · `good-first-issue`.

These are defined in [`.github/labels.json`](https://github.com/buchochelliq-labs/open-data-suite/blob/main/.github/labels.json) and created by the sync script.

## 7. Target crate layout (see [ADR-0001](adr/0001-monorepo-architecture-and-module-boundaries.md), #1)

```
crates/
  ods-core/        # semantic graph, domain types, capability model — no I/O, no vendors
  ods-sdk/         # plugin contracts (traits) + versioning + conformance harness
  ods-events/      # event schema, EventSink, tracing glue
  ods-config/      # layered config, profiles, secret references
  ods-policy/      # allow/deny/require-approval evaluation
  ods-state/       # fingerprints, invalidation, planner, StateStore trait
  ods-health/      # health checks, findings and badges, configured by [health] (ADR-0030)
  ods-erd/  ods-usage/  ods-ci/  ods-lsp/  ods-agent/  ods-mesh/  ods-synthetic/
  ods-cli/         # `ods` binary; module subcommand registration; presentation boundary
providers/
  ods-provider-dbt/          # ArtifactProvider, Executor, ERD provider
  ods-provider-databricks/   # Metadata/Change/Clone/Usage providers
  ods-store-sqlite/  ods-store-fs/  ods-store-postgres/
  ods-provider-fake/         # reference fake used by conformance tests
fixtures/
  dbt/jaffle-ods/            # #100 demo project + artifacts for multiple dbt versions
```

Dependency direction: `ods-core` ← foundation ← `ods-sdk` ← {modules, providers} ← `ods-cli`.
Modules and providers never depend on each other; the CLI wires concrete providers in at the edge.

## 8. Immediate next steps (first two weeks)

1. Merge this plan; run `scripts/sync-milestones.sh` to create milestones/labels and assign issues.
2. Close the duplicates in §5.
3. Write ADR-0001 (#1) and ADR-0002 (#105) — use the `adr` skill.
4. Scaffold the Cargo workspace + CI (fmt, clippy `-D warnings`, test, cargo-deny) — `new-crate` skill.
5. Land `ods-core` graph types (#4) + SDK traits (#2) + capability model (#3) with a fake provider.
6. Build the fixture dbt project and commit its `manifest.json` for dbt 1.8/1.9/1.10 (#100).


### M10 — Advanced Intelligence & Developer Experience (unscheduled)
Cross-cutting developer intelligence and operational ergonomics that build on the core metadata/evidence platform without changing the committed M0–M9 release train.

#194 `ods drift` · #195 environment parity / `ods env diff` · #196 dead asset detection · #197 lineage confidence/evidence strength · #198 contract compatibility · #199 `ods reproduce` · #200 warehouse query-plan/performance inspection · #201 metadata-aware search · #202 policy-as-code · #203 local metadata daemon/watch mode · #204 portable metadata snapshots · #205 PII/sensitive-data propagation · #206 mutation testing · #207 migration/upgrade assistant · #208 interactive `ods explore`.

This milestone is intentionally unscheduled. Individual capabilities can be pulled forward when they directly unblock an earlier milestone, but the source-of-truth grouping remains M10 until the roadmap is rebaselined.
