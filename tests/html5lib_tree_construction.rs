//! html5lib tree construction test suite harness.
//!
//! Drives the MusKitty parser through the official WPT tree-construction
//! fixtures (`tests/data/tree-construction/*.dat`) and reports pass/fail
//! per fixture. This is the ground-truth validation for Phase 5.
//!
//! Run with:
//!   cargo test --test html5lib_tree_construction -- --nocapture
//!
//! The test is data-driven: it never panics on an individual case mismatch.
//! Instead it collects all results, prints a detailed report with failure
//! samples, and only asserts that the harness ran at least one case.

use std::cell::RefCell;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::rc::Rc;

use muskitty_dom::{Attribute, Namespace, Node, NodeKind, NodeType};
use muskitty_html5_parser::parse;

// ─── Input preprocessing (WHATWG §13.2.3.5) ──────────────────────────
//
// Same normalization as the tokenizer harness: CRLF → LF, lone CR → LF.
// FF is left as-is. The parser does not do this itself.

fn preprocess_input(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            _ => out.push(c),
        }
    }
    out
}

// ─── .dat file parser ────────────────────────────────────────────────
//
// Format (per WPT html/syntax/parsing/resources/README.md):
//   Tests are separated by blank lines. Each test starts with "#data".
//   Fields: #data, #errors, #new-errors, #document-fragment,
//           #script-on / #script-off, #document.
//   #data content = all lines until #errors, joined by LF, with the
//   trailing newline removed.

#[derive(Debug, Clone)]
struct TestCase {
    data: String,
    document_fragment: Option<String>,
    /// None = run in both scripting modes; Some(true) = script-on;
    /// Some(false) = script-off.
    scripting: Option<bool>,
    expected_document: String,
    /// 1-based index within the file.
    index: usize,
}

struct DatParser {
    tests: Vec<TestCase>,
    data_lines: Vec<String>,
    fragment: Option<String>,
    scripting: Option<bool>,
    doc_lines: Vec<String>,
    field: &'static str,
    have_test: bool,
    index: usize,
}

impl DatParser {
    fn flush(&mut self) {
        if !self.have_test {
            return;
        }
        // Trim trailing empty lines from #document — they are test
        // separators (blank lines between tests), not document content.
        // #data does NOT get trimmed: a trailing blank line in #data
        // represents an actual newline in the input (e.g. tests16.dat #195
        // has input "<!doctype html><table>\n" where the \n is significant).
        while self.doc_lines.last().is_some_and(|l| l.is_empty()) {
            self.doc_lines.pop();
        }
        // #data: join lines with LF, drop trailing newline (the last line
        // already has no newline because .lines() strips it).
        let data_str = self.data_lines.join("\n");
        let doc_str = self.doc_lines.join("\n");
        self.index += 1;
        self.tests.push(TestCase {
            data: data_str,
            document_fragment: self.fragment.take(),
            scripting: self.scripting.take(),
            expected_document: doc_str,
            index: self.index,
        });
        self.data_lines.clear();
        self.doc_lines.clear();
        self.have_test = false;
    }
}

fn parse_dat(content: &str) -> Vec<TestCase> {
    let mut p = DatParser {
        tests: Vec::new(),
        data_lines: Vec::new(),
        fragment: None,
        scripting: None,
        doc_lines: Vec::new(),
        field: "",
        have_test: false,
        index: 0,
    };

    for line in content.lines() {
        if line == "#data" {
            // Start new test; flush previous if any.
            p.flush();
            p.have_test = true;
            p.field = "data";
        } else if line == "#errors" {
            p.field = "errors";
        } else if line == "#new-errors" {
            p.field = "new-errors";
        } else if line == "#document-fragment" {
            p.field = "document-fragment";
        } else if line == "#script-on" {
            p.scripting = Some(true);
            p.field = "script";
        } else if line == "#script-off" {
            p.scripting = Some(false);
            p.field = "script";
        } else if line == "#document" {
            p.field = "document";
        } else if line.is_empty() {
            // Blank line handling depends on the current field:
            //  - #errors: flush (end of test, separator before next #data)
            //  - #data / #document: preserve as content (a blank line inside
            //    these sections is legitimate — e.g. a "\n" inside a text
            //    node serialized as | " \n ". Trailing blank lines are
            //    trimmed in flush()).
            if p.field == "errors" {
                p.flush();
                p.field = "";
            } else if p.field == "data" {
                p.data_lines.push(String::new());
            } else if p.field == "document" {
                p.doc_lines.push(String::new());
            }
        } else {
            match p.field {
                "data" => p.data_lines.push(line.to_string()),
                "document" => p.doc_lines.push(line.to_string()),
                "document-fragment" => p.fragment = Some(line.to_string()),
                _ => { /* errors / new-errors lines ignored */ }
            }
        }
    }
    // Flush trailing test (file may not end with blank line).
    p.flush();
    p.tests
}

// ─── DOM serializer (html5lib #document format) ──────────────────────
//
// WPT README serialization rules:
//   - Each line: "| " + "  " * depth + node representation.
//   - Element: "<" + namespace_designator + local_name + ">"
//       HTML ns → "" ; SVG → "svg " ; MathML → "math ".
//   - Attribute (indented as if child of element):
//       attribute_name_string + "=\"" + value + "\""
//       Sorted by attribute_name_string (UTF-16 code unit).
//   - Text: "\"" + data + "\""
//   - Comment: "<!-- " + data + " -->"
//   - DOCTYPE: "<!DOCTYPE " + name + ">"
//       or "<!DOCTYPE " + name + " \"" + public_id + "\" \"" + system_id + "\">"
//   - Template: "content" line, children nested under it.

fn attr_designator(attr: &Attribute) -> &'static str {
    match attr.namespace_uri.as_deref() {
        Some("http://www.w3.org/1999/xlink") => "xlink ",
        Some("http://www.w3.org/XML/1998/namespace") => "xml ",
        Some("http://www.w3.org/2000/xmlns/") => "xmlns ",
        _ => "",
    }
}

fn element_designator(ns: Namespace) -> &'static str {
    match ns {
        Namespace::Html => "",
        Namespace::Svg => "svg ",
        Namespace::MathMl => "math ",
    }
}

fn serialize_node(node: &Rc<RefCell<Node>>, depth: usize, out: &mut String) {
    let indent = format!("| {}", "  ".repeat(depth));
    let children: Vec<Rc<RefCell<Node>>>;
    let template_content: Option<Rc<RefCell<Node>>>;

    {
        let n = node.borrow();
        children = n.children.to_vec();
        template_content = match &n.kind {
            NodeKind::Element(e) if e.local_name == "template" => e.template_content.clone(),
            _ => None,
        };

        match n.node_type {
            NodeType::Element => {
                if let NodeKind::Element(e) = &n.kind {
                    let designator = element_designator(e.namespace);
                    out.push_str(&format!("{}<{}{}>\n", indent, designator, e.local_name));

                    // Attributes sorted by attribute_name_string (designator + local_name).
                    let mut attrs: Vec<&Attribute> = e.attributes.iter().collect();
                    attrs.sort_by(|a, b| {
                        let an = format!("{}{}", attr_designator(a), a.local_name);
                        let bn = format!("{}{}", attr_designator(b), b.local_name);
                        an.cmp(&bn)
                    });
                    let attr_indent = format!("| {}", "  ".repeat(depth + 1));
                    for attr in attrs {
                        let designator = attr_designator(attr);
                        out.push_str(&format!(
                            "{}{}{}=\"{}\"\n",
                            attr_indent, designator, attr.local_name, attr.value
                        ));
                    }
                }
            }
            NodeType::Text => {
                if let NodeKind::Text(t) = &n.kind {
                    out.push_str(&format!("{}\"{}\"\n", indent, t.data));
                }
            }
            NodeType::Comment => {
                if let NodeKind::Comment(c) = &n.kind {
                    out.push_str(&format!("{}<!-- {} -->\n", indent, c.data));
                }
            }
            NodeType::ProcessingInstruction => {
                if let NodeKind::ProcessingInstruction(pi) = &n.kind {
                    // WPT test.js format: `<?{target} {data}?>`
                    // (always a space after target, even when data is empty).
                    out.push_str(&format!("{}<?{} {}?>\n", indent, pi.target, pi.data));
                }
            }
            NodeType::DocumentType => {
                if let NodeKind::DocumentType(d) = &n.kind {
                    if d.public_id.is_empty() && d.system_id.is_empty() {
                        out.push_str(&format!("{}<!DOCTYPE {}>\n", indent, d.name));
                    } else {
                        out.push_str(&format!(
                            "{}<!DOCTYPE {} \"{}\" \"{}\">\n",
                            indent, d.name, d.public_id, d.system_id
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    // Template content: html5lib serializes template's content DocumentFragment
    // as a "content" line with the fragment's children nested under it.
    if let Some(content) = &template_content {
        let content_indent = format!("| {}", "  ".repeat(depth + 1));
        out.push_str(&format!("{}content\n", content_indent));
        let content_children: Vec<Rc<RefCell<Node>>> = content.borrow().children.to_vec();
        for child in &content_children {
            serialize_node(child, depth + 2, out);
        }
    }

    for child in &children {
        serialize_node(child, depth + 1, out);
    }
}

/// Serialize a Document node into the html5lib #document format.
fn serialize_document(doc: &Rc<RefCell<Node>>) -> String {
    let mut out = String::new();
    let children: Vec<Rc<RefCell<Node>>> = doc.borrow().children.to_vec();
    for child in &children {
        serialize_node(child, 0, &mut out);
    }
    // #document blocks in .dat files have no trailing newline beyond the
    // last line's own. Our `out` ends with '\n' after the last node; trim
    // the single trailing newline to match.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

// ─── Test case driver ────────────────────────────────────────────────

#[derive(Clone)]
struct CaseResult {
    file: String,
    index: usize,
    passed: bool,
    skipped: bool,
    skip_reason: String,
    detail: String,
    data_preview: String,
}

fn run_case(file: &str, case: &TestCase) -> CaseResult {
    // Fragment parsing is not yet supported — skip these.
    if case.document_fragment.is_some() {
        return CaseResult {
            file: file.to_string(),
            index: case.index,
            passed: false,
            skipped: true,
            skip_reason: "document-fragment (fragment parsing not implemented)".to_string(),
            detail: String::new(),
            data_preview: preview(&case.data),
        };
    }
    // #script-on tests require scripting enabled; we only support disabled.
    if case.scripting == Some(true) {
        return CaseResult {
            file: file.to_string(),
            index: case.index,
            passed: false,
            skipped: true,
            skip_reason: "script-on (scripting flag not implemented)".to_string(),
            detail: String::new(),
            data_preview: preview(&case.data),
        };
    }

    let input = preprocess_input(&case.data);
    // parse() may panic on edge cases (e.g. adoption agency index bugs);
    // catch_unwind isolates failures so the full suite still runs.
    let parse_result = catch_unwind(AssertUnwindSafe(|| parse(&input)));
    let actual = match parse_result {
        Ok(doc) => serialize_document(&doc),
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("(non-string panic)");
            return CaseResult {
                file: file.to_string(),
                index: case.index,
                passed: false,
                skipped: false,
                skip_reason: String::new(),
                detail: format!("\n      PANIC: {}", msg),
                data_preview: preview(&case.data),
            };
        }
    };

    let passed = actual == case.expected_document;
    let detail = if passed {
        String::new()
    } else {
        format!(
            "\n      input:    {:?}\n      expected:\n{}\n      actual:\n{}",
            case.data,
            indent_block(&case.expected_document, "        "),
            indent_block(&actual, "        ")
        )
    };

    CaseResult {
        file: file.to_string(),
        index: case.index,
        passed,
        skipped: false,
        skip_reason: String::new(),
        detail,
        data_preview: preview(&case.data),
    }
}

fn preview(s: &str) -> String {
    let s = s.replace('\n', "\\n");
    // Char-boundary-safe truncation: find the largest byte index ≤ 60
    // that falls on a char boundary.
    let cut = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= 60)
        .last()
        .unwrap_or(0);
    if s.len() > 60 {
        format!("{}…", &s[..cut])
    } else {
        s
    }
}

fn indent_block(s: &str, prefix: &str) -> String {
    s.lines()
        .map(|l| format!("{}{}", prefix, l))
        .collect::<Vec<_>>()
        .join("\n")
}

// ─── Fixture loader ──────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("tree-construction")
}

// ─── Main test entry ─────────────────────────────────────────────────

#[test]
fn html5lib_tree_construction_suite() {
    let dir = fixture_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read tree-construction dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("dat"))
        .collect();
    entries.sort();

    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    let mut total_skip = 0usize;
    let mut total_panic = 0usize;
    let mut per_file: Vec<(String, usize, usize, usize)> = Vec::new();
    let mut failure_samples: Vec<CaseResult> = Vec::new();
    let mut panic_samples: Vec<CaseResult> = Vec::new();
    let mut skip_reasons: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    const MAX_SAMPLES_PER_FILE: usize = 3;

    for path in &entries {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let cases = parse_dat(&text);

        let mut file_pass = 0usize;
        let mut file_fail = 0usize;
        let mut file_skip = 0usize;
        let mut samples = 0usize;

        for case in &cases {
            let r = run_case(&name, case);
            if r.skipped {
                file_skip += 1;
                *skip_reasons.entry(r.skip_reason.clone()).or_insert(0) += 1;
            } else if r.passed {
                file_pass += 1;
            } else {
                file_fail += 1;
                if r.detail.starts_with("\n      PANIC") {
                    total_panic += 1;
                    if panic_samples.len() < 15 {
                        panic_samples.push(r.clone());
                    }
                } else if samples < MAX_SAMPLES_PER_FILE {
                    failure_samples.push(r.clone());
                    samples += 1;
                }
            }
        }
        total_pass += file_pass;
        total_fail += file_fail;
        total_skip += file_skip;
        per_file.push((name, file_pass, file_fail, file_skip));
    }

    // ── Report ────────────────────────────────────────────────────────
    eprintln!("\n═══════════════════════════════════════════════════════════════");
    eprintln!(" html5lib tree-construction test suite — results");
    eprintln!("═══════════════════════════════════════════════════════════════");
    eprintln!(
        " {:<36} {:>8} {:>8} {:>8} {:>8}",
        "fixture", "pass", "fail", "skip", "total"
    );
    eprintln!(" ─────────────────────────────────────────────────────────────────");
    for (name, p, f, s) in &per_file {
        eprintln!(" {:<36} {:>8} {:>8} {:>8} {:>8}", name, p, f, s, p + f + s);
    }
    eprintln!(" ─────────────────────────────────────────────────────────────────");
    let total = total_pass + total_fail + total_skip;
    let ran = total_pass + total_fail;
    let pct = if ran == 0 {
        0.0
    } else {
        100.0 * total_pass as f64 / ran as f64
    };
    eprintln!(
        " {:<36} {:>8} {:>8} {:>8} {:>8}   (pass rate of non-skipped: {:.1}%)",
        "TOTAL", total_pass, total_fail, total_skip, total, pct
    );

    if !failure_samples.is_empty() {
        eprintln!("\n── failure samples (up to {MAX_SAMPLES_PER_FILE} per file) ──");
        for r in &failure_samples {
            eprintln!(
                "\n[{} #{}] input: {}\n  diff:{}",
                r.file, r.index, r.data_preview, r.detail
            );
        }
    }

    if !panic_samples.is_empty() {
        eprintln!("\n── panic samples ({} total) ──", total_panic);
        for r in &panic_samples {
            eprintln!(
                "\n[{} #{}] input: {}{}",
                r.file, r.index, r.data_preview, r.detail
            );
        }
    }

    if !skip_reasons.is_empty() {
        eprintln!("\n── skip reasons ──");
        let mut reasons: Vec<(String, usize)> = skip_reasons.clone().into_iter().collect();
        reasons.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for (reason, count) in &reasons {
            eprintln!("  {:>6}  {}", count, reason);
        }
    }
    eprintln!("═══════════════════════════════════════════════════════════════\n");

    // ── Write gap report to file (for tracking across phases) ─────────
    let report_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("tree_construction_gap_report.md");
    let mut report = String::new();
    report.push_str("# html5lib Tree Construction Gap Report\n\n");
    report.push_str(&format!(
        "**Pass rate: {:.1}% ({}/{})** — {} skipped — {} panicked\n\n",
        pct, total_pass, ran, total_skip, total_panic
    ));
    report.push_str("## Per-fixture results\n\n");
    report.push_str("| Fixture | Pass | Fail | Skip | Total |\n");
    report.push_str("|---------|-----:|-----:|-----:|------:|\n");
    for (name, p, f, s) in &per_file {
        report.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            name,
            p,
            f,
            s,
            p + f + s
        ));
    }
    if !panic_samples.is_empty() {
        report.push_str("\n## Panic samples (parser crashes)\n\n");
        for r in &panic_samples {
            report.push_str(&format!(
                "- **{} #{}** `{}` — {}\n",
                r.file,
                r.index,
                r.data_preview,
                r.detail.trim()
            ));
        }
    }
    if !skip_reasons.is_empty() {
        report.push_str("\n## Skip reasons\n\n");
        report.push_str("| Count | Reason |\n|------:|--------|\n");
        let mut reasons: Vec<(String, usize)> = skip_reasons.into_iter().collect();
        reasons.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for (reason, count) in &reasons {
            report.push_str(&format!("| {} | {} |\n", count, reason));
        }
    }
    let _ = fs::write(&report_path, &report);
    eprintln!("Gap report written to {}", report_path.display());

    assert!(
        total > 0,
        "no test cases were loaded — fixture data missing?"
    );
    eprintln!(
        "PASS RATE: {:.1}% ({}/{}) — {} skipped — harness ran to completion; not asserting a hard threshold yet.",
        pct, total_pass, ran, total_skip
    );
}
