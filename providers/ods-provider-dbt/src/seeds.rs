//! A seed's columns from its CSV header (ADR-0008 §1: known schemas give column-level
//! lineage instead of "any column may change").
//!
//! dbt loads every column of a seed's CSV, in file order, so the header *is* the table's
//! schema. The file is only used when its SHA-256 matches the checksum dbt recorded:
//! otherwise it may not be the file dbt loaded, and the columns stay unknown.

use std::path::{Path, PathBuf};

use ods_core::state::sha256_hex;

use crate::{Manifest, ResourceType};

/// Where the project root may be: dbt's own `root_path`, then the target directory's
/// parent and a few levels above it (a target directory copied under the project).
fn roots(root_path: Option<&str>, target_dir: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = root_path.map(PathBuf::from).into_iter().collect();
    let absolute = std::path::absolute(target_dir).unwrap_or_else(|_| target_dir.to_owned());
    roots.extend(absolute.ancestors().skip(1).take(4).map(Path::to_path_buf));
    roots
}

/// Whether `bytes` is the file dbt checksummed. dbt hashes a seed's UTF-8 text with
/// surrounding whitespace stripped.
fn matches_checksum(bytes: &[u8], checksum: &str) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|text| sha256_hex(text.trim().as_bytes()) == checksum)
}

/// The header of `bytes` as CSV with `delimiter`.
fn header(bytes: &[u8], delimiter: u8) -> Option<Vec<String>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .from_reader(bytes);
    let columns: Vec<String> = reader
        .headers()
        .ok()?
        .iter()
        .map(|c| c.trim().trim_start_matches('\u{feff}').to_owned())
        .collect();
    (!columns.is_empty() && columns.iter().all(|c| !c.is_empty())).then_some(columns)
}

/// Sets [`file_columns`](crate::ManifestNode::file_columns) on every seed whose CSV is
/// found and is the one dbt loaded.
pub(crate) fn attach_columns(manifest: &mut Manifest, target_dir: &Path) {
    for node in &mut manifest.nodes {
        if node.resource_type != ResourceType::Seed {
            continue;
        }
        let (Some(file), Some(checksum)) = (&node.original_file_path, &node.checksum) else {
            continue;
        };
        let delimiter = node
            .config
            .raw
            .get("delimiter")
            .and_then(|d| d.as_str())
            .and_then(|d| d.bytes().next())
            .unwrap_or(b',');
        node.file_columns = roots(node.root_path.as_deref(), target_dir)
            .into_iter()
            .filter_map(|root| std::fs::read(root.join(file)).ok())
            .find(|bytes| matches_checksum(bytes, checksum))
            .and_then(|bytes| header(&bytes, delimiter));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_read_with_the_seed_delimiter() {
        assert_eq!(
            header(b"id,user_id\n1,2\n", b','),
            Some(vec!["id".into(), "user_id".into()])
        );
        assert_eq!(
            header(b"\xef\xbb\xbfa;\"b c\"\n", b';'),
            Some(vec!["a".into(), "b c".into()])
        );
        assert_eq!(header(b"", b','), None);
        assert_eq!(
            header(b"a,,c\n", b','),
            None,
            "a blank column name isn't a schema"
        );
    }
}
