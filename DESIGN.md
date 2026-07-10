# rusty_regx — design & roadmap

A minimal POSIX-ERE regex engine intended to replace the `regex` crate in
[`rush`](https://github.com/baileyrd/rush), whose only regex consumer is the
`[[ $s =~ pattern ]]` conditional (capability C56). Full dependency analysis:
`rush/docs/REGEX_DEPENDENCY_ANALYSIS.md`.

## Scope

POSIX ERE, nothing more:

- Alternation `|`, concatenation, capturing groups `( )` (ERE has no
  non-capturing groups), quantifiers `* + ?` and intervals `{m} {m,} {m,n}`.
- Anchors `^ $`, any-char `.`, backslash-escaped metacharacters.
- Bracket expressions: `[^...]`, ranges, literal `]` first (`[]a]`),
  trailing `-`, POSIX classes `[[:alpha:]]` … `[[:xdigit:]]`.

**Out of scope:** backreferences, lookaround, lazy/possessive quantifiers,
named groups, Perl classes (`\d`, `\w`, `\b`), Unicode property classes,
replacement APIs, streaming.

## Public API (mirrors rush's exact usage)

```rust
pub struct Regex;            // compiled program
pub struct Captures<'t>;     // group 0 = whole match
pub enum Error;              // structured parse errors, Display'd by rush

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, Error>;
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
&str pattern ─▶ ERE parser ─▶ AST ─▶ bytecode compiler ─▶ Pike VM
                                      Char / Class / Split / Jump / Save / Match
```

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
   `exec.rs` / `expand.rs`, drop 5 crates from the lock file).
5. **Differential harness** — random patterns/inputs cross-checked against the
   `regex` crate (dev-dependency only) and a bash oracle script; port rush's
   C56 tests. Gate the swap on this.
6. **(v2) POSIX leftmost-longest mode** — closes the known `a|ab` divergence
   between rush and real bash.

Estimated size: ~1.5–2.5k lines of engine + a comparable test volume.
