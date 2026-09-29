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

**Built (#312) with these tokens** (light / dark), beside the board's:

| Token | Light | Dark | Used for |
|---|---|---|---|
| `--kind-seed` | `#7C9A6A` | `#9BB88A` | seed stripe (the board's) |
| `--kind-model` | `#6B7B93` | `#8E9DB3` | model stripe (the board's) |
| `--kind-source` | `#A08450` | `#C9AE7C` | source stripe (not on the board) |
| `--kind-snapshot` | `#8570B0` | `#B0A0DA` | snapshot stripe (not on the board) |
| `--edge` | `#A7B0BD` | `#4A5563` | DAG edge; dashed when only declared |
| `--edge-indirect` | `#A98BD0` | `#B59BE0` | row-shaping column edge, dashed |
| `--edge-up` | `#4A5462` | `#C9D1DB` | ↑ upstream of a traced column |
| `--edge-down` | `#D9730D` | `#FFA34D` | ↓ downstream of a traced column |
| `--maybe` | `#8A5A00` | `#E3B341` | past an opaque node: may change (dashed) |
| `--warn-text` | `#8A5A00` | `#E3B341` | warning text (AA on the panel) |

The model graph's selection path uses the accent. NEVER BUILT is an outlined build
pill; UNKNOWN keeps the board's dashed pill. The offline page (`ods lineage view`) has
no font files, so it falls back to the system fonts.

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

### Live run on the DAG, with follow mode (#322)

The boards are in [`boards/live-run/`](boards/live-run/). `Main.dc.html` there is interactive: it plays a demo run, and the other boards reuse it in fixed states. Screenshots will be added once they are exported from the design canvas.

- **What the page shows:**
  - A **Live** overlay on Lineage while `ods state run` or `build` is running.
  - Each node shows one state: queued, running, built, failed, skipped (upstream failed) or kept (not selected in this run).
  - Nodes show their elapsed time while running, and their time taken and rows once finished.
  - The side panel shows progress, counts, what is running now, rows affected so far and an event log.
- **Follow mode:**
  - It is on by default. The camera frames every running node at once, moves at most about once a second, and never zooms below about 60%.
  - If the running nodes can't all fit at that size, the camera keeps one **focus** node until it finishes. It then moves to the running node blocking the most queued work (the nearest one on a tie).
  - Running nodes outside the view show as **edge chips**. A **minimap** shows the whole run.
  - Any pan, zoom, Fit or node click turns follow off. **Follow run** turns it back on, and so does `F` in the real page.
- **Follow scopes:**
  - The node menu (right-click, the menu key or Shift+F10) and the stats card offer "Follow this node and its downstream", plus upstream or just this node.
  - Nodes outside the scope fade. A toolbar chip clears the scope.
  - The scope is kept in the URL.
- **Node stats:**
  - Status, start and end time, time taken (compile and execute), rows affected, adapter extras, thread, relation, why it ran, and tests.
  - A stat that isn't reported shows as `—` with the reason, never as `0`. Run totals say "at least N".
- **Terminal:** the same per-node lines and summary in `ods state run`.

### Failed node: the error explained (#323)

Board: [`boards/live-run/ErrorExplained.dc.html`](boards/live-run/ErrorExplained.dc.html).

- **What a failed node shows:**
  - a plain-language headline;
  - a category;
  - how sure ODS is: **known pattern + evidence**, **known pattern** or **not recognised**;
  - **why ODS thinks so**, from its own evidence: what changed, column lineage, the manifest, source versions and run history;
  - **where** (source file and line);
  - **what to try**, with copyable commands;
  - the **impact** on downstream nodes.
- **dbt's own message** stays one click away, with literal values and SQL removed.
- **An unrecognised error** never gets a guessed cause. The card shows what ODS knows instead.

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
- Live run: dark mode, the Home "run in progress" banner, and several runs at once.
