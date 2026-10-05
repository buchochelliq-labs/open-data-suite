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
///   apostrophe in `can't` or `column's` opens nothing, unless it starts a prefixed
///   literal (`X'1F'`, `r'…'`) or the word after it runs into another `'`;
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

/// Whether the `'` at `at`, right after a word, opens a literal rather than being an
/// apostrophe: the word is a string prefix (`X'1F'`, `r'…'`, `E'…'`), or the word after
/// the quote runs straight into another `'` (`v'secret'`), which no apostrophe does.
fn prefixed(chars: &[char], at: usize) -> bool {
    const PREFIXES: [&str; 8] = ["b", "br", "e", "n", "r", "rb", "u", "x"];
    let start = chars[..at]
        .iter()
        .rposition(|c| !is_word(*c))
        .map_or(0, |p| p + 1);
    let word: String = chars[start..at].iter().collect();
    if PREFIXES.contains(&word.to_lowercase().as_str()) {
        return true;
    }
    let after = chars[at + 1..]
        .iter()
        .position(|c| !is_word(*c))
        .map_or(chars.len(), |p| at + 1 + p);
    after > at + 1 && chars.get(after) == Some(&'\'')
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
        let apostrophe = c == '\'' && i > 0 && is_word(chars[i - 1]) && !prefixed(&chars, i);
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

/// Word patterns that start SQL. A message is cut where one starts, since what follows
/// is SQL, which can hold resolved values. Each element is matched in order, separated
/// by whitespace:
/// - a word matches itself, whole and case-insensitively;
/// - `*` matches any one token (an identifier, possibly quoted or dotted);
/// - `*(` matches a token directly followed by `(`;
/// - a word ending in `(` matches that word directly followed by `(`.
///
/// The last element is followed by whitespace, `(`, `;` or the end. Words that are
/// ordinary English (`update`, `create`, `from`, `set`, `join`) count only in the shape
/// of a statement (`update <x> set`, `create table`, `left join`), so "could not update
/// the relation" isn't cut. Statements that carry no values (`use`, `show`,
/// `describe`, `commit`) aren't listed.
const SQL_STARTS: &[&[&str]] = &[
    &["select"],
    &["insert"],
    &["upsert"],
    &["truncate"],
    &["grant"],
    &["revoke"],
    &["unload"],
    &["where"],
    &["having"],
    &["cast("],
    &["values("],
    &["values", "("],
    &["merge", "into"],
    &["group", "by"],
    &["order", "by"],
    &["partition", "by"],
    &["with", "recursive"],
    &["with", "*", "as"],
    &["update", "*", "set"],
    &["delete", "from"],
    &["replace", "into"],
    &["replace", "table"],
    &["rename", "table"],
    &["rename", "column"],
    &["copy", "into"],
    &["copy", "*", "from"],
    &["call", "*("],
    &["execute", "immediate"],
    &["declare", "*"],
    &["begin", "transaction"],
    &["left", "join"],
    &["right", "join"],
    &["inner", "join"],
    &["outer", "join"],
    &["cross", "join"],
    &["full", "join"],
    &["join", "*", "on"],
    &["create", "or", "replace"],
    &["create", "*", "table"],
    &["create", "table"],
    &["create", "view"],
    &["create", "schema"],
    &["create", "database"],
    &["create", "index"],
    &["create", "function"],
    &["create", "procedure"],
    &["create", "stage"],
    &["create", "sequence"],
    &["create", "user"],
    &["create", "role"],
    &["create", "temp"],
    &["create", "temporary"],
    &["create", "transient"],
    &["create", "materialized"],
    &["create", "external"],
    &["create", "if"],
    &["alter", "table"],
    &["alter", "view"],
    &["alter", "schema"],
    &["alter", "database"],
    &["alter", "user"],
    &["alter", "role"],
    &["alter", "session"],
    &["alter", "system"],
    &["drop", "table"],
    &["drop", "view"],
    &["drop", "schema"],
    &["drop", "database"],
    &["drop", "index"],
    &["drop", "function"],
    &["drop", "user"],
    &["drop", "role"],
    &["drop", "if"],
    &["set", "session"],
    &["set", "role"],
];

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'"' | b'`' | b'$' | b'[' | b']')
}

/// Whether `pattern` matches at byte `at` of `lower`; if so, where it ends.
fn match_at(lower: &str, at: usize, pattern: &[&str]) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut pos = at;
    for (i, element) in pattern.iter().enumerate() {
        if i > 0 {
            let gap = bytes[pos..]
                .iter()
                .take_while(|b| b.is_ascii_whitespace())
                .count();
            if gap == 0 && !element.starts_with('(') {
                return None;
            }
            pos += gap;
        }
        match *element {
            "*" | "*(" => {
                let len = bytes[pos..]
                    .iter()
                    .take_while(|b| is_token_char(**b))
                    .count();
                if len == 0 {
                    return None;
                }
                pos += len;
                if *element == "*(" {
                    if bytes.get(pos) != Some(&b'(') {
                        return None;
                    }
                    pos += 1;
                }
            }
            word => {
                if !lower[pos..].starts_with(word) {
                    return None;
                }
                pos += word.len();
                // A whole word: not followed by more of one (unless it ends in `(`).
                if !word.ends_with('(')
                    && bytes
                        .get(pos)
                        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    return None;
                }
            }
        }
    }
    let last = pattern.last().copied().unwrap_or_default();
    let ends = last.ends_with('(')
        || bytes
            .get(pos)
            .is_none_or(|b| b.is_ascii_whitespace() || matches!(b, b'(' | b';'));
    // A text that is only a statement word and a count (an adapter's status,
    // `INSERT`, `SELECT 5`) carries no value to remove.
    let only_status = at == 0
        && lower[pos..]
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_whitespace() || b == b';');
    (ends && !only_status).then_some(pos)
}

/// Where the first SQL start is in `lower` (ASCII-lowercased), in bytes.
fn sql_start(lower: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    (0..bytes.len()).find(|&at| {
        let starts_word =
            at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        starts_word
            && bytes[at].is_ascii_alphabetic()
            && SQL_STARTS.iter().any(|p| match_at(lower, at, p).is_some())
    })
}

/// Cuts `line` where SQL starts. Returns the text before it, and whether it was cut.
fn cut_sql(line: &str) -> (&str, bool) {
    // ASCII lowercasing keeps byte offsets, so the cut lands in `line` exactly.
    match sql_start(&line.to_ascii_lowercase()) {
        Some(at) => (line[..at].trim_end(), true),
        None => (line, false),
    }
}

/// Terminal escape sequences (`ESC [ … m` and other `ESC`-led ones) and control
/// characters become spaces, before anything is looked for: colour codes around a
/// keyword (`\x1b[1mUPDATE\x1b[0m`) must not hide it.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                // CSI: parameters and intermediates up to a final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.peek() == Some(&'\\')) {
                            if c == '\u{1b}' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
            out.push(' ');
        } else if c.is_control() {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// Replaces the unquoted value after an assignment (`=`, `:=`, `=>`, `==`, `!=`, `<=`,
/// `>=`) by [`REMOVED`]: a resolved secret can appear unquoted (`token = sk_live_…`).
/// The value runs to the next whitespace, `,`, `;` or `)`.
fn assignments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        out.push(c);
        i += 1;
        if c != '=' {
            continue;
        }
        while i < chars.len() && matches!(chars[i], '=' | '>') {
            out.push(chars[i]);
            i += 1;
        }
        let mut j = i;
        while j < chars.len() && chars[j] == ' ' {
            j += 1;
        }
        let value_end = (j..chars.len())
            .find(|&k| chars[k].is_whitespace() || matches!(chars[k], ',' | ';' | ')'))
            .unwrap_or(chars.len());
        let value: String = chars[j..value_end].iter().collect();
        if value.is_empty() || value.starts_with(REMOVED) || value.starts_with('[') {
            continue;
        }
        out.extend(&chars[i..j]);
        out.push_str(REMOVED);
        i = value_end;
    }
    out
}

fn finish(mut clean: String, sql: bool, max_chars: usize) -> String {
    clean.retain(|c| !c.is_control());
    let clean_trimmed = clean.trim().to_owned();
    clean = clean_trimmed;
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

/// One line for people from an engine's message: its first non-blank line, with
/// terminal escapes and control characters made spaces, cut where SQL starts, with
/// [`literals`] and unquoted assigned values removed, and at most `max_chars`
/// characters (ending in `…` when cut). `None` if the message has no text.
pub fn summary_line(text: &str, max_chars: usize) -> Option<String> {
    let line = text
        .lines()
        .map(|l| plain(l).trim().to_owned())
        .find(|l| !l.is_empty())?;
    let (line, sql) = cut_sql(&line);
    Some(finish(numbers(&assignments(&spans(line))), sql, max_chars))
}

/// A value an engine reported (e.g. a query id or a status code), safe to keep: its
/// first line, with terminal escapes and control characters made spaces, cut where SQL
/// starts, with quoted spans and unquoted assigned values removed (numbers are kept:
/// they are what such values are made of), and at most `max_chars` characters. `None`
/// if it has no text.
pub fn value_line(text: &str, max_chars: usize) -> Option<String> {
    let line = plain(text.lines().next()?);
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let (line, sql) = cut_sql(line);
    Some(finish(assignments(&spans(line)), sql, max_chars))
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
            // Prefixed literals, and a value glued to a word (#192).
            ("cannot cast X'5345435245' to INT", "5345435245"),
            ("bad pattern r'a secret' here", "secret"),
            ("value v'hunter4' rejected", "hunter4"),
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
        assert_eq!(
            literals("can't cast X'1F' to INT"),
            "can't cast X[value removed] to INT"
        );
    }

    /// Property: whatever sits inside a quoted span of the input, for any mix of
    /// apostrophe words, quote kinds, backslashes and closings, never appears in the
    /// output.
    #[test]
    fn nothing_inside_a_quoted_span_survives() {
        // Unquoted, in SQL or after an assignment, with or without colour codes.
        for input in [
            "\u{1b}[1mUPDATE\u{1b}[0m accounts SET token = SECRET_Zq9",
            "\u{1b}[31mselect\u{1b}[0m SECRET_Zq9",
            "x = SECRET_Zq9",
        ] {
            let out = summary_line(input, 200).unwrap();
            assert!(!out.contains("SECRET"), "{input:?} => {out:?}");
        }
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
        assert_eq!(
            summary_line("x inserted 2 files\n", 200).as_deref(),
            Some("x inserted [value removed] files")
        );
        assert_eq!(
            summary_line("LINE 35: where cast(secret_col as integer) = ssn_col", 200).as_deref(),
            Some("LINE [value removed]: [SQL removed]")
        );
        assert_eq!(
            summary_line("could not update the relation", 200).as_deref(),
            Some("could not update the relation")
        );
        assert_eq!(summary_line("abcdef", 4).as_deref(), Some("abc…"));
        assert_eq!(summary_line("a\u{1b}[31mb", 200).as_deref(), Some("a b"));
        assert_eq!(summary_line("  \n ", 200), None);
        // Letters whose lowercase is longer (İ) or not ASCII (the Kelvin sign) can't
        // shift the cut.
        let out = summary_line("İİİİİİ select secret_col from t \u{212A}\u{212A}", 200).unwrap();
        assert_eq!(out, "İİİİİİ [SQL removed]");
        assert!(!out.contains("secret_col"));
    }

    /// Codex review on #326: statements outside the old keyword list, colour codes
    /// around a keyword, and values assigned without quotes.
    #[test]
    fn any_statement_start_and_unquoted_assignments_are_cut() {
        for input in [
            "Error: UPDATE accounts SET token = sk_live_SECRET",
            "Error: \u{1b}[1mUPDATE\u{1b}[0m accounts SET token = sk_live_SECRET",
            "failed: TRUNCATE TABLE sk_live_SECRET",
            "failed: truncate\tsk_live_SECRET",
            "failed: GRANT SELECT ON sk_live_SECRET TO x",
            "failed: REVOKE ALL ON sk_live_SECRET FROM y",
            "failed: DELETE\nFROM sk_live_SECRET",
            "bad: SELECT\u{7}sk_live_SECRET FROM t",
            "bad: WITH cte AS (select sk_live_SECRET)",
            "bad: MERGE INTO t USING sk_live_SECRET",
            "bad: COPY INTO t FROM sk_live_SECRET",
            "bad: CALL proc(sk_live_SECRET)",
            "bad: CREATE OR REPLACE TABLE sk_live_SECRET",
            "bad: DROP TABLE IF EXISTS sk_live_SECRET",
            "bad: x LEFT JOIN sk_live_SECRET",
            "bad: VALUES (sk_live_SECRET)",
            "token = sk_live_SECRET",
            "token:=sk_live_SECRET and more",
            "key => sk_live_SECRET, other",
            "password=sk_live_SECRET",
        ] {
            let out = summary_line(input, 200).unwrap();
            assert!(!out.contains("SECRET"), "{input:?} => {out:?}");
            let value = value_line(input, 200).unwrap();
            assert!(!value.contains("SECRET"), "{input:?} => {value:?}");
        }
        assert_eq!(
            summary_line(
                "Error: \u{1b}[1mUPDATE\u{1b}[0m accounts SET token = sk_live_SECRET",
                200
            )
            .as_deref(),
            Some("Error: [SQL removed]")
        );
        assert_eq!(
            summary_line("token = sk_live_SECRET", 200).as_deref(),
            Some("token = [value removed]")
        );
        // Ordinary English with those words isn't cut.
        for fine in [
            "could not update the relation",
            "failed to create the target directory",
            "the column was dropped from the source",
            "set up the profile and try again",
            "copy the file first",
        ] {
            assert_eq!(summary_line(fine, 200).as_deref(), Some(fine), "{fine}");
        }
    }

    /// Adapters' statuses are a statement word and a count: nothing to remove.
    #[test]
    fn a_statement_status_is_kept() {
        for status in ["INSERT", "INSERT 0 6", "SELECT 5", "CREATE TABLE", "MERGE"] {
            assert_eq!(value_line(status, 100).as_deref(), Some(status), "{status}");
        }
        assert_eq!(
            value_line("UPDATE accounts SET token = x", 100).as_deref(),
            Some("[SQL removed]")
        );
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
