//! The bytecode compiler (roadmap step 2).
//!
//! Lowers the AST to a flat instruction sequence executed by the Pike VM.
//! Intervals `{m,n}` are compiled by repetition, capped by
//! [`MAX_REPETITION_SIZE`] and an overall [`MAX_PROGRAM_SIZE`] so a pattern
//! cannot blow up the program size.

use crate::ast::{Ast, Class, PosixClass};
use crate::error::{Error, ErrorKind};

/// Cap on a single interval bound, mirroring the `regex` crate's limit in
/// spirit. Exceeding it yields [`ErrorKind::RepetitionTooLarge`].
pub const MAX_REPETITION_SIZE: u32 = 1000;

/// Cap on total compiled program size, so nested intervals like
/// `(a{1000}){1000}` cannot exhaust memory.
pub const MAX_PROGRAM_SIZE: usize = 1 << 16;

/// A single VM instruction.
///
/// Classes live in [`Program::classes`] and are referenced by index, so
/// instructions stay small and `Copy` and the dispatch loop stays
/// cache-friendly.
#[derive(Debug, Clone, Copy)]
pub enum Inst {
    /// Match one literal character.
    Char(char),
    /// Match any single character.
    AnyChar,
    /// Match any single character unconditionally — the unanchored-search
    /// scan loop. Unlike a pattern `.`, this must cross newlines even in
    /// `REG_NEWLINE` mode (it moves the search, it doesn't match text).
    ScanAny,
    /// Match one character against `Program::classes[i]`.
    Class(usize),
    /// Assert start of input.
    StartAnchor,
    /// Assert end of input.
    EndAnchor,
    /// Assert a GNU word boundary (exactly one adjacent char is a word char).
    WordBoundary,
    /// Assert a GNU non-boundary.
    NotWordBoundary,
    /// Assert start of word (`\<`).
    WordStart,
    /// Assert end of word (`\>`).
    WordEnd,
    /// Try `first` then `second` (thread split; order encodes greediness).
    Split { first: usize, second: usize },
    /// Unconditional jump.
    Jump(usize),
    /// Record the current input position in capture slot `n`.
    Save(usize),
    /// Successful match.
    Match,
}

/// A compiled bracket expression: ASCII membership precomputed as a
/// 128-bit table (negation and POSIX classes folded in — one bit test
/// per input char on the VM's hottest path), with the general
/// range/POSIX logic as the non-ASCII fallback.
#[derive(Debug, Clone)]
pub struct CompiledClass {
    ascii: [u64; 2],
    class: Class,
}

impl CompiledClass {
    fn new(class: Class) -> CompiledClass {
        let mut ascii = [0u64; 2];
        for b in 0..128u32 {
            let c = char::from_u32(b).expect("ASCII");
            if class_matches_general(&class, c) {
                ascii[(b / 64) as usize] |= 1 << (b % 64);
            }
        }
        CompiledClass { ascii, class }
    }

    /// Whether `c` is in the class.
    pub fn matches(&self, c: char) -> bool {
        let b = c as u32;
        if b < 128 {
            self.ascii[(b / 64) as usize] & (1 << (b % 64)) != 0
        } else {
            class_matches_general(&self.class, c)
        }
    }
}

/// The general membership test: binary search over the compile-time
/// sorted-and-merged ranges (see `normalize_ranges`), then the POSIX
/// classes, then negation.
fn class_matches_general(class: &Class, c: char) -> bool {
    let i = class.ranges.partition_point(|&(lo, _)| lo <= c);
    let hit =
        (i > 0 && class.ranges[i - 1].1 >= c) || class.posix.iter().any(|&p| posix_matches(p, c));
    hit != class.negated
}

/// POSIX classes are ASCII-first, with `char` method fallbacks where they
/// have a sensible Unicode meaning (per DESIGN.md — no Unicode tables).
fn posix_matches(class: PosixClass, c: char) -> bool {
    match class {
        PosixClass::Alnum => c.is_alphanumeric(),
        PosixClass::Alpha => c.is_alphabetic(),
        PosixClass::Blank => c == ' ' || c == '\t',
        PosixClass::Cntrl => c.is_control(),
        PosixClass::Digit => c.is_ascii_digit(),
        PosixClass::Graph => c.is_ascii_graphic(),
        PosixClass::Lower => c.is_lowercase(),
        PosixClass::Print => c.is_ascii_graphic() || c == ' ',
        PosixClass::Punct => c.is_ascii_punctuation(),
        PosixClass::Space => c.is_whitespace(),
        PosixClass::Upper => c.is_uppercase(),
        PosixClass::Xdigit => c.is_ascii_hexdigit(),
    }
}

/// A compiled program.
#[derive(Debug, Clone)]
pub struct Program {
    pub insts: Vec<Inst>,
    /// Interned bracket expressions, referenced by `Inst::Class` index.
    pub classes: Vec<CompiledClass>,
    /// Number of capture groups including group 0.
    pub group_count: usize,
    /// Total `Save` slots: two per group, plus two per repetition (hidden
    /// span tags used only by POSIX-mode disambiguation).
    pub slot_count: usize,
    /// Slot-pair base indices in syntactic pre-order (group 0 first, then
    /// groups and repetition spans outer-first, left-to-right) — the
    /// comparison order for POSIX leftmost-longest disambiguation.
    pub tag_order: Vec<usize>,
    /// Case-insensitive (`REG_ICASE`) mode: the compiler has already folded
    /// pattern literals and range endpoints via [`fold`]; the VM must fold
    /// each input character the same way before comparing.
    pub icase: bool,
    /// The literal string every match must start with (empty if none, or
    /// if the program can't use it). The VM may fast-forward the scan to
    /// its next occurrence whenever no live thread carries progress.
    /// Empty for anchored programs and patterns without a mandatory
    /// literal head. In `icase` mode the chars are pre-folded and the VM
    /// scans by folding each input char (`find_prefix`), so the
    /// case-insensitive modes get fast-forward too.
    pub prefix: String,
    /// Set when the whole pattern is a plain literal (optionally anchored
    /// at either end): the VM bypasses NFA simulation entirely and
    /// matches by substring search. Never set in `icase` mode.
    pub literal: Option<Literal>,
    /// The literal string every match must end with (empty if none):
    /// a no-match rejects with one substring scan before any VM work.
    /// Empty in `icase` mode.
    pub suffix: String,
    /// Whether every match must end at the input's end (`$` on every
    /// branch) — upgrades the suffix check from `contains` to
    /// `ends_with`.
    pub suffix_anchored: bool,
    /// POSIX `REG_NEWLINE` mode: `.` and negated bracket expressions
    /// don't match `\n` (the class exclusion is compiled in; the `.`
    /// exclusion is checked by the VM), and `^`/`$` also match right
    /// after/before a `\n`.
    pub newline: bool,
    /// When every match must start with a char of this class (index into
    /// `classes`) and no literal prefix exists, the VM fast-forwards the
    /// scan to the class's next member — `[0-9]+`, `\w+`-style heads.
    pub first_class: Option<usize>,
}

/// A pattern that is a plain literal, matched by substring search.
#[derive(Debug, Clone)]
pub struct Literal {
    /// The literal text.
    pub s: String,
    /// Whether the pattern began with `^`.
    pub anchored_start: bool,
    /// Whether the pattern ended with `$`.
    pub anchored_end: bool,
}

/// The `REG_ICASE` case fold: simple (single-character) uppercase mapping.
///
/// glibc — what bash's `=~` uses — implements `REG_ICASE` by uppercasing
/// both the pattern and the input (`build_upper_buffer` /
/// `build_wcs_upper_buffer`), so folding to *upper* is load-bearing:
/// it is what makes `[A-_]` match `b` while `[Z-a]` is an invalid range
/// under `nocasematch` (both verified against bash 5.2). Characters whose
/// uppercase form is not a single character (e.g. `ß` → `SS`) fold to
/// themselves, as in glibc.
pub fn fold(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// Compiles an AST into a [`Program`].
///
/// Unless every match is forced to start at position 0 (the pattern is
/// start-anchored), the program begins with an implicit non-greedy "any
/// char" loop so that execution is an unanchored search, matching bash
/// `=~` semantics. Anchored programs omit the prefix entirely: their
/// thread list empties as soon as position 0 fails, so `^b` against a
/// megabyte of text stops after one character instead of scanning it all.
pub fn compile(mut ast: Ast, icase: bool, newline: bool) -> Result<Program, Error> {
    let group_count = max_group(&ast) as usize + 1;
    let mut tag_order = vec![0];
    let mut next_slot = 2 * group_count;
    number(&mut ast, &mut next_slot, &mut tag_order);

    // Under REG_NEWLINE, `^` matches after any newline, so the
    // anchored-at-0 fast path doesn't apply.
    let anchored = starts_anchored(&ast) && !newline;
    let mut c = Compiler {
        insts: Vec::new(),
        classes: Vec::new(),
        icase,
        newline,
    };
    if !anchored {
        // Unanchored-search prefix, non-greedy: prefer starting the match
        // at the current position (leftmost) over consuming another char.
        c.push(Inst::Split {
            first: 3,
            second: 1,
        })?;
        c.push(Inst::ScanAny)?;
        c.push(Inst::Jump(0))?;
    }
    c.push(Inst::Save(0))?;
    c.emit(&ast)?;
    c.push(Inst::Save(1))?;
    c.push(Inst::Match)?;
    let mut prefix = String::new();
    if !anchored {
        collect_prefix(&ast, &mut prefix);
        if icase {
            // The VM compares folded chars; pre-fold the needle to match.
            prefix = prefix.chars().map(fold).collect();
        }
    }
    let mut suffix = String::new();
    if !icase {
        collect_suffix_rev(&ast, &mut suffix);
        suffix = suffix.chars().rev().collect();
    }
    // Under REG_NEWLINE, `\$` also matches before a newline, so the
    // suffix may sit anywhere — keep the contains() check only.
    let suffix_anchored = ends_anchored(&ast) && !newline;
    // Groups need real capture tracking; the substring path reports only
    // group 0.
    // Class-head scan hint: only when no literal prefix exists (the
    // string prefix is the stronger hint) and the search is unanchored.
    let first_class = if anchored || !prefix.is_empty() {
        None
    } else {
        head_class(&ast)
            .and_then(|cl| fold_class(&cl, icase).ok())
            .map(|mut cl| {
                normalize_ranges(&mut cl.ranges);
                let index = c.classes.len();
                c.classes.push(CompiledClass::new(cl));
                index
            })
    };
    let literal = if icase || group_count > 1 {
        None
    } else {
        // Anchored literals assume `^`/`$` mean input start/end, which
        // REG_NEWLINE changes; unanchored literals stay valid.
        literal_of(&ast).filter(|l| !newline || (!l.anchored_start && !l.anchored_end))
    };
    Ok(Program {
        insts: c.insts,
        classes: c.classes,
        group_count,
        slot_count: next_slot,
        tag_order,
        icase,
        prefix,
        literal,
        suffix,
        suffix_anchored,
        newline,
        first_class,
    })
}

/// If the whole pattern is a plain literal — `Char`s only, optionally
/// `^`-anchored at the head and `$`-anchored at the tail — returns it for
/// the VM's substring fast path. Such a pattern has no groups, so group 0
/// is the only capture. This is the common shape rush produces via
/// `escape()` for `[[ $x =~ $literal ]]`.
fn literal_of(ast: &Ast) -> Option<Literal> {
    // An assertion constrains context without consuming — such a pattern
    // is never *exactly* its literal, even though collect_prefix walks
    // through assertions for scan-acceleration purposes.
    if has_assertions(ast) {
        return None;
    }
    let single = std::slice::from_ref(ast);
    let mut items: &[Ast] = match ast {
        Ast::Concat(items) => items,
        _ => single,
    };
    let mut lit = Literal {
        s: String::new(),
        anchored_start: false,
        anchored_end: false,
    };
    if let Some(Ast::StartAnchor) = items.first() {
        lit.anchored_start = true;
        items = &items[1..];
    }
    if let Some(Ast::EndAnchor) = items.last() {
        lit.anchored_end = true;
        items = &items[..items.len() - 1];
    }
    // Every remaining item must be *exactly* its literal — Chars, exact
    // repetitions of literals, Empty (the caller has already excluded
    // patterns with groups).
    for item in items {
        if !collect_prefix(item, &mut lit.s) {
            return None;
        }
    }
    Some(lit)
}

/// Whether every match of `ast` must start at position 0 — i.e. every
/// alternation branch begins with `^`. A `min == 0` repetition head is
/// never anchored: `(^a)?b` matches `b` anywhere.
fn starts_anchored(ast: &Ast) -> bool {
    match ast {
        Ast::StartAnchor => true,
        Ast::Concat(items) => items.first().is_some_and(starts_anchored),
        Ast::Alternation(branches) => branches.iter().all(starts_anchored),
        Ast::Group(_, inner) => starts_anchored(inner),
        Ast::Repeat { ast, min, .. } => *min >= 1 && starts_anchored(ast),
        _ => false,
    }
}

/// The class every match must start with, if the pattern's head is a
/// mandatory class (the class analog of `collect_prefix`'s first char).
/// Assertions are transparent; `min == 0` heads disqualify.
fn head_class(ast: &Ast) -> Option<Class> {
    match ast {
        Ast::Class(c) => Some(c.clone()),
        Ast::Concat(items) => {
            for item in items {
                match item {
                    Ast::WordBoundary
                    | Ast::NotWordBoundary
                    | Ast::WordStart
                    | Ast::WordEnd
                    | Ast::Empty => continue,
                    other => return head_class(other),
                }
            }
            None
        }
        Ast::Group(_, inner) => head_class(inner),
        Ast::Repeat { ast, min, .. } if *min >= 1 => head_class(ast),
        Ast::Alternation(branches) => {
            let c = head_class(branches.first()?)?;
            branches
                .iter()
                .all(|b| head_class(b).as_ref() == Some(&c))
                .then_some(c)
        }
        _ => None,
    }
}

/// Whether the pattern contains any zero-width word assertion.
fn has_assertions(ast: &Ast) -> bool {
    match ast {
        Ast::WordBoundary | Ast::NotWordBoundary | Ast::WordStart | Ast::WordEnd => true,
        Ast::Concat(items) | Ast::Alternation(items) => items.iter().any(has_assertions),
        Ast::Group(_, inner) => has_assertions(inner),
        Ast::Repeat { ast, .. } => has_assertions(ast),
        _ => false,
    }
}

/// Accumulates the mandatory literal prefix of `ast` into `out` —
/// the string every match must start with. Returns whether the walked
/// construct is *exactly* its literal (so the prefix may keep growing
/// past it). Conservative: anything uncertain ends the prefix.
fn collect_prefix(ast: &Ast, out: &mut String) -> bool {
    match ast {
        Ast::Empty => true,
        // Zero-width constructs consume nothing: the mandatory literal
        // continues right through them (`\bword` and `^beta` — in line
        // mode, where prefixes apply — must still start with their
        // literal). The exact-literal path excludes assertions separately
        // via `has_assertions`, and anchored patterns compute no prefix
        // at all outside line mode.
        Ast::WordBoundary
        | Ast::NotWordBoundary
        | Ast::WordStart
        | Ast::WordEnd
        | Ast::StartAnchor => true,
        Ast::Char(c) => {
            out.push(*c);
            true
        }
        Ast::Concat(items) => items.iter().all(|item| collect_prefix(item, out)),
        Ast::Group(_, inner) => collect_prefix(inner, out),
        Ast::Repeat { ast, min, max, .. } if *min >= 1 => {
            // A fully-literal body repeats contiguously `min` times; the
            // prefix continues past it only for an exact count.
            let start = out.len();
            if !collect_prefix(ast, out) {
                return false;
            }
            let body = out[start..].to_string();
            for _ in 1..*min {
                out.push_str(&body);
            }
            *max == Some(*min)
        }
        Ast::Alternation(branches) => {
            // The longest common prefix of the branches is mandatory;
            // nothing past the alternation can extend it.
            let mut prefixes = branches.iter().map(|b| {
                let mut p = String::new();
                collect_prefix(b, &mut p);
                p
            });
            let mut common = prefixes.next().unwrap_or_default();
            for p in prefixes {
                let shared = common
                    .chars()
                    .zip(p.chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a.len_utf8())
                    .sum();
                common.truncate(shared);
            }
            out.push_str(&common);
            false
        }
        _ => false,
    }
}

/// Whether every match of `ast` must end at the input's end — the mirror
/// of `starts_anchored`.
fn ends_anchored(ast: &Ast) -> bool {
    match ast {
        Ast::EndAnchor => true,
        Ast::Concat(items) => items.last().is_some_and(ends_anchored),
        Ast::Alternation(branches) => branches.iter().all(ends_anchored),
        Ast::Group(_, inner) => ends_anchored(inner),
        Ast::Repeat { ast, min, .. } => *min >= 1 && ends_anchored(ast),
        _ => false,
    }
}

/// The mirror of `collect_prefix`: accumulates the mandatory literal
/// suffix in *reverse char order* into `out` (the caller un-reverses).
/// Returns whether the construct is exactly its literal.
fn collect_suffix_rev(ast: &Ast, out: &mut String) -> bool {
    match ast {
        Ast::Empty => true,
        Ast::WordBoundary
        | Ast::NotWordBoundary
        | Ast::WordStart
        | Ast::WordEnd
        | Ast::EndAnchor => true,
        Ast::Char(c) => {
            out.push(*c);
            true
        }
        Ast::Concat(items) => items.iter().rev().all(|item| collect_suffix_rev(item, out)),
        Ast::Group(_, inner) => collect_suffix_rev(inner, out),
        Ast::Repeat { ast, min, max, .. } if *min >= 1 => {
            let start = out.len();
            if !collect_suffix_rev(ast, out) {
                return false;
            }
            let body = out[start..].to_string();
            for _ in 1..*min {
                out.push_str(&body);
            }
            *max == Some(*min)
        }
        Ast::Alternation(branches) => {
            let mut suffixes = branches.iter().map(|b| {
                let mut p = String::new();
                collect_suffix_rev(b, &mut p);
                p
            });
            let mut common = suffixes.next().unwrap_or_default();
            for p in suffixes {
                let shared = common
                    .chars()
                    .zip(p.chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a.len_utf8())
                    .sum();
                common.truncate(shared);
            }
            out.push_str(&common);
            false
        }
        _ => false,
    }
}

/// Assigns span-tag slots to repetitions and records the disambiguation
/// order: syntactic pre-order, so an outer construct's pair is compared
/// before anything inside it, and siblings compare left-to-right. This is
/// what makes POSIX mode prefer a longer repetition span over a "better"
/// last iteration inside a shorter one.
fn number(ast: &mut Ast, next_slot: &mut usize, tag_order: &mut Vec<usize>) {
    // Canonicalize degenerate classes (`[a]`, `[[.a.]]`) to plain chars,
    // unlocking the literal/prefix tiers and cheaper dispatch. Folding
    // treats a one-char class and a char identically, so this is exact.
    if let Ast::Class(class) = ast {
        if !class.negated
            && class.posix.is_empty()
            && class.ranges.len() == 1
            && class.ranges[0].0 == class.ranges[0].1
        {
            *ast = Ast::Char(class.ranges[0].0);
        }
    }
    match ast {
        Ast::Empty
        | Ast::Char(_)
        | Ast::AnyChar
        | Ast::Class(_)
        | Ast::StartAnchor
        | Ast::EndAnchor
        | Ast::WordBoundary
        | Ast::NotWordBoundary
        | Ast::WordStart
        | Ast::WordEnd => {}
        Ast::Concat(items) | Ast::Alternation(items) => {
            for item in items {
                number(item, next_slot, tag_order);
            }
        }
        Ast::Group(index, inner) => {
            tag_order.push(2 * *index as usize);
            number(inner, next_slot, tag_order);
        }
        Ast::Repeat { ast, slot, .. } => {
            *slot = *next_slot;
            *next_slot += 2;
            tag_order.push(*slot);
            number(ast, next_slot, tag_order);
        }
    }
}

/// The highest group index appearing in the AST (0 if none).
fn max_group(ast: &Ast) -> u32 {
    match ast {
        Ast::Empty
        | Ast::Char(_)
        | Ast::AnyChar
        | Ast::Class(_)
        | Ast::StartAnchor
        | Ast::EndAnchor
        | Ast::WordBoundary
        | Ast::NotWordBoundary
        | Ast::WordStart
        | Ast::WordEnd => 0,
        Ast::Concat(items) | Ast::Alternation(items) => {
            items.iter().map(max_group).max().unwrap_or(0)
        }
        Ast::Group(index, inner) => (*index).max(max_group(inner)),
        Ast::Repeat { ast, .. } => max_group(ast),
    }
}

struct Compiler {
    insts: Vec<Inst>,
    classes: Vec<CompiledClass>,
    icase: bool,
    newline: bool,
}

/// Sorts ranges by start and merges overlapping or adjacent ones, so
/// membership tests can binary-search ([`Class`] invariant after
/// compilation).
fn normalize_ranges(ranges: &mut Vec<(char, char)>) {
    ranges.sort_unstable();
    let mut merged: Vec<(char, char)> = Vec::with_capacity(ranges.len());
    for &(lo, hi) in ranges.iter() {
        match merged.last_mut() {
            // Adjacent counts too: [a-cd-f] is one range.
            Some(&mut (_, ref mut phi)) if lo as u32 <= *phi as u32 + 1 => {
                *phi = (*phi).max(hi);
            }
            _ => merged.push((lo, hi)),
        }
    }
    *ranges = merged;
}

impl Compiler {
    /// Appends an instruction, returning its index; errors past the size cap.
    fn push(&mut self, inst: Inst) -> Result<usize, Error> {
        if self.insts.len() >= MAX_PROGRAM_SIZE {
            return Err(Error::new(ErrorKind::RepetitionTooLarge, None));
        }
        self.insts.push(inst);
        Ok(self.insts.len() - 1)
    }

    fn next(&self) -> usize {
        self.insts.len()
    }

    fn emit(&mut self, ast: &Ast) -> Result<(), Error> {
        match ast {
            Ast::Empty => {}
            Ast::Char(c) => {
                let c = if self.icase { fold(*c) } else { *c };
                self.push(Inst::Char(c))?;
            }
            Ast::AnyChar => {
                self.push(Inst::AnyChar)?;
            }
            Ast::Class(class) => {
                let mut class = self.fold_class(class)?;
                // REG_NEWLINE: a negated class also excludes newline;
                // adding it to the positive set before negation compiles
                // the exclusion in at zero runtime cost.
                if self.newline && class.negated {
                    class.ranges.push(('\n', '\n'));
                }
                normalize_ranges(&mut class.ranges);
                let index = self.classes.len();
                self.classes.push(CompiledClass::new(class));
                self.push(Inst::Class(index))?;
            }
            Ast::StartAnchor => {
                self.push(Inst::StartAnchor)?;
            }
            Ast::EndAnchor => {
                self.push(Inst::EndAnchor)?;
            }
            Ast::WordBoundary => {
                self.push(Inst::WordBoundary)?;
            }
            Ast::NotWordBoundary => {
                self.push(Inst::NotWordBoundary)?;
            }
            Ast::WordStart => {
                self.push(Inst::WordStart)?;
            }
            Ast::WordEnd => {
                self.push(Inst::WordEnd)?;
            }
            Ast::Concat(items) => {
                for item in items {
                    self.emit(item)?;
                }
            }
            Ast::Alternation(branches) => self.alternation(branches)?,
            Ast::Group(index, inner) => {
                let slot = 2 * *index as usize;
                self.push(Inst::Save(slot))?;
                self.emit(inner)?;
                self.push(Inst::Save(slot + 1))?;
            }
            Ast::Repeat {
                ast,
                min,
                max,
                slot,
            } => self.repeat(ast, *min, *max, *slot)?,
        }
        Ok(())
    }

    /// See the free function [`fold_class`].
    fn fold_class(&self, class: &Class) -> Result<Class, Error> {
        fold_class(class, self.icase)
    }
}

/// In `icase` mode, folds a class the way glibc's `REG_ICASE` does:
/// range endpoints (including single characters, which are degenerate
/// ranges) fold to uppercase, and a range that is reversed *after*
/// folding is an error (`[Z-a]` is valid case-sensitively but folds to
/// `[Z-A]`; bash rejects it under `nocasematch`). `[[:upper:]]` and
/// `[[:lower:]]` both become `[[:alpha:]]` — glibc's documented
/// `REG_ICASE` rule, verified against bash 5.2.
fn fold_class(class: &Class, icase: bool) -> Result<Class, Error> {
    if !icase {
        return Ok(class.clone());
    }
    let mut ranges = Vec::with_capacity(class.ranges.len());
    for &(lo, hi) in &class.ranges {
        let (lo, hi) = (fold(lo), fold(hi));
        if lo > hi {
            return Err(Error::new(ErrorKind::InvalidRange, None));
        }
        ranges.push((lo, hi));
    }
    let posix = class
        .posix
        .iter()
        .map(|&p| match p {
            PosixClass::Upper | PosixClass::Lower => PosixClass::Alpha,
            p => p,
        })
        .collect();
    Ok(Class {
        negated: class.negated,
        ranges,
        posix,
    })
}

impl Compiler {
    /// `b1|b2|…|bn`: a chain of splits, each preferring its branch (earlier
    /// branches win ties — leftmost-first).
    fn alternation(&mut self, branches: &[Ast]) -> Result<(), Error> {
        let mut jumps = Vec::new();
        let last = branches.len() - 1;
        for (i, branch) in branches.iter().enumerate() {
            if i < last {
                let split = self.push(Inst::Split {
                    first: 0,
                    second: 0,
                })?;
                self.insts[split] = Inst::Split {
                    first: split + 1,
                    second: 0, // patched below
                };
                self.emit(branch)?;
                jumps.push(self.push(Inst::Jump(0))?);
                let after = self.next();
                if let Inst::Split { second, .. } = &mut self.insts[split] {
                    *second = after;
                }
            } else {
                self.emit(branch)?;
            }
        }
        let end = self.next();
        for jump in jumps {
            self.insts[jump] = Inst::Jump(end);
        }
        Ok(())
    }

    /// Quantifiers compile by repetition: for `{m,}`, `m` copies with the
    /// last one looping (zero copies plus a standalone loop for `m = 0`);
    /// for `{m,n}`, `m` mandatory copies then `n - m` greedy optionals.
    ///
    /// Loop-backs target the *shared* body of the final copy, and the
    /// loop-back is a Split rather than a Jump to the entry Split. Both are
    /// load-bearing for capture agreement with the regex crate on bodies
    /// that can match empty: the epsilon-closure visited set must let a
    /// final empty iteration record its captures when nothing was consumed
    /// (`(a?)*` on "b" reports group 1 = "", not absent) yet kill it when a
    /// consuming iteration already happened (`(a?)*` on "aab" reports "a",
    /// not "") — which is exactly what sharing the body tail achieves.
    fn repeat(&mut self, ast: &Ast, min: u32, max: Option<u32>, slot: usize) -> Result<(), Error> {
        if min > MAX_REPETITION_SIZE || max.is_some_and(|m| m > MAX_REPETITION_SIZE) {
            return Err(Error::new(ErrorKind::RepetitionTooLarge, None));
        }
        // Hidden span tags around the whole construct (see `number`).
        self.push(Inst::Save(slot))?;
        match max {
            None => {
                for _ in 1..min {
                    self.emit(ast)?;
                }
                let enter = if min == 0 {
                    // Star: entering the loop at all is optional.
                    let enter = self.push(Inst::Split {
                        first: 0,
                        second: 0,
                    })?;
                    self.insts[enter] = Inst::Split {
                        first: enter + 1,
                        second: 0, // patched below
                    };
                    Some(enter)
                } else {
                    None
                };
                let body = self.next();
                self.emit(ast)?;
                let exit = self.push(Inst::Split {
                    first: body,
                    second: 0, // patched below
                })?;
                let after = self.next();
                for split in enter.into_iter().chain([exit]) {
                    if let Inst::Split { second, .. } = &mut self.insts[split] {
                        *second = after;
                    }
                }
            }
            Some(max) => {
                for _ in 0..min {
                    self.emit(ast)?;
                }
                // Greedy optionals: each split prefers its copy; on the
                // first skip, jump past the whole chain.
                let mut splits = Vec::new();
                for _ in min..max {
                    let split = self.push(Inst::Split {
                        first: 0,
                        second: 0,
                    })?;
                    self.insts[split] = Inst::Split {
                        first: split + 1,
                        second: 0, // patched below
                    };
                    splits.push(split);
                    self.emit(ast)?;
                }
                let end = self.next();
                for split in splits {
                    if let Inst::Split { second, .. } = &mut self.insts[split] {
                        *second = end;
                    }
                }
            }
        }
        self.push(Inst::Save(slot + 1))?;
        Ok(())
    }
}
