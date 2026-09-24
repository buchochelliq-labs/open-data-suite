# ODS State vs dbt State

Status: research note · 2026-09-25 · Informs the M1 scope in [ROADMAP.md](../ROADMAP.md)

Built from dbt Labs' public docs, blog and press as summarised by web search. The
product page itself was not directly readable from the research environment, so the
details should be re-checked before any public comparison. Clean-room rule (AGENTS.md
rule 8): only concepts from public documentation are used here, never code or
undocumented formats.

## What dbt State is (as of Sept 2026)
- **What it replaces:** it is the successor to state-aware orchestration, which ran on
  Fusion in production only. It launched in preview on 2026-06-01 and reached general
  availability in September 2026.
- **Where it runs:** dbt Core 1.7–2.0, Fusion, the dbt platform and external
  orchestrators, in both development and deployment.
- **Pricing:** a paid service with a 30-day trial; the dbt platform is not required.
- **How it decides each model:**
  1. **Reuse** it if it exists in the target schema, its logic is unchanged, and its
     parents' data is not fresher than its `lag_tolerance`.
  2. Otherwise **clone** it from the deferred environment, if that copy has the same
     logic and fresh-enough data. This uses zero-copy clone where the warehouse supports
     it, else `CREATE TABLE AS`.
  3. Otherwise **build** it, deferring unselected upstream models.
- **`lag_tolerance`:** how old upstream data may get before a rebuild. It is measured
  against the freshness of the underlying data, not the model's last run. The default
  is **45 minutes**, and it can be a Jinja template, so it can differ per environment.
- **Freshness through views:** it tracks freshness across the DAG and propagates it
  through models materialised as views.
- **Development:** it clones selected models from production automatically where
  possible.
- **Claimed savings:** 30% or more off warehouse compute.

## Comparison

| Capability | dbt State | ODS |
|---|---|---|
| Skip a model when its code is unchanged | Yes | M1: #13, #18, #20 |
| Skip a model when its upstream data is unchanged | Yes | **M1**: #16 (dbt source freshness), #17 slice (Delta table versions) |
| Staleness tolerance | `lag_tolerance`, 45-minute default | **M1**: #19 slice, `MaxStaleness`, **default 0** |
| Freshness propagated through views | Yes | **M1**: #18 |
| Clone or defer from another environment | Yes | M2: #29, #30 |
| Reuse in development | Yes | Not yet planned (candidate issue) |
| Reuse in CI | Yes | M4: #84 |
| Explains each decision | Reused models flagged in logs and lineage | M1: #21, reason chain and evidence as text and JSON |
| A failed run cannot corrupt state | Not stated | M1 guarantee: #11, #24 |
| Where state lives | dbt's hosted service | Local SQLite, later PostgreSQL; open source |
| Concurrent runs | Presumably handled by the service | #28; the lease contract exists (ADR-0006) |

## Decisions taken
1. **Move data awareness into M1.** It covers dbt `sources.json` freshness (#16), a
   minimal #19 and a Delta table-version slice of #17, so v0.1.0 is not behind on the
   headline feature.
2. **Default tolerance is 0.** A 45-minute default lets data go stale silently, which
   contradicts rule 3 (conservative defaults). Tolerance is opt-in per node or group,
   and per profile.
3. **Differentiate on what dbt State doesn't claim:**
   - open and local, with no paid service;
   - explanations as a first-class feature;
   - a failed run never corrupts state;
   - warehouse neutrality through capabilities.

## Open candidates (not yet issues)
- Reuse in development: clone or defer from production into dev (overlaps #29 and #114).
- Per-environment tolerance via profiles (ADR-0005) — likely part of #19.

## Sources
- [dbt State product page](https://www.getdbt.com/product/dbt-state)
- [About dbt State](https://docs.getdbt.com/docs/deploy/dbt-state-about)
- [dbt State configurations](https://docs.getdbt.com/reference/resource-configs/dbt-state-configs)
- [lag_tolerance](https://docs.getdbt.com/reference/resource-configs/lag-tolerance)
- [How dbt State actually works](https://docs.getdbt.com/blog/how-dbt-state-works)
- [What happened to state-aware orchestration?](https://docs.getdbt.com/faqs/Runs/what-happened-to-sao)
- [Migrating from state-aware orchestration to dbt State](https://docs.getdbt.com/docs/deploy/dbt-state-migration)
- [How dbt State cuts warehouse compute](https://www.getdbt.com/blog/dbt-state-use-case)
- [Fivetran + dbt Labs at dbt Summit 2026](https://www.fivetran.com/press/fivetran-dbt-labs-announces-new-capabilities-to-make-enterprise-data-agent-ready-at-dbt-summit-2026)
