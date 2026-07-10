//! End-to-end matching tests through the public API, including the
//! adversarial linear-time tests required by DESIGN.md from day one.

use rusty_regx::{Error, Regex};

/// Group 0 of the leftmost match, or None.
fn find<'t>(pattern: &str, text: &'t str) -> Option<&'t str> {
    Regex::new(pattern)
        .expect(pattern)
        .captures(text)
        .and_then(|caps| caps.get(0))
}

/// All groups of the leftmost match as owned strings (None = absent group).
fn groups(pattern: &str, text: &str) -> Option<Vec<Option<String>>> {
    Regex::new(pattern)
        .expect(pattern)
        .captures(text)
        .map(|caps| {
            (0..caps.len())
                .map(|i| caps.get(i).map(str::to_owned))
                .collect()
        })
}

#[test]
fn literal_search_is_unanchored() {
    assert_eq!(find("b", "abc"), Some("b"));
    assert_eq!(find("abc", "abc"), Some("abc"));
    assert_eq!(find("d", "abc"), None);
    assert_eq!(find("", "abc"), Some(""));
    assert_eq!(find("abc", ""), None);
}

#[test]
fn leftmost_match_wins() {
    let caps = Regex::new("a+").unwrap().captures("xxaayaaa").unwrap();
    assert_eq!(caps.get(0), Some("aa"));
    // Leftmost-first: the first alternative that matches at the leftmost
    // position wins, even if a later alternative would match more.
    assert_eq!(find("a|ab", "xab"), Some("a"));
    assert_eq!(find("ab|a", "xab"), Some("ab"));
}

#[test]
fn anchors() {
    assert_eq!(find("^a", "abc"), Some("a"));
    assert_eq!(find("^b", "abc"), None);
    assert_eq!(find("c$", "abc"), Some("c"));
    assert_eq!(find("b$", "abc"), None);
    assert_eq!(find("^abc$", "abc"), Some("abc"));
    assert_eq!(find("^$", ""), Some(""));
    assert_eq!(find("^$", "x"), None);
}

#[test]
fn quantifiers_are_greedy() {
    assert_eq!(find("a*", "aaa"), Some("aaa"));
    assert_eq!(find("a*", "bbb"), Some(""));
    assert_eq!(find("a+", "aaa"), Some("aaa"));
    assert_eq!(find("a+", "bbb"), None);
    assert_eq!(find("a?b", "ab"), Some("ab"));
    assert_eq!(find("a?b", "b"), Some("b"));
    assert_eq!(
        groups("(a*)(a*)", "aaa"),
        Some(vec![
            Some("aaa".into()),
            Some("aaa".into()),
            Some("".into())
        ])
    );
}

#[test]
fn intervals() {
    assert_eq!(find("a{3}", "aaaa"), Some("aaa"));
    assert_eq!(find("a{3}", "aa"), None);
    assert_eq!(find("a{2,3}", "aaaa"), Some("aaa"));
    assert_eq!(find("a{2,}", "aaaaa"), Some("aaaaa"));
    assert_eq!(find("(ab){2}", "ababab"), Some("abab"));
    assert_eq!(find("a{0,2}", "aaa"), Some("aa"));
}

#[test]
fn alternation_and_groups() {
    assert_eq!(find("cat|dog", "hotdog"), Some("dog"));
    assert_eq!(find("(a|b)+", "abba"), Some("abba"));
    assert_eq!(
        groups("(a)(b)(c)", "abc"),
        Some(vec![
            Some("abc".into()),
            Some("a".into()),
            Some("b".into()),
            Some("c".into())
        ])
    );
    // A repeated group reports its final iteration.
    assert_eq!(
        groups("(a|b)*", "ab"),
        Some(vec![Some("ab".into()), Some("b".into())])
    );
}

#[test]
fn unmatched_groups_are_absent() {
    assert_eq!(groups("(a)?b", "b"), Some(vec![Some("b".into()), None]));
    assert_eq!(
        groups("(a)|(b)", "b"),
        Some(vec![Some("b".into()), None, Some("b".into())])
    );
    // But a group that matched the empty string is present and empty.
    assert_eq!(
        groups("(a*)b", "b"),
        Some(vec![Some("b".into()), Some("".into())])
    );
}

#[test]
fn classes() {
    assert_eq!(find("[abc]+", "xcabz"), Some("cab"));
    assert_eq!(find("[^abc]+", "abxyc"), Some("xy"));
    assert_eq!(find("[a-fA-F]+", "zzBeefz"), Some("Beef"));
    assert_eq!(find("[[:digit:]]+", "abc123def"), Some("123"));
    assert_eq!(find("[^[:space:]]+", "  hi  "), Some("hi"));
    assert_eq!(find("[[:xdigit:]]+", "zzcafezz"), Some("cafe"));
    // Corner cases: literal `]` first, trailing literal `-`.
    assert_eq!(find("[]a]+", "x]a]x"), Some("]a]"));
    assert_eq!(find("[a-]+", "x-a-x"), Some("-a-"));
    assert_eq!(find("[^]]+", "ab]cd"), Some("ab"));
}

#[test]
fn dot_and_utf8() {
    assert_eq!(find("a.c", "abc"), Some("abc"));
    assert_eq!(find("a.c", "ac"), None);
    // `.` and classes operate on chars, not bytes.
    assert_eq!(find("a.c", "aéc"), Some("aéc"));
    assert_eq!(find("[é-ü]", "xñy"), Some("ñ"));
    assert_eq!(find("é+", "ééé"), Some("ééé"));
}

#[test]
fn rush_shaped_patterns() {
    // The kinds of patterns rush's C56 conditional exercises.
    assert_eq!(
        groups("^([[:alpha:]]+)-([0-9]{2,4})$", "release-2026"),
        Some(vec![
            Some("release-2026".into()),
            Some("release".into()),
            Some("2026".into())
        ])
    );
    assert_eq!(
        groups("^/([^/]+)/([^/]+)$", "/usr/bin"),
        Some(vec![
            Some("/usr/bin".into()),
            Some("usr".into()),
            Some("bin".into())
        ])
    );
    assert_eq!(find("^([0-9]+)\\.([0-9]+)$", "3.14"), Some("3.14"));
}

#[test]
fn repetition_size_limits() {
    assert_eq!(
        Regex::new("a{1001}").unwrap_err(),
        Error::RepetitionTooLarge
    );
    // Within the per-interval cap but past the program-size cap.
    assert_eq!(
        Regex::new("(a{1000}){1000}").unwrap_err(),
        Error::RepetitionTooLarge
    );
    assert!(Regex::new("a{1000}").is_ok());
}

/// The non-negotiable property: patterns that make backtracking engines
/// exponential must finish instantly. The assertions are generous (CI
/// machines vary) — a backtracker would need longer than the age of the
/// universe for 2^60 steps, and CI's job timeout is the final backstop.
#[test]
fn adversarial_patterns_are_linear_time() {
    let start = std::time::Instant::now();

    let a60 = "a".repeat(60);
    assert_eq!(find("(a+)+b", &a60), None);
    assert_eq!(find("(a*)*b", &a60), None);
    assert_eq!(find("(a|aa)*b", &a60), None);
    assert_eq!(find("(a|a?)+b", &a60), None);
    assert_eq!(find("(a{2,10}){2,10}b", &a60), None);

    // And the matching variants must still match.
    let mut ab = "a".repeat(500);
    ab.push('b');
    assert_eq!(find("(a+)+b", &ab).map(str::len), Some(501));
    assert_eq!(find("(a|aa)*b", &ab).map(str::len), Some(501));

    // Long input through a non-trivial program.
    let long = "xy".repeat(50_000);
    assert_eq!(find("(x|y)*z", &long), None);

    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "adversarial patterns took {:?} — matching is not linear-time",
        start.elapsed()
    );
}
