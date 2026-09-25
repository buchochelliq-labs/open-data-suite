//! dbt-style node selection: `name`, `+name` (and its ancestors), `name+` (and its
//! descendants), `+name+`. Names match a node's name or id.

use std::collections::{BTreeMap, BTreeSet};

use crate::Project;

/// The ids selected by `specs`; an empty `specs` selects every node.
///
/// # Errors
/// Returns a message naming any selector that matches no node.
pub fn select(project: &Project, specs: &[String]) -> Result<BTreeSet<String>, String> {
    if specs.is_empty() {
        return Ok(project.nodes.iter().map(|n| n.id.clone()).collect());
    }
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let parents: BTreeMap<&str, &[String]> = project
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.parents.as_slice()))
        .collect();
    for node in &project.nodes {
        for parent in &node.parents {
            children.entry(parent.as_str()).or_default().push(&node.id);
        }
    }
    let mut selected = BTreeSet::new();
    let mut unmatched = Vec::new();
    for spec in specs.iter().flat_map(|s| s.split_whitespace()) {
        let (up, rest) = spec.strip_prefix('+').map_or((false, spec), |r| (true, r));
        let (down, name) = rest.strip_suffix('+').map_or((false, rest), |r| (true, r));
        let matched: Vec<&str> = project
            .nodes
            .iter()
            .filter(|n| n.id == name || n.name == name)
            .map(|n| n.id.as_str())
            .collect();
        if matched.is_empty() {
            unmatched.push(spec.to_owned());
        }
        for id in matched {
            selected.insert(id.to_owned());
            if up {
                walk(
                    id,
                    |n| {
                        parents
                            .get(n)
                            .map(|p| p.iter().map(String::as_str).collect())
                    },
                    &parents,
                    &mut selected,
                );
            }
            if down {
                walk(id, |n| children.get(n).cloned(), &parents, &mut selected);
            }
        }
    }
    if unmatched.is_empty() {
        Ok(selected)
    } else {
        Err(format!("no node matches {}", unmatched.join(", ")))
    }
}

/// Adds everything reachable from `start` through `next` that is a node.
fn walk<'a>(
    start: &'a str,
    next: impl Fn(&'a str) -> Option<Vec<&'a str>>,
    nodes: &BTreeMap<&'a str, &'a [String]>,
    selected: &mut BTreeSet<String>,
) {
    let mut stack = vec![start];
    let mut seen = BTreeSet::from([start]);
    while let Some(at) = stack.pop() {
        for n in next(at).unwrap_or_default() {
            if seen.insert(n) && nodes.contains_key(n) {
                selected.insert(n.to_owned());
                stack.push(n);
            }
        }
    }
}
