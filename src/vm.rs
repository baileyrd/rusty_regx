//! The Pike VM (roadmap steps 2–3).
//!
//! Breadth-first NFA simulation with per-thread capture slots.
//!
//! **Non-negotiable: no backtracking.** Each input character is processed
//! once against a deduplicated thread list, so execution is
//! `O(len(text) * len(program))` regardless of the pattern — `(a+)+b` can
//! never hang the shell.

use crate::compile::Program;

/// Executes `program` against `text` as an unanchored, leftmost-first
/// search.
///
/// On a match, returns one `(start, end)` byte-offset pair per capture
/// group (group 0 first); groups that did not participate are `None`.
pub fn exec(_program: &Program, _text: &str) -> Option<Vec<Option<(usize, usize)>>> {
    todo!("roadmap steps 2-3: Pike VM with capture slots")
}
