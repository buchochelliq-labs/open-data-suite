#!/usr/bin/env python3
"""Enforce the ODS crate dependency direction (ADR-0001).

    ods-core  <-  ods-sdk / foundation  <-  modules  <-  providers  <-  ods-cli

Each workspace crate gets a layer; a crate may depend only on crates in a strictly
lower layer (or, for modules, other modules when listed in ALLOWED_MODULE_EDGES).
Unknown crate names fail the check so every new crate is placed deliberately.
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
            if target not in members or dep.get("kind") == "dev":
                continue
            dst = layer(target)
            if dst is None:
                continue  # reported on its own entry
            ok = dst < src or (src == dst == MODULE and (name, target) in ALLOWED_MODULE_EDGES)
            if not ok:
                errors.append(f"{name} -> {target}: violates dependency direction")
    for e in errors:
        print(f"layering: {e}", file=sys.stderr)
    if not errors:
        print(f"layering: ok ({len(members)} crates)")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
