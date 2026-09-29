# Roadmap

!!! warning "Plans, not promises"
    Dates are proposals for a small team, and are revisited at the end of each
    milestone. Scope and order will change. Until 1.0, anything may break between
    minor versions.

## Available today, as previews

These work today, on the demo project and on small dbt projects, but are unreleased and still changing:

| Capability | Command |
|---|---|
| Column-level lineage, offline explorer, graph exports, OpenLineage export | `ods lineage …` |
| Change impact: what must run and what can be skipped | `ods lineage impact` |
| Hosted explorer with a read-only JSON API | `ods serve` |
| Observed lineage from a Unity Catalog export | `ods lineage compare`, `--observed` |
| Entity-relationship diagrams | `ods erd generate` |
| MCP server for AI agents | `ods mcp` |
| dbt State configuration reader | `ods state policies` |
| State planning against recorded dbt runs | `ods state plan`, `record`, `history` |

## Release train

| Release | Theme | Target | What it delivers |
|---|---|---|---|
| pre-release (internal) | Foundations | 2026-10-30 | Workspace, architecture decisions, plugin SDK, demo dbt project |
| **v0.0.1**, the first public release | **ODS State MVP + first dashboard screens** | 2026-12-18 | `ods state plan / run / explain / diff / history`: skip models whose code **and upstream data** are unchanged; failed runs never replace state; installable binaries. Dashboard in `ods serve`: Home, State Plan and Why, Runs, Run, Lineage with the State overlay, Catalog and Model pages |
| v0.0.2, v0.0.3, … | Incremental releases | as ready | More dashboard screens, and any later work that is ready early |
| **v0.1.0** | **ODS Dashboard complete** | 2027-01-29 | Every screen in the dashboard design: adds Freshness evidence, Impact simulator, ERD, Settings and dark mode |
| v0.2.0 (after v0.1.0) | Databricks depth | 2027-02-26 | Unity Catalog metadata, Delta change detection, reuse strategies, PostgreSQL state store |
| v0.3.0 (after v0.1.0) | ERD and Usage | 2027-04-30 | `ods erd inspect / validate`, real consumer usage |
| v0.4.0 (after v0.1.0) | ODS CI | 2027-06-30 | Selective CI and pull-request impact reports |
| v0.5.0 (after v0.1.0) | Developer experience | 2027-08-31 | Language server and VS Code extension |
| v0.6.0 (after v0.1.0) | ODS Agent | 2027-10-29 | A policy-controlled agent, bring-your-own model, using the MCP tools |
| later | Platform | — | Mesh, server mode, integrations, synthetic data |

Releases stay v0.0.x until the whole dashboard design is built. **v0.1.0 ships only when
every dashboard screen is done.** The later releases keep their order and numbers, and
each comes after v0.1.0.

```mermaid
gantt
    dateFormat YYYY-MM-DD
    axisFormat %b %Y
    section Releases
    Foundations (internal)   :active, 2026-09-01, 2026-10-30
    State MVP (v0.0.1)       :        2026-10-30, 2026-12-18
    Dashboard (v0.1.0)       :        2026-12-18, 2027-01-29
    Databricks depth (v0.2.0):        2027-01-29, 2027-02-26
    ERD and Usage (v0.3.0)   :        2027-02-26, 2027-04-30
    ODS CI (v0.4.0)          :        2027-04-30, 2027-06-30
    LSP and VS Code (v0.5.0) :        2027-06-30, 2027-08-31
    ODS Agent (v0.6.0)       :        2027-08-31, 2027-10-29
```

Some preview capabilities, such as lineage, ERDs and MCP, were built ahead of their
milestones, because State and CI depend on them.

The [full plan](ROADMAP.md) maps every GitHub issue to a milestone. Progress is tracked
in [GitHub milestones](https://github.com/buchochelliq-labs/open-data-suite/milestones).
