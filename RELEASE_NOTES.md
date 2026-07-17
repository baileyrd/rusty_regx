# Release Notes

What's new in each release of `rusty_regx`, in human terms. For the
technical changelog, see [CHANGELOG.md](CHANGELOG.md); for how to use
anything mentioned here, see the [cookbook](docs/COOKBOOK.md).

---

## 0.4.0 — July 17, 2026

**Real bash patterns, out of the box.** This release makes the engine
speak the dialect bash scripts are actually written in, and adds the
tools to understand what your pattern is doing.

### ✨ New

- **GNU extensions, verified against bash**: `\b` word boundaries,
  `\w`/`\s` classes, `\<`/`\>` word edges, `` \` ``/`\'` input anchors,
  and `{,n}` intervals. Before this release, `[[ $x =~ \bfoo\b ]]`
  silently failed to match — the class of bug you don't notice until
  production. Every behavior was probed against bash 5.2 first, then
  wired into the differential test oracles so it can never regress.
- **Collating brackets**: `[[.a.]]` and `[[=a=]]` now work the way bash
  accepts them (including `[[.a.]-c]` ranges).
- **Line-mode matching**: `Regex::builder().newline(true)` gives you
  grep-style semantics — `.` stops at newlines, `^`/`$` match at line
  boundaries. The builder also composes all existing modes.
- **`debug_dump()`**: see exactly how your pattern compiled — which
  fast path it took, what literal prefix the scanner extracted, and the
  full instruction listing.

### 📚 Docs

- A [flavor guide](docs/FLAVORS.md): what to un-learn from PCRE, every
  difference pinned by a test.
- A [pattern cookbook](docs/COOKBOOK.md) whose every recipe is executed
  by the test suite — it cannot rot.
- A [design proposal](docs/GLOB_DESIGN.md) for running shell globs and
  extglob on this same linear-time engine (rush's current glob matcher
  is backtracking — the DoS class this engine eliminated for `=~`
  still lives there).

---

## 0.3.0 — July 17, 2026

**The engine got an iterator — and much faster eyes.**

### ✨ New

- **`find_iter` / `captures_iter`**: walk every match in a string, with
  the same empty-match rules as the `regex` crate (verified against it
  directly).
- **`find`**: just the match location, at near-boolean cost no matter
  how many groups the pattern has.
- `group_count()`, `"pattern".parse::<Regex>()`.

### ⚡ Faster

- Plain-text patterns (what `escape()` produces) skip the regex
  machinery entirely — they're a substring search now.
- Patterns with a required literal tail reject non-matching text with
  one scan.
- Case-insensitive matching gained the scan acceleration it never had.
- Character classes test ASCII input with a single bit lookup.

---

## 0.2.0 — July 17, 2026

**Hardening, speed, and a real API.**

### 🐛 Fixed

- A deeply-nested pattern (`((((…`) could crash the process with a
  stack overflow. Given that a shell compiles user-supplied patterns,
  this was the most important fix in the release. Found the day fuzzing
  was added; nesting is now capped like the `regex` crate.

### ✨ New

- `is_match`, `new_ci`, `Captures::span`/`iter`, pattern introspection
  (`as_str`, `Display`), `Clone`.
- Parse errors now point at the problem: `unclosed bracket expression
  at position 2`.

### ⚡ Faster

- `^`-anchored no-matches went from ~11 ms to 123 ns on large inputs
  (the common shell-pattern shape).
- Literal-prefix scanning, copy-on-write capture slots.

---

## 0.1.0 — July 11, 2026

**The engine exists.** A complete POSIX-ERE matcher — parser, compiler,
Pike VM — in ~2.5k lines with zero dependencies and no backtracking, so
no pattern can hang the caller. Includes the POSIX leftmost-longest
mode and `REG_ICASE` case-insensitivity, both differentially verified
against the `regex` crate and a live bash oracle. Built to be the
`[[ =~ ]]` engine of [rush](https://github.com/baileyrd/rush).
