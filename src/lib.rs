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
//! - [`Regex::new`] uses leftmost-first (Perl-style) match semantics,
//!   identical to the `regex` crate. [`Regex::new_posix`] opts into POSIX
//!   leftmost-longest semantics — what real bash/glibc report — and
//!   [`Regex::new_posix_ci`] adds case-insensitive matching on top
//!   (`REG_ICASE`, bash's `shopt -s nocasematch`).
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
#![warn(missing_docs)]

mod ast;
mod compile;
mod error;
mod parser;
mod vm;

pub use error::{Error, ErrorKind};

/// A compiled regular expression program.
#[derive(Clone)]
pub struct Regex {
    pattern: Box<str>,
    program: compile::Program,
    posix: bool,
}

/// Shows the original pattern, like the `regex` crate — the compiled
/// bytecode is an implementation detail.
impl std::fmt::Debug for Regex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Regex").field(&self.pattern).finish()
    }
}

/// Displays the original pattern.
impl std::fmt::Display for Regex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.pattern)
    }
}

impl Regex {
    /// Compiles a POSIX-ERE pattern with leftmost-first (Perl-style) match
    /// semantics — identical to the `regex` crate's behavior.
    ///
    /// Returns a structured [`Error`] describing the first problem found in
    /// the pattern.
    pub fn new(pattern: &str) -> Result<Regex, Error> {
        Self::compile(pattern, false, false)
    }

    /// Compiles a POSIX-ERE pattern with POSIX leftmost-longest match
    /// semantics — what real bash/glibc `=~` reports (v2 opt-in mode).
    ///
    /// Where [`Regex::new`] matches `a|ab` against `"ab"` as `"a"` (first
    /// alternative wins), this mode matches `"ab"` (longest wins).
    /// Submatches use leftmost-longest disambiguation per group in index
    /// order. Still linear-space and polynomial-time — never backtracking.
    pub fn new_posix(pattern: &str) -> Result<Regex, Error> {
        Self::compile(pattern, true, false)
    }

    /// As [`Regex::new_posix`], but ordinary-letter comparisons are
    /// case-insensitive (ASCII plus Unicode simple case folding) — POSIX
    /// `REG_ICASE`, which is what bash applies to `[[ =~ ]]` under
    /// `shopt -s nocasematch`.
    ///
    /// Folding happens per character at comparison time and matches glibc's
    /// `REG_ICASE` exactly (differentially verified against bash 5.2):
    ///
    /// - Pattern literals and input fold to uppercase, so `abc` matches
    ///   `"ABC"` and vice versa.
    /// - Range endpoints fold too: `[a-f]` also matches `A`–`F`, `[X-Z]`
    ///   also matches `x`–`z` — but `a` still does *not* match `[X-Z]`.
    ///   A range that is reversed after folding (e.g. `[Z-a]`) is an
    ///   [`ErrorKind::InvalidRange`], as glibc rejects it.
    /// - `[[:upper:]]` and `[[:lower:]]` both behave as `[[:alpha:]]` —
    ///   glibc's `REG_ICASE` rule, so `[[ ABC =~ [[:lower:]]bc ]]` matches
    ///   under `nocasematch` in real bash.
    /// - Folding affects comparison only: captures always report the
    ///   original input spans, so `^(a)` against `"ABC"` captures `"A"`.
    pub fn new_posix_ci(pattern: &str) -> Result<Regex, Error> {
        Self::compile(pattern, true, true)
    }

    /// As [`Regex::new`] (leftmost-first semantics), but case-insensitive.
    ///
    /// Folding is identical to [`Regex::new_posix_ci`]'s: `REG_ICASE`
    /// semantics — pattern literals, range endpoints, and input fold to
    /// uppercase; `[[:upper:]]`/`[[:lower:]]` behave as `[[:alpha:]]`;
    /// captures report the original input spans.
    pub fn new_ci(pattern: &str) -> Result<Regex, Error> {
        Self::compile(pattern, false, true)
    }

    fn compile(pattern: &str, posix: bool, icase: bool) -> Result<Regex, Error> {
        let ast = parser::parse(pattern)?;
        let program = compile::compile(ast, icase)?;
        Ok(Regex {
            pattern: pattern.into(),
            program,
            posix,
        })
    }

    /// The pattern this regex was compiled from.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Whether `text` contains a match, without computing capture groups.
    ///
    /// Equivalent to `self.captures(text).is_some()` but faster: the VM
    /// skips capture tracking entirely, and match *existence* does not
    /// depend on the match semantics, so this is the same single fast path
    /// in every mode (including POSIX).
    pub fn is_match(&self, text: &str) -> bool {
        vm::exec_bool(&self.program, text)
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
        self.span(i).map(|(start, end)| &self.text[start..end])
    }

    /// The byte span `(start, end)` of group `i` in the searched text, or
    /// `None` if the group did not participate in the match (or `i` is out
    /// of range). Both offsets fall on `char` boundaries.
    pub fn span(&self, i: usize) -> Option<(usize, usize)> {
        self.slots.get(i).copied().flatten()
    }

    /// Iterates over all groups (group 0 first): `Some(text)` for groups
    /// that participated in the match, `None` for those that did not.
    pub fn iter(&self) -> impl Iterator<Item = Option<&'t str>> + '_ {
        (0..self.len()).map(|i| self.get(i))
    }
}

/// `caps[i]` is the text of group `i`.
///
/// # Panics
///
/// If group `i` did not participate in the match or is out of range; use
/// [`Captures::get`] for a fallible lookup.
impl std::ops::Index<usize> for Captures<'_> {
    type Output = str;

    fn index(&self, i: usize) -> &str {
        self.get(i)
            .unwrap_or_else(|| panic!("no capture group {i}"))
    }
}

/// Escapes `text` so it matches itself literally under this engine.
///
/// Exactly this engine's metacharacters are escaped: `^ $ . [ ] ( ) | * + ? { } \`.
/// Borrows the input unchanged when it contains none of them — the common
/// case for shell words — so no allocation happens.
pub fn escape(text: &str) -> std::borrow::Cow<'_, str> {
    let is_meta = |c: char| {
        matches!(
            c,
            '^' | '$' | '.' | '[' | ']' | '(' | ')' | '|' | '*' | '+' | '?' | '{' | '}' | '\\'
        )
    };
    if !text.chars().any(is_meta) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        if is_meta(c) {
            out.push('\\');
        }
        out.push(c);
    }
    std::borrow::Cow::Owned(out)
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

    #[test]
    fn escape_borrows_when_nothing_to_escape() {
        assert!(matches!(
            escape("hello"),
            std::borrow::Cow::Borrowed("hello")
        ));
        assert!(matches!(escape("a.b"), std::borrow::Cow::Owned(_)));
    }
}
