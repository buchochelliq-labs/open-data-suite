//! Reading exports of `system.access.column_lineage`.

use std::fs;
use std::path::{Path, PathBuf};

use ods_core::{CapabilitySet, ColumnRef, RelationName};
use ods_sdk::contracts::observed_lineage::{ObservedLineage, ObservedLineageSource};
use ods_sdk::{Provider, ProviderError, ProviderInfo};
use serde::Deserialize;

use crate::KIND;

/// How an export is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExportFormat {
    /// CSV with a header row, as downloaded from a SQL warehouse or notebook.
    Csv,
    /// A JSON array of objects, or one object per line.
    Json,
}

impl ExportFormat {
    /// The format a file name implies: `.csv`, or `.json`/`.jsonl`/`.ndjson`.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "json" | "jsonl" | "ndjson" => Some(Self::Json),
            _ => None,
        }
    }
}

/// An export of Unity Catalog's `system.access.column_lineage`.
///
/// Only the columns lineage needs are read, by name, so exports may contain any subset
/// that includes them, in any order:
/// - `source_table_full_name` (or `source_table_catalog`/`_schema`/`_name`);
/// - `source_column_name`;
/// - `target_table_full_name` (or `target_table_catalog`/`_schema`/`_name`);
/// - `target_column_name`;
/// - `event_time` (optional).
///
/// Rows with no source table (file paths) or no target table (plain reads) are
/// counted as skipped. A row with a target table and no target column is a row input
/// of that table.
#[derive(Debug, Clone)]
pub struct UcColumnLineage {
    path: PathBuf,
    format: ExportFormat,
}

impl UcColumnLineage {
    /// An export at `path` in `format`.
    pub fn new(path: impl Into<PathBuf>, format: ExportFormat) -> Self {
        Self {
            path: path.into(),
            format,
        }
    }

    /// An export at `path`, with the format from its extension.
    ///
    /// # Errors
    /// Returns [`ProviderError::Other`] if the extension isn't `.csv`, `.json`, `.jsonl`
    /// or `.ndjson`.
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self, ProviderError> {
        let path = path.into();
        let format = ExportFormat::from_path(&path).ok_or_else(|| {
            ProviderError::Other(format!(
                "`{}`: can't tell the format; name it .csv, .json or .ndjson",
                path.display()
            ))
        })?;
        Ok(Self::new(path, format))
    }

    fn error(&self, message: impl std::fmt::Display) -> ProviderError {
        ProviderError::Other(format!("`{}`: {message}", self.path.display()))
    }

    fn rows(&self) -> Result<Vec<Row>, ProviderError> {
        let text = fs::read_to_string(&self.path).map_err(|e| self.error(e))?;
        match self.format {
            ExportFormat::Csv => {
                let mut reader = csv::Reader::from_reader(text.as_bytes());
                let headers = reader.headers().map_err(|e| self.error(e))?.clone();
                self.check_columns(headers.iter())?;
                reader
                    .deserialize()
                    .collect::<Result<_, _>>()
                    .map_err(|e| self.error(e))
            }
            ExportFormat::Json => {
                let objects: Vec<serde_json::Map<String, serde_json::Value>> =
                    if text.trim_start().starts_with('[') {
                        serde_json::from_str(&text).map_err(|e| self.error(e))?
                    } else {
                        text.lines()
                            .filter(|l| !l.trim().is_empty())
                            .map(serde_json::from_str)
                            .collect::<Result<_, _>>()
                            .map_err(|e| self.error(e))?
                    };
                if let Some(first) = objects.first() {
                    self.check_columns(first.keys().map(String::as_str))?;
                }
                objects
                    .into_iter()
                    .map(|o| {
                        serde_json::from_value(serde_json::Value::Object(o))
                            .map_err(|e| self.error(e))
                    })
                    .collect()
            }
        }
    }

    /// Fails unless the export has the columns lineage needs, so a wrong file is an
    /// error rather than "no lineage observed".
    fn check_columns<'a>(
        &self,
        columns: impl Iterator<Item = &'a str>,
    ) -> Result<(), ProviderError> {
        let columns: std::collections::BTreeSet<&str> = columns.collect();
        let table = |side: &str| {
            columns.contains(format!("{side}_table_full_name").as_str())
                || ["catalog", "schema", "name"]
                    .iter()
                    .all(|part| columns.contains(format!("{side}_table_{part}").as_str()))
        };
        let mut missing = Vec::new();
        for side in ["source", "target"] {
            if !table(side) {
                missing.push(format!("{side}_table_full_name"));
            }
            if !columns.contains(format!("{side}_column_name").as_str()) {
                missing.push(format!("{side}_column_name"));
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(self.error(format!(
                "not a column_lineage export: missing {}",
                missing.join(", ")
            )))
        }
    }
}

/// One row, with every column optional so a missing column is reported by name.
#[derive(Debug, Default, Deserialize)]
struct Row {
    source_table_full_name: Option<String>,
    source_table_catalog: Option<String>,
    source_table_schema: Option<String>,
    source_table_name: Option<String>,
    source_column_name: Option<String>,
    target_table_full_name: Option<String>,
    target_table_catalog: Option<String>,
    target_table_schema: Option<String>,
    target_table_name: Option<String>,
    target_column_name: Option<String>,
    event_time: Option<String>,
}

/// An empty CSV cell or JSON `null` is `None`; so is `""`.
fn present(value: Option<&String>) -> Option<&str> {
    value.map(String::as_str).filter(|v| !v.trim().is_empty())
}

impl Row {
    fn table(
        full: Option<&String>,
        catalog: Option<&String>,
        schema: Option<&String>,
        name: Option<&String>,
    ) -> Option<RelationName> {
        // The parts are exact; the full name is split on dots, which is right for every
        // name UC writes unquoted.
        match (present(catalog), present(schema), present(name)) {
            (Some(c), Some(s), Some(n)) => RelationName::new([c, s, n]).ok(),
            _ => RelationName::new(present(full)?.split('.')).ok(),
        }
    }

    fn source(&self) -> Option<ColumnRef> {
        let table = Self::table(
            self.source_table_full_name.as_ref(),
            self.source_table_catalog.as_ref(),
            self.source_table_schema.as_ref(),
            self.source_table_name.as_ref(),
        )?;
        Some(ColumnRef::new(
            table,
            present(self.source_column_name.as_ref())?,
        ))
    }

    fn target(&self) -> Option<(RelationName, Option<String>)> {
        let table = Self::table(
            self.target_table_full_name.as_ref(),
            self.target_table_catalog.as_ref(),
            self.target_table_schema.as_ref(),
            self.target_table_name.as_ref(),
        )?;
        Some((
            table,
            present(self.target_column_name.as_ref()).map(str::to_owned),
        ))
    }
}

impl Provider for UcColumnLineage {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "unity-catalog-column-lineage",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

impl ObservedLineageSource for UcColumnLineage {
    fn observed_lineage(&self) -> Result<ObservedLineage, ProviderError> {
        let rows = self.rows()?;
        let mut observed = ObservedLineage::default();
        let mut times: Vec<&str> = Vec::new();
        for row in &rows {
            observed.records += 1;
            let (Some(source), Some((target, column))) = (row.source(), row.target()) else {
                observed.skipped += 1;
                continue;
            };
            match column {
                Some(column) => observed.add_column_edge(source, ColumnRef::new(target, column)),
                None => observed.add_row_input(source, target),
            }
            if let Some(time) = present(row.event_time.as_ref()) {
                times.push(time);
            }
        }
        // ISO-8601 timestamps in one zone sort as strings.
        times.sort_unstable();
        if let (Some(first), Some(last)) = (times.first(), times.last()) {
            observed.observed_between = Some(((*first).to_owned(), (*last).to_owned()));
        }
        Ok(observed)
    }
}
