# Producing-code references

The `ods-sdk::contracts::producing_code` API locates code metadata for a scoped
project node. It does not read a checkout, contact Git, or execute inspected code.
Basic use needs neither a catalog nor ontology.

A generic `DeclaredMappings` JSON document:

```json
{
  "schema_version": { "major": 0, "minor": 1 },
  "entries": [{
    "asset": {
      "project": "claims",
      "environment": "prod",
      "node": "source.claims.raw_claims"
    },
    "code": {
      "repository": "ingestion",
      "path": "jobs/load_claims.py",
      "revision": { "kind": "current", "reference": "main" }
    }
  }]
}
```

`repository` is a host-managed alias: letters, digits, `-`, `_` and `.`, not
starting with `.`. It is not a URL or credential. Paths are
relative, slash-separated and reject dot/parent components. Lookup validates
metadata only; a future retriever must enforce its own checkout allowlist and
canonical path/symlink containment before reading anything.

For dbt, place an array of the `code` objects above in a node's
`meta.ods.producing_code` or `config.meta.ods.producing_code`. If both exist they
must agree. `DbtProducingCode::from_manifest` reads the existing ODS `Manifest` and
requires the caller's project/environment scope. It works for sources as well as
models; the dbt source boundary does not establish the origin of the data.

The revision forms are `unknown`, `current` with `reference`, and `exact` with a
full hexadecimal `commit` plus a `run`. The run is the **producing run**, not the
invocation of a downstream test. Exact references are returned only for a matching
requested run. An exact reference from project metadata remains **declared**, not
observed proof. Old mappings can report a revision mismatch alongside a valid
matching reference; consumers should retain that diagnostic without blocking the
valid candidate. Multiple distinct locations are ambiguous and stay visible.

A third-party source implements `ProducingCodeSource` and the standard `Provider`
trait. It can construct `CodeLocation` and `CodeReference` directly, and returns
`CodeResolution` with typed gaps. Consumers retain provider identity/version and
all gaps when combining sources. The reusable conformance check is available with
the SDK's `conformance` feature; a fake provider demonstrates the same contract.
This initial version is an in-process API. Registry/process transport, Git retrieval
and catalog-derived code references are remaining parts of #462.

See [ADR-0033](adr/0033-producing-code-references.md).
