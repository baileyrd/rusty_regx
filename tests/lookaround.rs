//! Tests for the Perl-style lookaround extension (`(?=` `(?!` `(?<=`
//! `(?<!`) and the non-capturing group `(?:`. This is a `rusty_regx`
//! extension beyond POSIX ERE + the GNU/glibc escape set — see
//! `docs/LOOKAROUND.md` for the design (in particular, why lookbehind is
//! fixed-length only).

use rusty_regx::{ErrorKind, Regex};

fn find<'t>(pattern: &str, text: &'t str) -> Option<&'t str> {
    Regex::new(pattern)
        .expect(pattern)
        .captures(text)
        .and_then(|caps| caps.get(0))
}

fn err(pattern: &str) -> ErrorKind {
    Regex::new(pattern).unwrap_err().kind()
}

#[test]
fn lookahead_basics() {
    // Positive: matches iff the assertion holds, but doesn't consume it.
    assert_eq!(find("foo(?=bar)", "foobar"), Some("foo"));
    assert_eq!(find("foo(?=bar)", "foobaz"), None);
    assert_eq!(find("foo(?=bar)", "foo"), None);
    // Negative: the mirror image.
    assert_eq!(find("foo(?!bar)", "foobaz"), Some("foo"));
    assert_eq!(find("foo(?!bar)", "foobar"), None);
    assert_eq!(find("foo(?!bar)", "foo"), Some("foo")); // nothing follows: "not bar" holds
                                                        // A lookahead alone is a zero-width match at any position it holds.
    assert_eq!(find("(?=bar)", "xbar"), Some(""));
    assert_eq!(
        Regex::new("(?=bar)").unwrap().find("xbar").unwrap().start(),
        1
    );
}

#[test]
fn lookbehind_basics() {
    assert_eq!(find("(?<=foo)bar", "foobar"), Some("bar"));
    assert_eq!(find("(?<=foo)bar", "xxxbar"), None);
    assert_eq!(find("(?<=foo)bar", "bar"), None); // not enough text behind
    assert_eq!(find("(?<!foo)bar", "xxxbar"), Some("bar"));
    assert_eq!(find("(?<!foo)bar", "foobar"), None);
    assert_eq!(find("(?<!foo)bar", "bar"), Some("bar")); // no text behind: "not foo" holds
}

#[test]
fn lookaround_combinations() {
    // Digit/non-digit boundary — \d is a literal `d` in this engine (GNU
    // escape rules, not Perl classes), so [0-9]/[^0-9] are used instead.
    let re = Regex::new(r"(?<=[0-9])(?=[^0-9])").unwrap();
    assert_eq!(re.find("3a").unwrap().range(), 1..1);
    assert!(!re.is_match("33"));
    assert!(!re.is_match("ab"));
    // Nested lookaround.
    assert_eq!(find("a(?=b(?=c))", "abc"), Some("a"));
    assert_eq!(find("a(?=b(?=c))", "abd"), None);
    assert_eq!(find("a(?<=(?<=x)a)", "xa"), Some("a"));
    assert_eq!(find("a(?<=(?<=x)a)", "ya"), None);
}

#[test]
fn non_capturing_group() {
    assert_eq!(find("(?:ab)+c", "ababc"), Some("ababc"));
    // Group numbering skips non-capturing groups entirely.
    let caps = Regex::new("(?:a)(b)(?:c)(d)")
        .unwrap()
        .captures("abcd")
        .unwrap();
    assert_eq!(caps.get(1), Some("b"));
    assert_eq!(caps.get(2), Some("d"));
    assert_eq!(caps.len(), 3); // group 0 + 2 capturing groups
}

#[test]
fn groups_inside_lookaround_do_not_capture() {
    // A capturing-looking `(...)` inside a lookaround is just grouping —
    // it doesn't allocate a capture slot, and numbering for groups
    // *outside* the lookaround is unaffected.
    let re = Regex::new(r"(a)(?=(b)c)(d)?").unwrap();
    let caps = re.captures("abc").unwrap();
    assert_eq!(caps.len(), 3); // group 0, 1 ("a"), 2 (the real "(d)?")
    assert_eq!(caps.get(1), Some("a"));
    assert_eq!(caps.get(2), None); // (d)? didn't match here — "c" follows, not "d"
}

#[test]
fn quantified_lookaround_is_safe() {
    // Zero-width bodies under `*` rely on the same visited-set dedup
    // already used for `^*`/`$*` — must not loop forever or panic. Greedy
    // `*` prefers "take the iteration" over "skip it": at the position
    // right after "a" in "abc", `(?=b)` holds, so the highest-priority
    // thread takes that branch and (being zero-width) is still at the
    // same position afterward, where `c` doesn't match `b` — and the
    // lower-priority "skip the loop" thread that *would* reach `c` is
    // deduped away by the higher-priority thread already having claimed
    // that program point. So "abc" doesn't match here, only "ac" does
    // (where `(?=b)` fails, so the loop takes zero iterations and `c`
    // matches directly) — a real, if initially surprising, consequence of
    // non-backtracking automaton semantics, not a bug: pinned here so a
    // future change can't silently alter it (or worse, hang/crash).
    assert_eq!(find("a(?=b)*c", "ac"), Some("ac"));
    assert_eq!(find("a(?=b)*c", "abc"), None);
}

#[test]
fn invalid_group_syntax_is_rejected() {
    assert_eq!(err("(?"), ErrorKind::InvalidGroupSyntax);
    assert_eq!(err("(?x)"), ErrorKind::InvalidGroupSyntax);
    assert_eq!(err("(?<x)"), ErrorKind::InvalidGroupSyntax);
    assert_eq!(err("(?<"), ErrorKind::InvalidGroupSyntax);
}

#[test]
fn variable_length_lookbehind_is_rejected() {
    for pattern in ["(?<=a+)b", "(?<=a*)b", "(?<=a{2,})b", "(?<=ab|c)d"] {
        assert_eq!(
            err(pattern),
            ErrorKind::VariableLengthLookbehind,
            "{pattern}"
        );
    }
    // Same-length alternation branches ARE fixed-length.
    assert!(Regex::new("(?<=ab|xy)c").is_ok());
    assert_eq!(find("(?<=ab|xy)c", "abc"), Some("c"));
    assert_eq!(find("(?<=ab|xy)c", "xyc"), Some("c"));
    assert_eq!(find("(?<=ab|xy)c", "zzc"), None);
    // Exact-count intervals are fixed-length too.
    assert!(Regex::new("(?<=a{3})b").is_ok());
    assert_eq!(find("(?<=a{3})b", "aaab"), Some("b"));
    assert_eq!(find("(?<=a{3})b", "aab"), None);
}

#[test]
fn error_positions_are_reported() {
    assert_eq!(Regex::new("(?<=a+)b").unwrap_err().position(), Some(0));
    assert_eq!(Regex::new("x(?<=a+)b").unwrap_err().position(), Some(1));
    assert_eq!(Regex::new("(?huh)").unwrap_err().position(), Some(0));
}

#[test]
fn lookaround_composes_with_other_modes() {
    // POSIX leftmost-longest mode.
    assert_eq!(
        Regex::new_posix("foo(?=bar)")
            .unwrap()
            .captures("foobar")
            .and_then(|c| c.get(0)),
        Some("foo")
    );
    // Case-insensitive mode: the lookaround body folds too.
    let re = Regex::new_ci("foo(?=BAR)").unwrap();
    assert_eq!(re.find("foobar").map(|m| m.as_str()), Some("foo"));
    assert!(!Regex::new_ci("foo(?=bar)").unwrap().is_match("fooBAX"));
}

#[test]
fn find_iter_with_lookaround() {
    // Every position where a digit is followed by a letter.
    let re = Regex::new(r"[0-9](?=[a-z])").unwrap();
    let matches: Vec<&str> = re.find_iter("1a2b3").map(|m| m.as_str()).collect();
    assert_eq!(matches, vec!["1", "2"]);
}

#[test]
fn nesting_depth_cap_applies_to_lookaround() {
    // Lookaround nesting has its own, much stricter cap than plain groups
    // (16, not 250): each level compiles via a full recursive sub-Program
    // compile, which costs far more stack per level than a group's single
    // `emit` step — 250 nested lookarounds overflows the stack even with
    // several MB to work with (this was caught by this very test during
    // development; the previous, wrong assumption was that the general
    // 250-deep structural cap covered it, matching ordinary groups).
    let deep = "(?=".repeat(16) + "a" + &")".repeat(16);
    assert!(Regex::new(&deep).is_ok());
    let too_deep = "(?=".repeat(17) + "a" + &")".repeat(17);
    assert_eq!(err(&too_deep), ErrorKind::NestingTooDeep);
    // The pathological case that used to crash the process.
    let pathological = "(?=".repeat(10_000) + "a" + &")".repeat(10_000);
    assert_eq!(err(&pathological), ErrorKind::NestingTooDeep);
}

#[test]
fn debug_dump_shows_lookaround_instructions() {
    let dump = Regex::new("foo(?=bar)").unwrap().debug_dump();
    assert!(dump.contains("Lookahead"), "{dump}");
    let dump = Regex::new("(?<=foo)bar").unwrap().debug_dump();
    assert!(dump.contains("Lookbehind"), "{dump}");
}

/// A lookaround sub-program must check "does the body match starting
/// *exactly* at this position", never "does it occur somewhere ahead" —
/// this was a real bug during development. The sub-program's own literal
/// substring fast path (`Program::literal`) only understands "anchored at
/// true position 0" or "unanchored search from `from` onward" (see
/// `vm::literal_span`); neither matches lookaround's need, so
/// `compile_impl` disables it outright when `force_anchored` is set — for
/// `(?=bar)` against `"xbar"`, the disabled-fast-path version correctly
/// fails to hold at position 0 (an *enabled* fast path would have found
/// "bar" at position 1 and wrongly reported holding at 0).
#[test]
fn lookahead_checks_the_exact_position_not_an_unanchored_search() {
    let re = Regex::new("(?=bar)").unwrap();
    let m = re.find("xbar").unwrap();
    assert_eq!(m.start(), 1); // holds at 1 ("bar" follows), not 0 ("xbar" follows)
    assert_eq!(m.end(), 1); // zero-width
                            // Same check via is_match/captures paths and the POSIX/icase modes,
                            // since each has its own VM entry point that could regress
                            // independently.
    assert!(!re.is_match("xxx"));
    assert!(Regex::new_posix("(?=bar)").unwrap().find("xbar").is_some());
    assert!(Regex::new_ci("(?=BAR)").unwrap().find("xbar").is_some());
}

/// A lookaround sub-program must not pay for the outer VM's suffix
/// quick-reject: that optimization scans from the check position to the
/// *end of the whole haystack* looking for a mandatory literal tail — a
/// good trade once per top-level search, but ruinous for a sub-program
/// invoked at every candidate position during an outer scan (each of
/// those checks would re-scan the remaining haystack for a suffix that
/// may never occur, turning an O(n) scan into O(n^2)). This was a real
/// bug during development: `a(?=bcd)` against a haystack with no "bcd"
/// took roughly 1 second per call before the fix. Pinned with a small
/// iteration count against a wall-clock ceiling generous enough not to
/// flake on a loaded CI runner, but tight enough to catch a real
/// regression back to quadratic behavior.
#[test]
fn lookaround_sub_program_skips_the_outer_suffix_quick_reject() {
    let text: String = "abc def ghi jkl mno pqr stu vwx ".repeat(3_000);
    let re = Regex::new("a(?=bcd)").unwrap(); // "bcd" never occurs in `text`
    let start = std::time::Instant::now();
    for _ in 0..50 {
        assert!(!re.is_match(&text));
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "50 calls took {elapsed:?} — the O(n^2) suffix-scan bug is back"
    );
}
