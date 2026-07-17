# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/).

## [0.3.0] — 2026-07-17

### Added

- `Regex::find_iter` / `Regex::captures_iter`: all non-overlapping
  matches, with the `regex` crate's exact empty-match rule (verified
  against it directly); VM buffers are reused across matches.
- `Regex::find` returning a `Match` (start/end/range/as_str): tracks
  only the overall match's offsets, at near-boolean cost in every mode.
- `Regex::group_count()`; `FromStr` for `Regex`.
- Doctests across the public API; differential oracles for
  `is_match`/`find` and the case-insensitive modes (`new_ci` vs the
  crate's `(?i)` over ASCII).
- Tag-triggered crates.io publish workflow (needs the
  `CARGO_REGISTRY_TOKEN` repository secret).

### Performance

- Pure-literal, group-free patterns — including exact repetitions like
  `a{3}` and everything `escape()` produces — bypass the VM via
  substring search, in all modes.
- Mandatory-literal-suffix quick reject: no-matches cost one substring
  scan before any VM work.
- The scan fast-forward prefix grows from one mandatory char to the
  longest mandatory literal string (663µs → 74µs on the literal-prefix
  benchmark), and now works in the case-insensitive modes via a
  pre-folded prefix.
- Classes carry precomputed 128-bit ASCII membership (one bit test per
  input char on the hot path).
- Generation-stamped visited/best sets drop the O(program) clear per
  input char; `compile()` no longer deep-clones the AST; the
  triplicated VM step dispatch is unified.

## [0.2.0] — 2026-07-17

Hardening, API, and performance pass on the 0.1.0 engine; now the
`[[ =~ ]]` backend of [`rush`](https://github.com/baileyrd/rush).

### Breaking

- `Error` is a struct: match on `Error::kind()` (`ErrorKind`) instead of
  `Error` variants; `Error::position()` reports the 0-based char offset
  of the offending construct and `Display` appends `at position N`.
- `escape()` returns `Cow<str>`, borrowing input with no
  metacharacters (call sites using `&escape(s)` compile unchanged).

### Added

- `Regex::is_match` (capture-free fast path), `Regex::new_ci`,
  `as_str`, `Display`, `Clone`.
- `Captures::span` (byte offsets), `Captures::iter`, `Index<usize>`.

### Fixed

- Deeply nested group patterns overflowed the parser's stack and
  aborted the process; nesting is now capped at 250
  (`ErrorKind::NestingTooDeep`).

### Performance

- Start-anchored patterns skip the unanchored scan prefix entirely
  (`^`-anchored no-match on 96KB: ~11ms → 123ns).
- Scans with a mandatory literal first char fast-forward via substring
  search (~17× on literal-headed scans).
- Copy-on-write capture slots; interned, range-normalized classes with
  binary-search membership; allocation-light `&str` parsing.

### Tooling

- Fuzz targets (parse / exec invariants / regex-crate differential)
  with a seeded CI smoke job and a weekly unseeded deep-fuzz workflow,
  benchmarks vs the `regex` crate, MSRV (1.75) verification, and a
  crates.io package check in CI.

## [0.1.0] — 2026-07-11

The original engine: POSIX-ERE parser (full grammar including bracket
corner cases), bytecode compiler, Pike VM with captures — linear-time,
zero dependencies, no `unsafe`. POSIX leftmost-longest mode
(`Regex::new_posix`) and case-insensitive `REG_ICASE` mode
(`Regex::new_posix_ci`), verified differentially against the `regex`
crate and live bash 5.2 oracles.
