//! MCP tools for people who *use* the data rather than build the dbt project: find
//! tables and columns by meaning, understand what a row is and how tables relate, and
//! get a join plan with SQL for a question.
//!
//! Every answer comes from the entity-relationship model (ADR-0012), whose keys and
//! relationships carry their evidence. The plan never invents columns or joins; the
//! model writes the final SQL from it.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt::Write as _;

use ods_erd::{Basis, Cardinality, Entity, Erd, Relationship};
use ods_mcp::ToolOutput;
use serde_json::{Value, json};

use super::erd::{ErdOptions, project_erd};
use super::mcp_tools::Project;

fn whole_erd(project: &Project, infer: bool) -> Result<Erd, ToolOutput> {
    let options = ErdOptions {
        select: Vec::new(),
        depth: 0,
        infer,
        all: true,
    };
    project_erd(project.target_dir(), project.load_options(), &options).map_err(|e| {
        ToolOutput::Error(match &e.hint {
            Some(hint) => format!("{} ({}); {hint}", e.message, e.code),
            None => format!("{} ({})", e.message, e.code),
        })
    })
}

fn basis_word(basis: Basis) -> &'static str {
    match basis {
        Basis::Declared => "declared",
        Basis::Tested => "tested",
        Basis::Joined => "joined in the project's SQL",
        _ => "inferred from naming",
    }
}

fn cardinality_word(cardinality: Cardinality) -> &'static str {
    match cardinality {
        Cardinality::ManyToOne => "many-to-one",
        Cardinality::OneToOne => "one-to-one",
        _ => "unknown (neither side is a known key)",
    }
}

/// What one row is: the primary key, with how we know.
fn grain(entity: &Entity) -> Value {
    match &entity.primary_key {
        Some(key) => json!({
            "columns": key.columns,
            "basis": basis_word(key.basis),
            "statement": format!("one row per {}", key.columns.join(" + ")),
        }),
        None => json!({
            "columns": [],
            "statement": "unknown: no tested or declared key; don't assume rows are unique",
        }),
    }
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_owned)
        .collect()
}

/// Tables and columns whose names or descriptions match the question's words.
pub(super) fn find_data(project: &Project, arguments: &Value) -> ToolOutput {
    let Some(query) = arguments["query"].as_str().filter(|q| !q.trim().is_empty()) else {
        return ToolOutput::Error("`query` is required: words describing the data you need".into());
    };
    let limit = arguments["limit"].as_u64().map_or(10, |n| n.clamp(1, 50));
    let erd = match whole_erd(project, false) {
        Ok(erd) => erd,
        Err(e) => return e,
    };
    let terms: BTreeSet<String> = words(query).into_iter().collect();
    // A term in a name counts more than one in a description; a term in the table's own
    // name or description counts more than one in a column's.
    let score = |name: &str, description: Option<&str>, weight: usize| -> usize {
        let name_words: BTreeSet<String> = words(name).into_iter().collect();
        let described: BTreeSet<String> = description
            .map(words)
            .unwrap_or_default()
            .into_iter()
            .collect();
        terms
            .iter()
            .map(|t| {
                let in_name = name_words.iter().any(|w| w.starts_with(t.as_str()));
                let in_description = described.iter().any(|w| w.starts_with(t.as_str()));
                weight * (3 * usize::from(in_name) + usize::from(in_description))
            })
            .sum()
    };
    let mut hits: Vec<(usize, Value)> = erd
        .entities
        .iter()
        .filter_map(|entity| {
            let own = score(&entity.name, entity.description.as_deref(), 2);
            let columns: Vec<(usize, Value)> = entity
                .columns
                .iter()
                .filter_map(|c| {
                    let s = score(&c.name, c.description.as_deref(), 1);
                    (s > 0).then(|| {
                        (s, json!({"name": c.name, "type": c.data_type, "description": c.description}))
                    })
                })
                .collect();
            let total = own + columns.iter().map(|(s, _)| s).sum::<usize>();
            (total > 0).then(|| {
                let mut columns = columns;
                columns.sort_by_key(|c| std::cmp::Reverse(c.0));
                (
                    total,
                    json!({
                        "entity": entity.name,
                        "id": entity.id,
                        "kind": entity.kind,
                        "description": entity.description,
                        "relation": entity.relation,
                        "grain": grain(entity),
                        "matching_columns": columns.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
                    }),
                )
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1["entity"].as_str().cmp(&b.1["entity"].as_str()))
    });
    let total = hits.len();
    ToolOutput::Json(json!({
        "query": query,
        "results": hits.into_iter().take(usize::try_from(limit).unwrap_or(10)).map(|(_, v)| v).collect::<Vec<_>>(),
        "total": total,
        "next": "Use `ods_describe_entity` on the best candidates, then `ods_plan_query`.",
    }))
}

fn relationship_json(erd: &Erd, rel: &Relationship, outgoing: bool) -> Value {
    let other = if outgoing { &rel.to } else { &rel.from };
    let other_name = erd
        .entity(other)
        .map_or(other.as_str(), |e| e.name.as_str());
    json!({
        "entity": other_name,
        "join_on": rel.from_columns.iter().zip(&rel.to_columns).map(|(f, t)| {
            if outgoing { format!("{f} = {other_name}.{t}") } else { format!("{other_name}.{f} = {t}") }
        }).collect::<Vec<_>>(),
        "cardinality": cardinality_word(rel.cardinality),
        "basis": basis_word(rel.basis),
        "evidence": rel.evidence,
    })
}

/// A table explained for someone who doesn't know the project.
pub(super) fn describe_entity(project: &Project, arguments: &Value) -> ToolOutput {
    let Some(name) = arguments["entity"]
        .as_str()
        .filter(|e| !e.trim().is_empty())
    else {
        return ToolOutput::Error("`entity` is required: a table name or id".into());
    };
    let infer = arguments["infer"].as_bool().unwrap_or(false);
    let erd = match whole_erd(project, infer) {
        Ok(erd) => erd,
        Err(e) => return e,
    };
    let Some(entity) = erd.entity(name.trim()) else {
        return ToolOutput::Error(format!(
            "no table called `{name}`; use `ods_find_data` to search by meaning"
        ));
    };
    let references: Vec<Value> = erd
        .relationships
        .iter()
        .filter(|r| r.from == entity.id)
        .map(|r| relationship_json(&erd, r, true))
        .collect();
    let referenced_by: Vec<Value> = erd
        .relationships
        .iter()
        .filter(|r| r.to == entity.id)
        .map(|r| relationship_json(&erd, r, false))
        .collect();
    let relation = entity
        .relation
        .clone()
        .unwrap_or_else(|| entity.name.clone());
    ToolOutput::Json(json!({
        "entity": entity.name,
        "id": entity.id,
        "kind": entity.kind,
        "description": entity.description,
        "relation": relation,
        "grain": grain(entity),
        "columns": entity.columns.iter().map(|c| json!({
            "name": c.name,
            "type": c.data_type,
            "description": c.description,
            "primary_key": c.primary_key,
            "references_another_table": c.foreign_key,
            "never_null": c.not_null,
        })).collect::<Vec<_>>(),
        "references": references,
        "referenced_by": referenced_by,
        "unique_keys": entity.unique_keys,
        "sample_query": format!("select *\nfrom {relation}\nlimit 10"),
    }))
}

/// How much we trust an edge for joining: tests and constraints first, then joins the
/// project already makes, then guesses. Unknown cardinality costs extra.
fn weight(rel: &Relationship) -> u32 {
    let basis = match rel.basis {
        Basis::Declared | Basis::Tested => 1,
        Basis::Joined => 2,
        _ => 4,
    };
    basis
        + if rel.cardinality == Cardinality::Unknown {
            2
        } else {
            0
        }
}

/// The cheapest path of relationships from `start` to `goal`, as edge indexes.
fn path(erd: &Erd, start: &str, goal: &str) -> Option<Vec<usize>> {
    let mut best: BTreeMap<&str, u32> = BTreeMap::from([(start, 0)]);
    let mut previous: BTreeMap<&str, usize> = BTreeMap::new();
    let mut queue = BinaryHeap::from([Reverse((0_u32, start))]);
    while let Some(Reverse((cost, node))) = queue.pop() {
        if node == goal {
            let mut edges = Vec::new();
            let mut at = goal;
            while let Some(&edge) = previous.get(at) {
                edges.push(edge);
                let rel = &erd.relationships[edge];
                at = if rel.from == at { &rel.to } else { &rel.from };
            }
            edges.reverse();
            return Some(edges);
        }
        if cost > best.get(node).copied().unwrap_or(u32::MAX) {
            continue;
        }
        for (i, rel) in erd.relationships.iter().enumerate() {
            let next = if rel.from == node {
                rel.to.as_str()
            } else if rel.to == node {
                rel.from.as_str()
            } else {
                continue;
            };
            let next_cost = cost + weight(rel);
            if next_cost < best.get(next).copied().unwrap_or(u32::MAX) {
                best.insert(next, next_cost);
                previous.insert(next, i);
                queue.push(Reverse((next_cost, next)));
            }
        }
    }
    None
}

/// Short, unique table aliases: `order_items` → `oi`.
fn aliases(names: &[String]) -> Vec<String> {
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    names
        .iter()
        .map(|name| {
            let base: String = name
                .rsplit('.')
                .next()
                .unwrap_or(name)
                .split('_')
                .filter_map(|w| w.chars().next())
                .filter(char::is_ascii_alphabetic)
                .collect::<String>()
                .to_lowercase();
            let base = if base.is_empty() {
                "t".to_owned()
            } else {
                base
            };
            let n = used.entry(base.clone()).or_default();
            *n += 1;
            if *n == 1 { base } else { format!("{base}{n}") }
        })
        .collect()
}

/// A join plan and SQL for a question over the given tables.
#[allow(clippy::too_many_lines, reason = "one plan, built top to bottom")]
pub(super) fn plan_query(project: &Project, arguments: &Value) -> ToolOutput {
    let requested: Vec<String> = arguments["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(|s| s.trim().to_owned()))
        .filter(|s| !s.is_empty())
        .collect();
    if requested.is_empty() || requested.len() > 6 {
        return ToolOutput::Error(
            "`entities` is required: 1 to 6 tables, the one whose rows you want first".into(),
        );
    }
    let infer = arguments["infer"].as_bool().unwrap_or(false);
    let erd = match whole_erd(project, infer) {
        Ok(erd) => erd,
        Err(e) => return e,
    };
    let mut ids = Vec::new();
    for name in &requested {
        match erd.entity(name) {
            Some(entity) => ids.push(entity.id.clone()),
            None => {
                return ToolOutput::Error(format!(
                    "no table called `{name}`; use `ods_find_data` to search by meaning"
                ));
            }
        }
    }
    let base = ids[0].clone();

    // Join each requested table in along the most trustworthy path, reusing tables
    // already in the query.
    let mut in_query: Vec<String> = vec![base.clone()];
    let mut steps: Vec<(usize, String)> = Vec::new(); // (edge, entity it adds)
    for target in &ids[1..] {
        if in_query.contains(target) {
            continue;
        }
        let Some(edges) = in_query
            .iter()
            .filter_map(|from| path(&erd, from, target))
            .min_by_key(|p| {
                p.iter()
                    .map(|&e| weight(&erd.relationships[e]))
                    .sum::<u32>()
            })
        else {
            let name = erd
                .entity(target)
                .map_or(target.as_str(), |e| e.name.as_str());
            return ToolOutput::Error(format!(
                "no known relationship connects `{name}` to the other tables{}",
                if infer {
                    ""
                } else {
                    "; retry with `infer: true` to also use naming conventions (labelled as guesses)"
                }
            ));
        };
        // Walk the path from whichever end is already in the query.
        let mut at: String = {
            let first = &erd.relationships[edges[0]];
            if in_query.contains(&first.from) {
                first.from.clone()
            } else {
                first.to.clone()
            }
        };
        for edge in edges {
            let rel = &erd.relationships[edge];
            let next = if rel.from == at {
                rel.to.clone()
            } else {
                rel.from.clone()
            };
            if !in_query.contains(&next) {
                in_query.push(next.clone());
                steps.push((edge, next.clone()));
            }
            at = next;
        }
    }

    let entity = |id: &str| erd.entity(id).expect("planned entities exist");
    let names: Vec<String> = in_query.iter().map(|id| entity(id).name.clone()).collect();
    let alias_of: BTreeMap<&str, String> = in_query
        .iter()
        .map(String::as_str)
        .zip(aliases(&names))
        .collect();

    // Columns: validated against the model; by default each table's key.
    let mut selected: Vec<String> = Vec::new();
    for spec in arguments["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let Some((table, column)) = spec.rsplit_once('.') else {
            return ToolOutput::Error(format!("`{spec}` is not table.column"));
        };
        let Some(e) = erd.entity(table).filter(|e| in_query.contains(&e.id)) else {
            return ToolOutput::Error(format!(
                "`{table}` is not in the query; add it to `entities`"
            ));
        };
        let Some(c) = e
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(column))
        else {
            return ToolOutput::Error(format!(
                "`{}` has no column `{column}`; its columns are: {}",
                e.name,
                e.columns
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        selected.push(format!("{}.{}", alias_of[e.id.as_str()], c.name));
    }
    if selected.is_empty() {
        for id in &in_query {
            let e = entity(id);
            let key: Vec<&str> = e
                .primary_key
                .iter()
                .flat_map(|k| k.columns.iter().map(String::as_str))
                .collect();
            let columns: Vec<&str> = if key.is_empty() {
                e.columns.iter().take(3).map(|c| c.name.as_str()).collect()
            } else {
                key
            };
            selected.extend(
                columns
                    .iter()
                    .map(|c| format!("{}.{c}", alias_of[id.as_str()])),
            );
        }
    }

    let relation = |id: &str| {
        let e = entity(id);
        e.relation.clone().unwrap_or_else(|| e.name.clone())
    };
    let mut warnings = Vec::new();
    let mut joins = Vec::new();
    let mut sql = format!(
        "select\n    {}\nfrom {} as {}",
        selected.join(",\n    "),
        relation(&base),
        alias_of[base.as_str()]
    );
    for (edge, added) in &steps {
        let rel = &erd.relationships[*edge];
        let (added_alias, other) = (
            alias_of[added.as_str()].clone(),
            if rel.from == *added {
                &rel.to
            } else {
                &rel.from
            },
        );
        let other_alias = alias_of[other.as_str()].clone();
        let conditions: Vec<String> = rel
            .from_columns
            .iter()
            .zip(&rel.to_columns)
            .map(|(f, t)| {
                format!(
                    "{}.{f} = {}.{t}",
                    alias_of[rel.from.as_str()],
                    alias_of[rel.to.as_str()]
                )
            })
            .collect();
        // Joining the "many" side onto the "one" side repeats rows of the query so far.
        let fans_out = match rel.cardinality {
            Cardinality::ManyToOne => rel.from == *added,
            Cardinality::OneToOne => false,
            _ => true,
        };
        let added_name = entity(added).name.clone();
        let other_name = entity(other).name.clone();
        if fans_out {
            warnings.push(if rel.cardinality == Cardinality::Unknown {
                format!(
                    "`{added_name}` joins `{other_name}` on columns that aren't a known key on either side: \
                     rows may be duplicated; check the grain or aggregate first"
                )
            } else {
                format!(
                    "`{added_name}` has many rows per `{other_name}` row: the result repeats `{other_name}` rows. \
                     Aggregate `{added_name}` in a subquery first if you need one row per `{other_name}`"
                )
            });
        }
        if rel.basis == Basis::Inferred {
            warnings.push(format!(
                "the join between `{added_name}` and `{other_name}` is guessed from naming; verify it"
            ));
        }
        let _ = write!(
            sql,
            "\nleft join {} as {added_alias}\n    on {}",
            relation(added),
            conditions.join("\n   and ")
        );
        joins.push(json!({
            "entity": added_name,
            "alias": added_alias,
            "relation": relation(added),
            "with": other_name,
            "with_alias": other_alias,
            "on": conditions,
            "cardinality": cardinality_word(rel.cardinality),
            "basis": basis_word(rel.basis),
            "evidence": rel.evidence,
            "repeats_rows": fans_out,
        }));
    }
    let base_entity = entity(&base);
    if base_entity.primary_key.is_none() {
        warnings.push(format!(
            "`{}` has no known key: don't assume one row per anything",
            base_entity.name
        ));
    }
    ToolOutput::Json(json!({
        "base": base_entity.name,
        "grain": grain(base_entity),
        "tables": in_query.iter().map(|id| json!({
            "entity": entity(id).name,
            "alias": alias_of[id.as_str()],
            "relation": relation(id),
        })).collect::<Vec<_>>(),
        "joins": joins,
        "warnings": warnings,
        "sql": sql,
        "next": "Add filters, grouping and the question's measures to this SQL, using only the columns \
                 `ods_describe_entity` lists. Keep the join conditions as given.",
    }))
}
