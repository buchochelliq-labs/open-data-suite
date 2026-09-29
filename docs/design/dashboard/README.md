# ODS Dashboard

The dashboard is the central, read-only view of one dbt project and its targets.
`ods serve` hosts it locally today (lineage only). This design grows it into the
control-plane view of every ODS module. It stays a server-rendered page with inlined
assets and no npm build ([ADR-0009](../../adr/0009-hostable-explorer-ods-web.md)).

**Layout.**

- **Left nav:** project and target pickers, then Home, Catalog (Models, Freshness
  evidence, Semantic layer), Lineage, State (Plan, Runs, History, Policies), ERD,
  Usage, CI · Impact, Agent and Settings. A "Local · read-only" badge sits at the
  bottom.
- **Page header:** a breadcrumb, a search box that also accepts selectors
  (`+orders`), and the current snapshot.
- **Look:** IBM Plex Sans and Mono, with an indigo accent (`#3F51D8`). Build is blue
  (`#DCE8FB`/`#1F4FA0`) and Reuse is teal (`#D5F0EC`/`#0B6B60`). Health is green,
  amber, red, and dashed grey for unknown.

Light is the default. Home, Lineage, Model and Plan also have dark versions.

## Screens

### Home: project health

![Home](images/main.png)

- **Tiles:** nodes, reused and built in the last run, and snapshots.
- **Recent runs:** built and reused counts per run.
- **Needs attention:** changed, unknown-evidence and opaque nodes.
- **Summary panels:** health, test and doc coverage, and which modules are set up.

**Needs:** State history (`ods state history`) and the plan. Health and coverage are
`[n]` until the health signals exist.

### Catalog: models

![Catalog](images/catalog.png)

- **Facets:** resource type, layer, materialization, tags, next-run decision and
  lineage confidence.
- **Table:** every node, with its next-run pill, health badge and last build.

**Needs:**
- the manifest;
- the plan against the latest snapshot;
- a health badge per node, worked out from the last run outcome, test results and
  freshness evidence (static; no monitoring service).

### Catalog: freshness evidence

![Freshness evidence](images/sources.png)

- **Evidence per input:** how ODS knows whether each seed or source changed.
- **Grades:** exact, timestamp, inferred and unknown, and what each grade does to the
  plan. Unknown evidence always means build.

**Needs:** the freshness evidence already recorded in each snapshot.

### Catalog: semantic layer (placeholder)

![Semantic layer](images/metrics.png)

- **Contents:** semantic models and metrics, read from the project.
- **Source picker:** "dbt semantic manifest" is the first source; other build tools
  are greyed as planned.

The read goes through a generic semantic-source contract, so another build tool can
plug in the way warehouses do. ODS reads definitions only; it never serves or queries
metrics.

### Lineage: State overlay

![Lineage](images/lineage.png)

- **Graph:** the model graph, coloured by the plan's decision for each node.
- **Side panel:** the reason for the selected node.

**Needs:** `ods lineage` and the plan. Model-to-model edges come from the DAG. They
are not relationships; those belong to the ERD (AGENTS rule 6).

### Lineage: impact simulator

![Impact simulator](images/impact.png)

- **Input:** pick a column and a change (rename, type change or drop).
- **Must run:** each node, with its reason and whether its lineage is parsed, inferred
  or opaque.
- **Skipped:** each node left out, with the reason.
- **Side panel:** the column trail, a copyable selector, and the tests that run with
  it.
- **Contracts:** a warning when an enforced contract would break.

**Needs:** column-level impact (`ods lineage impact`). Nothing is run.

### Model page

![Model page](images/model.png)

- **Tabs:** overview, columns, lineage, code, State and tests.
- **Current decision:** the Build or Reuse pill, with its reason.

### State: plan and why

![Plan](images/plan.png)

- **Decision table:** every node, with its decision and reason code.
- **Why panel:** for the selected node, the reason chain, the fingerprint components
  that changed, and the relation check.

**Needs:** `ods state plan --output json`, which already carries all of this.

### State: runs (local)

![Runs](images/runs.png)

- **Source:** every run recorded in `.ods/state.db` on this machine, newest first. ODS
  doesn't schedule anything; runs appear after `ods state build/run` in a terminal or
  CI job.
- **Failed run:** shows that the last good snapshot was kept, the failed node, and
  the next command (`ods state retry --failed`).
- **CI runs:** a placeholder tab until server mode.

### State: one run

![Run](images/run.png)

- **Timeline:** built and reused per node, with durations as `[duration]` until they
  are recorded.
- **Built this run:** why each node was built.
- **Earlier runs:** the runs before it.

### ERD

![ERD](images/erd.png)

- **Diagram:** tables and keys, with each edge drawn by its strongest evidence:
  tested, declared constraint, joined in SQL, or inferred.
- **Cardinality:** only where tests prove it.
- **Missing relationships:** each comes with the YAML test that would make it tested.

**Needs:** `ods erd generate`.

### Settings

![Settings](images/settings.png)

- **Read-only view of `ods.toml`:** project and target, the state store, secret
  references (never values), and providers with their capabilities.
- **Server mode (planned):** Identity & access, Webhooks and Audit log, greyed as
  placeholders. Webhooks and audit are designed as event sinks on the event stream the
  CLI already emits, not a second record.

## Dark versions

| Home | Lineage |
|---|---|
| ![Home, dark](images/main-dark.png) | ![Lineage, dark](images/lineage-dark.png) |
| **Model page** | **Plan** |
| ![Model page, dark](images/model-dark.png) | ![Plan, dark](images/plan-dark.png) |

## Decisions taken from the dbt platform review

1. **Naming:**
   - Keep `ods state` as the CLI. The dashboard section's name is still open; the
     leading candidate is *Builds*.
   - Rename *Mesh* to *Projects*.
   - Don't use *Catalog* or *Explorer* as product names.
2. **`ods serve` grows into this read-only dashboard.**
3. **No scheduler.** Show runs instead, starting with local runs from the state store.
4. **Lag tolerance and cost savings:** in scope, shown next to the plan.
5. **Health signals:** in scope, as static badges from run, test and freshness
   evidence.
6. **Semantic layer:** read-only, through a generic source so other build tools can
   supply it.
7. **Identity, webhooks and audit:** deferred. They appear as placeholders, designed
   as event sinks.

## Still to design

- Usage, CI · Impact, Agent and History pages.
- Empty and error states for a project with no state store.
- Narrow screens.
