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
//!   (`REG_ICASE`, bash's `shopt -s nocasematch`). [`Regex::builder`]
//!   composes the modes and adds `REG_NEWLINE` line matching.
//! - GNU/glibc extensions are supported as bash accepts them: `\w` `\s`
//!   `\b` `\<` and friends — see the parser docs and
//!   `docs/FLAVORS.md`.
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

std::thread_local! {
    /// Reused by every one-shot call (`is_match`/`captures`/`find`) on
    /// this thread, making them allocation-free after warmup. The VM
    /// never calls user code, so the RefCell can't be re-entered.
    static SCRATCH: std::cell::RefCell<vm::Scratch> =
        std::cell::RefCell::new(vm::Scratch::default());
}

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

    /// Starts building a regex with non-default options — the general
    /// form of the `new_*` constructors, plus options they don't cover
    /// (currently [`RegexBuilder::newline`]).
    pub fn builder() -> RegexBuilder {
        RegexBuilder::default()
    }

    fn compile(pattern: &str, posix: bool, icase: bool) -> Result<Regex, Error> {
        Regex::compile_full(pattern, posix, icase, false)
    }

    fn compile_full(
        pattern: &str,
        posix: bool,
        icase: bool,
        newline: bool,
    ) -> Result<Regex, Error> {
        let ast = parser::parse(pattern)?;
        let program = compile::compile(ast, icase, newline)?;
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

    /// A human-readable rendering of how this pattern was compiled: the
    /// mode, the chosen execution strategy (literal substring path,
    /// extracted scan prefix / suffix, or the Pike VM), and the
    /// instruction listing. Intended for debugging "why doesn't this
    /// match?".
    ///
    /// The output format is **unstable** — it is not part of the semver
    /// contract and may change in any release.
    pub fn debug_dump(&self) -> String {
        use std::fmt::Write;
        let p = &self.program;
        let mut out = String::new();
        let _ = writeln!(out, "pattern: {:?}", self.pattern);
        let _ = writeln!(
            out,
            "mode: {}{}{}",
            if self.posix {
                "posix (leftmost-longest)"
            } else {
                "leftmost-first"
            },
            if p.icase { " + case-insensitive" } else { "" },
            if p.newline { " + newline" } else { "" },
        );
        if let Some(lit) = &p.literal {
            let _ = writeln!(
                out,
                "tier: literal substring path {:?} (anchored start: {}, end: {})",
                lit.s, lit.anchored_start, lit.anchored_end,
            );
            return out;
        }
        if !p.prefix.is_empty() {
            let _ = writeln!(out, "scan prefix: {:?}", p.prefix);
        }
        if !p.suffix.is_empty() {
            let _ = writeln!(
                out,
                "required suffix: {:?} (anchored: {})",
                p.suffix, p.suffix_anchored,
            );
        }
        let _ = writeln!(
            out,
            "tier: pike-vm ({} instructions, {} groups)",
            p.insts.len(),
            p.group_count,
        );
        for (i, inst) in p.insts.iter().enumerate() {
            let _ = writeln!(out, "{i:5}: {inst:?}");
        }
        out
    }

    /// Whether `text` contains a match, without computing capture groups.
    ///
    /// Equivalent to `self.captures(text).is_some()` but faster: the VM
    /// skips capture tracking entirely, and match *existence* does not
    /// depend on the match semantics, so this is the same single fast path
    /// in every mode (including POSIX).
    ///
    /// ```
    /// let re = rusty_regx::Regex::new("[0-9]+")?;
    /// assert!(re.is_match("build 42"));
    /// assert!(!re.is_match("no digits"));
    /// # Ok::<(), rusty_regx::Error>(())
    /// ```
    pub fn is_match(&self, text: &str) -> bool {
        SCRATCH.with(|s| vm::exec_bool(&self.program, text, 0, &mut s.borrow_mut()))
    }

    /// Searches `text` for the leftmost match, returning the capture groups.
    ///
    /// Group 0 is always the whole match. Groups that did not participate in
    /// the match report as absent via [`Captures::get`].
    pub fn captures<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        SCRATCH.with(|s| self.captures_at(text, 0, &mut s.borrow_mut()))
    }

    fn captures_at<'t>(
        &self,
        text: &'t str,
        from: usize,
        scratch: &mut vm::Scratch,
    ) -> Option<Captures<'t>> {
        let slots = if self.posix {
            vm::exec_posix(&self.program, text, self.program.slot_count, from, scratch)
        } else {
            vm::exec(&self.program, text, self.program.slot_count, from, scratch)
        };
        slots.map(|slots| Captures { text, slots })
    }

    fn find_at<'t>(
        &self,
        text: &'t str,
        from: usize,
        scratch: &mut vm::Scratch,
    ) -> Option<Match<'t>> {
        let slots = if self.posix {
            vm::exec_posix(&self.program, text, 2, from, scratch)
        } else {
            vm::exec(&self.program, text, 2, from, scratch)
        };
        slots
            .and_then(|slots| slots.first().copied().flatten())
            .map(|(start, end)| Match { text, start, end })
    }

    /// Iterates over all non-overlapping matches in `text`, leftmost
    /// first (leftmost-longest per match in the POSIX modes).
    ///
    /// Empty-match handling matches the `regex` crate: after a match the
    /// search resumes at its end; an empty match advances one `char`, and
    /// an empty match starting exactly where the previous match ended is
    /// skipped — so `a*` over `"aab"` yields `"aa"` then `""` at the end,
    /// never a zero-width match glued to `"aa"`.
    ///
    /// ```
    /// let re = rusty_regx::Regex::new("[0-9]+")?;
    /// let nums: Vec<&str> = re.find_iter("1, 22, 333").map(|m| m.as_str()).collect();
    /// assert_eq!(nums, ["1", "22", "333"]);
    /// # Ok::<(), rusty_regx::Error>(())
    /// ```
    pub fn find_iter<'r, 't>(&'r self, text: &'t str) -> FindIter<'r, 't> {
        FindIter {
            re: self,
            text,
            at: 0,
            last_match: None,
            scratch: vm::Scratch::default(),
        }
    }

    /// As [`Regex::find_iter`], yielding full [`Captures`] per match.
    ///
    /// ```
    /// let re = rusty_regx::Regex::new("([a-z]+)=([0-9]+)")?;
    /// let pairs: Vec<(&str, &str)> = re
    ///     .captures_iter("a=1 b=22")
    ///     .map(|c| (c.get(1).unwrap(), c.get(2).unwrap()))
    ///     .collect();
    /// assert_eq!(pairs, [("a", "1"), ("b", "22")]);
    /// # Ok::<(), rusty_regx::Error>(())
    /// ```
    pub fn captures_iter<'r, 't>(&'r self, text: &'t str) -> CapturesIter<'r, 't> {
        CapturesIter {
            re: self,
            text,
            at: 0,
            last_match: None,
            scratch: vm::Scratch::default(),
        }
    }

    /// Searches `text` for the leftmost match, returning only its
    /// location.
    ///
    /// Cheaper than [`Regex::captures`] when the groups aren't needed:
    /// the VM tracks only the overall match's two offsets, however many
    /// groups the pattern has. The span is the same one `captures` would
    /// report as group 0 (in every mode — POSIX leftmost-longest
    /// disambiguates group 0 by its own span first).
    ///
    /// ```
    /// let re = rusty_regx::Regex::new("[0-9]+")?;
    /// let m = re.find("build 42!").unwrap();
    /// assert_eq!((m.start(), m.end(), m.as_str()), (6, 8, "42"));
    /// # Ok::<(), rusty_regx::Error>(())
    /// ```
    pub fn find<'t>(&self, text: &'t str) -> Option<Match<'t>> {
        SCRATCH.with(|s| self.find_at(text, 0, &mut s.borrow_mut()))
    }

    /// The number of capture groups this pattern has, including group 0
    /// (the whole match) — the `len()` of any successful
    /// [`Regex::captures`] result.
    pub fn group_count(&self) -> usize {
        self.program.group_count
    }
}

/// `"pattern".parse::<Regex>()` — equivalent to [`Regex::new`].
impl std::str::FromStr for Regex {
    type Err = Error;

    fn from_str(pattern: &str) -> Result<Regex, Error> {
        Regex::new(pattern)
    }
}

/// Configures and compiles a [`Regex`] — the general form of the
/// `new_*` constructors (see [`Regex::builder`]).
///
/// ```
/// let re = rusty_regx::Regex::builder()
///     .posix(true)
///     .newline(true)
///     .build("^ab$")?;
/// assert!(re.is_match("x\nab\ny")); // ^/$ match at line boundaries
/// # Ok::<(), rusty_regx::Error>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct RegexBuilder {
    posix: bool,
    case_insensitive: bool,
    newline: bool,
}

impl RegexBuilder {
    /// Equivalent to [`RegexBuilder::default`].
    pub fn new() -> RegexBuilder {
        RegexBuilder::default()
    }

    /// POSIX leftmost-longest match semantics, as [`Regex::new_posix`].
    pub fn posix(mut self, yes: bool) -> RegexBuilder {
        self.posix = yes;
        self
    }

    /// `REG_ICASE` case-insensitive matching, as [`Regex::new_posix_ci`] /
    /// [`Regex::new_ci`].
    pub fn case_insensitive(mut self, yes: bool) -> RegexBuilder {
        self.case_insensitive = yes;
        self
    }

    /// POSIX `REG_NEWLINE` mode: `.` and negated bracket expressions do
    /// not match `\n`, and `^`/`$` also match right after/before one —
    /// grep-style line-oriented matching over multi-line text. bash's
    /// `=~` does *not* use this mode; it exists for grep-shaped
    /// consumers.
    pub fn newline(mut self, yes: bool) -> RegexBuilder {
        self.newline = yes;
        self
    }

    /// Compiles `pattern` with the configured options.
    pub fn build(&self, pattern: &str) -> Result<Regex, Error> {
        Regex::compile_full(pattern, self.posix, self.case_insensitive, self.newline)
    }
}

/// The location of a single match in the searched text (see
/// [`Regex::find`]).
#[derive(Debug, Clone, Copy)]
pub struct Match<'t> {
    text: &'t str,
    start: usize,
    end: usize,
}

impl<'t> Match<'t> {
    /// The match's starting byte offset (on a `char` boundary).
    pub fn start(&self) -> usize {
        self.start
    }

    /// The match's ending byte offset (exclusive, on a `char` boundary).
    pub fn end(&self) -> usize {
        self.end
    }

    /// The match's byte range, `start()..end()`.
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }

    /// The matched text.
    pub fn as_str(&self) -> &'t str {
        &self.text[self.start..self.end]
    }

    /// The match's length in bytes.
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Whether this is a zero-width match (empty matches are legal and
    /// handled specially by the iteration APIs).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// The shared iteration step (the `regex` crate's rule): resume at the
/// match's end; an empty match advances one `char` and is *skipped*
/// entirely when it starts exactly where the previous match ended.
/// Returns the span to yield, updating `at`/`last_match`.
fn iter_step(
    re: &Regex,
    text: &str,
    at: &mut usize,
    last_match: &mut Option<usize>,
    scratch: &mut vm::Scratch,
) -> Option<(usize, usize)> {
    loop {
        if *at > text.len() {
            return None;
        }
        let m = re.find_at(text, *at, scratch)?;
        let (start, end) = (m.start, m.end);
        if start == end {
            // Advance one char past the empty match (never splitting one).
            *at = if end >= text.len() {
                text.len() + 1
            } else {
                end + text[end..].chars().next().map_or(1, char::len_utf8)
            };
            if Some(end) == *last_match {
                continue;
            }
        } else {
            *at = end;
        }
        *last_match = Some(end);
        return Some((start, end));
    }
}

/// An iterator over all non-overlapping matches (see
/// [`Regex::find_iter`]). VM working buffers are reused across matches.
#[derive(Debug)]
pub struct FindIter<'r, 't> {
    re: &'r Regex,
    text: &'t str,
    at: usize,
    last_match: Option<usize>,
    scratch: vm::Scratch,
}

impl<'t> Iterator for FindIter<'_, 't> {
    type Item = Match<'t>;

    fn next(&mut self) -> Option<Match<'t>> {
        let (start, end) = iter_step(
            self.re,
            self.text,
            &mut self.at,
            &mut self.last_match,
            &mut self.scratch,
        )?;
        Some(Match {
            text: self.text,
            start,
            end,
        })
    }
}

/// An iterator over all non-overlapping matches with full captures (see
/// [`Regex::captures_iter`]).
#[derive(Debug)]
pub struct CapturesIter<'r, 't> {
    re: &'r Regex,
    text: &'t str,
    at: usize,
    last_match: Option<usize>,
    scratch: vm::Scratch,
}

impl<'t> Iterator for CapturesIter<'_, 't> {
    type Item = Captures<'t>;

    fn next(&mut self) -> Option<Captures<'t>> {
        // Locate via the cheap group-0 path (including the crate's
        // empty-match rule), then run full captures anchored at the
        // match we already found.
        let (start, _) = iter_step(
            self.re,
            self.text,
            &mut self.at,
            &mut self.last_match,
            &mut self.scratch,
        )?;
        self.re.captures_at(self.text, start, &mut self.scratch)
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
    ///
    /// ```
    /// let re = rusty_regx::Regex::new("(b)(x)?c")?;
    /// let caps = re.captures("abc").unwrap();
    /// assert_eq!(caps.span(1), Some((1, 2)));
    /// assert_eq!(caps.span(2), None); // did not participate
    /// # Ok::<(), rusty_regx::Error>(())
    /// ```
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
///
/// ```
/// assert_eq!(rusty_regx::escape("1+1=2?"), r"1\+1=2\?");
/// let re = rusty_regx::Regex::new(&rusty_regx::escape("a.b"))?;
/// assert!(re.is_match("xa.by") && !re.is_match("xaXby"));
/// # Ok::<(), rusty_regx::Error>(())
/// ```
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
