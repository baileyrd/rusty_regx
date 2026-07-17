use std::fmt;

/// A structured error produced while compiling a pattern.
///
/// Carries what went wrong ([`Error::kind`]) and, for parse errors, where
/// in the pattern ([`Error::position`]). rush displays these to shell
/// users via `Display`, so messages are written to stand alone without
/// the pattern context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    pos: Option<usize>,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, pos: Option<usize>) -> Error {
        Error { kind, pos }
    }

    /// What went wrong.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Where in the pattern the error was found, as a 0-based `char`
    /// offset — the position of the offending construct's start (e.g. the
    /// unclosed `[`, the `{` of a malformed interval).
    ///
    /// `None` for errors detected after parsing, where no single pattern
    /// position applies (e.g. a repetition that exceeds the program-size
    /// cap, or a range that only becomes invalid after case folding).
    pub fn position(&self) -> Option<usize> {
        self.pos
    }
}

/// The kinds of pattern error, without location information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// `(` without a matching `)`, or a stray `)`.
    UnbalancedParenthesis,
    /// `[` without a matching `]`.
    UnclosedBracket,
    /// A malformed POSIX class such as `[[:foo:]]`, or `[[:alpha` cut short.
    InvalidPosixClass,
    /// A reversed range such as `[z-a]`.
    InvalidRange,
    /// A malformed interval such as `{,}`, `{a}`, or `{3,2}`.
    InvalidInterval,
    /// A quantifier (`* + ? {..}`) with nothing to repeat.
    DanglingQuantifier,
    /// The pattern ends in a lone `\`.
    TrailingBackslash,
    /// Groups are nested deeper than the parser's depth cap.
    NestingTooDeep,
    /// An interval expansion exceeds the program-size cap.
    RepetitionTooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self.kind {
            ErrorKind::UnbalancedParenthesis => "unbalanced parenthesis",
            ErrorKind::UnclosedBracket => "unclosed bracket expression",
            ErrorKind::InvalidPosixClass => "invalid POSIX character class",
            ErrorKind::InvalidRange => "invalid character range",
            ErrorKind::InvalidInterval => "invalid repetition interval",
            ErrorKind::DanglingQuantifier => {
                "quantifier is not preceded by a repeatable expression"
            }
            ErrorKind::TrailingBackslash => "pattern ends with a trailing backslash",
            ErrorKind::NestingTooDeep => "groups are nested too deeply",
            ErrorKind::RepetitionTooLarge => "repetition interval is too large",
        };
        match self.pos {
            Some(pos) => write!(f, "{msg} at position {pos}"),
            None => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}
