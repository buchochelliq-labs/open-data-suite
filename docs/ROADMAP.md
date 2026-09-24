# OpenDataSuite (ODS) — Roadmap, Milestones & Release Plan

Status: **proposed** · Last updated: 2026-09-24

This document turns the initial backlog (issues #1–#157) into an ordered set of
milestones and releases. It is the planning source of truth until the vision
document (#103) lands; milestone membership is mirrored in
[`.github/milestones.json`](../.github/milestones.json) and applied to GitHub with
[`scripts/sync-milestones.sh`](../scripts/sync-milestones.sh).

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
7. **Clean-room** — only public/OSS dbt contracts; no proprietary binaries (#10, #67).
8. **ERD ≠ lineage** — separate domain types (#5).

## 3. Release train

| Release | Theme | Milestone(s) | Target | Exit criteria (summary) |
|---|---|---|---|---|
| **v0.0.x** (internal) | Foundations | M0 | 2026-10-30 | Workspace builds in CI; ADRs for architecture, Rust stack, CLI presentation; plugin SDK + semantic core compile with fake providers; demo dbt fixture project. |
| **v0.1.0** — *first public release* | **ODS State MVP (local)** | M1 | **2026-12-18** | `ods state plan / run / explain / diff / history` on a dbt project using `manifest.json`, SQLite state, exact-selection dbt executor; failed runs leave prior state authoritative; JSON output; installable binary for Linux/macOS/Windows. |
| v0.2.0 | Databricks & data-aware State | M2 | 2027-02-26 | Unity Catalog metadata + Delta change providers; freshness/trigger policies; REUSE/DEFER/CLONE strategies; PostgreSQL store with locking; secrets providers. |
| v0.3.0 | Schema intelligence: ERD + Usage | M3 | 2027-04-30 | `ods erd generate/inspect/validate` (dbt provider, Mermaid/DOT/PlantUML); `ods usage …` backed by Unity Catalog. |
| v0.4.0 | ODS CI | M4 | 2027-06-30 | Change-impact engine, selective CI planner, PR report (Markdown + JSON), usage-aware risk; SQL parser + column lineage. |
| v0.5.0 | Developer experience | M5 | 2027-08-31 | Clean-room LSP (index, completion, diagnostics, navigation, rename) + VS Code extension. |
| v0.6.0 | ODS Agent | M6 | 2027-10-29 | Agent runtime, BYOK LLM providers, policy/approval, patch engine, self-validation, first skills (tests/docs/investigate). |
| later | Platform & ecosystem | M7–M9 | — | Mesh, server mode/API/RBAC/audit, integrations, synthetic data, dev envs, ops/cost/governance. |

Target dates are proposals for a small team and should be revisited at the end of M0.
Versioning follows SemVer with a `0.x` "anything may break between minors" caveat;
the formal policy is delivered by #101 in M0.

## 4. Milestones

Issue numbers in **bold** are on the critical path for the milestone's release.

### M0 — Foundations (→ v0.0.x, internal)
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

### M1 — State MVP (→ **v0.1.0**, first release)
Goal: prove the core value — *"ODS decides WHAT runs, dbt decides HOW"* — locally and explainably.

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
| #16 | Source/upstream change detection contract (conservative, no warehouse yet) |
| #99 | Plugin conformance test suite (State-related contracts) |

Out of scope for v0.1.0: warehouse metadata, clone/defer, distributed locking, server mode.

### M2 — Databricks & data-aware State (→ v0.2.0)
| #15 Databricks/UC metadata provider · #17 Delta change providers · #19 freshness & trigger policies · #29 REUSE/DEFER/CLONE abstraction · #30 Databricks shallow clone · #27 PostgreSQL StateStore · #28 locking/leases/fencing · #126 secrets & external config · #31 column-aware invalidation (research) |

### M3 — ERD & Usage (→ v0.3.0)
| #60 ERD domain model · #61 dbt ERD provider · #62 relationship inference · #63 render/export · #65 `ods erd` CLI · #55 Usage domain & UsageProvider · #56 Unity Catalog usage provider · #59 `ods usage` CLI · #66 warehouse-native ERD providers (stretch) · #64 interactive ERD web view (stretch) |

### M4 — ODS CI (→ v0.4.0)
| #73 dialect-aware SQL parser/AST · #74 column lineage · #75 change-impact engine · #84 selective CI planner · #85 PR report/check output · #58 usage in CI risk · #109 data diff · #110 data diff in CI |

### M5 — LSP & VS Code (→ v0.5.0)
| #67 clean-room LSP architecture · #68 indexing · #69 completion · #70 diagnostics · #71 navigation/hover · #72 semantic rename · #107 VS Code extension · #116 SQL scratch/REPL |

### M6 — ODS Agent (→ v0.6.0)
Core: #32 architecture & tool runtime · #33 LLMProvider/BYOK · #34 context planner · #9 policy & approval (pulled forward if State needs it) · #50 patch engine · #51 self-validation · #52 Git-aware review · #53 skills SDK.
First skills: #36 missing tests · #37 test review · #38 test priority · #57 usage-driven tests · #39/#40 docs · #41 contracts · #35 investigate.
Stretch: #42–#49 review skills, #54 explore mode, #123 diagnostics.

### M7 — Platform: Mesh & Server (unscheduled)
#86 Mesh resolver · #89 registry/contracts · #90 change propagation · #91 cross-platform mapping · #95 server mode · #96 REST API · #97 RBAC/OIDC · #98 audit log · #106 language bindings · #102 docs site · #124 multi-repo discovery.

### M8 — Ecosystem integrations & operations (unscheduled)
#92 OpenMetadata · #93 Elementary · #94 MetricFlow · #111–#113 cost & optimisation · #117 health scoring · #118 observability store · #119 alert routing · #120 deployment/promotion · #121 package governance · #122 ownership/SLA governance · #125 snapshot/SCD review · #114 dev env cloning · #115 dev sampling.

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

These are defined in [`.github/labels.json`](../.github/labels.json) and created by the sync script.

## 7. Target crate layout (see [ADR-0001](adr/0001-monorepo-architecture-and-module-boundaries.md), #1)

```
crates/
  ods-core/        # semantic graph, domain types, capability model — no I/O, no vendors
  ods-sdk/         # plugin contracts (traits) + versioning + conformance harness
  ods-events/      # event schema, EventSink, tracing glue
  ods-config/      # layered config, profiles, secret references
  ods-policy/      # allow/deny/require-approval evaluation
  ods-state/       # fingerprints, invalidation, planner, StateStore trait
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
