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

// Escaped metacharacters only: escapes like `\a` mean a literal `a` in
// POSIX ERE but the BEL control character to the regex crate.
const ESCAPES: &[&str] = &["\\.", "\\*", "\\+", "\\?", "\\(", "\\)", "\\[", "\\|"];

/// `quant` — may this level carry quantifiers? The bash oracle sets it to
/// false inside groups: glibc's regexec (what bash uses) goes superlinear
/// on quantifiers nested inside quantified groups, so the oracle would hang
/// on patterns our engine and the regex crate handle in linear time.
#[derive(Clone, Copy)]
struct Gen {
    quant: bool,
    nested_quant: bool,
}

fn gen_atom(rng: &mut Rng, depth: u32, g: Gen) -> String {
    match rng.below(if depth > 0 { 12 } else { 10 }) {
        0..=4 => {
            let c = b"abc01 "[rng.below(6) as usize] as char;
            c.to_string()
        }
        5 => ".".to_string(),
        6 | 7 => CLASSES[rng.below(CLASSES.len() as u32) as usize].to_string(),
        8 | 9 => ESCAPES[rng.below(ESCAPES.len() as u32) as usize].to_string(),
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
    if !g.quant {
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
fn gen_pattern(rng: &mut Rng, nested_quant: bool) -> String {
    let g = Gen {
        quant: true,
        nested_quant,
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
    let len = rng.below(13);
    (0..len)
        .map(|_| b"aabbcc0123def .-"[rng.below(16) as usize] as char)
        .collect()
}

const CASES: u32 = 2000;
const TEXTS_PER_PATTERN: u32 = 4;

#[test]
fn differential_against_regex_crate() {
    let mut rng = Rng(0x5EED_CAFE_F00D_0001);
    for case in 0..CASES {
        let pattern = gen_pattern(&mut rng, true);
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
            assert_eq!(
                a, b,
                "case {case}: divergence on pattern {pattern:?}, text {text:?} (ours = left)"
            );
        }
    }
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
        let pattern = gen_pattern(&mut rng, false);
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
