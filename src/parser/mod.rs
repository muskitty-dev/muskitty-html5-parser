//! Tree construction stage (§13.2.6).
//!
//! Consumes the token stream produced by the tokenizer and builds the DOM
//! tree per the WHATWG HTML insertion mode state machine.
//!
//! # Architecture
//!
//! - [`HtmlTreeConstructor`] holds the parser state: open elements stack,
//!   active formatting elements list, current insertion mode, and flags.
//! - [`dispatch`] routes each token to the handler for the current insertion
//!   mode.
//! - [`helpers`] contains the "insert a node" / "create an element" helper
//!   algorithms from §13.2.6.2.
//! - [`insertion_mode`] defines the 23 insertion modes from §13.2.6.1.

mod dispatch;
mod foreign;
mod helpers;
mod insertion_mode;

pub use insertion_mode::InsertionMode;

use std::cell::RefCell;
use std::rc::Rc;

use muskitty_dom::Node;

use crate::error::ParseError;
use crate::tokenizer::{Token, Tokenizer};

/// An entry in the list of active formatting elements (§13.2.6.2).
///
/// The list holds either a reference to an element on the open elements
/// stack, or a marker that delimits a section (pushed when entering table
/// contexts, template content, etc.).
#[derive(Clone)]
pub enum ActiveFormattingEntry {
    /// A marker entry, used to delimit sections of the list.
    Marker,
    /// An element entry, holding a reference to the formatting element.
    Element(Rc<RefCell<Node>>),
}

/// The HTML tree construction stage.
///
/// Holds the state of the insertion mode state machine (§13.2.6) and the
/// DOM tree being built. The `document` field is the output root; the
/// `open_elements` stack tracks the current open element chain.
pub struct HtmlTreeConstructor {
    /// The output Document node. Inserted elements are ultimately attached
    /// here (directly or via the `<html>` / `<head>` / `<body>` chain).
    pub document: Rc<RefCell<Node>>,
    /// The stack of open elements (§13.2.6.2). The top is the current node.
    pub open_elements: Vec<Rc<RefCell<Node>>>,
    /// The list of active formatting elements (§13.2.6.2). Used by the
    /// adoption agency algorithm; populated in Phase 3.3.
    pub active_formatting_elements: Vec<ActiveFormattingEntry>,
    /// The current insertion mode (§13.2.6.1).
    pub insertion_mode: InsertionMode,
    /// The original insertion mode, saved when entering Text mode or
    /// template content (§13.2.6.5, §13.2.6.16).
    pub original_insertion_mode: Option<InsertionMode>,
    /// The `<head>` element pointer, set in BeforeHead mode (§13.2.6.4).
    pub head_element: Option<Rc<RefCell<Node>>>,
    /// The `<form>` element pointer, updated in InBody mode (§13.2.6.4).
    pub form_element: Option<Rc<RefCell<Node>>>,
    /// Whether foster parenting is active (§13.2.6.3). Used by table
    /// insertion modes; deferred to Phase 3.4.
    pub foster_parenting: bool,
    /// Pending character tokens accumulated in InTableText mode
    /// (§13.2.6.4.10). Flushed when a non-character token is seen.
    pub pending_table_text: String,
    /// The stack of template insertion modes (§13.2.6.4.19). Pushed when
    /// entering `<template>` content, popped when leaving.
    pub template_insertion_modes: Vec<InsertionMode>,
    /// The "frameset-ok" flag (§13.2.6.1). Initially true; set to false by
    /// certain tokens that prevent subsequent `<frameset>`.
    pub frameset_ok: bool,
    /// The scripting flag (§13.2.6.1). Defaults to false for non-scripting
    /// parsers; affects `<noscript>` handling and template content.
    pub scripting_flag: bool,
    /// Parse errors accumulated during tree construction (§13.2.6).
    pub errors: Vec<ParseError>,
    /// Whether the next U+000A LF character token should be ignored.
    /// Set by `<pre>`/`<listing>` start tags per §13.2.6.4.7: "If the next
    /// token is a U+000A LINE FEED (LF) character token, then ignore that
    /// token and move on to the next one."
    pub skip_next_lf: bool,
    /// Whether the Document is in quirks mode (§13.2.6.4.1). Set by the
    /// DOCTYPE token in Initial mode, or by the "anything else" branch of
    /// Initial mode (no DOCTYPE → quirks). Affects `<table>` handling in
    /// InBody (§13.2.6.4.7: in quirks mode, `<p>` is not closed before a
    /// `<table>`).
    pub quirks_mode: bool,
}

impl HtmlTreeConstructor {
    /// Create a new tree constructor that will build into `document`.
    ///
    /// Per §13.2.6.1, the initial insertion mode is `Initial`, the
    /// frameset-ok flag is true, and the scripting flag defaults to false.
    pub fn new(document: Rc<RefCell<Node>>) -> Self {
        Self {
            document,
            open_elements: Vec::new(),
            active_formatting_elements: Vec::new(),
            insertion_mode: InsertionMode::Initial,
            original_insertion_mode: None,
            head_element: None,
            form_element: None,
            foster_parenting: false,
            pending_table_text: String::new(),
            template_insertion_modes: Vec::new(),
            frameset_ok: true,
            scripting_flag: false,
            errors: Vec::new(),
            skip_next_lf: false,
            quirks_mode: false,
        }
    }

    /// Return the current node (§13.2.6.2).
    ///
    /// The current node is the top of the open elements stack. If the
    /// stack is empty (before any element is pushed), the current node is
    /// the Document itself.
    pub fn current_node(&self) -> Rc<RefCell<Node>> {
        self.open_elements
            .last()
            .cloned()
            .unwrap_or_else(|| self.document.clone())
    }

    /// Whether the adjusted current node is in foreign content (i.e., not
    /// in the HTML namespace).
    ///
    /// Used by the tokenizer to decide between CDATA section state and
    /// bogus comment state when encountering `<![CDATA[` (§13.2.5.42).
    /// Returns `false` when the stack is empty (no adjusted current node)
    /// or the current node is in the HTML namespace.
    pub fn current_node_in_foreign_content(&self) -> bool {
        match self.open_elements.last() {
            Some(node) => {
                let n = node.borrow();
                matches!(&n.kind, muskitty_dom::NodeKind::Element(e)
                    if e.namespace != muskitty_dom::Namespace::Html)
            }
            None => false,
        }
    }

    /// Feed a single token to the tree construction state machine.
    ///
    /// Dispatches the token to the handler for the current insertion mode.
    /// If the handler returns `Step::Reprocess`, the same token is fed again
    /// to the (now switched) insertion mode. This loop terminates because
    /// every reprocess step must change `insertion_mode` or return `Done`.
    ///
    /// The `tokenizer` is passed so handlers can switch the tokenizer's
    /// content model (e.g. to RCDATA for `<title>`, per §13.2.6.4.4).
    pub fn run(&mut self, token: &Token, tokenizer: &mut dyn Tokenizer) {
        // §13.2.6.4.7: pre/listing start tags cause the parser to skip a
        // single leading U+000A LF character token (authoring convenience).
        if self.skip_next_lf {
            self.skip_next_lf = false;
            if let Token::Character('\n') = token {
                return;
            }
        }
        let mut reprocess_count = 0u32;
        loop {
            match dispatch::dispatch(self, token, tokenizer) {
                dispatch::Step::Done => return,
                dispatch::Step::Reprocess => {
                    reprocess_count += 1;
                    if reprocess_count > 50 {
                        panic!(
                            "reprocess loop on token {:?}, insertion_mode {:?}",
                            token, self.insertion_mode
                        );
                    }
                    continue;
                }
            }
        }
    }

    /// Run the "stop parsing" finalization step (§13.2.7 step 4): pop all
    /// nodes off the stack of open elements. This fires the maybe-clone
    /// hook (§4.10.10) for any open `<option>` elements, ensuring their
    /// content is mirrored into `<selectedcontent>` before the document
    /// is returned.
    pub fn finalize(&mut self) {
        while !self.open_elements.is_empty() {
            helpers::pop_open_element(self);
        }
    }
}
