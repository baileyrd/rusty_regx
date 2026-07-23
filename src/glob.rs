//! Shell glob (`fnmatch`-style) pattern matching on the same linear-time
//! engine used for POSIX ERE — see `docs/GLOB_DESIGN.md` for the full
//! design and roadmap (tracking issue: #20).
//!
//! Glob syntax translates onto the existing ERE [`Ast`], so it reuses
//! every execution tier (literal path, prefix/suffix, anchors, Pike VM)
//! and inherits the same non-negotiable guarantee: no backtracking, ever.
//!
//! This module currently covers the foundation only: `?`, `*`,
//! `[...]`/`[!...]`, literals, and whole-pattern (full-string) matching.
//! Extglob operators (`@()` `?()` `*()` `+()`), `!()` negation, pathname
//! mode, leading-period rules, case-insensitivity, and prefix/suffix
//! matching are follow-up issues tracked under #20.

use crate::ast::Ast;
use crate::bracket;
use crate::compile::{self, Program};
use crate::error::{Error, ErrorKind};
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
    /// uses, since glob patterns share the bracket-expression grammar.
    pub fn build(&self, pattern: &str) -> Result<Glob, Error> {
        let ast = parse(pattern)?;
        // Glob matching is always full-string (unlike `Regex`, which
        // searches): wrapping in `^...$` gets that for free from the
        // existing anchored-match compilation path, with no separate
        // "full match" mode needed in the VM.
        let wrapped = Ast::Concat(vec![Ast::StartAnchor, ast, Ast::EndAnchor]);
        let program = Arc::new(compile::compile(wrapped, false, false)?);
        Ok(Glob { program })
    }
}

/// A compiled shell glob pattern (`fnmatch`-style: `?`, `*`, `[...]`).
///
/// Unlike [`crate::Regex`], matching is always a full match against the
/// whole input — glob patterns describe an entire name, not a substring
/// to search for.
#[derive(Clone)]
pub struct Glob {
    program: Arc<Program>,
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
        crate::SCRATCH.with(|s| crate::vm::exec_bool(&self.program, name, 0, &mut s.borrow_mut()))
    }
}

/// Parses a glob pattern into the shared ERE [`Ast`] (unanchored — the
/// caller wraps `^...$`).
fn parse(pattern: &str) -> Result<Ast, Error> {
    let mut p = Parser {
        pattern,
        byte_pos: 0,
        char_pos: 0,
    };
    let items = p.items()?;
    Ok(match items.len() {
        0 => Ast::Empty,
        1 => items.into_iter().next().expect("checked len == 1"),
        _ => Ast::Concat(items),
    })
}

/// Negation characters glob accepts inside `[...]` — `!` alongside the
/// POSIX `^` (bash accepts both).
const NEGATION_CHARS: [char; 2] = ['^', '!'];

struct Parser<'p> {
    pattern: &'p str,
    byte_pos: usize,
    char_pos: usize,
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

    /// `items := (atom)*` — a flat sequence; glob has no alternation or
    /// grouping outside the extglob operators (a later issue).
    fn items(&mut self) -> Result<Vec<Ast>, Error> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            let item = match c {
                '?' => {
                    self.bump();
                    Ast::AnyChar
                }
                '*' => {
                    self.bump();
                    Ast::Repeat {
                        ast: Box::new(Ast::AnyChar),
                        min: 0,
                        max: None,
                        slot: 0,
                    }
                }
                '[' => {
                    let open = self.char_pos;
                    self.bump();
                    let mut cursor =
                        bracket::Cursor::new(self.pattern, self.byte_pos, self.char_pos);
                    let class = bracket::parse(&mut cursor, open, &NEGATION_CHARS)?;
                    self.byte_pos = cursor.byte_pos;
                    self.char_pos = cursor.char_pos;
                    Ast::Class(class)
                }
                '\\' => {
                    self.bump();
                    match self.bump() {
                        Some(c) => Ast::Char(c),
                        None => {
                            return Err(Error::new(
                                ErrorKind::TrailingBackslash,
                                Some(self.char_pos),
                            ))
                        }
                    }
                }
                _ => {
                    self.bump();
                    Ast::Char(c)
                }
            };
            items.push(item);
        }
        Ok(items)
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
    fn errors() {
        assert_eq!(err("[abc"), ErrorKind::UnclosedBracket);
        assert_eq!(err(r"a\"), ErrorKind::TrailingBackslash);
        assert_eq!(err("[z-a]"), ErrorKind::InvalidRange);
        assert_eq!(err("[[:bogus:]]"), ErrorKind::InvalidPosixClass);
    }

    #[test]
    fn debug_does_not_panic() {
        let g = Glob::new("a*b").unwrap();
        let _ = format!("{g:?}");
        let _ = g.clone();
    }
}
