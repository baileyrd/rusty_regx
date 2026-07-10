//! The POSIX-ERE parser (roadmap step 1).
//!
//! Grammar to cover, including the bracket-expression corner cases:
//! `[]a]` (literal `]` first), `[a-]` (trailing `-`), `[^]]`,
//! POSIX classes `[[:alpha:]]`…`[[:xdigit:]]`, and interval validation.

use crate::ast::Ast;
use crate::error::Error;

/// Parses an ERE pattern into an [`Ast`].
pub fn parse(_pattern: &str) -> Result<Ast, Error> {
    todo!("roadmap step 1: parser + AST + errors")
}
