//! The bytecode compiler (roadmap step 2).
//!
//! Lowers the AST to a flat instruction sequence executed by the Pike VM.
//! Intervals `{m,n}` are compiled by repetition, capped by
//! [`MAX_REPETITION_SIZE`] so a pattern cannot blow up the program size.

// TODO: remove once the compiler and VM (roadmap step 2) use these.
#![allow(dead_code)]

use crate::ast::{Ast, Class};
use crate::error::Error;

/// Cap on interval expansion, mirroring the `regex` crate's program-size
/// limit in spirit. Exceeding it yields [`Error::RepetitionTooLarge`].
pub const MAX_REPETITION_SIZE: u32 = 1000;

/// A single VM instruction.
#[derive(Debug, Clone)]
pub enum Inst {
    /// Match one literal character.
    Char(char),
    /// Match any single character.
    AnyChar,
    /// Match one character against a class.
    Class(Class),
    /// Assert start of input.
    StartAnchor,
    /// Assert end of input.
    EndAnchor,
    /// Try `first` then `second` (thread split; order encodes greediness).
    Split { first: usize, second: usize },
    /// Unconditional jump.
    Jump(usize),
    /// Record the current input position in capture slot `n`.
    Save(usize),
    /// Successful match.
    Match,
}

/// A compiled program.
#[derive(Debug)]
pub struct Program {
    pub insts: Vec<Inst>,
    /// Number of capture groups including group 0.
    pub group_count: usize,
}

/// Compiles an AST into a [`Program`].
///
/// The program begins with an implicit non-greedy "any char" loop so that
/// execution is an unanchored search, matching bash `=~` semantics.
pub fn compile(_ast: &Ast) -> Result<Program, Error> {
    todo!("roadmap step 2: compiler + Pike VM")
}
