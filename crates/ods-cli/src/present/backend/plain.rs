//! Plain backend: stable, uncoloured text for pipes, CI logs and `grep` (ADR-0003 §1).
//!
//! Decorations are ASCII only. Tables are tab-separated with a header row. Cell text has
//! tabs and newlines replaced by spaces so every table row stays on one line.

use std::fmt::Write as _;

use super::super::view::{Level, TreeItem, ViewNode, plain_text};

/// Renders `node` to a string ending in a newline.
pub fn render(node: &ViewNode) -> String {
    let mut out = String::new();
    write_node(&mut out, node);
    out
}

fn write_node(out: &mut String, node: &ViewNode) {
    match node {
        ViewNode::Heading(title) => push_line(out, &one_line(title)),
        ViewNode::Paragraph(line) => push_line(out, &plain_text(line)),
        ViewNode::KeyValue(pairs) => {
            for (key, value) in pairs {
                push_line(
                    out,
                    &format!("{}: {}", one_line(key), one_line(&plain_text(value))),
                );
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
            push_line(out, &format!("{prefix}: {}", plain_text(message)));
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
    let _ = writeln!(
        out,
        "{}{}",
        "  ".repeat(depth),
        one_line(&plain_text(&item.label))
    );
    for child in &item.children {
        write_tree(out, child, depth + 1);
    }
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// Collapses line breaks so one logical value is one output line.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
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
                vec![Span::toned("line1\nline2", Tone::Code)],
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
}
