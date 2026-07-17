# Flavor notes: this engine vs PCRE vs the `regex` crate

`rusty_regx` speaks **POSIX ERE as glibc implements it** — the dialect
bash's `[[ =~ ]]` actually uses — including glibc's GNU extensions.
Users arriving from PCRE or the Rust `regex` crate hit a handful of
real differences. Every row below is pinned by the differential test
harness or a fuzz-target filter (the file that enforces it is cited),
and the bash-facing rows are verified against live bash 5.2.

| Construct | This engine (glibc ERE) | `regex` crate / PCRE | Pinned by |
| --- | --- | --- | --- |
| `\d` | literal `d` | digit class | `tests/matching.rs` (`gnu_word_assertions_and_classes`) |
| `\w` `\s` `\b` `\<` `\>` | GNU word/space classes and word assertions | `\w`/`\s`/`\b` similar (Unicode defs; identical over ASCII); `\<` `\>` unsupported | `tests/differential.rs` generators |
| `\b*` (quantified assertion) | compile error, as glibc | crate: error; PCRE: varies | `tests/matching.rs` |
| `a+?` | optional stacked quantifier: `(a+)?` — matches empty | *lazy* plus — requires one `a` | `fuzz/fuzz_targets/differential.rs` filter |
| `[a[bc]d]` | `[` is a literal inside brackets | nested character class | fuzz filter (found by fuzzing) |
| `[a&&b]` `[a--b]` `[a~~b]` | literal chars | class set operations | fuzz filter |
| `[\w]` | backslash and `w`, both literal (POSIX bracket rule) | word class inside brackets | `tests/matching.rs` |
| `.` vs newline | matches `\n` (bash `=~` behavior); `RegexBuilder::newline` opts into `REG_NEWLINE` | crate: `.` excludes `\n` by default | `tests/differential.rs` module docs |
| `a^b`, `a$b` | anchors are anchors anywhere | mid-pattern anchors rejected/literal depending on flavor | differential generator constraint |
| `x{,3}` `x{,}` | GNU `{0,3}` / `*` | error | `tests/matching.rs` |
| `[[.a.]]` `[[=a=]]` | degenerate collating/equivalence forms accepted | error | `tests/matching.rs`, bash oracle classes |
| `(?:…)` | non-capturing group (Perl extension, this branch only — see [`docs/LOOKAROUND.md`](LOOKAROUND.md)) | supported | `tests/lookaround.rs` |
| `(?i)` `(?<name>…)` | not syntax | supported | grammar |
| Backreferences `\1` | never (linear-time guarantee) | PCRE yes; crate no | DESIGN.md |
| Lookaround `(?=` `(?!` `(?<=` `(?<!` | supported on this branch (Perl extension bolted onto ERE — see [`docs/LOOKAROUND.md`](LOOKAROUND.md)); lookbehind is fixed-length only | PCRE yes (variable-length too); crate no | `tests/lookaround.rs` |
| `{0,0}` intervals | groups keep POSIX numbering | crate elides trailing groups from `Captures::len` | `tests/differential.rs` generator note |
| POSIX classes on non-ASCII | Unicode `char` fallbacks (UTF-8-locale glibc) | crate POSIX classes are ASCII-only | README locale section, `posix_classes_are_unicode_not_c_locale` |
| Match semantics | leftmost-first by default; POSIX leftmost-longest via `new_posix` | crate/PCRE: leftmost-first only | `tests/matching.rs`, bash oracles |
| Literal optimizations returning later matches | never — leftmost is exact | crate prefilter can report a later-starting match | `crate_skipped_earlier_match` in the harness |

Two glibc quirks worth knowing when comparing against real bash: glibc
itself deviates from POSIX's longest-alternative submatch rule in rare
corners (~0.01% of random cases; group 0 always agrees). The confirmed
cases live in `known_glibc_submatch_quirk` in `tests/differential.rs`.

See also [`docs/COOKBOOK.md`](COOKBOOK.md) for common patterns written
in this dialect.
