# rusty_regx — design & roadmap

A minimal POSIX-ERE regex engine intended to replace the `regex` crate in
[`rush`](https://github.com/baileyrd/rush), whose only regex consumer is the
`[[ $s =~ pattern ]]` conditional (capability C56). Full dependency analysis:
`rush/docs/REGEX_DEPENDENCY_ANALYSIS.md`.

## Scope

The original plan (v0.1, historical — see "Current state and direction"
below for what's actually implemented today): POSIX ERE, nothing more.

- Alternation `|`, concatenation, capturing groups `( )` (ERE has no
  non-capturing groups), quantifiers `* + ?` and intervals `{m} {m,} {m,n}`.
- Anchors `^ $`, any-char `.`, backslash-escaped metacharacters.
- Bracket expressions: `[^...]`, ranges, literal `]` first (`[]a]`),
  trailing `-`, POSIX classes `[[:alpha:]]` … `[[:xdigit:]]`.

**Out of scope then, in scope now:** `\d`/`\w`/`\b` and the rest of the
GNU/glibc escape set were "out of scope" at v0.1 — bash's `regcomp` is
glibc, and real scripts rely on them, so they shipped as GNU extensions
(see below); **`\d` stays out**, since glibc has no such escape (`\d` is
literal `d`, matching bash).

**Still out of scope:** backreferences, lookaround, lazy/possessive
quantifiers, named groups, Unicode property classes, replacement APIs,
streaming.

## Public API (mirrors rush's exact usage)

```rust
pub struct Regex;            // compiled program
pub struct Captures<'t>;     // group 0 = whole match
pub enum Error;              // structured parse errors, Display'd by rush

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, Error>;
    // Later additions: new_posix / new_ci / new_posix_ci (semantics modes),
    // is_match (fast boolean path), as_str.
    pub fn captures<'t>(&self, text: &'t str) -> Option<Captures<'t>>;
}
impl<'t> Captures<'t> {
    pub fn len(&self) -> usize;
    pub fn get(&self, i: usize) -> Option<&'t str>;
}
pub fn escape(text: &str) -> String;  // escapes exactly THIS engine's metachars
```

## Architecture

```
&str pattern ─▶ ERE parser ─▶ AST ─▶ bytecode compiler ─▶ execution tiers
                                      Char / Class / Split / Jump / Save / Match
```

Compilation analyzes the AST once and picks the cheapest execution
strategy at match time (all tiers preserve identical semantics; each is
pinned by tests and fuzz invariants):

1. **Literal substring path** — a group-free pattern that is exactly its
   mandatory literal (optionally `^`/`$`-anchored; what rush's
   `escape()` produces) never touches the NFA: `find`/`starts_with`/
   `ends_with`/`==` do the whole job.
2. **Suffix quick reject** — any pattern with a mandatory literal suffix
   rejects non-matching text with one substring scan before VM work.
3. **Anchored fast path** — start-anchored patterns compile without the
   unanchored `.*?` prefix, so the thread list empties (and the search
   ends) as soon as position 0 fails.
4. **Prefix fast-forward** — when a mandatory literal prefix exists and
   no live thread carries progress, the scan skips straight to its next
   occurrence (`str::find`; a fold-and-compare scan in `REG_ICASE`
   mode, where the prefix is pre-folded).
5. **Pike VM** — everything else: breadth-first NFA simulation with
   copy-on-write capture slots, generation-stamped visited sets, interned
   classes with precomputed 128-bit ASCII membership, and one shared
   step dispatcher across the three modes (leftmost-first captures,
   POSIX leftmost-longest via best-wins closure, capture-free boolean).

**Non-negotiable: no backtracking.** A shell compiles user-supplied patterns;
the engine must be linear-time (Pike VM — breadth-first NFA simulation with
per-thread capture slots) so `(a+)+b` can never hang the shell. This is the
one property of the `regex` crate we must not regress on.

Other decisions:

- **Unanchored search** via an implicit non-greedy `.*?` prefix at the program
  start (bash `=~` is a search, not a full match).
- **Match semantics:** leftmost-first (Perl-style) in v1 — identical to the
  `regex` crate rush uses today, so the swap is behavior-neutral. POSIX
  leftmost-longest submatching (what real bash/glibc does; the hard,
  tagged-NFA part) is a v2 opt-in mode.
- **Input model:** iterate `char`s of the UTF-8 `&str`; classes are codepoint
  ranges; POSIX classes are ASCII-first with `char` method fallbacks. No
  Unicode tables.
- **Intervals** compiled by repetition with a size cap (error out past e.g.
  1000, like the crate's program-size limit).

## Roadmap

1. **Parser + AST + errors** — full grammar incl. bracket corner cases
   (`[]a]`, `[a-]`, `[^]]`, bad intervals → error). (~400–600 loc)
2. **Compiler + Pike VM, boolean matching** — no captures yet; linear-time
   adversarial tests from day one. (~500 loc)
3. **Captures** — `Save` slots on threads; unmatched optional groups report
   as absent (rush turns them into empty strings for `BASH_REMATCH`).
4. **`escape()`** and rush integration behind a branch (swap ~10 lines in
   `exec.rs` / `expand.rs`, drop 5 crates from the lock file). *(Done:
   rush depends on this crate via git; `Regex::new_posix` /
   `Regex::new_posix_ci` power `[[ =~ ]]` in `exec.rs`, `escape()` is
   used in `expand.rs`, and the `regex` crate is out of rush's lock
   file. Verified against rush's full test suite and a bash-parity
   acceptance script.)*
5. **Differential harness** — random patterns/inputs cross-checked against the
   `regex` crate (dev-dependency only) and a bash oracle script; port rush's
   C56 tests. Gate the swap on this.
6. **(v2) POSIX leftmost-longest mode** — closes the known `a|ab` divergence
   between rush and real bash. *(Done: `Regex::new_posix`. Implemented as a
   second VM execution mode that replaces priority-based disambiguation with
   capture-vector comparison over hidden repetition span tags, compared in
   syntactic pre-order. Polynomial worst case, never exponential.)*
7. **(v3) Case-insensitive mode** — `REG_ICASE` semantics for rush's
   `shopt -s nocasematch` + `=~`. *(Done: `Regex::new_posix_ci`. Follows
   glibc's model, verified differentially against bash 5.2: pattern
   literals, range endpoints, and input all fold to uppercase (simple,
   single-char — ASCII + Unicode); `[[:upper:]]`/`[[:lower:]]` both become
   `[[:alpha:]]`; ranges reversed after folding are errors; captures always
   report the original, unfolded input spans.)*

Estimated size: ~1.5–2.5k lines of engine + a comparable test volume.

## Current state and direction (post-roadmap)

The numbered roadmap above completed at v0.1; everything since is
tracked in [RELEASE_NOTES.md](RELEASE_NOTES.md) /
[CHANGELOG.md](CHANGELOG.md). Highlights beyond the original plan:

- The five-tier execution strategy described under Architecture
  (literal substring path → suffix quick reject → anchored fast path →
  prefix/class fast-forward → Pike VM).
- GNU/glibc extensions (`\w` `\s` `\b` `\<` …), degenerate collating
  forms, `REG_NEWLINE` line mode via `Regex::builder()` — each
  verified against bash 5.2 and pinned by the differential oracles
  (six suites, ~48k comparisons per run).
- The iteration API (`find_iter`/`captures_iter`), `find`,
  `debug_dump`, and the docs set (`docs/FLAVORS.md`,
  `docs/COOKBOOK.md`).

Open direction: running shell globs and extglob on this same engine —
see [docs/GLOB_DESIGN.md](docs/GLOB_DESIGN.md) (issue #20), which
would close the last backtracking matcher in rush.
