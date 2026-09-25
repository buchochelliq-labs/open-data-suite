//! Reads dbt's public JSON artifacts (#12).
//!
//! Only the fields ODS needs are modelled, and unknown fields are ignored, so one set of
//! types reads every supported schema version. The field meanings follow the Apache-2.0
//! artifact schemas published in `dbt-labs/dbt` (`schemas/dbt/`, branch `1.latest`);
//! nothing here depends on proprietary dbt code (AGENTS.md rule 8).
//!
//! | Artifact | Versions | dbt |
//! |---|---|---|
//! | `manifest.json` | v11, v12 | 1.7 – 1.12, v2 (JSON output) |
//! | `catalog.json` | v1 | all |
//! | dbt Information Schema (Parquet, `target/info_schema/v1/`) | v1 | v2 |
//!
//! dbt v2 writes `manifest.json` by default and the Information Schema with
//! `--generate-info-schema`; with `--no-write-json` only the latter exists. Both read into
//! the same [`Manifest`].

mod artifacts;
mod info_schema;
pub mod state_config;

pub use artifacts::{
    ArtifactPreference, ArtifactSource, Artifacts, Catalog, DbtConfig, DbtConstraint, DbtError,
    DbtTest, Manifest, ManifestNode, ResourceType,
};
