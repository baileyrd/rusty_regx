# rusty_regx

A minimal, linear-time [POSIX Extended Regular
Expression](https://pubs.opengroup.org/onlinepubs/9699919799/basedefs/V1_chap09.html)
engine in Rust, with **zero dependencies** and **no backtracking**.

Built to replace the `regex` crate in [`rush`](https://github.com/baileyrd/rush),
whose only regex consumer is the `[[ $s =~ pattern ]]` conditional — swapping
it in drops five crates from rush's dependency tree.

## Status

✅ **Engine complete.** The parser, compiler, and Pike VM are implemented,
including the POSIX leftmost-longest mode (`Regex::new_posix`) and the
case-insensitive mode (`Regex::new_posix_ci`), and validated by a
differential harness against the `regex` crate and a live bash oracle.
The remaining [roadmap](DESIGN.md#roadmap) item is the rush integration
itself (step 4).

## Scope

POSIX ERE, nothing more:

| Supported | Not supported (by design) |
| --- | --- |
| Alternation `\|`, capturing groups `( )` | Backreferences, lookaround |
| Quantifiers `* + ?`, intervals `{m} {m,} {m,n}` | Lazy/possessive quantifiers |
| Anchors `^ $`, any-char `.`, escaped metachars | Named groups, Perl classes (`\d` `\w` `\b`) |
| Bracket expressions incl. `[^…]`, ranges, `[]a]`, `[a-]` | Unicode property classes |
| POSIX classes `[[:alpha:]]` … `[[:xdigit:]]` | Replacement APIs, streaming |

## Why no backtracking?

A shell compiles *user-supplied* patterns. The engine executes patterns on a
Pike VM (breadth-first NFA simulation with per-thread capture slots), so
matching is linear in the input — a pathological pattern like `(a+)+b` cannot
hang the shell. This is the one property of the `regex` crate that must not
regress.

## API

```rust
use rusty_regx::Regex;

let re = Regex::new(r"^([[:alpha:]]+)-([0-9]{2,4})$")?;
if let Some(caps) = re.captures("release-2026") {
    assert_eq!(caps.get(0), Some("release-2026"));
    assert_eq!(caps.get(1), Some("release"));
    assert_eq!(caps.get(2), Some("2026"));
}
```

Matching is an unanchored search (like bash `=~`). When only match/no-match
is needed, `is_match` skips capture tracking entirely and is faster than
`captures`. `Regex::new` uses
leftmost-first semantics, identical to the `regex` crate. `Regex::new_posix`
opts into POSIX leftmost-longest semantics — what real bash/glibc report:

```rust
use rusty_regx::Regex;

assert_eq!(Regex::new("a|ab")?.captures("ab").unwrap().get(0), Some("a"));
assert_eq!(Regex::new_posix("a|ab")?.captures("ab").unwrap().get(0), Some("ab"));
```

The POSIX mode's overall match (group 0) agrees with bash exactly across the
differential harness; submatch reporting follows the POSIX
longest-alternative rule, which glibc itself deviates from in rare corners
(~0.01% of randomly generated cases).

`Regex::new_posix_ci` adds case-insensitive matching on top of the POSIX
mode (`Regex::new_ci` is its leftmost-first counterpart) — POSIX `REG_ICASE`, which is what bash applies to `=~` under
`shopt -s nocasematch`. Folding happens per character at comparison time
(never in the captured text) and matches glibc exactly: literals and range
endpoints fold, and `[[:upper:]]`/`[[:lower:]]` both behave as
`[[:alpha:]]`:

```rust
use rusty_regx::Regex;

let re = Regex::new_posix_ci("^(a)(b)c$")?;
let caps = re.captures("ABC").unwrap();
assert_eq!(caps.get(1), Some("A")); // captures keep the original case
```

See [DESIGN.md](DESIGN.md) for the architecture and full roadmap.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
