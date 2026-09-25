//! Formatting-insensitive SQL for fingerprints (#209).
//!
//! Compiled SQL is split into tokens and rejoined with single spaces, so comments,
//! whitespace and the case of reserved keywords don't change the fingerprint. Anything
//! that could mean different things in different dialects makes normalisation give up,
//! and the raw text is hashed instead (AGENTS rule 3). It gives up on:
//! - a backslash in a string or quoted identifier (escape rules differ by dialect);
//! - `$` outside a literal (dollar quoting, positional parameters);
//! - `#` outside a literal (a comment in some dialects, an operator in others);
//! - a block comment inside a block comment (nesting differs by dialect);
//! - an unterminated string, identifier or comment.
//!
//! Optimizer hints (`/*+ … */`, `--+ …`) are kept as tokens: they change how a query
//! runs. Identifiers keep their case: some warehouses treat quoted or even unquoted
//! names case-sensitively.

/// Reserved words that no mainstream dialect accepts as an unquoted identifier, so
/// their case never matters.
const RESERVED: [&str; 52] = [
    "all",
    "and",
    "any",
    "as",
    "asc",
    "between",
    "both",
    "by",
    "case",
    "cast",
    "cross",
    "desc",
    "distinct",
    "else",
    "end",
    "except",
    "exists",
    "false",
    "from",
    "full",
    "group",
    "having",
    "in",
    "inner",
    "intersect",
    "into",
    "is",
    "join",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "on",
    "or",
    "order",
    "outer",
    "over",
    "partition",
    "right",
    "select",
    "some",
    "table",
    "then",
    "trailing",
    "true",
    "union",
    "using",
    "when",
    "where",
];

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Characters that start or continue an operator token.
fn is_operator_char(c: char) -> bool {
    !c.is_whitespace()
        && !is_word_char(c)
        && !matches!(c, '(' | ')' | ',' | ';' | '\'' | '"' | '`' | '$' | '#')
}

/// The normalised form of `sql`, or `None` if it can't be normalised safely.
pub fn normalize_sql(sql: &str) -> Option<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && next == Some('-') {
            let end = chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |p| i + p);
            if chars.get(i + 2) == Some(&'+') {
                let hint: String = chars[i..end].iter().collect();
                tokens.push(hint.trim_end().to_owned());
            }
            i = end;
        } else if c == '/' && next == Some('*') {
            let body_start = i + 2;
            let close = (body_start..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/')?;
            let body: String = chars[body_start..close].iter().collect();
            if body.contains("/*") {
                return None;
            }
            if body.starts_with('+') {
                tokens.push(format!("/*{body}*/"));
            }
            i = close + 2;
        } else if matches!(c, '\'' | '"' | '`') {
            // A doubled quote is an escaped quote in every dialect.
            let mut j = i + 1;
            loop {
                match chars.get(j) {
                    // Unterminated, or a backslash escape (its rules differ by dialect).
                    None | Some('\\') => return None,
                    Some(&q) if q == c && chars.get(j + 1) == Some(&c) => j += 2,
                    Some(&q) if q == c => break,
                    Some(_) => j += 1,
                }
            }
            tokens.push(chars[i..=j].iter().collect());
            i = j + 1;
        } else if matches!(c, '$' | '#') {
            return None;
        } else if matches!(c, '(' | ')' | ',' | ';') {
            tokens.push(c.to_string());
            i += 1;
        } else if is_word_char(c) || (c == '.' && next.is_some_and(|n| n.is_ascii_digit())) {
            let start = i;
            let number = c.is_ascii_digit() || c == '.';
            i += 1;
            while let Some(&d) = chars.get(i) {
                let exponent_sign = number
                    && matches!(d, '+' | '-')
                    && matches!(chars[i - 1], 'e' | 'E')
                    && chars.get(i + 1).is_some_and(char::is_ascii_digit);
                if is_word_char(d) || (number && d == '.') || exponent_sign {
                    i += 1;
                } else {
                    break;
                }
            }
            let word: String = chars[start..i].iter().collect();
            let lower = word.to_ascii_lowercase();
            tokens.push(if RESERVED.contains(&lower.as_str()) {
                lower
            } else {
                word
            });
        } else {
            // An operator: the longest run of operator characters, stopping before a
            // comment.
            let start = i;
            while let Some(&d) = chars.get(i) {
                let comment = (d == '-' && chars.get(i + 1) == Some(&'-'))
                    || (d == '/' && chars.get(i + 1) == Some(&'*'));
                if !is_operator_char(d) || (i > start && comment) {
                    break;
                }
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect());
        }
    }
    Some(tokens.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(a: &str, b: &str) {
        assert_eq!(normalize_sql(a), normalize_sql(b), "{a:?} vs {b:?}");
        assert!(normalize_sql(a).is_some());
    }

    fn differ(a: &str, b: &str) {
        assert_ne!(normalize_sql(a), normalize_sql(b), "{a:?} vs {b:?}");
    }

    #[test]
    fn formatting_is_ignored() {
        same(
            "select id, amount from orders where amount > 0",
            "SELECT\n    id,\n    amount -- in cents\nFROM orders\n/* only paid */\nWHERE amount > 0\n",
        );
        same("select a+b from t", "select a + b from t");
        same("select count(*) from t", "select count( * )\tfrom t");
        same("select 1.5e+3, .5 from t", "select 1.5e+3 ,  .5 from t");
    }

    #[test]
    fn meaning_is_kept() {
        differ("select a from t", "select b from t");
        differ("select 'a b' from t", "select 'a  b' from t");
        differ("select \"My Col\" from t", "select \"my col\" from t");
        differ("select Orders.id from t", "select orders.id from t");
        differ("select 1 from t", "select 2 from t");
        differ(
            "select a from t where x > 1",
            "select a from t where x >= 1",
        );
        differ("select a - -1 from t", "select a --1 from t");
        differ("select 'a''b' from t", "select 'a' 'b' from t");
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
        differ("select a --+ index(t)\nfrom t", "select a\nfrom t");
    }

    #[test]
    fn dialect_dependent_input_is_not_normalised() {
        assert_eq!(normalize_sql(r"select 'it\'s' from t"), None);
        assert_eq!(normalize_sql("select $$body$$"), None);
        assert_eq!(normalize_sql("select $1 from @stage"), None);
        assert_eq!(normalize_sql("select a # b from t"), None);
        assert_eq!(normalize_sql("select /* a /* b */ */ 1"), None);
        assert_eq!(normalize_sql("select 'open"), None);
        assert_eq!(normalize_sql("select 1 /* open"), None);
    }
}
