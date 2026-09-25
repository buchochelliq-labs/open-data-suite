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
    /// Columns asserted never null, with the evidence.
    not_null: BTreeMap<String, Vec<String>>,
}

impl EntityFacts {
    /// The primary key, unless it is only a guess.
    fn trusted_primary(&self) -> Option<&Key> {
        self.primary.as_ref().filter(|k| k.basis != Basis::Inferred)
    }

    /// Whether `columns` are a declared or tested key (primary or unique).
    fn has_trusted_key(&self, columns: &[String]) -> bool {
        self.trusted_primary()
            .is_some_and(|k| same_columns(&k.columns, columns))
            || self
                .unique
                .iter()
                .any(|k| k.basis != Basis::Inferred && same_columns(&k.columns, columns))
    }

    /// Whether the column is declared or tested never null.
    fn trusted_not_null(&self, column: &str) -> bool {
        self.not_null.contains_key(column)
            || self
                .trusted_primary()
                .is_some_and(|k| k.columns.iter().any(|c| c == column))
    }
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

    let mut joined = Vec::new();
    apply_facts(
        &index,
        facts,
        &mut per_entity,
        &mut foreign,
        &mut joined,
        &mut diagnostics,
    );
    promote_primary_keys(&mut per_entity, &mut diagnostics);
    if options.infer {
        infer_primary_keys(entities, &mut per_entity);
    }
    // Joins take their direction from keys, so they come after every key is known.
    resolve_joins(joined, &per_entity, &mut foreign);
    if options.infer {
        infer_relationships(entities, &per_entity, &mut foreign, &mut diagnostics);
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
    joined: &mut Vec<Join>,
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
                    if let Some(column) = columns.pop() {
                        per_entity
                            .entry(entity.clone())
                            .or_default()
                            .not_null
                            .entry(column)
                            .or_default()
                            .push(evidence.clone());
                    }
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
                        keys.sort_by_key(|k| std::cmp::Reverse(k.basis));
                        diagnostics.push(format!(
                            "{entity}: conflicting primary keys {:?} and {:?}",
                            keys[0].columns, keys[1].columns
                        ));
                    }
                    facts.primary = keys.into_iter().next();
                }
                Err(why) => diagnostics.push(format!("{evidence}: {why}")),
            },
            Fact::Joined { .. } | Fact::ForeignKey { .. } => {
                apply_relationship(index, fact, foreign, joined, diagnostics);
            }
        }
    }
}

/// Records a join or foreign key, or a diagnostic if it names something unknown.
fn apply_relationship(
    index: &Index<'_>,
    fact: &Fact,
    foreign: &mut Vec<Relationship>,
    joined: &mut Vec<Join>,
    diagnostics: &mut Vec<String>,
) {
    match fact {
        Fact::Joined {
            left,
            left_columns,
            right,
            right_columns,
            evidence,
        } => match (
            index.resolve(left, left_columns),
            index.resolve(right, right_columns),
        ) {
            (Ok(left_columns), Ok(right_columns))
                if left_columns.len() == right_columns.len() && left != right =>
            {
                joined.push(Join {
                    left: left.clone(),
                    left_columns,
                    right: right.clone(),
                    right_columns,
                    evidence: evidence.clone(),
                });
            }
            (Ok(_), Ok(_)) => {}
            (Err(why), _) | (_, Err(why)) => diagnostics.push(format!("{evidence}: {why}")),
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
                (Ok(from_columns), Ok(to_columns)) if from_columns.len() == to_columns.len() => {
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
        _ => {}
    }
}

/// A join the SQL makes, with resolved column names.
struct Join {
    left: String,
    left_columns: Vec<String>,
    right: String,
    right_columns: Vec<String>,
    evidence: String,
}

fn same_columns(a: &[String], b: &[String]) -> bool {
    let mut a: Vec<String> = a.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut b: Vec<String> = b.iter().map(|c| c.to_ascii_lowercase()).collect();
    a.sort();
    b.sort();
    a == b
}

/// Whether `columns` are a declared or tested key of the entity. Guessed keys never
/// decide direction or cardinality (AGENTS.md rule 3).
fn is_key(per_entity: &BTreeMap<String, EntityFacts>, entity: &str, columns: &[String]) -> bool {
    per_entity
        .get(entity)
        .is_some_and(|f| f.has_trusted_key(columns))
}

/// Without a declared primary key, a unique key becomes it:
/// - first one whose columns are all not null;
/// - else the smallest unique *combination* of columns. Composite keys are usually
///   tested only for uniqueness, and a single-column `unique` test can't express them.
///
/// A single column tested only `unique` stays a nullable unique key.
fn promote_primary_keys(
    per_entity: &mut BTreeMap<String, EntityFacts>,
    diagnostics: &mut Vec<String>,
) {
    for (entity, facts) in per_entity.iter_mut() {
        if facts.primary.is_some() {
            continue;
        }
        let qualifying: Vec<usize> = facts
            .unique
            .iter()
            .enumerate()
            .filter(|(_, k)| k.columns.iter().all(|c| facts.not_null.contains_key(c)))
            .map(|(i, _)| i)
            .collect();
        if qualifying.len() > 1 {
            diagnostics.push(format!(
                "{entity}: several unique, not-null keys; the first ({:?}) is used as the primary key",
                facts.unique[qualifying[0]].columns
            ));
        }
        let not_null = qualifying.first().copied();
        let composite = || {
            facts
                .unique
                .iter()
                .enumerate()
                .filter(|(_, k)| k.columns.len() > 1)
                .min_by_key(|(_, k)| k.columns.len())
                .map(|(i, _)| i)
        };
        if let Some(position) = not_null {
            let mut key = facts.unique.remove(position);
            for column in &key.columns {
                key.evidence
                    .extend(facts.not_null.get(column).into_iter().flatten().cloned());
            }
            key.evidence.sort();
            key.evidence.dedup();
            facts.primary = Some(key);
        } else if let Some(position) = composite() {
            let mut key = facts.unique.remove(position);
            key.evidence
                .push("unique combination; nullability not tested".to_owned());
            facts.primary = Some(key);
        }
    }
}

/// Joins become relationships: the side whose columns are a key is referenced (the
/// "one" side). With keys on neither side, the relationship is kept with
/// [`Cardinality::Unknown`], so a query writer knows the join may fan out.
fn resolve_joins(
    joins: Vec<Join>,
    per_entity: &BTreeMap<String, EntityFacts>,
    foreign: &mut Vec<Relationship>,
) {
    for join in joins {
        let right_key = is_key(per_entity, &join.right, &join.right_columns);
        let left_key = is_key(per_entity, &join.left, &join.left_columns);
        let (from, from_columns, to, to_columns, cardinality) = match (left_key, right_key) {
            (_, true) => (
                join.left,
                join.left_columns,
                join.right,
                join.right_columns,
                Cardinality::ManyToOne,
            ),
            (true, false) => (
                join.right,
                join.right_columns,
                join.left,
                join.left_columns,
                Cardinality::ManyToOne,
            ),
            (false, false) => (
                join.left,
                join.left_columns,
                join.right,
                join.right_columns,
                Cardinality::Unknown,
            ),
        };
        foreign.push(Relationship {
            from,
            from_columns,
            to,
            to_columns,
            cardinality,
            optional: true,
            basis: Basis::Joined,
            evidence: vec![format!("joined in {}", join.evidence)],
        });
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
            // Guessed relationships don't mark columns as foreign keys.
            let fk_columns: BTreeSet<&str> = relationships
                .iter()
                .filter(|r| r.from == input.id && r.basis != Basis::Inferred)
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
                description: input.description.clone(),
                relation: input.relation.clone(),
                columns: input
                    .columns
                    .iter()
                    .map(|c| Column {
                        name: c.name.clone(),
                        data_type: c.data_type.clone(),
                        description: c.description.clone(),
                        primary_key: pk.contains(c.name.as_str()),
                        foreign_key: fk_columns.contains(c.name.as_str()),
                        not_null: facts.trusted_not_null(&c.name),
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
                // A test or constraint knows more about cardinality than a join does.
                if existing.cardinality == Cardinality::Unknown {
                    existing.cardinality = rel.cardinality;
                }
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
            let from = per_entity.get(&rel.from);
            let to_is_key = per_entity
                .get(&rel.to)
                .is_some_and(|f| f.has_trusted_key(&rel.to_columns));
            // Only declared or tested keys decide cardinality and optionality; a
            // reference to columns not known to be a key may fan out.
            if rel.basis != Basis::Inferred && !to_is_key {
                rel.cardinality = Cardinality::Unknown;
            } else if rel.cardinality != Cardinality::Unknown
                && from.is_some_and(|f| f.has_trusted_key(&rel.from_columns))
            {
                rel.cardinality = Cardinality::OneToOne;
            }
            rel.optional =
                !from.is_some_and(|f| rel.from_columns.iter().all(|c| f.trusted_not_null(c)));
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
///
/// This one does the first: an entity without a known primary key whose column is `id`
/// or `<entity>_id` gets it.
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
