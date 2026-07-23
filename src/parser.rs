//! The POSIX-ERE parser (roadmap step 1).
//!
//! Covers the full ERE grammar, including the bracket-expression corner
//! cases: `[]a]` (literal `]` first), `[a-]` (trailing `-`), `[^]]`, and
//! POSIX classes `[[:alpha:]]`…`[[:xdigit:]]`. Malformed intervals are
//! errors (`{a}`, `{3,2}`), per DESIGN.md.
//!
//! GNU extensions (bash's `regcomp` is glibc, and real scripts rely on
//! these; each verified against bash 5.2):
//!
//! - `\w` `\W` = (non-)word char (`[[:alnum:]_]`), `\s` `\S` =
//!   (non-)whitespace (`[[:space:]]`) — outside brackets only (inside,
//!   POSIX's literal-backslash rule applies, as in glibc).
//! - `\b` `\B` word boundary / non-boundary, `\<` `\>` word start/end.
//!   Quantifying one directly is an error, as in glibc.
//! - `` \` `` and `\'` = absolute start/end of the buffer — unlike `^`/`$`,
//!   which under `REG_NEWLINE` also match right after/before any `\n`,
//!   `` \` ``/`\'` always mean true position 0 / true end of input, no
//!   matter the mode (bash-verified).
//! - `{,n}` = `{0,n}`, and `{,}` = `*`.
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
//!   accepted in their degenerate single-char forms (the whole symbol in
//!   the C/UTF-8 locales bash runs in); a collating symbol may be a range
//!   endpoint, an equivalence class may not, and multi-char collating
//!   names are errors — all as glibc behaves (bash-verified).
//! - Groups may nest at most [`MAX_NESTING_DEPTH`] deep, and quantifiers may
//!   stack (`a****…`, `a{2}{2}{2}…`) at most that deep too — the two share
//!   one cap, since either alone (or a mix) recurses the same downstream AST
//!   walks; beyond it the pattern is rejected rather than risking a stack
//!   overflow.

use crate::ast::{Ast, Class, PosixClass};
use crate::bracket;
use crate::compile::MAX_REPETITION_SIZE;
use crate::error::{Error, ErrorKind};

/// Cap on structural AST nesting depth: how many `Group`/`Repeat` wrappers
/// deep a single chain gets, whether from parenthesis nesting (`((((…`),
/// quantifier stacking (`a****…`, `a{2}{2}{2}…`), or any mix of the two. The
/// parser recurses per group-nesting level while parsing (checked eagerly
/// below, at `(`, so a pathological `(((((…` can't overflow the parser's own
/// stack before this check ever runs); the compiler's AST walks and the
/// AST's own `Drop` then recurse per `Group`/`Repeat` level over the
/// *constructed* tree, so quantifier stacking needs the same cap even though
/// it never recurses in the parser itself — without it, `a` followed by a
/// few thousand `*` builds an arbitrarily deep `Repeat` chain with nothing
/// to stop it and aborts the process downstream. 250 matches the `regex`
/// crate's default `nest_limit`.
const MAX_NESTING_DEPTH: u32 = 250;

/// Rejects a structural depth beyond the cap. `depth` is the depth the node
/// being constructed *would* have (child depth + 1); `at` is the position to
/// blame.
fn check_depth(depth: u32, at: usize) -> Result<(), Error> {
    if depth > MAX_NESTING_DEPTH {
        Err(Error::new(ErrorKind::NestingTooDeep, Some(at)))
    } else {
        Ok(())
    }
}

/// Parses an ERE pattern into an [`Ast`].
pub fn parse(pattern: &str) -> Result<Ast, Error> {
    let mut parser = Parser {
        pattern,
        byte_pos: 0,
        char_pos: 0,
        group_count: 0,
        depth: 0,
    };
    let (ast, _depth) = parser.alternation()?;
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

    /// `alternation := concat ('|' concat)*`. Returns the branches' greatest
    /// structural depth alongside the `Ast` (an `Alternation` node itself
    /// adds no depth: only one branch is ever on the call stack at a time).
    fn alternation(&mut self) -> Result<(Ast, u32), Error> {
        let mut branches = vec![self.concat()?];
        while self.eat('|') {
            branches.push(self.concat()?);
        }
        let depth = branches.iter().map(|(_, d)| *d).max().unwrap_or(0);
        let ast = if branches.len() == 1 {
            branches.pop().unwrap().0
        } else {
            Ast::Alternation(branches.into_iter().map(|(ast, _)| ast).collect())
        };
        Ok((ast, depth))
    }

    /// `concat := (atom quantifier*)*` — ends at `|`, `)`, or end of input.
    /// Returns the items' greatest structural depth (a `Concat` node itself
    /// adds no depth, for the same reason as `Alternation`).
    fn concat(&mut self) -> Result<(Ast, u32), Error> {
        let mut items: Vec<(Ast, u32)> = Vec::new();
        loop {
            match self.peek() {
                None | Some('|') | Some(')') => break,
                Some('*') => self.quantify(&mut items, 0, None)?,
                Some('+') => self.quantify(&mut items, 1, None)?,
                Some('?') => self.quantify(&mut items, 0, Some(1))?,
                Some('{') => {
                    let at = self.char_pos;
                    let (min, max) = self.interval()?;
                    let (ast, depth) = items
                        .pop()
                        .ok_or(Error::new(ErrorKind::DanglingQuantifier, Some(at)))?;
                    reject_quantified_assertion(&ast, at)?;
                    let depth = depth + 1;
                    check_depth(depth, at)?;
                    items.push((
                        Ast::Repeat {
                            ast: Box::new(ast),
                            min,
                            max,
                            slot: 0,
                        },
                        depth,
                    ));
                }
                Some(_) => items.push(self.atom()?),
            }
        }
        let depth = items.iter().map(|(_, d)| *d).max().unwrap_or(0);
        let ast = match items.len() {
            0 => Ast::Empty,
            1 => items.pop().unwrap().0,
            _ => Ast::Concat(items.into_iter().map(|(ast, _)| ast).collect()),
        };
        Ok((ast, depth))
    }

    /// Applies `* + ?` (already peeked) to the preceding atom.
    fn quantify(
        &mut self,
        items: &mut Vec<(Ast, u32)>,
        min: u32,
        max: Option<u32>,
    ) -> Result<(), Error> {
        let at = self.char_pos;
        self.bump();
        let (ast, depth) = items
            .pop()
            .ok_or(Error::new(ErrorKind::DanglingQuantifier, Some(at)))?;
        reject_quantified_assertion(&ast, at)?;
        let depth = depth + 1;
        check_depth(depth, at)?;
        items.push((
            Ast::Repeat {
                ast: Box::new(ast),
                min,
                max,
                slot: 0,
            },
            depth,
        ));
        Ok(())
    }

    fn atom(&mut self) -> Result<(Ast, u32), Error> {
        match self.bump().expect("atom() called with input remaining") {
            '(' => {
                let open = self.char_pos - 1;
                self.depth += 1;
                if self.depth > MAX_NESTING_DEPTH {
                    return Err(Error::new(ErrorKind::NestingTooDeep, Some(open)));
                }
                self.group_count += 1;
                let index = self.group_count;
                let (inner, inner_depth) = self.alternation()?;
                if !self.eat(')') {
                    return Err(Error::new(ErrorKind::UnbalancedParenthesis, Some(open)));
                }
                self.depth -= 1;
                let depth = inner_depth + 1;
                check_depth(depth, open)?;
                Ok((Ast::Group(index, Box::new(inner)), depth))
            }
            '[' => Ok((self.bracket()?, 0)),
            '.' => Ok((Ast::AnyChar, 0)),
            '^' => Ok((Ast::StartAnchor, 0)),
            '$' => Ok((Ast::EndAnchor, 0)),
            '\\' => match self.bump() {
                // GNU extensions (glibc regcomp; what bash =~ accepts).
                Some('w') => Ok((word_class(false), 0)),
                Some('W') => Ok((word_class(true), 0)),
                Some('s') => Ok((space_class(false), 0)),
                Some('S') => Ok((space_class(true), 0)),
                Some('b') => Ok((Ast::WordBoundary, 0)),
                Some('B') => Ok((Ast::NotWordBoundary, 0)),
                Some('<') => Ok((Ast::WordStart, 0)),
                Some('>') => Ok((Ast::WordEnd, 0)),
                Some('`') => Ok((Ast::BufferStart, 0)),
                Some('\'') => Ok((Ast::BufferEnd, 0)),
                Some(c) => Ok((Ast::Char(c), 0)),
                None => Err(Error::new(
                    ErrorKind::TrailingBackslash,
                    Some(self.char_pos - 1),
                )),
            },
            c => Ok((Ast::Char(c), 0)),
        }
    }

    /// Parses `{m}`, `{m,}`, or `{m,n}`; the leading `{` has been peeked.
    /// A bound past [`MAX_REPETITION_SIZE`] is rejected here too — purely
    /// syntactic (the interval's own written bound, independent of any
    /// nesting), so unlike the compiler's backstop check for interactions
    /// between intervals (`(a{1000}){1000}`), this one can carry a
    /// position.
    fn interval(&mut self) -> Result<(u32, Option<u32>), Error> {
        let open = self.char_pos;
        let err = || Error::new(ErrorKind::InvalidInterval, Some(open));
        let too_large = || Error::new(ErrorKind::RepetitionTooLarge, Some(open));
        self.bump();
        // GNU: `{,n}` means `{0,n}` (and `{,}` means `*`), as in glibc.
        let min = if self.peek() == Some(',') {
            0
        } else {
            self.integer(open)?
        };
        if self.eat('}') {
            return if min > MAX_REPETITION_SIZE {
                Err(too_large())
            } else {
                Ok((min, Some(min)))
            };
        }
        if !self.eat(',') {
            return Err(err());
        }
        if self.eat('}') {
            return if min > MAX_REPETITION_SIZE {
                Err(too_large())
            } else {
                Ok((min, None))
            };
        }
        let max = self.integer(open)?;
        if !self.eat('}') {
            return Err(err());
        }
        if min > max {
            return Err(err());
        }
        if min > MAX_REPETITION_SIZE || max > MAX_REPETITION_SIZE {
            return Err(too_large());
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
    /// Delegates to the shared bracket-expression grammar (also used by
    /// the glob translator) — see `bracket::parse`.
    fn bracket(&mut self) -> Result<Ast, Error> {
        let open = self.char_pos - 1;
        let mut cursor = bracket::Cursor::new(self.pattern, self.byte_pos, self.char_pos);
        let class = bracket::parse(&mut cursor, open, &['^'])?;
        self.byte_pos = cursor.byte_pos;
        self.char_pos = cursor.char_pos;
        Ok(Ast::Class(class))
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
        // Degenerate (single-char) collating symbols and equivalence
        // classes are literals, as bash accepts them; collating symbols
        // work as range endpoints, equivalence classes don't, and
        // multi-char collating names are errors (all bash-verified).
        assert_eq!(parse("[[.a.]]"), Ok(class(false, &[('a', 'a')], &[])));
        assert_eq!(parse("[[=a=]]"), Ok(class(false, &[('a', 'a')], &[])));
        assert_eq!(parse("[[.a.]-c]"), Ok(class(false, &[('a', 'c')], &[])));
        assert_eq!(parse("[a-[.c.]]"), Ok(class(false, &[('a', 'c')], &[])));
        assert_eq!(
            parse("[[.-.]c]"),
            Ok(class(false, &[('-', '-'), ('c', 'c')], &[]))
        );
        assert_eq!(err("[[=a=]-c]"), ErrorKind::InvalidRange);
        assert_eq!(err("[a-[=c=]]"), ErrorKind::InvalidRange);
        assert_eq!(err("[[.ab.]]"), ErrorKind::InvalidPosixClass);
        assert_eq!(err("[[.a"), ErrorKind::InvalidPosixClass);
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
    fn stacked_quantifier_depth_is_capped() {
        // Stacked postfix quantifiers build a `Repeat` chain just like
        // nested groups do, and share the same cap (they used to be
        // uncapped and could abort the process — this is the same crash
        // class as `nesting_depth_is_capped`, reached via `*` instead of
        // `(`).
        let deep = "a".to_string() + &"*".repeat(250);
        assert!(parse(&deep).is_ok());
        let too_deep = "a".to_string() + &"*".repeat(251);
        assert_eq!(err(&too_deep), ErrorKind::NestingTooDeep);
        // `{2}` stacking hits the same cap.
        let deep_interval = "a".to_string() + &"{2}".repeat(250);
        assert!(parse(&deep_interval).is_ok());
        let too_deep_interval = "a".to_string() + &"{2}".repeat(251);
        assert_eq!(err(&too_deep_interval), ErrorKind::NestingTooDeep);
        // The original crash case, via quantifiers instead of parens.
        let pathological = "a".to_string() + &"*".repeat(500_000);
        assert_eq!(err(&pathological), ErrorKind::NestingTooDeep);
        // Mixed group + quantifier nesting shares the same combined cap.
        let mixed = "(".repeat(125) + "a" + &")".repeat(125) + &"*".repeat(125);
        assert!(parse(&mixed).is_ok());
        let mixed_too_deep = "(".repeat(125) + "a" + &")".repeat(125) + &"*".repeat(126);
        assert_eq!(err(&mixed_too_deep), ErrorKind::NestingTooDeep);
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
