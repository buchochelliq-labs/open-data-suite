//! Plain backend: stable, uncoloured text for pipes, CI logs and `grep` (ADR-0003 §1).
//!
//! Decorations are ASCII only. Every logical value occupies exactly one output line:
//! line breaks inside values become spaces. Tables are tab-separated with a header row,
//! and tabs inside cells also become spaces. Control characters are neutralised by
//! [`sanitize`].

use super::super::view::{Level, Line, TreeItem, ViewNode, plain_text, sanitize};

/// Renders `node` to a string ending in a newline.
pub fn render(node: &ViewNode) -> String {
    let mut out = String::new();
    write_node(&mut out, node);
    out
}

fn write_node(out: &mut String, node: &ViewNode) {
    match node {
        ViewNode::Heading(title) => push_line(out, &one_line(title)),
        ViewNode::Paragraph(line) => push_line(out, &text(line)),
        ViewNode::KeyValue(pairs) => {
            for (key, value) in pairs {
                push_line(out, &format!("{}: {}", one_line(key), text(value)));
            }
        }
        ViewNode::Table {
            title,
            columns,
            rows,
        } => {
            if let Some(title) = title {
                push_line(out, &one_line(title));
            }
            push_line(
                out,
                &columns
                    .iter()
                    .map(|c| cell(c))
                    .collect::<Vec<_>>()
                    .join("\t"),
            );
            for row in rows {
                let cells: Vec<String> = row.iter().map(|c| cell(&plain_text(c))).collect();
                push_line(out, &cells.join("\t"));
            }
        }
        ViewNode::Tree(root) => write_tree(out, root, 0),
        ViewNode::Notice { level, message } => {
            let prefix = match level {
                Level::Info => "info",
                Level::Warning => "warning",
                Level::Error => "error",
            };
            push_line(out, &format!("{prefix}: {}", text(message)));
        }
        ViewNode::Group(children) => {
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                write_node(out, child);
            }
        }
    }
}

fn write_tree(out: &mut String, item: &TreeItem, depth: usize) {
    push_line(out, &format!("{}{}", "  ".repeat(depth), text(&item.label)));
    for child in &item.children {
        write_tree(out, child, depth + 1);
    }
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

fn text(line: &Line) -> String {
    one_line(&plain_text(line))
}

/// Sanitises `text` and collapses line breaks so one logical value is one output line.
fn one_line(text: &str) -> String {
    sanitize(text)
        .replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
}

/// Like [`one_line`], but also removes tabs, which delimit table cells.
fn cell(text: &str) -> String {
    one_line(text).replace('\t', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::present::view::{Span, Tone};

    #[test]
    fn table_cells_cannot_break_rows_or_columns() {
        let node = ViewNode::Table {
            title: None,
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![
                vec![Span::plain("x\ty")],
                vec![Span::toned("line1\r\nline2", Tone::Code)],
            ]],
        };
        assert_eq!(render(&node), "a\tb\nx y\tline1 line2\n");
    }

    #[test]
    fn groups_are_separated_by_one_blank_line() {
        let node = ViewNode::Group(vec![
            ViewNode::Heading("one".into()),
            ViewNode::Paragraph(vec![Span::plain("two")]),
        ]);
        assert_eq!(render(&node), "one\n\ntwo\n");
    }

    #[test]
    fn key_value_tree_and_notice_layout() {
        let node = ViewNode::Group(vec![
            ViewNode::KeyValue(vec![("key".into(), vec![Span::plain("a\nb")])]),
            ViewNode::Tree(TreeItem {
                label: vec![Span::plain("root")],
                children: vec![TreeItem {
                    label: vec![Span::plain("child")],
                    children: vec![TreeItem::leaf(vec![Span::plain("grandchild")])],
                }],
            }),
            ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain("multi\nline")],
            },
        ]);
        assert_eq!(
            render(&node),
            "key: a b\n\nroot\n  child\n    grandchild\n\nwarning: multi line\n"
        );
    }

    #[test]
    fn control_characters_never_reach_the_output() {
        let node = ViewNode::Paragraph(vec![Span::plain("evil\x1b]0;title\x07")]);
        let out = render(&node);
        assert!(!out.contains('\x1b') && !out.contains('\x07'), "{out:?}");
    }
}
