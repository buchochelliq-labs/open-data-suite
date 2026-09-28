# ADR-0021: Databricks authentication: U2M first, M2M for CI, tokens never persisted

- **Status:** Proposed
- **Date:** 2026-09-28
- **Issues:** #297 (related: #294, #295, #126, #17, #30)
- **Deciders:** @n1ckyb

## Context
ODS will talk to Databricks directly, not only through dbt:
- Unity Catalog metadata and lineage (#17, #30);
- SQL checks through the SQL Statement API.

It needs to authenticate two kinds of caller:
- **people**, on their own machines;
- **CI**, in the `databricks` workflow (#294).

### What #294 / #295 found
The CI script (`.github/databricks/ci.py`, PR #295) has run against a Databricks Free
Edition workspace.
- **Scopes.** The service principal's OAuth secret was scoped without `all-apis`. A
  token for `sql` was granted, and that is enough for SQL statements. The script asks
  for `all-apis`, then `sql` (or `DATABRICKS_OAUTH_SCOPES`). Identity comes from
  `current_user()`, because SCIM needs more than `sql`.
- **Federation subject.** This repository's GitHub OIDC subject uses the immutable-ID
  form, `repo:buchochelliq-labs@285328036/open-data-suite@1382703972:environment:databricks-free`.
  A policy written for the documented `repo:<owner>/<repo>:…` form matches nothing.
  When federation is refused, the script prints the issuer, subject and audience that
  a policy must match. These are claims, not credentials.
- **Free Edition has no account console.** Federation policies are account-level
  objects, so a Free Edition workspace can't have one. There, CI falls back to the
  service principal's secret.

### Constraints
- **Rule 9 and ADR-0005:** configuration holds only references
  (`{ secret = "env:…" }`, a profile name). Resolved tokens never enter `Config`,
  state, events, logs or output.
- **Rule 1:** the code lives in `providers/ods-provider-databricks`. Core and modules
  never name the vendor. They see a capability and a neutral contract.
- **No network in tests.** The token endpoint and the Databricks CLI are faked.
- `SecretProvider` (#126) resolves references. Only its `env:` scheme is needed here.

## Options considered
### Option A — Personal access tokens only
- Pros: simplest; one header.
- Cons: long-lived, usually as broad as the user, and easily pasted into files.
  Databricks recommends OAuth instead. Kept only as an explicit last resort.

### Option B — ODS runs its own OAuth flows and keeps its own token cache
- Pros: no dependency on the Databricks CLI.
- Cons: ODS would store refresh tokens, which makes it a secret store (rule 9). Users
  would sign in twice, once for the CLI and once for ODS. It also duplicates a
  browser flow the CLI already does well. Rejected.

### Option C — A Databricks SDK for Rust
- Pros: the credential chain comes ready-made.
- Cons: we found no official, maintained Databricks SDK for Rust to depend on. A
  community crate would need its licence and maintenance checked under ADR-0002. If
  an official one ships, the chain below can move onto it without changing the
  contract.

### Option D — A small credential chain in the provider, reusing the CLI for people (chosen)
- **U2M** through the Databricks CLI's profiles and token cache.
- **M2M** with a service principal's OAuth secret, or with workload identity
  federation.
- **PAT** only when configured explicitly.
- Tokens live in memory only.
- Pros: no token is stored by ODS. People sign in once, with the tool they already
  use. It is the same flow CI already runs.
- Cons: U2M needs the Databricks CLI installed. The CLI's command-line output becomes
  an interface we depend on.

## Decision
**`ods-provider-databricks` authenticates with a credential chain: U2M through a
Databricks CLI profile for people (modelled as delegation to that profile, and
reported with the profile's own auth type), a service principal's OAuth secret or workload identity
federation for CI, and a PAT only when asked for by name. Tokens are held in memory,
never written, and never cross the SDK contract. Configuration holds only references.**

U2M ships first. M2M follows for the `databricks` CI job (#294), and federation after
that.

### 1. Methods

| `auth` | For | How | Holds |
|---|---|---|---|
| `cli` | people | **Delegated to a Databricks CLI profile.** U2M (OAuth authorization code with PKCE) is what it is meant for: `databricks auth login` signs in, and ODS runs `databricks auth token --profile <p>` and reads `access_token` and `expiry` from its JSON. The CLI refreshes from its own cache. | a profile name, and `host` |
| `m2m` | CI, services | `client_credentials` grant at `<host>/oidc/v1/token`, with HTTP Basic auth of the client ID and secret | `client_id`, a `client_secret` reference |
| `federated` | CI where the account allows it | RFC 8693 token exchange at `<host>/oidc/v1/token`: the CI platform's OIDC token (GitHub's today) for a Databricks token, under the service principal's federation policy | `client_id`, optionally `audience` |
| `pat` | last resort | a bearer token, sent as is | a `token` reference |

- **`cli` details:**
  - **It is modelled as delegation, not as U2M.** A profile can use a PAT, a client
    secret or other methods as well as a U2M login. Whatever it resolves to is the
    profile's choice, not ODS's. So `status` reports `delegated`, with the profile name
    and the profile's own auth type. It claims `user` only when that type is the CLI's
    U2M login (`databricks-cli`).
  - **Finding the auth type.** ODS asks the CLI
    (`databricks auth describe --profile <p> --output json`) and reads only the auth
    type and host fields. Before this ships, a test against the real CLI checks that
    this output never contains the profile's secret values. A fake CLI covers it in
    unit tests. If the type can't be determined, `status` says `unknown`, and a warning
    says so. ODS never presents an unknown type as U2M.
  - **A profile that resolves to a PAT is refused.** A PAT is only used when named
    (`auth = "pat"`, below). The error says so. We believe `auth token` only returns
    OAuth tokens, but we don't rely on that.
  - **`host` is required** in ODS's configuration (or `DATABRICKS_HOST`), until host
    discovery is decided. ODS never parses `~/.databrickscfg`, so a profile alone
    gives it no host. When `auth describe` reports the profile's host, ODS compares it
    with `host`. On a mismatch it refuses before sending anything, so a token issued for
    one workspace is never sent to another.
  - ODS never parses `~/.databrickscfg`, because profiles can hold PATs and client
    secrets. The CLI resolves them.
  - ODS never reads or writes the CLI's token cache file, whose format is the CLI's.
  - ODS never runs `databricks auth login` itself. When there is no valid login, the
    error names the command to run, e.g.
    `databricks auth login --profile free`.
  - The CLI is found on `PATH`, or at the `cli_path` setting.
- **Choosing a method:**
  - When `auth` is set, only that method is used. There is no fallback. This is
    recommended in CI, so a misconfiguration fails instead of changing identity.
  - When `auth` is unset, methods are tried in this order:
    1. `federated`, only when `client_id` is set and the process has a CI OIDC token;
    2. `m2m`, when `client_id` and `client_secret` are set;
    3. `cli`, when `profile` is set.

    A method that is refused falls back to the next, as `ci.py` does, and the report
    says why each one was skipped. `pat` is never chosen automatically.
- **The short-lived token `ci.py` exports** as `DATABRICKS_TOKEN` is an OAuth token,
  not a PAT. It can be passed as `auth = "pat"` with
  `token = { secret = "env:DATABRICKS_TOKEN" }` until ODS runs the M2M flow itself.

### 2. Scopes
- Each operation asks for the narrowest scope that serves it:
  - SQL statements: `sql`;
  - Unity Catalog REST calls: `unity-catalog`, if Databricks grants that scope name;
  - otherwise `all-apis`.
- `scopes = [...]` overrides the list. It is tried in order while Databricks answers
  that a scope isn't assigned, as `ci.py` does. `ci.py` tries the broadest scope first,
  because a smoke check doesn't know what it will need. ODS knows the operation, so it
  starts narrow.
- #295 verified `all-apis` and `sql`. Any other scope name is checked against a real
  workspace before it becomes a default.
- Tokens from a CLI profile carry the scopes its login requested. ODS can't narrow
  them, and says so in `status`.

### 3. Configuration (ADR-0005)
```toml
[providers.uc]
kind = "databricks"

[providers.uc.settings]
host = "https://<workspace>.cloud.databricks.com"
auth = "cli"                     # cli | m2m | federated | pat; unset = automatic
profile = "free"                 # cli: a ~/.databrickscfg profile; host is still required
cli_path = "databricks"          # cli: optional, default the CLI on PATH
client_id = "<application id>"  # m2m, federated: not a secret
client_secret = { secret = "env:DATABRICKS_CLIENT_SECRET" }
audience = "<account id>"        # federated
scopes = ["sql"]
# token = { secret = "env:DATABRICKS_TOKEN" }   # pat, last resort
```
- `client_secret` and `token` are credential-like names, so ADR-0005 already rejects
  plaintext values (ODS-E0103).
- The provider names its settings, as the dbt provider does (#214), so an unknown key
  is ODS-E0102.
- `DATABRICKS_HOST` and `DATABRICKS_CONFIG_PROFILE` are read as defaults for `host`
  and `profile`, after flags and before configuration, as the dbt provider reads
  `DBT_*`.

### 4. Where the code lives (rules 1 and 2)
- **`ods-provider-databricks`:** the chain, the token endpoint client and the CLI
  runner, in a private `auth` module.
- **`ods-core`:** a capability, `user_login`. It is advertised by a provider that
  signs people in through an external tool. `ods_core::choose` then lets the CLI offer
  a login hint, or fall back to "configure a reference".
- **`ods-sdk`:** a contract, `credentials` 0.1:
  - `status() -> AuthStatus`: the method (`user`, `service_secret`, `workload`,
    `static_token`, or `delegated` with the tool profile's name and its own reported
    auth type, `unknown` when it can't be told), the principal if known, the scopes,
    and when the token expires;
  - `login_hint() -> Option<String>`.

  **No token crosses the contract.** Providers use their tokens internally.
- **`ods-provider-fake`:** a fake `credentials` implementation. The conformance suite
  checks that no `AuthStatus` field or error can hold a token.
- **`ods-cli`:** wires the provider, and shows `status` in commands that connect.

### 5. Handling tokens
- Tokens are held in a wrapper whose `Debug` and `Display` print `<redacted>` and
  whose memory is cleared on drop. We will use a maintained crate for this (e.g.
  `secrecy`, MIT/Apache-2.0), checked with `cargo deny`.
- A token is refreshed shortly before `expiry`. On one `401`, the provider gets a new
  token and retries once.
- Tokens are never passed on a command line, in an environment variable to a child
  process, or in a URL.
- Error messages keep Databricks' own error text, truncated. They never include
  request bodies or headers. HTTP tracing redacts `Authorization`.
- Only HTTPS hosts are accepted. Redirects that change host are not followed with
  credentials.

### 6. Threat model
| What | Where it could leak | Mitigation |
|---|---|---|
| Access tokens | ODS config, state, events, logs, JSON output, error messages | Never written: memory only, redacted wrapper, no token in the contract. A test runs every method with a marker token and searches all outputs and the state database for it. |
| Access tokens | process list, child environments | Not passed on command lines or to children. The CLI's answer is read from a pipe. |
| Refresh tokens | the CLI's cache on disk | Owned and protected by the CLI. ODS doesn't read, copy or write the file. |
| PATs and secrets in `~/.databrickscfg` | ODS reading the file | ODS doesn't parse it; the CLI resolves profiles. |
| Client secret | configuration, CI logs | A reference only (ADR-0005). In CI, a GitHub environment secret, masked, with required reviewers. Fork PRs never run the job. Federation removes the secret where the account allows it. |
| An over-broad token | misuse if stolen | Narrowest scope per operation. The CI principal has only `USE CATALOG`, `CREATE SCHEMA` and warehouse use. |
| A spoofed host | tokens sent to an attacker | HTTPS only, TLS always verified, and no credentialed redirects to another host. |
| A profile's token sent to another workspace | the configured `host` | When the CLI reports the profile's host, a mismatch with `host` is refused before any request. |
| A profile that silently uses a PAT or secret | an identity the user didn't expect | `status` reports the profile's own auth type, never an assumed U2M. A PAT-backed profile is refused. |
| OIDC claims printed on refusal | public CI logs | They name the repository, environment and audience, which aren't secrets. The OIDC token itself is never printed. |
| A PR changing the CI script | the job's credentials | The `databricks` label runs a PR's own code: review it first (docs/contributing-databricks.md, #295). |

Out of scope: a compromised user account or machine, and memory dumps.

### 7. Tests
- **Unit tests, no network:** the HTTP client sits behind a small transport trait. A
  fake token endpoint covers each grant, scope fallback, expiry and refresh, a refused
  federation, and error messages that never contain a token.
- **A fake `databricks` CLI** in `fixtures/`, like `fixtures/dbt/fake-dbt`. It covers a
  valid login, an expired login ("run `databricks auth login`"), and a missing CLI. It
  also covers each auth type `auth describe` can report (U2M, PAT, M2M, unknown), and a
  host that doesn't match `host`.
- **Conformance:** the `credentials` suite, run by the fake and Databricks providers.
- **Live:** U2M by hand against Free Edition, with a CLI profile. M2M in the
  `databricks` CI job (#294). Federation where an account allows it.

## Consequences
- Positive:
  - ODS stores no Databricks token. People sign in once, with the Databricks CLI.
  - CI runs the flow it already has. Where the account allows federation, CI needs no
    secret at all.
  - Core sees a capability and a token-free status, nothing vendor-specific.
- Negative / trade-offs:
  - U2M needs the Databricks CLI. The JSON of `auth token` and `auth describe`
    becomes an interface we test against.
  - `host` must be configured even when a profile names one.
  - Free Edition can't use federation, so CI there keeps a service principal secret.
  - Scope names beyond `all-apis` and `sql` are unverified until tested on a real
    workspace.
  - A new dependency for the token wrapper.
- Follow-up issues:
  - #126: `SecretProvider` with the `env:` scheme, which `client_secret` and `token`
    need.
  - Host discovery: make `host` optional for `cli` by reading it from
    `databricks auth describe`, once a test shows that output never includes
    credentials. Until then `host` is required.
  - Federation from other CI platforms' OIDC tokens.
  - Move the chain onto an official Databricks SDK for Rust, if one ships under a
    permissive licence.

## References
- #297; #294 and PR #295 (`.github/databricks/ci.py`,
  `docs/contributing-databricks.md`).
- [ADR-0005](0005-configuration-and-profiles.md) (references),
  [ADR-0006](0006-plugin-sdk-and-capabilities.md) (contracts and capabilities),
  [ADR-0002](0002-rust-first-backend-and-technology-stack.md) (dependencies).
- Databricks documentation: OAuth U2M and M2M, workload identity federation, and
  `databricks auth login` / `databricks auth token`.
- RFC 7636 (PKCE), RFC 8693 (token exchange).
