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

## Reading an exact revision

`CodeReader` is a separate async trait. `ods-provider-git::LocalGitCode::new` takes a
map of approved aliases to local checkout roots, a byte limit (1 byte–8 MiB), and a
nonzero total deadline. Call `read_code(&location, Some(producing_run))` explicitly.
It needs Git 2.48+ and local objects. No clone/fetch, hooks, filters, textconv or code
execution occurs. Standard host-controlled checkout layouts only; linked worktrees,
bare repositories, symlinks as source and external object alternates are rejected.

The returned `CodeDocument` exposes `location()`, `commit()` and opt-in `text()`.
It has no serialization implementation and its Debug output omits source text.
Do not add the text to ordinary evidence reports: source may contain sensitive
literals. Errors are typed and omit stderr and filesystem paths. Current/unknown
revision references require resolution before reading; an inaccessible commit or
file returns an error rather than today's source or an empty success.

See [ADR-0034](adr/0034-local-immutable-code-retrieval.md) for trust boundaries.

### Remote repositories are read-only

`RemoteGitCode::new` accepts a host-managed alias-to-HTTPS-URL map, byte limit and
nonzero deadline. It fetches the exact commit at depth one into a disposable local
object store; nothing is checked out and no remote/push URL is configured. The same
local immutable reader handles the requested blob. Git process groups are killed
on timeout/cancellation, and temporary storage is removed on completion.

The built-in remote reader is anonymous: no inherited SSH agent, token, askpass,
credential helper, global/system Git configuration or proxy credentials. User info,
query strings, fragments and redirects are rejected. Private repository access must
use a separate provider with server-enforced read-only credentials. Never give a
reader a token that can push. A fetch-only API alone does not sandbox rogue native
plugins; untrusted plugins require process isolation and restricted credentials.
Remote pack storage needs a host disk quota; source byte limits do not cap pack size.

Authentication is **fail-closed**: `CodeReader::verify_read_access` defaults to
unverified. `require_read_only` rejects `WriteCapable` and `Unverified` before a read;
Elephantry runs this gate within its total deadline before calling the reader.
A credentialed adapter may return `VerifiedReadOnly` only after authoritative
permission inspection for the target repository. A user-provided flag, plugin
advertisement or unsuccessful push is not verification. If scope introspection is
unsupported, it must refuse. The built-in remote reader accepts no authentication
at all, so authenticated private access remains unsupported rather than bypassing
this requirement. This protects the inspected repositories from write credentials
being passed to fetch processes; a separate sandbox is still required for arbitrary
untrusted native code.
