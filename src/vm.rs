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

use crate::ast::{Class, PosixClass};
use crate::compile::{fold, Inst, Program};
use std::rc::Rc;

/// Byte offsets recorded by `Save`; two slots per group.
///
/// Reference-counted copy-on-write: a `Split` shares the vector between
/// both threads in O(1), and only a `Save` on a shared vector pays for a
/// clone (`Rc::make_mut`). `Rc` never escapes the VM, so `Regex` stays
/// `Send + Sync`.
type Slots = Rc<Vec<Option<usize>>>;

/// Executes `program` against `text` as an unanchored, leftmost-first
/// search.
///
/// On a match, returns one `(start, end)` byte-offset pair per capture
/// group (group 0 first); groups that did not participate are `None`.
pub fn exec(program: &Program, text: &str) -> Option<Vec<Option<(usize, usize)>>> {
    let len = text.len();
    let mut clist: Vec<(usize, Slots)> = Vec::new();
    let mut nlist: Vec<(usize, Slots)> = Vec::new();
    let mut visited = vec![false; program.insts.len()];
    // Reused across every add_thread call; always drained back to empty.
    let mut stack: Vec<(usize, Slots)> = Vec::new();
    let mut matched: Option<Slots> = None;

    let initial = Rc::new(vec![None; program.slot_count]);
    add_thread(
        program,
        &mut clist,
        &mut visited,
        &mut stack,
        0,
        initial,
        0,
        len,
    );
    // The thread state a scan (re)starts from; used by the fast-forward
    // check below. Captured at position 0, which never fires for patterns
    // with an anchored head branch (their restart set shrinks after 0) —
    // that is safe, just unaccelerated.
    let restart: Vec<usize> = clist.iter().map(|t| t.0).collect();

    let mut pos = 0;
    loop {
        // Fast-forward: when the pattern requires a literal first char and
        // every live thread is indistinguishable from a fresh restart
        // (same pcs, and every recorded offset equals the current position
        // — i.e. no thread carries progress), nothing can match before the
        // next occurrence of that char. Skip the scan straight to it.
        if let Some(want) = program.prefix_char {
            if matched.is_none()
                && clist.len() == restart.len()
                && clist.iter().map(|t| t.0).eq(restart.iter().copied())
                && clist
                    .iter()
                    .all(|(_, s)| s.iter().flatten().all(|&v| v == pos))
            {
                match text[pos..].find(want) {
                    Some(0) => {}
                    Some(off) => {
                        pos += off;
                        visited.fill(false);
                        clist.clear();
                        add_thread(
                            program,
                            &mut clist,
                            &mut visited,
                            &mut stack,
                            0,
                            Rc::new(vec![None; program.slot_count]),
                            pos,
                            len,
                        );
                    }
                    // The required char never occurs again: no match.
                    None => break,
                }
            }
        }
        let c = text[pos..].chars().next();
        let next_pos = pos + c.map_or(0, char::len_utf8);
        // The pattern side was folded at compile time; fold the input to
        // match. Positions (and so captures) always use the original text.
        let fc = if program.icase { c.map(fold) } else { c };
        visited.fill(false);
        nlist.clear();
        for (pc, slots) in clist.drain(..) {
            match &program.insts[pc] {
                Inst::Char(x) => {
                    if fc == Some(*x) {
                        add_thread(
                            program,
                            &mut nlist,
                            &mut visited,
                            &mut stack,
                            pc + 1,
                            slots,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::AnyChar => {
                    if c.is_some() {
                        add_thread(
                            program,
                            &mut nlist,
                            &mut visited,
                            &mut stack,
                            pc + 1,
                            slots,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::Class(i) => {
                    if fc.is_some_and(|ch| class_matches(&program.classes[*i], ch)) {
                        add_thread(
                            program,
                            &mut nlist,
                            &mut visited,
                            &mut stack,
                            pc + 1,
                            slots,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::Match => {
                    // Cut every lower-priority thread; higher-priority
                    // threads already queued in nlist survive and may
                    // overwrite this match on a later step.
                    matched = Some(slots);
                    break;
                }
                // Epsilon instructions are resolved inside add_thread.
                Inst::Split { .. }
                | Inst::Jump(_)
                | Inst::Save(_)
                | Inst::StartAnchor
                | Inst::EndAnchor => unreachable!("epsilon inst in thread list"),
            }
        }
        std::mem::swap(&mut clist, &mut nlist);
        if c.is_none() || clist.is_empty() {
            break;
        }
        pos = next_pos;
    }

    matched.map(|slots| {
        (0..program.group_count)
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
/// `visited` deduplicates by program counter: the first (highest-priority)
/// thread to reach a pc claims it, which both preserves leftmost-first
/// priority and bounds work per step to one visit per instruction — the
/// linear-time guarantee. Iterative so pathological epsilon chains cannot
/// overflow the stack.
#[allow(clippy::too_many_arguments)] // internal; mirrors the VM's state
fn add_thread(
    program: &Program,
    list: &mut Vec<(usize, Slots)>,
    visited: &mut [bool],
    stack: &mut Vec<(usize, Slots)>,
    pc: usize,
    slots: Slots,
    pos: usize,
    len: usize,
) {
    debug_assert!(stack.is_empty());
    stack.push((pc, slots));
    while let Some((pc, slots)) = stack.pop() {
        if visited[pc] {
            continue;
        }
        visited[pc] = true;
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
                Rc::make_mut(&mut slots)[*slot] = Some(pos);
                stack.push((pc + 1, slots));
            }
            Inst::StartAnchor => {
                if pos == 0 {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::EndAnchor => {
                if pos == len {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::Char(_) | Inst::AnyChar | Inst::Class(_) | Inst::Match => {
                list.push((pc, slots));
            }
        }
    }
}

/// Executes `program` against `text` as a pure boolean test.
///
/// No capture tracking: threads are bare program counters, so `Split`
/// costs O(1) instead of cloning a slot vector. Match *existence* is
/// identical across leftmost-first and POSIX semantics (both are "does
/// any match exist?"), so this single path serves every mode.
pub fn exec_bool(program: &Program, text: &str) -> bool {
    let len = text.len();
    let mut clist: Vec<usize> = Vec::new();
    let mut nlist: Vec<usize> = Vec::new();
    let mut visited = vec![false; program.insts.len()];
    let mut stack: Vec<usize> = Vec::new();
    add_thread_bool(program, &mut clist, &mut visited, &mut stack, 0, 0, len);
    // Boolean threads are bare pcs, so pc-list equality with the restart
    // state is exact state equality — see the fast-forward note in `exec`.
    let restart: Vec<usize> = clist.clone();

    let mut pos = 0;
    loop {
        if let Some(want) = program.prefix_char {
            if clist == restart {
                match text[pos..].find(want) {
                    Some(0) => {}
                    Some(off) => pos += off,
                    None => return false,
                }
            }
        }
        let c = text[pos..].chars().next();
        let next_pos = pos + c.map_or(0, char::len_utf8);
        let fc = if program.icase { c.map(fold) } else { c };
        visited.fill(false);
        nlist.clear();
        for pc in clist.drain(..) {
            match &program.insts[pc] {
                Inst::Char(x) => {
                    if fc == Some(*x) {
                        add_thread_bool(
                            program,
                            &mut nlist,
                            &mut visited,
                            &mut stack,
                            pc + 1,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::AnyChar => {
                    if c.is_some() {
                        add_thread_bool(
                            program,
                            &mut nlist,
                            &mut visited,
                            &mut stack,
                            pc + 1,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::Class(i) => {
                    if fc.is_some_and(|ch| class_matches(&program.classes[*i], ch)) {
                        add_thread_bool(
                            program,
                            &mut nlist,
                            &mut visited,
                            &mut stack,
                            pc + 1,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::Match => return true,
                Inst::Split { .. }
                | Inst::Jump(_)
                | Inst::Save(_)
                | Inst::StartAnchor
                | Inst::EndAnchor => unreachable!("epsilon inst in thread list"),
            }
        }
        std::mem::swap(&mut clist, &mut nlist);
        if c.is_none() || clist.is_empty() {
            return false;
        }
        pos = next_pos;
    }
}

/// [`add_thread`] without slot tracking: `Save` becomes a no-op.
fn add_thread_bool(
    program: &Program,
    list: &mut Vec<usize>,
    visited: &mut [bool],
    stack: &mut Vec<usize>,
    pc: usize,
    pos: usize,
    len: usize,
) {
    debug_assert!(stack.is_empty());
    stack.push(pc);
    while let Some(pc) = stack.pop() {
        if visited[pc] {
            continue;
        }
        visited[pc] = true;
        match &program.insts[pc] {
            Inst::Jump(target) => stack.push(*target),
            Inst::Split { first, second } => {
                stack.push(*second);
                stack.push(*first);
            }
            Inst::Save(_) => stack.push(pc + 1),
            Inst::StartAnchor => {
                if pos == 0 {
                    stack.push(pc + 1);
                }
            }
            Inst::EndAnchor => {
                if pos == len {
                    stack.push(pc + 1);
                }
            }
            Inst::Char(_) | Inst::AnyChar | Inst::Class(_) | Inst::Match => {
                list.push(pc);
            }
        }
    }
}

/// Executes `program` against `text` as an unanchored, leftmost-longest
/// (POSIX) search — the v2 opt-in mode behind [`crate::Regex::new_posix`].
///
/// Same return contract as [`exec`]. Instead of cutting on `Match`, every
/// candidate runs to completion and the best capture vector wins under
/// [`posix_better`].
pub fn exec_posix(program: &Program, text: &str) -> Option<Vec<Option<(usize, usize)>>> {
    let len = text.len();
    // Best slot vector seen at each pc during the current step's closure;
    // `order` lists the consuming/Match pcs discovered (each once).
    let mut best: Vec<Option<Slots>> = vec![None; program.insts.len()];
    let mut order: Vec<usize> = Vec::new();
    let mut clist: Vec<(usize, Slots)> = Vec::new();
    let mut stack: Vec<(usize, Slots)> = Vec::new();
    let mut best_match: Option<Slots> = None;

    let initial = Rc::new(vec![None; program.slot_count]);
    closure_posix(
        program, &mut best, &mut order, &mut stack, 0, initial, 0, len,
    );
    harvest(&mut best, &mut order, &mut clist);
    // See the fast-forward note in `exec`.
    let restart: Vec<usize> = clist.iter().map(|t| t.0).collect();

    let mut pos = 0;
    loop {
        if let Some(want) = program.prefix_char {
            if best_match.is_none()
                && clist.len() == restart.len()
                && clist.iter().map(|t| t.0).eq(restart.iter().copied())
                && clist
                    .iter()
                    .all(|(_, s)| s.iter().flatten().all(|&v| v == pos))
            {
                match text[pos..].find(want) {
                    Some(0) => {}
                    Some(off) => {
                        pos += off;
                        clist.clear();
                        closure_posix(
                            program,
                            &mut best,
                            &mut order,
                            &mut stack,
                            0,
                            Rc::new(vec![None; program.slot_count]),
                            pos,
                            len,
                        );
                        harvest(&mut best, &mut order, &mut clist);
                    }
                    None => break,
                }
            }
        }
        let c = text[pos..].chars().next();
        let next_pos = pos + c.map_or(0, char::len_utf8);
        let fc = if program.icase { c.map(fold) } else { c };
        for (pc, slots) in clist.drain(..) {
            match &program.insts[pc] {
                Inst::Char(x) => {
                    if fc == Some(*x) {
                        closure_posix(
                            program,
                            &mut best,
                            &mut order,
                            &mut stack,
                            pc + 1,
                            slots,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::AnyChar => {
                    if c.is_some() {
                        closure_posix(
                            program,
                            &mut best,
                            &mut order,
                            &mut stack,
                            pc + 1,
                            slots,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::Class(i) => {
                    if fc.is_some_and(|ch| class_matches(&program.classes[*i], ch)) {
                        closure_posix(
                            program,
                            &mut best,
                            &mut order,
                            &mut stack,
                            pc + 1,
                            slots,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::Match => {
                    // (map_or, not is_none_or: MSRV is 1.75.)
                    if best_match
                        .as_ref()
                        .map_or(true, |cur| posix_better(program, &slots, cur))
                    {
                        best_match = Some(slots);
                    }
                }
                Inst::Split { .. }
                | Inst::Jump(_)
                | Inst::Save(_)
                | Inst::StartAnchor
                | Inst::EndAnchor => unreachable!("epsilon inst in thread list"),
            }
        }
        harvest(&mut best, &mut order, &mut clist);
        if c.is_none() || clist.is_empty() {
            break;
        }
        pos = next_pos;
    }

    best_match.map(|slots| {
        (0..program.group_count)
            .map(|i| match (slots[2 * i], slots[2 * i + 1]) {
                (Some(start), Some(end)) => Some((start, end)),
                _ => None,
            })
            .collect()
    })
}

/// Moves the step's surviving threads out of `best`/`order` into `clist`
/// and resets `best` for the next step.
fn harvest(best: &mut [Option<Slots>], order: &mut Vec<usize>, clist: &mut Vec<(usize, Slots)>) {
    for pc in order.drain(..) {
        let slots = best[pc].take().expect("ordered pc has best slots");
        clist.push((pc, slots));
    }
    for slot in best.iter_mut() {
        *slot = None;
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
    order: &mut Vec<usize>,
    stack: &mut Vec<(usize, Slots)>,
    pc: usize,
    slots: Slots,
    pos: usize,
    len: usize,
) {
    debug_assert!(stack.is_empty());
    stack.push((pc, slots));
    while let Some((pc, slots)) = stack.pop() {
        match &best[pc] {
            Some(cur) if !posix_better(program, &slots, cur) => continue,
            Some(_) => {}
            None => {
                if matches!(
                    program.insts[pc],
                    Inst::Char(_) | Inst::AnyChar | Inst::Class(_) | Inst::Match
                ) {
                    order.push(pc);
                }
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
                Rc::make_mut(&mut slots)[*slot] = Some(pos);
                stack.push((pc + 1, slots));
            }
            Inst::StartAnchor => {
                if pos == 0 {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::EndAnchor => {
                if pos == len {
                    stack.push((pc + 1, slots));
                }
            }
            Inst::Char(_) | Inst::AnyChar | Inst::Class(_) | Inst::Match => {}
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

/// Ranges are compile-time sorted and merged (see `normalize_ranges`), so
/// membership is a binary search.
fn class_matches(class: &Class, c: char) -> bool {
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
