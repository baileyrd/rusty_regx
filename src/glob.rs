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
//! `+()`, `!()` negation restricted to the whole pattern, `pathname`/
//! `period` matching modes, and case-insensitivity (see [`GlobBuilder`]).
//! Prefix/suffix matching is a follow-up issue tracked under #20.

use crate::ast::{Ast, Class};
use crate::bracket;
use crate::compile::{self, Program};
use crate::error::{Error, ErrorKind};
use crate::parser;
use std::sync::Arc;

/// Builds a [`Glob`] with non-default options.
#[derive(Debug, Clone, Default)]
pub struct GlobBuilder {
    pathname: bool,
    period: bool,
    case_insensitive: bool,
}

impl GlobBuilder {
    /// Equivalent to [`GlobBuilder::default`].
    #[must_use]
    pub fn new() -> GlobBuilder {
        GlobBuilder::default()
    }

    /// Pathname mode (`fnmatch`'s `FNM_PATHNAME`): `?`, `*`, and bracket
    /// expressions never match `/` — only a literal `/` in the pattern
    /// matches one in the input. `AnyChar`-driven atoms compile as `[^/]`
    /// instead of `.`; bracket expressions get `/` added to their
    /// exclusions (see [`Glob`] for how this composes with `!(p)`
    /// negation and extglob groups — it applies uniformly throughout the
    /// whole pattern, not just at the top level).
    ///
    /// A positive (non-negated) bracket expression that includes `/` only
    /// via `[[:punct:]]`/`[[:print:]]`/`[[:graph:]]` (the only POSIX
    /// classes that can) can't be losslessly narrowed and is a compile
    /// error ([`ErrorKind::GlobClassExclusionUnsupported`]) — restricted
    /// v1, same spirit as `!(p)`'s.
    #[must_use]
    pub fn pathname(mut self, yes: bool) -> GlobBuilder {
        self.pathname = yes;
        self
    }

    /// Leading-period mode (`fnmatch`'s `FNM_PERIOD`): a `.` at the very
    /// start of the pattern's target string only matches an explicit
    /// literal `.` at the start of the pattern — `*`, `?`, and bracket
    /// expressions never match a leading dot, matching bash's default
    /// "hidden files aren't globbed" behavior (`dotglob` off).
    ///
    /// Restricted v1: only the pattern's literal, unambiguous first atom
    /// is checked — a plain char, `?`, or `[...]`/`[!...]`. A pattern
    /// that *starts* with `*` or an extglob group is a compile error
    /// ([`ErrorKind::GlobLeadingPeriodUnsupported`]) rather than a silent
    /// under-restriction, since whether such a pattern's first *matched*
    /// character is really the pattern's first *written* atom depends on
    /// how much the repetition/group chooses to consume at match time —
    /// not something this compile-time rewrite can decide. Write the
    /// pattern with an explicit leading `.` (`.*`) if it should also
    /// match dotfiles.
    ///
    /// Only applies to the pattern as a whole — not inside a whole-pattern
    /// `!(p)` negation (`docs/GLOB_DESIGN.md`'s `!(p)` restricted v1 and
    /// `period` mode don't yet compose; `pathname` mode does still apply
    /// inside `!(p)`, since it's plain character-class semantics).
    #[must_use]
    pub fn period(mut self, yes: bool) -> GlobBuilder {
        self.period = yes;
        self
    }

    /// Case-insensitive matching — `REG_ICASE` semantics, identical to
    /// [`crate::Regex::new_ci`]/[`crate::Regex::new_posix_ci`]'s: pattern
    /// literals, bracket-expression range endpoints, and the input all
    /// fold to uppercase (glibc's model, not simple lowercasing — see
    /// [`crate::Regex::new_posix_ci`]'s doc comment for the corner cases
    /// this gets right that naive lowercasing doesn't). Folding is
    /// applied by the same compiler stage `Regex` uses, so it composes
    /// with everything else in this module — `pathname`/`period`'s `/`/`.`
    /// exclusions, extglob groups, and `!(p)` negation — for free.
    #[must_use]
    pub fn case_insensitive(mut self, yes: bool) -> GlobBuilder {
        self.case_insensitive = yes;
        self
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
                pathname: self.pathname,
            };
            let (inner, _depth) = p.alternation()?;
            if !p.eat(')') || p.peek().is_some() {
                // Either the negation group never closed, or there's more
                // pattern after it — either way this isn't "the entire
                // pattern is `!(...)`", so it falls under the unsupported
                // embedded case rather than a silent (wrong) parse.
                return Err(Error::new(ErrorKind::EmbeddedGlobNegation, Some(0)));
            }
            // `period` doesn't (yet) compose with whole-pattern `!(p)` —
            // see `GlobBuilder::period`'s doc comment.
            let wrapped = Ast::Concat(vec![Ast::StartAnchor, inner, Ast::EndAnchor]);
            let program = Arc::new(compile::compile(wrapped, self.case_insensitive, false)?);
            return Ok(Glob {
                compiled: Compiled::Negated(program),
            });
        }
        let mut ast = parse(pattern, self.pathname)?;
        if self.period {
            ast = apply_leading_period_rule(ast, 0)?;
        }
        // Glob matching is always full-string (unlike `Regex`, which
        // searches): wrapping in `^...$` gets that for free from the
        // existing anchored-match compilation path, with no separate
        // "full match" mode needed in the VM.
        let wrapped = Ast::Concat(vec![Ast::StartAnchor, ast, Ast::EndAnchor]);
        let program = Arc::new(compile::compile(wrapped, self.case_insensitive, false)?);
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

    /// Finds a prefix of `s` (starting at byte `0`) that fully matches
    /// this pattern, returning its length in bytes — the shell's
    /// `${var#pat}`/`${var##pat}` family: `longest = false` is `#`
    /// (shortest matching prefix removed), `longest = true` is `##`
    /// (longest). `None` if no prefix — including the empty one —
    /// matches.
    ///
    /// Checks candidate prefix lengths at `char` boundaries, scanning
    /// from the shortest or longest end first and stopping at the first
    /// hit — correct for any pattern, including `*`/extglob-heavy ones,
    /// by construction (each candidate is just [`Glob::matches`] against
    /// a substring), though it's `O(n)` `matches` calls rather than the
    /// single linear pass the crate's ERE matching guarantees.
    ///
    /// ```
    /// use rusty_regx::Glob;
    ///
    /// let g = Glob::new("a*")?;
    /// assert_eq!(g.match_prefix("azbz", false), Some(1)); // shortest: "a"
    /// assert_eq!(g.match_prefix("azbz", true), Some(4)); // longest: "azbz"
    /// assert_eq!(Glob::new("q*")?.match_prefix("azbz", false), None); // doesn't start with 'q' at all
    /// # Ok::<(), rusty_regx::Error>(())
    /// ```
    pub fn match_prefix(&self, s: &str, longest: bool) -> Option<usize> {
        let mut boundaries = prefix_boundaries(s);
        if longest {
            boundaries.rev().find(|&k| self.matches(&s[..k]))
        } else {
            boundaries.find(|&k| self.matches(&s[..k]))
        }
    }

    /// Finds a suffix of `s` (ending at `s.len()`) that fully matches
    /// this pattern, returning the suffix's *starting* byte offset — the
    /// shell's `${var%pat}`/`${var%%pat}` family: `longest = false` is
    /// `%` (shortest matching suffix removed, i.e. the largest starting
    /// offset), `longest = true` is `%%` (longest, smallest offset).
    /// `None` if no suffix — including the empty one — matches.
    ///
    /// Same approach and complexity as [`Glob::match_prefix`], scanning
    /// from the shortest or longest end first.
    ///
    /// ```
    /// use rusty_regx::Glob;
    ///
    /// let g = Glob::new("*z")?;
    /// assert_eq!(g.match_suffix("azbz", false), Some(3)); // shortest: "z"
    /// assert_eq!(g.match_suffix("azbz", true), Some(0)); // longest: "azbz"
    /// assert_eq!(Glob::new("*q")?.match_suffix("azbz", false), None); // doesn't end in 'q' at all
    /// # Ok::<(), rusty_regx::Error>(())
    /// ```
    pub fn match_suffix(&self, s: &str, longest: bool) -> Option<usize> {
        let mut boundaries = prefix_boundaries(s);
        if longest {
            boundaries.find(|&k| self.matches(&s[k..]))
        } else {
            boundaries.rev().find(|&k| self.matches(&s[k..]))
        }
    }
}

/// Every byte offset a prefix or suffix split could land on: each `char`
/// boundary plus `s.len()`, ascending. Shared by [`Glob::match_prefix`]
/// and [`Glob::match_suffix`] — a prefix's *end* offset and a suffix's
/// *start* offset range over the exact same set of positions.
fn prefix_boundaries(s: &str) -> impl DoubleEndedIterator<Item = usize> + '_ {
    s.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(s.len()))
}

/// Parses a glob pattern into the shared ERE [`Ast`] (unanchored — the
/// caller wraps `^...$`).
fn parse(pattern: &str, pathname: bool) -> Result<Ast, Error> {
    let mut p = Parser {
        pattern,
        byte_pos: 0,
        char_pos: 0,
        depth: 0,
        pathname,
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
    /// [`GlobBuilder::pathname`]: when set, every `AnyChar`-driven atom
    /// and bracket expression this parser constructs excludes `/`.
    pathname: bool,
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
                Ok((wildcard_atom(self.pathname), 0))
            }
            '*' => {
                self.bump();
                Ok((
                    Ast::Repeat {
                        ast: Box::new(wildcard_atom(self.pathname)),
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
                let mut class = bracket::parse(&mut cursor, open, &NEGATION_CHARS)?;
                self.byte_pos = cursor.byte_pos;
                self.char_pos = cursor.char_pos;
                if self.pathname {
                    class = exclude_char_from_class(class, '/', open)?;
                }
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

/// What `?`/`*` compile to: plain `AnyChar` normally, or `[^/]` under
/// `pathname` mode (`docs/GLOB_DESIGN.md`'s "`AnyChar` becomes `[^/]`").
fn wildcard_atom(pathname: bool) -> Ast {
    if pathname {
        Ast::Class(Class {
            negated: true,
            ranges: vec![('/', '/')],
            posix: Vec::new(),
        })
    } else {
        Ast::AnyChar
    }
}

/// Whether `class`, as currently written, matches `c` — a plain linear
/// scan (this only ever runs once per bracket expression at parse time,
/// not per input char, so it doesn't need `compile::CompiledClass`'s
/// sorted-ranges/ASCII-bitmap machinery).
fn class_matches_char(class: &Class, c: char) -> bool {
    let hit = class.ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi)
        || class.posix.iter().any(|&p| compile::posix_matches(p, c));
    hit != class.negated
}

/// Rewrites `class` so it never matches `excl`, given it currently might
/// (a no-op if it already doesn't). Used by `pathname` mode (`excl` =
/// `/`) and `period` mode (`excl` = `.`, on the pattern's first atom
/// only) — see `docs/GLOB_DESIGN.md` and [`GlobBuilder::pathname`].
///
/// Exact for the negated case (adding `excl` to `ranges` forces it out of
/// the negated union, regardless of what `posix` contains) and for a
/// positive class whose match on `excl` comes only from `ranges` (split
/// around the excluded point). A positive class matching `excl` only via
/// `[[:punct:]]`/`[[:print:]]`/`[[:graph:]]` (the sole POSIX classes that
/// can) can't be losslessly narrowed with this `Class` representation —
/// that's a compile error, not a silent under-restriction.
fn exclude_char_from_class(class: Class, excl: char, at: usize) -> Result<Class, Error> {
    if !class_matches_char(&class, excl) {
        return Ok(class);
    }
    if class.negated {
        let mut ranges = class.ranges;
        ranges.push((excl, excl));
        return Ok(Class { ranges, ..class });
    }
    if class.posix.iter().any(|&p| compile::posix_matches(p, excl)) {
        return Err(Error::new(
            ErrorKind::GlobClassExclusionUnsupported,
            Some(at),
        ));
    }
    let ranges = class
        .ranges
        .into_iter()
        .flat_map(|(lo, hi)| split_range_excluding(lo, hi, excl))
        .collect();
    Ok(Class { ranges, ..class })
}

/// Splits `[lo, hi]` into the (zero, one, or two) sub-ranges that remain
/// after removing exactly `excl`, given `lo <= excl <= hi` may or may not
/// hold (a no-op range back out if `excl` isn't actually inside it).
fn split_range_excluding(lo: char, hi: char, excl: char) -> Vec<(char, char)> {
    if excl < lo || excl > hi {
        return vec![(lo, hi)];
    }
    let mut out = Vec::with_capacity(2);
    if lo < excl {
        if let Some(before) = prev_char(excl) {
            out.push((lo, before));
        }
    }
    if excl < hi {
        if let Some(after) = next_char(excl) {
            out.push((after, hi));
        }
    }
    out
}

/// The codepoint immediately before `c`, skipping the surrogate gap
/// (`0xD800..=0xDFFF`, not valid `char`s) — `None` for `c == '\0'`.
fn prev_char(c: char) -> Option<char> {
    let mut n = (c as u32).checked_sub(1)?;
    if (0xDC00..=0xDFFF).contains(&n) {
        n = 0xD7FF;
    }
    char::from_u32(n)
}

/// The codepoint immediately after `c`, skipping the surrogate gap —
/// `None` for `c == char::MAX`.
fn next_char(c: char) -> Option<char> {
    let mut n = c as u32 + 1;
    if (0xD800..=0xDFFF).contains(&n) {
        n = 0xE000;
    }
    char::from_u32(n)
}

/// `period` mode (`GlobBuilder::period`): rewrites `ast`'s first consumed
/// character so it excludes `.`, given that character is a single fixed
/// position (a plain char, `?`, or `[...]`/`[!...]`). Errors on `*` or an
/// extglob group in that position — see `GlobBuilder::period`'s doc
/// comment for why that composition isn't supported in restricted v1.
///
/// (`pathname` mode's own `/`-exclusion, if any, is already baked into
/// `ast` by the time this runs — a pathname-mode `?`/`*` already compiled
/// to `Ast::Class`, never bare `Ast::AnyChar` — so this only ever needs
/// to layer `.` on top of whatever's already there.)
fn apply_leading_period_rule(ast: Ast, at: usize) -> Result<Ast, Error> {
    match ast {
        // A literal already either is exactly '.' (fine, explicit) or
        // structurally can't match '.' at all (also fine) — either way,
        // nothing to rewrite.
        Ast::Empty | Ast::Char(_) => Ok(ast),
        Ast::AnyChar => exclude_char_from_class(
            Class {
                negated: true,
                ranges: Vec::new(),
                posix: Vec::new(),
            },
            '.',
            at,
        )
        .map(Ast::Class),
        Ast::Class(class) => exclude_char_from_class(class, '.', at).map(Ast::Class),
        Ast::Concat(mut items) => {
            if let Some(first) = items.first().cloned() {
                items[0] = apply_leading_period_rule(first, at)?;
            }
            Ok(Ast::Concat(items))
        }
        Ast::Repeat { .. } | Ast::Alternation(_) => Err(Error::new(
            ErrorKind::GlobLeadingPeriodUnsupported,
            Some(at),
        )),
        _ => unreachable!("the glob parser never produces this Ast variant"),
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

    fn pathname_matches(pattern: &str, name: &str) -> bool {
        GlobBuilder::new()
            .pathname(true)
            .build(pattern)
            .unwrap()
            .matches(name)
    }

    fn pathname_err(pattern: &str) -> ErrorKind {
        GlobBuilder::new()
            .pathname(true)
            .build(pattern)
            .unwrap_err()
            .kind()
    }

    fn period_matches(pattern: &str, name: &str) -> bool {
        GlobBuilder::new()
            .period(true)
            .build(pattern)
            .unwrap()
            .matches(name)
    }

    fn period_err(pattern: &str) -> ErrorKind {
        GlobBuilder::new()
            .period(true)
            .build(pattern)
            .unwrap_err()
            .kind()
    }

    #[test]
    fn pathname_star_and_question_do_not_cross_slash() {
        assert!(pathname_matches("*.txt", "notes.txt"));
        assert!(!pathname_matches("*.txt", "dir/notes.txt"));
        assert!(pathname_matches("dir/*.txt", "dir/notes.txt"));
        assert!(!pathname_matches("dir/*.txt", "dir/sub/notes.txt"));
        assert!(!pathname_matches("a?c", "a/c"));
    }

    #[test]
    fn pathname_off_lets_star_cross_slash() {
        // Same pattern, default (pathname off) builder: `*` crosses `/`.
        assert!(matches("*.txt", "dir/notes.txt"));
    }

    #[test]
    fn pathname_extglob_does_not_cross_slash() {
        assert!(pathname_matches("*(a)", "aaa"));
        assert!(!pathname_matches("*(a)", "a/a"));
        assert!(pathname_matches("@(foo|bar)", "foo"));
    }

    #[test]
    fn pathname_literal_slash_still_matches() {
        assert!(pathname_matches("dir/file", "dir/file"));
        assert!(!pathname_matches("dir/file", "dirXfile"));
    }

    #[test]
    fn pathname_negated_bracket_excludes_slash() {
        // `[^a]` doesn't mention `/` at all, but pathname mode must still
        // exclude it (POSIX: a `/` is matched only by a literal `/`).
        assert!(pathname_matches("[^a]", "b"));
        assert!(!pathname_matches("[^a]", "/"));
    }

    #[test]
    fn pathname_positive_bracket_excludes_slash() {
        // A range that happens to span `/` (0x2E-0x30 covers '.', '/', '0').
        assert!(pathname_matches("[.-0]", "."));
        assert!(pathname_matches("[.-0]", "0"));
        assert!(!pathname_matches("[.-0]", "/"));
    }

    #[test]
    fn pathname_bracket_not_mentioning_slash_is_unaffected() {
        assert!(pathname_matches("[a-z]", "m"));
        assert!(!pathname_matches("[a-z]", "M"));
    }

    #[test]
    fn pathname_positive_posix_class_matching_slash_is_an_error() {
        // `/` is ASCII punctuation, so `[[:punct:]]` matches it — and a
        // positive class can't lose just that one member losslessly.
        assert_eq!(
            pathname_err("[[:punct:]]"),
            ErrorKind::GlobClassExclusionUnsupported
        );
    }

    #[test]
    fn pathname_negated_posix_class_matching_slash_is_fine() {
        // Negated case is always exact (adding `/` to `ranges` excludes
        // it regardless of what the posix classes say).
        assert!(pathname_matches("[^[:digit:]]", "!"));
        assert!(!pathname_matches("[^[:digit:]]", "/"));
    }

    #[test]
    fn period_leading_dot_needs_explicit_dot() {
        // A leading `*` errors under period mode (see
        // `period_leading_star_or_extglob_is_an_error`); `?*` exercises
        // the same "leading dot forbidden" rule without hitting that.
        assert!(!period_matches("?*", ".hidden"));
        assert!(period_matches("?*", "visible"));
        assert!(period_matches(".*", ".hidden"));
        assert!(!period_matches("?x", ".x"));
        assert!(period_matches("?x", "yx"));
    }

    #[test]
    fn period_only_restricts_the_leading_position() {
        // Dots elsewhere in the string are unaffected.
        assert!(period_matches("?*", "a.b.c"));
        assert!(period_matches("?*", "a."));
    }

    #[test]
    fn period_bracket_excludes_leading_dot() {
        assert!(!period_matches("[.a]bc", ".bc"));
        assert!(period_matches("[.a]bc", "abc"));
    }

    #[test]
    fn period_explicit_literal_first_atom_needs_no_rewrite() {
        assert!(period_matches("abc", "abc"));
        assert!(period_matches(".abc", ".abc"));
    }

    #[test]
    fn period_leading_star_or_extglob_is_an_error() {
        assert_eq!(period_err("*.txt"), ErrorKind::GlobLeadingPeriodUnsupported);
        assert_eq!(
            period_err("@(a|b)"),
            ErrorKind::GlobLeadingPeriodUnsupported
        );
    }

    #[test]
    fn period_and_pathname_compose() {
        let g = GlobBuilder::new()
            .pathname(true)
            .period(true)
            .build("?x")
            .unwrap();
        assert!(g.matches("yx"));
        assert!(!g.matches(".x"));
    }

    #[test]
    fn pathname_applies_inside_whole_pattern_negation() {
        // Without pathname mode, inner `a*` crosses `/` and matches
        // "a/b", so `!(a*)` does *not* match it.
        assert!(!Glob::new("!(a*)").unwrap().matches("a/b"));
        // With pathname mode, inner `a*` can't reach the `$` past the
        // `/`, so it does *not* match "a/b" — meaning `!(a*)` *does*.
        assert!(GlobBuilder::new()
            .pathname(true)
            .build("!(a*)")
            .unwrap()
            .matches("a/b"));
    }

    fn ci_matches(pattern: &str, name: &str) -> bool {
        GlobBuilder::new()
            .case_insensitive(true)
            .build(pattern)
            .unwrap()
            .matches(name)
    }

    #[test]
    fn case_insensitive_literals() {
        assert!(ci_matches("readme.md", "README.MD"));
        assert!(ci_matches("ReadMe.Md", "readme.md"));
        assert!(!matches("readme.md", "README.MD"));
    }

    #[test]
    fn case_insensitive_bracket_ranges_fold() {
        // Same `[A-_]`-style corner `Regex::new_posix_ci` documents: `_`
        // (0x5F) sits right after `Z` (0x5A) and folds specially under
        // glibc's fold-to-upper model.
        assert!(ci_matches("[a-f]", "C"));
        assert!(ci_matches("[a-f]", "c"));
        assert!(!ci_matches("[a-f]", "g"));
    }

    #[test]
    fn case_insensitive_composes_with_extglob_and_negation() {
        assert!(ci_matches("@(FOO|bar)", "foo"));
        assert!(ci_matches("@(FOO|bar)", "BAR"));
        assert!(GlobBuilder::new()
            .case_insensitive(true)
            .build("!(FOO)")
            .unwrap()
            .matches("bar"));
        assert!(!GlobBuilder::new()
            .case_insensitive(true)
            .build("!(FOO)")
            .unwrap()
            .matches("foo"));
    }

    #[test]
    fn case_insensitive_composes_with_pathname_and_period() {
        let g = GlobBuilder::new()
            .case_insensitive(true)
            .pathname(true)
            .period(true)
            .build("[Rr]*")
            .unwrap();
        assert!(g.matches("Readme"));
        assert!(g.matches("readme"));
        assert!(!g.matches("dir/readme")); // pathname: `*` doesn't cross `/`
        assert!(!g.matches(".readme")); // period: leading dot still excluded
    }

    #[test]
    fn match_prefix_shortest_and_longest() {
        let g = Glob::new("a*").unwrap();
        assert_eq!(g.match_prefix("aaab", false), Some(1));
        assert_eq!(g.match_prefix("aaab", true), Some(4));
        assert_eq!(Glob::new("z*").unwrap().match_prefix("aaab", false), None);
        assert_eq!(Glob::new("z*").unwrap().match_prefix("aaab", true), None);
    }

    #[test]
    fn match_suffix_shortest_and_longest() {
        let g = Glob::new("*b").unwrap();
        assert_eq!(g.match_suffix("aaab", false), Some(3));
        assert_eq!(g.match_suffix("aaab", true), Some(0));
        assert_eq!(Glob::new("*z").unwrap().match_suffix("aaab", false), None);
        assert_eq!(Glob::new("*z").unwrap().match_suffix("aaab", true), None);
    }

    #[test]
    fn match_prefix_suffix_empty_pattern_only_matches_empty() {
        let g = Glob::new("").unwrap();
        assert_eq!(g.match_prefix("abc", false), Some(0));
        assert_eq!(g.match_prefix("abc", true), Some(0));
        assert_eq!(g.match_suffix("abc", false), Some(3));
        assert_eq!(g.match_suffix("abc", true), Some(3));
    }

    #[test]
    fn match_prefix_suffix_bare_star_matches_empty_or_everything() {
        let g = Glob::new("*").unwrap();
        assert_eq!(g.match_prefix("abc", false), Some(0));
        assert_eq!(g.match_prefix("abc", true), Some(3));
        assert_eq!(g.match_suffix("abc", false), Some(3));
        assert_eq!(g.match_suffix("abc", true), Some(0));
    }

    #[test]
    fn match_prefix_with_extglob() {
        let g = Glob::new("@(foo|bar)*").unwrap();
        assert_eq!(g.match_prefix("foobar", false), Some(3));
        assert_eq!(g.match_prefix("foobar", true), Some(6));
    }

    #[test]
    fn match_prefix_composes_with_negation() {
        // "!(a)" matches anything except exactly "a" — the empty string
        // qualifies, so the shortest matching prefix is always length 0.
        let g = Glob::new("!(a)").unwrap();
        assert_eq!(g.match_prefix("ab", false), Some(0));
    }

    #[test]
    fn match_prefix_composes_with_pathname() {
        // Under pathname mode, `a*`'s `*` can't cross `/`, so the longest
        // matching prefix of "a/b" is the same as the shortest: just "a".
        let g = GlobBuilder::new().pathname(true).build("a*").unwrap();
        assert_eq!(g.match_prefix("a/b", false), Some(1));
        assert_eq!(g.match_prefix("a/b", true), Some(1));
    }

    #[test]
    fn match_prefix_suffix_respect_char_boundaries() {
        // "é" is 2 UTF-8 bytes; candidate lengths must land on char
        // boundaries, not split it.
        let g = Glob::new("?").unwrap();
        assert_eq!(g.match_prefix("héllo", false), Some(1)); // "h"
        let g2 = Glob::new("??").unwrap();
        assert_eq!(g2.match_prefix("héllo", false), Some(3)); // "h" + "é" (2 bytes)
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
