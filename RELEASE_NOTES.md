# Release Notes

What's new in each release of `rusty_regx`, in human terms. For the
technical changelog, see [CHANGELOG.md](CHANGELOG.md); for how to use
anything mentioned here, see the [cookbook](docs/COOKBOOK.md).

---

## Unreleased

**Shell glob matching, part 1 of #20.** The first piece of `fnmatch`-style
glob support (`?`, `*`, `[...]`, `[!...]`), built on the same linear-time
engine as ERE matching — see `docs/GLOB_DESIGN.md` for the full plan.

### ✨ New

- `Glob`/`GlobBuilder`: compile a shell glob pattern and test it against a
  whole string with `Glob::matches`. Unlike `Regex`, matching is always a
  full match, never a substring search — glob patterns describe an entire
  name. Translates directly onto the existing AST, so it gets the same
  execution tiers (and the same no-backtracking guarantee) as ERE
  patterns for free.
- Bracket expressions (`[abc]`, `[a-z]`, `[[:digit:]]`, collating/
  equivalence forms) work exactly as in `Regex`, plus glob's `!` as an
  alternative negation marker alongside POSIX `^` (`[!abc]` and `[^abc]`
  are equivalent).
- The bash extglob operators: `@(a|b)` (exactly one alternative), `*(p)`
  (zero or more), `+(p)` (one or more), `?(p)` (zero or one) — nest freely
  (`@(a|@(b|c))`) and compose with everything else (`file.@(txt|md)`).
  Same nesting-depth cap as ERE groups, and the same guarantee: no
  backtracking, however the alternatives are arranged.
- `!(p)` negation — restricted to the *whole* pattern for now (e.g.
  `"!(foo|bar)"`), per the design doc's v1 plan. Since glob matching is
  always full-string, a whole-pattern negation is just "does `p` match?
  flip the answer" — no new matching machinery needed. `!(p)` anywhere
  else (embedded in a larger pattern, or nested inside another extglob
  group) is a clear compile error rather than a silent wrong parse; the
  general embedded case is future work.
- `GlobBuilder::pathname`: `?`, `*`, and bracket expressions never match
  `/` — only a literal `/` in the pattern matches one in the input, the
  way real pathname expansion works (`*.txt` doesn't match
  `dir/notes.txt`, but `dir/*.txt` does). Applies uniformly through the
  whole pattern, including inside `!(p)` negation and extglob groups.
- `GlobBuilder::period`: a `.` at the very start of the matched string
  only matches an explicit literal `.` in the pattern — `*`/`?`/brackets
  never match a leading dot, matching bash's default "hidden files
  aren't globbed" behavior. Restricted v1: the pattern's opening
  construct has to be a plain char, `?`, or bracket expression; a
  pattern *starting* with `*` or an extglob group is a compile error
  rather than a silent under-restriction (write an explicit leading
  `.*` if dotfiles should match too).

Case-insensitivity and prefix/suffix matching (`${var#pat}` and friends)
land in follow-up rounds — see #20.

---

## 0.5.0 — July 18, 2026

**Hardening and a POSIX-mode performance gap closed.** No API changes —
this release is about a crash fixed, a mode brought back up to speed,
and cheaper cloning.

### 🐛 Fixed

- A pattern with enough stacked quantifiers (`a****…`, `a{2}{2}{2}…`)
  could crash the process with a stack overflow — closes the same class
  of bug 0.2.0 fixed for nested groups, reached this time through `*`
  instead of `(`.
- `` \` ``/`\'` (GNU absolute buffer anchors) incorrectly matched at
  every line boundary under line-mode matching, instead of only the
  true start/end of the whole input.
- `is_match` (only) could report a false match for `\b`-style patterns
  right after the scanner skipped ahead to a promising position.

### ⚡ Faster

- **`Regex::new_posix`'s class-headed patterns** (`[0-9]+`, `\w+`, and
  friends) now get the same scan-forward speedup `Regex::new` already
  had — about 150x faster on a large no-match in local testing. POSIX
  mode had silently fallen behind on exactly this shape of pattern.
- **`Regex::clone()` is instant** now, instead of copying the whole
  compiled pattern — clone and share a `Regex` freely, the way you
  would with the `regex` crate.
- Patterns that repeat the same bracket expression (`[0-9]{100}`, or
  the same class reused across `a|b` branches) compile faster and take
  less memory — the class is compiled once and shared, not rebuilt at
  every occurrence.
- Case-insensitive literal patterns (`Regex::new_ci(...)`) scan a bit
  faster over large no-match text.

### 🛠️ Under the hood

- `debug_dump()` now shows the class-head scan hint too, not just the
  literal prefix/suffix — one less blind spot when asking "why is this
  pattern slow?".
- `cargo audit` and Miri now run in CI on every change, and the
  benchmark suite gained a check that would have caught the POSIX-mode
  slowdown above automatically.

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
