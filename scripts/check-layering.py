#!/usr/bin/env python3
"""Enforce the ODS crate dependency direction (ADR-0001).

    ods-core  <-  foundation  <-  ods-sdk  <-  {modules, providers}  <-  ods-cli

Each workspace crate gets a layer; a crate may depend only on crates in a strictly
lower layer, with two refinements:
  * modules may depend on other modules only when listed in ALLOWED_MODULE_EDGES;
  * providers may not depend on modules (they implement SDK contracts only).
Dev-dependencies are exempt so tests can use fakes/fixtures from any layer.
Unknown crate names fail the check so every new crate is placed deliberately.

It also confines selected third-party crates to the crates allowed to use them, e.g.
terminal rendering (rs-rich) stays in ods-cli (ADR-0003).
"""
import json
import subprocess
import sys

CORE, FOUNDATION, SDK, MODULE, PROVIDER, BINARY = range(6)

EXACT = {
    "ods-core": CORE,
    "ods-events": FOUNDATION,
    "ods-config": FOUNDATION,
    "ods-policy": FOUNDATION,
    "ods-sdk": SDK,
    "ods-cli": BINARY,
}
MODULES = {"ods-state", "ods-erd", "ods-usage", "ods-ci", "ods-lsp", "ods-agent", "ods-mesh", "ods-synthetic"}
PROVIDER_PREFIXES = ("ods-provider-", "ods-store-")

# Module -> module edges approved by an ADR, e.g. ("ods-ci", "ods-state").
ALLOWED_MODULE_EDGES: set[tuple[str, str]] = set()

# Third-party crate-name prefix -> workspace crates allowed to depend on it (any kind).
CONFINED_EXTERNAL = {
    "rs-rich": {"ods-cli"},  # ADR-0003: presentation stays at the CLI edge
}


def layer(name: str) -> int | None:
    if name in EXACT:
        return EXACT[name]
    if name in MODULES:
        return MODULE
    if name.startswith(PROVIDER_PREFIXES):
        return PROVIDER
    return None


def main() -> int:
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"]))
    members = {p["name"]: p for p in meta["packages"]}
    errors = []
    for name, pkg in sorted(members.items()):
        src = layer(name)
        if src is None:
            errors.append(f"{name}: not assigned to a layer in scripts/check-layering.py")
            continue
        for dep in pkg["dependencies"]:
            target = dep["name"]
            for prefix, allowed in CONFINED_EXTERNAL.items():
                if target.startswith(prefix) and name not in allowed:
                    errors.append(f"{name} -> {target}: only {sorted(allowed)} may depend on {prefix}*")
            if target not in members or dep.get("kind") == "dev":
                continue
            dst = layer(target)
            if dst is None:
                continue  # reported on its own entry
            if src == PROVIDER:
                ok = dst <= SDK
            elif src == dst == MODULE:
                ok = (name, target) in ALLOWED_MODULE_EDGES
            else:
                ok = dst < src
            if not ok:
                errors.append(f"{name} -> {target}: violates dependency direction")
    for e in errors:
        print(f"layering: {e}", file=sys.stderr)
    if not errors:
        print(f"layering: ok ({len(members)} crates)")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
