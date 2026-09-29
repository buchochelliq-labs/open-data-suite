//! Removing values and SQL from text meant for people (AGENTS.md rule 9).
//!
//! Engines' messages quote the values and statements they failed on, and those can hold
//! anything a query resolved: `--vars` values, environment variables, credentials. So
//! any engine message ODS keeps, shows or serves passes through here first, e.g. a
//! failed node's error summary (#322) or a configuration error (ADR-0005). The rules
//! are deliberately blunt and fail closed: a removed identifier costs a little
//! context, a kept literal can leak a secret. When the quoting of a line can't be read
//! for sure, everything from its first quote on is removed. The full message stays
//! wherever the engine wrote it.

/// What a removed literal reads as.
pub const REMOVED: &str = "[value removed]";

/// What removed SQL reads as.
pub const SQL_REMOVED: &str = "[SQL removed]";

/// Replaces each span quoted with one of `quotes` by `placeholder`, for messages whose
/// quoting is regular (e.g. a deserializer's `"…"` with backslash escapes). Inside a
/// span a backslash escapes the next character and a doubled quote is part of it. A
/// span that isn't closed runs to the end of the text. For engines' free text, use
/// [`literals`], which fails closed.
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

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Replaces quoted spans (`'…'`, `"…"`, `` `…` ``) and dollar-quoted spans (`$$…$$`,
/// `$tag$…$tag$`) by [`REMOVED`], line by line, failing closed:
/// - a quote opens a span only when it doesn't follow a letter or digit, so the
///   apostrophe in `can't` or `column's` opens nothing;
/// - there are no escapes: a span ends at the next quote of its kind (a doubled quote,
///   `'it''s'`, stays inside), and a close right after a backslash can't be read;
/// - if a span doesn't close on its line, or its closing quote runs straight into a
///   letter or digit (`'a'b`), the quoting can't be read for sure, and everything from
///   the line's first quote to its end is removed.
fn spans(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&line_spans(line));
    }
    out
}

fn line_spans(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    // Where the line's first span opened: what can't be read is removed from there.
    let mut first_open: Option<usize> = None;
    let unreadable = |first_open: Option<usize>, at: usize| {
        let head: String = chars[..first_open.unwrap_or(at)].iter().collect();
        format!("{head}{REMOVED}")
    };
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Only `'` doubles as an apostrophe (`can't`, `users'`).
        let apostrophe = c == '\'' && i > 0 && is_word(chars[i - 1]);
        if c == '$' {
            // `$tag$`: a tag of letters, digits and `_`, possibly empty.
            let tag_end = chars[i + 1..]
                .iter()
                .position(|c| !is_word(*c))
                .map(|p| i + 1 + p);
            if let Some(end) = tag_end.filter(|&e| chars[e] == '$') {
                first_open.get_or_insert(i);
                let tag: Vec<char> = chars[i..=end].to_vec();
                let body_start = end + 1;
                let close = (body_start..=chars.len().saturating_sub(tag.len()))
                    .find(|&k| chars[k..k + tag.len()] == tag[..]);
                match close {
                    Some(k) => {
                        out.push_str(REMOVED);
                        i = k + tag.len();
                        continue;
                    }
                    None => return unreadable(first_open, i),
                }
            }
            out.push(c);
            i += 1;
            continue;
        }
        if !matches!(c, '\'' | '"' | '`') || apostrophe {
            out.push(c);
            i += 1;
            continue;
        }
        first_open.get_or_insert(i);
        // An opening quote: find its close, taking doubled quotes as inside.
        let mut k = i + 1;
        let close = loop {
            match chars.get(k) {
                None => break None,
                Some(&q) if q == c => {
                    if chars.get(k + 1) == Some(&c) {
                        k += 2;
                    } else {
                        break Some(k);
                    }
                }
                Some(_) => k += 1,
            }
        };
        match close {
            // A backslash before the close may have been meant as an escape: the span's
            // end can't be told for sure.
            Some(k)
                if chars[k - 1] != '\\'
                    && !chars.get(k + 1).is_some_and(|n| n.is_alphanumeric()) =>
            {
                out.push_str(REMOVED);
                i = k + 1;
            }
            _ => return unreadable(first_open, i),
        }
    }
    out
}

/// Replaces standalone numbers (`42`, `-3.5`, but not `INT32` or `v2`) by [`REMOVED`].
fn numbers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts_number =
            c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit));
        let after_word = i > 0 && (is_word(chars[i - 1]) || chars[i - 1] == '.');
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
        if end < chars.len() && is_word(chars[end]) {
            // Part of a word, e.g. `3rd` or `0xFF`.
            let mut word_end = end;
            while word_end < chars.len() && is_word(chars[word_end]) {
                word_end += 1;
            }
            let word: String = chars[i..word_end].iter().collect();
            // Ordinals (`3rd`, `21st`) are words; anything else (`0xFF`, `1e10`) may
            // be a value.
            let ordinal = ["st", "nd", "rd", "th"].iter().any(|s| {
                word.strip_suffix(s)
                    .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
            });
            if ordinal {
                out.push_str(&word);
            } else {
                out.push_str(REMOVED);
            }
            i = word_end;
        } else {
            out.push_str(REMOVED);
            out.extend(&chars[last..end]);
            i = end;
        }
    }
    out
}

/// Replaces literals by [`REMOVED`]: quoted and dollar-quoted spans (failing closed,
/// see the module documentation) and numbers that stand alone.
pub fn literals(text: &str) -> String {
    numbers(&spans(text))
}

/// Word sequences that start a SQL statement or clause. A message is cut where one
/// starts, since what follows is SQL. The words may be separated by any whitespace,
/// and the last is followed by whitespace, `(` or the end. Chosen so ordinary English
/// ("could not update the relation") isn't cut.
const SQL_STARTS: [&[&str]; 14] = [
    &["select"],
    &["insert", "into"],
    &["insert", "overwrite"],
    &["delete", "from"],
    &["merge", "into"],
    &["create", "or", "replace"],
    &["create", "table"],
    &["create", "view"],
    &["create", "temporary"],
    &["alter", "table"],
    &["drop", "table"],
    &["where"],
    &["cast("],
    &["with", "recursive"],
];

/// Where the first SQL start is in `lower` (ASCII-lowercased), in bytes.
fn sql_start(lower: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let starts_word = |at: usize| at == 0 || !is_word(char::from(bytes[at - 1]));
    let mut best: Option<usize> = None;
    for words in SQL_STARTS {
        for (at, _) in lower.match_indices(words[0]) {
            if !starts_word(at) || best.is_some_and(|b| b <= at) {
                continue;
            }
            let mut pos = at + words[0].len();
            let mut matched = true;
            for word in &words[1..] {
                let gap = lower[pos..]
                    .bytes()
                    .take_while(u8::is_ascii_whitespace)
                    .count();
                if gap == 0 || !lower[pos + gap..].starts_with(word) {
                    matched = false;
                    break;
                }
                pos += gap + word.len();
            }
            let ends = words.last().is_some_and(|w| w.ends_with('('))
                || lower[pos..]
                    .bytes()
                    .next()
                    .is_none_or(|b| b.is_ascii_whitespace() || b == b'(');
            if matched && ends {
                best = Some(at);
            }
        }
    }
    best
}

/// Cuts `line` where SQL starts. Returns the text before it, and whether it was cut.
fn cut_sql(line: &str) -> (&str, bool) {
    // ASCII lowercasing keeps byte offsets, so the cut lands in `line` exactly.
    match sql_start(&line.to_ascii_lowercase()) {
        Some(at) => (line[..at].trim_end(), true),
        None => (line, false),
    }
}

fn finish(mut clean: String, sql: bool, max_chars: usize) -> String {
    clean.retain(|c| !c.is_control());
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
    clean
}

/// One line for people from an engine's message: its first non-blank line, cut where a
/// SQL statement starts, with [`literals`] removed, control characters dropped and at
/// most `max_chars` characters (ending in `…` when cut). `None` if the message has no
/// text.
pub fn summary_line(text: &str, max_chars: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let (line, sql) = cut_sql(line);
    Some(finish(literals(line), sql, max_chars))
}

/// A value an engine reported (e.g. a query id or a status code), safe to keep: its
/// first line, cut where SQL starts, with quoted spans removed (numbers are kept: they
/// are what such values are made of), control characters dropped and at most
/// `max_chars` characters. `None` if it has no text.
pub fn value_line(text: &str, max_chars: usize) -> Option<String> {
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let (line, sql) = cut_sql(line);
    Some(finish(spans(line), sql, max_chars))
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

    /// The reviewers' probes (#322): apostrophes in words, backslashes, JSON, tags.
    #[test]
    fn apostrophes_in_words_never_let_a_value_through() {
        for (input, secret) in [
            ("Can't cast 'sk_live_abc123' to INT", "sk_live_abc123"),
            ("column's value 'hunter2' is invalid", "hunter2"),
            ("Couldn't find 'TOKEN_xyz' in scope 'x'", "TOKEN_xyz"),
            ("value \"it's\" then 'pw_secret'", "pw_secret"),
            ("Invalid value {{ var('api_key') }}", "api_key"),
            ("bad json {\"password\": \"hunter2\", \"n\": 42}", "hunter2"),
            ("Error: $tag$ secret_body $tag$ end", "secret_body"),
            ("Error: $tag$ secret_body never closed", "secret_body"),
            ("path 'C:\\' token=pw 'y'", "token=pw"),
            ("'it's a secret_word'", "secret_word"),
            ("O'Brien said 'hunter3'", "hunter3"),
        ] {
            let out = literals(input);
            assert!(!out.contains(secret), "{input:?} => {out:?}");
        }
        assert_eq!(
            literals("Can't cast 'sk_live_abc123' to INT"),
            "Can't cast [value removed] to INT"
        );
        assert_eq!(
            literals("Couldn't find 'TOKEN_xyz' in scope 'x'"),
            "Couldn't find [value removed] in scope [value removed]"
        );
        // A backslash before a close could be an escape: the rest can't be read.
        assert_eq!(literals("path 'C:\\' token=pw 'y'"), "path [value removed]");
        assert_eq!(literals("'a'b 'c'"), "[value removed]");
    }

    /// Property: whatever sits inside a quoted span of the input, for any mix of
    /// apostrophe words, quote kinds, backslashes and closings, never appears in the
    /// output.
    #[test]
    fn nothing_inside_a_quoted_span_survives() {
        let prefixes = [
            "",
            "Can't ",
            "column's ",
            "O'Brien's ",
            "it's \"",
            "x = ",
            "a\\",
            "(",
            "{{ var(",
            "rock 'n' roll ",
            "users' ",
            "`id` and ",
        ];
        let quotes = ['\'', '"', '`'];
        let suffixes = [
            "", " to INT", "s more", "' tail", "\\' x", " and 'y'", "\"", ")",
        ];
        for prefix in prefixes {
            for quote in quotes {
                for suffix in suffixes {
                    let input = format!("{prefix}{quote}SECRET_Zq9{quote}{suffix}");
                    let out = literals(&input);
                    assert!(!out.contains("SECRET"), "{input:?} => {out:?}");
                    assert!(!out.contains("Zq9"), "{input:?} => {out:?}");
                    let summary = summary_line(&input, 200).unwrap();
                    assert!(!summary.contains("SECRET"), "{input:?} => {summary:?}");
                    let value = value_line(&input, 200).unwrap();
                    assert!(!value.contains("SECRET"), "{input:?} => {value:?}");
                }
            }
        }
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
        assert_eq!(
            literals("error 0xDEADBEEF and 1e10"),
            "error [value removed] and [value removed]"
        );
        assert_eq!(literals("costs $5"), "costs $[value removed]");
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
            summary_line("Syntax error at SELECT\tsecret_col FROM t", 200).as_deref(),
            Some("Syntax error at [SQL removed]")
        );
        assert_eq!(
            summary_line("bad: SELECT(secret_col) FROM t", 200).as_deref(),
            Some("bad: [SQL removed]")
        );
        assert_eq!(summary_line("x INSERT\n", 200).as_deref(), Some("x INSERT"));
        assert_eq!(
            summary_line("LINE 35: where cast(secret_col as integer) = ssn_col", 200).as_deref(),
            Some("LINE [value removed]: [SQL removed]")
        );
        assert_eq!(
            summary_line("could not update the relation", 200).as_deref(),
            Some("could not update the relation")
        );
        assert_eq!(summary_line("abcdef", 4).as_deref(), Some("abc…"));
        assert_eq!(
            summary_line("a\u{1b}[31mb", 200).as_deref(),
            Some("a[[value removed]")
        );
        assert_eq!(summary_line("  \n ", 200), None);
        // Letters whose lowercase is longer (İ) or not ASCII (the Kelvin sign) can't
        // shift the cut.
        let out = summary_line("İİİİİİ select secret_col from t \u{212A}\u{212A}", 200).unwrap();
        assert_eq!(out, "İİİİİİ [SQL removed]");
        assert!(!out.contains("secret_col"));
    }

    #[test]
    fn a_value_line_keeps_numbers_and_drops_quotes_and_sql() {
        assert_eq!(value_line("01ef-2a 42", 100).as_deref(), Some("01ef-2a 42"));
        assert_eq!(
            value_line("OK 'secret'\nmore", 100).as_deref(),
            Some("OK [value removed]")
        );
        assert_eq!(
            value_line("rows: INSERT INTO t SELECT 1", 100).as_deref(),
            Some("rows: [SQL removed]")
        );
        assert_eq!(value_line("  ", 100), None);
    }
}
