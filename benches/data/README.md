# Benchmark Fixtures

Place the following three HTML files in this directory. They are read by
`benches/parser_bench.rs` at benchmark startup.

## Required Files

| File | Approx. Size | Purpose |
|------|-------------|---------|
| `lipsum_10kb.html` | ~10 KB | Heavy plain-text / `<p>` nodes — exercises the tokenizer's raw throughput with minimal tree construction overhead. |
| `wikipedia_fragment.html` | ~100 KB | Complex real-world DOM: nested `<div>`, `<table>`, `<ul>`, inline formatting, links, and mixed content — stresses the full parse pipeline (tokenizer + tree construction). |
| `large_doc_2mb.html` | ~2 MB | Large document — measures memory behaviour and sustained throughput under pressure. |

## How to Generate

### `lipsum_10kb.html`

A long plain-text document with minimal markup:

```bash
# On *nix:
python3 -c "
text = '<p>' + 'Lorem ipsum dolor sit amet, consectetur adipiscing elit. ' * 140 + '</p>'
with open('lipsum_10kb.html', 'w') as f:
    f.write('<!doctype html><html><head><title>Lipsum</title></head><body>' + text + '</body></html>')
"
```

### `wikipedia_fragment.html`

Download a Wikipedia article as a single-page HTML fragment:

```bash
curl -L -o wikipedia_fragment.html \
  "https://en.wikipedia.org/w/index.php?title=Web_browser&printable=yes"
```

Or use any complex real-world HTML page of 50–200 KB.

### `large_doc_2mb.html`

Generate a large document by duplicating content:

```bash
python3 -c "
import html
base = open('wikipedia_fragment.html').read()
with open('large_doc_2mb.html', 'w') as f:
    for _ in range(20):
        f.write('<section>' + base + '</section>\n')
"
```

Adjust the repeat count so the output is roughly 2 MB.

## Placeholder Files

The three `*.html.placeholder` files in this directory are minimal valid HTML
documents (~100 bytes each). They let `cargo check` succeed, but **benchmark
results with the placeholders are meaningless**.

Copy each placeholder to its target name for a quick sanity run, then replace
with real fixtures before collecting publishable data.
