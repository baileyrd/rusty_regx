//! A minimal, linear-time POSIX-ERE regex engine.
//!
//! `rusty_regx` implements POSIX Extended Regular Expressions — and nothing
//! more — as a drop-in replacement for the small slice of the `regex` crate
//! used by [`rush`](https://github.com/baileyrd/rush)'s `[[ $s =~ pattern ]]`
//! conditional.
//!
//! # Guarantees
//!
//! - **Linear-time matching.** Patterns are executed on a Pike VM
//!   (breadth-first NFA simulation); there is no backtracking, so
//!   pathological patterns like `(a+)+b` cannot hang the caller.
//! - **Zero dependencies** and no `unsafe` code.
//!
//! # Semantics
//!
//! - Matching is an *unanchored search* (like bash `=~`), implemented via an
//!   implicit non-greedy prefix.
//! - v1 uses leftmost-first (Perl-style) match semantics, identical to the
//!   `regex` crate. POSIX leftmost-longest is planned as a v2 opt-in mode.
//!
//! # Example
//!
//! ```
//! use rusty_regx::Regex;
//!
//! let re = Regex::new("^([[:alpha:]]+)-([0-9]{2,4})$")?;
//! let caps = re.captures("release-2026").unwrap();
//! assert_eq!(caps.get(0), Some("release-2026"));
//! assert_eq!(caps.get(1), Some("release"));
//! assert_eq!(caps.get(2), Some("2026"));
//! # Ok::<(), rusty_regx::Error>(())
//! ```
//!
//! See `DESIGN.md` in the repository for the full design and roadmap.

#![forbid(unsafe_code)]

mod ast;
mod compile;
mod error;
mod parser;
mod vm;

pub use error::Error;

/// A compiled regular expression program.
#[derive(Debug)]
pub struct Regex {
    program: compile::Program,
    posix: bool,
}

impl Regex {
    /// Compiles a POSIX-ERE pattern with leftmost-first (Perl-style) match
    /// semantics — identical to the `regex` crate's behavior.
    ///
    /// Returns a structured [`Error`] describing the first problem found in
    /// the pattern.
    pub fn new(pattern: &str) -> Result<Regex, Error> {
        Self::compile(pattern, false)
    }

    /// Compiles a POSIX-ERE pattern with POSIX leftmost-longest match
    /// semantics — what real bash/glibc `=~` reports (v2 opt-in mode).
    ///
    /// Where [`Regex::new`] matches `a|ab` against `"ab"` as `"a"` (first
    /// alternative wins), this mode matches `"ab"` (longest wins).
    /// Submatches use leftmost-longest disambiguation per group in index
    /// order. Still linear-space and polynomial-time — never backtracking.
    pub fn new_posix(pattern: &str) -> Result<Regex, Error> {
        Self::compile(pattern, true)
    }

    fn compile(pattern: &str, posix: bool) -> Result<Regex, Error> {
        let ast = parser::parse(pattern)?;
        let program = compile::compile(&ast)?;
        Ok(Regex { program, posix })
    }

    /// Searches `text` for the leftmost match, returning the capture groups.
    ///
    /// Group 0 is always the whole match. Groups that did not participate in
    /// the match report as absent via [`Captures::get`].
    pub fn captures<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        let slots = if self.posix {
            vm::exec_posix(&self.program, text)
        } else {
            vm::exec(&self.program, text)
        };
        slots.map(|slots| Captures { text, slots })
    }
}

/// The capture groups of a single successful match.
///
/// Group 0 is the whole match; groups 1..len() correspond to the pattern's
/// parenthesized groups in order of their opening parenthesis.
#[derive(Debug)]
pub struct Captures<'t> {
    text: &'t str,
    slots: Vec<Option<(usize, usize)>>,
}

impl<'t> Captures<'t> {
    /// The number of capture groups, including group 0 (the whole match).
    ///
    /// This is determined by the pattern, not the input: groups that did not
    /// participate in the match are still counted.
    #[allow(clippy::len_without_is_empty)] // never empty: group 0 always exists
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// The text matched by group `i`, or `None` if the group did not
    /// participate in the match (or `i` is out of range).
    pub fn get(&self, i: usize) -> Option<&'t str> {
        self.slots
            .get(i)
            .copied()
            .flatten()
            .map(|(start, end)| &self.text[start..end])
    }
}

/// Escapes `text` so it matches itself literally under this engine.
///
/// Exactly this engine's metacharacters are escaped: `^ $ . [ ] ( ) | * + ? { } \`.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '^' | '$' | '.' | '[' | ']' | '(' | ')' | '|' | '*' | '+' | '?' | '{' | '}' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_leaves_plain_text_alone() {
        assert_eq!(escape("hello world"), "hello world");
        assert_eq!(escape(""), "");
        assert_eq!(escape("héllo"), "héllo");
    }

    #[test]
    fn escape_escapes_every_metacharacter() {
        assert_eq!(escape(r"^$.[]()|*+?{}\"), r"\^\$\.\[\]\(\)\|\*\+\?\{\}\\");
        assert_eq!(escape("a.b*c"), r"a\.b\*c");
    }
}
