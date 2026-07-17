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
#[derive(Debug, Clone)]
pub struct Program {
    pub insts: Vec<Inst>,
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
/// The program begins with an implicit non-greedy "any char" loop so that
/// execution is an unanchored search, matching bash `=~` semantics.
pub fn compile(ast: &Ast, icase: bool) -> Result<Program, Error> {
    let group_count = max_group(ast) as usize + 1;
    let mut ast = ast.clone();
    let mut tag_order = vec![0];
    let mut next_slot = 2 * group_count;
    number(&mut ast, &mut next_slot, &mut tag_order);

    let mut c = Compiler {
        insts: Vec::new(),
        icase,
    };
    // Unanchored-search prefix, non-greedy: prefer starting the match at the
    // current position (leftmost) over consuming another character.
    c.push(Inst::Split {
        first: 3,
        second: 1,
    })?;
    c.push(Inst::AnyChar)?;
    c.push(Inst::Jump(0))?;
    c.push(Inst::Save(0))?;
    c.emit(&ast)?;
    c.push(Inst::Save(1))?;
    c.push(Inst::Match)?;
    Ok(Program {
        insts: c.insts,
        group_count,
        slot_count: next_slot,
        tag_order,
        icase,
    })
}

/// Assigns span-tag slots to repetitions and records the disambiguation
/// order: syntactic pre-order, so an outer construct's pair is compared
/// before anything inside it, and siblings compare left-to-right. This is
/// what makes POSIX mode prefer a longer repetition span over a "better"
/// last iteration inside a shorter one.
fn number(ast: &mut Ast, next_slot: &mut usize, tag_order: &mut Vec<usize>) {
    match ast {
        Ast::Empty
        | Ast::Char(_)
        | Ast::AnyChar
        | Ast::Class(_)
        | Ast::StartAnchor
        | Ast::EndAnchor => {}
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
    icase: bool,
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
                let class = self.fold_class(class)?;
                self.push(Inst::Class(class))?;
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
            Ast::Repeat {
                ast,
                min,
                max,
                slot,
            } => self.repeat(ast, *min, *max, *slot)?,
        }
        Ok(())
    }

    /// In `icase` mode, folds a class the way glibc's `REG_ICASE` does:
    /// range endpoints (including single characters, which are degenerate
    /// ranges) fold to uppercase, and a range that is reversed *after*
    /// folding is an error (`[Z-a]` is valid case-sensitively but folds to
    /// `[Z-A]`; bash rejects it under `nocasematch`). `[[:upper:]]` and
    /// `[[:lower:]]` both become `[[:alpha:]]` — glibc's documented
    /// `REG_ICASE` rule, verified against bash 5.2.
    fn fold_class(&self, class: &Class) -> Result<Class, Error> {
        if !self.icase {
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
