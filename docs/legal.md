# Legal and trademarks

## Status and warranty

OpenDataSuite is **experimental, pre-alpha software**. It is provided "as is",
without warranty of any kind, express or implied. Its output (lineage, impact
analysis, diagrams, generated SQL, suggestions) may be incomplete or wrong. Review it
before acting on it. The software's licence governs its use.

## Licence

The project intends to license its code and documentation under the
[Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0). The workspace's
`Cargo.toml` declares this provisionally, and the final decision is tracked in issue
#10. The licence takes effect when a `LICENSE` file is added to the repository.

## Independence and trademarks

OpenDataSuite is an independent open-source project. It is **not affiliated with,
endorsed by or sponsored by** dbt Labs, Inc., Databricks, Inc., Anthropic, or any other
company whose products are mentioned on this site.

dbt is a trademark of dbt Labs, Inc. Databricks and Unity Catalog are trademarks of
Databricks, Inc. Snowflake, BigQuery, Redshift, DuckDB, PostgreSQL, Visual Studio Code,
Cursor, Claude and other product names are trademarks of their respective owners.
They are used only to identify those products and describe compatibility.

## Interoperability and source policy

ODS works with dbt through its **public artifact formats**: the `manifest.json`,
`catalog.json` and Parquet Information Schema files that dbt writes. It is written from
public documentation and open-source code, used under their licences. It doesn't
include or link to proprietary code or binaries. The formal contributor policy is being
written (issue #10).

## Security

ODS has not had a security audit. `ods serve` has no authentication yet, so don't
expose it to untrusted networks.

## Statements about other products

Where this site or its design records compare ODS with other tools, the comparisons
describe publicly documented behaviour as understood when they were written. They are
not claims about quality, and they may be out of date. Corrections are welcome: please
[open an issue](https://github.com/buchochelliq-labs/open-data-suite/issues).

## Demo data

The demo project (`jaffle-ods`) and the Unity Catalog lineage sample are synthetic,
fictional records. Some use the names of historical figures. The demo's layout is
inspired by dbt Labs' `jaffle_shop` example project, which is Apache-2.0 licensed.

## Privacy on this site

This site has no analytics, no cookies and no tracking. It is hosted on GitHub Pages,
which may log visits under GitHub's own privacy statement. To draw diagrams, pages
load the open-source Mermaid library from `unpkg.com`.
