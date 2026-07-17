# muskitty-html5-parser

A from-scratch HTML5 parser written in pure Rust, implementing the [WHATWG HTML Living Standard](https://html.spec.whatwg.org/) with zero runtime dependencies.

Part of the [MusKitty](https://github.com/bit-torch/MusKitty) browser engine project.

## Status

| Component | Spec Coverage | Test Pass Rate |
|-----------|--------------|----------------|
| **Tokenizer** (§13.2.5) | 85/85 states | [99.8%](https://github.com/html5lib/html5lib-tests) (7022/7036) |
| **Tree Construction** (§13.2.6) | 21/21 insertion modes | [100%](https://github.com/html5lib/html5lib-tests) (1716/1716) |

- Zero `unsafe` code
- Zero C/C++ dependencies
- Rust stable toolchain only
- html5lib-tests suite as ground truth

## Quick Start

```rust
use muskitty_html_parser::parse;

let document = parse("<!DOCTYPE html><html><head><title>Hello</title></head><body><p>World</p></body></html>");
// Returns an Rc<RefCell<Node>> DOM tree
```

## Architecture

```
muskitty-html5-parser/
  src/
    tokenizer/
      types.rs          Token, TagToken, DoctypeToken, State definitions
      trait_def.rs      Tokenizer trait (supports reentrancy)
      impls.rs          HtmlTokenizer — 85-state machine (~6000 lines)
      entities.rs       2231 WHATWG named character references
    parser/
      mod.rs            HtmlTreeConstructor entry point
      dispatch.rs       Insertion mode dispatcher (21 modes)
      helpers.rs        Scope checks, foster parenting, adoption agency
      insertion_mode.rs InsertionMode enum
      foreign.rs        SVG/MathML foreign content handling
    dom/                DOM node types (via muskitty-dom)
    lib.rs              Public API: parse() entry point
```

### Two-Stage Pipeline

```
Input codepoints → Tokenizer (§13.2.5) → Token stream → Tree Construction (§13.2.6) → DOM
```

1. **Tokenizer**: Deterministic state machine consuming Unicode codepoints, emitting Tokens (Doctype, Tag, Comment, Character, EOF, ProcessingInstruction).
2. **Tree Construction**: Consumes Tokens, applies insertion mode logic, builds DOM tree using open elements stack, active formatting elements, and foster parenting.

## What's Implemented

### Tokenizer (§13.2.5)

- All 85 tokenization states
- Content model switching (RCDATA, RAWTEXT, ScriptData, PLAINTEXT)
- Character reference resolution (named + numeric, decimal + hex)
- 2231 WHATWG named entities with binary search lookup
- Windows-1252 replacement table
- Processing instruction states
- CDATA section states (foreign content)
- Reentrant design: tree construction can pause/resume tokenizer

### Tree Construction (§13.2.6)

- All 21 insertion modes
- Adoption agency algorithm (formatting elements)
- Foster parenting (table context)
- Foreign content (SVG/MathML) with namespace handling
- Template insertion mode
- Reset insertion mode
- Scope checks (button, list, table, default)

## Building

```bash
cargo check                          # Workspace check (must be zero warnings)
cargo check -p muskitty-html-parser  # Parser crate only
```

## Testing

```bash
# Unit tests (145 tests)
cargo test -p muskitty-html-parser --lib

# html5lib tokenizer suite (7036 tests)
cargo test --test html5lib_tokenizer -- --nocapture

# html5lib tree construction suite (1920 tests, 1716 non-skipped)
cargo test --test html5lib_tree_construction -- --nocapture

# All tests
cargo test
```

### Test Fixtures

Tests use the [html5lib-tests](https://github.com/html5lib/html5lib-tests) suite:

- `tests/data/tokenizer/*.test` — 14 tokenizer fixture files
- `tests/data/tree_construction/*.test` — 68 tree construction fixture files

## Design Principles

1. **WHATWG is ground truth** — Implementation follows the spec exactly. WPT and Chromium are secondary references.
2. **Spec-compliant, not test-compliant** — Tests verify the code; code is never modified to pass a test unless the spec proves the test is wrong.
3. **Minimal dependencies** — Only `muskitty-dom` (sibling crate) and `serde_json` (dev-dependency for test fixtures).
4. **Zero unsafe** — Pure safe Rust.
5. **Surgical changes** — Every diff is as small as the task requires.

## Spec Reference

This implementation references:

- [WHATWG HTML Living Standard](https://html.spec.whatwg.org/) — Primary authority
  - §13.2.5: Tokenization
  - §13.2.6: Tree Construction
- [html5lib-tests](https://github.com/html5lib/html5lib-tests) — Test ground truth

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

Copyright 2026 MusCat / MusKitty Bit-Torch Community
