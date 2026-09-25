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

mod artifacts;

pub use artifacts::{Artifacts, Catalog, DbtError, Manifest, ManifestNode, ResourceType};
