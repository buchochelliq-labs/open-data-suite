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
dependency-free.

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
    and no credentials.
- **Reverse proxies:** `--base-path /lineage` serves at `/lineage/`, and `/lineage`
  redirects there. The page resolves `api/…` and `graph.json` relative to its own URL,
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
