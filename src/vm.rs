//! The Pike VM (roadmap steps 2–3).
//!
//! Breadth-first NFA simulation with per-thread capture slots.
//!
//! **Non-negotiable: no backtracking.** Each input character is processed
//! once against a deduplicated, priority-ordered thread list, so execution
//! is `O(len(text) * len(program))` regardless of the pattern — `(a+)+b`
//! can never hang the shell.
//!
//! Match semantics are leftmost-first (Perl-style), encoded entirely in
//! thread priority: `Split` order makes greedy operators prefer another
//! iteration and alternations prefer earlier branches, and when a thread
//! reaches `Match`, every lower-priority thread is cut. A surviving
//! higher-priority thread may still overwrite the recorded match later.

use crate::ast::{Class, PosixClass};
use crate::compile::{Inst, Program};

/// Byte offsets recorded by `Save`; two slots per group.
type Slots = Vec<Option<usize>>;

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
    let mut matched: Option<Slots> = None;

    let initial = vec![None; program.group_count * 2];
    add_thread(program, &mut clist, &mut visited, 0, initial, 0, len);

    let mut steps = text.char_indices();
    loop {
        let (pos, c) = match steps.next() {
            Some((i, ch)) => (i, Some(ch)),
            None => (len, None),
        };
        let next_pos = pos + c.map_or(0, char::len_utf8);
        visited.fill(false);
        nlist.clear();
        for (pc, slots) in clist.drain(..) {
            match &program.insts[pc] {
                Inst::Char(x) => {
                    if c == Some(*x) {
                        add_thread(
                            program,
                            &mut nlist,
                            &mut visited,
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
                            pc + 1,
                            slots,
                            next_pos,
                            len,
                        );
                    }
                }
                Inst::Class(class) => {
                    if c.is_some_and(|ch| class_matches(class, ch)) {
                        add_thread(
                            program,
                            &mut nlist,
                            &mut visited,
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
fn add_thread(
    program: &Program,
    list: &mut Vec<(usize, Slots)>,
    visited: &mut [bool],
    pc: usize,
    slots: Slots,
    pos: usize,
    len: usize,
) {
    let mut stack = vec![(pc, slots)];
    while let Some((pc, slots)) = stack.pop() {
        if visited[pc] {
            continue;
        }
        visited[pc] = true;
        match &program.insts[pc] {
            Inst::Jump(target) => stack.push((*target, slots)),
            Inst::Split { first, second } => {
                // `first` is explored (and claims pcs) before `second`.
                stack.push((*second, slots.clone()));
                stack.push((*first, slots));
            }
            Inst::Save(slot) => {
                let mut slots = slots;
                slots[*slot] = Some(pos);
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

fn class_matches(class: &Class, c: char) -> bool {
    let hit = class.ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi)
        || class.posix.iter().any(|&p| posix_matches(p, c));
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
