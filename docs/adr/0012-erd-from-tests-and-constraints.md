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
  - **Inputs:** entities (id, name, kind, description, relation, typed and described
    columns) and `Fact`s (`Unique`, `NotNull`, `PrimaryKey`, `ForeignKey`, `Joined`).
    Each fact has a `Basis` (declared, tested, joined or inferred) and evidence (the
    test, constraint, config or model).
  - **Build rules:**
    - A declared primary key wins. A model's `unique_key` config (incremental models,
      snapshots) is declared: dbt merges on it.
    - A tested unique key whose columns are all tested not-null becomes the primary
      key.
    - Otherwise the smallest tested unique *combination* of columns
      (`unique_combination_of_columns`) becomes the primary key, with evidence saying
      nullability is untested. Composite grains are rarely tested column by column, and
      a single-column `unique` can't express them.
    - A single-column `unique` alone stays a nullable unique key. (dbt v2's own
      `primary_key` column counts `unique` alone as a primary key; we don't.)
    - A foreign key is one-to-one when its columns are unique, and optional unless
      they're not-null.
    - Only declared and tested keys decide cardinality, optionality and not-null. A
      guessed key never changes a tested fact.
    - Duplicate facts merge their evidence and keep the strongest basis.
  - **Relationships from joins.** Most projects don't write `relationships` tests, but
    their SQL joins entities all the time. The SQL analyzer records equi-join keys
    (`on a.x = b.y and …`, `using (…)`) traced through CTEs and subqueries to physical
    columns (`QueryLineage.join_keys`). Each becomes a `Joined` fact, one relationship
    per key (composite keys stay together), with the joining model as evidence.
    Direction and cardinality come from trusted keys on either side; when neither side
    is a trusted key the cardinality is `unknown`. This is relationship *evidence* taken
    from SQL, not lineage: rule 6 still holds, since a DAG edge alone never makes a
    relationship.
  - **Constraints** are declared facts, whether column-level or model-level (composite
    keys). A `foreign_key` names its target with `to: ref(…)`/`source(…)` plus
    `to_columns`, or with `expression: "schema.table (columns)"`, which is matched
    against the trailing parts of each entity's warehouse relation. A table that
    matches no entity, or several, is a diagnostic. dbt 2.0.5's Parquet Information
    Schema leaves column-level constraints out (`node_columns.constraints` is empty),
    so those need its `manifest.json`.
  - **Naming inference ranks trusted keys equally.** A constraint on one table and a
    test on another don't say which of them `order_id` refers to, so that stays
    ambiguous rather than picking the declared one.
  - **Filtered tests** (`config.where`) hold for part of a table only, so they are
    skipped with a diagnostic.
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
    model (`ods_test_gaps`);
  - data users get grain, join paths and SQL skeletons from the same model
    (`ods_find_data`, `ods_describe_entity`, `ods_plan_query`).
- Negative / trade-offs:
  - Projects without key tests get relationships from joins, but with unknown
    cardinality until a key is tested or declared.
  - A join in one model is taken as a relationship for all; a join on a coincidental
    column (e.g. a status code) shows up and is labelled `joined`.
  - Column types need a catalog (`dbt docs generate`).
- Follow-ups:
  - warehouse-declared constraints (#66);
  - an interactive ERD view in the explorer (#64);
  - usage-based relationship evidence (#57).
