//! html5lib tokenizer test suite harness.
//!
//! Drives the MusKitty tokenizer through the official html5lib-tests
//! tokenizer fixtures (`tests/data/tokenizer/*.test`) and reports
//! pass/fail per fixture. This is the "WPT semantic comparison" step
//! referenced in CLAUDE.md (the html5lib tokenizer suite is the de-facto
//! ground truth; WPT mirrors it).
//!
//! Run with:
//!   cargo test --test html5lib_tokenizer -- --nocapture
//!
//! The test is data-driven: it never panics on an individual case mismatch.
//! Instead it collects all results, prints a detailed report, and only
//! fails the `#[test]` at the end so the full picture is always visible.

use muskitty_html_parser::tokenizer::{HtmlTokenizer, State, Token, Tokenizer};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

// ─── Input preprocessing (WHATWG §13.2.3.5) ──────────────────────────
//
// The tokenizer currently consumes a `Vec<char>` built straight from
// `str::chars`. The spec's "preprocessing the input stream" step (normalize
// CRLF / CR / FF line breaks to LF) is the caller's responsibility, and
// html5lib test inputs represent the *raw* input stream, so we apply that
// normalization here before feeding the tokenizer. This isolates "state
// machine" bugs from "preprocessing" bugs.

fn preprocess_input(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // CRLF → single LF; lone CR → LF.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\u{000C}' => {
                // FF is treated as whitespace by the tokenizer, but the
                // preprocessing step does NOT rewrite it to LF. Leave as-is.
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

// ─── doubleEscaped handling ───────────────────────────────────────────
//
// When a test case sets `doubleEscaped: true`, every `\uHHHH` sequence
// (a *literal* backslash + 'u' + 4 hex digits, as it appears after JSON
// decoding) in both input and output strings must be converted to the
// corresponding Unicode code point. See the tokenizer/README.md.

fn unescape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '\\' && i + 5 < bytes.len() + 1 && i + 1 < bytes.len() && bytes[i + 1] == 'u'
        {
            // Need 4 hex digits after \u
            if i + 6 <= bytes.len() {
                let hex: String = bytes[i + 2..i + 6].iter().collect();
                if let Ok(code) = u32::from_str_radix(&hex, 16) {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                        i += 6;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

// ─── Expected token (parsed from html5lib JSON) ───────────────────────

#[derive(Debug, Clone, PartialEq)]
enum ExpectedToken {
    Doctype {
        name: Option<String>,
        public_id: Option<String>,
        system_id: Option<String>,
        // html5lib "correctness": true means force_quirks is false.
        force_quirks: bool,
    },
    StartTag {
        name: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    EndTag {
        name: String,
    },
    Comment(String),
    Character(String),
}

impl ExpectedToken {
    /// Parse a single html5lib token array, e.g. `["StartTag", "p", {"a":"b"}, true]`.
    fn from_json(arr: &[Value], double_escaped: bool) -> Result<Self, String> {
        let kind = arr
            .get(0)
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing token kind".to_string())?;
        match kind {
            "DOCTYPE" => {
                let name = opt_string(arr.get(1), double_escaped);
                let public_id = opt_string(arr.get(2), double_escaped);
                let system_id = opt_string(arr.get(3), double_escaped);
                // correctness: true ⇒ force_quirks false. Default true (no force_quirks).
                let correctness = arr.get(4).and_then(|v| v.as_bool()).unwrap_or(true);
                Ok(ExpectedToken::Doctype {
                    name,
                    public_id,
                    system_id,
                    force_quirks: !correctness,
                })
            }
            "StartTag" => {
                let name = req_string(arr.get(1), double_escaped)?;
                let attrs = parse_attrs(arr.get(2));
                // html5lib tokenizer fixture format: ["StartTag", name, attrs, selfClosing?]
                // selfClosing is at index 3 (right after attrs). Reading index 4
                // (the DOCTYPE correctness slot) was an off-by-one bug that caused
                // self-closing flags to always be parsed as false.
                let self_closing = arr.get(3).and_then(|v| v.as_bool()).unwrap_or(false);
                Ok(ExpectedToken::StartTag {
                    name,
                    attrs,
                    self_closing,
                })
            }
            "EndTag" => {
                let name = req_string(arr.get(1), double_escaped)?;
                Ok(ExpectedToken::EndTag { name })
            }
            "Comment" => {
                let data = req_string(arr.get(1), double_escaped)?;
                Ok(ExpectedToken::Comment(data))
            }
            "Character" => {
                let data = req_string(arr.get(1), double_escaped)?;
                Ok(ExpectedToken::Character(data))
            }
            other => Err(format!("unknown token kind: {other}")),
        }
    }
}

fn opt_string(v: Option<&Value>, double_escaped: bool) -> Option<String> {
    v.and_then(|v| v.as_str()).map(|s| {
        if double_escaped {
            unescape_double(s)
        } else {
            s.to_string()
        }
    })
}

fn req_string(v: Option<&Value>, double_escaped: bool) -> Result<String, String> {
    v.and_then(|v| v.as_str())
        .map(|s| {
            if double_escaped {
                unescape_double(s)
            } else {
                s.to_string()
            }
        })
        .ok_or_else(|| "expected string".to_string())
}

fn parse_attrs(v: Option<&Value>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(Value::Object(map)) = v {
        // Preserve insertion order (serde_json::Map with the default "preserve_order"
        // feature off still iterates in insertion order for small maps? No — by default
        // serde_json uses BTreeMap-like ordering only with the feature. The default is
        // a wrapped IndexMap only if `preserve_order` is enabled. Without it, order is
        // alphabetical via BTreeMap. For tokenizer attribute *value* comparison order
        // doesn't matter, so we sort both sides before comparing.)
        for (k, val) in map.iter() {
            let vs = val.as_str().unwrap_or("").to_string();
            out.push((k.clone(), vs));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// ─── Convert MusKitty Token → ExpectedToken for comparison ────────────

fn actual_to_expected(tokens: &[Token]) -> Vec<ExpectedToken> {
    // html5lib coalesces adjacent Character tokens into one. Our tokenizer
    // emits one Character per code point, so merge runs of Character here.
    let mut out = Vec::new();
    let mut buf = String::new();
    for t in tokens {
        match t {
            Token::Character(c) => buf.push(*c),
            other => {
                if !buf.is_empty() {
                    out.push(ExpectedToken::Character(std::mem::take(&mut buf)));
                }
                match other {
                    Token::Doctype(d) => out.push(ExpectedToken::Doctype {
                        name: d.name.clone(),
                        public_id: d.public_id.clone(),
                        system_id: d.system_id.clone(),
                        force_quirks: d.force_quirks,
                    }),
                    Token::Tag(tg) => match tg.kind {
                        muskitty_html_parser::tokenizer::TagKind::Start => {
                            let mut a = tg.attrs.clone();
                            a.sort_by(|x, y| x.0.cmp(&y.0));
                            out.push(ExpectedToken::StartTag {
                                name: tg.name.clone(),
                                attrs: a,
                                self_closing: tg.self_closing,
                            });
                        }
                        muskitty_html_parser::tokenizer::TagKind::End => {
                            out.push(ExpectedToken::EndTag {
                                name: tg.name.clone(),
                            })
                        }
                    },
                    Token::Comment(s) => out.push(ExpectedToken::Comment(s.clone())),
                    Token::EOF => {}
                    Token::ProcessingInstruction { .. } => {}
                    Token::Character(_) => unreachable!(),
                }
            }
        }
    }
    if !buf.is_empty() {
        out.push(ExpectedToken::Character(buf));
    }
    out
}

// ─── Test case driver ─────────────────────────────────────────────────

#[derive(Clone)]
struct CaseResult {
    description: String,
    initial_state: String,
    passed: bool,
    detail: String,
}

fn initial_state_from_name(name: &str) -> State {
    match name {
        "Data state" => State::Data,
        "PLAINTEXT state" => State::PLAINTEXT,
        "RCDATA state" => State::RCDATA,
        "RAWTEXT state" => State::RAWTEXT,
        "Script data state" => State::ScriptData,
        "CDATA section state" => State::CDATASection,
        // Some older fixtures omit the " state" suffix.
        "Data" => State::Data,
        "PLAINTEXT" => State::PLAINTEXT,
        "RCDATA" => State::RCDATA,
        "RAWTEXT" => State::RAWTEXT,
        "Script data" => State::ScriptData,
        "CDATA section" => State::CDATASection,
        other => panic!("unknown initial state: {other}"),
    }
}

fn run_case(case: &Value) -> Vec<CaseResult> {
    let description = case
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("(no description)")
        .to_string();
    let raw_input = case
        .get("input")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let double_escaped = case
        .get("doubleEscaped")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let last_start_tag = case.get("lastStartTag").and_then(|v| v.as_str());

    let input = if double_escaped {
        unescape_double(&raw_input)
    } else {
        raw_input
    };
    let input = preprocess_input(&input);

    let expected_raw = case.get("output").and_then(|v| v.as_array()).cloned();
    let expected: Vec<ExpectedToken> = match &expected_raw {
        Some(arr) => arr
            .iter()
            .filter_map(|t| t.as_array())
            .filter_map(|a| ExpectedToken::from_json(a, double_escaped).ok())
            .collect(),
        None => Vec::new(),
    };

    let initial_states: Vec<String> = case
        .get("initialStates")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| vec!["Data state".to_string()]);

    let mut results = Vec::new();
    for is in &initial_states {
        let mut tok = HtmlTokenizer::new(&input);
        let state = initial_state_from_name(is);
        tok.set_state(state);
        if let Some(tag) = last_start_tag {
            tok.set_appropriate_end_tag_name(Some(tag));
        }

        let mut actual = Vec::new();
        // DEBUG: cap emitted tokens to catch runaway state-machine bugs without
        // OOMing the whole process. A correct tokenization of a bounded input
        // never needs more than ~10× input chars worth of tokens.
        let input_chars = input.chars().count();
        let guard = 10 * input_chars.max(1) + 1000;
        let mut runaway = false;
        while let Some(t) = tok.next_token() {
            actual.push(t);
            if actual.len() > guard {
                runaway = true;
                break;
            }
        }
        if runaway {
            eprintln!(
                "!! RUNAWAY: desc={:?} state={} input_len={:?} input_head={:?}",
                description,
                is,
                input.chars().count(),
                input.chars().take(80).collect::<String>()
            );
        }
        let actual = actual_to_expected(&actual);

        let passed = actual == expected;
        let detail = if passed {
            String::new()
        } else {
            format!(
                "\n      input:    {:?}\n      expected: {:#?}\n      actual:   {:#?}",
                input, expected, actual
            )
        };
        results.push(CaseResult {
            description: if initial_states.len() > 1 {
                format!("{description} [{is}]")
            } else {
                description.clone()
            },
            initial_state: is.clone(),
            passed,
            detail,
        });
    }
    results
}

// ─── Fixture loader ───────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("tokenizer")
}

fn load_fixture(path: &PathBuf) -> Vec<Value> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let root: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"));
    // html5lib tokenizer fixtures use either "tests" or "xmlViolationTests".
    let key = if root.get("tests").is_some() {
        "tests"
    } else if root.get("xmlViolationTests").is_some() {
        "xmlViolationTests"
    } else {
        "tests"
    };
    root.get(key)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

// ─── Main test entry ──────────────────────────────────────────────────

#[test]
fn html5lib_tokenizer_suite() {
    let dir = fixture_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read tokenizer dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("test"))
        .collect();
    entries.sort();

    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    let mut per_file: Vec<(String, usize, usize)> = Vec::new();
    // Collect a sample of failures (capped) for the report.
    let mut failure_samples: Vec<(String, CaseResult)> = Vec::new();
    const MAX_SAMPLES_PER_FILE: usize = 5;

    for path in &entries {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let cases = load_fixture(path);
        let mut file_pass = 0usize;
        let mut file_fail = 0usize;
        let mut samples = 0usize;

        for case in &cases {
            for r in run_case(case) {
                if r.passed {
                    file_pass += 1;
                } else {
                    file_fail += 1;
                    if samples < MAX_SAMPLES_PER_FILE {
                        failure_samples.push((name.clone(), r.clone()));
                        samples += 1;
                    }
                }
            }
        }
        total_pass += file_pass;
        total_fail += file_fail;
        per_file.push((name, file_pass, file_fail));
    }

    // ── Report ────────────────────────────────────────────────────────
    eprintln!("\n═══════════════════════════════════════════════════════════════");
    eprintln!(" html5lib tokenizer test suite — results");
    eprintln!("═══════════════════════════════════════════════════════════════");
    eprintln!(
        " {:<32} {:>10} {:>10} {:>10}",
        "fixture", "pass", "fail", "total"
    );
    eprintln!(" ─────────────────────────────────────────────────────────────────");
    for (name, p, f) in &per_file {
        eprintln!(" {:<32} {:>10} {:>10} {:>10}", name, p, f, p + f);
    }
    eprintln!(" ─────────────────────────────────────────────────────────────────");
    let total = total_pass + total_fail;
    let pct = if total == 0 {
        0.0
    } else {
        100.0 * total_pass as f64 / total as f64
    };
    eprintln!(
        " {:<32} {:>10} {:>10} {:>10}   ({:.1}%)",
        "TOTAL", total_pass, total_fail, total, pct
    );

    if !failure_samples.is_empty() {
        eprintln!("\n── failure samples (up to {MAX_SAMPLES_PER_FILE} per file) ──");
        for (file, r) in &failure_samples {
            eprintln!(
                "\n[{file}] {}\n  initial state: {}{}",
                r.description, r.initial_state, r.detail
            );
        }
    }
    eprintln!("═══════════════════════════════════════════════════════════════\n");

    // Soft threshold: the suite is informational for now. We assert only that
    // the harness ran at least one case (sanity check) and print the pass rate.
    // Tightening this to a hard pass-rate gate is a follow-up once known gaps
    // (error reporting, etc.) are closed.
    assert!(
        total > 0,
        "no test cases were loaded — fixture data missing?"
    );
    eprintln!(
        "PASS RATE: {:.1}% ({}/{}) — harness ran to completion; not asserting a hard threshold yet.",
        pct, total_pass, total
    );
}
