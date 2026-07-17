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
fn is_match_agrees_with_captures_in_every_mode() {
    let cases = [
        ("a|ab", "xab"),
        ("^abc$", "ABC"),
        ("(a+)+b", "aaaa"),
        ("[[:digit:]]+", "abc123"),
        ("", ""),
        ("a", ""),
        ("^$", "x"),
        ("(a?)*", "b"),
    ];
    for (pattern, text) in cases {
        for re in [
            Regex::new(pattern).unwrap(),
            Regex::new_posix(pattern).unwrap(),
            Regex::new_ci(pattern).unwrap(),
            Regex::new_posix_ci(pattern).unwrap(),
        ] {
            assert_eq!(
                re.is_match(text),
                re.captures(text).is_some(),
                "is_match/captures disagree on pattern {pattern:?}, text {text:?}"
            );
        }
    }
}

#[test]
fn new_ci_is_leftmost_first_and_case_insensitive() {
    let re = Regex::new_ci("a|ab").unwrap();
    // Leftmost-first: the first alternative wins, unlike new_posix_ci.
    assert_eq!(re.captures("AB").unwrap().get(0), Some("A"));
    assert!(Regex::new_ci("^abc$").unwrap().is_match("ABC"));
    // Captures keep the original case, same as the POSIX ci mode.
    assert_eq!(
        groups_of(&Regex::new_ci("^(a)(b)").unwrap(), "ABC"),
        Some(vec![Some("AB".into()), Some("A".into()), Some("B".into())])
    );
    // Same folding rules as new_posix_ci.
    assert!(Regex::new_ci("[X-Z]").unwrap().is_match("y"));
    assert_eq!(Regex::new_ci("[Z-a]").unwrap_err(), Error::InvalidRange);
}

#[test]
fn regex_reports_its_pattern() {
    let re = Regex::new("a|b").unwrap();
    assert_eq!(re.as_str(), "a|b");
    assert_eq!(re.to_string(), "a|b");
    assert_eq!(format!("{re:?}"), r#"Regex("a|b")"#);
    // Clone produces an equivalent, independently usable regex.
    let clone = re.clone();
    assert_eq!(clone.as_str(), "a|b");
    assert!(clone.is_match("xby"));
}

/// All groups via an already-compiled regex.
fn groups_of(re: &Regex, text: &str) -> Option<Vec<Option<String>>> {
    re.captures(text).map(|caps| {
        (0..caps.len())
            .map(|i| caps.get(i).map(str::to_owned))
            .collect()
    })
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

/// Group 0 under POSIX (leftmost-longest) semantics.
fn find_posix<'t>(pattern: &str, text: &'t str) -> Option<&'t str> {
    Regex::new_posix(pattern)
        .expect(pattern)
        .captures(text)
        .and_then(|caps| caps.get(0))
}

fn groups_posix(pattern: &str, text: &str) -> Option<Vec<Option<String>>> {
    Regex::new_posix(pattern)
        .expect(pattern)
        .captures(text)
        .map(|caps| {
            (0..caps.len())
                .map(|i| caps.get(i).map(str::to_owned))
                .collect()
        })
}

#[test]
fn posix_mode_is_leftmost_longest() {
    // The flagship divergence this mode exists to close: real bash matches
    // the longest alternative, not the first.
    assert_eq!(find("a|ab", "xab"), Some("a"));
    assert_eq!(find_posix("a|ab", "xab"), Some("ab"));
    assert_eq!(find_posix("ab|a", "xab"), Some("ab"));

    // The classic POSIX composition test: overall-longest wins even when
    // that forces a shorter first alternative.
    assert_eq!(
        groups_posix("(a|ab)(c|bcd)", "abcd"),
        Some(vec![
            Some("abcd".into()),
            Some("a".into()),
            Some("bcd".into())
        ])
    );

    // Leftmost still beats longest: a later, longer match never wins.
    assert_eq!(find_posix("aa|b+", "xaabbb"), Some("aa"));
}

#[test]
fn posix_mode_agrees_where_semantics_coincide() {
    assert_eq!(find_posix("a+", "xxaayaaa"), Some("aa"));
    assert_eq!(find_posix("a*", "aaa"), Some("aaa"));
    assert_eq!(find_posix("^abc$", "abc"), Some("abc"));
    assert_eq!(find_posix("z", "abc"), None);
    assert_eq!(
        groups_posix("^([[:alpha:]]+)-([0-9]{2,4})$", "release-2026"),
        Some(vec![
            Some("release-2026".into()),
            Some("release".into()),
            Some("2026".into())
        ])
    );
    assert_eq!(
        groups_posix("(x)?(b)", "abc"),
        Some(vec![Some("b".into()), None, Some("b".into())])
    );
}

/// Group 0 under case-insensitive POSIX (`REG_ICASE`) semantics.
fn find_ci<'t>(pattern: &str, text: &'t str) -> Option<&'t str> {
    Regex::new_posix_ci(pattern)
        .expect(pattern)
        .captures(text)
        .and_then(|caps| caps.get(0))
}

fn groups_ci(pattern: &str, text: &str) -> Option<Vec<Option<String>>> {
    Regex::new_posix_ci(pattern)
        .expect(pattern)
        .captures(text)
        .map(|caps| {
            (0..caps.len())
                .map(|i| caps.get(i).map(str::to_owned))
                .collect()
        })
}

// Every assertion in the posix_ci tests below mirrors a probe run against
// bash 5.2 / glibc 2.39 with `shopt -s nocasematch` — the handoff's
// instruction was to capture bash's *actual* behavior, not the guesses.

#[test]
fn posix_ci_folds_ordinary_letters() {
    // The gap this mode exists to close: `shopt -s nocasematch; [[ ABC =~ ^abc$ ]]`.
    assert_eq!(find_ci("^abc$", "ABC"), Some("ABC"));
    assert_eq!(find_ci("^ABC$", "abc"), Some("abc"));
    assert_eq!(find_ci("B", "abc"), Some("b"));
    // Folding is opt-in: the case-sensitive constructors are unaffected.
    assert_eq!(find_posix("^abc$", "ABC"), None);
    assert_eq!(find("^abc$", "ABC"), None);
    // Unicode simple folding (matches bash in a UTF-8 locale).
    assert_eq!(find_ci("é", "É"), Some("É"));
    assert_eq!(find_ci("Σ", "σ"), Some("σ"));
    assert_eq!(find_ci("Σ", "ς"), Some("ς")); // final sigma uppercases to Σ
    assert_eq!(find_ci("i", "İ"), None); // İ's uppercase is itself, not I
}

#[test]
fn posix_ci_folds_range_endpoints() {
    // REG_ICASE folds range endpoints (to uppercase, like glibc): `[X-Z]`
    // also matches x–z, but `a` is still outside the folded range.
    assert_eq!(find_ci("[X-Z]bc", "xbc"), Some("xbc"));
    assert_eq!(find_ci("[X-Z]bc", "abc"), None);
    assert_eq!(find_ci("[x-z]bc", "Xbc"), Some("Xbc"));
    assert_eq!(find_ci("[a-f]bc", "Abc"), Some("Abc"));
    assert_eq!(find_ci("[A-F]bc", "abc"), Some("abc"));
    assert_eq!(find_ci("[a-f]", "G"), None);
    assert_eq!(find_ci("[A-F]", "g"), None);
    // Upper-folding, not lower-folding, is what bash/glibc do: `[A-_]`
    // stays A(0x41)–_(0x5F), so input `b` folds to `B` and matches.
    assert_eq!(find_ci("[A-_]", "b"), Some("b"));
    assert_eq!(find_ci("[a-{]", "B"), Some("B"));
}

#[test]
fn posix_ci_range_reversed_after_folding_is_an_error() {
    // `[Z-a]` is a valid range case-sensitively, but folds to `[Z-A]`;
    // bash rejects it under nocasematch (exit 2 from `=~`).
    assert!(Regex::new_posix("[Z-a]").is_ok());
    assert_eq!(
        Regex::new_posix_ci("[Z-a]").unwrap_err(),
        Error::InvalidRange
    );
}

#[test]
fn posix_ci_upper_and_lower_classes_become_alpha() {
    // glibc's REG_ICASE rule: [[:upper:]] and [[:lower:]] both behave as
    // [[:alpha:]]. (The handoff guessed named classes keep their literal
    // meaning; bash 5.2 says otherwise.)
    assert_eq!(find_ci("[[:lower:]]bc", "ABC"), Some("ABC"));
    assert_eq!(find_ci("[[:upper:]]bc", "abc"), Some("abc"));
    assert_eq!(find_ci("[^[:lower:]]", "A"), None);
    assert_eq!(find_ci("[^[:upper:]]", "a"), None);
    // Case-symmetric classes are unaffected.
    assert_eq!(find_ci("[[:digit:]]+", "abc123"), Some("123"));
    assert_eq!(find_ci("[[:xdigit:]]+", "zzCAFEzz"), Some("CAFE"));
}

#[test]
fn posix_ci_folds_before_negation() {
    assert_eq!(find_ci("[^a-z]", "A"), None);
    assert_eq!(find_ci("[^A-Z]", "a"), None);
    assert_eq!(find_ci("[^abc]", "A"), None);
    assert_eq!(find_ci("[^abc]", "d"), Some("d"));
    assert_eq!(find_ci("[^X-Z]", "B"), Some("B"));
    assert_eq!(find_ci("[^X-Z]", "y"), None);
}

#[test]
fn posix_ci_captures_report_original_case() {
    // $BASH_REMATCH must see the unfolded input — folding affects
    // comparison only, never the captured text.
    assert_eq!(
        groups_ci("^(a)(b)", "ABC"),
        Some(vec![Some("AB".into()), Some("A".into()), Some("B".into())])
    );
    assert_eq!(
        groups_ci("^([[:alpha:]]+)-([0-9]{2,4})$", "RELEASE-2026"),
        Some(vec![
            Some("RELEASE-2026".into()),
            Some("RELEASE".into()),
            Some("2026".into())
        ])
    );
}

#[test]
fn posix_ci_keeps_leftmost_longest_semantics() {
    assert_eq!(find_ci("a|ab", "xAB"), Some("AB"));
    assert_eq!(find_ci("AB|A", "xab"), Some("ab"));
    assert_eq!(
        groups_ci("(a|ab)(c|bcd)", "ABCD"),
        Some(vec![
            Some("ABCD".into()),
            Some("A".into()),
            Some("BCD".into())
        ])
    );
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

    // POSIX mode is polynomial, not exponential — same patterns must
    // finish instantly there too.
    assert_eq!(find_posix("(a+)+b", &a60), None);
    assert_eq!(find_posix("(a|aa)*b", &a60), None);
    assert_eq!(find_posix("(a|a?)+b", &a60), None);

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
