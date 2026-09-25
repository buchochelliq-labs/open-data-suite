//! Formatting-insensitive SQL for fingerprints (#209).
//!
//! Compiled SQL is split into tokens and rejoined with single spaces, so comments and
//! whitespace don't change the fingerprint. Nothing else is changed: identifiers,
//! keywords and literals keep their case and spelling, because some warehouses treat
//! even keywords-as-identifiers case-sensitively.
//!
//! It only accepts what means the same in every mainstream dialect, and gives up on
//! anything else, so the raw text is hashed instead (AGENTS rule 3). Outside quotes and
//! comments that means ASCII letters, digits, `_`, ASCII whitespace, `( ) , ;` and the
//! operator characters `= < > + - * / % | : ! .`. It also gives up on:
//! - `[` (a T-SQL identifier or an array index), `$`, `#`, `@`, `?`, `{`, `&` and any
//!   non-ASCII character;
//! - a backslash in quotes, a triple quote, or a quote touching a word or another quote
//!   (`x'41'`, `N'a'`, `'a'"b"`: prefixed or adjacent literals mean different things by
//!   dialect);
//! - `--` not followed by whitespace (MySQL only treats `-- ` as a comment), `//`
//!   (a Snowflake comment), `/*!` (MySQL runs it), nested block comments;
//! - `.` next to whitespace;
//! - an unterminated quote or comment.
//!
//! Optimizer hints (`/*+ … */`) are kept as tokens: they change how a query runs.

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_quote(c: char) -> bool {
    matches!(c, '\'' | '"' | '`')
}

fn is_operator_char(c: char) -> bool {
    matches!(
        c,
        '=' | '<' | '>' | '+' | '-' | '*' | '/' | '%' | '|' | ':' | '!' | '.'
    )
}

/// The normalised form of `sql`, or `None` if it can't be normalised safely.
pub fn normalize_sql(sql: &str) -> Option<String> {
    let chars: Vec<char> = sql.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut tokens: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(c) = at(i) {
        let next = at(i + 1);
        if is_space(c) {
            i += 1;
        } else if c == '-' && next == Some('-') {
            // `--x` is `- -x` in MySQL, and `--+` an Oracle hint.
            if !at(i + 2).is_none_or(is_space) {
                return None;
            }
            i = (i..chars.len())
                .find(|&j| matches!(chars[j], '\n' | '\r'))
                .unwrap_or(chars.len());
        } else if c == '/' && next == Some('/') {
            return None;
        } else if c == '/' && next == Some('*') {
            let body_start = i + 2;
            let close = (body_start..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/')?;
            let body: String = chars[body_start..close].iter().collect();
            if body.contains("/*") || body.starts_with('!') {
                return None;
            }
            if body.starts_with('+') {
                tokens.push(format!("/*{body}*/"));
            }
            i = close + 2;
        } else if is_quote(c) {
            let touches = |c: Option<char>| c.is_some_and(|c| is_word_char(c) || is_quote(c));
            if (i > 0 && touches(at(i - 1))) || (next == Some(c) && at(i + 2) == Some(c)) {
                return None;
            }
            // A doubled quote inside the literal stays part of it: whatever it means in
            // a dialect, the text is kept exactly.
            let mut j = i + 1;
            loop {
                match at(j) {
                    // Unterminated, or a backslash escape (its rules differ by dialect).
                    None | Some('\\') => return None,
                    Some(q) if q == c && at(j + 1) == Some(c) => j += 2,
                    Some(q) if q == c => break,
                    Some(_) => j += 1,
                }
            }
            if touches(at(j + 1)) {
                return None;
            }
            tokens.push(chars[i..=j].iter().collect());
            i = j + 1;
        } else if matches!(c, '(' | ')' | ',' | ';') {
            tokens.push(c.to_string());
            i += 1;
        } else if is_word_char(c) || (c == '.' && next.is_some_and(|n| n.is_ascii_digit())) {
            let start = i;
            let number = c.is_ascii_digit() || c == '.';
            i += 1;
            while let Some(d) = at(i) {
                let exponent_sign = number
                    && matches!(d, '+' | '-')
                    && matches!(chars[i - 1], 'e' | 'E')
                    && at(i + 1).is_some_and(|n| n.is_ascii_digit());
                if is_word_char(d) || (number && d == '.') || exponent_sign {
                    i += 1;
                } else {
                    break;
                }
            }
            tokens.push(chars[start..i].iter().collect());
        } else if is_operator_char(c) {
            // The longest run of operator characters, stopping before a comment.
            let start = i;
            while let Some(d) = at(i) {
                let comment = matches!((d, at(i + 1)), ('-', Some('-')) | ('/', Some('*' | '/')));
                if !is_operator_char(d) || (i > start && comment) {
                    break;
                }
                // `a . b` and `a.b` aren't the same in every dialect.
                if d == '.' && (at(i + 1).is_none_or(is_space) || (i > 0 && is_space(chars[i - 1])))
                {
                    return None;
                }
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect());
        } else {
            return None;
        }
    }
    Some(tokens.join(" "))
}

/// Whether a model's Jinja can run SQL that its compiled code doesn't show (`{% do %}`,
/// `{% call statement %}`, `run_query`, `adapter.execute`). Such a model's file must
/// stay in its fingerprint: editing that code changes what a build does.
pub fn has_hidden_side_effects(raw: &str) -> bool {
    let mut rest = raw;
    while let Some(open) = rest.find(['{']) {
        let after = &rest[open + 1..];
        let close = match after.chars().next() {
            Some('%') => "%}",
            Some('{') => "}}",
            _ => {
                rest = after;
                continue;
            }
        };
        let body_start = &after[1..];
        let Some(end) = body_start.find(close) else {
            return true;
        };
        let body = &body_start[..end];
        let first = body
            .trim_start_matches(['-', '+'])
            .split(|c: char| !is_word_char(c))
            .find(|w| !w.is_empty());
        if (close == "%}" && matches!(first, Some("do" | "call")))
            || body.contains("run_query")
            || body.contains("statement")
            || body.contains("execute(")
            || body.contains("execute (")
        {
            return true;
        }
        rest = &body_start[end + close.len()..];
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(a: &str, b: &str) {
        assert!(normalize_sql(a).is_some(), "{a:?} isn't normalised");
        assert_eq!(normalize_sql(a), normalize_sql(b), "{a:?} vs {b:?}");
    }

    /// Different meaning: different results, or one of them isn't normalised.
    fn differ(a: &str, b: &str) {
        let (x, y) = (normalize_sql(a), normalize_sql(b));
        assert!(
            x.is_none() || y.is_none() || x != y,
            "{a:?} vs {b:?}: {x:?}"
        );
    }

    #[test]
    fn comments_and_whitespace_are_ignored() {
        same(
            "select id, amount from orders where amount > 0",
            "select\n    id,\n    amount -- in cents\nfrom orders\n/* only paid */\nwhere amount > 0\n",
        );
        same("select a+b from t", "select a + b from t");
        same("select count(*) from t", "select count( * )\tfrom t");
        same("select 1.5e+3, .5 from t", "select 1.5e+3 ,  .5 from t");
        same(
            "select a::int from s.t",
            "select a :: int\r\nfrom s.t -- x\r\n",
        );
        same(
            "select a from t where b != 1",
            "select a from t /* c */ where b != 1",
        );
    }

    #[test]
    fn meaning_is_kept() {
        differ("select a from t", "select b from t");
        differ("select 'a b' from t", "select 'a  b' from t");
        differ("select \"My Col\" from t", "select \"my col\" from t");
        differ("select Orders.id from t", "select orders.id from t");
        differ("select a from t", "select A from t");
        differ("select a from t", "SELECT a from t");
        differ("select 1 from t", "select 2 from t");
        differ(
            "select a from t where x > 1",
            "select a from t where x >= 1",
        );
        differ("select a - -1 from t", "select a -- 1\nfrom t");
        differ("select 'a''b' from t", "select 'a' 'b' from t");
        differ("select 1 as Both from t", "select 1 as both from t");
    }

    #[test]
    fn literals_that_look_like_comments_are_kept() {
        differ("select '-- x' from t", "select '' from t");
        differ("select '/* x */' from t", "select '' from t");
        same("select '-- x' from t -- y", "select '-- x' from t");
    }

    #[test]
    fn hints_are_kept() {
        differ(
            "select /*+ BROADCAST(o) */ * from o",
            "select /*+ MERGE(o) */ * from o",
        );
    }

    /// Every pair here once normalised to the same text (#209 review).
    #[test]
    fn dialect_traps_are_not_normalised() {
        for sql in [
            r"select 'it\'s' from t",
            "select $$body$$",
            "select $1 from @stage",
            "select a # b from t",
            "select /* a /* b */ */ 1",
            "select 'open",
            "select 1 /* open",
            // T-SQL bracketed identifiers.
            "select [My  Col] from t",
            "select [net--gross]\nfrom t",
            // Prefixed and adjacent literals.
            "select x'41' from t",
            "select N'abc' from t",
            "select 'a'\"b\" from t",
            // BigQuery triple-quoted strings.
            "select '''a' -- b''' as x\nfrom t",
            // MySQL: executable comments, `--` without a space.
            "select 1 /*! + 1 */ from t",
            "select 5--1\nfrom t",
            // Oracle `--+` hints.
            "select a --+ x\nfrom t",
            // Snowflake `//` comments.
            "select 1 // n /* z\n, 2 as x -- */\nfrom t",
            // Non-ASCII outside literals, e.g. a no-break space in an identifier.
            "select a\u{A0}b from t",
            // `.` next to whitespace.
            "select t . a from t",
        ] {
            assert_eq!(normalize_sql(sql), None, "{sql:?}");
        }
        // Still fine inside literals and comments.
        assert!(normalize_sql("select 'déjà [vu] $1 #2' from t -- ¿qué?").is_some());
    }

    #[test]
    fn jinja_that_runs_sql_is_detected() {
        assert!(has_hidden_side_effects(
            "{% do run_query('delete from audit') %}\nselect 1"
        ));
        assert!(has_hidden_side_effects("{%- do log('x') -%}\nselect 1"));
        assert!(has_hidden_side_effects(
            "{% call statement('x', fetch_result=True) %}select 1{% endcall %}"
        ));
        assert!(has_hidden_side_effects(
            "{% set r = run_query('select 1') %}"
        ));
        assert!(has_hidden_side_effects(
            "{{ adapter.execute('grant ...') }}"
        ));
        assert!(has_hidden_side_effects("select {{ unterminated"));
        assert!(!has_hidden_side_effects(
            "{{ config(materialized='table') }}\n{% if execute %}\nselect {{ ref('a') }}\n{% endif %}"
        ));
        assert!(!has_hidden_side_effects("select '{' as brace, a from t"));
    }
}
