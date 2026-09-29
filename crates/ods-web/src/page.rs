//! The explorer page and static site export.

use std::fs;
use std::path::Path;

use ods_lineage::GraphDocument;

/// The offline explorer page (no shell, no overlay: those need the server). The graph
/// JSON replaces [`GRAPH_PLACEHOLDER`]; when it isn't replaced, the page fetches the
/// graph from `graph.json`, as named by [`SOURCE_PLACEHOLDER`].
const EXPLORER: &str = include_str!("../assets/explorer.html");
/// The layout library, @dagrejs/dagre 1.1.8 (MIT, see `assets/vendor/LICENSE-dagre`).
/// It's inlined into the page rather than served beside it, so the single offline file
/// works and the server's CSP (inline scripts only) holds.
pub(crate) const DAGRE: &str = include_str!("../assets/vendor/dagre.min.js");
/// The dashboard's tokens and base styles, so the offline page looks like the served one.
const DASHBOARD_CSS: &str = include_str!("../assets/dashboard.css");
const CSS_PLACEHOLDER: &str = "/*__ODS_CSS__*/";
const BODY_PLACEHOLDER: &str = "<!--__ODS_BODY__-->";
const DAGRE_PLACEHOLDER: &str = "/*__ODS_DAGRE__*/";
const SCRIPT_PLACEHOLDER: &str = "/*__ODS_SCRIPT__*/";
const GRAPH_PLACEHOLDER: &str = "/*__ODS_GRAPH__*/";
const SOURCE_PLACEHOLDER: &str = "__ODS_SOURCE__";
const GENERATION_PLACEHOLDER: &str = "__ODS_GENERATION__";

/// The offline page, with `embedded` as its graph (if any), loading from `source`
/// (`embedded` or `graph.json`).
pub(crate) fn page(
    embedded: Option<&GraphDocument>,
    source: &str,
    generation: u64,
) -> Result<String, serde_json::Error> {
    let graph = match embedded {
        Some(document) => crate::lineage::embeddable(document)?,
        None => String::new(),
    };
    // The inlined assets go in first and hold no placeholder, and the graph goes in
    // last, so a value in the graph can never be mistaken for one.
    Ok(EXPLORER
        .replacen(SOURCE_PLACEHOLDER, source, 1)
        .replacen(GENERATION_PLACEHOLDER, &generation.to_string(), 1)
        .replacen(
            CSS_PLACEHOLDER,
            &format!("{DASHBOARD_CSS}\n{}", crate::lineage::CSS),
            1,
        )
        .replacen(BODY_PLACEHOLDER, &crate::lineage::explorer_markup(false), 1)
        .replacen(DAGRE_PLACEHOLDER, DAGRE, 1)
        .replacen(SCRIPT_PLACEHOLDER, crate::lineage::JS, 1)
        .replacen(GRAPH_PLACEHOLDER, &graph, 1))
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

/// Writes `path` via a temporary file in the same directory and a rename. The temporary
/// file is removed if anything fails.
fn replace(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Temporary files are private (0600). A site is published, often served by a web
    // server running as another user, so the file gets the mode `fs::write` would give
    // it: 0666 less the umask, or the mode of the file it replaces.
    #[cfg(unix)]
    let mut temporary = {
        use std::os::unix::fs::PermissionsExt;
        let existing = fs::metadata(path).ok().map(|m| m.permissions());
        let temporary = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o666))
            .tempfile_in(dir)?;
        if let Some(permissions) = existing {
            temporary.as_file().set_permissions(permissions)?;
        }
        temporary
    };
    #[cfg(not(unix))]
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut temporary, contents)?;
    temporary.persist(path).map(drop).map_err(|e| e.error)
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

    #[cfg(unix)]
    #[test]
    fn exported_files_are_as_readable_as_written_ones() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let reference = dir.path().join("reference");
        fs::write(&reference, "x").unwrap();

        let new = dir.path().join("new.html");
        replace(&new, b"page").unwrap();
        assert_eq!(mode(&new), mode(&reference), "0666 less the umask");

        let kept = dir.path().join("kept.html");
        fs::write(&kept, "old").unwrap();
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o640)).unwrap();
        replace(&kept, b"page").unwrap();
        assert_eq!(mode(&kept), 0o640, "a replaced file keeps its mode");
        assert_eq!(fs::read(&kept).unwrap(), b"page");
    }

    #[test]
    fn pages_inline_the_layout_library() {
        assert!(
            !DAGRE.contains("__ODS_"),
            "dagre can't collide with a placeholder"
        );
        assert!(
            !DAGRE.to_ascii_lowercase().contains("</script"),
            "dagre can't end its element early"
        );
        let page = page(None, "graph.json", 0).unwrap();
        assert!(page.contains(&format!("<script>{DAGRE}</script>")));
        assert!(!page.contains(DAGRE_PLACEHOLDER));
    }
}
