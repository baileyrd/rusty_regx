//! Benchmarks against the `regex` crate (`cargo bench`).
//!
//! The crate exists to replace `regex` in rush's `[[ =~ ]]` conditional,
//! so the workloads are rush-shaped: compile a user-supplied pattern, run
//! it once or a handful of times, mostly on short shell-sized strings —
//! plus full-scan and adversarial cases to track the engine's worst side.
//! Plain `std::time` instead of a bench framework, keeping dev-dependencies
//! to the `regex` crate alone.

use std::time::{Duration, Instant};

fn time<R>(iters: u32, mut f: impl FnMut() -> R) -> Duration {
    // One warm-up, then the average of `iters` runs.
    std::hint::black_box(f());
    let start = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(f());
    }
    start.elapsed() / iters
}

fn row(name: &str, ours: Duration, theirs: Duration) {
    println!("{name:<38} {ours:>12.1?} {theirs:>12.1?}");
}

fn main() {
    println!("{:<38} {:>12} {:>12}", "benchmark", "rusty_regx", "regex");

    // Compilation: shells compile every pattern fresh.
    let pat = "^([[:alpha:]]+)-([0-9]{2,4})$";
    row(
        "compile rush-shaped pattern",
        time(2_000, || rusty_regx::Regex::new(pat).unwrap()),
        time(2_000, || regex::Regex::new(pat).unwrap()),
    );

    // Short-string captures: the common `[[ =~ ]]` case.
    let ours = rusty_regx::Regex::new(pat).unwrap();
    let theirs = regex::Regex::new(pat).unwrap();
    row(
        "captures, short match",
        time(20_000, || ours.captures("release-2026").is_some()),
        time(20_000, || theirs.captures("release-2026").is_some()),
    );
    row(
        "captures, short no-match",
        time(20_000, || ours.captures("nope_2026").is_some()),
        time(20_000, || theirs.captures("nope_2026").is_some()),
    );

    // Full-scan no-match over a long haystack: the engine's worst side
    // (the regex crate's prefilters shine here; ours is a plain Pike VM).
    let text: String = "abc def ghi jkl mno pqr stu vwx ".repeat(3_000);
    let pat = "([[:alpha:]]+)-([0-9]{2,4})(x(y)(z))?";
    let ours = rusty_regx::Regex::new(pat).unwrap();
    let ours_posix = rusty_regx::Regex::new_posix(pat).unwrap();
    let theirs = regex::Regex::new(pat).unwrap();
    row(
        "captures, 96KB scan no-match",
        time(20, || ours.captures(&text).is_some()),
        time(20, || theirs.captures(&text).is_some()),
    );
    row(
        "captures, 96KB scan (POSIX mode)",
        time(20, || ours_posix.captures(&text).is_some()),
        Duration::ZERO, // no regex-crate equivalent
    );
    row(
        "is_match, 96KB scan no-match",
        time(20, || ours.is_match(&text)),
        time(20, || theirs.is_match(&text)),
    );

    // Anchored no-match on a large haystack: the anchored fast path skips
    // the scan entirely (rush's `=~` patterns are usually `^...$`-shaped).
    let ours_anchored = rusty_regx::Regex::new("^nope").unwrap();
    let theirs_anchored = regex::Regex::new("^nope").unwrap();
    row(
        "captures, ^-anchored 96KB no-match",
        time(2_000, || ours_anchored.captures(&text).is_some()),
        time(2_000, || theirs_anchored.captures(&text).is_some()),
    );

    // Literal-prefix fast-forward: rare first char in a big haystack.
    let ours_lit = rusty_regx::Regex::new("qz[0-9]+").unwrap();
    let theirs_lit = regex::Regex::new("qz[0-9]+").unwrap();
    row(
        "captures, literal-prefix 96KB no-match",
        time(2_000, || ours_lit.captures(&text).is_some()),
        time(2_000, || theirs_lit.captures(&text).is_some()),
    );

    // Pure-literal pattern (what escape() produces): substring fast path.
    let ours_word = rusty_regx::Regex::new("qzj-lit").unwrap();
    let theirs_word = regex::Regex::new("qzj-lit").unwrap();
    row(
        "captures, literal 96KB no-match",
        time(2_000, || ours_word.captures(&text).is_some()),
        time(2_000, || theirs_word.captures(&text).is_some()),
    );

    // ASCII icase literal: the fast path (byte-level ASCII case fold)
    // versus falling back to the VM (what non-ASCII icase literals still do).
    let ours_ci_lit = rusty_regx::Regex::new_ci("qzj-lit").unwrap();
    let theirs_ci_lit = regex::Regex::new("(?i)qzj-lit").unwrap();
    row(
        "captures, icase literal 96KB no-match",
        time(2_000, || ours_ci_lit.captures(&text).is_some()),
        time(2_000, || theirs_ci_lit.captures(&text).is_some()),
    );

    // Word-boundary pattern — the idiomatic GNU shape (the crate's \b is
    // Unicode-aware but identical over ASCII).
    let ours_wb = rusty_regx::Regex::new(r"\bword\b").unwrap();
    let theirs_wb = regex::Regex::new(r"\bword\b").unwrap();
    row(
        r"is_match, \bword\b 96KB no-match",
        time(20, || ours_wb.is_match(&text)),
        time(20, || theirs_wb.is_match(&text)),
    );

    // Class-headed pattern where the suffix quick-reject can't help
    // (every position has an 'x' nearby).
    let ours_cls = rusty_regx::Regex::new("[0-9]+x").unwrap();
    let theirs_cls = regex::Regex::new("[0-9]+x").unwrap();
    row(
        "is_match, [0-9]+x 96KB no-match",
        time(20, || ours_cls.is_match(&text)),
        time(20, || theirs_cls.is_match(&text)),
    );

    // Line-mode matching over many lines.
    let lines: String = "alpha beta\n".repeat(4_000);
    let ours_nl = rusty_regx::Regex::builder()
        .newline(true)
        .build("^beta")
        .unwrap();
    let theirs_nl = regex::Regex::new("(?m)^beta").unwrap();
    row(
        "is_match, ^beta line-mode 44KB",
        time(200, || ours_nl.is_match(&lines)),
        time(200, || theirs_nl.is_match(&lines)),
    );

    // Iteration throughput: count all numbers in a busy haystack.
    let nums: String = "id 4217 x 99 :: 7 ".repeat(3_000);
    let ours_it = rusty_regx::Regex::new("[0-9]+").unwrap();
    let theirs_it = regex::Regex::new("[0-9]+").unwrap();
    row(
        "find_iter, count [0-9]+ in 54KB",
        time(20, || ours_it.find_iter(&nums).count()),
        time(20, || theirs_it.find_iter(&nums).count()),
    );

    // Adversarial: catastrophic for backtrackers, must stay flat here.
    let a512 = "a".repeat(512);
    let ours = rusty_regx::Regex::new("(a+)+b").unwrap();
    let theirs = regex::Regex::new("(a+)+b").unwrap();
    row(
        "captures, (a+)+b on a^512",
        time(200, || ours.captures(&a512).is_some()),
        time(200, || theirs.captures(&a512).is_some()),
    );

    lookaround_benches(&text);
}

/// Lookaround is a `rusty_regx`-only extension (see `docs/LOOKAROUND.md`)
/// — the `regex` crate has no lookahead/lookbehind at all, so there's no
/// baseline to race against. Instead, each row compares a lookaround
/// pattern against the `rusty_regx`-only pattern that reaches the same
/// match decision *without* it (consuming the asserted text instead of
/// asserting it), to isolate lookaround's own overhead: each check
/// allocates a fresh `Scratch` and runs a full nested `exec_bool` call
/// (see the "How it's implemented" section of `docs/LOOKAROUND.md`) —
/// this is deliberately the simplest correct implementation, not an
/// optimized one, and these numbers are the evidence for whether that
/// simplicity is affordable enough to build on.
fn lookaround_benches(text: &str) {
    println!();
    println!(
        "{:<38} {:>12} {:>12} {:>8}",
        "lookaround (rusty_regx only)", "lookaround", "equivalent", "×"
    );
    let row2 = |name: &str, la: Duration, equiv: Duration| {
        let factor = la.as_secs_f64() / equiv.as_secs_f64().max(1e-12);
        println!("{name:<38} {la:>12.1?} {equiv:>12.1?} {factor:>7.1}x");
    };

    // Lookahead over a full no-match scan: `[0-9]+(?=x)` (assert the `x`)
    // vs `[0-9]+x` (consume it) — same match decision, only the mechanism
    // differs.
    let la = rusty_regx::Regex::new("[0-9]+(?=x)").unwrap();
    let equiv = rusty_regx::Regex::new("[0-9]+x").unwrap();
    row2(
        "is_match, [0-9]+(?=x) 96KB no-match",
        time(20, || la.is_match(text)),
        time(20, || equiv.is_match(text)),
    );

    // Lookbehind: `(?<=[0-9])x` vs `[0-9]x`.
    let la = rusty_regx::Regex::new("(?<=[0-9])x").unwrap();
    let equiv = rusty_regx::Regex::new("[0-9]x").unwrap();
    row2(
        "is_match, (?<=[0-9])x 96KB no-match",
        time(20, || la.is_match(text)),
        time(20, || equiv.is_match(text)),
    );

    // With real matches to find, not just a full no-match scan: every hit
    // still re-triggers the lookahead check during the scan itself, since
    // it gates whether a candidate position is a match at all.
    let mixed: String = "id 4217x id 99 done 7x more ".repeat(2_000);
    let la = rusty_regx::Regex::new("[0-9]+(?=x)").unwrap();
    let equiv = rusty_regx::Regex::new("[0-9]+x").unwrap();
    row2(
        "find_iter, [0-9]+(?=x) matches in ~60KB",
        time(50, || la.find_iter(&mixed).count()),
        time(50, || equiv.find_iter(&mixed).count()),
    );

    // Nested lookahead: cost should compound roughly linearly with depth,
    // since each level is its own independent Scratch allocation + exec_bool
    // call chained off the outer one.
    let la = rusty_regx::Regex::new("a(?=b(?=c(?=d)))").unwrap();
    let equiv = rusty_regx::Regex::new("a(?=bcd)").unwrap();
    row2(
        "is_match, 3-deep nested lookahead, 96KB",
        time(200, || la.is_match(text)),
        time(200, || equiv.is_match(text)),
    );

    // Short-string case: the common shell-conditional shape, where
    // per-call fixed overhead (the Scratch allocation) dominates more
    // than in a long-haystack scan.
    let la = rusty_regx::Regex::new("^[a-z]+(?=[0-9])").unwrap();
    let equiv = rusty_regx::Regex::new("^[a-z]+[0-9]").unwrap();
    row2(
        "is_match, ^word(?=digit) short string",
        time(20_000, || la.is_match("release2026")),
        time(20_000, || equiv.is_match("release2026")),
    );
}
