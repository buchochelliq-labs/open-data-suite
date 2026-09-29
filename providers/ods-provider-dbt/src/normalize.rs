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
//!   (a Snowflake comment), a block comment starting with anything but whitespace,
//!   `*`, `-`, `=` or a `+` hint (`/*!` and `/*M!` run in MySQL and MariaDB), nested
//!   block comments, and a `\r` not followed by `\n` (MySQL doesn't end a comment
//!   there);
//! - two literals separated only by whitespace (whether a newline joins them depends
//!   on the dialect);
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
    // A lone `\r` ends a line comment in some dialects and not in others (MySQL).
    if (0..chars.len()).any(|i| chars[i] == '\r' && at(i + 1) != Some('\n')) {
        return None;
    }
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
            // Only plain comments and hints: `/*!` and `/*M!` run in MySQL and MariaDB.
            let plain = body
                .chars()
                .next()
                .is_none_or(|c| is_space(c) || matches!(c, '*' | '+' | '-' | '='));
            if body.contains("/*") || !plain {
                return None;
            }
            // A comment is whitespace, and `.` next to whitespace is refused.
            if (i > 0 && chars[i - 1] == '.') || at(close + 2) == Some('.') {
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
            // `'a'\n'b'` is one literal in Postgres and `'a' 'b'` an error: only the
            // whitespace between them decides.
            if tokens.last().is_some_and(|t| t.starts_with(is_quote)) {
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

/// Jinja calls that only render SQL or read settings: what they do is in the compiled
/// SQL or the config.
pub const PURE_CALLS: [&str; 6] = [
    "ref",
    "source",
    "config",
    "var",
    "env_var",
    "is_incremental",
];

/// Jinja tags that only choose or repeat text.
pub const PURE_TAGS: [&str; 8] = [
    "if", "elif", "else", "endif", "for", "endfor", "set", "endset",
];

/// Jinja words before which `(` only groups.
const GROUPING_KEYWORDS: [&str; 8] = ["if", "elif", "else", "and", "or", "not", "in", "is"];

/// A token inside a Jinja tag or expression.
enum JinjaToken {
    /// A name; `dotted` if it follows `.`.
    Word { word: String, dotted: bool },
    /// A string literal.
    Str,
    /// Any other character.
    Punct(char),
}

/// The name a token holds, if it is one.
fn word(token: &JinjaToken) -> Option<&str> {
    match token {
        JinjaToken::Word { word, .. } => Some(word.as_str()),
        _ => None,
    }
}

/// Whether one tag or expression body is pure (see [`jinja_is_pure`]).
fn body_is_pure(tokens: &[JinjaToken], tag: bool) -> bool {
    use JinjaToken::{Punct, Str, Word};
    let first = tokens.first().and_then(word);
    if tag && !first.is_some_and(|w| PURE_TAGS.contains(&w)) {
        return false;
    }
    // `{% set ref = run_query %}` and `{% for var in [run_query] %}` would make a pure
    // name impure.
    if matches!(first, Some("set" | "for")) {
        let rebinds = tokens[1..]
            .iter()
            .take_while(|t| !matches!(t, Punct('=')) && word(t) != Some("in"))
            .filter_map(word)
            .any(|w| PURE_CALLS.contains(&w));
        if rebinds {
            return false;
        }
    }
    for (k, token) in tokens.iter().enumerate() {
        if word(token) == Some("adapter") {
            return false;
        }
        if !matches!(token, Punct('(')) {
            continue;
        }
        // A call is only allowed on a bare pure name; `(` after `)`, `]`, a string or
        // any other name calls something else (`(run_query)(…)`, `dbt['x'](…)`).
        let allowed = match k.checked_sub(1).map(|p| &tokens[p]) {
            None => true,
            Some(Word { word, dotted }) => {
                !dotted
                    && (PURE_CALLS.contains(&word.as_str())
                        || GROUPING_KEYWORDS.contains(&word.as_str()))
            }
            Some(Punct(c)) => !matches!(c, ')' | ']'),
            Some(Str) => false,
        };
        if !allowed {
            return false;
        }
    }
    true
}

/// Whether all of a model's Jinja only shapes its SQL, so the compiled SQL shows
/// everything a build runs (#209). It is an allow-list: `{{ }}` and `{% %}` may only
/// use the [`PURE_TAGS`], call the [`PURE_CALLS`] by name and read names. Anything
/// else could run SQL the compiled code doesn't show, and editing its arguments
/// changes what a build does. That includes:
/// - a user or package macro (`{{ grant_select('x') }}`);
/// - `{% do %}` and `{% call %}`;
/// - `run_query` or `adapter.drop_relation(...)`;
/// - an indirect call (`(run_query)(…)`, `dbt['x'](…)`);
/// - rebinding a pure name (`{% set ref = run_query %}`).
pub fn jinja_is_pure(raw: &str) -> bool {
    let chars: Vec<char> = raw.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '{' {
            i += 1;
            continue;
        }
        let close = match at(i + 1) {
            Some('{') => '}',
            Some('%') => '%',
            Some('#') => '#',
            _ => {
                i += 1;
                continue;
            }
        };
        let mut j = i + 2;
        let mut tokens: Vec<JinjaToken> = Vec::new();
        loop {
            let Some(c) = at(j) else {
                return false; // unterminated
            };
            if c == close && at(j + 1) == Some('}') {
                break;
            }
            if close == '#' || c.is_whitespace() {
                j += 1;
            } else if c == '\'' || c == '"' {
                j += 1;
                loop {
                    match at(j) {
                        None => return false,
                        Some('\\') => j += 2,
                        Some(q) if q == c => break,
                        Some(_) => j += 1,
                    }
                }
                j += 1;
                tokens.push(JinjaToken::Str);
            } else if is_word_char(c) {
                let start = j;
                while at(j).is_some_and(is_word_char) {
                    j += 1;
                }
                tokens.push(JinjaToken::Word {
                    word: chars[start..j].iter().collect(),
                    dotted: matches!(tokens.last(), Some(JinjaToken::Punct('.'))),
                });
            } else {
                // Whitespace control (`{%-`, `-%}`) and operators.
                if !(matches!(c, '-' | '+') && (j == i + 2 || at(j + 1) == Some(close))) {
                    tokens.push(JinjaToken::Punct(c));
                }
                j += 1;
            }
        }
        if close != '#' && !body_is_pure(&tokens, close == '%') {
            return false;
        }
        i = j + 2;
    }
    true
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
        differ("select 'a'\n'b' from t", "select 'a' 'b' from t");
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
            // Second review: a lone `\r`, MariaDB executable comments, adjacent literals.
            "select 1 -- x\r+ 1\nfrom dual",
            "select 1 /*M! + 1 */ from t",
            "select 'a'\n'b' from t",
            "select t/**/.a from t",
            "select t./**/a from t",
        ] {
            assert_eq!(normalize_sql(sql), None, "{sql:?}");
        }
        // Still fine inside literals and comments.
        assert!(normalize_sql("select 'déjà [vu] $1 #2' from t -- ¿qué?").is_some());
    }

    #[test]
    fn only_pure_jinja_lets_the_file_go() {
        for pure in [
            "select 1",
            "{{ config(materialized='table', post_hook='grant select') }}\nselect * from {{ ref('a') }}",
            "{% if is_incremental() %}where x > (select max(x) from {{ this }}){% endif %}",
            "{%- set cols = ['a', 'b'] -%}\nselect {% for c in cols %}{{ c }}{% endfor %} from {{ source('s', 't') }}",
            "{# {% do run_query('x') %} is only a comment #}\nselect {{ var('v', 1) }}",
            "select '{' as brace, a from t",
            "select {{ \"%}\" }} as s",
            "{% if (is_incremental() and var('full', false)) or not (x in [1, 2]) %}1{% endif %}",
            "{%- set n = (var('n', 3) + 1) -%}select {{ n }}",
        ] {
            assert!(jinja_is_pure(pure), "{pure:?}");
        }
        for impure in [
            "{% do run_query('delete from audit') %}\nselect 1",
            "{%- do log('x') -%}\nselect 1",
            "{% call statement('x', fetch_result=True) %}select 1{% endcall %}",
            "{% set r = run_query('select 1') %}",
            "{{ adapter.execute('grant ...') }}",
            // User and package macros, and adapter methods (#209 review).
            "{{ grant_select('reporter') }}\nselect 1",
            "{%- set _ = cleanup('audit_2024') %}\nselect 1",
            "{{ adapter.drop_relation(api.Relation.create(schema='s', identifier='old')) }}\nselect 1",
            "{{ adapter.truncate_relation(this) }}\nselect 1",
            "select {{ dbt_utils.star(ref('a')) }}",
            // A closing delimiter inside a string doesn't end the tag.
            "{% set x = \"%}\" ~ run_query('delete from a') %}\nselect 1",
            "select {{ unterminated",
            "{% macro m() %}{% endmacro %}",
            // Third review: indirect calls and rebinding a pure name.
            "{{ (run_query)('delete from audit where d < 2024') }}",
            "{{ [run_query][0]('delete from audit') }}",
            "{{ dbt['truncate_relation'](this) }}",
            "{% set ref = run_query %}{{ ref('delete from audit') }}",
            "{% for var in [run_query] %}{{ var('delete from audit') }}{% endfor %}",
            "{{ this.incorporate(path={'schema': 'x'}) }}",
        ] {
            assert!(!jinja_is_pure(impure), "{impure:?}");
        }
    }
}
