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

/// The substring fast path for pure-literal patterns (see
/// [`Program::literal`]): the leftmost match's byte span, or `None`.
/// Leftmost-first and POSIX agree here — a literal has exactly one
/// possible span per start position.
fn literal_span(lit: &Literal, text: &str) -> Option<(usize, usize)> {
    let n = lit.s.len();
    match (lit.anchored_start, lit.anchored_end) {
        (true, true) => (text == lit.s).then_some((0, n)),
        (true, false) => text.starts_with(&lit.s).then_some((0, n)),
        (false, true) => text.ends_with(&lit.s).then(|| (text.len() - n, text.len())),
        (false, false) => text.find(&lit.s).map(|i| (i, i + n)),
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
        Inst::AnyChar if c.is_some() => Step::Advance,
        Inst::Class(i) if fc.is_some_and(|ch| program.classes[i].matches(ch)) => Step::Advance,
        Inst::Char(_) | Inst::AnyChar | Inst::Class(_) => Step::Die,
        Inst::Match => Step::Matched,
        // Epsilon instructions are resolved inside the closures.
        Inst::Split { .. }
        | Inst::Jump(_)
        | Inst::Save(_)
        | Inst::StartAnchor
        | Inst::EndAnchor => unreachable!("epsilon inst in thread list"),
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

/// Executes `program` against `text` as an unanchored, leftmost-first
/// search.
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
) -> Option<Vec<Option<(usize, usize)>>> {
    if let Some(lit) = &program.literal {
        // A literal pattern has no groups: group 0 is the only capture.
        return literal_span(lit, text).map(|span| vec![Some(span)]);
    }
    let len = text.len();
    let mut clist: Vec<(usize, Slots)> = Vec::new();
    let mut nlist: Vec<(usize, Slots)> = Vec::new();
    // Generation-stamped visited set: bumping `gen` invalidates every
    // entry in O(1) instead of an O(program) clear per input char.
    let mut visited = vec![0u64; program.insts.len()];
    let mut gen = 1u64;
    // Reused across every add_thread call; always drained back to empty.
    let mut stack: Vec<(usize, Slots)> = Vec::new();
    let mut matched: Option<Slots> = None;

    let initial = Rc::new(vec![None; slot_limit.min(program.slot_count)]);
    add_thread(
        program,
        &mut clist,
        &mut visited,
        gen,
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
        // no live thread carries progress (see `at_restart`), nothing can
        // match before the next occurrence of that char — skip straight
        // to it.
        if !program.prefix.is_empty() && matched.is_none() && at_restart(&clist, &restart, pos) {
            match text[pos..].find(&*program.prefix) {
                Some(0) => {}
                Some(off) => {
                    pos += off;
                    gen += 1;
                    clist.clear();
                    add_thread(
                        program,
                        &mut clist,
                        &mut visited,
                        gen,
                        &mut stack,
                        0,
                        Rc::new(vec![None; slot_limit.min(program.slot_count)]),
                        pos,
                        len,
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
        gen += 1;
        nlist.clear();
        for (pc, slots) in clist.drain(..) {
            match step(program, pc, c, fc) {
                Step::Advance => add_thread(
                    program,
                    &mut nlist,
                    &mut visited,
                    gen,
                    &mut stack,
                    pc + 1,
                    slots,
                    next_pos,
                    len,
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
        std::mem::swap(&mut clist, &mut nlist);
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
    if let Some(lit) = &program.literal {
        return literal_span(lit, text).is_some();
    }
    let len = text.len();
    let mut clist: Vec<usize> = Vec::new();
    let mut nlist: Vec<usize> = Vec::new();
    let mut visited = vec![0u64; program.insts.len()];
    let mut gen = 1u64;
    let mut stack: Vec<usize> = Vec::new();
    add_thread_bool(
        program,
        &mut clist,
        &mut visited,
        gen,
        &mut stack,
        0,
        0,
        len,
    );
    // Boolean threads are bare pcs, so pc-list equality with the restart
    // state is exact state equality — see the fast-forward note in `exec`.
    let restart: Vec<usize> = clist.clone();

    let mut pos = 0;
    loop {
        if !program.prefix.is_empty() && clist == restart {
            match text[pos..].find(&*program.prefix) {
                Some(0) => {}
                Some(off) => pos += off,
                None => return false,
            }
        }
        let c = text[pos..].chars().next();
        let next_pos = pos + c.map_or(0, char::len_utf8);
        let fc = if program.icase { c.map(fold) } else { c };
        gen += 1;
        nlist.clear();
        for pc in clist.drain(..) {
            match step(program, pc, c, fc) {
                Step::Advance => add_thread_bool(
                    program,
                    &mut nlist,
                    &mut visited,
                    gen,
                    &mut stack,
                    pc + 1,
                    next_pos,
                    len,
                ),
                Step::Die => {}
                Step::Matched => return true,
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

/// The mutable state of one POSIX-mode step: per-pc best slot vectors,
/// generation-stamped so invalidation is O(1) per step, plus the discovery
/// order of consuming/Match pcs.
struct PosixStep {
    best: Vec<Option<Slots>>,
    best_gen: Vec<u64>,
    gen: u64,
    order: Vec<usize>,
}

/// Executes `program` against `text` as an unanchored, leftmost-longest
/// (POSIX) search — the v2 opt-in mode behind [`crate::Regex::new_posix`].
///
/// Same return contract as [`exec`]. Instead of cutting on `Match`, every
/// candidate runs to completion and the best capture vector wins under
/// [`posix_better`].
/// See [`exec`] for `slot_limit`. Group-0-only tracking stays correct
/// here: truncated vectors compare on the group-0 pair alone, which is
/// exactly overall leftmost-longest, and ties keep the incumbent — same
/// group-0 span either way.
pub fn exec_posix(
    program: &Program,
    text: &str,
    slot_limit: usize,
) -> Option<Vec<Option<(usize, usize)>>> {
    if let Some(lit) = &program.literal {
        // Fixed-length literal: leftmost-first and leftmost-longest agree.
        return literal_span(lit, text).map(|span| vec![Some(span)]);
    }
    let len = text.len();
    let mut st = PosixStep {
        best: vec![None; program.insts.len()],
        best_gen: vec![0; program.insts.len()],
        gen: 1,
        order: Vec::new(),
    };
    let mut clist: Vec<(usize, Slots)> = Vec::new();
    let mut stack: Vec<(usize, Slots)> = Vec::new();
    let mut best_match: Option<Slots> = None;

    let initial = Rc::new(vec![None; slot_limit.min(program.slot_count)]);
    closure_posix(program, &mut st, &mut stack, 0, initial, 0, len);
    harvest(&mut st, &mut clist);
    // See the fast-forward note in `exec`.
    let restart: Vec<usize> = clist.iter().map(|t| t.0).collect();

    let mut pos = 0;
    loop {
        if !program.prefix.is_empty() && best_match.is_none() && at_restart(&clist, &restart, pos) {
            match text[pos..].find(&*program.prefix) {
                Some(0) => {}
                Some(off) => {
                    pos += off;
                    st.gen += 1;
                    clist.clear();
                    closure_posix(
                        program,
                        &mut st,
                        &mut stack,
                        0,
                        Rc::new(vec![None; slot_limit.min(program.slot_count)]),
                        pos,
                        len,
                    );
                    harvest(&mut st, &mut clist);
                }
                None => break,
            }
        }
        let c = text[pos..].chars().next();
        let next_pos = pos + c.map_or(0, char::len_utf8);
        let fc = if program.icase { c.map(fold) } else { c };
        // Closures during this drain run under a fresh generation; a
        // harvest never shares a generation with a later closure, so a
        // taken (`None`) entry can never be mistaken for a live claim.
        st.gen += 1;
        for (pc, slots) in clist.drain(..) {
            match step(program, pc, c, fc) {
                Step::Advance => {
                    closure_posix(program, &mut st, &mut stack, pc + 1, slots, next_pos, len)
                }
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
        harvest(&mut st, &mut clist);
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

/// Moves the step's surviving threads out of `st` into `clist`. Stale
/// `best` entries are invalidated by the next generation bump — no
/// O(program) reset here.
fn harvest(st: &mut PosixStep, clist: &mut Vec<(usize, Slots)>) {
    for pc in st.order.drain(..) {
        let slots = st.best[pc].take().expect("ordered pc has best slots");
        clist.push((pc, slots));
    }
}

/// The POSIX-mode epsilon closure: like [`add_thread`], but a thread
/// reaching an already-claimed pc replaces the incumbent when its slots
/// compare better, re-propagating downstream. Each pc's value strictly
/// improves on every replacement, so the closure terminates; total work per
/// step is `O(program^2)` worst case.
fn closure_posix(
    program: &Program,
    st: &mut PosixStep,
    stack: &mut Vec<(usize, Slots)>,
    pc: usize,
    slots: Slots,
    pos: usize,
    len: usize,
) {
    debug_assert!(stack.is_empty());
    stack.push((pc, slots));
    while let Some((pc, slots)) = stack.pop() {
        if st.best_gen[pc] == st.gen {
            // Claimed this step: replace only if strictly better.
            match &st.best[pc] {
                Some(cur) if !posix_better(program, &slots, cur) => continue,
                _ => {}
            }
        } else {
            // First claim this step.
            st.best_gen[pc] = st.gen;
            if matches!(
                program.insts[pc],
                Inst::Char(_) | Inst::AnyChar | Inst::Class(_) | Inst::Match
            ) {
                st.order.push(pc);
            }
        }
        st.best[pc] = Some(slots.clone());
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
