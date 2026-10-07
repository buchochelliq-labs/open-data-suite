# ADR-0031: Plugins: an in-process registry and out-of-process plugins

- **Status:** Proposed. Phase 2 is built (§7): `Ods`, `Plugins` and `WarehousePlugin` in `ods-cli`, with Databricks registered as a built-in; `[health.plugins.<id>]`; plugins listed in `ods version` and `ods doctor` (`capabilities.plugins`); and `examples/custom-ods`, built in CI. Phase 5 has begun: features are detected (§3c), and links, observed lineage, error patterns and the dialect come from the warehouse plugin (§3a); parents and `[warehouses.<kind>] extends` apply to error patterns and the dialect (§3b). Not built yet: per-warehouse probe SQL (§3b), `ods plugin list`/`show`, DuckDB (5); script checks (3); out-of-process plugins (4).
- **Date:** 2026-10-06
- **Issues:** #392 (phase 6: plugins), #387 (pluggable source-version providers), #415
  (the whole warehouse plugin, dispatch, detected features: §3a–§3c)
- **Deciders:** @n1ckyb

## Context
ODS has contracts that third parties can implement. Each has a fake and a conformance
suite (ADR-0006, #99). What it doesn't have is a way to **plug the result in**:
- **Health checks:** the engine accepts any `HealthCheck` (`HealthReport::with_check`,
  ADR-0030 §5), but only code in this repository can call it. ADR-0030 lists plugins as
  tier 5, "registered like any provider", through #387's loader.
- **Source versions (#387):** `ods state build` reads Delta table versions on
  Databricks. `crates/ods-cli/src/commands/state_versions.rs` hard-codes that the dbt
  adapter `databricks` uses `DeltaVersions`. A provider for Snowflake or Iceberg can
  implement `ChangeProvider` and pass its suite, but has no way into `ods`.
- **Login checks:** `ods health check` likewise hard-codes `UnityCatalog` as the
  `relation_privileges` for `databricks` (ADR-0030 §4e).
- **ADR-0006 §2** decided `Registry` and `ProviderFactory`, which create providers
  from `[providers.<name>]` by `kind`. They exist in `ods-sdk` but are unused: the CLI
  builds every provider directly.

Constraints:
- **No vendor logic in core** (rule 1): the CLI is the composition root, and only it
  maps a project's warehouse to a provider.
- **Conservative** (rule 3): a plugin that fails, times out or answers badly gives
  *unknown*, never a pass or a reuse.
- **Secrets** (rule 9): ODS never hands a plugin a resolved secret.
- **Trust** (ADR-0030 §4b): cloning a repository must never be enough to run its code.
- **Versioning** (ADR-0006 §6, ADR-0019): a plugin built against another contract
  version is refused, not half-run.

## Options considered
### Option A: In-process only (a custom `ods` binary)
A third party writes a small binary crate that depends on `ods-cli` as a library,
registers its `HealthCheck`s and providers, and runs the CLI.
- **Pros:** type-safe. The conformance suites run as ordinary Rust tests. No wire
  protocol, no process, no trust question: whoever builds the binary chose its code.
- **Cons:** needs Rust and a custom build. Every ODS release needs a rebuild, since SDK
  0.x versions must match exactly. Not usable with the released `ods`.

### Option B: Out-of-process only (executables speaking JSON)
Plugins are executables, declared in configuration, that speak a versioned JSON
protocol over stdin and stdout.
- **Pros:** any language, and they work with the released binary.
- **Cons:** a protocol to version, trust to manage (like scripts), and the conformance
  suites must run against a subprocess. In-process Rust users pay serialization and
  lose types for nothing.

### Option C: Dynamic libraries (`cdylib`, `libloading`)
- **Cons:** Rust has no stable ABI, so a plugin must be built with the same compiler and
  the same SDK as the host. It is unsafe code, and a crash takes `ods` down. That is the
  cost of Option A without its type safety. Rejected.

### Option D: WebAssembly components
- **Pros:** sandboxed, portable, any language that targets WASI.
- **Cons:** a large runtime dependency (`wasmtime`), and WASI networking is still young.
  Most plugins here need the warehouse, which a sandbox would block. Revisit when a
  plugin needs isolation more than access.

### Option E: Both A and B (chosen)
In-process first, then out-of-process, built on the script protocol (ADR-0030 §4), so
it is designed once.

## Decision
### 1. One plugin set, two ways in
The CLI (the composition root, ADR-0001) holds a **plugin set**, `ods_cli::Plugins`,
that every command reads. It has one registry per pluggable contract:

| Contract | Registry | Selected by |
|---|---|---|
| `health_check` (ADR-0030) | health checks, by check id | always run, tuned by `[health.plugins.<id>]` (§4) |
| `changes` (ADR-0022, #387) | source-version providers, by warehouse | the project's warehouse (§3) |
| `relation_privileges` (ADR-0030 §4c) | login checks, by warehouse | the probe target's warehouse (§3) |

More contracts join the table as they become pluggable. The built-ins (Databricks'
`DeltaVersions` and `UnityCatalog`) register in it like any plugin, so the hard-coded
mappings go away.

### 2. In-process plugins: a custom `ods`
- **Library:** `ods-cli` already runs the whole CLI in-process (`app::run`). It gains a
  builder:

  ```rust
  fn main() -> std::process::ExitCode {
      ods_cli::Ods::new()                       // the built-in plugins
          .health_check(origin!(), Arc::new(PiiTagged))?   // a HealthCheck
          .warehouse(SnowflakeVersions::factory())? // providers for one warehouse
          .run()                                // real args, streams and environment
  }
  ```

  The released `ods` binary is `Ods::new().run()`.
- **Registration** follows ADR-0006 §2's rules:
  - **Versions:** an in-process plugin is compiled against the same `ods-sdk` as the
    host. One built against another SDK doesn't link, since its traits are other types,
    so the compiler makes the version check. `Contract::accepts` is for out-of-process
    plugins (§5).
  - **Duplicates:** a plugin is refused when its id or warehouse is already taken. A
    custom build replaces a built-in only by saying so (`replacing`), never by
    registration order.
- **One set per process:** `Ods::run` installs the set once, as logging is set up once,
  and every command reads it. Without a custom build, the set is the built-ins.
- **Visibility** (rule 4): `ods version --json` and `ods doctor` list every plugin,
  built in or added: its contract, version, id or warehouse, and the crate that
  registered it. A custom binary never passes for the released one.
- **Conformance:** the suites already run from a provider's own tests (`docs/plugins.md`).
  An example crate outside the workspace (`examples/custom-ods`) builds a custom `ods`
  with a health check and a source-version provider, runs both suites, and is built in
  CI. That is #387's "a provider written outside this repository".

### 3. Choosing by warehouse, never by vendor in core
A warehouse plugin is a factory: given the project's **warehouse kind** (for dbt, the
manifest's `adapter_type`) and the dbt executor's `RelationProbe` (the connection dbt
already has, ADR-0022 §1), it returns the providers it offers for that warehouse.

```rust
pub trait WarehousePlugin: Send + Sync {
    fn info(&self) -> PluginInfo;                // id, contract versions, crate
    fn warehouse(&self) -> &str;                 // e.g. "databricks"
    fn changes(&self, probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn ChangeProvider>>;
    fn privileges(&self, probe: Arc<dyn RelationProbe>)
        -> Option<Arc<dyn PrivilegedProbe>>;     // ods_sdk: probe + relation_privileges
}
```

- Each method has a default of `None`, so a plugin offers only what it has.
- **Selection** is an exact string match on the warehouse kind, in the CLI only. There's
  one plugin per warehouse: two plugins that both offer source versions for the same
  warehouse would make the choice depend on what was registered.
- **None** keeps today's behaviour: no source versions means `sources.json` only;
  no login check means probes need `--allow-elevated-login` (ADR-0030 §4c).
- Whatever the plugin answers still goes through the existing rules. Inexact versions
  never allow reuse (ADR-0022), and every probe must pass the login check.

### 3a. The whole warehouse plugin (amended 2026-10-07, #415)
§3 gave a warehouse plugin two capabilities. Everything else ODS knows about one
warehouse is part of the same plugin, so that supporting a warehouse is one plugin and
the CLI names none. Like a dbt adapter, a plugin is chosen by the project's warehouse
kind. Unlike one, it never connects: it uses dbt's connection, or reads what it is
given. Any warehouse dbt supports works with ODS without a plugin. A plugin only adds
what ODS can know about that warehouse.

```rust
pub trait WarehousePlugin: Send + Sync {
    // §3: origin, warehouse, provides, changes, privileges.
    fn links(&self, settings: &WarehouseSettings)
        -> Result<Arc<dyn RelationLinker>, NoRelationLink>;   // "Open in warehouse"
    fn observed_lineage(&self, export: &Path)
        -> Option<Result<Arc<dyn ObservedLineageSource>, ProviderError>>;
    fn errors(&self) -> Option<Arc<dyn ErrorCatalogue>>;     // warehouse error patterns
    fn dialect(&self) -> Option<&str>;                        // for column lineage
}
```

Every method has a default meaning "not offered", and none needs a new contract: each
returns one of the SDK's existing contracts, whose conformance suites a plugin runs.

- **Settings** (`WarehouseSettings`): the settings of every `[providers.<name>]` whose
  `kind` is the plugin's warehouse, by name, as configured. Secret references stay
  references (rule 9): a plugin is never handed a resolved secret. A plugin may read its
  warehouse's own environment variables (`DATABRICKS_HOST`), as dbt does, and says which
  in its docs. Settings the plugin doesn't know are its own error, reported by
  `ods doctor` under the plugin.
- **Links:** `Unsupported` without a plugin, as today. A plugin that can't build a link
  says why (a missing host, two providers configuring different ones), and that reason is
  what the dashboard shows. The link still passes the capability check (ADR-0006 §3).
- **Observed lineage** is read from what the user exported (today, Unity Catalog's
  `system.access.column_lineage`), so `ods lineage compare --observed <file>` works for
  any warehouse whose plugin reads its export. Reading it live through the probe is a
  later change of this method, not a new one.
- **Error explanations:** a plugin's catalogue is consulted first, then dbt's
  (ADR-0025, "The dbt catalogue"). The first `Recognised` wins, so a warehouse's
  pattern takes precedence over dbt's generic one, and the explanation names the
  catalogue and version that produced it. Patterns specific to one warehouse move into
  its plugin; patterns several warehouses share stay in dbt's catalogue. A plugin
  catalogue implements `ErrorCatalogue` as it is: it classifies an `ErrorSummary` and
  sees nothing else. Joining a classification with the project (the node, relation and
  column, from the `ProjectIndex`) stays in the host, for every catalogue and both
  plugin forms, so evidence confirms only what it supports, the same way whoever
  classified (ADR-0025, "Evidence joins").
- **Dialect:** a name the shared SQL parser knows (`SqlDialect::ALL`), checked when the
  plugin is registered: an unknown name refuses the plugin. Without one, ODS maps the
  warehouse kind to a dialect as today, and a kind it can't map stays as today:
  `ods lineage` refuses it and asks for `--dialect` (exit 2), rather than parse vendor
  SQL as generic SQL and present the result as exact; the probe read-only check uses
  generic SQL, which can only refuse more. The parser stays one provider; a plugin
  chooses its dialect, never brings a parser.
- **Listing:** `ods version` and `ods doctor` list each capability a plugin offers, so
  "Databricks: source versions, login check, links, observed lineage, errors, dialect"
  is visible, as is what a warehouse lacks.
- **Out of process (§5):** the handshake's `describe` names the warehouse and the
  capabilities offered, including the dialect. The requests are `link` (relations, in
  one batch, giving a link or a reason for each), `classify` (error summaries, giving a
  `Classification` each) and `observed_lineage` (a path). Every request carries the
  plugin's `WarehouseSettings`, as the in-process methods are given them, with secret
  references left unresolved (rule 9), so an out-of-process plugin reads the same
  `[providers.<name>]` settings and never needs them duplicated into its environment.
  Their failures are *unknown* as in §5: no link, an unexplained error, no observed
  lineage.
- **Built-ins:** Databricks offers all six. DuckDB becomes the second built-in, in
  `providers/ods-provider-duckdb`: error patterns recorded from real dbt-duckdb in the
  `real-dbt` CI job, and its dialect. It offers no source versions (DuckDB has no table
  version to read), so `ods state` uses `sources.json` alone, which is what it does today.
  Snowflake and BigQuery follow as their own issues, each with recorded responses.

### 3b. Dispatch: parent warehouses and a default (amended 2026-10-07, #415)
dbt's `adapter.dispatch('m')` looks for `<adapter>__m`, then the same macro for each of
the adapter's parents, in order (`databricks` → `spark`, `redshift` → `postgres`), then
`default__m`. ODS finds a warehouse's capabilities the same way, one capability at a
time.

- **Parents.** The manifest names only the adapter (`adapter_type`), not its parents, so
  a plugin declares its own (`fn parents(&self) -> Vec<String>`), as a dbt adapter
  declares the adapters it depends on. A project can add or override them for a
  warehouse that has no plugin, as `dispatch:` in `dbt_project.yml` overrides dbt's
  search order:

  ```toml
  [warehouses.materialize]
  extends = ["postgres"]          # an adapter built on dbt-postgres
  ```

  A parent that isn't registered is skipped. A cycle is a configuration error.
- **What inherits.** Only capabilities that describe the engine's surface: **error
  patterns** (Redshift reports Postgres's messages) and the **dialect**. Capabilities
  that make a promise about data or access, **source versions** and the **login
  check**, never inherit (rule 3): Postgres's way of versioning a table, or of reading a
  login's privileges, says nothing about whether it holds on Redshift. **Links** and
  **observed lineage** don't inherit either: each warehouse's UI and lineage export are
  its own. These answer from the exact plugin or not at all.
- **Default.** The last step is the warehouse-neutral layer, which is what applies today
  with no plugin: dbt's catalogue (the patterns several warehouses share), the
  dialect mapped from the warehouse kind (a kind with none is handled as in §3a), and
  none of the rest.
- **Error catalogues in order:** the warehouse's, then each parent's, then dbt's. The
  first `Recognised` wins (§3a).
- **Listing:** `ods doctor` shows, for each capability, which plugin answers it and
  through which step (`databricks`, `postgres (parent)`, `default`), so an inherited
  answer is never mistaken for a warehouse's own.
- **Users' probe SQL dispatches too.** A probe check's `sql` may give a query per
  warehouse kind, with `default`:

  ```toml
  sql = { databricks = "select count_if(id is null) as n from {relation}",
          default = "select sum(case when id is null then 1 else 0 end) as n from {relation}" }
  ```

  The same order picks one: the kind, its parents, then `default`. With none that
  applies, the probe is *unknown* ("no query for this warehouse"), never run with a
  guess. Every variant is checked as read-only, in its own warehouse's dialect, when the
  configuration loads, and trust (ADR-0030 §4b) covers the whole definition, so a
  variant can't change without trusting it again.
- **dbt's own dispatch needs nothing new.** A model's dispatched macros are resolved
  before ODS sees it: the compiled SQL holds the implementation that ran, and dbt lists
  the macros it reached in `depends_on.macros`. So fingerprints (ADR-0013) and column
  lineage (ADR-0008) already follow `dispatch:` and adapter changes, and a change of
  adapter changes the fingerprint.

### 3c. What a plugin supports is detected (amended 2026-10-07, #415)
A plugin doesn't declare its features: a declaration can disagree with what it does.
ODS asks it instead, and lists what it finds.

- **Detection.** Every method of a warehouse plugin is a factory. ODS calls each one
  with a detection probe, which refuses every statement (building a provider runs
  nothing), and the warehouse's settings. A feature is supported when its method
  returns a provider, or a reason other than "not offered": Databricks' links without a
  host are supported but not configured, and say why. `provides()` goes away.
- **What each feature reports** is what the provider already says, with no new fields:
  its `info()` (kind, version and capabilities, ADR-0006), its contract and version,
  a catalogue's `CatalogueInfo`, the dialect's name, a health check's `CheckInfo`, and
  for source versions, what they read (`versions_read`).
- **Run time is unchanged.** The provider's own capabilities still decide what is used
  (ADR-0006 §3); detection only lists.
- **Out of process (§5):** the plugin's answer to `describe` lists the requests it
  handles and the same information, and `ods plugin test` checks each one it lists.
- **Shown everywhere from the same detection:**
  - `ods plugin list` (every plugin, one line each) and `ods plugin show <name>`, in
    human, plain and `--json` forms;
  - `ods version` and `ods doctor` (`capabilities.plugins`), with which step answered
    each capability for this project (§3b);
  - the MCP server's `list_plugins` tool, so an agent can tell what this `ods` can do;
  - the dashboard's About page;
  - `docs/plugins.md`'s table of built-in warehouses, generated from it, with a test
    that fails when the two differ.
- **Forward compatible:** a request or capability a host doesn't know is listed as
  given and otherwise ignored.

### 4. Health-check plugins and their configuration
- A registered check runs, as ADR-0030 §5 describes, at its own default severity.
- **`[health.plugins.<id>]`** takes the same `severity`, `select` and `exclude` as a
  built-in (`HealthCheckConfig`). `severity = "off"` turns a check off.
- A `[health.plugins.<id>]` naming no registered check is a configuration error, with
  the ids that exist. A misspelt id must never silently drop a gate.

### 5. Out-of-process plugins: executables on the script protocol
After script checks (ADR-0030 phase 5):
- **Declaration:** `[plugins.<name>]` with `command = [...]` (never run through a shell),
  plus an optional `timeout`.
- **Handshake:** ODS sends `{"protocol": {"major": 1, "minor": 0}, "request":
  "describe"}`. The plugin answers with what it provides: each contract and the version
  it was built against, its checks' `CheckInfo`, or the warehouse it serves. A contract
  version the host can't accept refuses the plugin, with both versions named.
- **Calls:** each call is one request and one response, the script protocol's envelope
  (ADR-0030 §4). The `request` names the contract method: `check` (a `CheckScope`, giving
  `Finding`s) and `versions` (`RequestedSource`s, giving `SourceVersion`s). The JSON
  shapes are the SDK types' own serde forms, versioned by the protocol's major.
- **Failure** (rule 3): a non-zero exit, a timeout, malformed output, an unknown protocol
  major, or an answer about something not asked gives *unknown* for everything in that
  call. It is never a pass, and never a version.
- **Secrets** (rule 9): ODS passes no credentials. A plugin that needs the warehouse
  connects with its own environment, as `dbt` does. An out-of-process warehouse plugin
  therefore can't use dbt's connection. It reports its own login's privileges if it
  offers `relation_privileges`.
- **Trust** (ADR-0030 §4b): a plugin declared in a project's files runs only once
  `ods health trust` has recorded a digest of its `command`. A plugin declared only in
  the user's own configuration layer is trusted, since the user wrote it.
  `--allow-scripts` covers plugins for one run, and it is never persisted.
- **Conformance:** `ods plugin test <name>` runs the SDK's conformance suites for every
  contract the plugin describes, against the plugin as a subprocess, with the fake
  provider's fixtures. A plugin author runs it before publishing.

### 6. Where the code lives
- **`ods-sdk`** gains `PrivilegedProbe`, one object that is both a `RelationProbe` and
  `RelationPrivileges` (ADR-0030 §4c: the login checked is the login probed), and
  `RelationProbe` for `Arc<T>`, so a plugin can wrap whatever connection it's given. It
  later gains the plugin protocol's types (`ods_sdk::protocol`: the envelope,
  `describe` and the per-contract requests and responses). They are pure serde types,
  with no process.
- **`providers/ods-provider-process`** (new) holds the only code that starts plugin and
  script processes. It implements `HealthCheck` and `ChangeProvider` over the protocol.
  This **amends ADR-0030 §5**: the script runner lives here, not in `ods-health`.
  `ods-health` then starts no process, and a script check is a `HealthCheck` the CLI
  builds from configuration.
- **`ods-cli`** owns `Plugins`, `Ods`, the `WarehousePlugin` trait, and the wiring.

```mermaid
graph LR
  core[ods-core] --> sdk[ods-sdk<br/>contracts, Registry, protocol types]
  sdk --> health[ods-health<br/>engine, no processes]
  sdk --> dbx[ods-provider-databricks<br/>DeltaVersions, UnityCatalog]
  sdk --> proc[ods-provider-process<br/>scripts, out-of-process plugins]
  sdk --> ext[a third party's crate<br/>HealthCheck, WarehousePlugin]
  health --> cli[ods-cli<br/>Plugins, Ods builder, wiring]
  dbx --> cli
  proc --> cli
  cli --> custom[a custom ods binary]
  ext --> custom
```

### 7. Phases
1. **This ADR.**
2. **In-process:**
   - `Plugins`, `Ods` and `WarehousePlugin`, with Databricks registered as a built-in;
   - `[health.plugins.<id>]`;
   - plugins listed in `ods version` and `ods doctor`;
   - `examples/custom-ods`, built in CI.

   Databricks behaves exactly as today, and its tests pass unchanged (#387's acceptance).
3. **Script checks** (ADR-0030 phase 5), in `ods-provider-process`.
4. **Out-of-process plugins:** `[plugins.<name>]`, the handshake, and
   `ods plugin test`.
5. **The whole warehouse plugin** (§3a, §3b, #415): `links`, `observed_lineage`,
   `errors` and `dialect`; Databricks' moved into its plugin with behaviour and tests
   unchanged, and no Databricks import left in `ods-cli` outside its registration; then
   parents, `[warehouses.<kind>] extends` and per-warehouse probe SQL; features
   detected (§3c) in place of `provides()`, with `ods plugin list` and `ods plugin
   show`; then DuckDB as the second built-in. This phase doesn't wait for 3 and 4: it is in-process, and the
   out-of-process requests above arrive with phase 4.

## Consequences
- **Positive:**
  - Anyone can add health checks, source-version providers and login checks without
    changing ODS: in Rust, in a custom build, or in any language as an executable.
  - The hard-coded Databricks mappings become ordinary registrations, so supporting a
    second warehouse means a plugin, not an edit to the CLI. With §3a that covers all
    of it: links, observed lineage, error explanations and dialect, as well as source
    versions and the login check.
  - One protocol serves scripts and plugins, and `ods-health` stops starting processes.
- **Negative / trade-offs:**
  - **API surface:** `ods-cli`'s library API (`Ods`, `Plugins`, `WarehousePlugin`) becomes
    a public interface. ADR-0019 adds a compatibility row for it: before 1.0, any
    release may break it, as with the SDK.
  - **Rebuilds:** in-process plugins must be rebuilt for every 0.x release.
  - **Out-of-process warehouse plugins** can't reuse dbt's connection, so they bring
    their own credentials, and their own login is what their privilege report covers.
  - **One plugin per warehouse:** two competing providers for the same warehouse can't
    both be registered. A custom build picks one.
  - **Error catalogues are ordered** (§3a): a warehouse plugin's patterns shadow dbt's
    for the same message. A wrong plugin pattern hides a right dbt one, so plugin
    catalogues run the same `error_catalogue` suite, with recorded messages.
- **Follow-up issues:** the phases in §7. #387's guidance for provider authors and its
  per-warehouse research notes stay in #387.

## References
- ADR-0001 (module boundaries), ADR-0004 (exit codes), ADR-0005 (configuration),
  ADR-0006 (plugin SDK, registries and capabilities), ADR-0019 (versioning), ADR-0022
  (Delta table versions), ADR-0030 (health checks: §4 scripts, §4b trust, §4c login
  checks).
- #387, #392, #415, #99 (conformance suites), #9 (policy, which may later replace trust).
- ADR-0025 (error explanations), ADR-0006 §3 (capabilities), ADR-0008 (column-level
  lineage, its dialects).
