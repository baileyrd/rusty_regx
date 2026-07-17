# POSIX-ERE cookbook

Common patterns written in this engine's dialect (glibc-style POSIX
ERE). The biggest arrival shock from PCRE is that `\d` is a literal
`d` — use `[0-9]` or `[[:digit:]]`; `\w`, `\s`, and `\b` *do* work
(GNU extensions, as in bash).

Every pattern below is asserted against its examples by
`tests/cookbook.rs` — the cookbook cannot rot.

| Task | Pattern | Matches | Rejects |
| --- | --- | --- | --- |
| Integer | `^[+-]?[0-9]+$` | `42`, `-7` | `1.5`, `""` |
| Decimal number | `^[+-]?[0-9]+(\.[0-9]+)?$` | `3.14`, `-2` | `.5`, `3.` |
| Hex number | `^0[xX][[:xdigit:]]+$` | `0xCAFE` | `0x`, `CAFE` |
| Identifier | `^[A-Za-z_]\w*$` | `_foo1` | `1foo` |
| Whole word | `\bword\b` | `a word.` | `password` |
| Email (pragmatic) | `^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$` | `bob@x.com` | `bob@x`, `@x.com` |
| IPv4 (loose) | `^([0-9]{1,3}\.){3}[0-9]{1,3}$` | `10.0.0.1` | `1.2.3` |
| ISO date | `^[0-9]{4}-[0-9]{2}-[0-9]{2}$` | `2026-07-17` | `17/07/2026` |
| Quoted string | `"[^"]*"` | `say "hi" now` | `say hi` |
| Trim (capture core) | `^[[:space:]]*(.*[^[:space:]])[[:space:]]*$` | ` hi there ` → `hi there` | — |
| key=value | `^([A-Za-z_]\w*)=(.*)$` | `PATH=/bin` | `=x` |
| Version string | `^([0-9]+)\.([0-9]+)\.([0-9]+)$` | `1.2.3` | `1.2` |

Notes:

- Anchor with `^`/`$` when validating a whole string; matching is
  otherwise an unanchored *search*, like bash `=~`.
- For "loose" recipes (IPv4 above), tighten in code from the captures —
  ERE has no way to express `0–255` compactly, and trying makes
  unreadable patterns.
- Under `shopt -s nocasematch` semantics, compile with
  `Regex::new_posix_ci`; captures always keep the original case.
- See [`docs/FLAVORS.md`](FLAVORS.md) for what to un-learn from PCRE.
