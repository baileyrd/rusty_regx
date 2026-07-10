//! The bytecode compiler (roadmap step 2).
//!
//! Lowers the AST to a flat instruction sequence executed by the Pike VM.
//! Intervals `{m,n}` are compiled by repetition, capped by
//! [`MAX_REPETITION_SIZE`] and an overall [`MAX_PROGRAM_SIZE`] so a pattern
//! cannot blow up the program size.

use crate::ast::{Ast, Class};
use crate::error::Error;

/// Cap on a single interval bound, mirroring the `regex` crate's limit in
/// spirit. Exceeding it yields [`Error::RepetitionTooLarge`].
pub const MAX_REPETITION_SIZE: u32 = 1000;

/// Cap on total compiled program size, so nested intervals like
/// `(a{1000}){1000}` cannot exhaust memory.
pub const MAX_PROGRAM_SIZE: usize = 1 << 16;

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
pub fn compile(ast: &Ast) -> Result<Program, Error> {
    let group_count = max_group(ast) as usize + 1;
    let mut c = Compiler { insts: Vec::new() };
    // Unanchored-search prefix, non-greedy: prefer starting the match at the
    // current position (leftmost) over consuming another character.
    c.push(Inst::Split {
        first: 3,
        second: 1,
    })?;
    c.push(Inst::AnyChar)?;
    c.push(Inst::Jump(0))?;
    c.push(Inst::Save(0))?;
    c.emit(ast)?;
    c.push(Inst::Save(1))?;
    c.push(Inst::Match)?;
    Ok(Program {
        insts: c.insts,
        group_count,
    })
}

/// The highest group index appearing in the AST (0 if none).
fn max_group(ast: &Ast) -> u32 {
    match ast {
        Ast::Empty
        | Ast::Char(_)
        | Ast::AnyChar
        | Ast::Class(_)
        | Ast::StartAnchor
        | Ast::EndAnchor => 0,
        Ast::Concat(items) | Ast::Alternation(items) => {
            items.iter().map(max_group).max().unwrap_or(0)
        }
        Ast::Group(index, inner) => (*index).max(max_group(inner)),
        Ast::Repeat { ast, .. } => max_group(ast),
    }
}

struct Compiler {
    insts: Vec<Inst>,
}

impl Compiler {
    /// Appends an instruction, returning its index; errors past the size cap.
    fn push(&mut self, inst: Inst) -> Result<usize, Error> {
        if self.insts.len() >= MAX_PROGRAM_SIZE {
            return Err(Error::RepetitionTooLarge);
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
                self.push(Inst::Char(*c))?;
            }
            Ast::AnyChar => {
                self.push(Inst::AnyChar)?;
            }
            Ast::Class(class) => {
                self.push(Inst::Class(class.clone()))?;
            }
            Ast::StartAnchor => {
                self.push(Inst::StartAnchor)?;
            }
            Ast::EndAnchor => {
                self.push(Inst::EndAnchor)?;
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
            Ast::Repeat { ast, min, max } => self.repeat(ast, *min, *max)?,
        }
        Ok(())
    }

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
    fn repeat(&mut self, ast: &Ast, min: u32, max: Option<u32>) -> Result<(), Error> {
        if min > MAX_REPETITION_SIZE || max.is_some_and(|m| m > MAX_REPETITION_SIZE) {
            return Err(Error::RepetitionTooLarge);
        }
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
        Ok(())
    }
}
