//! Search over nodes and columns, shared by the API (and, in spirit, the page).

use ods_lineage::GraphDocument;
use serde::Serialize;

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SearchHit {
    /// The node id.
    pub node: String,
    /// The column, for column hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    /// What to show, e.g. `orders.amount`.
    pub label: String,
    /// `model`, `seed`, `source`, `snapshot` or `column`.
    pub kind: String,
}

/// Case-insensitive search: every whitespace-separated term must appear in the node
/// name, id, or `name.column`. Prefix matches rank first, then shorter labels; ties are
/// broken by label so results are deterministic.
pub fn search(document: &GraphDocument, query: &str, limit: usize) -> Vec<SearchHit> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let terms: Vec<&str> = query.split_whitespace().collect();
    let mut hits: Vec<(bool, SearchHit)> = Vec::new();
    for node in &document.nodes {
        let kind = serde_json::to_value(node.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let haystack = format!("{} {}", node.name, node.id).to_lowercase();
        if terms.iter().all(|t| haystack.contains(t)) {
            hits.push((
                haystack.starts_with(&query),
                SearchHit {
                    node: node.id.clone(),
                    column: None,
                    label: node.name.clone(),
                    kind: kind.clone(),
                },
            ));
        }
        for column in &node.columns {
            let label = format!("{}.{column}", node.name);
            let haystack = label.to_lowercase();
            if terms.iter().all(|t| haystack.contains(t)) {
                hits.push((
                    haystack.starts_with(&query),
                    SearchHit {
                        node: node.id.clone(),
                        column: Some(column.clone()),
                        label,
                        kind: "column".into(),
                    },
                ));
            }
        }
    }
    hits.sort_by(|(pa, a), (pb, b)| {
        pb.cmp(pa)
            .then(a.label.len().cmp(&b.label.len()))
            .then_with(|| a.label.cmp(&b.label))
    });
    hits.into_iter().take(limit).map(|(_, hit)| hit).collect()
}
