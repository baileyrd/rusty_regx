//! The POSIX-ERE parser (roadmap step 1).
//!
//! Covers the full ERE grammar, including the bracket-expression corner
//! cases: `[]a]` (literal `]` first), `[a-]` (trailing `-`), `[^]]`, and
//! POSIX classes `[[:alpha:]]`…`[[:xdigit:]]`. Malformed intervals are
//! errors (`{,}`, `{a}`, `{3,2}`), per DESIGN.md.
//!
//! GNU extensions (bash's `regcomp` is glibc, and real scripts rely on
//! these; each verified against bash 5.2):
//!
//! - `\w` `\W` = (non-)word char (`[[:alnum:]_]`), `\s` `\S` =
//!   (non-)whitespace (`[[:space:]]`) — outside brackets only (inside,
//!   POSIX's literal-backslash rule applies, as in glibc).
//! - `\b` `\B` word boundary / non-boundary, `\<` `\>` word start/end.
//!   Quantifying one directly is an error, as in glibc.
//! - `` \` `` and `\'` = start/end of input.
//! - `{,n}` = `{0,n}`, and `{,}` = `*`.
//!
//! GNU extensions (bash's `regcomp` is glibc, and real scripts rely on
//! these; each verified against bash 5.2): `\w` `\W` (word chars,
//! `[[:alnum:]_]`), `\s` `\S` (`[[:space:]]`), `\b` `\B` word
//! boundary/non-boundary, `\<` `\>` word start/end (quantifying an
//! assertion directly is an error, as in glibc), `` \` `` `\'` input
//! start/end, and `{,n}` = `{0,n}` (so `{,}` = `*`). Outside this set,
//! `\c` stays a literal `c` (`\d` is a literal `d`, as in glibc); inside
//! brackets the POSIX literal-backslash rule still applies.
//!
//! Deliberate choices where POSIX leaves behavior undefined:
//!
//! - `\c` outside brackets makes any *other* `c` literal (so `\d` is a
//!   literal `d`, as in glibc); a lone trailing `\` is an error.
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
use crate::error::{Error, ErrorKind};

/// Cap on group-nesting depth. The parser recurses per nesting level (as do
/// the compiler's AST walks and the AST's own `Drop`), so without a cap a
/// user-supplied pattern like `((((…` overflows the stack and aborts the
/// process — the shell must survive any pattern. 250 matches the `regex`
/// crate's default `nest_limit`.
const MAX_NESTING_DEPTH: u32 = 250;

/// Parses an ERE pattern into an [`Ast`].
pub fn parse(pattern: &str) -> Result<Ast, Error> {
    let mut parser = Parser {
        pattern,
        byte_pos: 0,
        char_pos: 0,
        group_count: 0,
        depth: 0,
    };
    let ast = parser.alternation()?;
    if parser.byte_pos < pattern.len() {
        // alternation() only stops early on `)`.
        return Err(Error::new(
            ErrorKind::UnbalancedParenthesis,
            Some(parser.char_pos),
        ));
    }
    Ok(ast)
}

/// glibc rejects a quantifier directly on a word assertion (`\b*` is a
/// compile error in bash); `^`/`$` stay quantifiable as before.
fn reject_quantified_assertion(ast: &Ast, at: usize) -> Result<(), Error> {
    match ast {
        Ast::WordBoundary | Ast::NotWordBoundary | Ast::WordStart | Ast::WordEnd => {
            Err(Error::new(ErrorKind::DanglingQuantifier, Some(at)))
        }
        _ => Ok(()),
    }
}

/// `\w`/`\W`: glibc's word class, `[[:alnum:]_]` (optionally negated).
fn word_class(negated: bool) -> Ast {
    Ast::Class(Class {
        negated,
        ranges: vec![('_', '_')],
        posix: vec![PosixClass::Alnum],
    })
}

/// `\s`/`\S`: `[[:space:]]` (optionally negated).
fn space_class(negated: bool) -> Ast {
    Ast::Class(Class {
        negated,
        ranges: Vec::new(),
        posix: vec![PosixClass::Space],
    })
}

/// Iterates the pattern `&str` directly — no up-front `Vec<char>`
/// collection, keeping compilation allocation-light (compile speed is
/// this engine's headline advantage; a shell pays it on every `=~`).
/// `byte_pos` drives slicing; `char_pos` is carried alongside because
/// error positions are documented as char offsets.
struct Parser<'p> {
    pattern: &'p str,
    byte_pos: usize,
    char_pos: usize,
    group_count: u32,
    depth: u32,
}

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.pattern[self.byte_pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.rest().chars().nth(offset)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if let Some(c) = c {
            self.byte_pos += c.len_utf8();
            self.char_pos += 1;
        }
        c
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.bump();
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
                    let at = self.char_pos;
                    let (min, max) = self.interval()?;
                    let ast = items
                        .pop()
                        .ok_or(Error::new(ErrorKind::DanglingQuantifier, Some(at)))?;
                    reject_quantified_assertion(&ast, at)?;
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
        let at = self.char_pos;
        self.bump();
        let ast = items
            .pop()
            .ok_or(Error::new(ErrorKind::DanglingQuantifier, Some(at)))?;
        reject_quantified_assertion(&ast, at)?;
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
                let open = self.char_pos - 1;
                self.depth += 1;
                if self.depth > MAX_NESTING_DEPTH {
                    return Err(Error::new(ErrorKind::NestingTooDeep, Some(open)));
                }
                self.group_count += 1;
                let index = self.group_count;
                let inner = self.alternation()?;
                if !self.eat(')') {
                    return Err(Error::new(ErrorKind::UnbalancedParenthesis, Some(open)));
                }
                self.depth -= 1;
                Ok(Ast::Group(index, Box::new(inner)))
            }
            '[' => self.bracket(),
            '.' => Ok(Ast::AnyChar),
            '^' => Ok(Ast::StartAnchor),
            '$' => Ok(Ast::EndAnchor),
            '\\' => match self.bump() {
                // GNU extensions (glibc regcomp; what bash =~ accepts).
                Some('w') => Ok(word_class(false)),
                Some('W') => Ok(word_class(true)),
                Some('s') => Ok(space_class(false)),
                Some('S') => Ok(space_class(true)),
                Some('b') => Ok(Ast::WordBoundary),
                Some('B') => Ok(Ast::NotWordBoundary),
                Some('<') => Ok(Ast::WordStart),
                Some('>') => Ok(Ast::WordEnd),
                Some('`') => Ok(Ast::StartAnchor),
                Some('\'') => Ok(Ast::EndAnchor),
                Some(c) => Ok(Ast::Char(c)),
                None => Err(Error::new(
                    ErrorKind::TrailingBackslash,
                    Some(self.char_pos - 1),
                )),
            },
            c => Ok(Ast::Char(c)),
        }
    }

    /// Parses `{m}`, `{m,}`, or `{m,n}`; the leading `{` has been peeked.
    fn interval(&mut self) -> Result<(u32, Option<u32>), Error> {
        let open = self.char_pos;
        let err = || Error::new(ErrorKind::InvalidInterval, Some(open));
        self.bump();
        // GNU: `{,n}` means `{0,n}` (and `{,}` means `*`), as in glibc.
        let min = if self.peek() == Some(',') {
            0
        } else {
            self.integer(open)?
        };
        if self.eat('}') {
            return Ok((min, Some(min)));
        }
        if !self.eat(',') {
            return Err(err());
        }
        if self.eat('}') {
            return Ok((min, None));
        }
        let max = self.integer(open)?;
        if !self.eat('}') {
            return Err(err());
        }
        if min > max {
            return Err(err());
        }
        Ok((min, Some(max)))
    }

    /// `at` is the interval's `{` (char offset), where any failure is
    /// reported.
    fn integer(&mut self, at: usize) -> Result<u32, Error> {
        let start = self.byte_pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
        }
        if self.byte_pos == start {
            return Err(Error::new(ErrorKind::InvalidInterval, Some(at)));
        }
        self.pattern[start..self.byte_pos]
            .parse()
            .map_err(|_| Error::new(ErrorKind::InvalidInterval, Some(at)))
    }

    /// Parses a bracket expression; the leading `[` has been consumed.
    fn bracket(&mut self) -> Result<Ast, Error> {
        let open = self.char_pos - 1;
        let negated = self.eat('^');
        let mut class = Class {
            negated,
            ranges: Vec::new(),
            posix: Vec::new(),
        };
        let mut first = true;
        loop {
            match self.peek() {
                None => return Err(Error::new(ErrorKind::UnclosedBracket, Some(open))),
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
        let start = self.char_pos;
        if self.peek() == Some('[') {
            match self.peek_at(1) {
                Some(':') => {
                    class.posix.push(self.posix_class()?);
                    return Ok(());
                }
                // Collating symbols and equivalence classes are out of scope.
                Some('.') | Some('=') => {
                    return Err(Error::new(ErrorKind::InvalidPosixClass, Some(start)))
                }
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
                return Err(Error::new(ErrorKind::InvalidRange, Some(start)));
            }
            if lo > hi {
                return Err(Error::new(ErrorKind::InvalidRange, Some(start)));
            }
            class.ranges.push((lo, hi));
        } else {
            class.ranges.push((lo, lo));
        }
        Ok(())
    }

    /// Parses `[:name:]`; the leading `[` has been peeked (not consumed).
    fn posix_class(&mut self) -> Result<PosixClass, Error> {
        let open = self.char_pos;
        let err = || Error::new(ErrorKind::InvalidPosixClass, Some(open));
        self.bump(); // `[`
        self.bump(); // `:`
        let start = self.byte_pos;
        while self.peek().is_some_and(|c| c.is_ascii_lowercase()) {
            self.bump();
        }
        let name = &self.pattern[start..self.byte_pos];
        if !(self.eat(':') && self.eat(']')) {
            return Err(err());
        }
        match name {
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
            _ => Err(err()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: char) -> Ast {
        Ast::Char(c)
    }

    /// The error kind a pattern fails with.
    fn err(pattern: &str) -> ErrorKind {
        parse(pattern).unwrap_err().kind()
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
        assert_eq!(err(r"a\"), ErrorKind::TrailingBackslash);
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
        assert_eq!(err("(a"), ErrorKind::UnbalancedParenthesis);
        assert_eq!(err("a)"), ErrorKind::UnbalancedParenthesis);
        assert_eq!(err("(a))"), ErrorKind::UnbalancedParenthesis);
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
        assert_eq!(err("*a"), ErrorKind::DanglingQuantifier);
        assert_eq!(err("+"), ErrorKind::DanglingQuantifier);
        assert_eq!(err("a|?"), ErrorKind::DanglingQuantifier);
        assert_eq!(err("(*)"), ErrorKind::DanglingQuantifier);
        assert_eq!(err("{2}"), ErrorKind::DanglingQuantifier);
    }

    #[test]
    fn intervals() {
        assert_eq!(parse("a{3}"), Ok(repeat(ch('a'), 3, Some(3))));
        assert_eq!(parse("a{3,}"), Ok(repeat(ch('a'), 3, None)));
        assert_eq!(parse("a{3,5}"), Ok(repeat(ch('a'), 3, Some(5))));
        assert_eq!(parse("a{0,0}"), Ok(repeat(ch('a'), 0, Some(0))));
        // GNU: an omitted minimum is 0 (bash-verified).
        assert_eq!(parse("a{,3}"), Ok(repeat(ch('a'), 0, Some(3))));
        assert_eq!(parse("a{,}"), Ok(repeat(ch('a'), 0, None)));
    }

    #[test]
    fn bad_intervals() {
        for pattern in [
            "a{}",
            "a{x}",
            "a{3,x}",
            "a{3,2}",
            "a{3",
            "a{3,",
            "a{99999999999999999999}",
        ] {
            assert_eq!(err(pattern), ErrorKind::InvalidInterval, "{pattern}");
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
        assert_eq!(err("[abc"), ErrorKind::UnclosedBracket);
        assert_eq!(err("[a-"), ErrorKind::UnclosedBracket);
        assert_eq!(err("[]"), ErrorKind::UnclosedBracket);
        assert_eq!(err("[^]"), ErrorKind::UnclosedBracket);
        assert_eq!(err("[z-a]"), ErrorKind::InvalidRange);
        assert_eq!(err("[a-[:digit:]]"), ErrorKind::InvalidRange);
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
        assert_eq!(err("[[:foo:]]"), ErrorKind::InvalidPosixClass);
        assert_eq!(err("[[:alpha]]"), ErrorKind::InvalidPosixClass);
        assert_eq!(err("[[:alpha"), ErrorKind::InvalidPosixClass);
        // Collating symbols and equivalence classes are unsupported.
        assert_eq!(err("[[.a.]]"), ErrorKind::InvalidPosixClass);
        assert_eq!(err("[[=a=]]"), ErrorKind::InvalidPosixClass);
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
        assert_eq!(err(&too_deep), ErrorKind::NestingTooDeep);
        // Nesting is what's capped, not the total number of groups.
        let wide = "(a)".repeat(10_000);
        assert!(parse(&wide).is_ok());
        // The original crash case: a pathological pattern must be an error,
        // never a stack overflow (this used to abort the process).
        let pathological = "(".repeat(500_000);
        assert_eq!(err(&pathological), ErrorKind::NestingTooDeep);
    }

    #[test]
    fn errors_carry_positions() {
        // Positions are 0-based char offsets of the offending construct.
        let e = parse("ab[cd").unwrap_err();
        assert_eq!(
            (e.kind(), e.position()),
            (ErrorKind::UnclosedBracket, Some(2))
        );
        assert_eq!(e.to_string(), "unclosed bracket expression at position 2");
        let e = parse("a(b(c)").unwrap_err();
        assert_eq!(
            (e.kind(), e.position()),
            (ErrorKind::UnbalancedParenthesis, Some(1))
        );
        assert_eq!(parse("ab)").unwrap_err().position(), Some(2));
        assert_eq!(parse("ab*c{2,1}").unwrap_err().position(), Some(4));
        assert_eq!(parse("a|*b").unwrap_err().position(), Some(2));
        assert_eq!(parse("ab[x[:foo:]]").unwrap_err().position(), Some(4));
        assert_eq!(parse("a[z-a]").unwrap_err().position(), Some(2));
        assert_eq!(parse(r"ab\").unwrap_err().position(), Some(2));
        // Positions count chars, not bytes.
        assert_eq!(parse("éé[").unwrap_err().position(), Some(2));
    }

    #[test]
    fn realistic_patterns() {
        // The shapes rush's C56 tests exercise.
        assert!(parse(r"^([[:alpha:]]+)-([0-9]{2,4})$").is_ok());
        assert!(parse(r"(a|b)*c+").is_ok());
        assert!(parse(r"^/([^/]+)/([^/]+)$").is_ok());
    }
}
