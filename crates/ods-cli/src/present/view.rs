//! The ODS-owned view tree (ADR-0003 §1).
//!
//! Presenters turn result models into these nodes; backends render them. The vocabulary
//! is deliberately small and carries *semantic* styles only, so no backend-specific
//! concept (colours, markup, box styles) leaks into presenters.

/// Meaning-level style for a span of text. Backends decide how (or whether) to show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Tone {
    /// Draws attention to the text.
    Emphasis,
    /// De-emphasised, secondary information.
    Muted,
    /// Something new or that will be created/built.
    Added,
    /// Something removed or skipped.
    Removed,
    /// A condition the user should look at.
    Warning,
    /// A failure.
    Error,
    /// A successful outcome.
    Success,
    /// An identifier, path or code fragment.
    Code,
}

impl Tone {
    /// Every tone, for backends that must declare a style for each.
    pub const ALL: [Tone; 8] = [
        Tone::Emphasis,
        Tone::Muted,
        Tone::Added,
        Tone::Removed,
        Tone::Warning,
        Tone::Error,
        Tone::Success,
        Tone::Code,
    ];
}

/// A run of text with an optional tone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The literal text. Never interpreted as markup by any backend.
    pub text: String,
    /// How the text should be emphasised, if at all.
    pub tone: Option<Tone>,
}

impl Span {
    /// Unstyled text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: None,
        }
    }

    /// Text with a tone.
    pub fn toned(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone: Some(tone),
        }
    }
}

/// A line of text made of spans.
pub type Line = Vec<Span>;

/// Concatenates the text of `line`, dropping tones.
pub fn plain_text(line: &[Span]) -> String {
    line.iter().map(|span| span.text.as_str()).collect()
}

/// Replaces control characters that could drive the terminal.
///
/// Result text often carries data we do not control (node names, SQL, warehouse error
/// messages). ESC and other C0/C1 controls in it could emit arbitrary terminal
/// sequences, even in plain mode. Every backend passes displayed text through here.
/// Tabs, newlines and carriage returns are kept; backends decide how to lay them out.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && !matches!(c, '\n' | '\r' | '\t') {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// Severity of a [`ViewNode::Notice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Level {
    /// Informational.
    Info,
    /// Needs attention but did not fail.
    Warning,
    /// A failure.
    Error,
}

/// A node in a tree view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeItem {
    /// The node's label.
    pub label: Line,
    /// Child nodes, in display order.
    pub children: Vec<TreeItem>,
}

impl TreeItem {
    /// A tree item without children.
    pub fn leaf(label: Line) -> Self {
        Self {
            label,
            children: Vec::new(),
        }
    }
}

/// A renderable block.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ViewNode {
    /// A section title.
    Heading(String),
    /// A line of prose.
    Paragraph(Line),
    /// Aligned `key: value` pairs.
    KeyValue(Vec<(String, Line)>),
    /// Tabular data. Every row has one cell per column.
    Table {
        /// Optional caption shown above the table.
        title: Option<String>,
        /// Column headers.
        columns: Vec<String>,
        /// Rows of cells.
        rows: Vec<Vec<Line>>,
    },
    /// A hierarchy, e.g. a reason chain or dependency path.
    Tree(TreeItem),
    /// A message with a severity.
    Notice {
        /// How severe the message is.
        level: Level,
        /// The message.
        message: Line,
    },
    /// Blocks rendered in order, separated by a blank line.
    Group(Vec<ViewNode>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_neutralises_escape_sequences() {
        assert_eq!(
            sanitize("a\x1b[31mb\x07\u{9b}c"),
            "a\u{FFFD}[31mb\u{FFFD}\u{FFFD}c"
        );
        assert_eq!(sanitize("tab\tnew\nline"), "tab\tnew\nline");
    }
}
