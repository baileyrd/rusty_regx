# rusty_regx

A minimal, linear-time [POSIX Extended Regular
Expression](https://pubs.opengroup.org/onlinepubs/9699919799/basedefs/V1_chap09.html)
engine in Rust, with **zero dependencies** and **no backtracking**.

Built to replace the `regex` crate in [`rush`](https://github.com/baileyrd/rush),
whose only regex consumer is the `[[ $s =~ pattern ]]` conditional — swapping
it in drops five crates from rush's dependency tree.

## Status

🚧 **Early scaffolding.** The public API and module layout are in place;
the parser, compiler, and VM are being built per the
[roadmap](DESIGN.md#roadmap).

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

Matching is an unanchored search (like bash `=~`). `Regex::new` uses
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

See [DESIGN.md](DESIGN.md) for the architecture and full roadmap.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
