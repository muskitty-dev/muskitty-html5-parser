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

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::ParseError;
use crate::parser::HtmlTreeConstructor;
use muskitty_dom::Node;
use muskitty_html5_tokenizer::{HtmlTokenizer, Token, Tokenizer};

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
        constructor.run(&token, &mut tokenizer);
        if matches!(token, Token::EOF) {
            break;
        }
    }
    // §13.2.7 "stop parsing" step 4: pop all nodes off the stack of open
    // elements. This fires the maybe-clone hook (§4.10.10) for any open
    // <option> elements, mirroring their content into <selectedcontent>.
    constructor.finalize();
    ParseOutput {
        document,
        errors: constructor.errors,
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
}
