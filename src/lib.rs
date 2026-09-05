//! MusKitty HTML Parser
//!
//! Implements the WHATWG HTML parsing algorithm.
//!
//! # Architecture
//!
//! The parser follows the standard two-stage model (§13.2.1):
//! 1. **Tokenization** ([`tokenizer`]) — consumes a stream of code points
//!    and emits tokens.
//! 2. **Tree construction** ([`parser`]) — consumes tokens and builds the DOM.
//!
//! # References
//!
//! - WHATWG HTML Living Standard: <https://html.spec.whatwg.org/multipage/parsing.html>
//! - WPT test suite: <https://github.com/web-platform-tests/wpt/tree/master/html/syntax/parsing>

pub mod error;
pub mod parser;
pub mod serialize;

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::ParseError;
use crate::parser::{HtmlTreeConstructor, InsertionMode};
use muskitty_dom::{append_child, drain_children, remove_child, Node};
use muskitty_html5_tokenizer::{HtmlTokenizer, State, Token, Tokenizer};

/// 默认输入大小上限：64 MiB（参考 Chromium 的输入保护策略）。
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// 默认 open elements 栈深度上限：512（参考 Chromium `kMaxHTMLParserDOMDepth`、
/// WebKit `maxDOMTreeDepth = 500`）。
pub const MAX_OPEN_ELEMENTS: usize = 512;

/// 解析结果：包含 Document 与累积的解析错误。
pub struct ParseOutput {
    /// Document 节点。可能为部分构建（若触发 `InputTooLarge` 则反映
    /// 截止点的 DOM 状态；若触发 `DomDepthExceeded` 则某些深嵌套元素
    /// 未被插入）。
    pub document: Rc<RefCell<Node>>,
    /// 解析过程中累积的错误（含 `InputTooLarge` / `DomDepthExceeded`）。
    pub errors: Vec<ParseError>,
}

/// Parse an HTML string into a Document node.
///
/// Implements the two-stage model of §13.2.1: construct a tokenizer over
/// `input`, construct a tree constructor targeting a fresh Document, then
/// feed every emitted token to the tree constructor until EOF.
///
/// 向后兼容入口：使用默认限制（`MAX_INPUT_BYTES` / `MAX_OPEN_ELEMENTS`），
/// 丢弃累积的解析错误。需要错误信息的调用方应使用 [`parse_with_limits`]。
pub fn parse(input: &str) -> Rc<RefCell<Node>> {
    parse_with_limits(input, MAX_INPUT_BYTES, MAX_OPEN_ELEMENTS).document
}

/// 解析 HTML 字符串，自定义输入大小与栈深度限制。
///
/// # 行为
/// - 输入字节数超 `max_bytes`：立即停止解析，返回空 Document +
///   `ParseError::InputTooLarge`。
/// - open elements 栈深度超 `max_open_elements`：跳过当前 push（解析继续）
///   + `ParseError::DomDepthExceeded`。
///
/// 参考 WHATWG §13.2.6 错误恢复语义：parser 不应因资源限制而崩溃。
pub fn parse_with_limits(input: &str, max_bytes: usize, max_open_elements: usize) -> ParseOutput {
    let document = Node::new_document();

    // §1: 输入大小检查（参考 Chromium 输入保护策略）。
    if input.len() > max_bytes {
        return ParseOutput {
            document,
            errors: vec![ParseError::InputTooLarge {
                actual: input.len(),
                limit: max_bytes,
            }],
        };
    }

    let mut tokenizer = HtmlTokenizer::new(input);
    let mut constructor = HtmlTreeConstructor::new(document.clone());
    constructor.max_open_elements = max_open_elements;
    run_tokenizer_loop(&mut constructor, &mut tokenizer);
    // §13.2.7 "stop parsing" step 4: pop all nodes off the stack of open
    // elements. This fires the maybe-clone hook (§4.10.10) for any open
    // <option> elements, mirroring their content into <selectedcontent>.
    constructor.finalize();
    ParseOutput {
        document,
        errors: constructor.errors,
    }
}

/// Drive a tokenizer to completion, feeding every token to the tree
/// constructor (§13.2.1 two-stage model). Shared by full-document parsing
/// ([`parse_with_limits`]) and fragment parsing ([`parse_fragment`]).
///
/// The tokenizer's initial state is set by the caller (Data for documents,
/// `fragment_tokenizer_state` for fragments). Returns after EOF.
fn run_tokenizer_loop(constructor: &mut HtmlTreeConstructor, tokenizer: &mut dyn Tokenizer) {
    loop {
        // §13.2.5.42: The markup declaration open state needs to know
        // whether the adjusted current node is in foreign content to decide
        // between CDATA section state (foreign) and bogus comment state
        // (HTML) when encountering `<![CDATA[`. Sync the flag before each
        // token is produced so the tokenizer sees the post-previous-token
        // open elements stack state.
        let in_foreign = constructor.current_node_in_foreign_content();
        tokenizer.set_foreign_content(in_foreign);
        let Some(token) = tokenizer.next_token() else {
            break;
        };
        constructor.run(&token, tokenizer);
        if matches!(token, Token::EOF) {
            break;
        }
    }
}

/// Parse an HTML fragment (§13.4.2) and return the resulting
/// `DocumentFragment`.
///
/// `context_element` determines the parsing rules: its namespace/name drive
/// the insertion-mode reset (§13.2.6.4.1), its name selects the tokenizer's
/// initial state (§13.4.2 step 4), and a `<template>` context enters
/// InTemplate mode (§13.4.2 step 5).
///
/// The algorithm creates a fresh Document, appends a synthetic `<html>` root
/// directly to the returned fragment, parses the input into that root, then
/// unwraps the root's children into the fragment (html5lib `getFragment()`:
/// `openElements[0].reparentChildren(fragment)`).
pub fn parse_fragment(input: &str, context_element: &Rc<RefCell<Node>>) -> Rc<RefCell<Node>> {
    let doc = Node::new_document();
    let fragment = Node::new_document_fragment(&doc);
    if input.len() > MAX_INPUT_BYTES {
        return fragment;
    }
    let context = match recreate_context_element(context_element, &doc) {
        Some(c) => c,
        None => {
            // F-9（审计 H-M2）：上下文非 Element（如对 Text/Comment/Document
            // 调用 [`set_inner_html`]）。此前 `expect` panic——脚本桥接入后
            // 即远程 DoS。按 §13.4.2 "follow the rules for the context
            // element" 精神兜底：取中性流内容容器 `<div>` 作上下文（Data
            // 状态 + InBody 插入模式），保证输入内容按 body 语义完整解析，
            // 优雅降级不中断。（`<html>` 上下文会落入 BeforeHead 模式、
            // 隐式 head/body 吞掉内容，故不采用。）
            Node::new_element_html("div", vec![], &doc)
        }
    };
    let mut constructor = HtmlTreeConstructor::new(doc);
    constructor.fragment_context = Some(context.clone());
    constructor.fragment_root = Some(fragment.clone());

    // §13.4.2 step 11: the root `<html>` is appended to the fragment (not
    // the Document), so the parsed content lands directly under the fragment.
    let root = Node::new_element_html("html", vec![], &constructor.document);
    let _ = append_child(&fragment, root.clone());
    constructor.open_elements.push(root.clone());

    // §13.4.2 step 5: template context enters the template insertion modes.
    if is_html_template(context_element) {
        constructor
            .template_insertion_modes
            .push(InsertionMode::InTemplate);
    }
    // §13.4.2 step 7: set the insertion mode via the reset algorithm (the
    // fragment context substitutes for the stack root at `is_last`).
    crate::parser::dispatch::reset_insertion_mode(&mut constructor);

    let mut tokenizer = HtmlTokenizer::new(input);
    if let Some(state) = fragment_tokenizer_state(context_element) {
        tokenizer.set_state(state);
    }
    // Note: the tokenizer's appropriate end tag name is deliberately NOT set
    // to the context element's name. Per html5lib, it is only set when the
    // tree constructor processes a real start tag; in fragment parsing the
    // input is raw content of the context (e.g. `</script>` inside a script
    // context is literal text, not a closing tag — tests4.dat #9).

    run_tokenizer_loop(&mut constructor, &mut tokenizer);
    constructor.finalize();

    // Unwrap: move the root's children into the fragment, then drop the root.
    let children = drain_children(&root);
    for child in children {
        let _ = append_child(&fragment, child);
    }
    let _ = remove_child(&fragment, &root);
    fragment
}

/// §13.4.2 step 6: create a copy of the context element in the new
/// Document (same namespace / prefix / local name / attributes).
///
/// F-9: 上下文非 Element 时返回 `None`（调用方按 `<html>` 上下文兜底），
/// 不再 panic。
fn recreate_context_element(
    context_element: &Rc<RefCell<Node>>,
    doc: &Rc<RefCell<Node>>,
) -> Option<Rc<RefCell<Node>>> {
    let borrowed = context_element.borrow();
    let e = borrowed.kind.as_element()?;
    let attrs = e.attributes.clone();
    Some(match e.namespace {
        muskitty_dom::Namespace::Html => Node::new_element_html(&e.local_name, attrs, doc),
        ns => Node::new_element_ns(e.local_name.clone(), ns, e.prefix.clone(), attrs, doc),
    })
}

/// Whether the context element is an HTML `<template>` (§13.4.2 step 5).
fn is_html_template(context_element: &Rc<RefCell<Node>>) -> bool {
    context_element
        .borrow()
        .kind
        .as_element()
        .is_some_and(|e| e.namespace == muskitty_dom::Namespace::Html && e.local_name == "template")
}

/// §13.4.2 step 4: the tokenizer state selected by the context element.
/// `None` → Data state.
///
/// The rules name HTML elements (a "title element" is an HTML-namespace
/// element), so a foreign `<title>` (e.g. `svg title` context) stays in
/// Data state and its `</title>` is a real end tag (foreign-fragment.dat #8).
fn fragment_tokenizer_state(context_element: &Rc<RefCell<Node>>) -> Option<State> {
    let borrowed = context_element.borrow();
    // F-9: 非 Element 上下文 → Data 状态（调用方已按 `<html>` 兜底）。
    let e = borrowed.kind.as_element()?;
    if e.namespace != muskitty_dom::Namespace::Html {
        return None;
    }
    match e.local_name.as_str() {
        "title" | "textarea" => Some(State::RCDATA),
        "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(State::RAWTEXT),
        "script" => Some(State::ScriptData),
        "plaintext" => Some(State::PLAINTEXT),
        // "noscript" with the scripting flag enabled → RAWTEXT; our parser
        // runs with scripting disabled, so noscript stays in Data.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_returns_document_for_normal_input() {
        // 向后兼容入口：正常 HTML 返回 Document
        let doc = parse("<div>hello</div>");
        // Document 应有子节点
        assert!(!doc.borrow().child_nodes().is_empty());
    }

    #[test]
    fn input_too_large_returns_empty_document_and_error() {
        // 1 MiB 输入，限制为 1 KB
        let huge: String = "x".repeat(1024 * 1024);
        let out = parse_with_limits(&huge, 1024, MAX_OPEN_ELEMENTS);
        assert!(matches!(
            out.errors.first(),
            Some(ParseError::InputTooLarge { actual, limit })
                if *actual == 1024 * 1024 && *limit == 1024
        ));
        // 空 Document（未开始解析）
        assert!(out.document.borrow().child_nodes().is_empty());
    }

    #[test]
    fn dom_depth_exceeded_skips_push_but_continues() {
        // 嵌套 100 层 div，限制为 50
        let html = "<div>".repeat(100);
        let out = parse_with_limits(&html, MAX_INPUT_BYTES, 50);
        // 应至少触发一次 DomDepthExceeded 错误
        assert!(
            out.errors
                .iter()
                .any(|e| matches!(e, ParseError::DomDepthExceeded { .. })),
            "expected DomDepthExceeded error"
        );
        // 但解析应继续，Document 非空（前 50 层已构建）
        assert!(!out.document.borrow().child_nodes().is_empty());
    }

    #[test]
    fn normal_input_within_limits_no_errors() {
        let out = parse_with_limits("<div></div>", MAX_INPUT_BYTES, MAX_OPEN_ELEMENTS);
        assert!(out.errors.is_empty(), "expected no errors");
    }

    // ── Fragment parsing (§13.4.2) ────────────────────────────────────

    fn context_element(local_name: &str) -> Rc<RefCell<Node>> {
        let doc = Node::new_document();
        Node::new_element_html(local_name, vec![], &doc)
    }

    /// Element local name for elements, node_name otherwise (e.g. "#text").
    fn fragment_child_names(fragment: &Rc<RefCell<Node>>) -> Vec<String> {
        fragment
            .borrow()
            .child_nodes()
            .iter()
            .map(|n| {
                let borrowed = n.borrow();
                borrowed
                    .kind
                    .as_element()
                    .map(|e| e.local_name.clone())
                    .unwrap_or_else(|| borrowed.node_name.clone())
            })
            .collect()
    }

    #[test]
    fn parse_fragment_returns_document_fragment() {
        let frag = parse_fragment("<b>x</b>", &context_element("div"));
        assert_eq!(
            frag.borrow().node_type,
            muskitty_dom::NodeType::DocumentFragment
        );
        assert_eq!(fragment_child_names(&frag), vec!["b"]);
    }

    #[test]
    fn parse_fragment_div_context_uses_in_body() {
        let frag = parse_fragment("<span>hi</span>", &context_element("div"));
        assert_eq!(fragment_child_names(&frag), vec!["span"]);
        // The synthetic <html> root is unwrapped — no stray <html> in output.
        assert!(fragment_child_names(&frag).iter().all(|n| n != "html"));
    }

    #[test]
    fn parse_fragment_table_context_uses_in_table() {
        // InTable: <tr> switches to InTableBody which wraps it in <tbody>,
        // so the fragment's single top-level child is the <tbody> (matching
        // the WPT expected `| <tbody>` / `|   <tr>`).
        let frag = parse_fragment("<tr><td>x</td></tr>", &context_element("table"));
        assert_eq!(fragment_child_names(&frag), vec!["tbody"]);
    }

    #[test]
    fn parse_fragment_title_context_uses_rcdata() {
        // title context → RCDATA: markup inside is consumed as text, so the
        // fragment holds a single Text node with the literal input.
        let frag = parse_fragment("<b>bold</b>", &context_element("title"));
        assert_eq!(fragment_child_names(&frag), vec!["#text"]);
        assert_eq!(
            crate::serialize::inner_html(&frag),
            "&lt;b&gt;bold&lt;/b&gt;"
        );
    }

    #[test]
    fn parse_fragment_script_context_uses_script_data() {
        // script context → ScriptData: the input is raw text (only
        // "</script" would terminate it), so the fragment is one Text node.
        let frag = parse_fragment("if (a<b) {}", &context_element("script"));
        assert_eq!(fragment_child_names(&frag), vec!["#text"]);
        assert_eq!(crate::serialize::inner_html(&frag), "if (a&lt;b) {}");
    }

    // —— F-9: 非 Element 上下文兜底（不再 panic）——

    #[test]
    fn parse_fragment_text_context_degrades_not_panics() {
        // F-9：Text 节点作为上下文 → 按 `<html>`（Data 状态 + InBody）
        // 兜底解析，产出正常 fragment。
        let doc = Node::new_document();
        let text = Node::new_text("hello", &doc);
        let frag = parse_fragment("<b>x</b>", &text);
        assert_eq!(fragment_child_names(&frag), vec!["b"]);
    }

    #[test]
    fn parse_fragment_document_context_degrades_not_panics() {
        let doc = Node::new_document();
        let frag = parse_fragment("<b>x</b>", &doc);
        assert_eq!(fragment_child_names(&frag), vec!["b"]);
    }

    #[test]
    fn set_inner_html_on_text_node_does_not_panic() {
        // F-9 端到端：脚本桥接路径（text.innerHTML = ...）不得 abort。
        let doc = Node::new_document();
        let text = Node::new_text("old", &doc);
        crate::serialize::set_inner_html(&text, "<b>x</b>");
        let children = text.borrow().child_nodes().to_vec();
        assert_eq!(children.len(), 1, "content must be replaced");
        // DOM 惯例：HTML 元素 node_name 为大写。
        assert_eq!(children[0].borrow().node_name, "B");
    }
}
