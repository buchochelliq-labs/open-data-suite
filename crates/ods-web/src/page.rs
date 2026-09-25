//! The explorer page and static site export.

use std::fs;
use std::path::Path;

use ods_lineage::GraphDocument;

/// The explorer page. The graph JSON replaces [`GRAPH_PLACEHOLDER`]; when it isn't
/// replaced, the page fetches the graph from `graph.json` (static site) or the API
/// (server), as named by [`SOURCE_PLACEHOLDER`].
const EXPLORER: &str = include_str!("../assets/explorer.html");
const GRAPH_PLACEHOLDER: &str = "/*__ODS_GRAPH__*/";
const SOURCE_PLACEHOLDER: &str = "__ODS_SOURCE__";
const GENERATION_PLACEHOLDER: &str = "__ODS_GENERATION__";

/// Serializes the graph so it can sit inside a `<script>` element: `<` is escaped, so
/// no value (e.g. a model named `</script>`) can end the element early.
fn embeddable(document: &GraphDocument) -> Result<String, serde_json::Error> {
    Ok(serde_json::to_string(document)?.replace('<', "\\u003c"))
}

/// The page, with `embedded` as its first paint (if any), loading from `source`
/// (`embedded`, `graph.json` or `api`) and, when served, knowing the snapshot
/// `generation` it shows.
pub(crate) fn page(
    embedded: Option<&GraphDocument>,
    source: &str,
    generation: u64,
) -> Result<String, serde_json::Error> {
    let graph = match embedded {
        Some(document) => embeddable(document)?,
        None => String::new(),
    };
    Ok(EXPLORER
        .replacen(GRAPH_PLACEHOLDER, &graph, 1)
        .replacen(SOURCE_PLACEHOLDER, source, 1)
        .replacen(GENERATION_PLACEHOLDER, &generation.to_string(), 1))
}

/// A single self-contained HTML page with the graph embedded. No network access, no
/// external scripts: it works offline.
///
/// # Errors
/// Returns an error if the graph can't be serialized.
pub fn standalone_page(document: &GraphDocument) -> Result<String, serde_json::Error> {
    page(Some(document), "embedded", 0)
}

/// Writes a static site to `dir`: `index.html` (which loads `graph.json`) and
/// `graph.json`. Host the directory anywhere that serves static files. Returns the files
/// written.
///
/// # Errors
/// Returns an I/O error if the directory or files can't be written.
pub fn export_site(
    document: &GraphDocument,
    dir: &Path,
) -> std::io::Result<Vec<std::path::PathBuf>> {
    fs::create_dir_all(dir)?;
    let index = dir.join("index.html");
    let graph = dir.join("graph.json");
    let html = page(None, "graph.json", 0).map_err(std::io::Error::other)?;
    // The data first, each file replaced atomically: a failure never leaves a page
    // pointing at a missing or half-written graph.
    replace(
        &graph,
        &serde_json::to_vec(document).map_err(std::io::Error::other)?,
    )?;
    replace(&index, html.as_bytes())?;
    Ok(vec![index, graph])
}

/// Writes `path` via a temporary file in the same directory and a rename.
fn replace(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = std::path::PathBuf::from(temporary);
    fs::write(&temporary, contents)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_name_their_source_and_generation() {
        let page = page(None, "graph.json", 0).unwrap();
        assert!(page.contains(r#"<meta name="ods-source" content="graph.json">"#));
        assert!(page.contains(r#"<meta name="ods-generation" content="0">"#));
        assert!(!page.contains("__ODS_"), "every placeholder is replaced");
    }
}
