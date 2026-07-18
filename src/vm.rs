//! The Pike VM (roadmap steps 2–3), plus the POSIX mode (v2).
//!
//! Breadth-first NFA simulation with per-thread capture slots.
//!
//! **Non-negotiable: no backtracking.** Each input character is processed
//! once against a deduplicated thread list, so execution cannot go
//! exponential regardless of the pattern — `(a+)+b` can never hang the
//! shell. [`exec`] (leftmost-first) is `O(len(text) * len(program))`;
//! [`exec_posix`] replaces first-wins deduplication with best-wins
//! comparison and re-propagation, which is `O(len(text) * len(program)^2)`
//! worst case — still polynomial, never exponential.
//!
//! [`exec`] match semantics are leftmost-first (Perl-style), encoded
//! entirely in thread priority: `Split` order makes greedy operators prefer
//! another iteration and alternations prefer earlier branches, and when a
//! thread reaches `Match`, every lower-priority thread is cut. A surviving
//! higher-priority thread may still overwrite the recorded match later.
//!
//! [`exec_posix`] ignores priority entirely and disambiguates by comparing
//! capture-slot vectors ([`posix_better`]): earlier group-0 start
//! (leftmost), then later group-0 end (longest), then the same rule per
//! group in index order — the classic leftmost-longest approximation used
//! by RE2's POSIX mode, matching what bash/glibc report.

use crate::compile::{fold, Inst, Literal, Program};
use std::rc::Rc;

/// Byte offsets recorded by `Save`; two slots per group.
///
/// Reference-counted copy-on-write: a `Split` shares the vector between
/// both threads in O(1), and only a `Save` on a shared vector pays for a
/// clone (`Rc::make_mut`). `Rc` never escapes the VM, so `Regex` stays
/// `Send + Sync`.
type Slots = Rc<Vec<Option<usize>>>;

/// ASCII case-insensitive substring search: used only for icase literal
/// patterns, which are restricted to ASCII text (see [`Program::literal`]),
/// so this can stay a plain byte scan. Safe over raw bytes even though
/// `text` may hold multi-byte UTF-8 elsewhere: a continuation/lead byte
/// (>= 0x80) never equals an ASCII needle byte under `eq_ignore_ascii_case`
/// (it only case-flips `A-Za-z`, and falls back to plain equality
/// otherwise), so a byte match can never straddle a char boundary.
///
/// Skips straight to candidates whose first byte matches (either case)
/// before paying for the full-needle comparison — the same trick
/// `find_prefix` already uses for the icase prefix scan. Still `O(n·m)`
/// worst case (no Two-Way/Boyer-Moore here), but avoids the per-position
/// `eq_ignore_ascii_case` call for the common case where the first byte
/// alone rules a position out.
fn ascii_find_ignore_case(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if hay.len() < needle.len() {
        return None;
    }
    let (lo, hi) = (
        needle[0].to_ascii_lowercase(),
        needle[0].to_ascii_uppercase(),
    );
    (0..=hay.len() - needle.len())
        .filter(|&i| hay[i] == lo || hay[i] == hi)
        .find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// The substring fast path for pure-literal patterns (see
/// [`Program::literal`]): the leftmost match's byte span, or `None`.
/// Leftmost-first and POSIX agree here — a literal has exactly one
/// possible span per start position.
fn literal_span(lit: &Literal, text: &str, from: usize) -> Option<(usize, usize)> {
    let n = lit.s.len();
    if lit.icase {
        let (t, s) = (text.as_bytes(), lit.s.as_bytes());
        return match (lit.anchored_start, lit.anchored_end) {
            (true, true) => {
                (from == 0 && t.len() == n && t.eq_ignore_ascii_case(s)).then_some((0, n))
            }
            (true, false) => {
                (from == 0 && t.len() >= n && t[..n].eq_ignore_ascii_case(s)).then_some((0, n))
            }
            (false, true) => {
                (t.len() >= n && t.len() - n >= from && t[t.len() - n..].eq_ignore_ascii_case(s))
                    .then(|| (t.len() - n, t.len()))
            }
            (false, false) => {
                ascii_find_ignore_case(&t[from..], s).map(|i| (from + i, from + i + n))
            }
        };
    }
    match (lit.anchored_start, lit.anchored_end) {
        // `^`-anchored: only reachable when the search starts at 0.
        (true, true) => (from == 0 && text == lit.s).then_some((0, n)),
        (true, false) => (from == 0 && text.starts_with(&lit.s)).then_some((0, n)),
        (false, true) => (text.len() >= n && text.len() - n >= from && text.ends_with(&lit.s))
            .then(|| (text.len() - n, text.len())),
        (false, false) => text[from..].find(&lit.s).map(|i| (from + i, from + i + n)),
    }
}

/// glibc's word character for `\b`/`\w`-family assertions:
/// `[[:alnum:]_]`, with this crate's documented Unicode-locale stance on
/// `alnum`. `None` (text edge) is never a word char.
fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// The scan fast-forward search: a plain substring search, or — in
/// `icase` mode, where the prefix chars are pre-folded at compile time —
/// a fold-and-compare scan. The folded scan is O(len * prefix) worst
/// case, but a fold call per char is far cheaper than stepping the full
/// thread machinery, which is what the fast-forward replaces.
fn find_prefix(program: &Program, hay: &str) -> Option<usize> {
    if !program.icase {
        return hay.find(&*program.prefix);
    }
    let first = program.prefix.chars().next()?;
    for (i, c) in hay.char_indices() {
        if fold(c) != first {
            continue;
        }
        let mut want = program.prefix.chars().skip(1);
        let mut have = hay[i..].chars().skip(1);
        loop {
            match (want.next(), have.next()) {
                (None, _) => return Some(i),
                (Some(w), Some(h)) if fold(h) == w => {}
                _ => break,
            }
        }
    }
    None
}

/// Whether the program has any scan fast-forward hint.
fn has_scan_hint(program: &Program) -> bool {
    !program.prefix.is_empty() || program.first_class.is_some()
}

/// The next candidate match start in `hay`: the literal prefix's next
/// occurrence, or — for class-headed patterns — the next char of the
/// mandatory head class (ASCII via the bitmap on a byte loop; folded
/// compare in `icase` mode).
fn find_scan_hint(program: &Program, hay: &str) -> Option<usize> {
    if !program.prefix.is_empty() {
        return find_prefix(program, hay);
    }
    let class = &program.classes[program.first_class.expect("checked by has_scan_hint")];
    if program.icase {
        return hay
            .char_indices()
            .find(|&(_, c)| class.matches(fold(c)))
            .map(|(i, _)| i);
    }
    let bytes = hay.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 128 {
            if class.matches(b as char) {
                return Some(i);
            }
            i += 1;
        } else {
            let c = hay[i..].chars().next().expect("boundary");
            if class.matches(c) {
                return Some(i);
            }
            i += c.len_utf8();
        }
    }
    None
}

/// Whether `hay`, case-folded char by char, begins with pre-folded `needle`.
fn starts_with_folded(hay: &str, needle: &str) -> bool {
    let mut h = hay.chars();
    needle
        .chars()
        .all(|nc| h.next().is_some_and(|hc| fold(hc) == nc))
}

/// Whether `hay`, case-folded, ends with pre-folded `needle`.
fn ends_with_folded(hay: &str, needle: &str) -> bool {
    let mut h = hay.chars().rev();
    needle
        .chars()
        .rev()
        .all(|nc| h.next().is_some_and(|hc| fold(hc) == nc))
}

/// Whether `hay`, case-folded, contains pre-folded `needle` anywhere.
fn contains_folded(hay: &str, needle: &str) -> bool {
    needle.is_empty()
        || hay
            .char_indices()
            .any(|(i, _)| starts_with_folded(&hay[i..], needle))
}

/// The mandatory-suffix quick reject: every match must end with
/// `program.suffix`, so text that doesn't contain it (or doesn't end
/// with it, for `\$`-anchored patterns) can't match — one substring
/// scan instead of a full NFA simulation. In `icase` mode `program.suffix`
/// is pre-folded, so the comparison folds the haystack too.
fn suffix_rejects(program: &Program, text: &str, from: usize) -> bool {
    if program.suffix.is_empty() {
        return false;
    }
    // The suffix must lie within the searched region [from..].
    match (program.suffix_anchored, program.icase) {
        (true, false) => !text[from..].ends_with(&program.suffix),
        (false, false) => !text[from..].contains(&program.suffix),
        (true, true) => !ends_with_folded(&text[from..], &program.suffix),
        (false, true) => !contains_folded(&text[from..], &program.suffix),
    }
}

/// One thread's outcome stepping over the current input char — the single
/// point of truth for consuming-instruction dispatch, shared by all three
/// execution modes (`c` is the raw char, `fc` the case-folded one).
enum Step {
    /// The instruction consumed the char; continue at `pc + 1`.
    Advance,
    /// The thread dies.
    Die,
    /// The thread reached `Match`.
    Matched,
}

fn step(program: &Program, pc: usize, c: Option<char>, fc: Option<char>) -> Step {
    match program.insts[pc] {
        Inst::Char(x) if fc == Some(x) => Step::Advance,
        Inst::ScanAny if c.is_some() => Step::Advance,
        Inst::AnyChar if c.is_some() && !(program.newline && c == Some('\n')) => Step::Advance,
        Inst::Class(i) if fc.is_some_and(|ch| program.classes[i].matches(ch)) => Step::Advance,
        Inst::Char(_) | Inst::ScanAny | Inst::AnyChar | Inst::Class(_) => Step::Die,
        Inst::Match => Step::Matched,
        // Epsilon instructions are resolved inside the closures.
        Inst::Split { .. }
        | Inst::Jump(_)
        | Inst::Save(_)
        | Inst::StartAnchor
        | Inst::EndAnchor
        | Inst::BufferStart
        | Inst::BufferEnd
        | Inst::WordBoundary
        | Inst::NotWordBoundary
        | Inst::WordStart
        | Inst::WordEnd => unreachable!("epsilon inst in thread list"),
    }
}

/// Whether a slot-carrying thread list is indistinguishable from a fresh
/// restart at `pos`: same pcs as the position-0 restart state, and every
/// recorded offset equals `pos` — no thread carries progress. The
/// fast-forward precondition (see [`exec`]).
fn at_restart(clist: &[(usize, Slots)], restart: &[usize], pos: usize) -> bool {
    clist.len() == restart.len()
        && clist.iter().map(|t| t.0).eq(restart.iter().copied())
        && clist
            .iter()
            .all(|(_, s)| s.iter().flatten().all(|&v| v == pos))
}

/// Reusable VM working state. The one-shot entry points build one per
/// call; the iteration APIs reuse one across restarts so buffer capacity
/// is amortized over all matches. Generations are monotonic within a
/// `Scratch`, so stale visited/best stamps can never collide across
/// reuses; thread lists are cleared at each entry.
#[derive(Debug, Default)]
pub struct Scratch {
    clist: Vec<(usize, Slots)>,
    nlist: Vec<(usize, Slots)>,
    stack: Vec<(usize, Slots)>,
    bclist: Vec<usize>,
    bnlist: Vec<usize>,
    bstack: Vec<usize>,
    visited: Vec<u64>,
    best: Vec<Option<Slots>>,
    best_gen: Vec<u64>,
    order: Vec<usize>,
    gen: u64,
}

impl Scratch {
    fn ensure(&mut self, n: usize) {
        if self.visited.len() < n {
            self.visited.resize(n, 0);
            self.best.resize(n, None);
            self.best_gen.resize(n, 0);
        }
    }
}

/// Executes `program` against `text` as an unanchored, leftmost-first
/// search starting at byte offset `from` (a `char` boundary). Anchors
/// keep their absolute meaning: `^` still asserts position 0, not
/// `from` — which is what makes the iteration APIs correct.
///
/// On a match, returns one `(start, end)` byte-offset pair per capture
/// group (group 0 first); groups that did not participate are `None`.
/// `slot_limit` bounds capture tracking: `Save`s to slots at or past it
/// are ignored and slot vectors are allocated that long. Pass
/// `program.slot_count` for full captures, or 2 to track only group 0
/// ([`crate::Regex::find`]) at near-boolean cost.
pub fn exec(
    program: &Program,
    text: &str,
    slot_limit: usize,
    from: usize,
    scratch: &mut Scratch,
) -> Option<Vec<Option<(usize, usize)>>> {
    if let Some(lit) = &program.literal {
        // A literal pattern has no groups: group 0 is the only capture.
        return literal_span(lit, text, from).map(|span| vec![Some(span)]);
    }
    if suffix_rejects(program, text, from) {
        return None;
    }
    let len = text.len();
    scratch.ensure(program.insts.len());
    let Scratch {
        clist,
        nlist,
        stack,
        visited,
        gen,
        ..
    } = scratch;
    clist.clear();
    nlist.clear();
    *gen += 1;
    let mut matched: Option<Slots> = None;

    // Word-boundary assertions need the chars adjacent to the position.
    let prev0 = text[..from].chars().next_back();
    let next0 = text[from..].chars().next();
    let initial = Rc::new(vec![None; slot_limit.min(program.slot_count)]);
    add_thread(
        program, clist, visited, *gen, stack, 0, initial, from, len, prev0, next0,
    );
    // The thread state a scan (re)starts from; used by the fast-forward
    // check below. Captured at `from`: for `from == 0`, a pattern with an
    // anchored head branch shrinks its restart set afterwards, so the
    // check never fires (safe, just unaccelerated); for `from > 0` the
    // anchored heads are already gone and it fires normally.
    let restart: Vec<usize> = clist.iter().map(|t| t.0).collect();

    let mut pos = from;
    loop {
        // Fast-forward: when the pattern requires a literal prefix and no
        // live thread carries progress (see `at_restart`), nothing can
        // match before the prefix's next occurrence — skip straight to it.
        if has_scan_hint(program) && matched.is_none() && at_restart(clist, &restart, pos) {
            match find_scan_hint(program, &text[pos..]) {
                Some(0) => {}
                Some(off) => {
                    pos += off;
                    *gen += 1;
                    clist.clear();
                    add_thread(
                        program,
                        clist,
                        visited,
                        *gen,
                        stack,
                        0,
                        Rc::new(vec![None; slot_limit.min(program.slot_count)]),
                        pos,
                        len,
                        text[..pos].chars().next_back(),
                        text[pos..].chars().next(),
                    );
                }
                // The required prefix never occurs again: no match.
                None => break,
            }
        }
        let c = text[pos..].chars().next();
        let next_pos = pos + c.map_or(0, char::len_utf8);
        // The pattern side was folded at compile time; fold the input to
        // match. Positions (and so captures) always use the original text.
        let fc = if program.icase { c.map(fold) } else { c };
        // At next_pos, the previous char is `c`; look one char ahead for
        // the word-boundary assertions.
        let next_c = text[next_pos..].chars().next();
        *gen += 1;
        nlist.clear();
        for (pc, slots) in clist.drain(..) {
            match step(program, pc, c, fc) {
                Step::Advance => add_thread(
                    program,
                    nlist,
                    visited,
                    *gen,
                    stack,
                    pc + 1,
                    slots,
                    next_pos,
                    len,
                    c,
                    next_c,
                ),
                Step::Die => {}
                Step::Matched => {
                    // Cut every lower-priority thread; higher-priority
                    // threads already queued in nlist survive and may
                    // overwrite this match on a later step.
                    matched = Some(slots);
                    break;
                }
            }
        }
        std::mem::swap(clist, nlist);
        if c.is_none() || clist.is_empty() {
            break;
        }
        pos = next_pos;
    }

    matched.map(|slots| {
        (0..(slots.len() / 2).min(program.group_count))
            .map(|i| match (slots[2 * i], slots[2 * i + 1]) {
                (Some(start), Some(end)) => Some((start, end)),
                _ => None,
            })
            .collect()
    })
}

/// Adds a thread to `list`, following epsilon transitions (`Split`, `Jump`,
/// `Save`, anchors) until consuming instructions are reached.
///
/// `visited` deduplicates by program counter (an entry is claimed when its
/// stamp equals the current generation): the first (highest-priority)
/// thread to reach a pc claims it, which both preserves leftmost-first
/// priority and bounds work per step to one visit per instruction — the
/// linear-time guarantee. Iterative so pathological epsilon chains cannot
/// overflow the stack.
#[allow(clippy::too_many_arguments)] // internal; mirrors the VM's state
fn add_thread(
    program: &Program,
    list: &mut Vec<(usize, Slots)>,
    visited: &mut [u64],
    gen: u64,
    stack: &mut Vec<(usize, Slots)>,
    pc: usize,
    slots: Slots,
    pos: usize,
    len: usize,
    prev: Option<char>,
    next: Option<char>,
) {
    debug_assert!(stack.is_empty());
    stack.push((pc, slots));
    while let Some((pc, slots)) = stack.pop() {
        if visited[pc] == gen {
            continue;
        }
        visited[pc] = gen;
        match &program.insts[pc] {
            Inst::Jump(target) => stack.push((*target, slots)),
            Inst::Split { first, second } => {
                // `first` is explored (and claims pcs) before `second`.
                // Sharing, not cloning: Save copies on write if needed.
                stack.push((*second, slots.clone()));
                stack.push((*first, slots));
            }
            Inst::Save(slot) => {
                let mut slots = slots;
                if *slot < slots.len() {
                    Rc::make_mut(&mut slots)[*slot] = Some(pos);
                }
                stack.push((pc + 1, slots));
            }
            Inst::StartAnchor => {
                if pos == 0 || (program.newline && prev == Some('\n')) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::EndAnchor => {
                if pos == len || (program.newline && next == Some('\n')) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::BufferStart => {
                if pos == 0 {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::BufferEnd => {
                if pos == len {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::WordBoundary => {
                if is_word(prev) != is_word(next) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::NotWordBoundary => {
                if is_word(prev) == is_word(next) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::WordStart => {
                if !is_word(prev) && is_word(next) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::WordEnd => {
                if is_word(prev) && !is_word(next) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::Char(_) | Inst::ScanAny | Inst::AnyChar | Inst::Class(_) | Inst::Match => {
                list.push((pc, slots));
            }
        }
    }
}

/// Executes `program` against `text` as a pure boolean test, starting at
/// byte offset `from` (see [`exec`] for anchor semantics).
///
/// No capture tracking: threads are bare program counters, so `Split`
/// costs O(1) instead of cloning a slot vector. Match *existence* is
/// identical across leftmost-first and POSIX semantics (both are "does
/// any match exist?"), so this single path serves every mode.
pub fn exec_bool(program: &Program, text: &str, from: usize, scratch: &mut Scratch) -> bool {
    if let Some(lit) = &program.literal {
        return literal_span(lit, text, from).is_some();
    }
    if suffix_rejects(program, text, from) {
        return false;
    }
    let len = text.len();
    scratch.ensure(program.insts.len());
    let Scratch {
        bclist: clist,
        bnlist: nlist,
        bstack: stack,
        visited,
        gen,
        ..
    } = scratch;
    clist.clear();
    nlist.clear();
    *gen += 1;
    let prev0 = text[..from].chars().next_back();
    let next0 = text[from..].chars().next();
    add_thread_bool(
        program, clist, visited, *gen, stack, 0, from, len, prev0, next0,
    );
    // Boolean threads are bare pcs, so pc-list equality with the restart
    // state is exact state equality — see the fast-forward note in `exec`.
    let restart: Vec<usize> = clist.clone();

    let mut pos = from;
    loop {
        if has_scan_hint(program) && *clist == restart {
            match find_scan_hint(program, &text[pos..]) {
                Some(0) => {}
                Some(off) => {
                    pos += off;
                    // Re-seed at the new position: with assertion-headed
                    // patterns the spawn set is position-dependent (an
                    // earlier position's \b verdict must not be carried).
                    *gen += 1;
                    clist.clear();
                    add_thread_bool(
                        program,
                        clist,
                        visited,
                        *gen,
                        stack,
                        0,
                        pos,
                        len,
                        text[..pos].chars().next_back(),
                        text[pos..].chars().next(),
                    );
                }
                None => return false,
            }
        }
        let c = text[pos..].chars().next();
        let next_pos = pos + c.map_or(0, char::len_utf8);
        let fc = if program.icase { c.map(fold) } else { c };
        let next_c = text[next_pos..].chars().next();
        *gen += 1;
        nlist.clear();
        for pc in clist.drain(..) {
            match step(program, pc, c, fc) {
                Step::Advance => add_thread_bool(
                    program,
                    nlist,
                    visited,
                    *gen,
                    stack,
                    pc + 1,
                    next_pos,
                    len,
                    c,
                    next_c,
                ),
                Step::Die => {}
                Step::Matched => return true,
            }
        }
        std::mem::swap(clist, nlist);
        if c.is_none() || clist.is_empty() {
            return false;
        }
        pos = next_pos;
    }
}

/// [`add_thread`] without slot tracking: `Save` becomes a no-op.
#[allow(clippy::too_many_arguments)] // internal; mirrors the VM's state
fn add_thread_bool(
    program: &Program,
    list: &mut Vec<usize>,
    visited: &mut [u64],
    gen: u64,
    stack: &mut Vec<usize>,
    pc: usize,
    pos: usize,
    len: usize,
    prev: Option<char>,
    next: Option<char>,
) {
    debug_assert!(stack.is_empty());
    stack.push(pc);
    while let Some(pc) = stack.pop() {
        if visited[pc] == gen {
            continue;
        }
        visited[pc] = gen;
        match &program.insts[pc] {
            Inst::Jump(target) => stack.push(*target),
            Inst::Split { first, second } => {
                stack.push(*second);
                stack.push(*first);
            }
            Inst::Save(_) => stack.push(pc + 1),
            Inst::StartAnchor => {
                if pos == 0 || (program.newline && prev == Some('\n')) {
                    stack.push(pc + 1);
                }
            }
            Inst::EndAnchor => {
                if pos == len || (program.newline && next == Some('\n')) {
                    stack.push(pc + 1);
                }
            }
            Inst::BufferStart => {
                if pos == 0 {
                    stack.push(pc + 1);
                }
            }
            Inst::BufferEnd => {
                if pos == len {
                    stack.push(pc + 1);
                }
            }
            Inst::WordBoundary => {
                if is_word(prev) != is_word(next) {
                    stack.push(pc + 1);
                }
            }
            Inst::NotWordBoundary => {
                if is_word(prev) == is_word(next) {
                    stack.push(pc + 1);
                }
            }
            Inst::WordStart => {
                if !is_word(prev) && is_word(next) {
                    stack.push(pc + 1);
                }
            }
            Inst::WordEnd => {
                if is_word(prev) && !is_word(next) {
                    stack.push(pc + 1);
                }
            }
            Inst::Char(_) | Inst::ScanAny | Inst::AnyChar | Inst::Class(_) | Inst::Match => {
                list.push(pc);
            }
        }
    }
}

/// Executes `program` against `text` as an unanchored, leftmost-longest
/// (POSIX) search starting at byte offset `from` — the v2 opt-in mode
/// behind [`crate::Regex::new_posix`].
///
/// Same return contract as [`exec`]. Instead of cutting on `Match`, every
/// candidate runs to completion and the best capture vector wins under
/// [`posix_better`].
pub fn exec_posix(
    program: &Program,
    text: &str,
    slot_limit: usize,
    from: usize,
    scratch: &mut Scratch,
) -> Option<Vec<Option<(usize, usize)>>> {
    if let Some(lit) = &program.literal {
        // Fixed-length literal: leftmost-first and leftmost-longest agree.
        return literal_span(lit, text, from).map(|span| vec![Some(span)]);
    }
    if suffix_rejects(program, text, from) {
        return None;
    }
    let len = text.len();
    scratch.ensure(program.insts.len());
    let Scratch {
        clist,
        stack,
        best,
        best_gen,
        order,
        gen,
        ..
    } = scratch;
    clist.clear();
    order.clear();
    *gen += 1;
    let mut best_match: Option<Slots> = None;

    let prev0 = text[..from].chars().next_back();
    let next0 = text[from..].chars().next();
    let initial = Rc::new(vec![None; slot_limit.min(program.slot_count)]);
    closure_posix(
        program, best, best_gen, *gen, order, stack, 0, initial, from, len, prev0, next0,
    );
    harvest(best, order, clist);
    // See the fast-forward note in `exec`.
    let restart: Vec<usize> = clist.iter().map(|t| t.0).collect();

    let mut pos = from;
    loop {
        // See the fast-forward note in `exec`: this must cover the same
        // hints (literal prefix *or* mandatory head class) `exec`/`exec_bool`
        // do. It previously checked `prefix` alone, so POSIX-mode
        // class-headed patterns (`Regex::new_posix("[0-9]+")`) never got the
        // fast-forward and fell back to unaccelerated per-char stepping.
        if has_scan_hint(program) && best_match.is_none() && at_restart(clist, &restart, pos) {
            match find_scan_hint(program, &text[pos..]) {
                Some(0) => {}
                Some(off) => {
                    pos += off;
                    *gen += 1;
                    clist.clear();
                    closure_posix(
                        program,
                        best,
                        best_gen,
                        *gen,
                        order,
                        stack,
                        0,
                        Rc::new(vec![None; slot_limit.min(program.slot_count)]),
                        pos,
                        len,
                        text[..pos].chars().next_back(),
                        text[pos..].chars().next(),
                    );
                    harvest(best, order, clist);
                }
                None => break,
            }
        }
        let c = text[pos..].chars().next();
        let next_pos = pos + c.map_or(0, char::len_utf8);
        let fc = if program.icase { c.map(fold) } else { c };
        let next_c = text[next_pos..].chars().next();
        // Closures during this drain run under a fresh generation; a
        // harvest never shares a generation with a later closure, so a
        // taken (`None`) entry can never be mistaken for a live claim.
        *gen += 1;
        for (pc, slots) in clist.drain(..) {
            match step(program, pc, c, fc) {
                Step::Advance => closure_posix(
                    program,
                    best,
                    best_gen,
                    *gen,
                    order,
                    stack,
                    pc + 1,
                    slots,
                    next_pos,
                    len,
                    c,
                    next_c,
                ),
                Step::Die => {}
                Step::Matched => {
                    // (map_or, not is_none_or: MSRV is 1.75.)
                    if best_match
                        .as_ref()
                        .map_or(true, |cur| posix_better(program, &slots, cur))
                    {
                        best_match = Some(slots);
                    }
                }
            }
        }
        harvest(best, order, clist);
        if c.is_none() || clist.is_empty() {
            break;
        }
        pos = next_pos;
    }

    best_match.map(|slots| {
        (0..(slots.len() / 2).min(program.group_count))
            .map(|i| match (slots[2 * i], slots[2 * i + 1]) {
                (Some(start), Some(end)) => Some((start, end)),
                _ => None,
            })
            .collect()
    })
}

/// Moves the step's surviving threads out of `best`/`order` into `clist`.
/// Stale `best` entries are invalidated by the next generation bump — no
/// O(program) reset here.
fn harvest(best: &mut [Option<Slots>], order: &mut Vec<usize>, clist: &mut Vec<(usize, Slots)>) {
    for pc in order.drain(..) {
        let slots = best[pc].take().expect("ordered pc has best slots");
        clist.push((pc, slots));
    }
}

/// The POSIX-mode epsilon closure: like [`add_thread`], but a thread
/// reaching an already-claimed pc replaces the incumbent when its slots
/// compare better, re-propagating downstream. Each pc's value strictly
/// improves on every replacement, so the closure terminates; total work per
/// step is `O(program^2)` worst case.
#[allow(clippy::too_many_arguments)] // internal; mirrors the VM's state
fn closure_posix(
    program: &Program,
    best: &mut [Option<Slots>],
    best_gen: &mut [u64],
    gen: u64,
    order: &mut Vec<usize>,
    stack: &mut Vec<(usize, Slots)>,
    pc: usize,
    slots: Slots,
    pos: usize,
    len: usize,
    prev: Option<char>,
    next: Option<char>,
) {
    debug_assert!(stack.is_empty());
    stack.push((pc, slots));
    while let Some((pc, slots)) = stack.pop() {
        if best_gen[pc] == gen {
            // Claimed this step: replace only if strictly better.
            match &best[pc] {
                Some(cur) if !posix_better(program, &slots, cur) => continue,
                _ => {}
            }
        } else {
            // First claim this step.
            best_gen[pc] = gen;
            if matches!(
                program.insts[pc],
                Inst::Char(_) | Inst::ScanAny | Inst::AnyChar | Inst::Class(_) | Inst::Match
            ) {
                order.push(pc);
            }
        }
        best[pc] = Some(slots.clone());
        match &program.insts[pc] {
            Inst::Jump(target) => stack.push((*target, slots)),
            Inst::Split { first, second } => {
                stack.push((*second, slots.clone()));
                stack.push((*first, slots));
            }
            Inst::Save(slot) => {
                let mut slots = slots;
                if *slot < slots.len() {
                    Rc::make_mut(&mut slots)[*slot] = Some(pos);
                }
                stack.push((pc + 1, slots));
            }
            Inst::StartAnchor => {
                if pos == 0 || (program.newline && prev == Some('\n')) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::EndAnchor => {
                if pos == len || (program.newline && next == Some('\n')) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::BufferStart => {
                if pos == 0 {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::BufferEnd => {
                if pos == len {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::WordBoundary => {
                if is_word(prev) != is_word(next) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::NotWordBoundary => {
                if is_word(prev) == is_word(next) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::WordStart => {
                if !is_word(prev) && is_word(next) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::WordEnd => {
                if is_word(prev) && !is_word(next) {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::Char(_) | Inst::ScanAny | Inst::AnyChar | Inst::Class(_) | Inst::Match => {}
        }
    }
}

/// Whether `a` strictly beats `b` under leftmost-longest disambiguation.
///
/// Slot pairs are compared in `Program::tag_order` — syntactic pre-order,
/// so group 0 comes first (overall match: leftmost, then longest), an
/// outer construct's span beats anything recorded inside it, and siblings
/// compare left-to-right. For each pair an earlier start wins, then a
/// later end. Repetitions carry hidden span tags covering their full
/// extent, which is what lets an assignment that lets a repetition
/// consume more beat one with a "better" last iteration inside a shorter
/// span — the POSIX rule that earlier subexpressions match longest.
///
/// Participating-vs-absent is a *tie*, not a win: on full ties the
/// incumbent (first-arrived, i.e. earlier-alternative) thread is kept,
/// which reproduces glibc's first-branch preference when two alternatives
/// produce the same-length match (`(a)|a` reports group 1, `a|(a)`
/// doesn't).
fn posix_better(program: &Program, a: &Slots, b: &Slots) -> bool {
    for &base in &program.tag_order {
        // Untracked under the current slot limit (group-0-only mode).
        if base + 1 >= a.len() {
            continue;
        }
        match (a[base], b[base]) {
            (Some(x), Some(y)) if x != y => return x < y,
            _ => {}
        }
        match (a[base + 1], b[base + 1]) {
            (Some(x), Some(y)) if x != y => return x > y,
            _ => {}
        }
    }
    false
}
