# AGENTS.md — OpenDataSuite (ODS)

Instructions for AI coding agents (Claude Code, Codex, Copilot, Cursor, …) and humans
working in this repository. `CLAUDE.md` imports this file; keep this the single source.

## What this repo is

ODS is a **Rust-first, provider-neutral control plane for analytics engineering**:
a shared semantic core plus independently useful modules (State, ERD, Usage, CI,
LSP, Agent, Mesh, Synthetic). dbt is the first project format; Databricks/Unity
Catalog is the first warehouse. Read [`docs/ROADMAP.md`](docs/ROADMAP.md) before
starting work — it maps every GitHub issue to a milestone and release.

Current phase: **M0 Foundations → M1 State MVP (v0.1.0)**. Work outside M0/M1 needs
a reason (e.g. the user asked for it explicitly).

## Non-negotiable architecture rules

1. **No vendor logic in core.** `ods-core`, `ods-sdk` and module crates (`ods-state`,
   `ods-erd`, …) must never branch on provider names or import provider crates.
   Behaviour differences are expressed as **capabilities** (#3). Vendor code lives in
   `providers/*`.
2. **Dependency direction** ([ADR-0001](docs/adr/0001-monorepo-architecture-and-module-boundaries.md)):
   `ods-core` ← foundation ← `ods-sdk` ← {modules, providers} ← `ods-cli`. Modules and
   providers never depend on each other; only the CLI (or a server binary) wires concrete
   providers into modules. New crates must be registered in
   `scripts/check-layering.py`; CI fails otherwise.
3. **Conservative defaults.** Missing/uncertain evidence ⇒ BUILD, deny, or mark as
   *inferred*. Never silently REUSE, allow a destructive action, or present inference as fact.
4. **Explainability.** Planner decisions and findings carry a reason chain and evidence
   that can be rendered as human text *and* JSON.
5. **Canonical state is only replaced on success.** A failed or partial run must never
   overwrite the last successful state.
6. **ERD ≠ lineage.** Separate domain types; a DAG edge is not a PK/FK relationship.
7. **Presentation is separate from logic.** Commands produce view models; rendering
   (rs-rich, `--output plain|json`) happens at the CLI edge (#108, ADR-0003).
8. **Clean-room.** Use only public dbt artifact schemas and OSS sources. Never copy,
   decompile, or depend on proprietary dbt Cloud/Fusion code or binaries (#10).
9. **Secrets** are referenced, never persisted in config, state, events, or logs.

Architectural changes (new crate, new contract, new persisted format, new dependency
with a non-permissive licence) require an ADR in `docs/adr/` — use the `adr` skill.

## Tech stack ([ADR-0002](docs/adr/0002-rust-first-backend-and-technology-stack.md), #105)

Rust (stable, edition 2024) · Tokio · Clap · Serde · SQLx (SQLite/PostgreSQL) · Axum ·
tracing/OpenTelemetry · thiserror (libraries) / anyhow (binaries only) ·
rs-rich for terminal rendering, confined to `ods-cli`
([ADR-0003](docs/adr/0003-cli-presentation-boundary.md), proposed). LSP library and SQL parser are
decided in their own ADRs (#67, #73). Python/TypeScript/Go are thin consumers only.

## Repository layout (target — see ROADMAP §7)

```
crates/       ods-core, ods-sdk, ods-events, ods-config, ods-policy, ods-state, … ods-cli
providers/    ods-provider-dbt, ods-provider-databricks, ods-store-sqlite, ods-provider-fake, …
fixtures/     dbt demo project + versioned artifacts (#100)
docs/         ROADMAP.md, adr/, user & contributor docs
.claude/      skills/ and agents/ for AI-assisted development
scripts/      repo automation (e.g. sync-milestones.sh)
```

## Commands

Run from the repo root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo deny check            # licences/advisories (install: cargo install cargo-deny --locked)
python3 scripts/check-layering.py   # enforces ADR-0001 dependency direction
cargo run -p ods-cli -- --help    # binary is named `ods`
```

The `verify` skill runs all of these and reports results. Do not claim work is done
unless they pass, or state exactly what failed.

## Coding conventions

- Library crates: typed errors with `thiserror`; no `unwrap`/`expect` outside tests
  (use `expect` only with an invariant message). No `anyhow` in libraries.
- Public domain types: `#[non_exhaustive]` where they may grow; derive
  `Serialize`/`Deserialize` with explicit `#[serde(rename_all = "snake_case")]`;
  persisted formats carry a `schema_version`.
- Determinism: stable ordering (`BTreeMap`/sorted `Vec`) in anything hashed,
  serialized, or shown in plans. Hashes must be canonical and reproducible.
- Async only at I/O boundaries; pure planning/graph logic stays synchronous and testable.
- Every provider contract has: a fake implementation in `ods-provider-fake`, and
  conformance tests in `ods-sdk` that real providers also run (#99).
- Tests: unit tests next to code; integration tests use `fixtures/`. Snapshot tests
  (`insta`) for CLI JSON/plain output.
- Comments explain *why*, not *what*. Keep doc comments on every public item.

## Workflow

- One issue per PR where practical. Reference it: `Closes #N` / `Part of #N`.
- Branch names: `<area>/<issue>-<slug>` (e.g. `state/20-execution-planner`), unless
  the harness assigns a branch.
- Commits: Conventional Commits (`feat(state): …`, `fix(sdk): …`, `docs(adr): …`).
- Keep PRs small and reviewable; do not widen scope beyond the issue's acceptance criteria.
- New or changed persisted formats, CLI flags, or JSON output require tests and a
  CHANGELOG entry (once #101 defines the format).
- Update `docs/ROADMAP.md` and `.github/milestones.json` together if milestone scope changes.

## Skills & agents available (`.claude/`)

| Skill | Use when |
|---|---|
| `implement-issue` | Picking up a GitHub issue end to end (read → plan → code → verify → PR). |
| `adr` | Recording an architecture decision in `docs/adr/`. |
| `verify` | Running fmt/clippy/test/deny before claiming done. |
| `new-crate` | Adding a crate to the workspace with correct layering. |
| `provider-plugin` | Implementing an SDK contract for a provider (dbt, Databricks, SQLite…). |
| `triage-issue` | Labelling, milestoning, and de-duplicating backlog issues. |

| Subagent | Use when |
|---|---|
| `architecture-guard` | Reviewing a diff against the non-negotiable rules above. |
| `issue-planner` | Turning an issue into an implementation plan with file-level steps. |

## Things agents must not do

- Push to `main`, force-push shared branches, or rewrite published history.
- Add a dependency with a copyleft/BUSL/unknown licence without an ADR (SDV is optional-only, #136).
- Introduce network calls in tests (use fixtures/fakes).
- Invent dbt artifact fields — check the public JSON schemas for the versions in `fixtures/`.
- Close, relabel, or re-milestone issues unless asked.
