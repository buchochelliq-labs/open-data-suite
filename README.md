# OpenDataSuite (ODS)

A Rust-first, provider-neutral control plane for analytics engineering. It starts with dbt
and Databricks, and every decision it makes can be explained.

> **Status:** pre-alpha, still being planned. Nothing is usable yet.

| Module | What it does | Target |
|---|---|---|
| `ods state` | Incremental, explainable "what needs to run", handed back to dbt with exact selection | v0.1.0 |
| `ods erd` / `ods usage` | Entity-relationship model and real consumer usage | v0.3.0 |
| `ods ci` | Change impact, selective CI, and PR reports | v0.4.0 |
| `ods lsp` + VS Code | Clean-room language server and editor extension | v0.5.0 |
| `ods agent` | Policy-controlled analytics-engineering agent and skills | v0.6.0 |

- Roadmap, milestones, and releases: [`docs/ROADMAP.md`](docs/ROADMAP.md)
- Contributor and AI-agent guide: [`AGENTS.md`](AGENTS.md)
- Architecture decisions: [`docs/adr/`](docs/adr/)
