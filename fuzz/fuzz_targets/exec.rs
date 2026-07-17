//! The full pipeline — parse, compile, execute, extract captures — must
//! never panic in any mode, and the modes must agree on cross-checkable
//! invariants:
//!
//! - group 0 always participates in a match, and every participating
//!   group's span lies within group 0's (extracting them must not panic,
//!   which also proves all recorded offsets are UTF-8 boundaries);
//! - is_match and find agree with captures;
//! - leftmost-first and POSIX mode agree on match/no-match and on the
//!   match's start (both are leftmost), and the POSIX match is never
//!   shorter (it is longest at that start by definition);
//! - find_iter yields sorted, non-overlapping matches, boundedly many,
//!   whose first item equals find.
//!
//! The first input byte selects the mode (case-insensitive / newline /
//! reserved bits), so all builder combinations are exercised.
//!
//! Input layout: `mode-byte pattern 0xFF text` (0xFF never occurs in
//! UTF-8).

#![no_main]

use libfuzzer_sys::fuzz_target;
use rusty_regx::Regex;

fuzz_target!(|data: &[u8]| {
    let Some((&mode, data)) = data.split_first() else {
        return;
    };
    let icase = mode & 1 != 0;
    let newline = mode & 2 != 0;
    let Some(sep) = data.iter().position(|&b| b == 0xFF) else {
        return;
    };
    let (Ok(pattern), Ok(text)) = (
        std::str::from_utf8(&data[..sep]),
        std::str::from_utf8(&data[sep + 1..]),
    ) else {
        return;
    };
    // Keep units fast: interval-heavy patterns can legally compile to
    // 65536-instruction programs; bound the work per input instead.
    if pattern.len() > 256 || text.len() > 256 {
        return;
    }

    let build = |posix: bool| {
        Regex::builder()
            .posix(posix)
            .case_insensitive(icase)
            .newline(newline)
            .build(pattern)
            .ok()
    };
    let first = build(false).and_then(|re| span0(&re, text));
    let posix = build(true).and_then(|re| span0(&re, text));

    match (first, posix) {
        (None, None) => {}
        (Some((fs, fe)), Some((ps, pe))) => {
            assert_eq!(fs, ps, "modes disagree on match start");
            assert!(pe >= fe, "POSIX match shorter than leftmost-first");
        }
        _ => panic!("modes disagree on match/no-match"),
    }
});

/// Runs the match, exercises every group and the iteration invariants,
/// and returns group 0's byte span.
fn span0(re: &Regex, text: &str) -> Option<(usize, usize)> {
    let caps = re.captures(text);
    assert_eq!(
        re.is_match(text),
        caps.is_some(),
        "is_match disagrees with captures"
    );
    assert_eq!(
        re.find(text).map(|m| (m.start(), m.end())),
        caps.as_ref().and_then(|c| c.span(0)),
        "find disagrees with captures' group 0"
    );
    // Iteration: sorted, non-overlapping, bounded, first == find.
    let mut prev_end = 0;
    let mut count = 0usize;
    for (i, m) in re.find_iter(text).enumerate() {
        assert!(m.start() <= m.end(), "inverted span");
        assert!(m.start() >= prev_end || i == 0, "overlapping matches");
        if i == 0 {
            assert_eq!(
                Some((m.start(), m.end())),
                re.find(text).map(|m| (m.start(), m.end())),
                "find_iter's first item disagrees with find"
            );
        }
        prev_end = m.end();
        count += 1;
        assert!(count <= text.len() + 2, "find_iter failed to terminate");
    }
    let caps = caps?;
    let (start, end) = caps.span(0).expect("group 0 must participate");
    for i in 1..caps.len() {
        // get() slicing proves every span is on UTF-8 boundaries.
        let (g, span) = (caps.get(i), caps.span(i));
        assert_eq!(g.is_some(), span.is_some(), "get/span disagree on {i}");
        if let Some((s, e)) = span {
            assert!(s >= start && e <= end, "group {i} outside group 0");
        }
    }
    Some((start, end))
}
