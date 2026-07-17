//! Boolean match/no-match agreement with the `regex` crate on the syntax
//! subset where the two engines agree *by design*.
//!
//! The structured harness in `tests/differential.rs` documents the
//! intentional divergences; this target filters them out of the raw fuzz
//! input rather than generating around them:
//!
//! - `\` anywhere (escape semantics differ in and out of brackets),
//! - `^`/`$` anywhere (POSIX allows anchors mid-pattern; the crate rejects
//!   some of those, and inside brackets they are literals we can't cheaply
//!   distinguish),
//! - `{` anywhere (the crate elides capture groups under `{0,0}`),
//! - `?` directly after a quantifier (POSIX reads `k+?` as an optional
//!   stacked quantifier — matches empty; the crate reads it as a *lazy*
//!   plus — requires one `k`; found by this very target on first run),
//! - non-ASCII pattern or text (POSIX-class Unicode fallbacks differ),
//! - newlines in the text (our `.` matches `\n`; the crate's doesn't).
//!
//! Capture comparison is deliberately out of scope here: the crate's
//! prefilter can report a later-than-leftmost match (see
//! `crate_skipped_earlier_match` in the harness), which does not affect
//! match/no-match.
//!
//! Input layout: `pattern 0xFF text` (0xFF never occurs in UTF-8).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some(sep) = data.iter().position(|&b| b == 0xFF) else {
        return;
    };
    let (Ok(pattern), Ok(text)) = (
        std::str::from_utf8(&data[..sep]),
        std::str::from_utf8(&data[sep + 1..]),
    ) else {
        return;
    };
    if pattern.len() > 128 || text.len() > 256 {
        return;
    }
    if !pattern.is_ascii() || !text.is_ascii() {
        return;
    }
    if pattern.contains(['\\', '^', '$', '{']) || text.contains('\n') {
        return;
    }
    // Over-filtering (e.g. `[*?]` in a bracket) is fine for a fuzz target.
    if pattern.contains("*?") || pattern.contains("+?") || pattern.contains("??") {
        return;
    }

    // Grammar acceptance differs on corner cases (e.g. we accept `[]a]`,
    // the crate rejects it), so only inputs both engines compile count.
    let (Ok(ours), Ok(theirs)) = (rusty_regx::Regex::new(pattern), regex::Regex::new(pattern))
    else {
        return;
    };
    assert_eq!(
        ours.captures(text).is_some(),
        theirs.is_match(text),
        "match/no-match divergence on pattern {pattern:?}, text {text:?}"
    );
});
