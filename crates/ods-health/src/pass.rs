//! A probe's `pass` condition (ADR-0030 §3, §4d): comparisons of a column the probe
//! returns with a literal, combined with `and`/`or` and parentheses, e.g.
//! `n > 0 and late_rows = 0`. It is parsed when the configuration loads, so a mistake is
//! a configuration error, and evaluated on the probe's row with three values: a column
//! that is missing, or isn't a number where the comparison needs one, makes it *unknown*,
//! never a pass (AGENTS rule 3).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A parsed `pass` condition.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pass {
    Compare(Comparison),
    And(Box<Pass>, Box<Pass>),
    Or(Box<Pass>, Box<Pass>),
}

/// `column op literal`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Comparison {
    column: String,
    op: Op,
    literal: Literal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Literal {
    Number(f64),
    Text(String),
    Bool(bool),
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Text(t) => write!(f, "'{t}'"),
            Self::Bool(b) => write!(f, "{b}"),
        }
    }
}

/// What a condition concluded on a row.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "evaluated on a probe's row once probes run (#392)"
    )
)]
pub(crate) enum Verdict {
    Pass,
    /// It is false; why, for people.
    Fail(String),
    /// It can't be told; why.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Number(f64),
    Text(String),
    Op(Op),
    Open,
    Close,
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(at, c)) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                out.push(Token::Open);
            }
            ')' => {
                chars.next();
                out.push(Token::Close);
            }
            '=' | '!' | '<' | '>' => {
                chars.next();
                let next_eq = chars.peek().is_some_and(|&(_, n)| n == '=');
                let op = match (c, next_eq) {
                    ('=', _) => Op::Eq,
                    ('!', true) => Op::Ne,
                    ('<', true) => Op::Le,
                    ('>', true) => Op::Ge,
                    ('<', false) => Op::Lt,
                    ('>', false) => Op::Gt,
                    _ => return Err(format!("`!` at {at} must be `!=`")),
                };
                if next_eq && c != '=' {
                    chars.next();
                } else if c == '=' && next_eq {
                    // `==` reads as `=`.
                    chars.next();
                }
                out.push(Token::Op(op));
            }
            '\'' => {
                chars.next();
                let mut text = String::new();
                loop {
                    match chars.next() {
                        Some((_, '\'')) if chars.peek().is_some_and(|&(_, n)| n == '\'') => {
                            chars.next();
                            text.push('\'');
                        }
                        Some((_, '\'')) => break,
                        Some((_, c)) => text.push(c),
                        None => return Err(format!("the string at {at} isn't closed")),
                    }
                }
                out.push(Token::Text(text));
            }
            c if c.is_ascii_digit() || c == '-' || c == '.' => {
                let mut number = String::new();
                while let Some(&(_, d)) = chars.peek() {
                    if d.is_ascii_digit() || matches!(d, '.' | '-' | 'e' | 'E' | '+') {
                        number.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let value: f64 = number
                    .parse()
                    .map_err(|_| format!("`{number}` at {at} isn't a number"))?;
                if !value.is_finite() {
                    return Err(format!("`{number}` at {at} isn't a finite number"));
                }
                out.push(Token::Number(value));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while let Some(&(_, d)) = chars.peek() {
                    if d.is_ascii_alphanumeric() || d == '_' {
                        ident.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push(Token::Ident(ident));
            }
            other => return Err(format!("`{other}` at {at} isn't part of a condition")),
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn keyword(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Ident(w)) if w.eq_ignore_ascii_case(word))
    }

    fn or(&mut self) -> Result<Pass, String> {
        let mut left = self.and()?;
        while self.keyword("or") {
            self.at += 1;
            left = Pass::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Pass, String> {
        let mut left = self.atom()?;
        while self.keyword("and") {
            self.at += 1;
            left = Pass::And(Box::new(left), Box::new(self.atom()?));
        }
        Ok(left)
    }

    fn atom(&mut self) -> Result<Pass, String> {
        match self.next() {
            Some(Token::Open) => {
                let inner = self.or()?;
                match self.next() {
                    Some(Token::Close) => Ok(inner),
                    _ => Err("a `(` isn't closed".to_owned()),
                }
            }
            Some(Token::Ident(column))
                if !["and", "or", "true", "false"]
                    .iter()
                    .any(|w| column.eq_ignore_ascii_case(w)) =>
            {
                let Some(Token::Op(op)) = self.next() else {
                    return Err(format!(
                        "`{column}` must be compared: `=`, `!=`, `<`, `<=`, `>` or `>=`"
                    ));
                };
                let literal = match self.next() {
                    Some(Token::Number(n)) => Literal::Number(n),
                    Some(Token::Text(t)) => Literal::Text(t),
                    Some(Token::Ident(w)) if w.eq_ignore_ascii_case("true") => Literal::Bool(true),
                    Some(Token::Ident(w)) if w.eq_ignore_ascii_case("false") => {
                        Literal::Bool(false)
                    }
                    _ => {
                        return Err(format!(
                            "`{column} {}` must be followed by a number, a 'string', true or false",
                            op.symbol()
                        ));
                    }
                };
                if !matches!(literal, Literal::Number(_)) && !matches!(op, Op::Eq | Op::Ne) {
                    return Err(format!(
                        "`{column} {} {literal}`: only numbers can be ordered; use `=` or `!=`",
                        op.symbol()
                    ));
                }
                Ok(Pass::Compare(Comparison {
                    column: column.to_ascii_lowercase(),
                    op,
                    literal,
                }))
            }
            _ => Err("expected a column compared with a value, or `(`".to_owned()),
        }
    }
}

impl Pass {
    /// Parses `text`.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut parser = Parser {
            tokens: tokens(text)?,
            at: 0,
        };
        if parser.tokens.is_empty() {
            return Err("it is empty".to_owned());
        }
        let pass = parser.or()?;
        match parser.peek() {
            None => Ok(pass),
            Some(_) => Err(
                "something follows the condition: join conditions with `and` or `or`".to_owned(),
            ),
        }
    }

    /// The columns it reads, lowercased: what the probe must return.
    pub(crate) fn columns(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut BTreeSet<String>) {
        match self {
            Self::Compare(c) => {
                out.insert(c.column.clone());
            }
            Self::And(a, b) | Self::Or(a, b) => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    /// Its verdict on `row`, a column's value by lowercased name.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "evaluated on a probe's row once probes run (#392)"
        )
    )]
    pub(crate) fn evaluate(&self, row: &BTreeMap<String, String>) -> Verdict {
        match self {
            Self::Compare(c) => c.evaluate(row),
            Self::And(a, b) => match (a.evaluate(row), b.evaluate(row)) {
                (Verdict::Fail(why), _) | (_, Verdict::Fail(why)) => Verdict::Fail(why),
                (Verdict::Unknown(why), _) | (_, Verdict::Unknown(why)) => Verdict::Unknown(why),
                _ => Verdict::Pass,
            },
            Self::Or(a, b) => match (a.evaluate(row), b.evaluate(row)) {
                (Verdict::Pass, _) | (_, Verdict::Pass) => Verdict::Pass,
                (Verdict::Unknown(why), _) | (_, Verdict::Unknown(why)) => Verdict::Unknown(why),
                (Verdict::Fail(a), Verdict::Fail(b)) => Verdict::Fail(format!("{a}, and {b}")),
            },
        }
    }
}

impl Comparison {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "evaluated on a probe's row once probes run (#392)"
        )
    )]
    fn evaluate(&self, row: &BTreeMap<String, String>) -> Verdict {
        let Some(raw) = row.get(&self.column) else {
            return Verdict::Unknown(format!("the probe returned no `{}`", self.column));
        };
        let value = raw.trim();
        let holds = match &self.literal {
            Literal::Number(want) => {
                let Ok(got) = value.parse::<f64>() else {
                    return Verdict::Unknown(format!(
                        "`{}` is `{value}`, not a number",
                        self.column
                    ));
                };
                // Both are finite: a value that isn't parses as a number only if it is
                // `inf` or `NaN`, which nothing compares with.
                let Some(order) = got.partial_cmp(want).filter(|_| got.is_finite()) else {
                    return Verdict::Unknown(format!(
                        "`{}` is `{value}`, not a number",
                        self.column
                    ));
                };
                match self.op {
                    Op::Eq => order.is_eq(),
                    Op::Ne => order.is_ne(),
                    Op::Lt => order.is_lt(),
                    Op::Le => order.is_le(),
                    Op::Gt => order.is_gt(),
                    Op::Ge => order.is_ge(),
                }
            }
            Literal::Text(want) => (value == want) == (self.op == Op::Eq),
            Literal::Bool(want) => {
                let got = match value.to_ascii_lowercase().as_str() {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Verdict::Unknown(format!(
                            "`{}` is `{value}`, not true or false",
                            self.column
                        ));
                    }
                };
                (got == *want) == (self.op == Op::Eq)
            }
        };
        if holds {
            Verdict::Pass
        } else {
            Verdict::Fail(format!(
                "`{}` is {value}, not {} {}",
                self.column,
                self.op.symbol(),
                self.literal
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn comparisons_and_their_verdicts() {
        let pass = Pass::parse("n > 0").unwrap();
        assert_eq!(pass.columns(), BTreeSet::from(["n".to_owned()]));
        assert_eq!(pass.evaluate(&row(&[("n", "12")])), Verdict::Pass);
        assert_eq!(
            pass.evaluate(&row(&[("n", "0")])),
            Verdict::Fail("`n` is 0, not > 0".to_owned())
        );
        assert!(matches!(
            pass.evaluate(&row(&[("n", "None")])),
            Verdict::Unknown(_)
        ));
        assert!(matches!(pass.evaluate(&row(&[])), Verdict::Unknown(_)));
    }

    #[test]
    fn and_or_and_parentheses_with_three_values() {
        let pass = Pass::parse("(n >= 1 and late = 0) or status = 'skip'").unwrap();
        assert_eq!(
            pass.columns(),
            BTreeSet::from(["late".to_owned(), "n".to_owned(), "status".to_owned()])
        );
        assert_eq!(
            pass.evaluate(&row(&[("n", "3"), ("late", "0"), ("status", "ok")])),
            Verdict::Pass
        );
        assert_eq!(
            pass.evaluate(&row(&[("n", "3"), ("late", "2"), ("status", "skip")])),
            Verdict::Pass
        );
        assert!(matches!(
            pass.evaluate(&row(&[("n", "3"), ("late", "2"), ("status", "ok")])),
            Verdict::Fail(_)
        ));
        // An unknown never becomes a pass through `and`.
        assert!(matches!(
            Pass::parse("n > 0 and late = 0")
                .unwrap()
                .evaluate(&row(&[("n", "1"), ("late", "x")])),
            Verdict::Unknown(_)
        ));
        // A false `and` is false, whatever else is unknown.
        assert!(matches!(
            Pass::parse("n > 5 and late = 0")
                .unwrap()
                .evaluate(&row(&[("n", "1"), ("late", "x")])),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn strings_booleans_and_case() {
        let pass = Pass::parse("OK = TRUE AND Region != 'it''s'").unwrap();
        assert_eq!(
            pass.evaluate(&row(&[("ok", "true"), ("region", "eu")])),
            Verdict::Pass
        );
        assert!(matches!(
            pass.evaluate(&row(&[("ok", "yes"), ("region", "eu")])),
            Verdict::Unknown(_)
        ));
    }

    #[test]
    fn a_malformed_condition_is_refused_with_why() {
        for (text, why) in [
            ("", "empty"),
            ("n >", "must be followed by"),
            ("n 0", "must be compared"),
            ("(n > 0", "isn't closed"),
            ("n > 0 m < 1", "something follows"),
            ("status < 'b'", "only numbers can be ordered"),
            ("n ! 0", "must be `!=`"),
            ("n > 'x", "isn't closed"),
            ("n; drop", "isn't part of a condition"),
        ] {
            let error = Pass::parse(text).unwrap_err();
            assert!(error.contains(why), "{text}: {error}");
        }
    }
}
