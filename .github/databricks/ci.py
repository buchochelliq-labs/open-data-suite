#!/usr/bin/env python3
"""Databricks access for the `databricks` CI workflow (#294).

Standard library only, so the job needs nothing installed before it authenticates.

    ci.py token            get a short-lived workspace token and export it, masked
    ci.py sql STATEMENT    run STATEMENT on the warehouse and print the rows

Authentication, in order:

1. Workload identity federation: GitHub's OIDC token for this job is exchanged for a
   Databricks token under the CI service principal's federation policy. No secret.
2. The service principal's OAuth secret (`DATABRICKS_CLIENT_SECRET`), when federation
   isn't available or is refused.

The token is only ever written masked to `$GITHUB_ENV`, never printed or saved
(AGENTS.md rule 9). Errors show Databricks' message, which never contains it.
"""

from __future__ import annotations

import base64
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

TOKEN_EXCHANGE = "urn:ietf:params:oauth:grant-type:token-exchange"
JWT = "urn:ietf:params:oauth:token-type:jwt"
# Tried in order until one is granted: a scoped service principal secret may only
# carry `sql`, which is all the smoke check needs. `DATABRICKS_OAUTH_SCOPES` overrides.
DEFAULT_SCOPES = ("all-apis", "sql")
# Statements poll for up to this long: a stopped serverless warehouse takes a while.
STATEMENT_TIMEOUT_S = 600


class Failed(Exception):
    """A step failed; the message says why, without any credential."""


def env(name: str, required: bool = True) -> str:
    value = os.environ.get(name, "").strip()
    if required and not value:
        raise Failed(
            f"{name} is not set. Add it to the `databricks-free` environment as a "
            "variable or secret (Settings → Environments); see "
            "docs/contributing-databricks.md."
        )
    return value


def host() -> str:
    value = env("DATABRICKS_HOST").rstrip("/")
    if "://" not in value:
        value = "https://" + value
    return value


def request(
    method: str,
    url: str,
    *,
    form: dict[str, str] | None = None,
    body: object | None = None,
    headers: dict[str, str] | None = None,
) -> dict:
    data = None
    headers = dict(headers or {})
    if form is not None:
        data = urllib.parse.urlencode(form).encode()
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    elif body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=120) as response:
            return json.load(response)
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")[:500]
        raise Failed(f"{method} {urllib.parse.urlsplit(url).path}: HTTP {e.code}: {detail}")
    except urllib.error.URLError as e:
        raise Failed(f"{method} {urllib.parse.urlsplit(url).path}: {e.reason}")


def github_oidc_token(audience: str) -> str:
    url = os.environ.get("ACTIONS_ID_TOKEN_REQUEST_URL")
    bearer = os.environ.get("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
    if not url or not bearer:
        raise Failed("no GitHub OIDC token: the job needs `permissions: id-token: write`")
    sep = "&" if "?" in url else "?"
    got = request(
        "GET",
        f"{url}{sep}audience={urllib.parse.quote(audience, safe='')}",
        headers={"Authorization": f"bearer {bearer}"},
    )
    return got["value"]


def scopes() -> list[str]:
    configured = env("DATABRICKS_OAUTH_SCOPES", required=False)
    return [configured] if configured else list(DEFAULT_SCOPES)


def with_scopes(get_token) -> str:
    """Asks for each scope in turn while Databricks says it isn't assigned."""
    refused = []
    for scope in scopes():
        try:
            return get_token(scope)
        except Failed as e:
            if "are not assigned" not in str(e):
                raise
            refused.append(scope)
    raise Failed(f"none of the scopes {refused} is assigned to the service principal")


def claims(jwt: str) -> dict:
    """The claims of a JWT, unverified: only to say what a policy must match."""
    payload = jwt.split(".")[1]
    return json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))


def federated_token(workspace: str, client_id: str) -> str:
    # The audience must be one the federation policy lists. Databricks' default for a
    # service principal policy is the account ID.
    audience = (
        env("DATABRICKS_TOKEN_AUDIENCE", required=False)
        or env("DATABRICKS_ACCOUNT_ID", required=False)
        or f"{workspace}/oidc/v1/token"
    )
    print(f"federation: requesting a GitHub OIDC token for audience {audience}")
    subject = github_oidc_token(audience)

    def exchange(scope: str) -> str:
        got = request(
            "POST",
            f"{workspace}/oidc/v1/token",
            form={
                "grant_type": TOKEN_EXCHANGE,
                "client_id": client_id,
                "subject_token": subject,
                "subject_token_type": JWT,
                "scope": scope,
            },
        )
        return got["access_token"]

    try:
        return with_scopes(exchange)
    except Failed:
        # Say exactly what a federation policy must match. These claims name the
        # repository and environment; they aren't credentials.
        c = claims(subject)
        summary(
            "**Federation policy this job needs:** "
            f"issuer `{c.get('iss')}`, subject `{c.get('sub')}`, audience `{c.get('aud')}`"
        )
        raise


def secret_token(workspace: str, client_id: str) -> str:
    secret = env("DATABRICKS_CLIENT_SECRET", required=False)
    if not secret:
        raise Failed("no DATABRICKS_CLIENT_SECRET to fall back to")
    basic = base64.b64encode(f"{client_id}:{secret}".encode()).decode()

    def grant(scope: str) -> str:
        got = request(
            "POST",
            f"{workspace}/oidc/v1/token",
            form={"grant_type": "client_credentials", "scope": scope},
            headers={"Authorization": f"Basic {basic}"},
        )
        return got["access_token"]

    return with_scopes(grant)


def export(name: str, value: str) -> None:
    path = os.environ.get("GITHUB_ENV")
    if not path:
        raise Failed("GITHUB_ENV is not set: `token` only runs inside GitHub Actions")
    with open(path, "a", encoding="utf-8") as f:
        f.write(f"{name}={value}\n")


def summary(line: str) -> None:
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(line + "\n")


def cmd_token() -> None:
    workspace = host()
    # The environment's secret masking only hides the value as stored (with its
    # scheme); ODS and dbt print the bare hostname, so mask that too.
    print(f"::add-mask::{urllib.parse.urlsplit(workspace).hostname}")
    client_id = env("DATABRICKS_CLIENT_ID")
    method, token, refused = None, None, []
    for name, get in (
        ("workload identity federation", federated_token),
        ("service principal OAuth secret", secret_token),
    ):
        try:
            token = get(workspace, client_id)
            method = name
            break
        except (Failed, KeyError) as e:
            refused.append(f"{name}: {e}")
            print(f"::warning::{name} unavailable: {e}")
    if token is None:
        raise Failed("no way to authenticate:\n  " + "\n  ".join(refused))
    # Mask before anything could print it.
    print(f"::add-mask::{token}")
    export("DATABRICKS_TOKEN", token)
    export("ODS_DATABRICKS_AUTH", method)
    print(f"authenticated with {method}")
    summary(f"**Databricks auth:** {method}")
    for why in refused:
        summary(f"- fell back: {why}")


def warehouse_id() -> str:
    path = env("DATABRICKS_HTTP_PATH").rstrip("/")
    return path.rsplit("/", 1)[-1]


def token_headers() -> dict[str, str]:
    return {"Authorization": f"Bearer {env('DATABRICKS_TOKEN')}"}


def cmd_sql(statement: str) -> None:
    workspace = host()
    got = request(
        "POST",
        f"{workspace}/api/2.0/sql/statements",
        body={
            "warehouse_id": warehouse_id(),
            "statement": statement,
            "wait_timeout": "50s",
            "on_wait_timeout": "CONTINUE",
        },
        headers=token_headers(),
    )
    deadline = time.monotonic() + STATEMENT_TIMEOUT_S
    while got["status"]["state"] in ("PENDING", "RUNNING"):
        if time.monotonic() > deadline:
            raise Failed(f"statement still {got['status']['state']} after {STATEMENT_TIMEOUT_S}s")
        time.sleep(5)
        got = request(
            "GET",
            f"{workspace}/api/2.0/sql/statements/{got['statement_id']}",
            headers=token_headers(),
        )
    state = got["status"]["state"]
    if state != "SUCCEEDED":
        error = got["status"].get("error", {}).get("message", "")
        raise Failed(f"statement {state}: {error}")
    columns = [c["name"] for c in got.get("manifest", {}).get("schema", {}).get("columns", [])]
    rows = got.get("result", {}).get("data_array", []) or []
    print(f"ok: {statement}")
    for row in rows:
        print("  " + ", ".join(f"{c}={v}" for c, v in zip(columns, row)))


def main(argv: list[str]) -> int:
    try:
        match argv:
            case ["token"]:
                cmd_token()
            case ["sql", statement]:
                cmd_sql(statement)
            case _:
                print(__doc__, file=sys.stderr)
                return 2
    except Failed as e:
        print(f"::error::{e}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
