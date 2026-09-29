//! What a dbt invocation did (`run_results.json`) and how fresh sources were
//! (`sources.json`, from `dbt source freshness`). Used by State (#16, #24, ADR-0013).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::artifacts::{DbtError, check_version, read};

/// `run_results.json` versions read.
const RUN_RESULTS_VERSIONS: [u32; 3] = [4, 5, 6];
/// `sources.json` versions read.
const SOURCES_VERSIONS: [u32; 2] = [2, 3];

/// How a node's execution ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum RunStatus {
    /// `success` (models, seeds, snapshots) or `pass`/`warn` (tests).
    Success,
    /// `error`, `fail` or `runtime error`.
    Failed,
    /// `skipped`, e.g. because an upstream node failed.
    Skipped,
    /// Anything else.
    Other,
}

impl RunStatus {
    fn parse(status: &str) -> Self {
        match status {
            "success" | "pass" | "warn" => Self::Success,
            "error" | "fail" | "runtime error" => Self::Failed,
            "skipped" => Self::Skipped,
            _ => Self::Other,
        }
    }
}

/// One node's result.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NodeResult {
    /// Node id.
    pub unique_id: String,
    /// How it ended.
    pub status: RunStatus,
    /// dbt's own status word.
    pub raw_status: String,
    /// When it finished executing, as dbt wrote it.
    pub completed_at: Option<String>,
}

/// A dbt invocation's results.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunResults {
    /// dbt's invocation id.
    pub invocation_id: Option<String>,
    /// When the invocation started, if dbt recorded it (run results v6+).
    pub started_at: Option<String>,
    /// When the file was written.
    pub generated_at: Option<String>,
    /// The dbt command that wrote it (`build`, `run`, `compile`, `generate`, …).
    pub command: Option<String>,
    /// Whether it ran with `--empty` (schema-only builds with no rows).
    pub empty: bool,
    /// The `--vars` it ran with, as dbt parsed them (`{}` when none).
    pub vars: Option<serde_json::Value>,
    /// Per-node results, in file order.
    pub results: Vec<NodeResult>,
}

#[derive(Deserialize)]
struct RawRunResults {
    metadata: RawRunMetadata,
    #[serde(default)]
    args: RawArgs,
    #[serde(default)]
    results: Vec<RawNodeResult>,
}

#[derive(Deserialize)]
struct RawRunMetadata {
    dbt_schema_version: String,
    #[serde(default)]
    invocation_id: Option<String>,
    #[serde(default)]
    invocation_started_at: Option<String>,
    #[serde(default)]
    generated_at: Option<String>,
}

#[derive(Default, Deserialize)]
struct RawArgs {
    #[serde(default)]
    which: Option<String>,
    #[serde(default)]
    empty: Option<bool>,
    #[serde(default)]
    vars: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawNodeResult {
    unique_id: String,
    status: String,
    #[serde(default)]
    timing: Vec<RawTiming>,
}

#[derive(Deserialize)]
struct RawTiming {
    name: String,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
}

impl RunResults {
    /// Reads `run_results.json`.
    ///
    /// # Errors
    /// Returns [`DbtError`] if the file can't be read or isn't a supported version.
    pub fn read(path: &Path) -> Result<Self, DbtError> {
        let raw: RawRunResults =
            serde_json::from_str(&read(path)?).map_err(|e| DbtError::Invalid {
                path: path.to_owned(),
                artifact: "run results",
                message: e.to_string(),
            })?;
        check_version(
            path,
            "run results",
            &raw.metadata.dbt_schema_version,
            &RUN_RESULTS_VERSIONS,
        )?;
        let mut earliest: Option<String> = None;
        let results = raw
            .results
            .into_iter()
            .map(|r| {
                for t in &r.timing {
                    if let Some(start) = &t.started_at
                        && earliest.as_ref().is_none_or(|e| start < e)
                    {
                        earliest = Some(start.clone());
                    }
                }
                let completed_at = r
                    .timing
                    .iter()
                    .rev()
                    .find(|t| t.name == "execute")
                    .or_else(|| r.timing.last())
                    .and_then(|t| t.completed_at.clone());
                NodeResult {
                    status: RunStatus::parse(&r.status),
                    raw_status: r.status,
                    unique_id: r.unique_id,
                    completed_at,
                }
            })
            .collect();
        Ok(Self {
            invocation_id: raw.metadata.invocation_id,
            // Older versions have no start time: the earliest step stands in for it.
            started_at: raw.metadata.invocation_started_at.or(earliest),
            generated_at: raw.metadata.generated_at,
            command: raw.args.which,
            empty: raw.args.empty == Some(true),
            vars: raw.args.vars,
            results,
        })
    }
}

/// `max(loaded_at)` of each source, as `dbt source freshness` measured it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceFreshness {
    /// dbt's invocation id.
    pub invocation_id: Option<String>,
    /// When the measurement was taken.
    pub generated_at: Option<String>,
    /// Source id → `max_loaded_at`. Sources whose check failed are absent.
    pub max_loaded_at: BTreeMap<String, String>,
    /// Sources whose check errored, with dbt's status.
    pub errors: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct RawSources {
    metadata: RawRunMetadata,
    #[serde(default)]
    results: Vec<RawSourceResult>,
}

#[derive(Deserialize)]
struct RawSourceResult {
    unique_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    max_loaded_at: Option<String>,
}

impl SourceFreshness {
    /// Reads `sources.json`.
    ///
    /// # Errors
    /// Returns [`DbtError`] if the file can't be read or isn't a supported version.
    pub fn read(path: &Path) -> Result<Self, DbtError> {
        let raw: RawSources =
            serde_json::from_str(&read(path)?).map_err(|e| DbtError::Invalid {
                path: path.to_owned(),
                artifact: "sources",
                message: e.to_string(),
            })?;
        check_version(
            path,
            "sources",
            &raw.metadata.dbt_schema_version,
            &SOURCES_VERSIONS,
        )?;
        let mut max_loaded_at = BTreeMap::new();
        let mut errors = BTreeMap::new();
        for result in raw.results {
            match result
                .max_loaded_at
                .filter(|_| result.status != "runtime error")
            {
                Some(at) => {
                    max_loaded_at.insert(result.unique_id, at);
                }
                None => {
                    errors.insert(result.unique_id, result.status);
                }
            }
        }
        Ok(Self {
            invocation_id: raw.metadata.invocation_id,
            generated_at: raw.metadata.generated_at,
            max_loaded_at,
            errors,
        })
    }
}
