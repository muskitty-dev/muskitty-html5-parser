# html5lib Tokenizer Gap Report

> Generated: 2026-07-11 | Suite: html5lib/html5lib-tests `tokenizer/*.test`

## Headline

**Pass rate: 9.4% (658 / 7036)**

The harness ran every fixture to completion without panicking. The low rate
is dominated by **one architectural bug** (below), not by 80 individual
state-machine bugs. Fixing it is expected to move the rate sharply upward.

## Per-fixture results

| Fixture | Pass | Fail | Total |
|---------|-----:|-----:|------:|
| contentModelFlags.test | 7 | 17 | 24 |
| domjs.test | 10 | 49 | 59 |
| entities.test | 0 | 80 | 80 |
| escapeFlag.test | 0 | 9 | 9 |
| namedEntities.test | 0 | 4210 | 4210 |
| numericEntities.test | 0 | 336 | 336 |
| pendingSpecChanges.test | 0 | 1 | 1 |
| test1.test | 1 | 68 | 69 |
| test2.test | 1 | 44 | 45 |
| test3.test | 293 | 1493 | 1786 |
| test4.test | 19 | 66 | 85 |
| unicodeChars.test | 322 | 1 | 323 |
| unicodeCharsProblematic.test | 5 | 0 | 5 |
| xmlViolation.test | 0 | 4 | 4 |

**Strong areas** (already spec-correct):
- `unicodeChars.test` 322/323, `unicodeCharsProblematic.test` 5/5 — the
  numeric/hex character-reference end-of-range and Windows-1252 mapping is
  solid.
- `test3.test` 293/1786 — many of its 1786 cases *are* simple character
  emission; most fails there trace to the same root-cause bug below.

## Root cause #1 — `next_token` returning `None` prematurely (critical)

**Symptom**: a large fraction of failures show `actual: []` (zero tokens).

**Cause**: state handlers return `Option<Token>`. When a handler needs to
**switch state without emitting a token** (e.g. Data state sees `&` and
transitions to CharacterReference), it returns `None`. `next_token` then
returns that `None` to the caller, but the documented contract is
"`None` = stream exhausted after EOF". The consumption loop
`while let Some(t) = tok.next_token()` therefore stops on the first
state-only transition.

**Reproducer** (e.g. input `"&"`): Data state → `&` → set CharacterReference
→ return `None` → loop exits → `actual = []`, but expected `[Character("&")]`.

**Fix direction**: `next_token` must **loop internally** across state
transitions that don't emit, only returning to the caller when a real token
is produced (or EOF). This is the canonical tokenizer-driver shape. This is
the single highest-leverage fix and should be done before any per-state
debugging.

## Root cause #2 — input preprocessing not applied (§13.2.3.5)

The tokenizer's `new()` collects `str::chars()` directly. html5lib inputs
represent the *raw* input stream, which the spec preprocesses:
- `CRLF` → `LF`, lone `CR` → `LF`.
- U+000D handling in comment/bogus-comment etc. depends on this.

The harness currently applies CRLF→LF normalization itself as a workaround
(see `preprocess_input`), but `test_domjs` "CR in bogus comment state"
still expects `?\n` output, so the *tokenizer* must treat CR consistently
per §13.2.3.5. **Decision needed**: preprocess inside the tokenizer, or
document it as the caller's responsibility. Most reference implementations
preprocess inside.

## Root cause #3 — attribute entity resolution edge cases

`entities.test` 0/80. These test ambiguous-ampersand / attribute-context
entity handling (`&noti;`, `&lang=`, `&not=`). Since these are Tag-first
but `actual: []`, they are likely **also** blocked by root cause #1 (the
tag never gets emitted because an attribute-value state hit CharacterReference
and returned `None`). Re-test after #1 is fixed; residual failures will
then pinpoint genuine named-entity-in-attribute bugs.

## Root cause #4 — CDATA NUL handling

`domjs.test` "NUL in CDATA section": input `\0]]>` expected `Character("\0")`
but actual `Character("\u{FFFD}")`. The CDATA state (§13.2.5.69) does **not**
replace NUL with U+FFFD — only Data/RCDATA/RAWTEXT states do. Current CDATA
handler appears to be doing the replacement. Minor, but a real spec
divergence.

## Root cause #5 — xmlViolation (out of scope?)

`xmlViolation.test` expects "Coercing an HTML DOM into an infoset" tweaks
(replacing U+FFFF, U+000C→space, double-hyphen comment collapse). These are
**not** tokenizer behaviors per WHATWG §13.2.5 — they're infoset coercion.
MusKitty (per CLAUDE.md: WHATWG is ground truth) should **not** implement
these in the tokenizer. Recommend **excluding xmlViolation.test** from the
pass-rate baseline and documenting why.

## Recommended fix order

1. **Root cause #1** (next_token internal loop) — expected to flip thousands
   of cases green. Single commit, high ROI.
2. Re-run the suite, record the new baseline.
3. **Root cause #2** decision (where preprocessing lives) — then verify
   domjs CR/CRLF cases.
4. **Root cause #4** (CDATA NUL) — trivial fix.
5. Decide on **xmlViolation exclusion** — adjust harness to skip it.
6. Address residual per-state failures revealed after #1 lands.
7. **Error module**: once token-level behavior is correct, implement
   `ParseError` collection and compare `errors[]` arrays.

## How to run

```bash
cd muskitty-html-parser
cargo test --test html5lib_tokenizer -- --nocapture
```

The harness is informational: it prints the full report and never panics on
a mismatch. A hard pass-rate assertion can be added once the baseline is
healthy.
