//! End-to-end matching tests through the public API, including the
//! adversarial linear-time tests required by DESIGN.md from day one.

use rusty_regx::{ErrorKind, Regex};

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

/// Pins the *intended* divergence from the `C` locale (see README):
/// POSIX classes use Unicode `char` fallbacks, matching glibc in a UTF-8
/// locale — not `LC_ALL=C`, where classes are ASCII-only. If one of these
/// assertions starts failing, the engine's locale stance changed.
#[test]
fn posix_classes_are_unicode_not_c_locale() {
    // Matches here and in UTF-8 bash; fails in bash under LC_ALL=C.
    assert_eq!(find("[[:alpha:]]+", "héllo"), Some("héllo"));
    assert_eq!(find("[[:alnum:]]", "×é×"), Some("é"));
    assert_eq!(find("[[:space:]]", "a\u{a0}b"), Some("\u{a0}"));
    assert_eq!(find("[[:lower:]]+", "σφ"), Some("σφ"));
    assert_eq!(find("[[:upper:]]+", "ΣΦ"), Some("ΣΦ"));
    // ASCII-first classes stay ASCII-only in every locale: digit, xdigit,
    // blank, graph, print, punct never took the Unicode fallback.
    assert_eq!(find("[[:digit:]]", "٣"), None); // Arabic-Indic three
    assert_eq!(find("[[:xdigit:]]", "ａ"), None); // fullwidth a
    assert_eq!(find("[[:punct:]]", "«"), None);
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
    assert_eq!(
        Regex::new_ci("[Z-a]").unwrap_err().kind(),
        ErrorKind::InvalidRange
    );
}

/// The VM uses `Rc` internally (copy-on-write capture slots); this fails
/// to compile if that ever leaks into the public types.
#[test]
fn regex_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Regex>();
    assert_send_sync::<rusty_regx::Captures<'_>>();
    assert_send_sync::<rusty_regx::Error>();
}

#[test]
fn spans_iter_and_index() {
    let re = Regex::new("(a)(x)?(b)").unwrap();
    let caps = re.captures("zzab").unwrap();
    assert_eq!(caps.span(0), Some((2, 4)));
    assert_eq!(caps.span(1), Some((2, 3)));
    assert_eq!(caps.span(2), None); // did not participate
    assert_eq!(caps.span(3), Some((3, 4)));
    assert_eq!(caps.span(4), None); // out of range
                                    // Spans fall on char boundaries in multibyte text.
    let caps = Regex::new("(é)").unwrap().captures("xéy").unwrap();
    assert_eq!(caps.span(1), Some((1, 3)));
    // iter() yields every group in order, absent ones as None.
    let re = Regex::new("(a)(x)?(b)").unwrap();
    let caps = re.captures("zzab").unwrap();
    let all: Vec<Option<&str>> = caps.iter().collect();
    assert_eq!(all, vec![Some("ab"), Some("a"), None, Some("b")]);
    // Indexing panics only on absent groups.
    assert_eq!(&caps[0], "ab");
    assert_eq!(&caps[3], "b");
    assert!(std::panic::catch_unwind(|| {
        let _ = &caps[2];
    })
    .is_err());
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

/// The anchored fast path and literal-prefix fast-forward are pure
/// optimizations — these pin the cases where a buggy version would
/// change results (threads that loop back to the pattern head, captures
/// recorded across skipped regions, mixed anchored/unanchored branches).
#[test]
fn scan_optimizations_preserve_semantics() {
    // Anchored: no unanchored prefix is compiled.
    assert_eq!(find("^a", "xxa"), None);
    assert_eq!(find("^a|^b", "zab"), None);
    assert_eq!(find("(^a)(b)", "ab"), Some("ab"));
    assert_eq!(find("^(a|b)+c$", "bac"), Some("bac"));
    // Mixed branches are NOT anchored: the unanchored one must still search.
    assert_eq!(find("^a|b", "zzb"), Some("b"));
    // (^a)?b matches b anywhere — a min-0 head never anchors.
    assert_eq!(find("(^a)?b", "zzb"), Some("b"));

    // Literal-prefix fast-forward: matches after long skippable gaps,
    // with captures reporting the right (post-skip) offsets.
    let text = format!("{}ab-12", ".".repeat(1000).replace('.', "x"));
    assert_eq!(
        groups("(a)(b)-([0-9]+)", &text),
        Some(vec![
            Some("ab-12".into()),
            Some("a".into()),
            Some("b".into()),
            Some("12".into())
        ])
    );
    // Loop-back-to-head shapes: a thread mid-iteration must never be
    // mistaken for a fresh restart state.
    assert_eq!(find("(ab)+c", "ab abababc"), Some("abababc"));
    assert_eq!(find("a+b", "aaa aab"), Some("aab"));
    assert_eq!(
        groups("(a+)(b)", "aa aaab"),
        Some(vec![
            Some("aaab".into()),
            Some("aaa".into()),
            Some("b".into())
        ])
    );
    // The required char never recurs: must report no match, quickly.
    assert_eq!(find("ab", "zzzzzz"), None);
    // POSIX mode takes the same paths.
    assert_eq!(find_posix("(ab)+c", "ab abababc"), Some("abababc"));
    assert_eq!(find_posix("^a|^b", "zab"), None);
    assert_eq!(
        groups_posix("(a+)(b)", "aa aaab"),
        Some(vec![
            Some("aaab".into()),
            Some("aaa".into()),
            Some("b".into())
        ])
    );
    // And is_match agrees everywhere.
    for (p, t) in [
        ("^a", "xxa"),
        ("(ab)+c", "ab abababc"),
        ("a+b", "aaa aab"),
        ("ab", "zzzzzz"),
        ("^a|b", "zzb"),
    ] {
        for re in [Regex::new(p).unwrap(), Regex::new_posix(p).unwrap()] {
            assert_eq!(re.is_match(t), re.captures(t).is_some(), "{p} on {t}");
        }
    }
}

#[test]
fn find_agrees_with_captures_span_in_every_mode() {
    let cases = [
        ("a|ab", "xab"),
        ("(a+)(b+)", "zzaabbz"),
        ("^([[:alpha:]]+)-([0-9]+)$", "release-2026"),
        ("(x)?(b)", "abc"),
        ("q", "zz"),
        ("bcd", "abcdbcd"),
    ];
    for (pattern, text) in cases {
        for re in [
            Regex::new(pattern).unwrap(),
            Regex::new_posix(pattern).unwrap(),
        ] {
            let found = re.find(text).map(|m| (m.start(), m.end()));
            let span0 = re.captures(text).and_then(|c| c.span(0));
            assert_eq!(found, span0, "find/captures disagree: {pattern} on {text}");
        }
    }
    // POSIX find is leftmost-longest, like captures.
    let m = Regex::new_posix("a|ab").unwrap().find("xab").unwrap();
    assert_eq!((m.start(), m.end(), m.as_str()), (1, 3, "ab"));
    assert_eq!(m.range(), 1..3);
    // Leftmost-first find takes the first alternative.
    assert_eq!(
        Regex::new("a|ab").unwrap().find("xab").unwrap().as_str(),
        "a"
    );
}

/// Iteration semantics: non-overlapping, leftmost, empty matches
/// advance one char. The regex crate is the oracle where the engines'
/// semantics coincide (leftmost-first, no by-design divergent syntax).
#[test]
fn find_iter_agrees_with_regex_crate() {
    let cases = [
        ("a+", "aa b aaa"),
        ("a*", "aab"),
        ("", "é a"),
        ("ab", "ababab"),
        ("[0-9]+", "1 22 333"),
        ("a", "zzz"),
        ("(a)(b)?", "ab a ab"),
    ];
    for (pattern, text) in cases {
        let ours: Vec<(usize, usize)> = Regex::new(pattern)
            .unwrap()
            .find_iter(text)
            .map(|m| (m.start(), m.end()))
            .collect();
        let theirs: Vec<(usize, usize)> = regex::Regex::new(pattern)
            .unwrap()
            .find_iter(text)
            .map(|m| (m.start(), m.end()))
            .collect();
        assert_eq!(
            ours, theirs,
            "find_iter divergence on {pattern:?} / {text:?}"
        );
    }
}

/// `Match` derives `PartialEq`/`Eq`/`Hash` (matching the `regex` crate's
/// ergonomics): callers can dedupe matches in a `HashSet` or compare spans
/// directly instead of manually comparing `.range()`.
#[test]
fn match_is_comparable_and_hashable() {
    use std::collections::HashSet;
    let re = Regex::new("a+").unwrap();
    let text = "aa b aaa";
    let m1 = re.find(text).unwrap();
    let m2 = re.find(text).unwrap();
    assert_eq!(m1, m2); // same source text, same span
    let other = re.find_iter(text).nth(1).unwrap();
    assert_ne!(m1, other); // different span
    let set: HashSet<_> = re.find_iter(text).collect();
    assert_eq!(set.len(), 2);
    assert!(set.contains(&m1));
}

#[test]
fn iteration_semantics() {
    // Anchors keep absolute meaning across restarts: ^ matches only the
    // true start, $ only the true end.
    let spans = |p: &str, t: &str| -> Vec<(usize, usize)> {
        Regex::new(p)
            .unwrap()
            .find_iter(t)
            .map(|m| (m.start(), m.end()))
            .collect()
    };
    assert_eq!(spans("^a", "aaa"), vec![(0, 1)]);
    assert_eq!(spans("a$", "aaa"), vec![(2, 3)]);
    assert_eq!(spans("^a+$", "aaa"), vec![(0, 3)]);
    // Empty-match advance is char-based, never splitting a multibyte char.
    assert_eq!(spans("x*", "éé"), vec![(0, 0), (2, 2), (4, 4)]);
    // POSIX mode: each match is leftmost-longest.
    let posix: Vec<&str> = Regex::new_posix("a|ab")
        .unwrap()
        .find_iter("ab ab")
        .map(|m| m.as_str())
        .collect();
    assert_eq!(posix, vec!["ab", "ab"]);
    // captures_iter carries per-match groups.
    let caps: Vec<(Option<String>, Option<String>)> = Regex::new("(a)(b)?")
        .unwrap()
        .captures_iter("ab a")
        .map(|c| (c.get(1).map(String::from), c.get(2).map(String::from)))
        .collect();
    assert_eq!(
        caps,
        vec![
            (Some("a".into()), Some("b".into())),
            (Some("a".into()), None)
        ]
    );
}

#[test]
fn group_count_and_from_str() {
    assert_eq!(Regex::new("abc").unwrap().group_count(), 1);
    assert_eq!(Regex::new("(a)((b))").unwrap().group_count(), 4);
    let re: Regex = "a|b".parse().unwrap();
    assert!(re.is_match("zb"));
    assert!("a{".parse::<Regex>().is_err());
}

/// Pure-literal patterns take a substring-search path that bypasses the
/// VM; these pin its semantics against the general engine's.
#[test]
fn literal_fast_path_matches_vm_semantics() {
    // Leftmost occurrence, correct spans, all four anchor combinations.
    assert_eq!(find("bcd", "abcdbcd"), Some("bcd"));
    assert_eq!(find("^ab", "abab"), Some("ab"));
    assert_eq!(find("ab$", "abab"), Some("ab"));
    assert_eq!(find("^ab$", "ab"), Some("ab"));
    assert_eq!(find("^ab$", "abab"), None);
    assert_eq!(find("^", "xy"), Some(""));
    assert_eq!(find("$", "xy"), Some(""));
    assert_eq!(find("^$", ""), Some(""));
    assert_eq!(find("^$", "x"), None);
    // Escaped metacharacters are literal chars: still the literal path.
    assert_eq!(find(r"a\.b", "xa.by"), Some("a.b"));
    assert_eq!(find(r"a\.b", "xaXby"), None);
    // Multibyte literals, correct byte spans.
    let caps = Regex::new("éx").unwrap().captures("zéxy").unwrap();
    assert_eq!(caps.span(0), Some((1, 4)));
    // POSIX mode and is_match agree with the leftmost-first path.
    for (p, t) in [
        ("bcd", "abcdbcd"),
        ("^ab$", "abab"),
        ("ab$", "abab"),
        ("q", "zz"),
    ] {
        let first = Regex::new(p).unwrap();
        let posix = Regex::new_posix(p).unwrap();
        assert_eq!(
            first.captures(t).map(|c| c.span(0)),
            posix.captures(t).map(|c| c.span(0)),
            "{p} on {t}"
        );
        assert_eq!(first.is_match(t), first.captures(t).is_some(), "{p} on {t}");
    }
    // ASCII icase literals take the fast path (byte-level ASCII-case-
    // insensitive substring search); non-ASCII ones still fall back to the
    // VM (Unicode folding isn't byte-length-preserving). Both must agree.
    assert_eq!(
        Regex::new_ci("abc")
            .unwrap()
            .captures("xABCy")
            .unwrap()
            .get(0),
        Some("ABC")
    );
    assert!(Regex::new_ci("abc")
        .unwrap()
        .debug_dump()
        .contains("literal"));
    assert_eq!(
        Regex::new_ci("éx")
            .unwrap()
            .captures("zÉXy")
            .unwrap()
            .get(0),
        Some("ÉX")
    );
    assert!(!Regex::new_ci("éx")
        .unwrap()
        .debug_dump()
        .contains("literal"));
}

/// The icase literal fast path is ASCII-only (byte-level case folding);
/// these pin its correctness across anchors and its fallback for non-ASCII
/// literals, which must still match correctly via the VM.
#[test]
fn icase_literal_fast_path_is_ascii_only() {
    assert_eq!(find_ci("bcd", "aBCDbcd"), Some("BCD"));
    assert_eq!(find_ci("^ab", "ABab"), Some("AB"));
    assert_eq!(find_ci("ab$", "ABab"), Some("ab"));
    assert_eq!(find_ci("^ab$", "AB"), Some("AB"));
    assert_eq!(find_ci("^ab$", "abAB"), None);
    // A no-match must still terminate via the fast path, not the VM.
    assert_eq!(find_ci("zz", "aBCDbcd"), None);
    // Multibyte (non-ASCII) chars anywhere in the literal disqualify the
    // fast path for the *whole* literal, not just that char, but matching
    // stays correct via the VM.
    assert_eq!(find_ci("aéc", "xAÉCy"), Some("AÉC"));
    assert_eq!(find_ci("aéc", "xaecy"), None);
}

/// Group-free exactly-literal patterns (incl. exact repetitions and the
/// empty pattern) take the substring path; group-bearing ones must not.
#[test]
fn literal_fast_path_generalizations() {
    assert_eq!(find("a{3}", "xxaaay"), Some("aaa"));
    assert_eq!(find("ab{2}c", "zabbcz"), Some("abbc"));
    assert_eq!(find("^a{2}$", "aa"), Some("aa"));
    assert_eq!(find("^a{2}$", "aaa"), None);
    assert_eq!(find("", "xy"), Some(""));
    assert_eq!(find("", ""), Some(""));
    // A group forces real capture tracking — group 1 must still report.
    assert_eq!(
        groups("(a){3}", "aaa"),
        Some(vec![Some("aaa".into()), Some("a".into())])
    );
}

/// The mandatory-suffix quick reject must never veto a real match.
#[test]
fn suffix_quick_reject_preserves_semantics() {
    // Rejects (no "@x.com" anywhere / at end) and accepts.
    assert_eq!(find("[a-z]+@x\\.com", "aaa bbb ccc"), None);
    assert_eq!(find("[a-z]+@x\\.com", "hi bob@x.com!"), Some("bob@x.com"));
    assert_eq!(find("[a-z]+@x\\.com$", "bob@x.com later"), None);
    assert_eq!(find("[a-z]+@x\\.com$", "mail bob@x.com"), Some("bob@x.com"));
    // Suffix through groups, exact repetitions, and alternation LCS.
    assert_eq!(find("[0-9]+(ab){2}", "7abab"), Some("7abab"));
    assert_eq!(find("[0-9]+(xa|ya)", "3ya"), Some("3ya"));
    assert_eq!(find("[0-9]+(xa|ya)", "3yb"), None);
    // Open-ended repetition tails contribute their body once.
    assert_eq!(find(".b{2,}", "xbbb"), Some("xbbb"));
    // POSIX mode takes the same quick reject.
    assert_eq!(find_posix("[a-z]+@x\\.com", "aaa bbb"), None);
    assert_eq!(find_posix("[0-9]+(ab){2}", "7abab"), Some("7abab"));
}

/// Multi-char mandatory prefixes accelerate the scan; these pin the
/// cases where prefix extraction could overreach.
#[test]
fn prefix_acceleration_preserves_semantics() {
    let long = "xy ".repeat(500);
    // Prefix crosses group boundaries and alternation LCPs.
    assert_eq!(
        groups("(ab)(c|d)e?", &format!("{long}abde")),
        Some(vec![
            Some("abde".into()),
            Some("ab".into()),
            Some("d".into())
        ])
    );
    // Exact-count repetitions extend the prefix ("aaa" here)...
    assert_eq!(find("a{3}b?", &format!("{long}aaa")), Some("aaa"));
    // ...but open-ended ones stop it after the mandatory copies.
    assert_eq!(find("a{2,}b", &format!("{long}aaaab")), Some("aaaab"));
    // A non-literal repetition body contributes only its own prefix
    // ("a", not "aa"): this text contains no "aa", so an overreaching
    // prefix would fast-forward past the only match.
    assert_eq!(find("(ab?){2}c", &format!("{long}abac")), Some("abac"));
    assert_eq!(find("(ab?){2}c", &format!("{long}ababc")), Some("ababc"));
    assert_eq!(find("(ab?){2}c", &format!("{long}aac")), Some("aac"));
}

/// GNU/glibc ERE extensions (issue #18) — every assertion here mirrors a
/// probe run against bash 5.2, the semantics this engine follows.
#[test]
fn gnu_word_assertions_and_classes() {
    // \b / \B / \< / \>
    assert_eq!(find(r"\bword\b", "a word here"), Some("word"));
    assert_eq!(find(r"\bord\b", "a word here"), None);
    assert_eq!(find(r"\w+", "ab_1-x"), Some("ab_1"));
    assert_eq!(find(r"\W", "ab-c"), Some("-"));
    assert_eq!(find(r"\s", "a b"), Some(" "));
    assert_eq!(find(r"\S+", "  hi  "), Some("hi"));
    assert_eq!(find(r"\<end", "the end"), Some("end"));
    assert_eq!(find(r"a\>", "a b"), Some("a"));
    assert_eq!(find(r"\Bb", "ab"), Some("b"));
    // On the empty string: \B matches, \b doesn't (bash-verified).
    assert_eq!(find(r"\B", ""), Some(""));
    assert_eq!(find(r"\b", ""), None);
    // _ is a word char.
    assert_eq!(find(r"_\b", "a_ b"), Some("_"));
    // Inside brackets, POSIX's literal-backslash rule still applies:
    // [\w] is {backslash, w}, exactly as in glibc.
    assert_eq!(find(r"[\w]", "w"), Some("w"));
    assert_eq!(find(r"[\w]", "\\"), Some("\\"));
    // \` and \' are input anchors.
    assert_eq!(find(r"\`ab", "ab"), Some("ab"));
    assert_eq!(find(r"\`b", "ab"), None);
    assert_eq!(find(r"b\'", "ab"), Some("b"));
    // Quantifying an assertion directly is a compile error, as in glibc.
    assert_eq!(
        Regex::new(r"\b*ab").unwrap_err().kind(),
        ErrorKind::DanglingQuantifier
    );
    assert_eq!(
        Regex::new(r"\<{2}a").unwrap_err().kind(),
        ErrorKind::DanglingQuantifier
    );
    // ...but a grouped assertion may be quantified.
    assert_eq!(find(r"(\b)*ab", "ab"), Some("ab"));
    // {,n} and {,}.
    assert_eq!(find(r"^a{,2}$", "aa"), Some("aa"));
    assert_eq!(find(r"^a{,2}$", "aaa"), None);
    assert_eq!(find(r"^a{,}$", "aaa"), Some("aaa"));
    // \d stays a literal d, as in glibc.
    assert_eq!(find(r"\d", "5d"), Some("d"));
    // POSIX mode and boolean mode share the assertion machinery.
    assert_eq!(find_posix(r"\bword\b", "a word here"), Some("word"));
    assert!(Regex::new_posix(r"\<end").unwrap().is_match("the end"));
    assert!(!Regex::new(r"\bord").unwrap().is_match("a word"));
    // Word-ness follows the crate's Unicode locale stance: é is alnum.
    assert_eq!(find(r"\ba", "éa"), None);
    assert_eq!(find(r"\ba", " a"), Some("a"));
    // Captures across assertions.
    assert_eq!(
        groups(r"\<(\w+)\>", "  hey  "),
        Some(vec![Some("hey".into()), Some("hey".into())])
    );
}

/// REG_NEWLINE mode via the builder (issue #21): line-oriented matching.
#[test]
fn newline_mode() {
    let b = || Regex::builder().newline(true);
    // `.` and negated classes exclude \n.
    assert!(!b().build(".").unwrap().is_match("\n"));
    assert!(b().build(".").unwrap().is_match("x"));
    assert!(!b().build("[^a]").unwrap().is_match("\n"));
    assert!(b().build("[^a]").unwrap().is_match("b"));
    // Without the mode, both match \n (bash =~ behavior).
    assert!(Regex::new(".").unwrap().is_match("\n"));
    assert!(Regex::new("[^a]").unwrap().is_match("\n"));
    // ^/$ match at line boundaries (and still at input edges).
    let re = b().build("^b$").unwrap();
    assert_eq!(re.captures("a\nb\nc").unwrap().span(0), Some((2, 3)));
    assert!(b().build("^a").unwrap().is_match("a\nb"));
    assert!(b().build("c$").unwrap().is_match("b\nc"));
    assert!(!Regex::new("^b$").unwrap().is_match("a\nb\nc"));
    // find_iter walks lines.
    let re = b().build("^[a-z]+$").unwrap();
    let words: Vec<&str> = re.find_iter("foo\nbar\nbaz").map(|m| m.as_str()).collect();
    assert_eq!(words, ["foo", "bar", "baz"]);
    // Composes with the other modes.
    let re = Regex::builder()
        .posix(true)
        .case_insensitive(true)
        .newline(true)
        .build("^a|ab$")
        .unwrap();
    assert!(re.is_match("x\nAB"));
    // Builder defaults equal Regex::new.
    assert_eq!(
        Regex::builder()
            .build("a|ab")
            .unwrap()
            .find("xab")
            .unwrap()
            .as_str(),
        "a"
    );
}

/// `` \` `` / `\'` (GNU absolute buffer anchors) always mean true start/end
/// of input, unlike `^`/`$`, which under `REG_NEWLINE` also match around
/// embedded newlines. This used to be conflated with `^`/`$` (both parsed
/// to the same AST node) so `` \`b `` incorrectly matched at line starts
/// under the mode, exactly the combination glibc introduced these escapes
/// to avoid.
#[test]
fn buffer_anchors_are_immune_to_newline_mode() {
    let b = || Regex::builder().newline(true);
    // `^`/`$` match at line boundaries under the mode...
    assert!(b().build("^b").unwrap().is_match("a\nb"));
    assert!(b().build("a$").unwrap().is_match("a\nb"));
    // ...but `` \` ``/`\'` never do, mode or not.
    assert!(!b().build(r"\`b").unwrap().is_match("a\nb"));
    assert!(!b().build(r"a\'").unwrap().is_match("a\nb"));
    assert!(!Regex::new(r"\`b").unwrap().is_match("a\nb"));
    assert!(!Regex::new(r"a\'").unwrap().is_match("a\nb"));
    // They still match the true edges, mode or not.
    assert!(b().build(r"\`a").unwrap().is_match("a\nb"));
    assert!(b().build(r"b\'").unwrap().is_match("a\nb"));
    // The anchored/literal fast paths must agree: `` \` `` still enables
    // the anchored-at-0 fast path even under the mode (unlike `^`), and a
    // pure-literal `` \`lit `` pattern's substring path stays valid too.
    let re = b().build(r"\`abc").unwrap();
    assert!(re.debug_dump().contains("anchored"));
    assert!(re.is_match("abc"));
    assert!(!re.is_match("x\nabc"));
    let re = b().build(r"abc\'").unwrap();
    assert!(re.is_match("abc"));
    assert!(!re.is_match("abc\nx"));
}

/// debug_dump's format is unstable, but its load-bearing facts —
/// which tier ran, what was extracted — should stay visible.
#[test]
fn debug_dump_shows_strategy() {
    let dump = Regex::new("abc").unwrap().debug_dump();
    assert!(dump.contains("literal substring path"), "{dump}");
    let dump = Regex::new("release-[0-9]+").unwrap().debug_dump();
    assert!(dump.contains("scan prefix: \"release-\""), "{dump}");
    assert!(dump.contains("pike-vm"), "{dump}");
    assert!(dump.contains("Match"), "{dump}");
    let dump = Regex::builder()
        .posix(true)
        .newline(true)
        .build("x[0-9]$")
        .unwrap()
        .debug_dump();
    assert!(dump.contains("posix"), "{dump}");
    assert!(dump.contains("+ newline"), "{dump}");
}

/// Round-5 scan hints: assertion-transparent prefixes, class-head
/// fast-forward, degenerate-class canonicalization. Pins the regression
/// the bool path had (carrying a stale \b verdict across a skip).
#[test]
fn scan_hints_preserve_semantics() {
    let long = "pass word sword ".repeat(2_000);
    // Assertion-transparent prefix: correct matches and — critically —
    // correct *rejections* through is_match (the once-broken path).
    let re = Regex::new(r"\bword\b").unwrap();
    assert!(re.is_match(&long));
    assert_eq!(re.find(&long).map(|m| (m.start(), m.end())), Some((5, 9)));
    assert!(!re.is_match(&"password sword ".repeat(2_000)));
    assert!(!Regex::new(r"\bword\b").unwrap().is_match("password"));
    // Suffix through assertions still finds the real hit, not "sword"'s.
    assert_eq!(
        Regex::new(r"word\b")
            .unwrap()
            .find("swordfish word")
            .map(|m| m.start()),
        Some(10)
    );
    assert_eq!(
        Regex::new(r"\<sword")
            .unwrap()
            .find(&long)
            .map(|m| m.start()),
        Some(10)
    );
    // Class-head fast-forward, all modes.
    let digits = format!("{}42x", "xyz ".repeat(2_000));
    for re in [
        Regex::new("[0-9]+x").unwrap(),
        Regex::new_posix("[0-9]+x").unwrap(),
    ] {
        assert_eq!(re.find(&digits).map(|m| m.as_str()), Some("42x"));
        assert!(re.is_match(&digits));
        assert!(!re.is_match(&"xyz ".repeat(2_000)));
    }
    // icase class head folds while scanning.
    let re = Regex::new_ci("[a-f]+9").unwrap();
    assert_eq!(
        re.find(&format!("{}CAFE9", "XYZ ".repeat(1_000)))
            .map(|m| m.as_str()),
        Some("CAFE9")
    );
    // \w-headed patterns get the hint too (desugared class).
    assert!(Regex::new(r"\w+=").unwrap().is_match("   key=1"));
    // Canonicalized degenerate classes take the literal tier.
    let dump = Regex::new("[a]bc").unwrap().debug_dump();
    assert!(dump.contains("literal substring path"), "{dump}");
    let dump = Regex::new("[[.a.]]bc").unwrap().debug_dump();
    assert!(dump.contains("literal substring path"), "{dump}");
    // Negated and multi-char classes are untouched.
    assert!(Regex::new("[^a]").unwrap().is_match("b"));
    assert!(!Regex::new("[^a]").unwrap().is_match("a"));
    // \bword\b now reports a scan prefix in the dump.
    let dump = Regex::new(r"\bword\b").unwrap().debug_dump();
    assert!(dump.contains("scan prefix: \"word\""), "{dump}");
    // The class-head hint itself now shows up in the dump (previously
    // silently omitted, unlike the prefix/suffix hints).
    let dump = Regex::new("[0-9]+x").unwrap().debug_dump();
    assert!(dump.contains("scan hint: mandatory head class"), "{dump}");
}

/// `exec_posix` previously checked only `Program::prefix` before
/// fast-forwarding, so class-headed patterns like `[0-9]+` (no literal
/// prefix, only a mandatory head *class*) never got the scan-hint
/// fast-forward under `Regex::new_posix` — silently falling back to
/// unaccelerated per-char stepping. Both modes share one `Program`
/// (`posix` only selects the VM entry point), so leftmost-first and POSIX
/// must take the same amount of work, not just agree on the answer; a
/// generous wall-clock cap (not a tight one, to stay non-flaky on shared
/// CI runners) catches a regression back to the unaccelerated path, which
/// was roughly 150x slower on this shape in local measurements.
#[test]
fn posix_mode_gets_the_class_head_scan_hint() {
    let hay = "x".repeat(5_000_000);
    let re = Regex::new_posix("[0-9]+").unwrap();
    let start = std::time::Instant::now();
    assert!(re.captures(&hay).is_none());
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "POSIX class-headed no-match took {:?} — the scan-hint fast-forward regressed",
        start.elapsed()
    );
}

/// Under `REG_NEWLINE`, a negated bracket expression excludes `\n` (see
/// `newline_mode`) — and the class-head *scan hint* must exclude it too,
/// or it offers the VM a candidate position it will reject, degrading
/// (not breaking — the VM still validates) the fast-forward. Exercised
/// through a long run of otherwise-matching text broken by a newline, so
/// a hint that wrongly treats `\n` as a head-class member would still
/// (slowly) reach the right answer — this pins the *result*, which is
/// covered by other tests, together with using the same class instance
/// as the head-class hint via interning (see `identical_classes_are_interned`).
#[test]
fn newline_mode_class_head_hint_excludes_newline() {
    let re = Regex::builder().newline(true).build("[^a]+x").unwrap();
    // "bb" is followed by '\n', not 'x' — REG_NEWLINE excludes '\n' from
    // `[^a]`, so that run can't extend across it; the real match is "cc x".
    assert_eq!(re.find("bb\ncc x").map(|m| m.as_str()), Some("cc x"));
}

/// Repeated occurrences of the same bracket expression (a fixed-count
/// interval body, or the same class reused across alternation branches)
/// now intern to one shared `CompiledClass` at compile time (see
/// `Compiler::intern_class`) instead of a fresh copy per occurrence —
/// this pins that the sharing is transparent to matching.
#[test]
fn identical_classes_are_interned() {
    let re = Regex::new("[0-9]{6}").unwrap();
    assert_eq!(re.find("id 123456!").map(|m| m.as_str()), Some("123456"));
    assert!(!re.is_match("12345")); // too short: the interned class still gates length
    let re = Regex::new("([0-9]x|[0-9]y)+").unwrap();
    assert_eq!(re.find("_1x2y3x_").map(|m| m.as_str()), Some("1x2y3x"));
}

/// `Regex::clone()` shares the compiled program (`Arc`), not a deep copy —
/// this pins that a clone matches identically and independently (dropping
/// one doesn't affect the other) rather than testing the sharing itself,
/// which isn't observable from the public API.
#[test]
fn clone_is_independent_and_correct() {
    let re = Regex::new("([a-z]+)-([0-9]+)").unwrap();
    let clone = re.clone();
    drop(re);
    let caps = clone.captures("build-42").unwrap();
    assert_eq!(caps.get(0), Some("build-42"));
    assert_eq!(caps.get(1), Some("build"));
    assert_eq!(caps.get(2), Some("42"));
}

#[test]
fn repetition_size_limits() {
    // A single interval past the cap is a syntactic condition — the `{`'s
    // position is known and reported (unlike the aggregate case below).
    let e = Regex::new("a{1001}").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::RepetitionTooLarge);
    assert_eq!(e.position(), Some(1));
    let e = Regex::new("ab{3,1001}").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::RepetitionTooLarge);
    assert_eq!(e.position(), Some(2));
    let e = Regex::new("a{1001,}").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::RepetitionTooLarge);
    assert_eq!(e.position(), Some(1));
    // Within the per-interval cap but past the program-size cap: an
    // aggregate condition with no single position to blame.
    let e = Regex::new("(a{1000}){1000}").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::RepetitionTooLarge);
    assert_eq!(e.position(), None);
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

/// The case-insensitive modes now get scan fast-forward (folded prefix
/// search): matches deep in mixed-case text must still be found, in
/// both directions of folding.
#[test]
fn icase_prefix_acceleration_preserves_semantics() {
    let long = "XyZ ".repeat(500);
    assert_eq!(
        Regex::new_posix_ci("release-[0-9]+")
            .unwrap()
            .captures(&format!("{long}ReLeAsE-77"))
            .unwrap()
            .get(0),
        Some("ReLeAsE-77")
    );
    assert_eq!(
        Regex::new_ci("AbC[0-9]")
            .unwrap()
            .captures(&format!("{long}abc9"))
            .unwrap()
            .get(0),
        Some("abc9")
    );
    // No-match still terminates via the folded scan.
    assert!(!Regex::new_ci("qq[0-9]").unwrap().is_match(&long));
    // Sigma folds: pattern σ (folds to Σ) must find input ς.
    assert_eq!(
        Regex::new_ci("σx")
            .unwrap()
            .captures(&format!("{long}ςx"))
            .unwrap()
            .get(0),
        Some("ςx")
    );
}

/// The suffix quick-reject used to be disabled entirely in `icase` mode
/// (only the prefix side was fold-aware); a mandatory-tail pattern like
/// `foo[0-9]+bar$` against a long non-matching haystack fell back to full
/// NFA simulation instead of the one-scan reject non-icase patterns get.
/// These pin the fold-aware `contains`/`ends_with` comparisons.
#[test]
fn icase_suffix_quick_reject_preserves_semantics() {
    let long = "no match here, ".repeat(500);
    // Rejects: the folded suffix never occurs.
    assert!(!Regex::new_ci("[a-z]+@X\\.COM").unwrap().is_match(&long));
    assert!(!Regex::new_ci("[a-z]+@X\\.COM$")
        .unwrap()
        .is_match(&format!("{long}bob@x.com later")));
    // Accepts, folding either direction.
    assert_eq!(
        find_ci("[a-z]+@X\\.COM", &format!("{long}hi BOB@x.COM!")),
        Some("BOB@x.COM")
    );
    assert_eq!(
        find_ci("[a-z]+@X\\.COM$", &format!("{long}mail bob@X.com")),
        Some("bob@X.com")
    );
    // Sigma folds at the tail too.
    assert_eq!(
        Regex::new_ci("x[σΣ]")
            .unwrap()
            .captures(&format!("{long}xς"))
            .unwrap()
            .get(0),
        Some("xς")
    );
}

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
        Regex::new_posix_ci("[Z-a]").unwrap_err().kind(),
        ErrorKind::InvalidRange
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
