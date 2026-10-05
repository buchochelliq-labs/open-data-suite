//! Rich backend: styled terminal output via `rs-rich` (ADR-0003 §1, §4).
//!
//! This is the only module in the workspace that may use `rich`. Everything is rendered
//! into a string with [`Console::capture`], so ODS keeps control of which stream the
//! bytes go to. All displayed text is [`sanitize`]d, and it is never passed through
//! rs-rich's markup parser: styled text is built from spans directly.

use rich::cells::cell_len;
use rich::color::ColorSystem;
use rich::measure::Measurement;
use rich::panel::Panel;
use rich::protocol::Renderable;
use rich::segment::Segment;
use rich::style::Style;
use rich::table::Table;
use rich::text::Text;
use rich::theme::Theme;
use rich::tree::Tree;
use rich::{Console, ConsoleOptions, HorizontalAlign, Justify};

#[cfg(all(test, unix))]
use super::super::view::Link;
use super::super::view::{Level, Span, Tone, TreeItem, ViewNode, sanitize};
use crate::output::{ColorChoice, term_is_dumb};

/// Theme key for a tone. Keys are namespaced so they never collide with rich's own styles.
fn theme_key(tone: Tone) -> &'static str {
    match tone {
        Tone::Emphasis => "ods.emphasis",
        Tone::Muted => "ods.muted",
        Tone::Added => "ods.added",
        Tone::Removed => "ods.removed",
        Tone::Warning => "ods.warning",
        Tone::Error => "ods.error",
        Tone::Success => "ods.success",
        Tone::Code => "ods.code",
    }
}

/// Default style for a tone. Only the 16 standard colours, so output reads the same
/// on every palette.
fn default_style(tone: Tone) -> &'static str {
    match tone {
        Tone::Emphasis => "bold",
        Tone::Muted => "dim",
        Tone::Added | Tone::Success => "green",
        Tone::Removed => "red",
        Tone::Warning => "yellow",
        Tone::Error => "bold red",
        Tone::Code => "cyan",
    }
}

fn ods_theme() -> Theme {
    let styles = Tone::ALL
        .iter()
        .map(|&tone| (theme_key(tone), default_style(tone)));
    // The definitions above are constants covered by `theme_resolves_every_tone`.
    Theme::from_styles(styles, true).expect("built-in ODS theme styles must parse")
}

/// Renders view trees with `rs-rich`.
pub struct RichRenderer {
    console: Console,
    /// Whether spans' links are written (as OSC 8 hyperlinks): only to a terminal that
    /// follows them, and only with colour, since rs-rich writes them with the styles.
    links: bool,
}

impl RichRenderer {
    /// Creates a renderer. `width` overrides terminal detection.
    ///
    /// `ColorChoice::Auto` defers to rs-rich's detection (terminal, `NO_COLOR`) and also
    /// disables colour for `TERM=dumb`. `Always` overrides `NO_COLOR`; `Never` wins over
    /// everything.
    pub fn new(color: ColorChoice, width: Option<usize>) -> Self {
        let mut renderer = Self::with_environment(color, width, term_is_dumb());
        // `supports-hyperlinks` knows which terminals follow OSC 8, and honours
        // `FORCE_HYPERLINK`; an unknown terminal gets no links.
        renderer.links = supports_hyperlinks::on(supports_hyperlinks::Stream::Stdout);
        renderer
    }

    /// Writes spans' links (tests; `new` decides from the terminal).
    #[cfg(test)]
    fn with_links(mut self, links: bool) -> Self {
        self.links = links;
        self
    }

    fn with_environment(color: ColorChoice, width: Option<usize>, dumb_terminal: bool) -> Self {
        let mut builder = Console::builder()
            .theme(ods_theme())
            .highlight(false)
            .emoji(false);
        builder = match color {
            ColorChoice::Auto if dumb_terminal => builder.no_color(true),
            ColorChoice::Auto => builder,
            // `no_color(false)` is explicit: without it rs-rich falls back to `NO_COLOR`.
            ColorChoice::Always => builder
                .no_color(false)
                .force_terminal(true)
                .color_system(Some(ColorSystem::Standard)),
            ColorChoice::Never => builder.no_color(true),
        };
        if let Some(width) = width {
            builder = builder.width(width);
        }
        Self {
            console: builder.build(),
            links: false,
        }
    }

    /// Renders `node` to a string (ANSI escapes included when colour is enabled).
    pub fn render(&self, node: &ViewNode) -> String {
        if self.links {
            self.console.capture(|console| render_node(console, node))
        } else {
            let mut node = node.clone();
            node.drop_links();
            self.console.capture(|console| render_node(console, &node))
        }
    }
}

/// Prints `node`. Groups and key-value lines print one renderable at a time, as rs-rich
/// prints a top-level `Text` at its own width; everything else prints as one renderable.
fn render_node(console: &Console, node: &ViewNode) {
    match node {
        ViewNode::Group(children) => {
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    console.print(&Text::new(""));
                }
                render_node(console, child);
            }
        }
        ViewNode::KeyValue(_) => {
            for line in key_value_lines(node) {
                console.print(&line);
            }
        }
        _ => console.print(&Nested(renderable(node))),
    }
}

/// A shared renderable as a renderable of its own, for `Panel`'s boxed child.
struct Nested(Shared);

impl Renderable for Nested {
    fn rich_render(&self, console: &Console, options: &ConsoleOptions) -> Vec<Segment> {
        self.0.rich_render(console, options)
    }

    fn measure(&self, console: &Console, options: &ConsoleOptions) -> Measurement {
        self.0.measure(console, options)
    }

    fn fit_to_measurement(&self) -> bool {
        self.0.fit_to_measurement()
    }

    fn printed_text(&self) -> Option<Text> {
        self.0.printed_text()
    }
}

/// Aligned `key  value` lines.
fn key_value_lines(node: &ViewNode) -> Vec<Text> {
    let ViewNode::KeyValue(pairs) = node else {
        return Vec::new();
    };
    let keys: Vec<String> = pairs.iter().map(|(key, _)| sanitize(key)).collect();
    let key_width = keys.iter().map(|key| cell_len(key)).max().unwrap_or(0);
    keys.iter()
        .zip(pairs)
        .map(|(key, (_, value))| {
            let mut line = Text::new("");
            line.append(key, Some(theme_key(Tone::Emphasis).into()));
            line.append(&" ".repeat(key_width - cell_len(key) + 2), None);
            append_spans(&mut line, value);
            line
        })
        .collect()
}

/// A view as one renderable, so views nest (a panel holds any view).
type Shared = Box<dyn Renderable>;

/// Renderables one after another, each on its own lines (as rs-rich's `Renderables`,
/// which needs `Send + Sync` children that `Panel` isn't).
struct Stack(Vec<Shared>);

impl Renderable for Stack {
    fn rich_render(&self, console: &Console, options: &ConsoleOptions) -> Vec<Segment> {
        let options = options.reset_height();
        let mut segments = Vec::new();
        for part in &self.0 {
            let rendered = console.render(part.as_ref(), Some(&options));
            if !segments.is_empty() {
                segments.push(Segment::line());
            }
            segments.extend(rendered);
        }
        segments
    }
}

fn stacked(parts: Vec<Shared>) -> Shared {
    Box::new(Stack(parts))
}

fn renderable(node: &ViewNode) -> Shared {
    match node {
        ViewNode::Heading(title) => Box::new(Text::styled(sanitize(title), "bold underline")),
        ViewNode::Paragraph(line) => Box::new(text(line)),
        ViewNode::KeyValue(_) => stacked(
            key_value_lines(node)
                .into_iter()
                .map(|line| Box::new(line) as Shared)
                .collect(),
        ),
        ViewNode::Table {
            title,
            columns,
            rows,
            breaks,
            footer,
        } => {
            // The title, headers, cells and tree labels are all passed as `Text`, never
            // as strings: rs-rich parses a plain string there (and `Table::title`) as
            // markup, which would let data such as `[bold]` or `[/]` in a node name be
            // interpreted. `title_text` takes the title literally, so no escaping.
            let mut table = Table::new();
            if let Some(title) = title {
                table = table.title_text(Text::styled(sanitize(title), theme_key(Tone::Emphasis)));
            }
            for (i, column) in columns.iter().enumerate() {
                table.add_column_text(Text::new(sanitize(column)), Justify::Default);
                if let Some(cell) = footer.as_ref().and_then(|f| f.get(i)) {
                    table.column_footer(text(cell));
                }
            }
            if footer.is_some() {
                table = table.show_footer(true);
            }
            for (i, row) in rows.iter().enumerate() {
                // `add_section` ends the section at the last row added.
                if i > 0 && breaks.contains(&i) {
                    table.add_section();
                }
                table.add_row_text(row.iter().map(|cell| text(cell)).collect());
            }
            Box::new(table)
        }
        ViewNode::Tree(root) => {
            let mut tree = Tree::new(text(&root.label));
            add_children(&mut tree, &root.children);
            Box::new(tree)
        }
        ViewNode::Notice { level, message } => {
            let (label, tone) = level_label(*level);
            let mut line = Text::new("");
            line.append(label, Some(theme_key(tone).into()));
            line.append(" ", None);
            append_spans(&mut line, message);
            Box::new(line)
        }
        ViewNode::Panel { title, level, body } => {
            let (_, tone) = level_label(*level);
            // The title is `Text`, never markup, like every other piece of data.
            Box::new(
                Panel::new(Box::new(Nested(renderable(body))))
                    .title_as_text(text(title))
                    .title_align(HorizontalAlign::Left)
                    .border_style(theme_key(tone))
                    .padding((0, 1, 0, 1)),
            )
        }
        ViewNode::Group(children) => {
            let mut parts: Vec<Shared> = Vec::new();
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    parts.push(Box::new(Text::new("")));
                }
                parts.push(renderable(child));
            }
            stacked(parts)
        }
    }
}

/// How a level is labelled and coloured.
fn level_label(level: Level) -> (&'static str, Tone) {
    match level {
        Level::Info => ("info:", Tone::Muted),
        Level::Warning => ("warning:", Tone::Warning),
        Level::Error => ("error:", Tone::Error),
    }
}

fn add_children(tree: &mut Tree, items: &[TreeItem]) {
    for item in items {
        let child = tree.add(text(&item.label));
        add_children(child, &item.children);
    }
}

/// Builds styled text from spans without parsing markup, so user data is never
/// interpreted: tones become theme style names attached to exact byte ranges.
fn text(line: &[Span]) -> Text {
    let mut text = Text::new("");
    append_spans(&mut text, line);
    text
}

fn append_spans(text: &mut Text, line: &[Span]) {
    for span in line {
        let start = text.plain().len();
        text.append(
            &sanitize(&span.text),
            span.tone.map(|tone| theme_key(tone).into()),
        );
        // Over the tone's style, so the text looks the same with or without a link.
        // Only renderers built with links get here with one (see `RichRenderer`).
        if let Some(link) = &span.link {
            text.stylize(
                Style::new().with_link(link.as_str()),
                start,
                text.plain().len(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(text: &str) -> ViewNode {
        ViewNode::Paragraph(vec![Span::toned(text, Tone::Emphasis)])
    }

    #[test]
    fn theme_resolves_every_tone() {
        let theme = ods_theme();
        for tone in Tone::ALL {
            assert!(
                theme.get(theme_key(tone)).is_some(),
                "missing theme entry for {tone:?}"
            );
        }
    }

    #[test]
    fn user_text_is_not_interpreted_as_markup() {
        let renderer = RichRenderer::with_environment(ColorChoice::Never, Some(80), false);
        let out = renderer.render(&ViewNode::Paragraph(vec![
            Span::plain("[bold]not bold[/] \\"),
            Span::toned("[/] \\", Tone::Code),
        ]));
        assert_eq!(out, "[bold]not bold[/] \\[/] \\\n");
    }

    #[test]
    fn table_cells_render_brackets_literally() {
        let renderer = RichRenderer::with_environment(ColorChoice::Never, Some(40), false);
        let out = renderer.render(&ViewNode::Table {
            title: Some("[t]".into()),
            columns: vec!["[h]".into()],
            rows: vec![vec![vec![Span::toned("[bold]x[/]", Tone::Code)]]],
            breaks: Vec::new(),
            footer: None,
        });
        assert!(out.contains("[t]"), "{out}");
        assert!(out.contains("[h]"), "{out}");
        assert!(out.contains("[bold]x[/]"), "{out}");
        assert!(!out.contains('\\'), "no escape characters may leak: {out}");
    }

    #[test]
    fn model_names_render_literally_everywhere() {
        // A model name that is markup (`[bold]x`) or ends in a backslash (`a\`, which
        // `markup::escape` does not round-trip, ADR-0003 §6) must reach the terminal as
        // is, as a table title, header, cell and tree label.
        let renderer = RichRenderer::with_environment(ColorChoice::Never, Some(60), false);
        for name in ["[bold]x", "a\\"] {
            let table = renderer.render(&ViewNode::Table {
                title: Some(format!("title {name}")),
                columns: vec![format!("head {name}")],
                rows: vec![vec![vec![Span::plain(format!("cell {name}"))]]],
                breaks: Vec::new(),
                footer: None,
            });
            let tree = renderer.render(&ViewNode::Tree(TreeItem {
                label: vec![Span::plain(format!("root {name}"))],
                children: vec![TreeItem::leaf(vec![Span::plain(format!("leaf {name}"))])],
            }));
            for expected in ["title", "head", "cell"] {
                assert!(table.contains(&format!("{expected} {name}")), "{table}");
            }
            for expected in ["root", "leaf"] {
                assert!(tree.contains(&format!("{expected} {name}")), "{tree}");
            }
            assert!(
                !table.contains("\\\\") && !tree.contains("\\\\"),
                "{table}{tree}"
            );
        }
    }

    #[test]
    fn sections_are_ruled_off_and_the_footer_follows_the_rows() {
        let renderer = RichRenderer::with_environment(ColorChoice::Never, Some(40), false);
        let table = |breaks: Vec<usize>, footer: Option<Vec<crate::present::Line>>| {
            renderer.render(&ViewNode::Table {
                title: None,
                columns: vec!["node".into(), "action".into()],
                rows: vec![
                    vec![vec![Span::plain("a")], vec![Span::plain("build")]],
                    vec![vec![Span::plain("b")], vec![Span::plain("reuse")]],
                ],
                breaks,
                footer,
            })
        };
        let rules = |out: &str| {
            out.lines()
                .filter(|l| l.contains('─') || l.contains('━'))
                .count()
        };
        let plain = table(Vec::new(), None);
        let sectioned = table(vec![0, 1], None);
        assert_eq!(rules(&sectioned), rules(&plain) + 1, "{plain}\n{sectioned}");
        let totalled = table(
            Vec::new(),
            Some(vec![
                vec![Span::plain("[b]2 nodes")],
                vec![Span::plain("1 build")],
            ]),
        );
        let lines: Vec<&str> = totalled.lines().collect();
        let footer = lines
            .iter()
            .position(|l| l.contains("[b]2 nodes"))
            .expect("footer shown literally");
        let last_row = lines.iter().position(|l| l.contains("reuse")).unwrap();
        assert!(footer > last_row, "{totalled}");
        assert!(lines[footer].contains("1 build"), "{totalled}");
    }

    #[test]
    fn a_panel_frames_any_view_under_a_literal_title() {
        let renderer = RichRenderer::with_environment(ColorChoice::Never, Some(50), false);
        let out = renderer.render(&ViewNode::Panel {
            title: vec![
                Span::toned("[bold]orders", Tone::Code),
                Span::plain(" failed"),
            ],
            level: Level::Error,
            body: Box::new(ViewNode::Group(vec![
                ViewNode::KeyValue(vec![("what".into(), vec![Span::plain("it broke")])]),
                ViewNode::Tree(TreeItem {
                    label: vec![Span::plain("why")],
                    children: vec![TreeItem::leaf(vec![Span::plain("evidence")])],
                }),
            ])),
        });
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with('╭') && lines[0].contains("[bold]orders failed"),
            "{out}"
        );
        assert!(lines.last().unwrap().starts_with('╰'), "{out}");
        for inner in ["what  it broke", "why", "evidence"] {
            let line = lines
                .iter()
                .find(|l| l.contains(inner))
                .unwrap_or_else(|| panic!("{inner}: {out}"));
            assert!(
                line.starts_with('│') && line.trim_end().ends_with('│'),
                "{out}"
            );
        }
        // The group's blank line between the blocks stays inside the frame.
        assert!(
            lines
                .iter()
                .any(|l| l.trim_matches(|c| c == '│' || c == ' ').is_empty()),
            "{out}"
        );
    }

    // Unix paths: on Windows the same path gains a drive.
    #[cfg(unix)]
    #[test]
    fn links_are_written_only_when_the_terminal_follows_them() {
        let link = Link::file(std::path::Path::new("/tmp/runs/r.jsonl"));
        let node = ViewNode::Paragraph(vec![
            Span::toned("/tmp/runs/r.jsonl", Tone::Code).linked(link.clone()),
        ]);
        let renderer = || RichRenderer::with_environment(ColorChoice::Always, Some(80), false);
        let with = renderer().with_links(true).render(&node);
        assert!(
            with.contains("\x1b]8;;file:///tmp/runs/r.jsonl\x1b\\"),
            "{with:?}"
        );
        // The same text and colours either way.
        let without = renderer().with_links(false).render(&node);
        assert!(!without.contains("\x1b]8"), "{without:?}");
        assert!(without.contains("\x1b[36m/tmp/runs/r.jsonl"), "{without:?}");
    }

    // Unix paths: on Windows the same path gains a drive.
    #[cfg(unix)]
    #[test]
    fn a_link_cant_carry_a_control_character_out_of_its_escape() {
        // A file name with ESC, BEL and the OSC terminator in it.
        let link = Link::file(std::path::Path::new("/tmp/a\x1b\\b\x07c")).unwrap();
        assert!(
            !link.as_str().chars().any(char::is_control),
            "{}",
            link.as_str()
        );
        let out = RichRenderer::with_environment(ColorChoice::Always, Some(80), false)
            .with_links(true)
            .render(&ViewNode::Paragraph(vec![
                Span::plain("x").linked(Some(link)),
            ]));
        // One link opened and closed: nothing in the URL ends the escape early.
        assert_eq!(out.matches("\x1b]8;;").count(), 2, "{out:?}");
        assert!(!out.contains('\x07'), "{out:?}");
    }

    #[test]
    fn tree_labels_render_brackets_literally() {
        let renderer = RichRenderer::with_environment(ColorChoice::Never, Some(40), false);
        let out = renderer.render(&ViewNode::Tree(TreeItem {
            label: vec![Span::plain("[bold]root[/]")],
            children: vec![TreeItem::leaf(vec![Span::toned("[red]leaf", Tone::Code)])],
        }));
        assert!(out.contains("[bold]root[/]"), "{out}");
        assert!(out.contains("[red]leaf"), "{out}");
    }

    #[test]
    fn table_cell_tones_are_styled() {
        let renderer = RichRenderer::with_environment(ColorChoice::Always, Some(40), false);
        let out = renderer.render(&ViewNode::Table {
            title: None,
            columns: vec!["c".into()],
            rows: vec![vec![vec![Span::toned("added", Tone::Added)]]],
            breaks: Vec::new(),
            footer: None,
        });
        // `ods.added` is green (SGR 32) in the default theme.
        assert!(out.contains("\x1b[32madded"), "{out:?}");
    }

    #[test]
    fn control_characters_never_reach_the_output() {
        let renderer = RichRenderer::with_environment(ColorChoice::Never, Some(80), false);
        for node in [
            paragraph("a\x1b[31mb"),
            ViewNode::Tree(TreeItem::leaf(vec![Span::plain("a\x1b[31mb")])),
            ViewNode::Table {
                title: None,
                columns: vec!["c".into()],
                rows: vec![vec![vec![Span::plain("a\x1bb")]]],
                breaks: Vec::new(),
                footer: None,
            },
        ] {
            let out = renderer.render(&node);
            assert!(!out.contains('\x1b'), "{node:?} leaked ESC: {out:?}");
        }
    }

    #[test]
    fn dumb_terminal_disables_auto_colour_only() {
        let auto = RichRenderer::with_environment(ColorChoice::Auto, Some(80), true);
        assert!(!auto.render(&paragraph("x")).contains('\x1b'));
        let always = RichRenderer::with_environment(ColorChoice::Always, Some(80), true);
        assert!(always.render(&paragraph("x")).contains('\x1b'));
    }

    #[test]
    fn key_values_align_by_display_width() {
        let renderer = RichRenderer::with_environment(ColorChoice::Never, Some(80), false);
        let out = renderer.render(&ViewNode::KeyValue(vec![
            ("表".into(), vec![Span::plain("wide")]),
            ("ab".into(), vec![Span::plain("narrow")]),
        ]));
        // "表" is two cells wide, the same as "ab", so neither key is padded.
        assert_eq!(out, "表  wide\nab  narrow\n");
    }
}
