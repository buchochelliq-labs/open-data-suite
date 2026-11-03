# ADR-0032: Secret providers: references resolved in memory, at the point of use

- **Status:** Accepted (2026-11-03). Built: the `secret_provider` contract 0.1 in `ods-sdk`, its conformance suite, the built-in `env:` provider, `SecretResolver`, the fake in `ods-provider-fake`, and `Ods::secret_provider` for custom builds, listed by `ods version`, `ods plugin list` and `ods doctor`. Not yet: a consumer (the first is Databricks authentication, #297, ADR-0021), a doctor check that references resolve, out-of-process secret providers (§5), and the dashboard's About page listing secret providers.
- **Date:** 2026-11-03
- **Issues:** #126
- **Deciders:** @n1ckyb

## Context
AGENTS.md rule 9: secrets are referenced, never persisted in configuration, state,
events or logs. ADR-0005 §4 made every credential in `ods.toml` a reference,
`{ secret = "<scheme>:<name>" }`, rejected plaintext (ODS-E0103), and left to #126
which schemes exist and how a reference becomes a value.

Nothing resolves a reference yet. The first code that must is Databricks
authentication (#297, ADR-0021): a personal access token or a service principal's
client secret, read from where the user keeps it, used for one HTTP request, and never
written anywhere. #126 asks for environment variables (which is also how CI systems
inject secrets), OS keychains, and cloud secret stores through plugins, with a
provider-neutral contract and redaction in logs and errors.

Forces:
- **Neutrality** (rule 1): core and modules must not know Vault from a keychain. The
  scheme in a reference is the only thing that picks a store.
- **Conservative defaults** (rule 3): a missing or empty secret must never be taken for
  a credential, and an unknown scheme must never fall back to another store.
- **I/O**: keychains and cloud stores are I/O; AGENTS.md keeps async to I/O boundaries.
- **Leaks**: a value that reaches `Debug`, a log line, an error message, JSON output or
  the state store is a leak. Engines and servers sometimes echo a credential back in
  an error.

## Options considered
### Option A — Resolve in `ods-config` while loading
Pros: one place; every consumer gets values. Cons: the loaded `Config` would hold
secrets, which ADR-0005 rules out (it is shown by `ods config explain`, fingerprinted
and served by the dashboard); every command would read every store, even those it
doesn't need, and a locked keychain would break `ods --help`.

### Option B — A contract in `ods-sdk`; resolve at the point of use (chosen)
A `SecretProvider` per scheme; the host resolves a reference only where a credential is
used, and holds the value only for that use. Pros: configuration stays reference-only;
stores are pluggable like every other provider; nothing is read that isn't used. Cons:
each consumer must ask, and handle the error.

### Option C — Shell out to a helper (`credential_process`-style) for every scheme
Pros: any language. Cons: every secret crosses a process boundary and the plugin
protocol, which needs its own trust model (ADR-0031 §5); too much for the built-in
`env:` case. Kept as a later addition (§5).

For the value type: a hand-written wrapper, re-exporting `secrecy`'s types, or a
newtype over `secrecy::SecretString`. The newtype keeps a 0.x third-party type out of
the SDK's public API while getting zeroize-on-drop and a redacting `Debug` from a
maintained crate (`secrecy` and `zeroize` are Apache-2.0 OR MIT).

## Decision
### 1. The contract
`ods_sdk::contracts::secrets`, contract `secret_provider` 0.1 (SDK 0.10):

```rust
#[async_trait]
pub trait SecretProvider: Provider {
    fn scheme(&self) -> &str;
    async fn resolve(&self, reference: &SecretRef) -> Result<SecretValue, SecretError>;
}
```

- One provider serves one scheme, as `SecretRef::is_valid_scheme` allows (lowercase
  letters, digits, `-`), and the scheme never changes.
- Every provider must refuse an empty value (below); the resolver (§3) refuses one
  too, so an empty credential is never handed out whatever a provider does.
- `resolve` only reads. Async, because stores are I/O.
- A reference with another scheme is `UnknownScheme`; a missing secret is `NotFound`;
  one that exists but is empty is `Empty`, never an `Ok` with an empty value. The store
  refusing is `Denied`; a store that can't be reached is `Unavailable` (retryable); a
  value that can't be used (not text) is `Invalid`.
- `SecretError` messages name the reference, as `secret(<scheme>:<name>)`, never the
  value. The free-text `reason` of `Unavailable` and `Invalid` is the provider's own,
  and keeping the value out of it is the provider's obligation. It converts into
  `ProviderError` (`Unavailable` stays retryable), so a
  provider that needs a credential can use `?`.

### 2. The value
`SecretValue` wraps `secrecy::SecretString`: wiped from memory when dropped; `Debug`
prints `SecretValue([redacted])`; no `Display`, `Serialize` or `Clone` (compile-fail
doctests keep it so), so it can't be printed, written out or copied by accident, and
is shared with `Arc`. `expose()` returns the text for the one place that needs it.
`redact_in(text)` replaces every occurrence of the value in a message, with
`[secret removed]`, for errors that echo a credential (an HTTP 401 body, an engine
message).

### 3. Resolution
`ods_sdk::secrets::SecretResolver` maps each scheme to its one provider (a `BTreeMap`;
registering a second provider for a scheme, or an invalid scheme, is an error, so which
store is read never depends on registration order). `resolve` sends a reference to its
scheme's provider; an unknown scheme is `UnknownScheme` listing the schemes served. It
keeps no values: every call asks the provider again, so a rotated secret is picked up
and no value outlives its caller.

A host resolves a reference only where a credential is used, holds the `SecretValue`
for that use, and drops it. Resolved values never enter `Config`, state, run events,
the journal, logs, command output or the dashboard. ADR-0031's rule stands: a plugin is
never handed a resolved secret. How a warehouse plugin's own requests get a credential
(e.g. the Databricks provider's token for the SQL Statement API) is decided with #297,
in ADR-0021, within this ADR's rules.

### 4. Schemes
- **`env`** is built in (`EnvSecrets`), reading the environment `ods` was started with.
  It is how CI injects secrets (GitHub Actions secrets, GitLab CI variables, …), so CI
  needs nothing else. The host passes the environment in, so tests stay hermetic.
- **Keychains and cloud stores** (`keychain:`, `vault:`, `aws-sm:`, …) come from
  in-process plugins: a custom build adds one with `Ods::secret_provider(origin!(),
  provider)`, which `ods version`, `ods plugin list` and `ods doctor`
  (`capabilities.plugins`) list as `secret_provider` plugins. ODS ships none: each OS
  keychain and each cloud SDK is a large, platform-specific dependency, and the
  contract lets them live outside the released binary. `env` is reserved: no plugin
  can serve it, even in a build without the built-in.

### 5. Not now: out-of-process secret providers
A `secrets` request on the script protocol (ADR-0031 §5) would let a plugin in any
language serve a scheme, but a secret would then cross a process boundary, and the
protocol, its trust (`ods health trust`) and its records were designed never to carry
one. It needs its own decision before it is built.

## Consequences
- Positive:
  - Configuration, state and output stay reference-only, as ADR-0005 decided; values
    exist only in memory, only while used.
  - #297 can resolve `token` and `client_secret` with one call and one error type.
  - Third parties can add any store without a change to ODS, and run the conformance
    suite (`ods_sdk::conformance::secrets`) against it, as `ods-provider-fake` does.
- Negative / trade-offs:
  - Each consumer must resolve, and handle `SecretError`, itself.
  - The value is still in process memory while used; zeroize-on-drop shortens that,
    but a core dump or a debugger can see it. `env:` values are also in the process
    environment, and `EnvSecrets` keeps its copy of the variables, not zeroized, for
    as long as the resolver lives; only the `SecretValue`s it hands out are wiped.
  - `redact_in` only removes exact occurrences; an engine that echoes a transformed
    credential (e.g. base64 in a header dump) isn't caught. Messages from engines also
    pass through `ods_core::redact` (rule 9), which removes quoted literals.
- Follow-up issues:
  - #297: the first consumer (Databricks PAT and M2M client secret).
  - A doctor check that every configured reference resolves, without showing values;
    it needs commands to be handed the environment.
  - The dashboard's About page listing secret providers.
  - Out-of-process secret providers (§5).

## Threat model
| Where a value could leak | Why it doesn't |
|---|---|
| `ods.toml`, `ods config explain`, the dashboard's Settings | Only references are allowed (ADR-0005 §4); resolution never writes back. |
| State store, run events, journal, ledger | Nothing resolved is passed to them; they take configuration, which holds references. |
| `Debug`, `tracing` fields, panics | `SecretValue`'s `Debug` is redacted; it has no `Display`. |
| Error messages | `SecretError` names references only; a provider's free-text reasons are its obligation; `redact_in` for echoed values; conformance checks a provider's errors and `info()` never hold the value. |
| Child processes (dbt) | ODS passes dbt the environment it was given and adds no secret (ADR-0021 keeps dbt's own auth). |
| Plugins | Handed references, never values (ADR-0031). |

## References
- #126, #297; ADR-0005 §4, ADR-0006, ADR-0021, ADR-0031.
- [`secrecy`](https://crates.io/crates/secrecy), [`zeroize`](https://crates.io/crates/zeroize).
