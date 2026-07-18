//! The abstract syntax tree produced by the ERE parser.

/// A parsed pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ast {
    /// The empty pattern (matches the empty string).
    Empty,
    /// A literal character.
    Char(char),
    /// `.` — any character.
    AnyChar,
    /// A bracket expression `[...]` / `[^...]`.
    Class(Class),
    /// `^`.
    StartAnchor,
    /// `$`.
    EndAnchor,
    /// `` \` `` — GNU absolute start-of-buffer: unlike `^`, always means
    /// true position 0, immune to `REG_NEWLINE`.
    BufferStart,
    /// `\'` — GNU absolute end-of-buffer: unlike `$`, always means the true
    /// end of input, immune to `REG_NEWLINE`.
    BufferEnd,
    /// `\b` — GNU word boundary (exactly one side is a word char).
    WordBoundary,
    /// `\B` — GNU non-boundary (both sides word, or neither).
    NotWordBoundary,
    /// `\<` — GNU start-of-word (next is word, previous is not).
    WordStart,
    /// `\>` — GNU end-of-word (previous is word, next is not).
    WordEnd,
    /// A sequence of expressions matched one after another.
    Concat(Vec<Ast>),
    /// `a|b|...`.
    Alternation(Vec<Ast>),
    /// A capturing group `( )`, numbered from 1 by opening parenthesis.
    Group(u32, Box<Ast>),
    /// A quantified expression: `* + ?` or an interval `{m}` `{m,}` `{m,n}`.
    Repeat {
        ast: Box<Ast>,
        min: u32,
        /// `None` means unbounded (`*`, `+`, `{m,}`).
        max: Option<u32>,
        /// Base index of this repetition's hidden span-tag slot pair, used
        /// by POSIX-mode disambiguation. The parser leaves it 0; the
        /// compiler assigns real slots in a numbering pass.
        slot: usize,
    },
}

/// A bracket expression: a (possibly negated) set of codepoint ranges and
/// POSIX classes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Class {
    pub negated: bool,
    /// Inclusive codepoint ranges; a single char is a degenerate range.
    pub ranges: Vec<(char, char)>,
    pub posix: Vec<PosixClass>,
}

/// A named POSIX class inside a bracket expression, e.g. `[[:alpha:]]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PosixClass {
    Alnum,
    Alpha,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    Xdigit,
}
