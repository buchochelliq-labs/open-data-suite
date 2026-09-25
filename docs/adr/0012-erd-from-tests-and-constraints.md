# ADR-0012: An ERD from tests and constraints, with evidence

- **Status:** Proposed
- **Date:** 2026-09-25
- **Issues:** #60 (ERD domain), #61 (dbt ERD provider), #62 (relationship inference), #63 (render/export), #65 (`ods erd`); #169 (the MCP tool)
- **Deciders:** @n1ckyb

## Context
Agents and people need to know which columns identify rows and which point at other
entities. They need this to write joins, tests and documentation. dbt has no ERD. dbt
projects already *assert* keys through data tests and model contracts:
- `unique` and `not_null`;
- `relationships`;
- `dbt_utils.unique_combination_of_columns`;
- `primary_key` and `foreign_key` constraints.

The rest can be guessed from naming (`id`, `customer_id`). AGENTS.md rule 6 says an ERD
is not lineage, and rules 3 and 4 say a guess must never be presented as fact.

## Decision
- **A new module crate, `ods-erd`,** provider-neutral, and independent of lineage.
  - **Inputs:** entities (id, name, kind, typed columns) and `Fact`s (`Unique`,
    `NotNull`, `PrimaryKey`, `ForeignKey`). Each fact has a `Basis` (declared or
    tested) and evidence (the test or constraint).
  - **Build rules:**
    - A declared primary key wins.
    - A tested unique key whose columns are all tested not-null becomes the primary
      key.
    - `unique` alone stays a nullable unique key. (dbt v2's own `primary_key` column
      counts `unique` alone as a primary key; we don't.)
    - A foreign key is one-to-one when its columns are unique, and optional unless
      they're not-null.
    - Duplicate facts merge their evidence and keep the strongest basis.
  - **Unknown entities or columns** become diagnostics. Nothing is silently dropped.
  - **Inference is opt-in** (`--infer`) and labelled `inferred`:
    - `id` or `<entity>_id` is proposed as an entity's key;
    - `<x>_id` is proposed as a reference to the one entity with that key;
    - targets with a tested or declared key beat guessed ones;
    - ambiguity is reported, not guessed.
  - **Output:** `Erd` JSON (`schema_version` 1), Mermaid `erDiagram` (inferred
    relationships dotted), and Graphviz DOT (dashed). There is also a focus filter (an
    entity plus N relationship hops) and a "connected only" view.
- **Reading dbt.** `ods-provider-dbt` reads data tests (name, namespace, column,
  attached node, arguments), model and column constraints, declared column types, and
  catalog column types. It reads them from `manifest.json` v11/v12, `catalog.json` v1,
  and dbt v2's Information Schema (`dbt.data_tests`, `dbt.edges`, `dbt.node_columns`).
  Tests on the fixture read identically in all three.
- **The CLI adapter** (`ods erd generate`) maps dbt specifics to facts. The target of a
  `relationships` test is its other dependency, or else its `to: ref(...)`/`source(...)`.
  The other `ods erd` subcommands remain planned for M3.

## Consequences
- Positive:
  - an ERD from what the project already tests, with each edge's evidence;
  - agents get keys and joins without guessing (`ods_erd`), and test gaps from the same
    model (`ods_test_gaps`).
- Negative / trade-offs:
  - Projects without tests get sparse diagrams until they opt into inference.
  - Column types need a catalog (`dbt docs generate`).
- Follow-ups:
  - warehouse-declared constraints (#66);
  - an interactive ERD view in the explorer (#64);
  - usage-based relationship evidence (#57).
