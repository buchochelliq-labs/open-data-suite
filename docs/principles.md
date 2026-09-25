# Principles

These rules are **release gates**. Reviewers reject changes that break them, and
several are checked by CI. They come from
[`AGENTS.md`](https://github.com/buchochelliq-labs/open-data-suite/blob/main/AGENTS.md),
which humans and AI coding agents both follow.

## 1. Conservative by default

When evidence is missing or uncertain, ODS takes the safer action. It builds rather
than skips, denies rather than allows, and marks a result *inferred* rather than
presenting it as fact. It never silently reuses stale results, and never silently
allows a destructive action.

*In practice:* a model ODS can't analyze is always treated as affected by a change.
A relationship guessed from column names is labelled `inferred` and appears only when
you ask for guesses.

## 2. Explainable

Every planner decision and every finding carries a **reason chain and evidence**,
which can be rendered as human text and as JSON. If ODS says a model must run, it says
which changed column reaches it and how.

## 3. Provider-neutral core

The core and the modules never branch on a vendor's name. Differences between
warehouses and tools are expressed as **capabilities**, and planners always end in a
conservative fallback. Vendor-specific code lives in `providers/`, and CI rejects
vendor names in core code.

## 4. Canonical state is only replaced on success

A failed or partial run never overwrites the last successful state.

## 5. An ERD is not lineage

Entity relationships (keys and references) and data lineage (which columns feed which)
are separate domain types. A dependency edge in the DAG is not a primary key /
foreign key relationship.

## 6. Presentation is separate from logic

Commands produce view models. Rendering (styled text, plain text, JSON) happens only at
the edge, so every command has machine-readable output.

## 7. Interoperability through public formats

ODS works with other tools through their **public artifact formats**, and is built on
open-source code used under its licence. It doesn't include or link to proprietary
code or binaries. See [Legal and trademarks](legal.md).

## 8. Secrets are referenced, never stored

Configuration only accepts references to credentials (`{ secret = "env:NAME" }`). No
code path writes a resolved secret into configuration, state, events or logs.

## 9. Local and private by default

Today's commands read local files. They make no outbound network connections and
collect no telemetry. (`ods serve` accepts HTTP connections, on loopback by default.)
Planned warehouse features will connect only when you configure them. The MCP server's
tools are read-only.
