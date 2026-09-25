//! Rendering an [`Erd`] as Mermaid, Graphviz DOT or JSON.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::{Basis, Cardinality, Entity, Erd};

/// Mermaid and DOT need plain identifiers; `[A-Za-z0-9_]` only.
fn ident(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Display labels: names where they are unique, ids where two entities share a name
/// (e.g. a source and a model both called `orders`).
fn labels(erd: &Erd) -> BTreeMap<&str, String> {
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &erd.entities {
        *count.entry(e.name.as_str()).or_default() += 1;
    }
    // Sanitizing can still collide (`raw.orders` and `raw_orders`): number repeats.
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    erd.entities
        .iter()
        .map(|e| {
            let label = if count[e.name.as_str()] > 1 {
                &e.id
            } else {
                &e.name
            };
            let base = ident(label);
            let seen = used.entry(base.clone()).or_default();
            *seen += 1;
            let label = if *seen == 1 {
                base
            } else {
                format!("{base}_{seen}")
            };
            (e.id.as_str(), label)
        })
        .collect()
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "'")
}

fn basis_word(basis: Basis) -> &'static str {
    match basis {
        Basis::Declared => "declared",
        Basis::Tested => "tested",
        Basis::Joined => "joined",
        Basis::Inferred => "inferred",
    }
}

impl Erd {
    /// A Mermaid `erDiagram`. Inferred relationships are dotted (non-identifying) and
    /// labelled as inferred.
    pub fn to_mermaid(&self) -> String {
        let labels = labels(self);
        let mut out = String::from("erDiagram\n");
        for entity in &self.entities {
            let _ = writeln!(out, "    {} {{", labels[entity.id.as_str()]);
            for column in &entity.columns {
                let data_type = column
                    .data_type
                    .as_deref()
                    .map_or_else(|| "unknown".to_owned(), ident);
                let mut keys = Vec::new();
                if column.primary_key {
                    keys.push("PK");
                }
                if column.foreign_key {
                    keys.push("FK");
                }
                let _ = write!(out, "        {data_type} {}", ident(&column.name));
                if !keys.is_empty() {
                    let _ = write!(out, " {}", keys.join(", "));
                }
                if let Some(pk) = entity.primary_key.as_ref().filter(|_| column.primary_key)
                    && pk.basis != Basis::Declared
                {
                    let _ = write!(out, " \"{} key\"", basis_word(pk.basis));
                }
                out.push('\n');
            }
            out.push_str("    }\n");
        }
        for rel in &self.relationships {
            // Mermaid reads `A <left>--<right> B`: left is A's multiplicity, right B's.
            let one = if rel.optional { "o|" } else { "||" };
            let (left, right) = match rel.cardinality {
                Cardinality::OneToOne => ("|o", one),
                Cardinality::ManyToOne => ("}o", one),
                // Neither side is a known key: many on both sides.
                _ => ("}o", "o{"),
            };
            let line = if rel.basis == Basis::Inferred {
                ".."
            } else {
                "--"
            };
            let mut label = rel.from_columns.join(", ");
            if matches!(rel.basis, Basis::Inferred | Basis::Joined) {
                let _ = write!(label, " ({})", basis_word(rel.basis));
            }
            let _ = writeln!(
                out,
                "    {} {left}{line}{right} {} : \"{}\"",
                labels[rel.from.as_str()],
                labels[rel.to.as_str()],
                escape(&label)
            );
        }
        out
    }

    /// Graphviz DOT with one record per entity. Inferred relationships are dashed and
    /// inferred keys marked `PK?`.
    pub fn to_dot(&self) -> String {
        let labels = labels(self);
        let mut out = String::from(
            "digraph erd {\n  graph [rankdir=LR];\n  node [shape=plain, fontname=\"Helvetica\"];\n  edge [fontname=\"Helvetica\", fontsize=10, arrowhead=crow, arrowtail=tee, dir=both];\n",
        );
        for entity in &self.entities {
            let _ = writeln!(
                out,
                "  {} [label=<{}>];",
                labels[entity.id.as_str()],
                entity_table(entity)
            );
        }
        for rel in &self.relationships {
            let style = if rel.basis == Basis::Inferred {
                ", style=dashed"
            } else {
                ""
            };
            // The crow's foot is on the "many" end: the referencing entity.
            let _ = writeln!(
                out,
                "  {} -> {} [label=\"{}\", arrowhead=tee, arrowtail={}{style}];",
                labels[rel.from.as_str()],
                labels[rel.to.as_str()],
                escape(&format!(
                    "{} → {} ({}{})",
                    rel.from_columns.join(", "),
                    rel.to_columns.join(", "),
                    basis_word(rel.basis),
                    if rel.cardinality == Cardinality::Unknown {
                        ", cardinality unknown"
                    } else {
                        ""
                    }
                )),
                if rel.cardinality == Cardinality::OneToOne {
                    "tee"
                } else {
                    "crow"
                }
            );
        }
        out.push_str("}\n");
        out
    }

    /// Pretty JSON of the whole model (see [`crate::SCHEMA_VERSION`]).
    ///
    /// # Errors
    /// Never in practice; serialization of these types can't fail.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn entity_table(entity: &Entity) -> String {
    let mut out = format!(
        "<table border=\"0\" cellborder=\"1\" cellspacing=\"0\"><tr><td bgcolor=\"#e8eef7\"><b>{}</b></td></tr>",
        html(&entity.name)
    );
    for column in &entity.columns {
        let name = if column.primary_key {
            format!("<u>{}</u>", html(&column.name))
        } else {
            html(&column.name)
        };
        let mut detail: Vec<String> = column.data_type.iter().map(|t| html(t)).collect();
        if column.primary_key {
            let inferred = entity
                .primary_key
                .as_ref()
                .is_some_and(|k| k.basis == Basis::Inferred);
            detail.push(if inferred { "PK?" } else { "PK" }.into());
        }
        if column.foreign_key {
            detail.push("FK".into());
        }
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!(" <font color=\"#666666\">{}</font>", detail.join(" "))
        };
        let _ = write!(out, "<tr><td align=\"left\">{name}{detail}</td></tr>");
    }
    out.push_str("</table>");
    out
}
