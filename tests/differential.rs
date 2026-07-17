//! The differential harness (roadmap step 5).
//!
//! Generates random patterns from the syntax subset shared by this engine,
//! the `regex` crate, and bash ERE, then cross-checks:
//!
//! - **`regex` crate** (dev-dependency): full capture-group agreement —
//!   v1 promises regex-crate-compatible leftmost-first semantics.
//! - **bash oracle** (`[[ $text =~ $pattern ]]` via a spawned bash):
//!   match/no-match agreement only. Boolean matching is equivalent between
//!   leftmost-first and bash's leftmost-longest, so this is exact; submatch
//!   agreement with bash is the v2 POSIX mode's job.
//!
//! The generator deliberately avoids constructs where the engines disagree
//! by design (documented in DESIGN.md / the parser docs):
//!
//! - backslash inside brackets (POSIX: literal; regex crate: escape),
//! - `.` against newline (we match it; the regex crate doesn't by default),
//! - non-ASCII text against POSIX classes (we use Unicode `char` fallbacks;
//!   the regex crate's POSIX classes are ASCII-only),
//! - anchors anywhere but the pattern ends (the regex crate rejects e.g.
//!   quantified anchors that POSIX ERE permits),
//! - escapes of non-metacharacters (`\a` is a literal `a` in POSIX but BEL
//!   to the regex crate),
//! - degenerate `{0,0}` intervals (the regex crate elides trailing capture
//!   groups under them from `Captures::len()`; we keep POSIX numbering,
//!   as bash does).

use rusty_regx::Regex;

/// Deterministic xorshift64* PRNG so failures reproduce exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Uniform in `0..n`.
    fn below(&mut self, n: u32) -> u32 {
        (self.next() % u64::from(n)) as u32
    }
}

const CLASSES: &[&str] = &[
    "[abc]",
    "[^ab]",
    "[a-f]",
    "[0-9]",
    "[a-c0-3]",
    "[[:digit:]]",
    "[[:alpha:]]",
    "[[:alnum:]]",
    "[]a]",
    "[a-]",
];

/// Classes for the case-insensitive oracle: mixed-case sets and ranges,
/// plus the classes REG_ICASE treats specially ([[:upper:]]/[[:lower:]]).
/// No range here reverses after case folding.
const CLASSES_CI: &[&str] = &[
    "[abc]",
    "[ABC]",
    "[^ab]",
    "[^X-Z]",
    "[a-f]",
    "[X-Z]",
    "[A-F0-3]",
    "[[:digit:]]",
    "[[:upper:]]",
    "[[:lower:]]",
    "[]a]",
    "[A-]",
];

const LETTERS: &[u8] = b"abc01 ";
const LETTERS_CI: &[u8] = b"abcABC01 ";

// Escaped metacharacters, plus the GNU class escapes — a literal `a` in
// POSIX ERE would be BEL to the regex crate, so only metachars and the
// GNU set (which the crate agrees on over ASCII) are generated.
const ESCAPES: &[&str] = &[
    "\\.", "\\*", "\\+", "\\?", "\\(", "\\)", "\\[", "\\|", "\\w", "\\W", "\\s", "\\S",
];

// Zero-width word assertions. Never quantified by the generator: glibc
// rejects a quantifier directly on one (verified against bash 5.2).
// The regex crate supports \b/\B (Unicode-aware, but identical over
// ASCII); \< and \> are glibc-only, so only the bash oracles get them.
const ASSERTS_CRATE: &[&str] = &["\\b", "\\B"];
const ASSERTS_GNU: &[&str] = &["\\b", "\\B", "\\<", "\\>"];

/// `quant` — may this level carry quantifiers? The bash oracle sets it to
/// false inside groups: glibc's regexec (what bash uses) goes superlinear
/// on quantifiers nested inside quantified groups, so the oracle would hang
/// on patterns our engine and the regex crate handle in linear time.
#[derive(Clone, Copy)]
struct Gen {
    quant: bool,
    nested_quant: bool,
    letters: &'static [u8],
    classes: &'static [&'static str],
    asserts: &'static [&'static str],
}

fn gen_atom(rng: &mut Rng, depth: u32, g: Gen) -> String {
    match rng.below(if depth > 0 { 13 } else { 11 }) {
        0..=4 => {
            let c = g.letters[rng.below(g.letters.len() as u32) as usize] as char;
            c.to_string()
        }
        5 => ".".to_string(),
        6 | 7 => g.classes[rng.below(g.classes.len() as u32) as usize].to_string(),
        8 | 9 => ESCAPES[rng.below(ESCAPES.len() as u32) as usize].to_string(),
        10 if !g.asserts.is_empty() => {
            g.asserts[rng.below(g.asserts.len() as u32) as usize].to_string()
        }
        10 => ".".to_string(),
        _ => {
            let inner = Gen {
                quant: g.nested_quant,
                ..g
            };
            format!("({})", gen_alternation(rng, depth - 1, inner))
        }
    }
}

fn gen_piece(rng: &mut Rng, depth: u32, g: Gen) -> String {
    let atom = gen_atom(rng, depth, g);
    // Quantifying an assertion directly is a glibc compile error.
    if !g.quant || g.asserts.contains(&atom.as_str()) {
        return atom;
    }
    let quant = match rng.below(10) {
        0 => "*",
        1 => "+",
        2 => "?",
        3 => return format!("{atom}{{{}}}", rng.below(3) + 1),
        4 => {
            let m = rng.below(3);
            // n >= 1: under a degenerate `{0,0}` the regex crate elides
            // trailing capture groups from Captures::len(); we keep POSIX
            // group numbering (as bash does). Known, intentional divergence.
            let n = (m + rng.below(3)).max(1);
            return format!("{atom}{{{m},{n}}}");
        }
        _ => "",
    };
    format!("{atom}{quant}")
}

fn gen_concat(rng: &mut Rng, depth: u32, g: Gen) -> String {
    let n = rng.below(4) + 1;
    (0..n).map(|_| gen_piece(rng, depth, g)).collect()
}

fn gen_alternation(rng: &mut Rng, depth: u32, g: Gen) -> String {
    let n = rng.below(3) + 1;
    (0..n)
        .map(|_| gen_concat(rng, depth, g))
        .collect::<Vec<_>>()
        .join("|")
}

/// A full pattern: an alternation, optionally anchored at either end.
fn gen_pattern(
    rng: &mut Rng,
    nested_quant: bool,
    asserts: &'static [&'static str],
    classes: &'static [&'static str],
) -> String {
    gen_pattern_over(rng, nested_quant, LETTERS, classes, asserts)
}

/// `CLASSES` plus the bash-only collating forms, for the bash oracles.
const CLASSES_GNU: &[&str] = &[
    "[abc]",
    "[^ab]",
    "[a-f]",
    "[0-9]",
    "[a-c0-3]",
    "[[:digit:]]",
    "[[:alpha:]]",
    "[[:alnum:]]",
    "[]a]",
    "[a-]",
    "[[.a.]]",
    "[[.a.]-c]",
    "[[=b=]]",
];

fn gen_pattern_over(
    rng: &mut Rng,
    nested_quant: bool,
    letters: &'static [u8],
    classes: &'static [&'static str],
    asserts: &'static [&'static str],
) -> String {
    let g = Gen {
        quant: true,
        nested_quant,
        letters,
        classes,
        asserts,
    };
    let mut p = gen_alternation(rng, 2, g);
    if rng.below(4) == 0 {
        p.insert(0, '^');
    }
    if rng.below(4) == 0 {
        p.push('$');
    }
    p
}

/// Random text over the alphabet the patterns talk about (ASCII only, no
/// newlines — see the module docs for why).
fn gen_text(rng: &mut Rng) -> String {
    gen_text_over(rng, b"aabbcc0123def .-")
}

fn gen_text_over(rng: &mut Rng, alphabet: &[u8]) -> String {
    let len = rng.below(13);
    (0..len)
        .map(|_| alphabet[rng.below(alphabet.len() as u32) as usize] as char)
        .collect()
}

const CASES: u32 = 2000;
const TEXTS_PER_PATTERN: u32 = 4;

#[test]
fn differential_against_regex_crate() {
    let mut rng = Rng(0x5EED_CAFE_F00D_0001);
    for case in 0..CASES {
        let pattern = gen_pattern(&mut rng, true, ASSERTS_CRATE, CLASSES);
        let ours = Regex::new(&pattern)
            .unwrap_or_else(|e| panic!("case {case}: we rejected {pattern:?}: {e}"));
        let theirs = regex::Regex::new(&pattern)
            .unwrap_or_else(|e| panic!("case {case}: regex crate rejected {pattern:?}: {e}"));
        for _ in 0..TEXTS_PER_PATTERN {
            let text = gen_text(&mut rng);
            let a: Option<Vec<Option<String>>> = ours.captures(&text).map(|caps| {
                (0..caps.len())
                    .map(|i| caps.get(i).map(str::to_owned))
                    .collect()
            });
            let b: Option<Vec<Option<String>>> = theirs.captures(&text).map(|caps| {
                (0..caps.len())
                    .map(|i| caps.get(i).map(|m| m.as_str().to_owned()))
                    .collect()
            });
            // Boolean agreement is immune to the crate's prefilter
            // artifact, so it holds unconditionally.
            assert_eq!(
                ours.is_match(&text),
                theirs.is_match(&text),
                "case {case}: is_match divergence on pattern {pattern:?}, text {text:?}"
            );
            if a != b && crate_skipped_earlier_match(&pattern, &ours, &theirs, &text) {
                continue;
            }
            assert_eq!(
                a, b,
                "case {case}: divergence on pattern {pattern:?}, text {text:?} (ours = left)"
            );
            // With captures agreeing, find's span must agree too — this is
            // the external oracle for the group-0-only fast path.
            assert_eq!(
                ours.find(&text).map(|m| (m.start(), m.end())),
                theirs.find(&text).map(|m| (m.start(), m.end())),
                "case {case}: find divergence on pattern {pattern:?}, text {text:?}"
            );
        }
    }
}

/// The regex crate's literal optimizations can return a *later-starting*
/// match than its own leftmost contract promises (e.g. `a|[0-9].*aa` on
/// `"-bc0b0aa"` yields `"a"` at offset 6, though a match starts at 3 —
/// Perl and bash both return `"0b0aa"`). `find_at` shows the same
/// artifact, so when we disagree we accept our answer iff it starts
/// strictly earlier AND an *anchored* recompile of the same pattern
/// (which bypasses the prefilter) confirms our exact match at that offset
/// — proof the artifact is theirs, not ours.
fn crate_skipped_earlier_match(
    pattern: &str,
    ours: &Regex,
    theirs: &regex::Regex,
    text: &str,
) -> bool {
    crate_skipped_earlier_match_wrapped(pattern, pattern, ours, theirs, text)
}

/// As [`crate_skipped_earlier_match`], for engines compiled from a
/// transformed pattern (e.g. `(?i:...)`): `raw` is the generated pattern
/// (for the anchor check), `crate_pat` what the crate compiled.
fn crate_skipped_earlier_match_wrapped(
    raw: &str,
    crate_pat: &str,
    ours: &Regex,
    theirs: &regex::Regex,
    text: &str,
) -> bool {
    let pattern = raw;
    let (our_g0, their_m) = match (
        ours.captures(text).and_then(|c| c.get(0)),
        theirs.find(text),
    ) {
        (Some(g0), Some(m)) => (g0, m),
        _ => return false,
    };
    // Our group 0 is a subslice of `text`; recover its byte offset.
    let our_start = our_g0.as_ptr() as usize - text.as_ptr() as usize;
    if our_start >= their_m.start() {
        return false;
    }
    // Anchors keep their meaning relative to the full text, so anchored
    // patterns can't be confirmed this way (the generator only ever places
    // anchors at the pattern's ends; a leading `[^…]` class is not one).
    if pattern.starts_with('^') || pattern.ends_with('$') {
        return false;
    }
    regex::Regex::new(&format!("^(?:{crate_pat})"))
        .ok()
        .and_then(|re| re.find(&text[our_start..]))
        .is_some_and(|m| m.as_str() == our_g0)
}

#[test]
fn differential_against_bash_oracle() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // One bash process answers every case: pattern and text arrive on
    // alternating stdin lines, `1`/`0` per case comes back on stdout.
    let script = r#"while IFS= read -r pat && IFS= read -r text; do
        if [[ $text =~ $pat ]]; then echo 1; else echo 0; fi
    done"#;
    let spawned = Command::new("bash")
        .args(["-c", script])
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            eprintln!("bash not available; skipping oracle test");
            return;
        }
    };

    let mut rng = Rng(0xBA5E_0AC1_E000_0002);
    let mut cases = Vec::new();
    let mut input = String::new();
    for _ in 0..CASES {
        let pattern = gen_pattern(&mut rng, false, ASSERTS_GNU, CLASSES_GNU);
        let text = gen_text(&mut rng);
        input.push_str(&pattern);
        input.push('\n');
        input.push_str(&text);
        input.push('\n');
        cases.push((pattern, text));
    }
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "bash oracle exited with an error");
    let answers: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(answers.len(), cases.len(), "oracle answered fewer cases");

    for ((pattern, text), answer) in cases.iter().zip(answers) {
        let ours = Regex::new(pattern)
            .unwrap_or_else(|e| panic!("we rejected {pattern:?}: {e}"))
            .captures(text)
            .is_some();
        let bash = answer == "1";
        assert_eq!(
            ours, bash,
            "bash divergence on pattern {pattern:?}, text {text:?} (ours = left)"
        );
    }
}

/// The leftmost-first case-insensitive mode (`new_ci`) has no bash
/// oracle — bash's `nocasematch` is leftmost-longest — so the `regex`
/// crate's `(?i)` is the oracle: over ASCII input, `REG_ICASE`
/// upper-folding and the crate's case-insensitive matching agree
/// (including folded ranges and `[[:upper:]]`/`[[:lower:]]`, which both
/// engines make case-symmetric).
#[test]
fn differential_ci_against_regex_crate() {
    let mut rng = Rng(0xC1CA_5ED1_FF00_0005);
    for case in 0..CASES {
        let pattern = gen_pattern_over(&mut rng, true, LETTERS_CI, CLASSES_CI, ASSERTS_CRATE);
        let ours = Regex::new_ci(&pattern)
            .unwrap_or_else(|e| panic!("case {case}: we rejected {pattern:?}: {e}"));
        let wrapped = format!("(?i:{pattern})");
        // Grammar corner cases the crate rejects (e.g. `[]a]`) aren't
        // comparable; skip them rather than lose the whole run.
        let Ok(theirs) = regex::Regex::new(&wrapped) else {
            continue;
        };
        for _ in 0..TEXTS_PER_PATTERN {
            let text = gen_text_over(&mut rng, b"aAbBcC0123dDeEf .-");
            let a: Option<Vec<Option<String>>> = ours.captures(&text).map(|caps| {
                (0..caps.len())
                    .map(|i| caps.get(i).map(str::to_owned))
                    .collect()
            });
            let b: Option<Vec<Option<String>>> = theirs.captures(&text).map(|caps| {
                (0..caps.len())
                    .map(|i| caps.get(i).map(|m| m.as_str().to_owned()))
                    .collect()
            });
            assert_eq!(
                ours.is_match(&text),
                theirs.is_match(&text),
                "case {case}: ci is_match divergence on pattern {pattern:?}, text {text:?}"
            );
            if a != b
                && crate_skipped_earlier_match_wrapped(&pattern, &wrapped, &ours, &theirs, &text)
            {
                continue;
            }
            assert_eq!(
                a, b,
                "case {case}: ci divergence on pattern {pattern:?}, text {text:?} (ours = left)"
            );
            assert_eq!(
                ours.find(&text).map(|m| (m.start(), m.end())),
                theirs.find(&text).map(|m| (m.start(), m.end())),
                "case {case}: ci find divergence on pattern {pattern:?}, text {text:?}"
            );
        }
    }
}

/// POSIX mode vs. bash, comparing the full BASH_REMATCH contents — group
/// bounds and submatches, not just match/no-match. This is what validates
/// the v2 leftmost-longest mode against the real thing.
#[test]
fn differential_posix_captures_against_bash_oracle() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // On a match, bash answers `1<US>group0<US>group1…`; unmatched optional
    // groups are empty strings in BASH_REMATCH, matching our
    // `get(i).unwrap_or_default()` view. US (0x1f) can't occur in
    // generated text.
    let script = r#"while IFS= read -r pat && IFS= read -r text; do
        if [[ $text =~ $pat ]]; then
            printf 1
            for g in "${BASH_REMATCH[@]}"; do printf '\x1f%s' "$g"; done
            printf '\n'
        else echo 0; fi
    done"#;
    let spawned = Command::new("bash")
        .args(["-c", script])
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            eprintln!("bash not available; skipping oracle test");
            return;
        }
    };

    let mut rng = Rng(0xD1FF_0CA5_E5B0_0003);
    let mut cases = Vec::new();
    let mut input = String::new();
    for _ in 0..CASES {
        let pattern = gen_pattern(&mut rng, false, ASSERTS_GNU, CLASSES_GNU);
        let text = gen_text(&mut rng);
        input.push_str(&pattern);
        input.push('\n');
        input.push_str(&text);
        input.push('\n');
        cases.push((pattern, text));
    }
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "bash oracle exited with an error");
    let answers: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(answers.len(), cases.len(), "oracle answered fewer cases");

    for ((pattern, text), answer) in cases.iter().zip(answers) {
        let re =
            Regex::new_posix(pattern).unwrap_or_else(|e| panic!("we rejected {pattern:?}: {e}"));
        let ours = re.captures(text).map(|caps| {
            (0..caps.len())
                .map(|i| caps.get(i).unwrap_or_default().to_owned())
                .collect::<Vec<String>>()
        });
        let bash: Option<Vec<String>> = answer
            .strip_prefix('1')
            .map(|rest| rest.split('\x1f').skip(1).map(str::to_owned).collect());
        // Group 0 — the overall leftmost-longest match — must agree with
        // bash exactly; this is the divergence the mode exists to close.
        assert_eq!(
            ours.as_ref().map(|g| &g[0]),
            bash.as_ref().map(|g| &g[0]),
            "POSIX overall-match divergence on pattern {pattern:?}, text {text:?} (ours = left)"
        );
        // Submatches must agree too, except where glibc deviates from the
        // POSIX longest-alternative rule (see KNOWN_GLIBC_SUBMATCH_QUIRKS).
        if ours != bash && !known_glibc_submatch_quirk(pattern, text) {
            panic!(
                "POSIX submatch divergence on pattern {pattern:?}, text {text:?}\n  ours: {ours:?}\n  bash: {bash:?}"
            );
        }
    }
}

/// The case-insensitive POSIX mode vs. bash under `shopt -s nocasematch`,
/// comparing full BASH_REMATCH contents — this is what validates
/// `Regex::new_posix_ci` as REG_ICASE-equivalent (the handoff's acceptance
/// criterion for rush's `nocasematch` + `=~`).
#[test]
fn differential_posix_ci_captures_against_bash_oracle() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let script = r#"shopt -s nocasematch
    while IFS= read -r pat && IFS= read -r text; do
        if [[ $text =~ $pat ]]; then
            printf 1
            for g in "${BASH_REMATCH[@]}"; do printf '\x1f%s' "$g"; done
            printf '\n'
        else echo 0; fi
    done"#;
    let spawned = Command::new("bash")
        .args(["-c", script])
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            eprintln!("bash not available; skipping oracle test");
            return;
        }
    };

    let mut rng = Rng(0x1CA5_EF01_D000_0004);
    let mut cases = Vec::new();
    let mut input = String::new();
    for _ in 0..CASES {
        let pattern = gen_pattern_over(&mut rng, false, LETTERS_CI, CLASSES_CI, ASSERTS_GNU);
        let text = gen_text_over(&mut rng, b"aAbBcC0123dDeEf .-");
        input.push_str(&pattern);
        input.push('\n');
        input.push_str(&text);
        input.push('\n');
        cases.push((pattern, text));
    }
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "bash oracle exited with an error");
    let answers: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(answers.len(), cases.len(), "oracle answered fewer cases");

    for ((pattern, text), answer) in cases.iter().zip(answers) {
        let re =
            Regex::new_posix_ci(pattern).unwrap_or_else(|e| panic!("we rejected {pattern:?}: {e}"));
        let ours = re.captures(text).map(|caps| {
            (0..caps.len())
                .map(|i| caps.get(i).unwrap_or_default().to_owned())
                .collect::<Vec<String>>()
        });
        let bash: Option<Vec<String>> = answer
            .strip_prefix('1')
            .map(|rest| rest.split('\x1f').skip(1).map(str::to_owned).collect());
        assert_eq!(
            ours.as_ref().map(|g| &g[0]),
            bash.as_ref().map(|g| &g[0]),
            "nocasematch overall-match divergence on pattern {pattern:?}, text {text:?} (ours = left)"
        );
        if ours != bash && !known_glibc_submatch_quirk(pattern, text) {
            panic!(
                "nocasematch submatch divergence on pattern {pattern:?}, text {text:?}\n  ours: {ours:?}\n  bash: {bash:?}"
            );
        }
    }
}

/// glibc (what bash uses) does not implement the POSIX rule that an
/// alternation prefers its longest-matching branch *inside a repetition
/// iteration*: it can report a shorter branch for the final iteration when
/// a longer one is available (a long-known glibc nonconformance; our
/// engine follows POSIX). Divergent cases confirmed by hand go here so the
/// harness stays exact everywhere else.
fn known_glibc_submatch_quirk(pattern: &str, text: &str) -> bool {
    const KNOWN_GLIBC_SUBMATCH_QUIRKS: &[(&str, &str)] = &[
        // Under nocasematch, glibc reports the final `+` iteration as "C"
        // (the short `[[:lower:]]` alternative) where POSIX requires the
        // longest iteration "aAC" (`[[:lower:]][^X-Z].`). Group 0 agrees
        // ("daAC"); hand-confirmed against bash 5.2.
        (
            r"^0*\[[[:lower:]]+|([[:lower:]]b|([[:lower:]][^X-Z].|[[:lower:]]))+|[[:digit:]]{3}(\)a |.|[A-F0-3]b)A*.",
            " daAC",
        ),
        // Full-tie branch choice: both the 2nd and 3rd alternatives span
        // "AcEd0"; every tag comparison is participating-vs-absent (a tie
        // under the POSIX pre-order rule), so we keep the earlier branch
        // (group 2 absent) while glibc reports the later one's group 2 =
        // "0". Hand-confirmed against bash 5.2.
        (
            r"c+  \+{0,1}|.{2,4}(1Cc)*\w{0,1}\>|.+.?(.|.\S.[A-]|0\*){3}",
            "AcEd0",
        ),
    ];
    KNOWN_GLIBC_SUBMATCH_QUIRKS.contains(&(pattern, text))
}
