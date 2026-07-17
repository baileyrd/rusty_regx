# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/).

## [Unreleased]

### Added

- `examples/grep.rs`; `Match::len`/`is_empty`; `Debug` for the iterator
  types; a REG_NEWLINE differential oracle vs the crate's `(?m)`;
  mode-randomized fuzzing with `find_iter` invariants; GitHub Release
  automation on tag push (body extracted from RELEASE_NOTES.md).
- `Match` now derives `PartialEq`, `Eq`, `Hash`, so matches can be
  deduplicated in a `HashSet` or compared directly.
- CI: a `cargo audit` job checks the dev-dependency and fuzz crate against
  published RustSec advisories, substantiating the "zero runtime
  dependencies" story end-to-end; a Miri job runs the lib unit tests
  (scoped there — the integration suite's linear-time timing assertions
  and bash-shelling differential oracle don't hold up under Miri's
  interpreter overhead).
- `RepetitionTooLarge` now carries a position when the offending interval's
  own bound is too large (`a{1001}`) — only the aggregate case (nested
  intervals whose combined expansion exceeds the program-size cap) stays
  positionless, since no single `{...}` is at fault there.

### Performance

- Scan hints now see through zero-width constructs (`\bword\b`:
  2.6ms → 3.1µs on a 96KB no-match — parity with the regex crate);
  class-headed patterns (`[0-9]+`, `\w+`) fast-forward via the ASCII
  class bitmaps (~40×); one-shot calls share a thread-local scratch
  (allocation-free after warmup); degenerate classes (`[a]`) compile to
  plain chars.
- Case-insensitive patterns now get the suffix quick-reject fast path too
  (previously `icase` disabled it entirely, even with a mandatory literal
  tail like `foo[0-9]+bar$`).
- ASCII-only case-insensitive literal patterns (`Regex::new_ci("qzj-lit")`)
  now take the substring fast path instead of always running the full VM;
  non-ASCII icase literals still fall back to the VM (Unicode case folding
  isn't byte-length-preserving).

### Fixed

- `is_match` (only) could report a false match for assertion-headed
  patterns when the scan fast-forward skipped: the boolean path carried
  a thread list embedding the old position's `\b` verdict. It now
  re-seeds after every skip, like the capture paths.
- A pattern with enough stacked quantifiers (`a****…`, `a{2}{2}{2}…`)
  built an arbitrarily deep `Repeat` chain with no cap, and could abort
  the process with a stack overflow — the existing group-nesting depth
  cap now also covers quantifier stacking (and any mix of the two).
- `` \` ``/`\'` (GNU absolute buffer anchors) were parsed identically to
  `^`/`$`, so under `REG_NEWLINE` mode they incorrectly matched at every
  line boundary instead of only the true start/end of the whole input.

## [0.4.0] — 2026-07-17

### Breaking

- GNU/glibc ERE extensions ([#18]): `\w` `\W` `\s` `\S` are word/space
  classes, `\b` `\B` `\<` `\>` word assertions, `` \` `` `\'` input
  anchors, and `{,n}` means `{0,n}` — matching what bash's glibc
  `regcomp` does (each verified against bash 5.2; previously these were
  literals or errors, silently diverging from bash). Quantifying an
  assertion directly (`\b*`) is now an error, as in glibc. Other
  escapes (`\d`) stay literal, as in glibc.
- The bash-oracle differential generators now emit these constructs;
  two hand-confirmed glibc submatch nonconformances are recorded in the
  known-quirks list (group 0 always agrees).

### Added

- Degenerate collating symbols `[.c.]` and equivalence classes `[=c=]`
  ([#19]): accepted as bash does, including `[[.a.]-c]` ranges;
  `[[=a=]-c]` and multi-char collating names stay errors, as in glibc.
- `Regex::builder()` with `posix` / `case_insensitive` / `newline`
  options; `newline(true)` is POSIX `REG_NEWLINE` line matching ([#21]).
- `Regex::debug_dump()`: mode, chosen execution tier, extracted
  prefix/suffix, instruction listing; unstable output format ([#24]).
- `docs/FLAVORS.md` (differences vs PCRE / the regex crate, each pinned
  by a test) and `docs/COOKBOOK.md` backed by `tests/cookbook.rs`
  ([#22], [#23]); `docs/GLOB_DESIGN.md` proposal ([#20]);
  `RELEASE_NOTES.md`.

[#18]: https://github.com/baileyrd/rusty_regx/issues/18
[#19]: https://github.com/baileyrd/rusty_regx/issues/19
[#20]: https://github.com/baileyrd/rusty_regx/issues/20
[#21]: https://github.com/baileyrd/rusty_regx/issues/21
[#22]: https://github.com/baileyrd/rusty_regx/issues/22
[#23]: https://github.com/baileyrd/rusty_regx/issues/23
[#24]: https://github.com/baileyrd/rusty_regx/issues/24

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
