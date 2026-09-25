//! Exact dbt selection (#211): `--select` arguments that match exactly the requested
//! nodes, however the project names its folders and packages.
//!
//! dbt has no selector for one node by id. A bare name (`orders`) also matches a folder
//! or package of that name, and `fqn:` matches by prefix, so `fqn:shop.marts.orders`
//! also selects everything in a folder `models/marts/orders/`. So each selector is
//! checked here against the manifest, with dbt's own matching rules, before dbt runs:
//! - A node is selected by `fqn:<its fqn>,resource_type:<its type>`. The resource type
//!   keeps tests, sources and other kinds out of the direct selection; tests of the
//!   selected nodes still run through dbt's indirect selection, as with any selection.
//! - If that would also match another node of the same type, the node's file narrows
//!   it: `path:<file>,fqn:<fqn>,resource_type:<type>`. That only works for the root
//!   project's own files. A package node that can't be selected exactly is an error:
//!   ODS doesn't run what it didn't plan.
//! - To keep the command short, a folder (an fqn prefix) whose nodes of a type are all
//!   requested is selected as a whole.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Manifest, ManifestNode, ResourceType};

/// Characters dbt treats as wildcards in `fqn:` and `path:` selectors.
const WILDCARDS: [char; 4] = ['*', '?', '[', ']'];

fn type_name(t: ResourceType) -> Option<&'static str> {
    match t {
        ResourceType::Model => Some("model"),
        ResourceType::Seed => Some("seed"),
        ResourceType::Snapshot => Some("snapshot"),
        _ => None,
    }
}

/// dbt's `is_selected_node` (without wildcards, which are never emitted).
fn is_selected_node(fqn: &[String], versioned: bool, selector: &str) -> bool {
    let parts: Vec<&str> = selector.split('.').collect();
    if versioned {
        // A versioned node's fqn ends in its name and version; anything shorter is
        // evidence ODS can't reason about, so it counts as reached.
        if fqn.len() < 2 || fqn[fqn.len() - 2] == selector {
            return true;
        }
        let node_tail = fqn[fqn.len() - 2..].join("_");
        let sel_tail = parts[parts.len().saturating_sub(2)..].join("_");
        if node_tail == sel_tail {
            return true;
        }
    } else if fqn.last().is_some_and(|leaf| leaf == selector) {
        return true;
    }
    let flat: Vec<&str> = fqn.iter().flat_map(|s| s.split('.')).collect();
    flat.len() >= parts.len() && flat.iter().zip(&parts).all(|(a, b)| a == b)
}

/// Whether `fqn:<selector>` selects a node with this fqn. Like dbt's
/// `QualifiedNameSelectorMethod`, it also matches the fqn without its package ("across
/// packages"): `fqn:stripe` reaches the root project's `models/stripe/` folder. A node
/// without an fqn counts as reached (AGENTS rule 3).
fn fqn_selects(fqn: &[String], versioned: bool, selector: &str) -> bool {
    fqn.is_empty()
        || is_selected_node(fqn, versioned, selector)
        || (fqn.len() > 1 && is_selected_node(&fqn[1..], versioned, selector))
}

/// Whether a selector value is taken literally by dbt: no wildcards, no `.` inside a
/// segment, and nothing its selector syntax splits on or reads as an operator
/// (` ` separates selectors, `,` intersects, `+`/`@` select relatives, `:` a method).
fn is_literal(value: &str) -> bool {
    !value.contains(WILDCARDS) && !value.contains([' ', ',', '+', '@', ':', '\t', '\n'])
}

/// A node a selector could reach: its id, type and fqn.
struct Candidate<'m> {
    id: &'m str,
    kind: &'static str,
    fqn: &'m [String],
    versioned: bool,
}

fn candidates(manifest: &Manifest) -> Vec<Candidate<'_>> {
    manifest
        .nodes
        .iter()
        .filter_map(|n: &ManifestNode| {
            Some(Candidate {
                id: &n.unique_id,
                kind: type_name(n.resource_type)?,
                fqn: &n.fqn,
                versioned: n.version.is_some(),
            })
        })
        .collect()
}

/// The `--select` arguments that build exactly `requested` (node ids).
///
/// # Errors
/// Names the nodes that can't be selected exactly: not in the manifest, without an
/// fqn, or a package node whose fqn also matches other nodes.
pub fn exact_selectors(manifest: &Manifest, requested: &[String]) -> Result<Vec<String>, String> {
    let wanted: BTreeSet<&str> = requested.iter().map(String::as_str).collect();
    let all = candidates(manifest);
    let by_id: BTreeMap<&str, &ManifestNode> = manifest
        .nodes
        .iter()
        .map(|n| (n.unique_id.as_str(), n))
        .collect();
    let root_package = manifest.project_name.as_deref();
    // Everything a selector of this type and fqn would reach.
    let reach = |kind: &str, selector: &str| -> Vec<&str> {
        all.iter()
            .filter(|c| c.kind == kind && fqn_selects(c.fqn, c.versioned, selector))
            .map(|c| c.id)
            .collect()
    };
    let mut selectors = BTreeSet::new();
    let mut covered: BTreeSet<&str> = BTreeSet::new();
    let mut problems = Vec::new();
    for id in requested {
        if covered.contains(id.as_str()) {
            continue;
        }
        let Some(node) = by_id.get(id.as_str()) else {
            problems.push(format!("{id} isn't in the manifest"));
            continue;
        };
        let Some(kind) = type_name(node.resource_type) else {
            problems.push(format!("{id} isn't a model, seed or snapshot"));
            continue;
        };
        if node.fqn.is_empty() || node.fqn.iter().any(|p| p.contains('.') || !is_literal(p)) {
            problems.push(format!("{id} has no fqn that can be selected safely"));
            continue;
        }
        // The shortest fqn prefix that reaches only requested nodes: a whole folder
        // when every node of this type in it is requested, else the node itself.
        let chosen = (1..=node.fqn.len()).find_map(|len| {
            let selector = node.fqn[..len].join(".");
            let hits = reach(kind, &selector);
            hits.iter()
                .all(|h| wanted.contains(h))
                .then_some((selector, hits))
        });
        if let Some((selector, hits)) = chosen {
            covered.extend(hits);
            selectors.insert(format!("fqn:{selector},resource_type:{kind}"));
            continue;
        }
        // The full fqn also reaches other nodes (e.g. a folder named like the node):
        // narrow it to the node's own file.
        let own_file = node.original_file_path.as_deref().filter(|p| is_literal(p));
        let package = id.split('.').nth(1);
        match own_file {
            Some(path) if package.is_some() && package == root_package => {
                selectors.insert(format!(
                    "path:{path},fqn:{},resource_type:{kind}",
                    node.fqn.join(".")
                ));
                covered.insert(id.as_str());
            }
            _ => problems.push(format!(
                "{id} can't be selected without also selecting {}",
                reach(kind, &node.fqn.join("."))
                    .into_iter()
                    .filter(|h| *h != id.as_str() && !wanted.contains(h))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
    if problems.is_empty() {
        Ok(selectors.into_iter().collect())
    } else {
        Err(problems.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fqn(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn fqn_matching_follows_dbt() {
        let orders = fqn(&["shop", "marts", "orders"]);
        assert!(fqn_selects(&orders, false, "orders"));
        assert!(fqn_selects(&orders, false, "shop.marts.orders"));
        assert!(fqn_selects(&orders, false, "shop.marts"));
        assert!(!fqn_selects(&orders, false, "shop.staging"));
        // A folder named like a model: the model's full fqn is a prefix of its nodes.
        let nested = fqn(&["shop", "marts", "orders", "orders_detail"]);
        assert!(fqn_selects(&nested, false, "shop.marts.orders"));
        // Versioned models also match on `name_vN` across folders.
        let v2 = fqn(&["shop", "marts", "orders", "v2"]);
        assert!(fqn_selects(&v2, true, "orders"));
        assert!(fqn_selects(&v2, true, "other.place.orders.v2"));
        assert!(!fqn_selects(&v2, true, "shop.marts.orders.v1"));
        // Across packages: the fqn without its package also counts.
        let revenue = fqn(&["jaffle", "stripe", "fct_revenue"]);
        assert!(fqn_selects(&revenue, false, "stripe"));
        let extra = fqn(&["jaffle", "jaffle", "marts", "orders", "extra"]);
        assert!(fqn_selects(&extra, false, "jaffle.marts.orders"));
        // Missing evidence counts as reached.
        assert!(fqn_selects(&[], false, "anything"));
        assert!(!is_literal("old marts"));
        assert!(!is_literal("a,b"));
        assert!(!is_literal("x+"));
    }
}
