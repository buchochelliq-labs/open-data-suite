# ADR-0002: Rust-first backend and technology stack

- **Status:** Proposed
- **Date:** 2026-09-24
- **Issues:** #105 (also #6, #8, #25, #27, #67, #95, #106)
- **Deciders:** @n1ckyb

## Context
The same semantic logic — graph traversal, fingerprinting, invalidation, planning, ERD and
usage models, impact analysis — is needed by a CLI, a long-running LSP, an optional server
and an agent runtime. Duplicating that logic across languages would create divergent
behaviour, which directly contradicts the explainability goal. The workloads are
latency-sensitive (LSP), long-running (server, LSP), and must be distributed as a small,
dependency-free binary for local-first use and CI.

## Options considered

### Option A — Rust core, thin bindings elsewhere
- ✅ Single, fast, memory-safe implementation shared by CLI/LSP/server/agent.
- ✅ Static binaries; no runtime to install in CI.
- ✅ Strong ecosystem for the pieces we need (below). PyO3/napi-rs/C ABI for bindings (#106).
- ❌ Smaller contributor pool among analytics engineers than Python.
- ❌ Slower iteration on exploratory features; compile times.

### Option B — Python core (dbt's own ecosystem language)
- ✅ Familiar to analytics engineers; easy to import dbt internals.
- ❌ Importing dbt internals conflicts with the clean-room and neutrality goals (#10).
- ❌ Poor fit for an LSP/server; packaging/runtime friction in CI; slower graph work.

### Option C — Go core
- ✅ Simple concurrency, static binaries, fast compile.
- ❌ Weaker type modelling for rich domain types/sum types; less mature LSP, SQL-parsing and
  Python-binding stories than Rust.

### Option D — TypeScript core (shared with the VS Code extension)
- ✅ One language for the extension and core.
- ❌ Node runtime required everywhere; weaker performance for large graphs and hashing.

## Decision
**Rust owns all business and orchestration logic (Option A).** Python, TypeScript and Go are
thin consumers via bindings or protocols and never re-implement core semantics.

Baseline stack (all permissively licensed, actively maintained):

| Concern | Choice | Licence | Notes / exit strategy |
|---|---|---|---|
| Language/toolchain | Rust stable, edition 2024, MSRV 1.85 (raised to 1.90 by ADR-0003) | MIT/Apache-2.0 | MSRV bumps are minor-version changes. |
| CLI | `clap` 4 (derive) | MIT/Apache-2.0 | De facto standard. |
| Serialization | `serde`, `serde_json` | MIT/Apache-2.0 | All persisted types carry `SchemaVersion`. |
| Errors | `thiserror` in libraries, `anyhow` in binaries only | MIT/Apache-2.0 | Typed errors keep contracts explicit. |
| Async runtime | `tokio` | MIT | Only at I/O boundaries; planning stays sync. |
| HTTP/service | `axum` (server mode, #95) | MIT | Tower middleware shared with LSP. |
| Database | `sqlx` for SQLite/PostgreSQL stores (#25, #27) | MIT/Apache-2.0 | Behind the `StateStore` contract; swappable (e.g. `rusqlite`). |
| Observability | `tracing` + OpenTelemetry exporter (#8) | MIT | Events are ODS types; tracing is a sink. |
| LSP | `tower-lsp` or `async-lsp` — decided in the LSP ADR (#67) | MIT/Apache-2.0 | Protocol types via `lsp-types`. |
| Terminal rendering | `rs-rich-cli` (#108, ADR-0003) | per that repo | Behind the presentation boundary. |
| SQL parsing | evaluate `sqlparser-rs` vs `datafusion-sqlparser` vs tree-sitter (#73) | Apache-2.0/MIT | Decided in its own ADR. |
| Testing | built-in tests, `insta` snapshots, fixtures | MIT/Apache-2.0 | No network in tests. |
| Supply chain | `cargo-deny` (licences, advisories, sources) | MIT/Apache-2.0 | Permissive allow-list in `deny.toml`. |

Project licence for ODS itself is provisionally **Apache-2.0** (patent grant, common for
infrastructure). Final confirmation belongs to #10 / #101.

## Consequences
- Positive: one implementation for every surface; small static binaries for CI;
  cross-platform builds (Linux/macOS/Windows tested in CI from day one).
- Negative / trade-offs: contributors need Rust; bindings (#106) add a packaging surface;
  compile times need caching in CI.
- Follow-up issues: #106 (bindings ADR), #108 (ADR-0003 presentation), #67 (LSP library),
  #73 (SQL parser), #10 (licence confirmation), #101 (release tooling, e.g. `cargo-dist`/`release-plz`).

## References
- ADR-0001 — module boundaries
- `Cargo.toml` `[workspace.dependencies]`, `deny.toml`
