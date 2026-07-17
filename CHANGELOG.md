# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/).

## [0.1.0] — 2026-07-17

First tagged release: the complete engine, integrated as the `[[ =~ ]]`
backend of [`rush`](https://github.com/baileyrd/rush).

### Engine

- POSIX-ERE parser (full grammar including bracket corner cases),
  bytecode compiler, and Pike VM with captures — linear-time, zero
  dependencies, no `unsafe`.
- POSIX leftmost-longest mode (`Regex::new_posix`) and case-insensitive
  `REG_ICASE` modes (`Regex::new_posix_ci`, `Regex::new_ci`), verified
  differentially against the `regex` crate and live bash 5.2 oracles.
- Group-nesting depth cap (250): deeply nested user patterns report
  `ErrorKind::NestingTooDeep` instead of overflowing the stack.

### API

- `Regex::is_match` (capture-free fast path), `captures`, `as_str`,
  `Display`, `Clone`; `escape()` returns `Cow<str>`, borrowing input
  with no metacharacters.
- `Captures::get` / `span` (byte offsets) / `iter` / `len` and
  `Index<usize>`.
- Structured errors: `Error::kind()` (`ErrorKind`) plus
  `Error::position()` — the 0-based char offset of the offending
  construct; `Display` appends `at position N`.

### Performance

- Start-anchored patterns skip the unanchored scan prefix entirely
  (`^`-anchored no-match on 96KB: ~11ms → 123ns).
- Scans with a mandatory literal first char fast-forward via substring
  search (~17× on literal-headed scans).
- Copy-on-write capture slots; interned, range-normalized classes with
  binary-search membership; allocation-light `&str` parsing.

### Tooling

- Differential harness (2000 random cases × 4 oracles), fuzz targets
  (parse / exec invariants / regex-crate differential) with a seeded CI
  smoke job and a weekly unseeded deep-fuzz workflow, benchmarks vs the
  `regex` crate, MSRV (1.75) verification, and a crates.io package
  check in CI.
