use std::fmt;

/// A structured error produced while compiling a pattern.
///
/// rush displays these to shell users via `Display`, so messages are written
/// to stand alone without the pattern context.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
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
    /// An interval expansion exceeds the program-size cap.
    RepetitionTooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Error::UnbalancedParenthesis => "unbalanced parenthesis",
            Error::UnclosedBracket => "unclosed bracket expression",
            Error::InvalidPosixClass => "invalid POSIX character class",
            Error::InvalidRange => "invalid character range",
            Error::InvalidInterval => "invalid repetition interval",
            Error::DanglingQuantifier => "quantifier is not preceded by a repeatable expression",
            Error::TrailingBackslash => "pattern ends with a trailing backslash",
            Error::RepetitionTooLarge => "repetition interval is too large",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for Error {}
