# ADR-0003: CLI presentation boundary and rs-rich-cli

- **Status:** Proposed
- **Date:** 2026-09-24
- **Issues:** #108 (also #6, #21, #22, #85, #107)
- **Deciders:** @n1ckyb

## Context
Every ODS module has to show structured results to three kinds of consumer:

1. **People at a terminal.** They want State plans, explanations, ERD summaries and review
   findings as readable tables, trees, panels and diffs, with progress on long runs.
2. **Scripts, CI and logs.** They want stable, uncoloured, line-oriented text that is easy to
   grep and diff (#85, #88).
3. **Machines** (the VS Code extension #107, CI PR reports, the REST API #96). They want a
   versioned JSON contract.

AGENTS.md rule 7 says presentation stays separate from logic. Rule 4 says every decision
must be explainable in both human and JSON form. Issue #108 asks us to standardise on
[`buchochelliq-labs/rs-rich-cli`](https://github.com/buchochelliq-labs/rs-rich-cli)
rather than build our own terminal renderer. It also asks us to use ODS as a real consumer
of rs-rich-cli ("dogfood" it) and to report the primitives it is missing.

### What rs-rich-cli offers (surveyed 2026-09-24, updated for the 0.0.11 release)
- A Rust port of Python `rich`. The library is the `rs-rich` package, imported as `rich`;
  its extensions are `rs-rich-ext`. It is MIT-licensed.
- It is published on crates.io. The survey started on `rs-rich` 0.0.6. The proof of
  concept moved to **0.0.7**, the core library of the 0.0.11 release cohort, once it was
  published. Pre-1.0 `0.0.x` versions may break the API in every release, and Cargo's
  `^0.0.x` requirement pins the exact patch version.
- It has `Console` with a builder that sets `width`, `force_terminal`, `color_system`
  and `no_color`. It honours `NO_COLOR` and detects whether output goes to a terminal.
  From 0.0.7 its colour detection also honours `TERM=dumb`/`unknown` (in 0.0.6 that only
  affected the pager). It can capture output (`capture`) and export it (`export_text`,
  `export_svg`), which lets tests pin their output exactly.
- Renderables: `Table`, `Tree`, `Panel`, `Rule`, `Columns`, `Text`, markup, `Syntax`,
  `Markdown`, JSON/pretty printing, `Progress`, `Status` and `Live`. `rs-rich-ext` adds
  diff, badges, diagnostics, a hyperlink helper and a clap help adapter.
- It requires **Rust 1.90**; ODS currently requires 1.85. Its CI covers Linux, with Windows
  exercised manually. macOS is untested, and the legacy Windows `cmd.exe` console is not
  supported.
- It has 6 direct dependencies (`syntect`, `fancy-regex`, `pulldown-cmark`, `serde_json`,
  `terminal_size`, `anstyle-query`), and none of them is optional. 0.0.6 pulled in 76
  crates in total; 0.0.7 loads only syntect's bundled dumps, which removes 11 of them,
  including `yaml-rust` and `plist`.

### Spike (scratch crate, not committed)
A release binary that renders one `Table` through `export_text` with `no_color`
produced output that is correct and can be pinned exactly (fixed width).

| | Release | Stripped |
|---|---|---|
| Empty Rust binary | 0.44 MB | 0.35 MB |
| Plus `rs-rich` 0.0.6, one table | 4.25 MB | 3.41 MB |

Most of the growth comes from `syntect` and its bundled syntax and theme definitions.
`cargo deny check licenses` with ODS's `deny.toml` accepted every crate in the
dependency tree.

## Options considered

### Option A: modules call rs-rich directly
Each module crate builds rich renderables and prints them itself.
- ✅ Least code.
- ❌ Breaks AGENTS.md rules 2 and 7. Terminal code would sit in domain crates.
- ❌ JSON and plain output would be separate code paths per module, and would drift.
- ❌ Every rs-rich 0.0.x API break would touch every module.

### Option B: typed result models, with a presentation layer at the CLI edge (chosen)
Modules return typed, serialisable **result models**. `ods-cli` owns presentation. It maps
each result to a small ODS-owned **view tree**, and interchangeable backends render that
tree (rich, plain). JSON output serialises the result model directly.
- ✅ Domain crates never see terminal APIs. The server and the LSP reuse the same result
  models.
- ✅ rs-rich is confined to a single backend module, so an API break touches one place.
- ✅ JSON stays a real, versioned data contract and is not a by-product of rendering.
- ❌ One extra mapping step per command (result model to view tree).
- ❌ The view tree duplicates a small subset of rich's vocabulary.

### Option C: a full ODS rendering framework (no rs-rich)
- ✅ No external API churn.
- ❌ Rebuilds layout, width handling, Unicode cell widths and colour detection that
  rs-rich already has. #108 rules this out explicitly.

### Option D: render JSON first, then turn the JSON into a view generically
- ✅ One code path.
- ❌ A generic JSON-to-table view gives poor human output (no grouping, emphasis or reason
  chains). It also couples the JSON shape to how things look.

## Decision
Adopt **Option B**. The rules below define the boundary.

### 1. Three layers
```mermaid
graph LR
  mod["module crate<br/>(ods-state, …)"] -- "result model (Serialize)" --> cli
  subgraph cli["ods-cli (composition root)"]
    present["present::command<br/>result → ViewNode tree"]
    rich["backend::rich<br/>(rs-rich)"]
    plain["backend::plain"]
    json["emit: JSON envelope<br/>(serde_json)"]
  end
  present --> rich
  present --> plain
  cli -- "result model" --> json
```

- **Result model.** This is a `serde::Serialize` type owned by the module (for example
  `ods_state::ExecutionPlan`, or a plan-explanation type). It follows AGENTS.md
  conventions: `#[non_exhaustive]`, snake_case, deterministic ordering. It carries reasons
  and evidence as data, never as formatted strings.
- **View tree.** `ods-cli` defines a small `ViewNode` enum in `ods-cli::present`:
  `Heading`, `Paragraph`, `KeyValue`, `Table`, `Tree`, `Notice { level }` and `Group`.
  Further nodes, such as `List` and `Diff`, are added when a command first needs them.
  Views carry **semantic styles** (`Emphasis`, `Muted`, `Added`,
  `Removed`, `Warning`, `Error`, `Success`, `Code`) and never raw colours or markup.
  The mapping from result to view is a pure function and is unit-tested.
- **Backends** render a whole view to a `String`, which `present::emit` writes to stdout.
  Command results are small, and rs-rich can only be pointed at an ODS-chosen stream by
  capturing its output. There is no `Renderer` trait yet; one is introduced if a streaming
  backend (for example live progress) needs it.
  - `rich`: maps `ViewNode` to `rs-rich` renderables. Styled text is built from spans with
    `Text::append`, so **user data is never parsed as rich markup**. Semantic styles
    resolve through a `rich::theme::Theme` of named styles (`ods.added`, `ods.warning`,
    …), so colours live in one place and can be overridden later.
    **This is the only module that imports `rich`/`rich_ext`.**
  - `plain`: ODS's own implementation, with no ANSI escapes, no box drawing and ASCII
    only. It prints `key: value` lines, tab-separated tables with a header row, and
    indented trees. Its output is stable across terminals and platforms.
  - JSON (in `present::emit`): serialises the result model inside the envelope, not the
    view.
  - Both text backends replace terminal control characters (ESC and other C0/C1 controls)
    in displayed text, because node names, SQL and warehouse errors are untrusted data.

The view tree stays inside `ods-cli` until a second consumer needs it, such as
`ods-server` rendering HTML. At that point it is extracted to a foundation-layer
`ods-view` crate, which requires an amendment to ADR-0001.

### 2. Output modes and flags (global, implemented in #6)
| Flag | Values | Default |
|---|---|---|
| `--output` / `-o` | `human`, `plain`, `json` | `human` when stdout is a terminal, otherwise `plain` |
| `--json` | shorthand for `--output json` | |
| `--color` | `auto`, `always`, `never` | `auto` (honours `NO_COLOR`, `TERM=dumb` and redirection); `always` overrides `NO_COLOR` |
| `--width` | integer | detected; `100` when not a terminal |

- **Streams.** Results go to **stdout**. Logs, progress, spinners and warnings go to
  **stderr**. Progress and spinners appear only in `human` mode when stderr is a
  terminal. JSON mode never writes anything else to stdout.
- **Exit codes** are unaffected by the output mode. #6 defines them.

### 3. JSON contract
Every `--json` response is a single envelope object:
```json
{
  "schema_version": {"major": 0, "minor": 1},
  "command": "state.plan",
  "ods_version": "0.1.0",
  "result": { "...": "command-specific result model" },
  "diagnostics": [{"level": "warning", "code": "ODS-W0001", "message": "..."}]
}
```
- `schema_version` versions the envelope and the result models together, with the same
  compatibility rules as `ods_core::SchemaVersion`.
- A command's `result` shape is a public contract. Breaking changes need a major version
  bump and a CHANGELOG entry (#101).
- A JSON Schema is generated for each command. We will evaluate `schemars` against a
  hand-written schema in #6.

### 4. How rs-rich is consumed
- The dependency is `rs-rich` from **crates.io**, at a published version only. Git
  dependencies and other registries are rejected by `deny.toml` `[sources]`; a path
  dependency would be caught in review, because cargo-deny allows workspace paths. It is
  declared once in `[workspace.dependencies]` and used only by `ods-cli`. `rs-rich-ext` is added only when
  we need a specific feature (for example `diff`).
- **ODS's MSRV rises from 1.85 to 1.90.** This lands in the PR that adds the dependency;
  `Cargo.toml` and the CI `msrv` job must change together.
- Upgrades are deliberate. Because `^0.0.x` pins the patch version, each upgrade is its own
  PR, and it must update the `rich` backend snapshots.
- `scripts/check-layering.py` gains a check that no crate other than `ods-cli` depends on
  `rs-rich*`.
- **Data never reaches rs-rich as a string.** From 0.0.7, a plain string passed as a table
  header, cell or tree label is parsed as markup. The rich backend therefore passes
  `Text` values everywhere and prints table titles as their own line. A regression test
  pins this behaviour.

### 5. Testing
- `insta` snapshots for every command in **`json` and `plain`** modes, which are the
  contracts. They live in `crates/ods-cli/src/snapshots/`. The build's own `ods_version`
  is replaced with `[ods-version]`, so releases don't churn contract snapshots.
- `rich` mode snapshots use a pinned `Console` (`width(100)`, `force_terminal(true)`,
  `ColorSystem::Standard`) captured via `capture`. There are fewer of them, and they live
  in `crates/ods-cli/src/snapshots/rich/`, apart from the contract snapshots, because
  rs-rich upgrades may legitimately change them.
- The result-to-view mapping gets unit tests. The backends get tests with fixture view trees.
- The CI matrix already covers macOS and Windows, which also covers rs-rich on the
  platforms its own CI does not test.

### 6. Missing features and issues to report to rs-rich-cli
To be filed as issues in `buchochelliq-labs/rs-rich-cli`. Status is as of `rs-rich` 0.0.7.

**Open**
1. **Optional heavy dependencies.** Put `syntect` (`Syntax`) and `pulldown-cmark`
   (`Markdown`) behind default-on cargo features. That would cut roughly 3 MB from
   consumers that don't need them. It would also remove `bincode` 1.x
   (RUSTSEC-2025-0141, flagged as unmaintained, no known vulnerability), which `syntect`
   still pulls in. ODS ignores exactly that advisory ID in `deny.toml` and will remove
   the ignore when this is fixed. The same change would drop a duplicate `fancy-regex`:
   `rs-rich` uses 0.19 while `syntect` brings 0.16, so two regex engines are compiled in.
2. **A lower MSRV, or a documented MSRV policy** for the library crate, separate from the
   CLI's image and network features. The 1.90 floor is driven by the CLI tree.
3. **macOS in CI**, so downstream consumers get a support guarantee on that platform.
4. **A 0.1 / API-stability roadmap**, so ODS can move off exact patch pins.
5. **The strings-as-markup change in 0.0.7 compiles silently.** Plain strings passed as
   table headers, cells or tree labels changed from literal text to markup, but code
   written for 0.0.6 still compiles. Any caller passing data is now open to markup
   injection, and only a behavioural test catches it (ODS's did). Suggest making the
   change visible at compile time, for example by removing the implicit
   `From<&str>/From<String> for Cell` conversions or adding explicit
   `Cell::markup`/`Cell::plain` constructors, or at least flagging it as a security note
   in the migration guide. (Found while reviewing the 0.0.11 release.)
6. **`markup::escape` is not round-trip safe for a trailing backslash.** Escaping `a\`
   and then parsing it renders `a\\`. The 0.0.11 migration note recommends `escape` for
   literal data, so either this should be fixed or the note should recommend `Text`.
   It may be deliberate parity with Python `rich`; not yet checked. (Found while
   reviewing the 0.0.11 release.)
7. **No literal-text table title.** `Table::title` takes only a markup string, so a title
   built from data needs escaping, which runs into item 6. ODS prints the title as its
   own line instead. (Found while reviewing the 0.0.11 release.)

**Resolved in 0.0.7**
- *Styled table cells and tree labels* (reported during the proof of concept):
  `add_row_text`, `add_column_text` and `Tree::new`/`add` taking `impl Into<Cell>` now
  carry `Text`, so the rich backend keeps tones in cells and tree labels.
- *`TERM=dumb` in colour detection* (reported during the proof of concept): now
  honoured, as is `TERM=unknown`. ODS keeps its own check so the §2 contract holds
  independently of upstream and stays unit-testable.
- *`yaml-rust` (RUSTSEC-2024-0320)* is no longer in the tree, and its ignore was removed.

Named semantic styles are *not* a gap: `rich::theme::Theme` already maps style names to
styles.

## Consequences
- **Positive:**
  - Domain crates stay free of terminal code.
  - JSON is a real, versioned API that the VS Code extension, CI reports and the server
    can build on.
  - rs-rich churn is isolated to one module, and the exit strategy is to swap that one
    backend.
  - Plain output is stable, which suits CI logs.
- **Negative / trade-offs:**
  - The MSRV rises to 1.90.
  - The `ods` binary grows by about 3 MB (acceptable for a developer tool; point 6.1
    above would recover it).
  - Each command needs a presentation mapping.
  - Two sets of snapshots have to be maintained.
- **Follow-up work:**
  - **#6:** global output flags, the envelope, exit codes and stream discipline.
  - **Proof-of-concept PR for #108:** `ods-cli::present` plus the `rich`/`plain`/`json`
    backends, rendering `ods version` and a fixture `ExecutionPlan`, with the MSRV bump.
    It will re-render the real `ods state plan`/`explain` once #20 and #21 land.
  - Extend `scripts/check-layering.py` to confine `rs-rich*` to `ods-cli`.
  - File the issues listed in section 6 in rs-rich-cli.

## References
- #108; AGENTS.md rules 2, 4 and 7; ADR-0001 (module boundaries); ADR-0002 (stack)
- rs-rich-cli: `README.md`, `docs/getting-started.md`, `crates/rich/src/console.rs`
- Python `rich` documentation (upstream API that rs-rich ports)
