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

**The fast path (`LookaroundProg::simple`).** A lookaround body that's a
single atom — `(?=x)`, `(?<=[0-9])`, `(?=.)` — is checked directly
against the one adjacent char (the char at the position itself for
lookahead, the char immediately before it for lookbehind), with no
nested VM invocation at all: no `Scratch` allocation, no `exec_bool`
call, just a char/class comparison, folded and `REG_NEWLINE`-adjusted at
compile time the same way an ordinary `Inst::Char`/`Inst::Class`/
`Inst::AnyChar` is. This covers the common case of asserting a single
class or literal char next to something else — including the case that
originally showed an 18× overhead (see the benchmark below) — and was
added after the initial, always-nested implementation revealed that cost.

**The fallback (everything else).** A lookaround body that isn't a
single atom (multi-char, alternation, nested lookaround, `Empty`, and so
on) compiles to its own independent, nested `Program` — literally a full
recursive call back into the compiler (`compile_impl` with
`force_anchored: true`, see `compile.rs`). "Anchored" here means "must
match starting exactly at the position it's invoked from," not POSIX's
`^`-at-buffer-start sense — the sub-program has no unanchored-search
prefix, so it's checked at one position, not searched for. At VM time,
hitting a `Lookahead`/`Lookbehind` instruction whose `simple` is `None`
runs this sub-program as a plain boolean check (`vm::exec_bool`) at the
right position — the position itself for lookahead, `len` chars back
(the validated fixed length) for lookbehind — and the thread survives
only if the result matches the assertion's polarity.

This fallback path remains deliberately unoptimized — it's what's left
after the fast path above was added, not a from-scratch redesign:

- **A fresh `Scratch` is allocated per fallback check** — no
  thread-local reuse, unlike the top-level `is_match`/`find`/`captures`
  entry points (see `lib.rs`'s `SCRATCH` thread-local). Nested lookaround
  checks happen inside the *outer* VM's own epsilon-closure walk, which
  already holds the outer `Scratch`'s buffers borrowed; reusing a shared
  buffer there would need either a second buffer pool or interior
  mutability that would compromise `Regex: Send + Sync`. Allocating
  fresh is correct and simple; it is also exactly the kind of avoidable
  cost the benchmark below is measuring for the patterns that still hit
  this path (e.g. the 3-deep nested-lookahead row, or any multi-char
  lookaround body).
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
| `[0-9]+(?=x)` (no match, full scan) | 56µs | 57µs | 1.0× |
| `(?<=[0-9])x` (no match, full scan) | 262µs | 57µs | **4.6×** |
| `[0-9]+(?=x)` (`find_iter`, real matches) | 2.7ms | 3.0ms | 0.9× |
| `a(?=b(?=c(?=d)))` (3-deep nested) | 1.1ms | 863µs | 1.3× |
| `^[a-z]+(?=[0-9])` (short string, per-call) | 249ns | 217ns | 1.1× |

These are the numbers *after* adding the single-atom fast path
(`LookaroundProg::simple`, see above) — the table below the fold shows
what the always-nested first cut looked like, since the delta is the
interesting part of the answer.

Takeaways:

- **When the pattern's scan-hint machinery still applies (a mandatory
  literal/class *before* the lookaround), the overhead is within noise**
  (0.9×–1.0×): `head_class`/`collect_prefix` were made transparent to
  `Ast::Lookaround` (matching the existing transparency for
  word-boundary assertions), so `[0-9]+(?=x)` still fast-forwards the
  scan exactly like `[0-9]+x` does — the lookahead check itself only
  runs at the rare positions the hint identifies as candidates, and
  when it's a single-atom body (as here), that check no longer touches
  the VM at all.
- **When the lookaround sits *before* the mandatory literal instead**
  (`(?<=[0-9])x`, no scan hint of its own — a lookbehind can't be a
  "mandatory head" a scan hint fast-forwards to, since the hint would
  need to jump to a position *based on* text that comes before it, which
  is exactly what the lookbehind is checking), the fast path still cuts
  the cost from **~18× down to ~4.6×**: every position in the haystack
  now does one direct char/class comparison instead of a nested
  `Scratch` allocation + `exec_bool` call. The remaining 4.6× is the
  honest floor for this shape — checking one adjacent char per position,
  compared to the general VM's own per-position class check, which is
  already about as cheap as this engine gets.
- **Per-call fixed overhead is now negligible** (1.1× on a short
  string, down from 4.6×): the single-atom fast path has no allocation
  at all, so what's left is just the cost of one extra branch in the
  epsilon closure.
- **Nesting depth doesn't compound as badly as feared** (1.3× for 3
  levels): each level here is also a single-atom body, so all three take
  the fast path, and a failing inner assertion short-circuits before any
  deeper check ever runs.

**Bottom line for the "worth upstreaming?" question this branch exists
to answer:** with the single-atom fast path, lookaround is within noise
of its lookaround-free equivalent whenever an existing scan hint
survives past it, and a well-understood ~4.6× in the one case that has
no hint to inherit (a lookbehind gating a literal) — down from the
initial ~18×. Both real bugs the benchmark surfaced along the way (the
literal-fast-path correctness bug, and the suffix-quick-reject quadratic
blowup) are fixed and pinned by regression tests, and the fast path
itself is cross-checked against the general path in
`simple_lookaround_fast_path_matches_general_path`. The remaining
optimization opportunity — giving lookbehind a scan hint of its own so
`(?<=[0-9])x` doesn't need to visit *every* position, only "positions
after a digit" — would need the scan-hint machinery to reason about text
*before* the current position, which is a more involved change than
anything done so far; the 4.6× that's left is a reasonable place to stop
for this experiment.

<details>
<summary>First-cut numbers (always-nested, before the fast path)</summary>

| Pattern | lookaround | equivalent | × |
| --- | --- | --- | --- |
| `[0-9]+(?=x)` (no match, full scan) | 47µs | 49µs | 1.0× |
| `(?<=[0-9])x` (no match, full scan) | 839µs | 47µs | 17.8× |
| `[0-9]+(?=x)` (`find_iter`, real matches) | 3.9ms | 2.4ms | 1.6× |
| `a(?=b(?=c(?=d)))` (3-deep nested) | 1.3ms | 737µs | 1.8× |
| `^[a-z]+(?=[0-9])` (short string, per-call) | 979ns | 211ns | 4.6× |

</details>
