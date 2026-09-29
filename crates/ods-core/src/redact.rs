//! Removing values and SQL from text meant for people (AGENTS.md rule 9).
//!
//! Engines' messages quote the values and statements they failed on, and those can hold
//! anything a query resolved: `--vars` values, environment variables, credentials. So
//! any engine message ODS keeps, shows or serves passes through here first, e.g. a
//! failed node's error summary (#322) or a configuration error (ADR-0005). The rules
//! are deliberately blunt: a removed identifier costs a little context, a kept literal
//! can leak a secret. The full message stays wherever the engine wrote it.

/// What a removed literal reads as.
pub const REMOVED: &str = "[value removed]";

/// What removed SQL reads as.
pub const SQL_REMOVED: &str = "[SQL removed]";

/// Replaces each span quoted with one of `quotes` by `placeholder`. Inside a span a
/// backslash escapes the next character and a doubled quote (`'it''s'`) is part of it.
/// A span that isn't closed runs to the end of the text.
pub fn quoted(text: &str, quotes: &[char], placeholder: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if !quotes.contains(&c) {
            out.push(c);
            continue;
        }
        out.push_str(placeholder);
        let mut escaped = false;
        while let Some(inner) = chars.next() {
            if escaped {
                escaped = false;
            } else if inner == '\\' {
                escaped = true;
            } else if inner == c {
                if chars.peek() == Some(&c) {
                    chars.next();
                } else {
                    break;
                }
            }
        }
    }
    out
}

/// Replaces literals by [`REMOVED`]: quoted spans (`'…'`, `"…"`, `` `…` ``), dollar-quoted
/// spans (`$$…$$`) and numbers that stand alone (`42`, `-3.5`, but not `INT32` or `v2`).
pub fn literals(text: &str) -> String {
    numbers(&quoted(&dollar_quoted(text), &['\'', '"', '`'], REMOVED))
}

fn dollar_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("$$") {
        out.push_str(&rest[..start]);
        out.push_str(REMOVED);
        let after = &rest[start + 2..];
        match after.find("$$") {
            Some(end) => rest = &after[end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn numbers(text: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts_number =
            c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit));
        let after_word = i > 0 && (word(chars[i - 1]) || chars[i - 1] == '.');
        if !starts_number || after_word {
            out.push(c);
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '.') {
            end += 1;
        }
        // A trailing full stop ends the sentence, not the number.
        let mut last = end;
        while last > i + 1 && chars[last - 1] == '.' {
            last -= 1;
        }
        if end < chars.len() && word(chars[end]) {
            // Part of a word, e.g. `3rd` or `2xl`: keep it as it is.
            out.extend(&chars[i..end]);
        } else {
            out.push_str(REMOVED);
            out.extend(&chars[last..end]);
        }
        i = end;
    }
    out
}

/// Phrases that start a SQL statement. A message is cut where one starts, since what
/// follows is the statement. Chosen so ordinary English ("could not update the
/// relation") isn't cut.
const SQL_STARTS: [&str; 11] = [
    "select ",
    "insert into ",
    "insert overwrite ",
    "delete from ",
    "merge into ",
    "create or replace ",
    "create table ",
    "create view ",
    "create temporary ",
    "alter table ",
    "drop table ",
];

/// One line for people from an engine's message: its first non-blank line, cut where a
/// SQL statement starts, with [`literals`] removed, control characters dropped and at
/// most `max_chars` characters (ending in `…` when cut). `None` if the message has no
/// text.
pub fn summary_line(text: &str, max_chars: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let lower = line.to_lowercase();
    let cut = SQL_STARTS
        .iter()
        .filter_map(|start| {
            lower.match_indices(start).map(|(at, _)| at).find(|&at| {
                at == 0
                    || !lower[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
        })
        .min();
    // Lowercasing can change byte lengths; only cut where the offsets agree.
    let (line, sql) = match cut {
        Some(at) if lower.len() == line.len() && line.is_char_boundary(at) => {
            (line[..at].trim_end(), true)
        }
        Some(_) => ("", true),
        None => (line, false),
    };
    let mut clean: String = literals(line).chars().filter(|c| !c.is_control()).collect();
    if sql {
        if !clean.is_empty() {
            clean.push(' ');
        }
        clean.push_str(SQL_REMOVED);
    }
    if clean.chars().count() > max_chars {
        clean = clean.chars().take(max_chars.saturating_sub(1)).collect();
        clean.push('…');
    }
    Some(clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_spans_are_replaced() {
        assert_eq!(
            quoted(
                r#"invalid type: string "s3cr3t", expected"#,
                &['"'],
                "<value>"
            ),
            "invalid type: string <value>, expected"
        );
        assert_eq!(quoted(r#"a "x\"y" b"#, &['"'], "<v>"), "a <v> b");
        assert_eq!(quoted("a 'it''s' b", &['\''], "<v>"), "a <v> b");
        assert_eq!(quoted("a 'open", &['\''], "<v>"), "a <v>");
        assert_eq!(quoted(r#"x "" y"#, &['"'], "<v>"), "x <v> y");
    }

    #[test]
    fn literals_remove_quotes_dollar_quotes_and_numbers() {
        assert_eq!(literals("KeyError: 'segment'"), "KeyError: [value removed]");
        assert_eq!(
            literals("Could not convert 12345 to INT32 at v2."),
            "Could not convert [value removed] to INT32 at v2."
        );
        assert_eq!(
            literals("got -3.5, want 3rd"),
            "got [value removed], want 3rd"
        );
        assert_eq!(
            literals("body $$ secret $$ end"),
            "body [value removed] end"
        );
        assert_eq!(literals("a `x` b"), "a [value removed] b");
        assert_eq!(
            literals("table_1 has 2 rows"),
            "table_1 has [value removed] rows"
        );
    }

    #[test]
    fn a_summary_line_is_the_first_line_without_sql_or_literals() {
        assert_eq!(
            summary_line("\n  KeyError: 'segment'\nTraceback …", 200).as_deref(),
            Some("KeyError: [value removed]")
        );
        assert_eq!(
            summary_line("Binder Error: in select id, 'sk_live' from t", 200).as_deref(),
            Some("Binder Error: in [SQL removed]")
        );
        assert_eq!(
            summary_line("SELECT secret FROM t", 200).as_deref(),
            Some("[SQL removed]")
        );
        assert_eq!(
            summary_line("could not update the relation", 200).as_deref(),
            Some("could not update the relation")
        );
        assert_eq!(summary_line("abcdef", 4).as_deref(), Some("abc…"));
        assert_eq!(summary_line("a\u{1b}[31mb", 200).as_deref(), Some("a[31mb"));
        assert_eq!(summary_line("  \n ", 200), None);
        // Lowercasing that changes lengths still cuts: everything goes.
        assert_eq!(
            summary_line("İ select 'x'", 200).as_deref(),
            Some("[SQL removed]")
        );
    }
}
