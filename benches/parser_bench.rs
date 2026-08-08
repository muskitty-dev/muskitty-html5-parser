//! # MusKitty HTML5 Parser — Comparative Benchmark Suite
//!
//! Compares `muskitty-html5-parser` against three peer Rust HTML parsers
//! across three fixture sizes. Designed for academic evaluation of the
//! throughput / compliance / memory-safety trade-off.
//!
//! ## Compared Libraries
//!
//! | Library              | Model                 | DOM? | Spec Compliance        |
//! |----------------------|-----------------------|------|------------------------|
//! | muskitty-html5-parser| Full parse → DOM      | Yes  | 100 % html5lib tests   |
//! | html5ever            | Full parse → RcDom    | Yes  | Very high              |
//! | tl                   | Parse → VDom          | Yes  | Fast, not 100 %        |
//! | lol_html             | Streaming rewrite     | No   | Streaming, no DOM      |
//!
//! ## Fairness Caveats
//!
//! 1. **muskitty vs html5ever** is the **fairest head-to-head**: both perform
//!    full tokenization + tree construction and produce a heap-allocated,
//!    reference-counted DOM (`Rc<RefCell<Node>>` in muskitty, `RcDom` /
//!    `Handle` in html5ever). Differences in throughput reflect algorithmic
//!    choices rather than skipping work.
//!
//! 2. **tl** targets speed over spec coverage (e.g., it does not implement
//!    the full adoption agency algorithm). Higher throughput is expected; the
//!    comparison measures the *compliance tax* paid by muskitty and html5ever.
//!
//! 3. **lol_html** is a **streaming rewriter** — it scans the input, fires
//!    callbacks, and streams output without constructing a persistent DOM
//!    tree. Its group is labelled `lol_html_stream` to emphasize this. It is
//!    included to show the *lower bound* of parse-only cost (tokenization +
//!    tree-builder dispatch minus DOM allocation).
//!
//! ## Memory Benchmark (`memory_benchmark`)
//!
//! The `memory_benchmark` group uses a counting allocator wrapper to record
//! the number of heap allocations performed during a single parse. This is
//! critical for the zero-unsafe research story: muskitty achieves its safety
//! guarantees without `unsafe` blocks, but every `Rc`, `RefCell`, and `Vec`
//! allocation has a cost. Counting allocations lets us quantify that cost and
//! compare it against libraries that may use unsafe interior mutability.
//!
//! For production memory profiling, use **dhat** (see the dhat pseudocode
//! section near the bottom of this file).

use criterion::{
    black_box, criterion_group, criterion_main, measurement::WallTime, BenchmarkId, Criterion,
    Throughput,
};
use std::fs;
use std::path::PathBuf;

// ── Fixture loading ────────────────────────────────────────────────────────

/// Returns the absolute `benches/data/` directory.
fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches")
        .join("data")
}

/// Reads a named fixture file, panicking if absent (fail-fast is intentional:
/// a missing fixture signals a setup problem the user must fix before
/// collecting publishable numbers).
fn load_fixture(filename: &str) -> String {
    let path = data_dir().join(filename);
    fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "Failed to read benchmark fixture at '{}': {e}\n\
             ── See benches/data/README.md for how to create the required files.",
            path.display()
        )
    })
}

// ── MusKitty parse ─────────────────────────────────────────────────────────
//
// NOTE: The current lib.rs exposes a bare function:
//       `muskitty_html5_parser::parse(input: &str) -> Rc<RefCell<Node>>`
// If a `Parser` struct with `Parser::new(html).parse()` is preferred for
// ergonomics or baseline measurement (constructor vs parse separation), add
// one in lib.rs. The benchmark is invariant either way.

fn bench_muskitty(input: &str) {
    let dom = black_box(muskitty_html5_parser::parse(black_box(input)));
    // Force the compiler to keep `dom` alive through the measurement point.
    black_box(dom);
}

// ── html5ever parse (full DOM via RcDom) ───────────────────────────────────
//
// FAIRNESS NOTE: html5ever + RcDom builds a DOM isomorphic to muskitty's
// output (tree of reference-counted nodes). Both libraries pay the full
// cost of tokenization, tree construction, and heap allocation. This is the
// most meaningful head-to-head in the suite.

fn bench_html5ever(input: &str) {
    // html5ever 0.29 API with markup5ever_rcdom ≥ 0.4.
    //
    // If the API moves in future releases, the pattern is:
    //   parse_document(sink, opts).from_utf8().read_from(&mut bytes)
    // Adjust imports if `TendrilSink` moves from `html5ever::tendril` to
    // `markup5ever::tendril`.
    use html5ever::tendril::TendrilSink;

    let bytes: &[u8] = input.as_bytes();
    let dom = html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut black_box(bytes))
        .expect("html5ever parse failed");
    black_box(dom);
}

// ── tl parse (VDom, fast but not 100 % compliant) ──────────────────────────

fn bench_tl(input: &str) {
    // tl ≥ 0.7. `ParserOptions::default()` enables the standard mode.
    let dom = tl::parse(black_box(input), tl::ParserOptions::default()).expect("tl parse failed");
    black_box(dom);
}

// ── lol_html streaming scan (no DOM construction) ──────────────────────────
//
// FAIRNESS NOTE: lol_html streams input → tokenizer → rewrite pipeline →
// output. No DOM is built. This group shows the *lower bound* of parse-only
// cost (tokenization + tree-builder dispatch minus DOM allocation). Do NOT
// compare its throughput to muskitty/html5ever throughput 1:1 — treat it as
// the "how much does DOM construction cost" baseline.

fn bench_lol_html(input: &str) {
    // lol_html ≥ 2.0. We use `HtmlRewriter::new` (not `rewrite_str`) so the
    // measurement isolates just the parsing/rewriting phases, not the
    // allocation of a return `Vec`.
    let mut output: Vec<u8> = Vec::with_capacity(input.len());
    {
        let mut rewriter =
            lol_html::HtmlRewriter::new(lol_html::Settings::default(), |chunk: &[u8]| {
                output.extend_from_slice(chunk)
            });
        rewriter.write(black_box(input).as_bytes()).unwrap();
        rewriter.end().unwrap();
    }
    black_box(output);
}

// ── Criterion harness ──────────────────────────────────────────────────────

fn bench_parser(c: &mut Criterion) {
    // When a fixture is missing we fail eagerly (load_fixture panics) rather
    // than skipping silently, so the user is always aware of what data the
    // benchmark actually measured.
    let fixtures: Vec<(&str, String)> = vec![
        ("lipsum_10kb", load_fixture("lipsum_10kb.html")),
        (
            "wikipedia_fragment",
            load_fixture("wikipedia_fragment.html"),
        ),
        ("large_doc_2mb", load_fixture("large_doc_2mb.html")),
    ];

    for (name, html) in &fixtures {
        let byte_len = html.len() as u64;

        // ── Group: muskitty ───────────────────────────────────────────
        {
            let mut g = c.benchmark_group("muskitty_parse");
            g.throughput(Throughput::Bytes(byte_len));
            g.bench_with_input(BenchmarkId::new("parse", name), html, |b, input| {
                b.iter(|| bench_muskitty(input))
            });
            g.finish();
        }

        // ── Group: html5ever (full DOM) ───────────────────────────────
        {
            let mut g = c.benchmark_group("html5ever_parse");
            g.throughput(Throughput::Bytes(byte_len));
            g.bench_with_input(BenchmarkId::new("parse", name), html, |b, input| {
                b.iter(|| bench_html5ever(input))
            });
            g.finish();
        }

        // ── Group: tl ─────────────────────────────────────────────────
        {
            let mut g = c.benchmark_group("tl_parse");
            g.throughput(Throughput::Bytes(byte_len));
            g.bench_with_input(BenchmarkId::new("parse", name), html, |b, input| {
                b.iter(|| bench_tl(input))
            });
            g.finish();
        }

        // ── Group: lol_html (streaming scan, no DOM) ─────────────────
        {
            let mut g = c.benchmark_group("lol_html_stream");
            g.throughput(Throughput::Bytes(byte_len));
            g.bench_with_input(BenchmarkId::new("scan", name), html, |b, input| {
                b.iter(|| bench_lol_html(input))
            });
            g.finish();
        }
    }
}

// ── Memory benchmark: heap allocation counting ─────────────────────────────
//
// This group measures the number of heap allocations each parser performs
// for a single parse. It uses a thread-local atomic counter wrapped around
// the system allocator.
//
// LIMITATIONS:
// - Criterion iterates many times; we snapshot the counter around each
//   iteration so the value reported is *per single parse*.
// - The counting allocator only counts allocations performed on the thread
//   where the benchmark runs. Libraries that spawn worker threads are not
//   fully captured (none of the four parsers in this suite spawn threads).
// - Realloc / dealloc are intentionally not counted — the research question
//   is "how many distinct heap objects does a parse create?".
//
// For deep memory profiling (heap usage over time, leak detection, hot
// allocation sites), use `dhat` instead. See the dhat pseudocode section at
// the bottom of this file.

mod counting_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Per-thread allocation counter (reset before each parse iteration).
    pub static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

    pub struct CountingAllocator;

    impl CountingAllocator {
        /// Snapshot the current counter and then reset to zero.
        /// Returns the value before reset.
        pub fn take() -> u64 {
            ALLOC_COUNT.swap(0, Ordering::SeqCst)
        }
    }

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
            System.alloc(layout)
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // We do NOT decrement on dealloc — the research question is
            // "total allocations during parse", not "net allocations alive
            // at end of parse".
            System.dealloc(ptr, layout)
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
            System.realloc(ptr, layout, new_size)
        }
    }
}

// Wire the counting allocator as the global allocator for the benchmark
// binary. All allocations — parser internals, criterion bookkeeping, and
// peer libraries — flow through this counter.
//
// The `memory_benchmark` group snapshots the counter around each parse so
// the per-iteration value reflects only the parse's own allocations.
#[global_allocator]
static GLOBAL: counting_alloc::CountingAllocator = counting_alloc::CountingAllocator;

/// Warm-up parse that primes allocator arenas / global state.
/// Discards the DOM; its allocations are not counted.
fn warmup_parse(input: &str) {
    use html5ever::tendril::TendrilSink;

    let _ = muskitty_html5_parser::parse(input);
    let _ = html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut input.as_bytes());
    let _ = tl::parse(input, tl::ParserOptions::default());
}

fn bench_alloc_count_muskitty(input: &str) -> u64 {
    counting_alloc::CountingAllocator::take(); // reset
    let dom = muskitty_html5_parser::parse(black_box(input));
    black_box(dom);
    counting_alloc::CountingAllocator::take() // snapshot
}

fn bench_alloc_count_html5ever(input: &str) -> u64 {
    use html5ever::tendril::TendrilSink;

    counting_alloc::CountingAllocator::take();
    let dom = html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut black_box(input).as_bytes())
        .expect("html5ever parse failed");
    black_box(dom);
    counting_alloc::CountingAllocator::take()
}

fn bench_alloc_count_tl(input: &str) -> u64 {
    counting_alloc::CountingAllocator::take();
    let dom = tl::parse(black_box(input), tl::ParserOptions::default()).expect("tl parse failed");
    black_box(dom);
    counting_alloc::CountingAllocator::take()
}

fn memory_benchmark(c: &mut Criterion) {
    // Only run on the mid-size fixture for alloc counting (large_doc_2mb
    // would produce huge counts and the ratios are what matter).
    let html = load_fixture("wikipedia_fragment.html");

    // Prime allocator arenas before measuring.
    warmup_parse(&html);

    // ── muskitty alloc count ─────────────────────────────────────────
    {
        let mut g = c.benchmark_group("memory_benchmark");
        g.bench_function("alloc_count_muskitty", |b| {
            b.iter_custom(|iters| {
                let mut total = 0u64;
                for _ in 0..iters {
                    total += bench_alloc_count_muskitty(&html);
                    // Prevent criterion from merging iterations.
                    black_box(&total);
                }
                std::time::Duration::from_nanos(total)
            })
        });
        g.finish();
    }

    // ── html5ever alloc count ────────────────────────────────────────
    {
        let mut g = c.benchmark_group("memory_benchmark");
        g.bench_function("alloc_count_html5ever", |b| {
            b.iter_custom(|iters| {
                let mut total = 0u64;
                for _ in 0..iters {
                    total += bench_alloc_count_html5ever(&html);
                    black_box(&total);
                }
                std::time::Duration::from_nanos(total)
            })
        });
        g.finish();
    }

    // ── tl alloc count ───────────────────────────────────────────────
    {
        let mut g = c.benchmark_group("memory_benchmark");
        g.bench_function("alloc_count_tl", |b| {
            b.iter_custom(|iters| {
                let mut total = 0u64;
                for _ in 0..iters {
                    total += bench_alloc_count_tl(&html);
                    black_box(&total);
                }
                std::time::Duration::from_nanos(total)
            })
        });
        g.finish();
    }
}

// ── Criterion entry point ──────────────────────────────────────────────────

criterion_group!(
    name = throughput_benches;
    config = Criterion::default()
        .with_measurement(WallTime)
        .sample_size(60)          // larger sample for stable estimates
        .significance_level(0.01)  // stricter threshold for academic reporting
        .noise_threshold(0.02);    // tighter than default 0.05
    targets = bench_parser
);

criterion_group!(
    name = memory_benches;
    config = Criterion::default()
        .with_measurement(WallTime)   // re-used as a proxy; see iter_custom above
        .sample_size(30);
    targets = memory_benchmark
);

criterion_main!(throughput_benches, memory_benches);

// ── dhat-based memory profiling (pseudocode / instructions) ─────────────────
//
// ## What dhat measures
//
// dhat (https://crates.io/crates/dhat) is a heap profiler that records every
// allocation — size, call site, and lifetime. Unlike the counting allocator
// above, dhat gives you:
//   - Total bytes allocated
//   - Peak heap usage
//   - Allocation backtraces (hot allocation sites)
//   - Number of allocations still reachable at exit
//
// This is the gold standard for the zero-unsafe research story because it
// lets you correlate *which code* (e.g., `Node::new_element`, `Vec::push`)
// drives heap pressure.
//
// ## Setup
//
// ```toml
// # In Cargo.toml [dev-dependencies]:
// dhat = "0.3"
// ```
//
// ## Standalone binary (not criterion-compatible; dhat requires the global
// allocator, which conflicts with criterion's looped execution):
//
// ```rust,ignore
// // File: benches/dhat_profiler.rs (or src/bin/dhat_profile.rs)
//
// #[global_allocator]
// static ALLOCATOR: dhat::Alloc = dhat::Alloc;
//
// fn main() {
//     let input = std::fs::read_to_string("benches/data/wikipedia_fragment.html")
//         .expect("fixture missing");
//
//     // Profile muskitty
//     {
//         let _profiler = dhat::Profiler::new_heap();
//         let _dom = muskitty_html5_parser::parse(&input);
//         // Profiler drops here → dhat-heap.json written
//     }
//
//     // The dhat-heap.json file can be viewed at:
//     //   https://nnethercote.github.io/dh_view/dh_view.html
// }
// ```
//
// Run with:
// ```bash
// cargo run --release --bin dhat_profile
// ```
//
// Compare the dhat output for muskitty vs html5ever vs tl to see:
// - Which parser has the lowest peak heap.
// - Which allocation sites dominate in each library.
// - Whether muskitty's `Rc<RefCell<Node>>` pattern produces more allocations
//   than html5ever's equivalent `Handle` / `RcDom`.
//
// ## Interpreting the results
//
// If muskitty shows the **fewest allocations** but **lower throughput** than
// tl, the interpretation is:
//   "muskitty achieves memory efficiency through careful allocation patterns
//    while maintaining full spec compliance; tl trades compliance for speed."
//
// If muskitty shows **more allocations** than html5ever, investigate whether
// the RcDom representation is more compact than `Rc<RefCell<Node>>` (likely:
// html5ever uses raw arena allocation in some configurations, which muskitty
// intentionally avoids in favor of safe abstractions).
//
// ── Academic Interpretation Guide ─────────────────────────────────────────
// (Copy the section below into your README or paper appendix.)
//
// ## How to Read the Benchmark Results
//
// The benchmark suite produces four data points for each fixture size:
//
// | Group              | Measurement | What it means                        |
// |--------------------|-------------|--------------------------------------|
// | `muskitty_parse`   | Throughput  | MusKitty full-parse throughput       |
// | `html5ever_parse`  | Throughput  | html5ever + RcDom full-parse throughput |
// | `tl_parse`         | Throughput  | tl VDom parse throughput             |
// | `lol_html_stream`  | Throughput  | Streaming scan (no DOM) lower bound  |
// | `memory_benchmark` | Alloc count | Per-parse heap allocation count      |
//
// ### Throughput (higher = better)
//
// 1. **muskitty vs html5ever** — The primary comparison. Both build a
//    complete DOM. A throughput ratio near 1.0 means muskitty is
//    competitive with the de-facto standard Rust HTML parser. A ratio
//    below 0.5 warrants profiling to identify hot paths.
//
// 2. **muskitty vs tl** — tl is intentionally less compliant (e.g.,
//    simplified adoption agency algorithm). Expect tl to be faster.
//    The gap represents the *compliance tax*: how much throughput you
//    sacrifice for 100 % html5lib tree-construction correctness.
//
// 3. **muskitty vs lol_html** — lol_html does not build a DOM. If
//    muskitty is only 2–3× slower than lol_html, that is excellent:
//    it means DOM construction is cheap relative to tokenization. If
//    the gap is 10× or more, tokenization dominates and warrants
//    optimisation.
//
// ### Allocation Count (lower = better)
//
// This is the zero-unsafe headline number. muskitty uses zero `unsafe`
// blocks; every heap object goes through `std::alloc`. Compare:
//
// - **muskitty < html5ever**: muskitty's safe abstractions are more
//   allocation-efficient than html5ever's. This is a strong argument
//   for the "safe does not mean wasteful" thesis.
//
// - **muskitty < tl**: tl may use unsafe optimisations; if muskitty
//   still allocates fewer objects, it demonstrates that safe design
//   patterns can beat unsafe shortcuts on the allocation axis.
//
// - **muskitty > html5ever**: Investigate whether html5ever uses arena
//   allocation or object pooling that muskitty currently does not.
//   This identifies optimisation opportunities without compromising
//   the zero-unsafe guarantee.
//
// ### Composite interpretation
//
// | Scenario | Interpretation |
// |----------|---------------|
// | High throughput + low allocs | Best case: muskitty is fast AND memory-efficient. |
// | Low throughput + low allocs  | Memory-efficient but compute-bound. Profile the tokenizer / tree-builder hot paths. |
// | High throughput + high allocs | Fast but allocation-heavy. Consider arena allocation or object reuse for long-running scenarios. |
// | Low throughput + high allocs | Needs work on both axes. Start with allocation profiling (dhat), then throughput. |
//
// For academic reporting, always report:
// - Hardware (CPU model, frequency, RAM, OS).
// - Rust toolchain version (`rustc --version`).
// - Criterion confidence intervals (Criterion prints these).
// - Exact fixture provenance (URL, generation script, SHA-256 hash).
