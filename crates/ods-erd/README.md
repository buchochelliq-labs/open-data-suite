# ods-erd

Entity-relationship models for [OpenDataSuite](https://github.com/buchochelliq-labs/open-data-suite)
(ODS): which columns identify the rows of a dbt model (keys), and which point at rows
of another model (relationships). Each key and relationship carries its basis
(declared, tested or inferred) and its evidence.

This is the model behind `ods erd`. It is published so other tools can read ODS's ERD
output with the same types, for example the JSON from `ods erd --output json`. That
JSON carries a `schema_version` (`ods_erd::SCHEMA_VERSION`).

The crate's version is the ODS product version: `ods-erd` X.Y.Z is the model of `ods`
X.Y.Z. Before 1.0 a minor release may break the API, and while versions are 0.0.x every
release may. See [ADR-0019](https://github.com/buchochelliq-labs/open-data-suite/blob/main/docs/adr/0019-release-and-versioning.md)
and [ADR-0032](https://github.com/buchochelliq-labs/open-data-suite/blob/main/docs/adr/0032-publish-ods-erd-to-crates-io.md).

Licensed under Apache-2.0.
