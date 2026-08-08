//! # Memory Allocation Benchmark — HTML5 Parsers
//!
//! Standalone heap-allocation profiler for `muskitty-html5-parser` and its
//! peer libraries. Measures **allocation count** and **total bytes allocated**
//! for a single full parse (tokenization + tree construction + DOM).
//!
//! ## Why This Tool Exists
//!
//! The criterion-based `memory_benchmark` group in `benches/parser_bench.rs`
//! uses a custom counting allocator whose discrete output (integers) caused
//! plotters panics inside criterion's HTML report generator. This example
//! **does not link criterion and does not touch any plotting code** — it
//! outputs plain terminal numbers and a `dhat-heap.json` file for offline
//! flame-graph analysis.
//!
//! ## Quick Start
//!
//! ```bash
//! # Run on a single parser (must use --release for realistic numbers):
//! cargo run --release --example bench-memory -- muskitty
//! cargo run --release --example bench-memory -- html5ever
//! cargo run --release --example bench-memory -- tl
//! ```
//!
//! ## Output
//!
//! 1. **Terminal** — allocation count + total bytes in a formatted box.
//! 2. **dhat-heap.json** — full allocation trace. Upload to
//!    <https://nnethercote.github.io/dh_view/dh_view.html> for a flame graph.
//!
//! ## Academic Interpretation
//!
//! Low allocation counts are desirable for several reasons beyond raw speed:
//!
//! - **Formal verification** — fewer heap objects mean a smaller state space
//!   for tools like Kani or VeriFast.
//! - **WCET (worst-case execution time)** — allocator jitter is a dominant
//!   source of latency variance; fewer allocations reduce tail latency.
//! - **Cache locality** — compact heap layout improves L1/L2 hit rates.
//!
//! For the zero-unsafe research narrative: muskitty achieves full spec
//! compliance with zero `unsafe` blocks. If its allocation profile is
//! comparable to (or better than) parsers that use unsafe optimisations, it
//! demonstrates that **safe abstraction does not necessarily impose a
//! memory-overhead tax**.

use std::fs;
use std::path::PathBuf;

// ── dhat global allocator ───────────────────────────────────────────────────
// Every allocation in the entire process flows through this, giving us an
// omniscient view of heap behaviour. The `testing()` builder mode below
// unlocks `HeapStats::get()` for programmatic readout.

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Absolute path to `benches/data/`.
fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("benches")
        .join("data")
}

/// Reads `wikipedia_fragment.html`, printing a helpful error if missing.
fn load_fixture() -> String {
    let path = data_dir().join("wikipedia_fragment.html");
    fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!();
        eprintln!("❌ 无法读取测试文件:");
        eprintln!("   {}", path.display());
        eprintln!("   错误: {e}");
        eprintln!();
        eprintln!("   请先运行数据抓取脚本生成测试数据:");
        eprintln!("   bash scripts/fetch_bench_data.sh");
        eprintln!();
        std::process::exit(1);
    })
}

/// Human-readable byte count (binary SI prefixes).
fn human_bytes(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.2} KiB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

// ── Parser dispatch ─────────────────────────────────────────────────────────

/// Which HTML parser to benchmark.
#[derive(Clone, Copy)]
enum Parser {
    Muskitty,
    Html5ever,
    Tl,
}

impl Parser {
    fn from_arg(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "muskitty" => Self::Muskitty,
            "html5ever" => Self::Html5ever,
            "tl" => Self::Tl,
            other => {
                eprintln!();
                eprintln!("❌ 未知解析器: \"{other}\"");
                eprintln!();
                eprintln!("   用法: cargo run --release --example bench-memory -- [muskitty|html5ever|tl]");
                eprintln!();
                std::process::exit(1);
            }
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Muskitty => "muskitty-html5-parser",
            Self::Html5ever => "html5ever + RcDom",
            Self::Tl => "tl",
        }
    }
}

/// Execute a single full parse (tokenize + tree-construct + DOM) with the
/// chosen parser. The result is wrapped in `black_box` to prevent the
/// compiler from eliding the parse as dead code.
///
/// Three parsers, three strategies — but we measure the same work:
/// take a `&str`, produce a DOM tree, and hand control back.
fn parse(html: &str, parser: Parser) {
    match parser {
        // ── MusKitty ─────────────────────────────────────────────────────
        // `parse()` returns `Rc<RefCell<Node>>` — a full DOM with
        // reference-counted interior mutability. Zero unsafe blocks.
        Parser::Muskitty => {
            let dom = muskitty_html5_parser::parse(html);
            std::hint::black_box(dom);
        }

        // ── html5ever ────────────────────────────────────────────────────
        // The de-facto standard Rust HTML parser. We use `RcDom` (the
        // simplest bundled DOM sink) so the comparison is fair:
        // muskitty builds `Rc<RefCell<Node>>`, html5ever builds `RcDom`
        // (also Rc-based). Both perform full tokenization + tree
        // construction. No unsafe in the hot path (html5ever uses
        // `unsafe` in tendril internals but not in parser logic).
        Parser::Html5ever => {
            use html5ever::tendril::TendrilSink;

            let dom =
                html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default())
                    .from_utf8()
                    .read_from(&mut html.as_bytes())
                    .expect("html5ever parse failed");
            std::hint::black_box(dom);
        }

        // ── tl ───────────────────────────────────────────────────────────
        // tl prioritises speed over spec compliance (e.g., simplified
        // adoption agency algorithm). We use `ParserOptions::default()`
        // which enables the standard mode without SIMD acceleration,
        // matching our criterion benchmarks.
        //
        // NOTE: tl has a `simd` feature gate. We intentionally leave it
        // OFF (default) for a level playing field — muskitty does not
        // use SIMD either.
        Parser::Tl => {
            let dom = tl::parse(html, tl::ParserOptions::default()).expect("tl parse failed");
            std::hint::black_box(dom);
        }
    }
}

// ── Main ────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let parser = Parser::from_arg(args.get(1).map(|s| s.as_str()).unwrap_or("muskitty"));

    // ── Step 1: load fixture BEFORE any profiler ─────────────────────────
    // File I/O triggers heap allocations (String buffer, UTF-8 validation).
    // Loading before the profiler ensures those allocations are not counted
    // in the parser's allocation budget.
    let html = load_fixture();
    let file_size = html.len();

    // ── Step 2: warm-up (trigger lazy_static / codegen / arenas) ─────────
    // The first parse may initialise global data structures (interners,
    // atom tables, regex caches). We parse once without profiling so these
    // one-time costs are excluded from the measurement. The warm-up DOM is
    // dropped immediately — dhat does not record it.
    eprintln!("⏳ 预热中（排除惰性初始化开销）...");
    parse(&html, parser);

    // ── Step 3: profiled parse ───────────────────────────────────────────
    // `testing()` mode is the key: it unlocks `HeapStats::get()` so we can
    // read the allocation counters programmatically. Without testing mode
    // we would have to parse `dhat-heap.json` for the numbers.
    //
    // We take a baseline *after* creating the profiler but *before* the
    // parse, then a final snapshot *after* the parse. The difference is
    // exactly the parse's own allocations.
    let _profiler = dhat::Profiler::builder().testing().build();
    let baseline = dhat::HeapStats::get();

    parse(&html, parser);

    let final_stats = dhat::HeapStats::get();

    // ── Drop profiler → writes dhat-heap.json ────────────────────────────
    // The JSON file captures every individual allocation with its backtrace,
    // callable from the dhat online viewer:
    //   https://nnethercote.github.io/dh_view/dh_view.html
    drop(_profiler);

    // ── Step 4: compute diff ─────────────────────────────────────────────
    let alloc_count = final_stats
        .total_blocks
        .saturating_sub(baseline.total_blocks);
    let total_bytes = final_stats.total_bytes.saturating_sub(baseline.total_bytes);

    // ── Step 5: print results ────────────────────────────────────────────
    let file_kib = file_size as f64 / 1024.0;

    println!();
    println!("╔══════════════════════════════════════════════════════╗");
    println!("║        Memory Benchmark Results (dhat heap)         ║");
    println!("╠══════════════════════════════════════════════════════╣");
    println!("║  Parser:     {:<38}║", parser.display_name());
    println!("║  Test File:  wikipedia_fragment.html                ║");
    println!("║  File Size:  {:.0} KiB{:<31}║", file_kib, "");
    println!("╠══════════════════════════════════════════════════════╣");
    println!(
        "║  Allocations:          {:<10}               ║",
        alloc_count
    );
    println!(
        "║  Total Bytes:          {:<10}               ║",
        human_bytes(total_bytes)
    );
    println!("╠══════════════════════════════════════════════════════╣");
    println!(
        "║  max_blocks:           {:<10}               ║",
        final_stats.max_blocks
    );
    println!(
        "║  max_bytes:            {:<10}               ║",
        human_bytes(final_stats.max_bytes as u64)
    );
    println!("╚══════════════════════════════════════════════════════╝");
    println!();
    println!(
        "📄 dhat-heap.json 已生成 → 上传到 https://nnethercote.github.io/dh_view/dh_view.html"
    );
    println!();

    // ── Step 6: wiki template ────────────────────────────────────────────
    print_wiki_template();
}

// ── Wiki template ───────────────────────────────────────────────────────────

/// Prints a markdown table skeleton that can be copied directly into a GitHub
/// Wiki page. The user fills in the three rows with their measured values.
fn print_wiki_template() {
    println!("── 可直接复制到 GitHub Wiki 的结果模板 ──");
    println!();
    println!("```markdown");
    println!("# HTML Parser Memory Allocation Comparison");
    println!();
    println!("## Test Setup");
    println!();
    println!("- **Hardware**: [填写 CPU 型号 / RAM]");
    println!("- **OS**: [填写操作系统及版本]");
    println!("- **Rust toolchain**: [填写 `rustc --version` 输出]");
    println!("- **Test file**: `benches/data/wikipedia_fragment.html`");
    println!("- **Profiler**: [dhat](https://crates.io/crates/dhat) v0.3");
    println!("- **Profile mode**: `--release`");
    println!();
    println!("## Results");
    println!();
    println!("| Parser | Allocations (count) | Total Bytes | Max Live Blocks | Max Live Bytes |");
    println!("|--------|--------------------|-------------|-----------------|----------------|");
    println!("| muskitty-html5-parser | ? | ? | ? | ? |");
    println!("| html5ever + RcDom     | ? | ? | ? | ? |");
    println!("| tl                    | ? | ? | ? | ? |");
    println!();
    println!("## Interpretation Notes");
    println!();
    println!("- **Allocations (count)**: Total number of heap allocation calls during");
    println!("  a single parse. Lower is better — fewer allocations mean less");
    println!("  allocator contention and more predictable latency.");
    println!("- **Total Bytes**: Sum of all allocation sizes. This includes both");
    println!("  short-lived scratch buffers and the final DOM tree.");
    println!("- **Max Live Blocks / Bytes**: Peak heap usage during the parse.");
    println!("  Useful for sizing memory budgets in embedded / WASM contexts.");
    println!();
    println!("### What the numbers mean for zero-unsafe research");
    println!();
    println!("If `muskitty` achieves allocation counts comparable to or lower than");
    println!("`html5ever`, it supports the thesis that **safe abstractions (Rc,");
    println!("RefCell, Vec) do not inherently waste memory**. If `muskitty` shows");
    println!("fewer allocations than `tl`, it suggests that spec-compliance");
    println!("bookkeeping (adoption agency algorithm, list of active formatting");
    println!("elements) accounts for most allocation pressure — not the choice");
    println!("of safe vs unsafe primitives.");
    println!();
    println!("## Reproducibility");
    println!();
    println!("```bash");
    println!("# 1. Generate test data");
    println!("bash scripts/fetch_bench_data.sh");
    println!();
    println!("# 2. Run each parser (from crates/muskitty-html5-parser/)");
    println!("cargo run --release --example bench-memory -- muskitty");
    println!("cargo run --release --example bench-memory -- html5ever");
    println!("cargo run --release --example bench-memory -- tl");
    println!();
    println!("# 3. Fill in the table above with the printed numbers");
    println!("```");
    println!();
    println!("## Raw dhat Profiles");
    println!();
    println!("- [muskitty-heap.json](./dhat/muskitty-heap.json)");
    println!("- [html5ever-heap.json](./dhat/html5ever-heap.json)");
    println!("- [tl-heap.json](./dhat/tl-heap.json)");
    println!();
    println!("Upload each JSON to <https://nnethercote.github.io/dh_view/dh_view.html>");
    println!("for an interactive flame graph of allocation sites.");
    println!("```");
}
