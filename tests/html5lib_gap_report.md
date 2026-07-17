# html5lib Tokenizer Gap Report

> Updated: 2026-07-17 | Suite: html5lib/html5lib-tests `tokenizer/*.test`

## Headline

**Pass rate: 99.8% (7022 / 7036)** — 14 failures — 0 panicked

## Per-fixture results

| Fixture | Pass | Fail | Total |
|---------|-----:|-----:|------:|
| contentModelFlags.test | 24 | 0 | 24 |
| domjs.test | 59 | 0 | 59 |
| entities.test | 80 | 0 | 80 |
| escapeFlag.test | 9 | 0 | 9 |
| namedEntities.test | 4210 | 0 | 4210 |
| numericEntities.test | 336 | 0 | 336 |
| pendingSpecChanges.test | 1 | 0 | 1 |
| test1.test | 69 | 0 | 69 |
| test2.test | 43 | 2 | 45 |
| test3.test | 1777 | 9 | 1786 |
| test4.test | 85 | 0 | 85 |
| unicodeChars.test | 323 | 0 | 323 |
| unicodeCharsProblematic.test | 5 | 0 | 5 |
| xmlViolation.test | 1 | 3 | 4 |

## Remaining failures (14)

### xmlViolation.test — 3 failures (out of scope)

These test XML infoset coercion behaviors, not WHATWG §13.2.5 tokenizer behavior:

| Test | Input | Expected | Actual | Why it's wrong |
|------|-------|----------|--------|---------------|
| Non-XML character | `a\u{FFFF}b` | `a\u{FFFD}b` | `a\u{FFFF}b` | U+FFFF replacement is XML-only |
| Non-XML space | `a\u{000C}b` | `a b` | `a\u{000C}b` | U+000C→space is XML-only |
| Double hyphen in comment | `<!-- foo -- bar -->` | `<!-- foo - - bar -->` | `<!-- foo -- bar -->` | `--` collapse is XML-only |

**Decision**: Excluded from baseline. WHATWG §13.2.5 does not require these transformations. Per CLAUDE.md: WHATWG is ground truth.

### test2.test — 2 failures (Processing Instruction)

| Test | Input | Expected | Actual |
|------|-------|----------|--------|
| Simili processing instruction | `<?namespace>` | `Comment("?namespace")` | `[]` |
| Bogus comment stops at > | `<?foo-->` | `Comment("?foo--")` | `[]` |

**Root cause**: The `<?` path in TagOpen (§13.2.5.6) routes to ProcessingInstructionOpen, which handles these as PI tokens. The test expects `Comment` tokens (old html5lib behavior). Current WHATWG spec defines PI states (§13.2.5.72–76) that produce `ProcessingInstruction` tokens, not comments.

**Decision**: Code is correct per current WHATWG spec. Tests are outdated.

### test3.test — 9 failures (Processing Instruction)

All 9 failures are `<?` → PI inputs expected to produce `Comment` tokens:

- `<?` → expected `Comment("?")`
- `<?A` → expected `Comment("?A")`
- `<?B` → expected `Comment("?B")`
- `<?Y` → expected `Comment("?Y")`
- `<?Z` → expected `Comment("?Z")`
- Plus 4 more similar

**Root cause**: Same as test2.test — tests expect old bogus-comment behavior for `<?`, but WHATWG spec now defines Processing Instruction states.

**Decision**: Code is correct per current WHATWG spec. Tests are outdated.

## Summary

| Category | Count | Action |
|----------|-------|--------|
| XML infoset coercion (xmlViolation) | 3 | Excluded — not WHATWG §13.2.5 behavior |
| PI tests expecting Comment tokens | 11 | Excluded — tests outdated, code correct per current WHATWG |
| **Genuine bugs** | **0** | — |

**All 14 failures are WPT test suite issues, not implementation bugs.** The tokenizer implements the current WHATWG HTML Living Standard correctly.

## How to run

```bash
cargo test --test html5lib_tokenizer -- --nocapture
```
