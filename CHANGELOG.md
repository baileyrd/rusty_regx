# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/).

## [Unreleased]

### Added

- `Regex::is_match`: boolean matching without capture tracking — faster
  than `captures`, one shared fast path across all modes.
- `Regex::new_ci`: leftmost-first case-insensitive mode, completing the
  `{leftmost-first, POSIX} × {sensitive, insensitive}` constructor matrix.
- `Regex::as_str`, `Display`, `Clone`, and a `Debug` impl that shows the
  pattern instead of compiled bytecode.
- `Error::position()`: parse errors now report the 0-based char offset of
  the offending construct; `Display` appends `at position N`.
  `Error::kind()` (the new `ErrorKind` enum) carries what went wrong.
- Fuzz targets (`fuzz/`): parse robustness, exec robustness with
  cross-mode invariants, and boolean differential against the `regex`
  crate; CI runs a smoke pass per change.
- Benchmarks against the `regex` crate (`cargo bench`).
- CI verifies the declared MSRV (1.75).

### Changed

- `Error` is now a struct (`kind()` + `position()`) instead of a bare
  enum; match on `ErrorKind` instead of `Error` variants.
- Pike VM capture slots are copy-on-write; measured ~17% faster
  leftmost-first captures, ~34% faster POSIX captures, and ~62% faster
  boolean matching on full-scan workloads.

### Fixed

- A pattern with very deeply nested groups (e.g. hundreds of thousands of
  open parens) overflowed the parser's stack and aborted the process.
  Nesting is now capped at 250 levels (`ErrorKind::NestingTooDeep`).

## [0.1.0] — unreleased scaffold

Initial engine: POSIX-ERE parser, bytecode compiler, Pike VM with
captures, POSIX leftmost-longest mode (`new_posix`), case-insensitive
mode (`new_posix_ci`), `escape()`, and the differential harness against
the `regex` crate and a live bash oracle.
