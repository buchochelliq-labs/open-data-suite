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

/// The site ODS's documentation is published at (`site_url` in `mkdocs.yml`).
const DOCS_SITE: &str = "https://buchochelliq-labs.github.io/open-data-suite/";

/// Where a span leads when a terminal can follow links (ADR-0003 §7): a local file, or
/// a page of ODS's documentation. It is always a URL ODS built itself, never text from
/// data, so nothing a project or an engine says becomes a link; and `url` encodes it,
/// so it can't carry a control character out of the escape that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link(url::Url);

impl Link {
    /// A local file, made absolute against the current directory (as the path would be
    /// opened). `None` if it can't be expressed as a `file:` URL.
    pub fn file(path: &std::path::Path) -> Option<Self> {
        let path = std::path::absolute(path).ok()?;
        url::Url::from_file_path(path).ok().map(Self)
    }

    /// A page of ODS's documentation, e.g. `cli/#exit-status`.
    pub fn docs(page: &'static str) -> Option<Self> {
        url::Url::parse(DOCS_SITE).ok()?.join(page).ok().map(Self)
    }

    /// The URL.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// A run of text with an optional tone, and optionally a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The literal text. Never interpreted as markup by any backend.
    pub text: String,
    /// How the text should be emphasised, if at all.
    pub tone: Option<Tone>,
    /// Where it leads, for backends that can link. The text says the same thing on its
    /// own: a link only saves copying it.
    pub link: Option<Link>,
}

impl Span {
    /// Unstyled text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: None,
            link: None,
        }
    }

    /// Text with a tone.
    pub fn toned(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone: Some(tone),
            link: None,
        }
    }

    /// The span, leading to `link` where a terminal can follow it.
    #[must_use]
    pub fn linked(mut self, link: Option<Link>) -> Self {
        self.link = link;
        self
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
/// Tabs and line breaks are kept; backends decide how to lay them out. A carriage
/// return becomes a newline (`\r\n` one newline): on its own it moves the cursor back
/// to the start of the line, so a value could overwrite what was printed before it
/// (e.g. a status) (#192).
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// A line of an engine's own output (e.g. dbt's), as streamed to stderr while it runs
/// (#322): `HH:MM:SS  text`, or the text alone without a time. The engine's colour
/// codes (`ESC [ … m`) are dropped, and any other control character is neutralised by
/// [`sanitize`].
pub fn engine_line(time: Option<&str>, text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameters and intermediates, up to the final byte (`m` for colours).
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
            continue;
        }
        plain.push(c);
    }
    let plain = sanitize(&plain);
    match time {
        Some(time) => format!("{}  {plain}", sanitize(time)),
        None => plain,
    }
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
        /// Indices into `rows` where a new section starts (e.g. what is built, then
        /// what is reused), shown as a rule between them. Order is the caller's.
        breaks: Vec<usize>,
        /// One cell per column summing the rows up (e.g. totals). It repeats what the
        /// view says elsewhere, so the plain backend, whose tables are data rows only,
        /// leaves it out.
        footer: Option<Vec<Line>>,
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
    /// A block set apart under a title, e.g. one failure and why it happened. Its level
    /// colours the frame. The plain backend prints the title as a line of its own and
    /// the body as it would anyway.
    Panel {
        /// What the panel is about.
        title: Line,
        /// How severe its content is.
        level: Level,
        /// What it holds.
        body: Box<ViewNode>,
    },
    /// Blocks rendered in order, separated by a blank line.
    Group(Vec<ViewNode>),
}

impl ViewNode {
    /// Removes every span's link, for output that can't follow them.
    pub fn drop_links(&mut self) {
        fn line(spans: &mut [Span]) {
            for span in spans {
                span.link = None;
            }
        }
        fn tree(item: &mut TreeItem) {
            line(&mut item.label);
            item.children.iter_mut().for_each(tree);
        }
        match self {
            ViewNode::Heading(_) => {}
            ViewNode::Paragraph(l) | ViewNode::Notice { message: l, .. } => line(l),
            ViewNode::KeyValue(pairs) => pairs.iter_mut().for_each(|(_, l)| line(l)),
            ViewNode::Table { rows, footer, .. } => {
                rows.iter_mut().flatten().for_each(|l| line(l));
                footer.iter_mut().flatten().for_each(|l| line(l));
            }
            ViewNode::Tree(root) => tree(root),
            ViewNode::Panel { title, body, .. } => {
                line(title);
                body.drop_links();
            }
            ViewNode::Group(children) => children.iter_mut().for_each(ViewNode::drop_links),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_lines_drop_colours_and_neutralise_controls() {
        assert_eq!(
            engine_line(Some("14:02:05"), "OK [\u{1b}[32mOK\u{1b}[0m in 0.13s]\u{7}"),
            "14:02:05  OK [OK in 0.13s]\u{FFFD}"
        );
        assert_eq!(
            engine_line(None, "print from a model"),
            "print from a model"
        );
    }

    #[test]
    fn sanitize_neutralises_escape_sequences() {
        assert_eq!(
            sanitize("a\x1b[31mb\x07\u{9b}c"),
            "a\u{FFFD}[31mb\u{FFFD}\u{FFFD}c"
        );
        assert_eq!(sanitize("tab\tnew\nline"), "tab\tnew\nline");
    }

    proptest::proptest! {
        /// Whatever a value holds, what reaches the terminal holds no control character
        /// but a newline or a tab, and sanitizing it again changes nothing.
        #[test]
        fn nothing_can_drive_the_terminal(text in proptest::prelude::any::<String>()) {
            let clean = sanitize(&text);
            proptest::prop_assert!(
                !clean.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\t')),
                "{:?}", clean
            );
            proptest::prop_assert_eq!(sanitize(&clean), clean.clone());
            let line = engine_line(Some("12:00:00"), &text);
            proptest::prop_assert!(!line.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\t')));
        }
    }

    #[test]
    fn a_carriage_return_cant_overwrite_what_came_before() {
        assert_eq!(sanitize("ok\rFAILED"), "ok\nFAILED");
        assert_eq!(sanitize("a\r\nb\r\r\n"), "a\nb\n\n");
        assert_eq!(
            engine_line(Some("12:00:00"), "1 of 2 OK\rERROR"),
            "12:00:00  1 of 2 OK\nERROR"
        );
    }

    #[test]
    fn links_can_be_dropped_everywhere() {
        let link = Link::docs("cli/#exit-status");
        assert_eq!(
            link.as_ref().map(Link::as_str),
            Some("https://buchochelliq-labs.github.io/open-data-suite/cli/#exit-status")
        );
        let linked = || vec![Span::plain("x").linked(link.clone())];
        let mut node = ViewNode::Group(vec![
            ViewNode::Paragraph(linked()),
            ViewNode::KeyValue(vec![("k".into(), linked())]),
            ViewNode::Table {
                title: None,
                columns: vec!["c".into()],
                rows: vec![vec![linked()]],
                breaks: Vec::new(),
                footer: Some(vec![linked()]),
            },
            ViewNode::Tree(TreeItem {
                label: linked(),
                children: vec![TreeItem::leaf(linked())],
            }),
            ViewNode::Panel {
                title: linked(),
                level: Level::Error,
                body: Box::new(ViewNode::Notice {
                    level: Level::Info,
                    message: linked(),
                }),
            },
        ]);
        node.drop_links();
        assert!(!format!("{node:?}").contains("Link("), "{node:?}");
    }
}
