//! Shared bracket-expression (`[...]`) parsing.
//!
//! Both the ERE parser and the glob translator (`docs/GLOB_DESIGN.md`)
//! accept the same bracket-expression grammar — POSIX classes, ranges,
//! literal `]` first, trailing `-`, and the degenerate collating-symbol /
//! equivalence-class forms — differing only in which character(s)
//! introduce negation (`^` for ERE; `^` or `!` for glob). Keeping the
//! grammar in one place means a bracket-parsing fix or corner case never
//! has to be made twice.

use crate::ast::{Class, PosixClass};
use crate::error::{Error, ErrorKind};

/// A minimal `&str` char cursor, iterating the pattern directly (no
/// up-front `Vec<char>` collection) — `byte_pos` drives slicing, `char_pos`
/// is carried alongside because error positions are documented as char
/// offsets.
pub(crate) struct Cursor<'p> {
    pattern: &'p str,
    pub(crate) byte_pos: usize,
    pub(crate) char_pos: usize,
}

impl<'p> Cursor<'p> {
    pub(crate) fn new(pattern: &'p str, byte_pos: usize, char_pos: usize) -> Cursor<'p> {
        Cursor {
            pattern,
            byte_pos,
            char_pos,
        }
    }

    fn rest(&self) -> &str {
        &self.pattern[self.byte_pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.rest().chars().nth(offset)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if let Some(c) = c {
            self.byte_pos += c.len_utf8();
            self.char_pos += 1;
        }
        c
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.bump();
            true
        } else {
            false
        }
    }
}

/// Parses a bracket expression's contents (the leading `[` has already been
/// consumed by the caller; `open` is that `[`'s char position, for the
/// `UnclosedBracket` error). `negation_chars` are the character(s) that, if
/// they appear immediately after `[`, negate the class — `&['^']` for ERE,
/// `&['^', '!']` for glob.
pub(crate) fn parse(
    cursor: &mut Cursor<'_>,
    open: usize,
    negation_chars: &[char],
) -> Result<Class, Error> {
    let negated = match cursor.peek() {
        Some(c) if negation_chars.contains(&c) => {
            cursor.bump();
            true
        }
        _ => false,
    };
    let mut class = Class {
        negated,
        ranges: Vec::new(),
        posix: Vec::new(),
    };
    let mut first = true;
    loop {
        match cursor.peek() {
            None => return Err(Error::new(ErrorKind::UnclosedBracket, Some(open))),
            Some(']') if !first => {
                cursor.bump();
                break;
            }
            _ => {}
        }
        first = false;
        bracket_item(cursor, &mut class)?;
    }
    Ok(class)
}

/// One bracket item: a POSIX class, a range `a-z`, or a literal char.
fn bracket_item(cursor: &mut Cursor<'_>, class: &mut Class) -> Result<(), Error> {
    let start = cursor.char_pos;
    let mut lo_is_equiv = false;
    let lo = if cursor.peek() == Some('[') {
        match cursor.peek_at(1) {
            Some(':') => {
                class.posix.push(posix_class(cursor)?);
                return Ok(());
            }
            // The degenerate (single-char) forms of collating symbols and
            // equivalence classes — what bash accepts in C/UTF-8 locales;
            // multi-char collating names stay errors.
            Some('.') => collating(cursor, start, '.')?,
            Some('=') => {
                lo_is_equiv = true;
                collating(cursor, start, '=')?
            }
            _ => cursor.bump().expect("checked non-empty"),
        }
    } else {
        cursor.bump().expect("checked non-empty")
    };
    // `a-z` is a range unless the `-` is last (`[a-]`, trailing `-` is
    // literal) or the expression is unclosed.
    if cursor.peek() == Some('-') && cursor.peek_at(1).is_some_and(|c| c != ']') {
        // glibc: an equivalence class can't be a range endpoint
        // (`[[=a=]-c]` is a compile error), but a collating symbol can.
        if lo_is_equiv {
            return Err(Error::new(ErrorKind::InvalidRange, Some(start)));
        }
        cursor.bump();
        let hi = if cursor.peek() == Some('[') && cursor.peek_at(1) == Some('.') {
            collating(cursor, start, '.')?
        } else {
            let hi = cursor.bump().expect("checked non-empty");
            // A class or equivalence can't be a range endpoint:
            // `[a-[:digit:]]`, `[a-[=c=]]`.
            if hi == '[' && matches!(cursor.peek(), Some(':') | Some('=')) {
                return Err(Error::new(ErrorKind::InvalidRange, Some(start)));
            }
            hi
        };
        if lo > hi {
            return Err(Error::new(ErrorKind::InvalidRange, Some(start)));
        }
        class.ranges.push((lo, hi));
    } else {
        class.ranges.push((lo, lo));
    }
    Ok(())
}

/// Parses the degenerate form of `[.c.]` (`delim == '.'`) or `[=c=]`
/// (`delim == '='`): exactly one char between the delimiters. In the
/// C/UTF-8 locales bash runs in, that char is the whole collating symbol /
/// equivalence class; multi-char collating names are errors in glibc too
/// ("no such collating element").
fn collating(cursor: &mut Cursor<'_>, at: usize, delim: char) -> Result<char, Error> {
    let err = || Error::new(ErrorKind::InvalidPosixClass, Some(at));
    cursor.bump(); // `[`
    cursor.bump(); // delim
    let c = cursor.bump().ok_or_else(err)?;
    if !(cursor.eat(delim) && cursor.eat(']')) {
        return Err(err());
    }
    Ok(c)
}

/// Parses `[:name:]`; the leading `[` has been peeked (not consumed).
fn posix_class(cursor: &mut Cursor<'_>) -> Result<PosixClass, Error> {
    let open = cursor.char_pos;
    let err = || Error::new(ErrorKind::InvalidPosixClass, Some(open));
    cursor.bump(); // `[`
    cursor.bump(); // `:`
    let start = cursor.byte_pos;
    while cursor.peek().is_some_and(|c| c.is_ascii_lowercase()) {
        cursor.bump();
    }
    let name = &cursor.pattern[start..cursor.byte_pos];
    if !(cursor.eat(':') && cursor.eat(']')) {
        return Err(err());
    }
    match name {
        "alnum" => Ok(PosixClass::Alnum),
        "alpha" => Ok(PosixClass::Alpha),
        "blank" => Ok(PosixClass::Blank),
        "cntrl" => Ok(PosixClass::Cntrl),
        "digit" => Ok(PosixClass::Digit),
        "graph" => Ok(PosixClass::Graph),
        "lower" => Ok(PosixClass::Lower),
        "print" => Ok(PosixClass::Print),
        "punct" => Ok(PosixClass::Punct),
        "space" => Ok(PosixClass::Space),
        "upper" => Ok(PosixClass::Upper),
        "xdigit" => Ok(PosixClass::Xdigit),
        _ => Err(err()),
    }
}
