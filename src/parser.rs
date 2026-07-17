//! The POSIX-ERE parser (roadmap step 1).
//!
//! Covers the full ERE grammar, including the bracket-expression corner
//! cases: `[]a]` (literal `]` first), `[a-]` (trailing `-`), `[^]]`, and
//! POSIX classes `[[:alpha:]]`…`[[:xdigit:]]`. Malformed intervals are
//! errors (`{,}`, `{a}`, `{3,2}`), per DESIGN.md.
//!
//! Deliberate choices where POSIX leaves behavior undefined:
//!
//! - `\c` outside brackets makes `c` literal for any `c`; a lone trailing
//!   `\` is an error.
//! - Inside a bracket expression, `\` is a literal backslash (POSIX rule;
//!   this matches bash/glibc, not the `regex` crate).
//! - `^` and `$` are anchors anywhere in the pattern, and atoms (including
//!   anchors and already-quantified expressions) may be quantified.
//! - An empty alternation branch (`a|`) matches the empty string.
//! - `{` always begins an interval and must be well-formed; a lone `}` is a
//!   literal.
//! - Collating symbols `[.x.]` and equivalence classes `[=x=]` are
//!   unsupported and rejected.
//! - Groups may nest at most [`MAX_NESTING_DEPTH`] deep; beyond that the
//!   pattern is rejected rather than risking a stack overflow.

use crate::ast::{Ast, Class, PosixClass};
use crate::error::Error;

/// Cap on group-nesting depth. The parser recurses per nesting level (as do
/// the compiler's AST walks and the AST's own `Drop`), so without a cap a
/// user-supplied pattern like `((((…` overflows the stack and aborts the
/// process — the shell must survive any pattern. 250 matches the `regex`
/// crate's default `nest_limit`.
const MAX_NESTING_DEPTH: u32 = 250;

/// Parses an ERE pattern into an [`Ast`].
pub fn parse(pattern: &str) -> Result<Ast, Error> {
    let mut parser = Parser {
        chars: pattern.chars().collect(),
        pos: 0,
        group_count: 0,
        depth: 0,
    };
    let ast = parser.alternation()?;
    if parser.pos < parser.chars.len() {
        // alternation() only stops early on `)`.
        return Err(Error::UnbalancedParenthesis);
    }
    Ok(ast)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    group_count: u32,
    depth: u32,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// `alternation := concat ('|' concat)*`
    fn alternation(&mut self) -> Result<Ast, Error> {
        let mut branches = vec![self.concat()?];
        while self.eat('|') {
            branches.push(self.concat()?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().unwrap()
        } else {
            Ast::Alternation(branches)
        })
    }

    /// `concat := (atom quantifier*)*` — ends at `|`, `)`, or end of input.
    fn concat(&mut self) -> Result<Ast, Error> {
        let mut items: Vec<Ast> = Vec::new();
        loop {
            match self.peek() {
                None | Some('|') | Some(')') => break,
                Some('*') => self.quantify(&mut items, 0, None)?,
                Some('+') => self.quantify(&mut items, 1, None)?,
                Some('?') => self.quantify(&mut items, 0, Some(1))?,
                Some('{') => {
                    let (min, max) = self.interval()?;
                    let ast = items.pop().ok_or(Error::DanglingQuantifier)?;
                    items.push(Ast::Repeat {
                        ast: Box::new(ast),
                        min,
                        max,
                        slot: 0,
                    });
                }
                Some(_) => items.push(self.atom()?),
            }
        }
        Ok(match items.len() {
            0 => Ast::Empty,
            1 => items.pop().unwrap(),
            _ => Ast::Concat(items),
        })
    }

    /// Applies `* + ?` (already peeked) to the preceding atom.
    fn quantify(&mut self, items: &mut Vec<Ast>, min: u32, max: Option<u32>) -> Result<(), Error> {
        self.bump();
        let ast = items.pop().ok_or(Error::DanglingQuantifier)?;
        items.push(Ast::Repeat {
            ast: Box::new(ast),
            min,
            max,
            slot: 0,
        });
        Ok(())
    }

    fn atom(&mut self) -> Result<Ast, Error> {
        match self.bump().expect("atom() called with input remaining") {
            '(' => {
                self.depth += 1;
                if self.depth > MAX_NESTING_DEPTH {
                    return Err(Error::NestingTooDeep);
                }
                self.group_count += 1;
                let index = self.group_count;
                let inner = self.alternation()?;
                if !self.eat(')') {
                    return Err(Error::UnbalancedParenthesis);
                }
                self.depth -= 1;
                Ok(Ast::Group(index, Box::new(inner)))
            }
            '[' => self.bracket(),
            '.' => Ok(Ast::AnyChar),
            '^' => Ok(Ast::StartAnchor),
            '$' => Ok(Ast::EndAnchor),
            '\\' => match self.bump() {
                Some(c) => Ok(Ast::Char(c)),
                None => Err(Error::TrailingBackslash),
            },
            c => Ok(Ast::Char(c)),
        }
    }

    /// Parses `{m}`, `{m,}`, or `{m,n}`; the leading `{` has been peeked.
    fn interval(&mut self) -> Result<(u32, Option<u32>), Error> {
        self.bump();
        let min = self.integer()?;
        if self.eat('}') {
            return Ok((min, Some(min)));
        }
        if !self.eat(',') {
            return Err(Error::InvalidInterval);
        }
        if self.eat('}') {
            return Ok((min, None));
        }
        let max = self.integer()?;
        if !self.eat('}') {
            return Err(Error::InvalidInterval);
        }
        if min > max {
            return Err(Error::InvalidInterval);
        }
        Ok((min, Some(max)))
    }

    fn integer(&mut self) -> Result<u32, Error> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(Error::InvalidInterval);
        }
        self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .parse()
            .map_err(|_| Error::InvalidInterval)
    }

    /// Parses a bracket expression; the leading `[` has been consumed.
    fn bracket(&mut self) -> Result<Ast, Error> {
        let negated = self.eat('^');
        let mut class = Class {
            negated,
            ranges: Vec::new(),
            posix: Vec::new(),
        };
        let mut first = true;
        loop {
            match self.peek() {
                None => return Err(Error::UnclosedBracket),
                Some(']') if !first => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            first = false;
            self.bracket_item(&mut class)?;
        }
        Ok(Ast::Class(class))
    }

    /// One bracket item: a POSIX class, a range `a-z`, or a literal char.
    fn bracket_item(&mut self, class: &mut Class) -> Result<(), Error> {
        if self.peek() == Some('[') {
            match self.peek_at(1) {
                Some(':') => {
                    class.posix.push(self.posix_class()?);
                    return Ok(());
                }
                // Collating symbols and equivalence classes are out of scope.
                Some('.') | Some('=') => return Err(Error::InvalidPosixClass),
                _ => {}
            }
        }
        let lo = self.bump().expect("checked non-empty");
        // `a-z` is a range unless the `-` is last (`[a-]`, trailing `-` is
        // literal) or the expression is unclosed.
        if self.peek() == Some('-') && self.peek_at(1).is_some_and(|c| c != ']') {
            self.bump();
            let hi = self.bump().expect("checked non-empty");
            // A class can't be a range endpoint: `[a-[:digit:]]`.
            if hi == '[' && matches!(self.peek(), Some(':') | Some('.') | Some('=')) {
                return Err(Error::InvalidRange);
            }
            if lo > hi {
                return Err(Error::InvalidRange);
            }
            class.ranges.push((lo, hi));
        } else {
            class.ranges.push((lo, lo));
        }
        Ok(())
    }

    /// Parses `[:name:]`; the leading `[` has been peeked (not consumed).
    fn posix_class(&mut self) -> Result<PosixClass, Error> {
        self.bump(); // `[`
        self.bump(); // `:`
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_lowercase()) {
            self.pos += 1;
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        if !(self.eat(':') && self.eat(']')) {
            return Err(Error::InvalidPosixClass);
        }
        match name.as_str() {
            "alnum" => Ok(PosixClass::Alnum),
            "alpha" => Ok(PosixClass::Alpha),
            "blank" => Ok(PosixClass::Blank),
            "cntrl" => Ok(PosixClass::Cntrl),
            "digit" => Ok(PosixClass::Digit),
            "graph" => Ok(PosixClass::Graph),
            "lower" => Ok(PosixClass::Lower),
            "print" => Ok(PosixClass::Print),
            "punct" => Ok(PosixClass::Punct),
            "space" => Ok(PosixClass::Space),
            "upper" => Ok(PosixClass::Upper),
            "xdigit" => Ok(PosixClass::Xdigit),
            _ => Err(Error::InvalidPosixClass),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: char) -> Ast {
        Ast::Char(c)
    }

    fn class(negated: bool, ranges: &[(char, char)], posix: &[PosixClass]) -> Ast {
        Ast::Class(Class {
            negated,
            ranges: ranges.to_vec(),
            posix: posix.to_vec(),
        })
    }

    fn repeat(ast: Ast, min: u32, max: Option<u32>) -> Ast {
        Ast::Repeat {
            ast: Box::new(ast),
            min,
            max,
            slot: 0,
        }
    }

    #[test]
    fn literals_and_concat() {
        assert_eq!(parse(""), Ok(Ast::Empty));
        assert_eq!(parse("a"), Ok(ch('a')));
        assert_eq!(parse("ab"), Ok(Ast::Concat(vec![ch('a'), ch('b')])));
        assert_eq!(parse("é"), Ok(ch('é')));
        // `}` and `-` and `]` are literals outside special positions.
        assert_eq!(parse("}"), Ok(ch('}')));
        assert_eq!(parse("-"), Ok(ch('-')));
        assert_eq!(parse("]"), Ok(ch(']')));
    }

    #[test]
    fn anchors_and_any_char() {
        assert_eq!(
            parse("^a.$"),
            Ok(Ast::Concat(vec![
                Ast::StartAnchor,
                ch('a'),
                Ast::AnyChar,
                Ast::EndAnchor,
            ]))
        );
        // Anchors are special anywhere in an ERE.
        assert_eq!(
            parse("a^b"),
            Ok(Ast::Concat(vec![ch('a'), Ast::StartAnchor, ch('b')]))
        );
    }

    #[test]
    fn escapes() {
        assert_eq!(
            parse(r"\.\*\\"),
            Ok(Ast::Concat(vec![ch('.'), ch('*'), ch('\\')]))
        );
        // Any escaped char is that literal char (no Perl classes).
        assert_eq!(parse(r"\d"), Ok(ch('d')));
        assert_eq!(parse(r"a\"), Err(Error::TrailingBackslash));
    }

    #[test]
    fn alternation() {
        assert_eq!(
            parse("a|b|c"),
            Ok(Ast::Alternation(vec![ch('a'), ch('b'), ch('c')]))
        );
        // Empty branches are allowed and match the empty string.
        assert_eq!(parse("a|"), Ok(Ast::Alternation(vec![ch('a'), Ast::Empty])));
        assert_eq!(parse("|a"), Ok(Ast::Alternation(vec![Ast::Empty, ch('a')])));
    }

    #[test]
    fn groups_and_numbering() {
        assert_eq!(parse("(a)"), Ok(Ast::Group(1, Box::new(ch('a')))));
        assert_eq!(parse("()"), Ok(Ast::Group(1, Box::new(Ast::Empty))));
        // Groups number by opening parenthesis, depth-first.
        assert_eq!(
            parse("((a)(b))"),
            Ok(Ast::Group(
                1,
                Box::new(Ast::Concat(vec![
                    Ast::Group(2, Box::new(ch('a'))),
                    Ast::Group(3, Box::new(ch('b'))),
                ]))
            ))
        );
        assert_eq!(parse("(a"), Err(Error::UnbalancedParenthesis));
        assert_eq!(parse("a)"), Err(Error::UnbalancedParenthesis));
        assert_eq!(parse("(a))"), Err(Error::UnbalancedParenthesis));
    }

    #[test]
    fn quantifiers() {
        assert_eq!(parse("a*"), Ok(repeat(ch('a'), 0, None)));
        assert_eq!(parse("a+"), Ok(repeat(ch('a'), 1, None)));
        assert_eq!(parse("a?"), Ok(repeat(ch('a'), 0, Some(1))));
        // Quantifiers bind to the last atom only.
        assert_eq!(
            parse("ab*"),
            Ok(Ast::Concat(vec![ch('a'), repeat(ch('b'), 0, None)]))
        );
        // Stacked quantifiers apply outward.
        assert_eq!(
            parse("a*?"),
            Ok(repeat(repeat(ch('a'), 0, None), 0, Some(1)))
        );
        assert_eq!(
            parse("(ab)+"),
            Ok(repeat(
                Ast::Group(1, Box::new(Ast::Concat(vec![ch('a'), ch('b')]))),
                1,
                None
            ))
        );
    }

    #[test]
    fn dangling_quantifiers() {
        assert_eq!(parse("*a"), Err(Error::DanglingQuantifier));
        assert_eq!(parse("+"), Err(Error::DanglingQuantifier));
        assert_eq!(parse("a|?"), Err(Error::DanglingQuantifier));
        assert_eq!(parse("(*)"), Err(Error::DanglingQuantifier));
        assert_eq!(parse("{2}"), Err(Error::DanglingQuantifier));
    }

    #[test]
    fn intervals() {
        assert_eq!(parse("a{3}"), Ok(repeat(ch('a'), 3, Some(3))));
        assert_eq!(parse("a{3,}"), Ok(repeat(ch('a'), 3, None)));
        assert_eq!(parse("a{3,5}"), Ok(repeat(ch('a'), 3, Some(5))));
        assert_eq!(parse("a{0,0}"), Ok(repeat(ch('a'), 0, Some(0))));
    }

    #[test]
    fn bad_intervals() {
        for pattern in [
            "a{}",
            "a{,}",
            "a{,3}",
            "a{x}",
            "a{3,x}",
            "a{3,2}",
            "a{3",
            "a{3,",
            "a{99999999999999999999}",
        ] {
            assert_eq!(parse(pattern), Err(Error::InvalidInterval), "{pattern}");
        }
    }

    #[test]
    fn bracket_basics() {
        assert_eq!(
            parse("[abc]"),
            Ok(class(false, &[('a', 'a'), ('b', 'b'), ('c', 'c')], &[]))
        );
        assert_eq!(parse("[a-z]"), Ok(class(false, &[('a', 'z')], &[])));
        assert_eq!(
            parse("[a-cx-z]"),
            Ok(class(false, &[('a', 'c'), ('x', 'z')], &[]))
        );
        assert_eq!(
            parse("[^ab]"),
            Ok(class(true, &[('a', 'a'), ('b', 'b')], &[]))
        );
        // Metacharacters are literal inside brackets; so is backslash (POSIX).
        assert_eq!(
            parse(r"[.*\]"),
            Ok(class(false, &[('.', '.'), ('*', '*'), ('\\', '\\')], &[]))
        );
    }

    #[test]
    fn bracket_corner_cases() {
        // A `]` right after `[` or `[^` is a literal.
        assert_eq!(
            parse("[]a]"),
            Ok(class(false, &[(']', ']'), ('a', 'a')], &[]))
        );
        assert_eq!(parse("[^]]"), Ok(class(true, &[(']', ']')], &[])));
        // Leading or trailing `-` is a literal.
        assert_eq!(
            parse("[a-]"),
            Ok(class(false, &[('a', 'a'), ('-', '-')], &[]))
        );
        assert_eq!(
            parse("[-a]"),
            Ok(class(false, &[('-', '-'), ('a', 'a')], &[]))
        );
        // `[]-]` is a class of `]` and `-`, not a range.
        assert_eq!(
            parse("[]-]"),
            Ok(class(false, &[(']', ']'), ('-', '-')], &[]))
        );
    }

    #[test]
    fn bracket_errors() {
        assert_eq!(parse("[abc"), Err(Error::UnclosedBracket));
        assert_eq!(parse("[a-"), Err(Error::UnclosedBracket));
        assert_eq!(parse("[]"), Err(Error::UnclosedBracket));
        assert_eq!(parse("[^]"), Err(Error::UnclosedBracket));
        assert_eq!(parse("[z-a]"), Err(Error::InvalidRange));
        assert_eq!(parse("[a-[:digit:]]"), Err(Error::InvalidRange));
    }

    #[test]
    fn posix_classes() {
        assert_eq!(
            parse("[[:alpha:]]"),
            Ok(class(false, &[], &[PosixClass::Alpha]))
        );
        assert_eq!(
            parse("[^[:space:][:digit:]x]"),
            Ok(class(
                true,
                &[('x', 'x')],
                &[PosixClass::Space, PosixClass::Digit]
            ))
        );
        for name in [
            "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct",
            "space", "upper", "xdigit",
        ] {
            assert!(parse(&format!("[[:{name}:]]")).is_ok(), "{name}");
        }
        assert_eq!(parse("[[:foo:]]"), Err(Error::InvalidPosixClass));
        assert_eq!(parse("[[:alpha]]"), Err(Error::InvalidPosixClass));
        assert_eq!(parse("[[:alpha"), Err(Error::InvalidPosixClass));
        // Collating symbols and equivalence classes are unsupported.
        assert_eq!(parse("[[.a.]]"), Err(Error::InvalidPosixClass));
        assert_eq!(parse("[[=a=]]"), Err(Error::InvalidPosixClass));
        // A `[` not followed by `:` `.` `=` is a literal inside brackets.
        assert_eq!(
            parse("[[a]"),
            Ok(class(false, &[('[', '['), ('a', 'a')], &[]))
        );
    }

    #[test]
    fn nesting_depth_is_capped() {
        // At the cap: fine.
        let deep = "(".repeat(250) + "a" + &")".repeat(250);
        assert!(parse(&deep).is_ok());
        // One past the cap: rejected.
        let too_deep = "(".repeat(251) + "a" + &")".repeat(251);
        assert_eq!(parse(&too_deep), Err(Error::NestingTooDeep));
        // Nesting is what's capped, not the total number of groups.
        let wide = "(a)".repeat(10_000);
        assert!(parse(&wide).is_ok());
        // The original crash case: a pathological pattern must be an error,
        // never a stack overflow (this used to abort the process).
        let pathological = "(".repeat(500_000);
        assert_eq!(parse(&pathological), Err(Error::NestingTooDeep));
    }

    #[test]
    fn realistic_patterns() {
        // The shapes rush's C56 tests exercise.
        assert!(parse(r"^([[:alpha:]]+)-([0-9]{2,4})$").is_ok());
        assert!(parse(r"(a|b)*c+").is_ok());
        assert!(parse(r"^/([^/]+)/([^/]+)$").is_ok());
    }
}
