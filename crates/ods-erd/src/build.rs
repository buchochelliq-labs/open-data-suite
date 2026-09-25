//! Turning entities and facts into an [`Erd`].

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Basis, Cardinality, Column, Entity, EntityInput, Erd, Fact, Key, Relationship, SCHEMA_VERSION,
};

/// How to build.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BuildOptions {
    /// Also propose keys and relationships from naming conventions (`id`,
    /// `<entity>_id`). Off by default: inferred facts are always labelled
    /// [`Basis::Inferred`], but they are guesses.
    pub infer: bool,
}

impl BuildOptions {
    /// With or without inference.
    #[must_use]
    pub fn with_inference(mut self, infer: bool) -> Self {
        self.infer = infer;
        self
    }
}

/// Per entity: its input, and column lookup by lower-cased name.
struct Index<'a> {
    entities: BTreeMap<&'a str, &'a EntityInput>,
}

impl<'a> Index<'a> {
    fn new(entities: &'a [EntityInput]) -> Self {
        Self {
            entities: entities.iter().map(|e| (e.id.as_str(), e)).collect(),
        }
    }

    /// `columns` in the entity's spelling (matched case-insensitively), or the first
    /// one that doesn't exist. Entities with unknown columns accept any name.
    fn resolve(&self, entity: &str, columns: &[String]) -> Result<Vec<String>, String> {
        let Some(input) = self.entities.get(entity) else {
            return Err(format!("no entity `{entity}`"));
        };
        if input.columns.is_empty() {
            return Ok(columns.to_vec());
        }
        columns
            .iter()
            .map(|c| {
                input
                    .columns
                    .iter()
                    .find(|k| k.name.eq_ignore_ascii_case(c))
                    .map(|k| k.name.clone())
                    .ok_or_else(|| format!("`{}` has no column `{c}`", input.name))
            })
            .collect()
    }
}

#[derive(Default)]
struct EntityFacts {
    primary: Option<Key>,
    unique: Vec<Key>,
    not_null: BTreeSet<String>,
}

/// Merges evidence for the same key, keeping the strongest basis.
fn add_key(keys: &mut Vec<Key>, columns: Vec<String>, basis: Basis, evidence: String) {
    let mut sorted = columns.clone();
    sorted.sort();
    if let Some(key) = keys.iter_mut().find(|k| {
        let mut other = k.columns.clone();
        other.sort();
        other == sorted
    }) {
        key.basis = key.basis.max(basis);
        if !key.evidence.contains(&evidence) {
            key.evidence.push(evidence);
            key.evidence.sort();
        }
    } else {
        keys.push(Key {
            columns,
            basis,
            evidence: vec![evidence],
        });
    }
}

/// Builds the model. Facts about unknown entities or columns are reported in
/// [`Erd::diagnostics`], never silently dropped.
pub fn build(entities: &[EntityInput], facts: &[Fact], options: BuildOptions) -> Erd {
    let index = Index::new(entities);
    let mut diagnostics = Vec::new();
    let mut per_entity: BTreeMap<String, EntityFacts> = BTreeMap::new();
    let mut foreign: Vec<Relationship> = Vec::new();

    apply_facts(
        &index,
        facts,
        &mut per_entity,
        &mut foreign,
        &mut diagnostics,
    );

    // A unique key whose columns are all not-null is a primary key, if none is declared.
    for facts in per_entity.values_mut() {
        if facts.primary.is_none()
            && let Some(position) = facts
                .unique
                .iter()
                .position(|k| k.columns.iter().all(|c| facts.not_null.contains(c)))
        {
            let mut key = facts.unique.remove(position);
            key.evidence
                .extend(key.columns.iter().map(|c| format!("not null: {c}")));
            facts.primary = Some(key);
        }
    }

    if options.infer {
        infer(entities, &mut per_entity, &mut foreign, &mut diagnostics);
    }

    let relationships = finish_relationships(foreign, &per_entity);
    let entities = finish_entities(entities, &mut per_entity, &relationships);
    diagnostics.sort();
    diagnostics.dedup();
    Erd {
        schema_version: SCHEMA_VERSION,
        entities,
        relationships,
        diagnostics,
    }
}

/// Records each fact against its entity, or a diagnostic if it names something unknown.
fn apply_facts(
    index: &Index<'_>,
    facts: &[Fact],
    per_entity: &mut BTreeMap<String, EntityFacts>,
    foreign: &mut Vec<Relationship>,
    diagnostics: &mut Vec<String>,
) {
    for fact in facts {
        match fact {
            Fact::Unique {
                entity,
                columns,
                basis,
                evidence,
            } => match index.resolve(entity, columns) {
                Ok(columns) => {
                    add_key(
                        &mut per_entity.entry(entity.clone()).or_default().unique,
                        columns,
                        *basis,
                        evidence.clone(),
                    );
                }
                Err(why) => diagnostics.push(format!("{evidence}: {why}")),
            },
            Fact::NotNull {
                entity,
                column,
                evidence,
                ..
            } => match index.resolve(entity, std::slice::from_ref(column)) {
                Ok(mut columns) => {
                    per_entity
                        .entry(entity.clone())
                        .or_default()
                        .not_null
                        .extend(columns.pop());
                }
                Err(why) => diagnostics.push(format!("{evidence}: {why}")),
            },
            Fact::PrimaryKey {
                entity,
                columns,
                basis,
                evidence,
            } => match index.resolve(entity, columns) {
                Ok(columns) => {
                    let facts = per_entity.entry(entity.clone()).or_default();
                    let mut keys: Vec<Key> = facts.primary.take().into_iter().collect();
                    add_key(&mut keys, columns, *basis, evidence.clone());
                    // Two different primary keys: keep the stronger one, report both.
                    if keys.len() > 1 {
                        keys.sort_by(|a, b| b.basis.cmp(&a.basis));
                        diagnostics.push(format!(
                            "{entity}: conflicting primary keys {:?} and {:?}",
                            keys[0].columns, keys[1].columns
                        ));
                    }
                    facts.primary = keys.into_iter().next();
                }
                Err(why) => diagnostics.push(format!("{evidence}: {why}")),
            },
            Fact::ForeignKey {
                entity,
                columns,
                to,
                to_columns,
                basis,
                evidence,
            } => {
                let from = index.resolve(entity, columns);
                let target = index.resolve(to, to_columns);
                match (from, target) {
                    (Ok(from_columns), Ok(to_columns))
                        if from_columns.len() == to_columns.len() =>
                    {
                        foreign.push(Relationship {
                            from: entity.clone(),
                            from_columns,
                            to: to.clone(),
                            to_columns,
                            cardinality: Cardinality::ManyToOne,
                            optional: true,
                            basis: *basis,
                            evidence: vec![evidence.clone()],
                        });
                    }
                    (Ok(_), Ok(_)) => {
                        diagnostics.push(format!("{evidence}: column counts differ"));
                    }
                    (Err(why), _) | (_, Err(why)) => {
                        diagnostics.push(format!("{evidence}: {why}"));
                    }
                }
            }
        }
    }
}

/// The output entities, with their keys and column flags, sorted by id.
fn finish_entities(
    entities: &[EntityInput],
    per_entity: &mut BTreeMap<String, EntityFacts>,
    relationships: &[Relationship],
) -> Vec<Entity> {
    let mut out: Vec<Entity> = entities
        .iter()
        .map(|input| {
            let facts = per_entity.remove(&input.id).unwrap_or_default();
            let fk_columns: BTreeSet<&str> = relationships
                .iter()
                .filter(|r| r.from == input.id)
                .flat_map(|r| r.from_columns.iter().map(String::as_str))
                .collect();
            let pk: BTreeSet<&str> = facts
                .primary
                .iter()
                .flat_map(|k| k.columns.iter().map(String::as_str))
                .collect();
            Entity {
                id: input.id.clone(),
                name: input.name.clone(),
                kind: input.kind,
                columns: input
                    .columns
                    .iter()
                    .map(|c| Column {
                        name: c.name.clone(),
                        data_type: c.data_type.clone(),
                        primary_key: pk.contains(c.name.as_str()),
                        foreign_key: fk_columns.contains(c.name.as_str()),
                        not_null: facts.not_null.contains(&c.name) || pk.contains(c.name.as_str()),
                    })
                    .collect(),
                primary_key: facts.primary,
                unique_keys: facts.unique,
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Deduplicates relationships (keeping the strongest basis and all evidence) and
/// works out cardinality and optionality from the referencing side's keys.
fn finish_relationships(
    foreign: Vec<Relationship>,
    per_entity: &BTreeMap<String, EntityFacts>,
) -> Vec<Relationship> {
    let mut merged: BTreeMap<(String, Vec<String>, String, Vec<String>), Relationship> =
        BTreeMap::new();
    for rel in foreign {
        let key = (
            rel.from.clone(),
            rel.from_columns.clone(),
            rel.to.clone(),
            rel.to_columns.clone(),
        );
        match merged.get_mut(&key) {
            Some(existing) => {
                existing.basis = existing.basis.max(rel.basis);
                existing.evidence.extend(rel.evidence);
                existing.evidence.sort();
                existing.evidence.dedup();
            }
            None => {
                merged.insert(key, rel);
            }
        }
    }
    merged
        .into_values()
        .map(|mut rel| {
            let facts = per_entity.get(&rel.from);
            let same = |k: &Key| {
                let mut a = k.columns.clone();
                a.sort();
                let mut b = rel.from_columns.clone();
                b.sort();
                a == b
            };
            let unique = facts
                .is_some_and(|f| f.primary.as_ref().is_some_and(same) || f.unique.iter().any(same));
            if unique {
                rel.cardinality = Cardinality::OneToOne;
            }
            rel.optional = !facts.is_some_and(|f| {
                rel.from_columns.iter().all(|c| {
                    f.not_null.contains(c)
                        || f.primary.as_ref().is_some_and(|k| k.columns.contains(c))
                })
            });
            rel
        })
        .collect()
}

/// Name forms an entity may be referred to by: `stg_orders` → `stg_orders`, `orders`,
/// `order`.
fn name_forms(name: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    let mut forms = vec![lower.clone()];
    let mut base = lower.as_str();
    for prefix in ["stg_", "int_", "dim_", "fct_", "fact_", "raw_", "base_"] {
        if let Some(rest) = base.strip_prefix(prefix) {
            base = rest;
            forms.push(base.to_owned());
            break;
        }
    }
    if let Some(singular) = base.strip_suffix("ies") {
        forms.push(format!("{singular}y"));
    } else if let Some(singular) = base.strip_suffix('s') {
        forms.push(singular.to_owned());
    }
    forms.dedup();
    forms
}

/// Naming-convention guesses, labelled [`Basis::Inferred`]:
/// - an entity without a known primary key whose column is `id` or `<entity>_id` gets
///   that as primary key;
/// - a column `<x>_id` refers to the one entity whose primary key is that column, or
///   whose name form is `<x>` with primary key `id`.
fn infer(
    entities: &[EntityInput],
    per_entity: &mut BTreeMap<String, EntityFacts>,
    foreign: &mut Vec<Relationship>,
    diagnostics: &mut Vec<String>,
) {
    infer_primary_keys(entities, per_entity);
    infer_relationships(entities, per_entity, foreign, diagnostics);
}

/// An entity without a known primary key whose column is `id` or `<entity>_id` gets it.
fn infer_primary_keys(entities: &[EntityInput], per_entity: &mut BTreeMap<String, EntityFacts>) {
    for entity in entities {
        let facts = per_entity.entry(entity.id.clone()).or_default();
        if facts.primary.is_some() {
            continue;
        }
        let candidates: Vec<String> = std::iter::once("id".to_owned())
            .chain(
                name_forms(&entity.name)
                    .into_iter()
                    .map(|f| format!("{f}_id")),
            )
            .collect();
        if let Some(column) = entity
            .columns
            .iter()
            .find(|c| candidates.iter().any(|k| c.name.eq_ignore_ascii_case(k)))
        {
            facts.primary = Some(Key {
                columns: vec![column.name.clone()],
                basis: Basis::Inferred,
                evidence: vec![format!("naming: `{}` looks like the key", column.name)],
            });
        }
    }
}

/// A column `<x>_id` refers to the one entity whose primary key is that column, or whose
/// name form is `<x>` with primary key `id`.
fn infer_relationships(
    entities: &[EntityInput],
    per_entity: &BTreeMap<String, EntityFacts>,
    foreign: &mut Vec<Relationship>,
    diagnostics: &mut Vec<String>,
) {
    // Single-column primary keys, by lower-cased column name, and entities by name form.
    let mut by_key_column: BTreeMap<String, Vec<(Basis, &str, String)>> = BTreeMap::new();
    let mut by_form: BTreeMap<String, Vec<(Basis, &str, String)>> = BTreeMap::new();
    for entity in entities {
        let Some(key) = per_entity.get(&entity.id).and_then(|f| f.primary.as_ref()) else {
            continue;
        };
        let [column] = key.columns.as_slice() else {
            continue;
        };
        by_key_column
            .entry(column.to_ascii_lowercase())
            .or_default()
            .push((key.basis, entity.id.as_str(), column.clone()));
        if column.eq_ignore_ascii_case("id") {
            for form in name_forms(&entity.name) {
                by_form.entry(form).or_default().push((
                    key.basis,
                    entity.id.as_str(),
                    column.clone(),
                ));
            }
        }
    }

    let known: BTreeSet<(String, String)> = foreign
        .iter()
        .flat_map(|r| {
            r.from_columns
                .iter()
                .map(|c| (r.from.clone(), c.to_ascii_lowercase()))
        })
        .collect();
    for entity in entities {
        let own_key: BTreeSet<String> = per_entity
            .get(&entity.id)
            .and_then(|f| f.primary.as_ref())
            .map(|k| k.columns.iter().map(|c| c.to_ascii_lowercase()).collect())
            .unwrap_or_default();
        for column in &entity.columns {
            let lower = column.name.to_ascii_lowercase();
            if own_key.contains(&lower)
                || known.contains(&(entity.id.clone(), lower.clone()))
                || !lower.ends_with("_id")
            {
                continue;
            }
            let prefix = &lower[..lower.len() - 3];
            let candidates: Vec<(Basis, &str, String)> = by_key_column
                .get(&lower)
                .into_iter()
                .flatten()
                .chain(by_form.get(prefix).into_iter().flatten())
                .filter(|(_, id, _)| *id != entity.id)
                .cloned()
                .collect();
            // Prefer targets whose key is tested or declared over guessed ones.
            let best = candidates.iter().map(|(b, _, _)| *b).max();
            let mut targets: Vec<(&str, String)> = candidates
                .into_iter()
                .filter(|(b, _, _)| Some(*b) == best)
                .map(|(_, id, c)| (id, c))
                .collect();
            targets.sort();
            targets.dedup();
            match targets.as_slice() {
                [(to, to_column)] => foreign.push(Relationship {
                    from: entity.id.clone(),
                    from_columns: vec![column.name.clone()],
                    to: (*to).to_owned(),
                    to_columns: vec![to_column.clone()],
                    cardinality: Cardinality::ManyToOne,
                    optional: true,
                    basis: Basis::Inferred,
                    evidence: vec![format!(
                        "naming: `{}` matches the key of `{to}`",
                        column.name
                    )],
                }),
                [] => {}
                many => diagnostics.push(format!(
                    "{}.{}: several possible targets ({}); not inferred",
                    entity.name,
                    column.name,
                    many.iter()
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
    }
}
