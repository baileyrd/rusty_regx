# Design: shell patterns (fnmatch/glob + extglob) on the linear-time engine

Status: **design proposal** (issue #20). Not yet implemented.

## Why

Survey of rush (2026-07, `rush/src/glob.rs`, 553 lines): rush ships a
hand-rolled glob matcher used by `[[ == ]]` (`exec.rs`), pathname
expansion and the `${var#pat}`/`${var%pat}`/`${var/pat}` family
(`expand.rs`), `case`, and completion (`main.rs`). It is explicitly
backtracking: `*` "tries the tail at every remaining position" and
extglob alternatives are tried "backtracking-style" (its own comment).
That means patterns like `*(a|aa)*(a|aa)b` against a long run of `a`s
are exponential — **the exact failure mode rusty_regx was built to
eliminate for `=~`** still exists in rush for `==`, `case`, and
expansion. Nocasematch is handled by lowercasing both sides
(`exec.rs:256`), which differs from glibc's fold-to-upper `REG_ICASE`
model in the known `[A-_]`-style corners.

One engine for both pattern languages closes the DoS hole, unifies
case-folding semantics, and deletes rush's second matcher.

## Translation (glob → ERE AST)

Target the existing AST, not pattern strings — no escaping pitfalls,
and all existing execution tiers (literal path, prefix/suffix,
anchors, Pike VM) apply for free:

| Glob | AST |
| --- | --- |
| `?` | `AnyChar` |
| `*` | `Repeat(AnyChar, 0..)` |
| `[...]` / `[!...]` | `Class` (glob accepts `!` as negation alongside `^`; POSIX classes work unchanged) |
| literal `c`, `\c` | `Char(c)` |
| `@(p1\|p2)` | `Alternation` |
| `?(p)` / `*(p)` / `+(p)` | `Repeat(p, 0..=1 / 0.. / 1..)` |
| `!(p)` | see below |
| whole pattern | wrapped `^…$` (glob is a *full* match) |

Extglob nesting recurses naturally. The nesting-depth cap carries over.

## `!(p)` — negation without NFA complement

Glob matching is full-string matching, so `!(p)` standing alone is just
`!is_match(^p$)` — no automaton complement needed. Embedded occurrences
(`a!(b)c`) are the hard case. Proposal, following ksh semantics:

- Compile the pattern with each `!(p)` replaced by `.*` (its matching
  envelope), tracking the span of every `!(p)` via hidden tags (the
  machinery POSIX submatch tags already provide).
- On a candidate match, check each negated span does **not** match its
  `p` (anchored); if any does, reject and continue from the next
  candidate assignment. Iterating assignments uses the POSIX-mode
  best-wins closure with a "forbidden spans" refinement loop; each
  round is polynomial and the rounds are bounded by the number of
  distinct span assignments — worst case pseudo-polynomial, never the
  exponential of naive backtracking. (Simplification for v1: support
  `!(p)` only at top level or as the whole component, which covers the
  overwhelming majority of real usage; error otherwise.)

## API sketch

```rust
pub struct GlobBuilder { case_insensitive: bool, pathname: bool, period: bool }
impl GlobBuilder {
    pub fn build(&self, pattern: &str) -> Result<Glob, Error>;
}
impl Glob {
    pub fn matches(&self, name: &str) -> bool;          // full-string
    pub fn match_prefix(&self, s: &str, longest: bool) -> Option<usize>; // ${var#pat}/${var##pat}
    pub fn match_suffix(&self, s: &str, longest: bool) -> Option<usize>; // ${var%pat}/${var%%pat}
}
```

- `matches` = anchored `is_match` — boolean VM, linear time.
- `match_prefix`/`match_suffix` shortest/longest map to leftmost-first
  vs POSIX-longest machinery that already exists (`${var#pat}` is
  "shortest prefix", `##` "longest prefix" — exactly first-match vs
  longest-match at position 0).
- `pathname` mode (`*`/`?` not crossing `/`, bracket `/` rules) and
  leading-`period` rules compile in: `AnyChar` becomes `[^/]`, etc.
  rush's `glob()` directory walker stays in rush; only `match_component`
  is replaced.
- `case_insensitive` uses the engine's `REG_ICASE` fold — fixing the
  lowercase-both-sides divergence.

## Migration plan

1. Land `Glob` here with a differential harness against bash `==` /
   `case` (same oracle pattern as the regex harness; bash accepts
   extglob under `shopt -s extglob`).
2. Swap rush's `match_component` behind the same signature; port
   rush's `glob.rs` unit tests as acceptance tests.
3. Adversarial tests: `*(a|aa)`-style patterns that hang the current
   backtracker must finish instantly.
4. Delete `matches`/`match_extglob` from rush; keep its filesystem
   walker and quoting logic.

## Open questions

- Embedded `!(p)` general case: ship the restricted v1 or wait for the
  refinement loop? (Recommend restricted v1; bash scripts rarely embed.)
- `[!a-z]` vs `[^a-z]`: accept both in glob mode (bash does).
- Locale collation in glob brackets: same degenerate stance as the
  regex side (#19).
