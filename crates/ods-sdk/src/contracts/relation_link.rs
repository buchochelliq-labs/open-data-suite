//! `RelationLinker`: turns a relation's name into a link to where the warehouse's own
//! UI shows it (#329, ADR-0006 amendment).
//!
//! # Semantics
//! - [`link`](RelationLinker::link) is pure: it builds a URL from the provider's
//!   configuration and the relation's name, with no I/O. That is why, unlike most
//!   contracts, it is synchronous (ADR-0002: async only at I/O boundaries).
//! - A link is the relation's **expected** location, from the project's artifacts. It
//!   is never proof that the relation exists: nothing is checked (AGENTS rule 3).
//! - When the provider can't build a link it says why ([`NoRelationLink`]); it never
//!   guesses one. A relation named with fewer parts than the warehouse's UI needs is
//!   [`NoRelationLink::NotQualified`], a missing setting
//!   [`NoRelationLink::NotConfigured`].
//! - A link is `https://` only and carries no user part, query string or fragment, so
//!   no credential can travel in it (AGENTS rule 9). Each name is percent-encoded.
//! - The same relation always gives the same link.
//! - The provider supplies the link's label for people (e.g. "Open in" and the name of its UI), so
//!   hosts never write a warehouse's name.
//! - Providers advertise [`Capability::RelationLink`](ods_core::Capability::RelationLink).

use std::fmt;

use ods_core::SchemaVersion;
use serde::{Deserialize, Serialize};

use crate::provider::{Contract, Provider};

/// The `relation_linker` contract.
pub const RELATION_LINKER: Contract = Contract {
    name: "relation_linker",
    version: SchemaVersion::new(0, 1),
};

/// A link to where a relation is expected to be, in the warehouse's own UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RelationLink {
    /// The URL: `https://`, with no user part, query string or fragment.
    pub url: String,
    /// What to call it, for people, e.g. "Open in Catalog Explorer".
    pub label: String,
}

impl RelationLink {
    /// A link, for providers to return.
    pub fn new(url: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            label: label.into(),
        }
    }
}

/// Why a relation has no link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
#[non_exhaustive]
pub enum NoRelationLink {
    /// No provider with the `relation_link` capability serves the target's warehouse.
    Unsupported {
        /// The warehouse, as the project names it, if it does.
        warehouse: Option<String>,
    },
    /// A setting the link is built from isn't configured.
    NotConfigured {
        /// The setting, e.g. `host`.
        setting: String,
    },
    /// A setting the link is built from is configured but can't be used.
    InvalidSetting {
        /// The setting, e.g. `host`.
        setting: String,
        /// Why, without the value (it may be anything).
        why: String,
    },
    /// The relation's name doesn't have every part the warehouse's UI needs.
    NotQualified {
        /// The relation's name, as given.
        relation: String,
        /// How many parts it has.
        parts: usize,
        /// How many the link needs.
        needed: usize,
    },
    /// The relation's name can't be read, or a name in it can't be one segment of a
    /// link's path (empty, `.` or `..`).
    InvalidName {
        /// The relation's name, as given.
        relation: String,
        /// What is wrong with it.
        why: String,
    },
    /// The provider serving the target's warehouse doesn't advertise the
    /// `relation_link` capability.
    NotOffered {
        /// The provider's kind.
        provider: String,
    },
    /// The node builds no relation (e.g. it is inlined into its readers).
    NoRelation,
}

impl fmt::Display for NoRelationLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported {
                warehouse: Some(warehouse),
            } => write!(f, "no warehouse link for `{warehouse}` targets"),
            Self::Unsupported { warehouse: None } => {
                f.write_str("no warehouse link: the project doesn't name its warehouse")
            }
            Self::NotConfigured { setting } => {
                write!(f, "no warehouse link: `{setting}` isn't configured")
            }
            Self::InvalidSetting { setting, why } => {
                write!(f, "no warehouse link: `{setting}` {why}")
            }
            Self::NotQualified {
                relation,
                parts,
                needed,
            } => write!(
                f,
                "no warehouse link: {relation} has {parts} part{s}, and a link needs {needed}",
                s = if *parts == 1 { "" } else { "s" }
            ),
            Self::InvalidName { relation, why } => {
                write!(f, "no warehouse link: {relation} {why}")
            }
            Self::NotOffered { provider } => write!(
                f,
                "no warehouse link: the `{provider}` provider doesn't offer links"
            ),
            Self::NoRelation => f.write_str("no warehouse link: it builds no relation"),
        }
    }
}

/// What a host knows about a relation's link, as fields to flatten into its JSON: the
/// link, or why there is none. All three are left out when nothing was asked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RelationLinkFields {
    /// Where the relation is expected to be, in the warehouse's UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation_url: Option<String>,
    /// The link's label, from the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation_url_label: Option<String>,
    /// Why there is no link, for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation_url_unavailable: Option<String>,
}

impl RelationLinkFields {
    /// The link, if there is one.
    pub fn link(&self) -> Option<RelationLink> {
        Some(RelationLink::new(
            self.relation_url.clone()?,
            self.relation_url_label.clone().unwrap_or_default(),
        ))
    }
}

impl From<Result<RelationLink, NoRelationLink>> for RelationLinkFields {
    fn from(result: Result<RelationLink, NoRelationLink>) -> Self {
        match result {
            Ok(link) => Self {
                relation_url: Some(link.url),
                relation_url_label: Some(link.label),
                relation_url_unavailable: None,
            },
            Err(why) => Self {
                relation_url: None,
                relation_url_label: None,
                relation_url_unavailable: Some(why.to_string()),
            },
        }
    }
}

/// Kept as they are in a path segment (RFC 3986's unreserved characters); everything
/// else is percent-encoded, so a name is always exactly one segment.
const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Splits a relation's name into its names, for providers: `quote` quotes a name (and
/// is doubled inside one), and whitespace is allowed only around the `.` separators.
/// Nothing is repaired: a name with whitespace inside it unquoted, text straight after
/// a closing quote, an unclosed quote, a stray quote or an empty part is refused, with
/// why.
///
/// # Errors
/// What is wrong with the name, as the end of a sentence ("isn't …").
pub fn split_relation(relation: &str, quote: char) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = relation.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut name = String::new();
        if chars.next_if_eq(&quote).is_some() {
            loop {
                match chars.next() {
                    Some(c) if c == quote => {
                        if chars.next_if_eq(&quote).is_some() {
                            name.push(quote);
                        } else {
                            break;
                        }
                    }
                    Some(c) => name.push(c),
                    None => return Err("has a quote that isn't closed".to_owned()),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| *c != '.' && !c.is_whitespace()) {
                if c == quote {
                    return Err("has a quote inside an unquoted name".to_owned());
                }
                name.push(c);
            }
        }
        if name.is_empty() {
            return Err("has an empty name".to_owned());
        }
        out.push(name);
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        match chars.next() {
            None => return Ok(out),
            Some('.') => {}
            Some(_) => {
                return Err(
                    "has text that isn't separated by `.` (whitespace or text after a quoted name)"
                        .to_owned(),
                );
            }
        }
    }
}

/// `name` as one percent-encoded segment of a URL's path, for providers. `.` and `..`
/// are refused: browsers resolve them as dot segments even when encoded (`%2E`), so they
/// would leave the path.
///
/// # Errors
/// Why `name` can't be a segment, as the end of a sentence ("has …").
pub fn path_segment(name: &str) -> Result<String, String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!(
            "has a name, `{name}`, that can't be one segment of a link"
        ));
    }
    Ok(percent_encoding::utf8_percent_encode(name, SEGMENT).to_string())
}

/// Turns relations' names into links to the warehouse's UI.
pub trait RelationLinker: Provider {
    /// The link to `relation`, named as the project's artifacts render it (quoted as
    /// the warehouse quotes identifiers), or why there is none.
    ///
    /// # Errors
    /// [`NoRelationLink`] when no link can be built; never a guessed link.
    fn link(&self, relation: &str) -> Result<RelationLink, NoRelationLink>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_carry_the_link_or_the_reason() {
        let linked = RelationLinkFields::from(Ok(RelationLink::new("https://h/x", "Open")));
        assert_eq!(
            serde_json::to_value(&linked).unwrap(),
            serde_json::json!({"relation_url": "https://h/x", "relation_url_label": "Open"})
        );
        assert_eq!(
            linked.link(),
            Some(RelationLink::new("https://h/x", "Open"))
        );
        let none = RelationLinkFields::from(Err(NoRelationLink::NotQualified {
            relation: "s.t".into(),
            parts: 2,
            needed: 3,
        }));
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            serde_json::json!({"relation_url_unavailable": "no warehouse link: s.t has 2 parts, and a link needs 3"})
        );
        assert_eq!(none.link(), None);
        assert_eq!(
            serde_json::to_value(RelationLinkFields::default()).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn splits_only_well_formed_names() {
        let split = |r: &str| split_relation(r, '`');
        assert_eq!(split("a.b.c").unwrap(), ["a", "b", "c"]);
        assert_eq!(split(" `a` . b .`c`` d` ").unwrap(), ["a", "b", "c` d"]);
        assert_eq!(split("`s.x`.t").unwrap(), ["s.x", "t"]);
        for bad in [
            "",
            "   ",
            "a..c",
            "a.b.",
            ".a.b",
            "my table.s.t",
            "`a`b.c.d",
            "`a` b.c",
            "a`b`.c",
            "`a.b",
            "``.b.c",
        ] {
            assert!(split(bad).is_err(), "{bad}: {:?}", split(bad));
        }
    }

    #[test]
    fn path_segments_never_leave_the_path() {
        assert_eq!(path_segment("a/b %").unwrap(), "a%2Fb%20%25");
        assert_eq!(path_segment("a.b").unwrap(), "a.b");
        for bad in ["", ".", ".."] {
            assert!(path_segment(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reasons_read_as_sentences() {
        for (why, text) in [
            (
                NoRelationLink::Unsupported {
                    warehouse: Some("w".into()),
                },
                "no warehouse link for `w` targets",
            ),
            (
                NoRelationLink::NotConfigured {
                    setting: "host".into(),
                },
                "no warehouse link: `host` isn't configured",
            ),
            (
                NoRelationLink::InvalidSetting {
                    setting: "host".into(),
                    why: "must be https".into(),
                },
                "no warehouse link: `host` must be https",
            ),
            (
                NoRelationLink::NoRelation,
                "no warehouse link: it builds no relation",
            ),
            (
                NoRelationLink::NotOffered {
                    provider: "p".into(),
                },
                "no warehouse link: the `p` provider doesn't offer links",
            ),
            (
                NoRelationLink::InvalidName {
                    relation: "a..b".into(),
                    why: "has an empty name".into(),
                },
                "no warehouse link: a..b has an empty name",
            ),
        ] {
            assert_eq!(why.to_string(), text);
        }
    }
}
