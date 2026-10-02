# ADR-0009: A hostable explorer (`ods-web`) and an EDGE layer

- **Status:** Proposed
- **Date:** 2026-09-25
- **Issues:** #74 (column lineage), #95 (server mode), #96 (REST API), #102 (docs site), #107 (VS Code view); related #97 (RBAC/OIDC)
- **Deciders:** @n1ckyb

## Context
`ods lineage view` writes one self-contained HTML file with the graph embedded
([ADR-0008](0008-column-level-lineage.md)). That suits a laptop. It doesn't suit a team:
- nobody wants to email a 5 MB HTML file after every `dbt compile`;
- the page can't answer questions it wasn't built with, such as "what must run if
  this column is removed?";
- it goes stale as soon as the artifacts change.

Hosted metadata browsers exist in commercial platforms. We want a page anyone can host
themselves, with no login.

We want the same page to work in three ways, with no build toolchain and no login:
1. **Standalone:** one offline file, as today.
2. **Static site:** host it on S3, GitHub Pages, or nginx.
3. **Served:** a small server with a JSON API and live reload.

Constraints:
- **ADR-0001 layering:** modules and providers never depend on each other. A server
  needs both: providers to read dbt, and modules to answer queries.
- **ADR-0003:** presentation stays at the edge.
- **Secrets never leave config** (AGENTS rule 9), and the server must not become an
  accidental data-exfiltration endpoint.

## Options considered
### Option A — put the server in `ods-cli`
This is the smallest change. But it pulls `axum` into the CLI crate, and a later server
binary (#95) couldn't reuse it without depending on the CLI.

### Option B — a new crate, `ods-web`, in a new EDGE layer (chosen)
`ods-web` owns the page asset, static export, search, and an `axum` router. It depends on
`ods-core` and module crates (`ods-lineage`), **never on providers**. A binary supplies a
`Loader` closure that reads artifacts with whatever providers it wires, and returns a
neutral `Snapshot`. The same crate later serves State, ERD and Usage views.

### Option C — a JS single-page app (React/Vite) or DuckDB-WASM like dbt docs v2
- Pros: a rich UI ecosystem.
- Cons:
  - it adds an npm build and a supply chain to a Rust repo;
  - a WASM engine costs tens of MB;
  - it can't compute impact without re-implementing our planner in JS.

We may revisit WASM for a queryable index later (the ODS index ADR), but the page stays
free of a JS build and of runtime fetches.

*Amended 2026-09-27:* the page inlines one vendored, permissively licensed library, the
layered graph layout [dagre](https://github.com/dagrejs/dagre) (MIT, in
`crates/ods-web/assets/vendor/` with its licence). It replaces a hand-written layout. It
is checked in as released, with no npm build, and inlined into the page, so the page
still works offline and under the server's inline-only CSP. Any further vendored library
must also be permissively licensed and inlined the same way.

*Amended 2026-09-29 (#310, the dashboard shell and Home):* `ods serve` grows into the
read-only ODS Dashboard ([design](../design/dashboard/README.md)).
- **Pages:** Home is served at `<base>/`, server-rendered from view models, with its
  stylesheet and a small script inlined. The lineage explorer moves to `<base>/lineage`,
  where its relative `api/…` still resolves to `<base>/api/…`.
- **Data:** the binary still owns every provider. Its `Loader` now also fills a
  neutral `ods_web::Dashboard` on the `Snapshot`: the project, planned nodes by kind,
  the recent runs (`RunRecord`, derived from committed snapshots), the snapshot count,
  the plan against the head, opaque nodes and module status. `ods-web` depends only on
  `ods-core` types for it. The state store is opened read-only (no migration, never
  created); without one, Home shows how to record a first run. The watcher also
  watches the state database, its WAL and the source freshness results (even before
  they exist), so a new run or measurement reloads open pages.
- **Plans depend on time:** a lag tolerance can expire with no file changing, so the
  binary also supplies a `Planner` (offline, cheap) and Home plans again as of each
  request, instead of showing a plan made at load time. A timer at the earliest
  lag-tolerance deadline was the alternative; it needs the planner to report
  deadlines, and still goes stale between a deadline and the next reload.
- **API:** `/api/shell` (`ShellView`) and `/api/home` (`HomeView`) return the view
  models the page renders, at `schema_version` 1; additive fields keep it. They are
  `GET` only, like every route. Beyond loopback, `/api/home` omits the store's path and
  error text, as `/api/version` does.
- **Fonts:** IBM Plex Sans (400, 500, 600) and Mono (400, 500), Latin-1 subsets as
  released in `@ibm/plex-sans` 1.1.0 and `@ibm/plex-mono` 2.5.0, are vendored in
  `crates/ods-web/assets/vendor/fonts/` with their licence, the
  [SIL Open Font License 1.1](https://openfontlicense.org). OFL permits bundling and
  redistribution with software (the fonts may not be sold alone, and a modified font
  must be renamed; we ship them unmodified). `cargo-deny` doesn't see fonts, so this
  note is their licence record. They are served from `<base>/assets/fonts/<file>`, and
  the CSP gains only `font-src 'self'`: no font CDN, and the page works offline. An
  installed copy (`local()`) is used first.
- **Escaping:** server-rendered text and attributes go through the `html-escape` crate
  (MIT).

*Amended 2026-09-29 (#311, the State pages):* the dashboard gains Plan (with its Why
panel), Runs and one Run under `<base>/state/`.
- **Pages below the root:** the shell takes the path back to the root (`../`, `../../`)
  and prefixes every navigation link, font URL and the script's API calls with it (a
  `<meta name="ods-root">`), so pages under `state/` work under any base path. The
  CSP's `base-uri 'none'` rules out a `<base>` element. This one rule serves every page
  below the root: `state/…` (#311) and `catalog/<id>` (#313). A section may list its
  pages (`NavSection::items`), shown under it while it is current: State lists Plan and
  Runs, and History and Policies as planned; Catalog lists Models, and Freshness
  evidence and Semantic layer as planned (#309). A page with its own search (the
  Catalog) leaves the header's out (`Frame::search`).
- **Data:** `Recorded` gains an optional `History`: up to 51 committed snapshots
  (`StateSnapshot`, newest first; one more than the 50 listed, so the oldest can say
  what it replaced) and the `LastRun` kept beside the store for `ods state retry`
  (its redacted command, start time, failed and skipped nodes, scope, run id, and the
  retry commands). The binary fills both, read-only; `ods-web` stays free of providers
  and of the store. `ods-web` now depends on `ods-state` (a module, which ADR-0001
  allows an EDGE crate) for `explain` and `diff_states`, so the Why panel's chain is
  `ods state explain`'s by construction, and a run's builds are explained as
  `ods state history <node>` does. `WhyView.explanation` is the `explanation` of
  `ods state explain --output json`: it tracks that JSON's schema (the `ods_state::
  Explanation` type), and changes when it does. The watcher also watches
  `<state-db>.last-run.json`.
- **The last-run file, version 1.2 (persisted format, additive):** it now also keeps
  the `scope` the run was for and its `run_id` (the id its snapshot records), written
  once the run has built. Files at 1.0 and 1.1 still read, without them. The file is
  kept per state database, which several targets may share, so the pages show the last
  run only when its scope is the page's; a file without a scope is shown apart, as
  possibly another target's, and never tied to a run. A run is tied to the snapshot
  that records its run id; when no listed snapshot does, the page says it *probably*
  recorded nothing, marked inferred (a clock step or a later `ods state record` could
  make that wrong).
- **The last-run file, version 1.3 (persisted format, additive, #321):** `args` no
  longer holds the values of options that may carry secrets (`--vars`) or what followed
  `--`; the new `withheld` lists which were given (`["vars", "--"]`). No digest of them
  is kept either: a digest of a short secret can be reversed by guessing.
  `ods state retry` takes them again (`--vars`, `-- …`) and refuses without them, or
  with ones the run didn't have. Every option of the commands that keep their line is
  classed as kept or withheld, and a test fails on one that isn't. Files at 1.0–1.2
  still read: their withheld values are dropped as they are read (the dashboard), and
  the file is rewritten without them the first time `retry` reads it, a dry run
  included. One with an option or a word this build doesn't know is removed, as which
  words are values can't be told, rather than kept or retried with a wider selection.
  The dashboard offers the retry with placeholders for what to give again
  (`ods state retry --vars '<value>' -- '<dbt arguments>'`).
- **Secrets (AGENTS rule 9):** the command line reaches `ods-web` redacted by the CLI:
  option names are kept, and only the values of `--select`, `--exclude`,
  `--resource-type`, `--exclude-resource-type`, `--target`, `--environment` and
  `--dbt-output`; every other value (e.g. `--vars`) and everything after `--` reads
  `<redacted>`. The file itself keeps what was typed, as `ods state retry` needs it,
  and its `Debug` redacts it the same way.
- **Planning is shared and bounded:** every page asks `Dashboard::plan_at`, which plans
  at most once per 30 s time bucket and reload (the memo lives on the reloaded facts,
  so a reload starts afresh); lag tolerances are whole minutes or more. Pages that may
  plan are built on a blocking thread. Node names from the graph are built once per
  reload.
- **API:** `/api/state/plan`, `/api/state/plan/<node>`, `/api/state/runs` and
  `/api/state/runs/<run_id>` return the view models the pages render (`PlanView`,
  `WhyView`, `RunsView`, `RunPageView`) at `schema_version` 1, `GET` only; beyond
  loopback without paths, error text or the last run's options.

*Amended 2026-09-29 (#313, the Catalog and model pages):*
- **Data:** the binary also fills a neutral `ods_web::catalog::CatalogInput` on the
  `Dashboard`: each node's id, name, type, language, layer, materialization, tags,
  description, relation, file, parents, columns (type only when recorded, and whether
  it came from the warehouse or was declared), code as written, and tests; plus
  each node's last successful build from the latest snapshot, with the snapshot that
  recorded it. The binary decides what a layer is (the model's first folder under the
  model paths, from the artifacts) and says so; `ods-web` names no build tool. Decisions
  come from the shared `Dashboard::plan_at` (see *Planning is shared and bounded*
  above), on a blocking thread as the State pages do; lineage confidence from the graph
  document the server already holds. The binary reads the latest snapshot and history
  once and derives both the planner's input and the last builds from that read, so the
  builds shown and the decisions can't rest on different snapshots.
- **No compiled code (AGENTS rule 9):** compiled SQL can contain values resolved from
  `env_var()`, `var()` or macros, including credentials, so the binary never puts it in
  `CatalogInput` and no view model has it; only the raw code, with its templating
  unresolved, is served. The Code tab points to `target/compiled/` instead.
- **Tests vouched for only while unchanged:** a build's test record keeps the digest of
  the checks that passed. The binary compares it with the node's checks now, using the
  planner's `checks_digest` and `NodeState::is_tested_with`, and passes the result
  (`LastBuild::checks_current`), so a test added or edited since never reads as passed.
- **Offline decisions:** this plan checks no relation, so a reuse is shown as taken on
  trust ("its relation isn't checked by this plan; it is when a run starts"), and the
  view model carries it (`relations_checked: false`, `caveats`).
- **Pages:** `<base>/catalog` and `<base>/catalog/<id>`, below the root by the rule
  above (#311). Filters are a plain `GET` form, so the URL is the state and the page
  works without script. Model pages link to Why (`state/plan?node=`), and the State
  pages link node names to their model pages.
- **Inline scripts:** besides the shared script, these pages add small static inline
  scripts (submit a facet form on change and restore focus; bind `/` to the Catalog's
  search; filter columns; copy the page's link). They embed no data, so the existing
  `script-src 'unsafe-inline'` covers them; a CSP hash per script is a possible
  tightening, not needed for them to work.
- **API:** `/api/catalog` (`CatalogView`, same query as the page) and
  `/api/catalog/<id>` (`ModelView`, every tab), `GET` only, at `schema_version` 1.
  Beyond loopback they omit file paths and error text.

*Amended 2026-09-29 (#312, the Lineage page):* the explorer at `<base>/lineage` moves
into the dashboard's shell (a root page, `Frame::search` off: its toolbar has the
page's search), with a State overlay.
- **One explorer, two pages:** the explorer's script and stylesheet
  (`assets/lineage.js`, `assets/lineage.css`) are shared. Served, the page is the shell
  around them, with the graph, the overlay and a deep-linked selection embedded as
  JSON (`<` escaped, as before). Offline (`ods lineage view`, `--site`), the same
  explorer has a small header instead of the shell, and the graph only: no overlay, no
  impact, no font files. No library is added.
- **Overlay contract:** `ods_web::lineage::LineageOverlay` at `schema_version` 1,
  served at `/api/lineage/overlay` (`GET` only). Its decisions come from the shared
  `Dashboard::plan_at` (see *Planning is shared and bounded* above), on a blocking
  thread, so the page and the API show the same plan as Home, State and Catalog and as
  `ods state plan`. Each node gets a `Decision`: `build`, `reuse`,
  `never_built`, or `unknown` when the evidence to reuse it is missing or the plan
  couldn't be made (never shown as reuse, AGENTS rule 3), with its reason chain (rule
  4). Without a state store every node is `never_built`. Sources have no decision.
  Beyond loopback, error text is omitted.
- **Links out:** each node links to its Model page, `catalog/<id>`, and to its decision
  on the State plan page, `state/plan?node=<id>`, relative to the dashboard's root,
  with the id percent-encoded except for RFC 3986's unreserved characters.
  `<base>/lineage?node=<id>` selects a node.
- **Edges are the DAG's** (AGENTS rule 6): the exported graph now also links a node to
  the parents it declares, so an opaque node (a Python model) is no longer drawn apart;
  impact already read them. Each `NodeEdge` says how it is known, `via: "sql"` or
  `"declared"` (additive: the graph stays at `schema_version` 1); declared-only edges
  are drawn dashed. They are drawn and described as "reads", never as relationships.
- **Reuse on trust** (rules 3 and 4): as for the Catalog (#313), this plan checks no
  relation. Each reused node says so (`relation`), and the overlay carries the same
  warning as `ods state run`; nothing calls it checked. The Why tab lists the compared
  fingerprint components from the latest snapshot in the `History` (#311).
- **Column traces stop visibly:** at an opaque node the explorer can't follow a
  column, so it names the stop and shows every node past it as *may change*, never as
  unaffected.

## Decision
- **New EDGE layer** between PROVIDER and BINARY in `scripts/check-layering.py`. EDGE
  crates may depend on anything up to MODULE, and not on providers. `axum` is confined to
  `ods-web` (`CONFINED_EXTERNAL`).
- **`ods-web`** exposes:
  - `standalone_page(doc)`: embedded graph, `<meta name="ods-source" content="embedded">`.
    `<` is escaped as `<`, so data can't close the `<script>` element.
  - `export_site(doc, dir)`: `index.html` (fetches `graph.json`) and `graph.json`.
  - `router(snapshot, base)` and `serve(options, loader, ready)`: the served page embeds
    the first paint and then talks to the API.
- **HTTP API v1** (read-only, JSON, `GET` only; `/api/version` reports `api: 1`):

  | Route | Answer |
  |---|---|
  | `/api/version` | API version, snapshot generation, source, last reload error |
  | `/api/graph` | the `GraphDocument` (the same contract as `ods lineage graph --format json`) |
  | `/api/search?q=&limit=` | node and column hits: prefix first, then shorter labels, then by label |
  | `/api/node?id=` | a node (by id or unique name) plus its analyzed lineage |
  | `/api/impact?node=&column=&kind=` | `Change` plus `Impact`, with reasons and pruned readers |
  | `/healthz` | `ok` |
  | `/api/shell`, `/api/home` | the dashboard's view models (amended 2026-09-29) |
  | `/api/state/plan`, `/api/state/plan/<node>`, `/api/state/runs`, `/api/state/runs/<run_id>` | the State pages' view models (amended 2026-09-29, #311) |
  | `/api/lineage/overlay` | the Lineage page's State overlay (amended 2026-09-29, #312) |

  Breaking changes bump the API version. Additive fields don't.
- **Safe by default:**
  - binds `127.0.0.1`, and on loopback accepts only `Host: localhost`, `127.0.0.1` or
    `[::1]` (421 otherwise), to mitigate DNS-rebinding attacks from web pages;
    `--allow-host` names a reverse proxy's host;
  - beyond loopback, `/api/version` omits local paths and error text;
  - impact on a modified or removed column that the node doesn't have is a 400, never
    "nothing affected" (AGENTS rule 3);
  - `--base-path` is validated to plain segments;
  - binding elsewhere prints a warning that there is no authentication, so it should
    sit behind a proxy that has some (RBAC/OIDC is #97);
  - every response carries a strict CSP (`default-src 'none'`, `connect-src 'self'`,
    `frame-ancestors 'none'`), `nosniff`, `no-referrer` and `no-store`;
  - no route writes anything;
  - the snapshot holds only lineage metadata: names, columns and edges, no SQL results
    and no credentials. The SQL analyzed is compiled and can hold resolved values, so
    no compiled SQL is served, and analyzer diagnostics name constructs and positions,
    never quote the SQL (amended 2026-09-29, #312).
- **Reverse proxies:** `--base-path /ods` serves at `/ods/` (the explorer at
  `/ods/lineage`, amended by #310), and `/ods` redirects there. The page resolves `api/…` and `graph.json` relative to its own URL,
  so the same asset works at any prefix.
- **Live reload:** the server polls each artifact's mtime and length every second (no
  `notify` dependency), and loads a change only once it has held for a whole tick, so a
  build still writing files isn't read half-way. It rebuilds on a blocking thread with a shared content-addressed cache, so
  only changed models are re-analyzed. A failed reload **keeps the last good snapshot**
  (AGENTS rule 5, in spirit) and reports the error in `/api/version`. The page is rendered with its
  generation and polls `/api/version`, reloading when it changes. Ctrl-C and SIGTERM stop
  the server gracefully.
- **CLI:**
  - `ods lineage view --site DIR` writes the static site;
  - `ods serve [--host] [--port] [--base-path] [--no-watch]` serves it.

  Both reuse `ods lineage`'s artifact options.

## Consequences
- Positive:
  - one page, three deliveries;
  - teams can host lineage anywhere, with no login and no vendor;
  - the API is the seam for the VS Code view (#107), an MCP server, and #95/#96;
  - providers stay out of `ods-web`.
- Negative / trade-offs:
  - Tokio and `axum` enter the dependency graph (both MIT).
  - The binary grows by roughly 1 MB.
  - Polling costs a `stat` of four files per second.
  - The served mode has no authentication until #97.
- Follow-up issues:
  - an ODS metadata index (search across State, ERD and Usage);
  - an MCP server over the same API;
  - an authentication/RBAC middleware (#97);
  - a server binary for shared deployments (#95).

## References
- [ADR-0001](0001-monorepo-architecture-and-module-boundaries.md) layering,
  [ADR-0003](0003-cli-presentation-boundary.md) presentation boundary,
  [ADR-0008](0008-column-level-lineage.md) column lineage and the `GraphDocument`.
