//! Rich backend: styled terminal output via `rs-rich` (ADR-0003 §1, §4).
//!
//! This is the only module in the workspace that may use `rich`. Everything is rendered
//! into a string with [`Console::capture`], so ODS keeps control of which stream the
//! bytes go to.

use rich::Console;
use rich::color::ColorSystem;
use rich::markup::escape;
use rich::table::Table;
use rich::text::Text;
use rich::theme::Theme;
use rich::tree::Tree;

use super::super::view::{Level, Span, Tone, TreeItem, ViewNode, plain_text};
use crate::output::ColorChoice;

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
    pub fn new(color: ColorChoice, width: Option<usize>) -> Self {
        let mut builder = Console::builder()
            .theme(ods_theme())
            .highlight(false)
            .emoji(false);
        builder = match color {
            ColorChoice::Auto => builder,
            ColorChoice::Always => builder
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
        ViewNode::Heading(title) => {
            print_markup(console, &styled(&escape(title), "bold underline"));
        }
        ViewNode::Paragraph(line) => print_markup(console, &markup(line)),
        ViewNode::KeyValue(pairs) => {
            let key_width = pairs
                .iter()
                .map(|(key, _)| key.chars().count())
                .max()
                .unwrap_or(0);
            for (key, value) in pairs {
                let padded = format!("{key:<key_width$}");
                let key = styled(&escape(&padded), theme_key(Tone::Emphasis));
                print_markup(console, &format!("{key}  {}", markup(value)));
            }
        }
        ViewNode::Table {
            title,
            columns,
            rows,
        } => {
            // rs-rich 0.0.6 parses the title as markup but treats headers and cells as
            // plain text, so cell tones are dropped until styled cells are published
            // (0.0.7 `add_row_text`; see ADR-0003 §6).
            let mut table = Table::new();
            if let Some(title) = title {
                table = table.title(escape(title));
            }
            for column in columns {
                table.add_column(column.as_str());
            }
            for row in rows {
                let cells: Vec<String> = row.iter().map(|cell| plain_text(cell)).collect();
                let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
                table.add_row(&refs);
            }
            console.print(&table);
        }
        ViewNode::Tree(root) => {
            // rs-rich 0.0.6 tree labels are plain text, so tones are dropped here
            // (reported upstream; see ADR-0003 §6).
            let mut tree = Tree::new(plain_text(&root.label));
            add_children(&mut tree, &root.children);
            console.print(&tree);
        }
        ViewNode::Notice { level, message } => {
            let (label, tone) = match level {
                Level::Info => ("info", Tone::Muted),
                Level::Warning => ("warning", Tone::Warning),
                Level::Error => ("error", Tone::Error),
            };
            print_markup(
                console,
                &format!(
                    "{} {}",
                    styled(&format!("{label}:"), theme_key(tone)),
                    markup(message)
                ),
            );
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
        let child = tree.add(plain_text(&item.label));
        add_children(child, &item.children);
    }
}

fn print_markup(console: &Console, markup: &str) {
    // `markup` is built only from escaped text and ODS theme keys, so parsing cannot
    // fail on user data; fall back to literal text rather than lose output if it does.
    let text = Text::from_markup(markup).unwrap_or_else(|_| Text::new(markup));
    console.print(&text);
}

/// Converts spans to rich markup, escaping all literal text.
fn markup(line: &[Span]) -> String {
    line.iter()
        .map(|span| match span.tone {
            Some(tone) => styled(&escape(&span.text), theme_key(tone)),
            None => escape(&span.text),
        })
        .collect()
}

fn styled(escaped: &str, style: &str) -> String {
    format!("[{style}]{escaped}[/]")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn table_cells_render_brackets_literally() {
        let renderer = RichRenderer::new(ColorChoice::Never, Some(40));
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
    fn user_text_is_not_interpreted_as_markup() {
        let renderer = RichRenderer::new(ColorChoice::Never, Some(80));
        let out = renderer.render(&ViewNode::Paragraph(vec![Span::plain("[bold]not bold[/]")]));
        assert_eq!(out, "[bold]not bold[/]\n");
    }
}
