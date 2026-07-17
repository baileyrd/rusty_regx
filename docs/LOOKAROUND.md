# Lookaround (experimental, this branch only)

This branch (`claude/perl-lookaround`) adds Perl-style lookaround
assertions and non-capturing groups on top of the POSIX ERE + GNU/glibc
engine documented in DESIGN.md and FLAVORS.md. **It is not on `main`.**
The point of the branch is to answer a concrete question: what does
lookaround cost in an engine built around the "no backtracking, ever"
guarantee, and is a straightforward implementation fast enough to be
worth carrying upstream? See the benchmark section below for the answer
so far.

## Syntax

- `(?:...)` — non-capturing group (grouping without a capture slot).
- `(?=...)` — positive lookahead: matches if `...` matches starting right
  here, without consuming any input.
- `(?!...)` — negative lookahead: matches if `...` does *not* match here.
- `(?<=...)` — positive lookbehind: matches if `...` matches ending right
  here (i.e. the text immediately before this position).
- `(?<!...)` — negative lookbehind: the negation.

None of this is POSIX ERE — it's bolted on as a Perl-shaped extension,
the same way the GNU/glibc escapes (`\b`, `\w`, ...) are extensions to
the base grammar. Unlike those, lookaround has no glibc precedent to
match against; the semantics here follow Perl/PCRE's, restricted where
noted below.

## Restrictions

**Lookbehind is fixed-length only.** `(?<=foo)`, `(?<=a{3})`, and
`(?<=ab|xy)` (branches of equal length) all compile; `(?<=a+)`,
`(?<=a*)`, `(?<=a{2,})`, and `(?<=ab|c)` (branches of *different*
lengths) are all `ErrorKind::VariableLengthLookbehind`. This is the same
restriction older PCRE and Python's `re` shipped for years (Perl and
modern PCRE2 support variable-length lookbehind now, at real
implementation cost). The reason here is architectural: the VM never
backtracks and has no reverse-matching mode, so the only way to check
"does the body match ending exactly here" is to know how many chars back
to start an ordinary forward check from — which requires the length to
be a compile-time constant. Supporting variable length would mean either
trying every possible start position backward (expensive, and still not
enough on its own — see the alternation case) or adding a genuinely new
execution mode (a reversed NFA walking the text backward), which is a
different-sized project than this branch's experiment.

**Lookaround bodies don't capture.** `(a)(?=(b)c)` has exactly one real
capture group (`a`); the `(b)` inside the lookahead is parsed as plain
grouping, identical to `(?:b)`. This matches how most engines that
support lookaround at all treat it by default (Perl is the outlier in
exposing lookahead captures) and sidesteps a real design question — what
does it mean for a capture to "exist" when the enclosing assertion
doesn't consume, and the pattern can retry from many positions — that
doesn't need answering to evaluate the performance question this branch
exists to explore.

**Lookaround nesting is capped at 16**, separately from and much
stricter than the general structural-nesting cap (250, shared by group
parens and stacked quantifiers — see `parser.rs`'s `MAX_NESTING_DEPTH`).
This surprised the implementation: a naive port of the existing
250-deep cap to lookaround overflowed the stack outright (see
"Implementation notes" below). 16 is chosen with real margin down to a
256KB stack; legitimate patterns don't nest lookaround more than 2-3
deep, so this isn't a practical limitation.

## How it's implemented

Each lookaround body compiles to its own independent, nested `Program` —
literally a full recursive call back into the compiler (`compile_impl`
with `force_anchored: true`, see `compile.rs`). "Anchored" here means
"must match starting exactly at the position it's invoked from," not
POSIX's `^`-at-buffer-start sense — the sub-program has no
unanchored-search prefix, so it's checked at one position, not searched
for. At VM time, hitting a `Lookahead`/`Lookbehind` instruction runs that
sub-program as a plain boolean check (`vm::exec_bool`) at the right
position — the position itself for lookahead, `len` chars back (the
validated fixed length) for lookbehind — and the thread survives only if
the result matches the assertion's polarity.

This is about the simplest correct design available in this engine's
architecture, and deliberately not optimized:

- **A fresh `Scratch` is allocated per lookaround check** — no
  thread-local reuse, unlike the top-level `is_match`/`find`/`captures`
  entry points (see `lib.rs`'s `SCRATCH` thread-local). Nested lookaround
  checks happen inside the *outer* VM's own epsilon-closure walk, which
  already holds the outer `Scratch`'s buffers borrowed; reusing a shared
  buffer there would need either a second buffer pool or interior
  mutability that would compromise `Regex: Send + Sync`. Allocating
  fresh is correct and simple; it is also exactly the kind of avoidable
  cost the benchmark below is measuring.
- **No cross-check caching.** If a pattern has `(?=foo)` appearing where
  many threads visit the same position in the same step, each visit that
  reaches the `Lookahead` instruction re-runs the full sub-match. In
  practice the VM's per-pc visited-set dedup means at most one thread per
  pc per step reaches the instruction, so this is less costly than it
  sounds — but a position-keyed micro-cache (same idea as the top-level
  scan-hint fast-forward) is the obvious next optimization if the
  benchmark motivates it.
- **Lookaround sub-programs never use the literal substring fast path**
  (`compile_impl` disables `Program::literal` outright when
  `force_anchored` is set). This was a real bug caught during
  development, not a design choice made up front: the literal fast path
  only understands "anchored at true position 0" or "unanchored search
  from `from` onward" (see `vm::literal_span`) — neither matches "must
  match starting exactly at this arbitrary position," so `(?=bar)`
  against `"xbar"` incorrectly reported holding at position 0 (the
  unanchored literal search found `"bar"` at position 1 and treated that
  as good enough). Disabling the fast path for forced-anchored programs
  fixed it; the sub-program still runs the ordinary VM correctly, just
  without that one acceleration tier.
- **Lookaround sub-programs never compute the outer suffix quick-reject**
  either (`Program::suffix` stays empty when `force_anchored` is set) —
  a second real bug, found by the benchmark below rather than by
  inspection. That optimization scans from the check position to the
  *end of the whole haystack* looking for a mandatory literal tail — a
  good trade the *one* time a top-level search runs it, but ruinous for
  a sub-program invoked at *every* candidate position during an outer
  scan: `a(?=bcd)` against a 96KB haystack containing no `"bcd"` took
  roughly 1 second per call (each of ~3000 candidate `a` positions
  re-scanning the remaining haystack for a suffix that never occurs —
  quadratic overall) before this was disabled for forced-anchored
  programs; single-digit milliseconds after. `tests/lookaround.rs` pins
  both fixes with a wall-clock ceiling loose enough not to flake, tight
  enough to catch a regression back to quadratic behavior.

## Implementation notes: the stack-overflow bisection

Nested lookaround compiles via a real recursive function call per level
(`compile_impl` calling `Compiler::emit` calling `compile_impl` again for
the next level in), not a single flat instruction emission like an
ordinary group. Each level's stack frame carries a full `Compiler`, the
cloned sub-AST, and several `String`/`Vec` locals for the prefix/suffix/
class-hint computation — measurably heavier than a group's `self.emit`
recursion. `(?=(?=(?=...a...)))` at the same 250-deep cap that's safe for
nested parens overflowed the stack outright.

The actual per-level cost was bisected empirically by running the same
250-deep pattern inside `std::thread::Builder::stack_size`-bounded
threads: it survives an 8MB stack, but overflows at 2MB; narrowing
further, roughly 100 levels survive a 1MB stack, ~50 survive 512KB, and
~25 survive 256KB — consistently landing around 10KB of stack per
nesting level, on the order of 15-20x an ordinary group's per-level cost.
`MAX_LOOKAROUND_DEPTH = 16` was chosen to leave real margin even at the
low end of that range, on the assumption that pattern compilation
shouldn't be assumed to run on a generously-sized stack (a worker thread
in a thread pool, for instance, commonly gets less than the 8MB a
process's main thread does).

## Benchmark

`benches/compare.rs` measures each lookaround pattern against the
`rusty_regx`-only pattern that reaches the same match decision *without*
lookaround (consuming the asserted text instead of asserting it) — the
`regex` crate has no lookaround at all, so it isn't a valid baseline
here. Release-mode numbers (`cargo bench`), 96KB haystack unless noted:

| Pattern | lookaround | equivalent | × |
| --- | --- | --- | --- |
| `[0-9]+(?=x)` (no match, full scan) | 47µs | 49µs | 1.0× |
| `(?<=[0-9])x` (no match, full scan) | 839µs | 47µs | **17.8×** |
| `[0-9]+(?=x)` (`find_iter`, real matches) | 3.9ms | 2.4ms | 1.6× |
| `a(?=b(?=c(?=d)))` (3-deep nested) | 1.3ms | 737µs | 1.8× |
| `^[a-z]+(?=[0-9])` (short string, per-call) | 979ns | 211ns | 4.6× |

Takeaways:

- **When the pattern's scan-hint machinery still applies (a mandatory
  literal/class *before* the lookaround), the overhead is close to
  free** (1.0×–1.8×): `head_class`/`collect_prefix` were made transparent
  to `Ast::Lookaround` as part of this work (matching the existing
  transparency for word-boundary assertions), so `[0-9]+(?=x)` still
  fast-forwards the scan exactly like `[0-9]+x` does — the lookahead
  check itself only runs at the rare positions the hint identifies as
  candidates.
- **When the lookaround sits *before* the mandatory literal instead
  (`(?<=[0-9])x`), there's no such hint**, because a lookbehind can't be
  the "mandatory head" a scan hint fast-forwards to (the hint would need
  to jump to a position *based on* text that comes before it, which is
  exactly what the lookbehind itself is checking) — so every position in
  the haystack triggers a real sub-program check. Even with both real
  bugs above fixed, this case still costs ~18× the position-by-position
  literal/class check, which is the closest this branch gets to an
  "inherent" lookaround cost floor for the common case of asserting a
  single class right before a required literal.
- **Per-call fixed overhead is real but modest** (~800ns extra per call,
  ~4.6× on a short string where that overhead isn't amortized over a
  long scan) — consistent with "one extra `Scratch` allocation and
  `exec_bool` call", not something worse.
- **Nesting depth doesn't compound as badly as feared** (1.8× for 3
  levels, not ~3× or worse): a failing inner assertion short-circuits
  before any deeper nested check ever runs, so the common case (the
  assertion fails somewhere in the middle) doesn't pay for the full
  chain.

**Bottom line for the "worth upstreaming?" question this branch exists
to answer:** the naive implementation is close to free when an existing
scan hint survives past the lookaround, and a bounded, well-understood
~18× in the one case that has no hint to inherit — not a runaway cost,
and the two real bugs the benchmark surfaced (the literal-fast-path
correctness bug, and the suffix-quick-reject quadratic blowup) are now
both fixed and pinned by regression tests. The most valuable next
optimization, if this graduates past the experiment stage, is giving
lookbehind its own scan-hint story — perhaps precomputing a class hint
from the lookbehind body itself when it's fixed-length (which it always
is, by construction) — to close that 18× gap.
