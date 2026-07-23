//! Shell glob (`fnmatch`-style) pattern matching on the same linear-time
//! engine used for POSIX ERE — see `docs/GLOB_DESIGN.md` for the full
//! design and roadmap (tracking issue: #20).
//!
//! Glob syntax translates onto the existing ERE [`Ast`], so it reuses
//! every execution tier (literal path, prefix/suffix, anchors, Pike VM)
//! and inherits the same non-negotiable guarantee: no backtracking, ever.
//!
//! Covered so far: `?`, `*`, `[...]`/`[!...]`, literals, whole-pattern
//! (full-string) matching, the bash extglob operators `@()` `?()` `*()`
//! `+()`, and `!()` negation restricted to the whole pattern (see
//! [`GlobBuilder::build`]). Pathname mode, leading-period rules,
//! case-insensitivity, and prefix/suffix matching are follow-up issues
//! tracked under #20.

use crate::ast::Ast;
use crate::bracket;
use crate::compile::{self, Program};
use crate::error::{Error, ErrorKind};
use crate::parser;
use std::sync::Arc;

/// Builds a [`Glob`] with non-default options.
///
/// Currently exposes only [`GlobBuilder::build`] with the default
/// translation; later issues under #20 add `pathname`, `period`, and
/// `case_insensitive` methods here.
#[derive(Debug, Clone, Default)]
pub struct GlobBuilder {
    _private: (),
}

impl GlobBuilder {
    /// Equivalent to [`GlobBuilder::default`].
    #[must_use]
    pub fn new() -> GlobBuilder {
        GlobBuilder::default()
    }

    /// Compiles `pattern` as a shell glob.
    ///
    /// Returns a structured [`Error`] describing the first problem found
    /// in the pattern — the same [`ErrorKind`] variants POSIX-ERE parsing
    /// uses, since glob patterns share the bracket-expression grammar and
    /// the group-nesting depth cap.
    ///
    /// `!(p)` negation is supported only as the *entire* pattern (e.g.
    /// `"!(foo|bar)"`), per `docs/GLOB_DESIGN.md`'s restricted-v1 plan:
    /// glob matching is always full-string, so a whole-pattern `!(p)` is
    /// just the boolean complement of matching `p`, computed by compiling
    /// `p` on its own and negating [`Glob::matches`]'s result — no NFA
    /// complement needed. `!(p)` anywhere else (embedded in a larger
    /// pattern, or nested inside another extglob group) is a compile
    /// error ([`ErrorKind::EmbeddedGlobNegation`]) rather than silently
    /// mismatching — the general embedded case needs a forbidden-spans
    /// refinement loop the design doc defers to a later round.
    pub fn build(&self, pattern: &str) -> Result<Glob, Error> {
        if let Some(rest) = pattern.strip_prefix("!(") {
            // `char_pos` starts biased by 2 (for "!(") purely so error
            // positions this sub-parse reports land on the right offset
            // into the *original* pattern; byte-slicing stays correct
            // because it only ever indexes into `rest`.
            let mut p = Parser {
                pattern: rest,
                byte_pos: 0,
                char_pos: 2,
                depth: 1,
            };
            let (inner, _depth) = p.alternation()?;
            if !p.eat(')') || p.peek().is_some() {
                // Either the negation group never closed, or there's more
                // pattern after it — either way this isn't "the entire
                // pattern is `!(...)`", so it falls under the unsupported
                // embedded case rather than a silent (wrong) parse.
                return Err(Error::new(ErrorKind::EmbeddedGlobNegation, Some(0)));
            }
            let wrapped = Ast::Concat(vec![Ast::StartAnchor, inner, Ast::EndAnchor]);
            let program = Arc::new(compile::compile(wrapped, false, false)?);
            return Ok(Glob {
                compiled: Compiled::Negated(program),
            });
        }
        let ast = parse(pattern)?;
        // Glob matching is always full-string (unlike `Regex`, which
        // searches): wrapping in `^...$` gets that for free from the
        // existing anchored-match compilation path, with no separate
        // "full match" mode needed in the VM.
        let wrapped = Ast::Concat(vec![Ast::StartAnchor, ast, Ast::EndAnchor]);
        let program = Arc::new(compile::compile(wrapped, false, false)?);
        Ok(Glob {
            compiled: Compiled::Positive(program),
        })
    }
}

/// A compiled shell glob pattern (`fnmatch`-style: `?`, `*`, `[...]`, the
/// bash extglob operators `@()` `?()` `*()` `+()`, and whole-pattern
/// `!()` negation).
///
/// Unlike [`crate::Regex`], matching is always a full match against the
/// whole input — glob patterns describe an entire name, not a substring
/// to search for.
#[derive(Clone)]
pub struct Glob {
    compiled: Compiled,
}

#[derive(Clone)]
enum Compiled {
    /// Matches iff the program matches.
    Positive(Arc<Program>),
    /// Matches iff the program does *not* match — a whole-pattern `!(p)`.
    Negated(Arc<Program>),
}

/// Shows the compiled program is opaque — there's no source pattern
/// string kept around to display (unlike [`crate::Regex`]'s `Debug`).
impl std::fmt::Debug for Glob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Glob").finish_non_exhaustive()
    }
}

impl Glob {
    /// Compiles `pattern` with default options.
    ///
    /// Equivalent to `GlobBuilder::new().build(pattern)`; see
    /// [`GlobBuilder`] for non-default matching modes.
    ///
    /// ```
    /// use rusty_regx::Glob;
    ///
    /// let g = Glob::new("*.txt")?;
    /// assert!(g.matches("notes.txt"));
    /// assert!(!g.matches("notes.txt.bak"));
    ///
    /// let extglob = Glob::new("@(foo|bar).txt")?;
    /// assert!(extglob.matches("foo.txt"));
    /// assert!(!extglob.matches("baz.txt"));
    ///
    /// let negated = Glob::new("!(foo|bar)")?;
    /// assert!(negated.matches("baz"));
    /// assert!(!negated.matches("foo"));
    /// # Ok::<(), rusty_regx::Error>(())
    /// ```
    pub fn new(pattern: &str) -> Result<Glob, Error> {
        GlobBuilder::new().build(pattern)
    }

    /// Whether `name` matches this pattern.
    ///
    /// Glob patterns always match the *whole* string — there is no
    /// substring-search mode, unlike [`crate::Regex::is_match`].
    pub fn matches(&self, name: &str) -> bool {
        let (program, negate) = match &self.compiled {
            Compiled::Positive(program) => (program, false),
            Compiled::Negated(program) => (program, true),
        };
        let matched =
            crate::SCRATCH.with(|s| crate::vm::exec_bool(program, name, 0, &mut s.borrow_mut()));
        matched != negate
    }
}

/// Parses a glob pattern into the shared ERE [`Ast`] (unanchored — the
/// caller wraps `^...$`).
fn parse(pattern: &str) -> Result<Ast, Error> {
    let mut p = Parser {
        pattern,
        byte_pos: 0,
        char_pos: 0,
        depth: 0,
    };
    let (ast, _depth) = p.concat(false)?;
    Ok(ast)
}

/// Negation characters glob accepts inside `[...]` — `!` alongside the
/// POSIX `^` (bash accepts both).
const NEGATION_CHARS: [char; 2] = ['^', '!'];

struct Parser<'p> {
    pattern: &'p str,
    byte_pos: usize,
    char_pos: usize,
    /// Extglob group-nesting recursion depth, checked eagerly at each `(`
    /// (mirrors `parser::Parser::depth`) so pathological nesting like
    /// `@(@(@(@(…` can't overflow this parser's own call stack before the
    /// depth cap is ever consulted.
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

    /// `alternation := concat ('|' concat)*` — only meaningful inside an
    /// extglob group's parentheses; plain glob has no top-level
    /// alternation (a bare `|` outside `@()`/`?()`/`*()`/`+()` is literal).
    fn alternation(&mut self) -> Result<(Ast, u32), Error> {
        let mut branches = vec![self.concat(true)?];
        while self.eat('|') {
            branches.push(self.concat(true)?);
        }
        let depth = branches.iter().map(|(_, d)| *d).max().unwrap_or(0);
        let ast = if branches.len() == 1 {
            branches.pop().unwrap().0
        } else {
            Ast::Alternation(branches.into_iter().map(|(ast, _)| ast).collect())
        };
        Ok((ast, depth))
    }

    /// `concat := atom*`. At top level (`in_group == false`) this consumes
    /// the whole pattern, treating `|` and `)` as literal characters (there
    /// is nothing for them to delimit outside an extglob group). Inside a
    /// group, stops at `|` or `)` so [`Parser::alternation`] and
    /// [`Parser::extglob_group`] can see them.
    fn concat(&mut self, in_group: bool) -> Result<(Ast, u32), Error> {
        let mut items: Vec<(Ast, u32)> = Vec::new();
        loop {
            match self.peek() {
                None => break,
                Some('|') | Some(')') if in_group => break,
                _ => {}
            }
            items.push(self.atom()?);
        }
        let depth = items.iter().map(|(_, d)| *d).max().unwrap_or(0);
        let ast = match items.len() {
            0 => Ast::Empty,
            1 => items.pop().unwrap().0,
            _ => Ast::Concat(items.into_iter().map(|(ast, _)| ast).collect()),
        };
        Ok((ast, depth))
    }

    fn atom(&mut self) -> Result<(Ast, u32), Error> {
        let c = self.peek().expect("atom() called with input remaining");
        // Extglob operator: `@`/`?`/`*`/`+` immediately followed by `(`.
        if matches!(c, '@' | '?' | '*' | '+') && self.peek_at(1) == Some('(') {
            return self.extglob_group(c);
        }
        // `!(...)` reaching here means it's *not* the whole top-level
        // pattern (that case is intercepted in `GlobBuilder::build` before
        // any `Parser` runs) — restricted-v1 doesn't support it embedded,
        // however deeply, so reject clearly instead of parsing `!` as a
        // literal and `(...)` as something it isn't.
        if c == '!' && self.peek_at(1) == Some('(') {
            return Err(Error::new(
                ErrorKind::EmbeddedGlobNegation,
                Some(self.char_pos),
            ));
        }
        match c {
            '?' => {
                self.bump();
                Ok((Ast::AnyChar, 0))
            }
            '*' => {
                self.bump();
                Ok((
                    Ast::Repeat {
                        ast: Box::new(Ast::AnyChar),
                        min: 0,
                        max: None,
                        slot: 0,
                    },
                    0,
                ))
            }
            '[' => {
                let open = self.char_pos;
                self.bump();
                let mut cursor = bracket::Cursor::new(self.pattern, self.byte_pos, self.char_pos);
                let class = bracket::parse(&mut cursor, open, &NEGATION_CHARS)?;
                self.byte_pos = cursor.byte_pos;
                self.char_pos = cursor.char_pos;
                Ok((Ast::Class(class), 0))
            }
            '\\' => {
                self.bump();
                match self.bump() {
                    Some(c) => Ok((Ast::Char(c), 0)),
                    None => Err(Error::new(
                        ErrorKind::TrailingBackslash,
                        Some(self.char_pos),
                    )),
                }
            }
            _ => {
                self.bump();
                Ok((Ast::Char(c), 0))
            }
        }
    }

    /// Parses `@(...)` / `?(...)` / `*(...)` / `+(...)` (the leading
    /// operator char has been peeked, not consumed) into the matching AST
    /// node — see `docs/GLOB_DESIGN.md`'s "Translation" table.
    fn extglob_group(&mut self, op: char) -> Result<(Ast, u32), Error> {
        let open = self.char_pos;
        self.bump(); // op
        self.bump(); // '('
        self.depth += 1;
        if self.depth > parser::MAX_NESTING_DEPTH {
            return Err(Error::new(ErrorKind::NestingTooDeep, Some(open)));
        }
        let (inner, inner_depth) = self.alternation()?;
        if !self.eat(')') {
            return Err(Error::new(ErrorKind::UnbalancedParenthesis, Some(open)));
        }
        self.depth -= 1;
        let depth = inner_depth + 1;
        parser::check_depth(depth, open)?;
        let ast = match op {
            '@' => inner,
            '*' => Ast::Repeat {
                ast: Box::new(inner),
                min: 0,
                max: None,
                slot: 0,
            },
            '+' => Ast::Repeat {
                ast: Box::new(inner),
                min: 1,
                max: None,
                slot: 0,
            },
            '?' => Ast::Repeat {
                ast: Box::new(inner),
                min: 0,
                max: Some(1),
                slot: 0,
            },
            _ => unreachable!("caller only dispatches here for @ ? * +"),
        };
        Ok((ast, depth))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, name: &str) -> bool {
        Glob::new(pattern).unwrap().matches(name)
    }

    fn err(pattern: &str) -> ErrorKind {
        Glob::new(pattern).unwrap_err().kind()
    }

    #[test]
    fn literals() {
        assert!(matches("hello", "hello"));
        assert!(!matches("hello", "hell"));
        assert!(!matches("hello", "hello!"));
        assert!(matches("", ""));
        assert!(!matches("", "x"));
    }

    #[test]
    fn escaped_metachar_is_literal() {
        assert!(matches(r"a\*b", "a*b"));
        assert!(!matches(r"a\*b", "aXb"));
        assert!(matches(r"a\?b", "a?b"));
    }

    #[test]
    fn question_mark_matches_exactly_one_char() {
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "ac"));
        assert!(!matches("a?c", "abbc"));
    }

    #[test]
    fn star_matches_any_run_including_empty() {
        assert!(matches("a*c", "ac"));
        assert!(matches("a*c", "abc"));
        assert!(matches("a*c", "abbbbbc"));
        assert!(matches("*", ""));
        assert!(matches("*", "anything at all"));
        assert!(!matches("a*c", "abcd"));
    }

    #[test]
    fn star_is_full_string_not_search() {
        // Unlike `Regex`, a bare literal glob pattern must match the whole
        // name, not just contain it.
        assert!(!matches("bc", "abc"));
        assert!(matches("*bc", "abc"));
    }

    #[test]
    fn bracket_class_and_negation() {
        assert!(matches("[abc]", "b"));
        assert!(!matches("[abc]", "d"));
        assert!(matches("[!abc]", "d"));
        assert!(matches("[^abc]", "d"));
        assert!(!matches("[!abc]", "a"));
        assert!(matches("[a-z]", "m"));
        assert!(!matches("[a-z]", "M"));
    }

    #[test]
    fn bracket_posix_class() {
        assert!(matches("[[:digit:]]", "7"));
        assert!(!matches("[[:digit:]]", "x"));
    }

    #[test]
    fn bracket_literal_close_and_trailing_dash() {
        assert!(matches("[]a]", "]"));
        assert!(matches("[]a]", "a"));
        assert!(matches("[a-]", "-"));
    }

    #[test]
    fn combined_pattern() {
        assert!(matches("[Rr]eadme*.md", "Readme.md"));
        assert!(matches("[Rr]eadme*.md", "readme.notes.md"));
        assert!(!matches("[Rr]eadme*.md", "README.md"));
        assert!(!matches("[Rr]eadme*.md", "readme.txt"));
    }

    #[test]
    fn extglob_at_is_alternation() {
        assert!(matches("@(foo|bar)", "foo"));
        assert!(matches("@(foo|bar)", "bar"));
        assert!(!matches("@(foo|bar)", "baz"));
        assert!(!matches("@(foo|bar)", "foobar"));
    }

    #[test]
    fn extglob_star_is_zero_or_more() {
        assert!(matches("*(ab)", ""));
        assert!(matches("*(ab)", "ab"));
        assert!(matches("*(ab)", "ababab"));
        assert!(!matches("*(ab)", "aba"));
    }

    #[test]
    fn extglob_plus_is_one_or_more() {
        assert!(!matches("+(ab)", ""));
        assert!(matches("+(ab)", "ab"));
        assert!(matches("+(ab)", "ababab"));
    }

    #[test]
    fn extglob_question_is_zero_or_one() {
        assert!(matches("?(ab)", ""));
        assert!(matches("?(ab)", "ab"));
        assert!(!matches("?(ab)", "abab"));
    }

    #[test]
    fn extglob_single_alternative_needs_no_pipe() {
        assert!(matches("@(ab)", "ab"));
        assert!(!matches("@(ab)", "ac"));
    }

    #[test]
    fn extglob_composes_with_surrounding_literals_and_classes() {
        assert!(matches("file.@(txt|md)", "file.txt"));
        assert!(matches("file.@(txt|md)", "file.md"));
        assert!(!matches("file.@(txt|md)", "file.rs"));
        assert!(matches("[Rr]eadme.*(bak)", "Readme."));
        assert!(matches("[Rr]eadme.*(bak)", "Readme.bakbak"));
    }

    #[test]
    fn extglob_nests() {
        assert!(matches("@(a|@(b|c))", "a"));
        assert!(matches("@(a|@(b|c))", "b"));
        assert!(matches("@(a|@(b|c))", "c"));
        assert!(!matches("@(a|@(b|c))", "d"));
        assert!(matches("*(a|@(bc)*)", "abcbc"));
    }

    #[test]
    fn extglob_operator_char_is_literal_without_paren() {
        // `@`/`+` (and `?`/`*` without a following `(`) outside extglob
        // context keep their ordinary meaning: `@`/`+` are plain literals,
        // `?`/`*` are the classic single-char/any-run wildcards.
        assert!(matches("a@b", "a@b"));
        assert!(matches("a+b", "a+b"));
        assert!(!matches("a@b", "ab"));
    }

    #[test]
    fn negation_whole_pattern() {
        assert!(matches("!(foo|bar)", "baz"));
        assert!(!matches("!(foo|bar)", "foo"));
        assert!(!matches("!(foo|bar)", "bar"));
        assert!(matches("!(foo)", "bar"));
        assert!(!matches("!(foo)", "foo"));
    }

    #[test]
    fn negation_single_alternative_needs_no_pipe() {
        assert!(matches("!(ab)", "ac"));
        assert!(!matches("!(ab)", "ab"));
    }

    #[test]
    fn negation_of_empty_matches_any_nonempty_string() {
        assert!(matches("!()", "anything"));
        assert!(!matches("!()", ""));
    }

    #[test]
    fn negation_composes_with_other_atoms_inside_the_group() {
        assert!(matches("!(a*|b?)", "c"));
        assert!(matches("!(a*|b?)", "bxx"));
        assert!(!matches("!(a*|b?)", "abc"));
        assert!(!matches("!(a*|b?)", "bx"));
    }

    #[test]
    fn negation_must_be_the_entire_pattern() {
        // Embedded (anywhere but as the whole pattern) is unsupported in
        // restricted v1 — a clear error, not a silent wrong parse.
        assert_eq!(err("a!(b)c"), ErrorKind::EmbeddedGlobNegation);
        assert_eq!(err("!(a)b"), ErrorKind::EmbeddedGlobNegation);
        assert_eq!(err("a!(b)"), ErrorKind::EmbeddedGlobNegation);
        assert_eq!(err("@(!(a)|b)"), ErrorKind::EmbeddedGlobNegation);
        assert_eq!(err("*(!(a))"), ErrorKind::EmbeddedGlobNegation);
        assert_eq!(err("!(!(a))"), ErrorKind::EmbeddedGlobNegation);
    }

    #[test]
    fn negation_unterminated_group_is_an_error() {
        assert_eq!(err("!(abc"), ErrorKind::EmbeddedGlobNegation);
    }

    #[test]
    fn bang_without_paren_is_literal() {
        assert!(matches("a!b", "a!b"));
        assert!(!matches("a!b", "ab"));
        assert!(matches("!", "!"));
    }

    #[test]
    fn errors() {
        assert_eq!(err("[abc"), ErrorKind::UnclosedBracket);
        assert_eq!(err(r"a\"), ErrorKind::TrailingBackslash);
        assert_eq!(err("[z-a]"), ErrorKind::InvalidRange);
        assert_eq!(err("[[:bogus:]]"), ErrorKind::InvalidPosixClass);
        assert_eq!(err("@(foo|bar"), ErrorKind::UnbalancedParenthesis);
        assert_eq!(err("@(foo"), ErrorKind::UnbalancedParenthesis);
    }

    #[test]
    fn deeply_nested_extglob_is_rejected() {
        let mut pattern = String::new();
        for _ in 0..300 {
            pattern.push_str("@(");
        }
        pattern.push('a');
        for _ in 0..300 {
            pattern.push(')');
        }
        assert_eq!(err(&pattern), ErrorKind::NestingTooDeep);
    }

    #[test]
    fn debug_does_not_panic() {
        let g = Glob::new("a*b").unwrap();
        let _ = format!("{g:?}");
        let _ = g.clone();
    }
}
