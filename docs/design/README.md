# UX designs

These are the two user interfaces we are building on top of the ODS engines. Both
start as mock-ups: every screen shows data from the `jaffle_ods` demo project, and
anything ODS doesn't record yet is shown as a `[placeholder]`. The dashboard's first
screens (Home, Catalog and model pages, Lineage with the State overlay, Plan, Runs and
Run) are built in `ods serve`; its README marks which.

| Design | What it is | Built on | Roadmap |
|---|---|---|---|
| [ODS Dashboard](dashboard/README.md) | The central, read-only view of a project: catalog, lineage, State plans and runs, ERD, settings. | `ods serve` and the `ods-web` crate ([ADR-0009](../adr/0009-hostable-explorer-ods-web.md)) | #74, #64, #95, #172 |
| [ODS for VS Code](vscode-extension/README.md) | The same answers where the SQL is edited: code lenses, hovers, diagnostics, lineage and runs. | `ods lsp` and the CLI | M5: #67–#72, #107 |

## Shared rules

- **Familiar, not copied.** The layout follows conventions analytics engineers already
  know from dbt tooling (a left nav, a catalog, a lineage graph, model pages with tabs),
  but uses ODS's own name, icons and colours. No dbt Labs logos, product names or
  branding ([legal](../legal.md)).
- **Every decision shows its reason.** A Build or Reuse pill always leads to the
  reason chain and evidence behind it (AGENTS rule 4). Inferred facts are labelled
  *inferred*, never drawn as fact (rule 3).
- **Read-only.** Neither interface writes configuration or state. Actions are CLI
  commands the user copies or runs in a terminal.
- **Secrets are references only.** Settings show `{ secret = "env:NAME" }`, never a
  value (rule 9).
- **Vendor-neutral.** Warehouses and build tools appear as providers with
  capabilities; no screen branches on a provider's name (rule 1).
- **Placeholders are marked.** Planned features (server mode, CI runs, the semantic
  layer) appear greyed with a *Planned* chip, so the layout has room for them without
  pretending they work.

## Open question

- **The name of the State area.** "dbt State" is now a commercial dbt product, so the
  designs keep `ods state` as the CLI but the dashboard section needs a product name.
  The leading candidate is **Builds** (Plan, Runs, History). Until it is decided the
  boards say *State*.

## Files

Each design has:

- `README.md`: the screens, what each one needs from ODS, and what is still open;
- `images/`: every screen rendered at 1440×900;
- `boards/`: the editable sources (`.dc.html`), made in a Design canvas. They need
  that canvas's runtime (`support.js`), which is not in this repository, so open them
  in the canvas rather than a browser. The PNGs are the reference.
