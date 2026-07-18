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

use muskitty_dom::Node;

use crate::parser::HtmlTreeConstructor;
use muskitty_html5_tokenizer::{HtmlTokenizer, Token, Tokenizer};

/// Parse an HTML string into a Document node.
///
/// Implements the two-stage model of §13.2.1: construct a tokenizer over
/// `input`, construct a tree constructor targeting a fresh Document, then
/// feed every emitted token to the tree constructor until EOF.
///
/// Returns the Document node. Parse errors encountered during tree
/// construction are stored on the constructor and currently discarded;
/// a future API will expose them.
pub fn parse(input: &str) -> Rc<RefCell<Node>> {
    let document = Node::new_document();
    let mut tokenizer = HtmlTokenizer::new(input);
    let mut constructor = HtmlTreeConstructor::new(document.clone());
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
    document
}
