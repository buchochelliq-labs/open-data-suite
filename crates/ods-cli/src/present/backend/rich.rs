//! Rich backend: styled terminal output via `rs-rich` (ADR-0003 §1, §4).
//!
//! This is the only module in the workspace that may use `rich`. Everything is rendered
//! into a string with [`Console::capture`], so ODS keeps control of which stream the
//! bytes go to. All displayed text is [`sanitize`]d, and it is never passed through
//! rs-rich's markup parser: styled text is built from spans directly.

use rich::cells::cell_len;
use rich::color::ColorSystem;
use rich::table::Table;
use rich::text::Text;
use rich::theme::Theme;
use rich::tree::Tree;
use rich::{Console, Justify};

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
}

impl RichRenderer {
    /// Creates a renderer. `width` overrides terminal detection.
    ///
    /// `ColorChoice::Auto` defers to rs-rich's detection (terminal, `NO_COLOR`) and also
    /// disables colour for `TERM=dumb`. `Always` overrides `NO_COLOR`; `Never` wins over
    /// everything.
    pub fn new(color: ColorChoice, width: Option<usize>) -> Self {
        Self::with_environment(color, width, term_is_dumb())
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
        }
    }

    /// Renders `node` to a string (ANSI escapes included when colour is enabled).
    pub fn render(&self, node: &ViewNode) -> String {
        self.console.capture(|console| render_node(console, node))
    }
}

fn render_node(console: &Console, node: &ViewNode) {
    match node {
        ViewNode::Heading(title) => console.print(&Text::styled(sanitize(title), "bold underline")),
        ViewNode::Paragraph(line) => console.print(&text(line)),
        ViewNode::KeyValue(pairs) => {
            let keys: Vec<String> = pairs.iter().map(|(key, _)| sanitize(key)).collect();
            let key_width = keys.iter().map(|key| cell_len(key)).max().unwrap_or(0);
            for (key, (_, value)) in keys.iter().zip(pairs) {
                let mut line = Text::new("");
                line.append(key, Some(theme_key(Tone::Emphasis).into()));
                line.append(&" ".repeat(key_width - cell_len(key) + 2), None);
                append_spans(&mut line, value);
                console.print(&line);
            }
        }
        ViewNode::Table {
            title,
            columns,
            rows,
        } => {
            // Printed as a line of its own: rs-rich parses `Table::title` as markup,
            // and building `Text` directly avoids escaping user data altogether.
            if let Some(title) = title {
                console.print(&Text::styled(sanitize(title), theme_key(Tone::Emphasis)));
            }
            // Headers, cells and tree labels are passed as `Text`, never as strings:
            // since rs-rich 0.0.7 a plain string there is parsed as markup, which would
            // let data such as `[bold]` or `[/]` in a node name be interpreted.
            let mut table = Table::new();
            for column in columns {
                table.add_column_text(Text::new(sanitize(column)), Justify::Default);
            }
            for row in rows {
                table.add_row_text(row.iter().map(|cell| text(cell)).collect());
            }
            console.print(&table);
        }
        ViewNode::Tree(root) => {
            let mut tree = Tree::new(text(&root.label));
            add_children(&mut tree, &root.children);
            console.print(&tree);
        }
        ViewNode::Notice { level, message } => {
            let (label, tone) = match level {
                Level::Info => ("info:", Tone::Muted),
                Level::Warning => ("warning:", Tone::Warning),
                Level::Error => ("error:", Tone::Error),
            };
            let mut line = Text::new("");
            line.append(label, Some(theme_key(tone).into()));
            line.append(" ", None);
            append_spans(&mut line, message);
            console.print(&line);
        }
        ViewNode::Group(children) => {
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    console.print(&Text::new(""));
                }
                render_node(console, child);
            }
        }
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
        text.append(
            &sanitize(&span.text),
            span.tone.map(|tone| theme_key(tone).into()),
        );
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
        });
        assert!(out.contains("[t]"), "{out}");
        assert!(out.contains("[h]"), "{out}");
        assert!(out.contains("[bold]x[/]"), "{out}");
        assert!(!out.contains('\\'), "no escape characters may leak: {out}");
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
