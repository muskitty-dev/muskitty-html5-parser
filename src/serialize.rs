//! HTML fragment serialization (WHATWG §13.6.4 / §13.6.5).
//!
//! Converts a DOM tree back into an HTML string, matching the browser's
//! `innerHTML` / `outerHTML` semantics: void elements have no closing tag,
//! `<template>` serializes its content, text escaping depends on the
//! parent element's content model (raw text / escapable raw text / normal),
//! and attributes are emitted in document order with `"` quoting.
//!
//! # References
//!
//! - §13.6.4: `Element.innerHTML` / `Element.outerHTML` getters.
//! - §13.6.5: The HTML fragment serialization algorithm (escaping modes,
//!   void elements, attribute escaping).

use std::cell::RefCell;
use std::rc::Rc;

use muskitty_dom::{adopt_node, append_child, drain_children, Node, NodeKind, NodeType};

use crate::parse_fragment;

/// How text children of an element are escaped during serialization
/// (§13.6.5). Determined by the parent element's content model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapingMode {
    /// Escape `&`, no-break space, `<`, `>`, NUL.
    Normal,
    /// Emit verbatim (style, script, xmp, iframe, noembed, noframes,
    /// plaintext content).
    RawText,
    /// Escape `&`, `<`, `>` and NUL but not no-break space (textarea, title).
    EscapableRawText,
}

/// Void elements (§13.6.5): serialized without a closing tag.
fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "br"
            | "col"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// The escaping mode applied to an element's text children (§13.6.5).
fn escaping_mode_for_name(local_name: &str) -> EscapingMode {
    match local_name {
        "style" | "xmp" | "iframe" | "noembed" | "noframes" | "plaintext" | "script" => {
            EscapingMode::RawText
        }
        "textarea" | "title" => EscapingMode::EscapableRawText,
        _ => EscapingMode::Normal,
    }
}

/// `Element.innerHTML` getter (§13.6.4): serialize the node's children,
/// using the node as the fragment-serialization parent (so e.g.
/// `textarea.innerHTML` escapes its text).
pub fn inner_html(node: &Rc<RefCell<Node>>) -> String {
    let mut out = String::new();
    let borrowed = node.borrow();
    let mode = borrowed
        .kind
        .as_element()
        .map(|e| escaping_mode_for_name(&e.local_name))
        .unwrap_or(EscapingMode::Normal);
    // A template element serializes its content DocumentFragment, not its
    // (empty) child list.
    let is_template = matches!(
        &borrowed.kind,
        NodeKind::Element(e)
            if e.namespace == muskitty_dom::Namespace::Html && e.local_name == "template"
    );
    let children: Vec<Rc<RefCell<Node>>> = if is_template {
        match &borrowed.kind {
            NodeKind::Element(e) => e
                .template_content
                .as_ref()
                .map(|c| c.borrow().child_nodes().to_vec())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    } else {
        borrowed.child_nodes().to_vec()
    };
    drop(borrowed);
    for child in &children {
        serialize_node(child, mode, &mut out);
    }
    out
}

/// `Element.outerHTML` getter (§13.6.4): serialize the node itself with
/// null as the fragment parent (i.e. normal escaping mode).
pub fn outer_html(node: &Rc<RefCell<Node>>) -> String {
    let mut out = String::new();
    serialize_node(node, EscapingMode::Normal, &mut out);
    out
}

/// `Element.innerHTML` setter (§13.6.4): parse `html` with `node` as the
/// fragment context, remove the node's existing children, then append the
/// parsed nodes (adopting them into the node's document).
pub fn set_inner_html(node: &Rc<RefCell<Node>>, html: &str) {
    let fragment = parse_fragment(html, node);
    let doc = node.borrow().owner_document.upgrade();
    // Remove existing children (they are detached from `node`'s list; the
    // stale `parent_node` weak refs are harmless since nothing references
    // the removed nodes anymore).
    drain_children(node);
    let new_children = drain_children(&fragment);
    for child in new_children {
        if let Some(d) = &doc {
            adopt_node(&child, d);
        }
        let _ = append_child(node, child);
    }
}

/// Recursively serialize one node (§13.6.5). `mode` is the escaping mode
/// inherited from the parent (used for text nodes and fragment children).
fn serialize_node(node: &Rc<RefCell<Node>>, mode: EscapingMode, out: &mut String) {
    let borrowed = node.borrow();
    match &borrowed.kind {
        NodeKind::Element(e) => {
            let is_template =
                e.namespace == muskitty_dom::Namespace::Html && e.local_name == "template";
            let local_name = e.local_name.clone();
            out.push('<');
            out.push_str(&local_name);
            for attr in &e.attributes {
                out.push(' ');
                if let Some(prefix) = &attr.prefix {
                    out.push_str(prefix);
                    out.push(':');
                }
                out.push_str(&attr.local_name);
                out.push_str("=\"");
                escape_attribute_value(&attr.value, out);
                out.push('"');
            }
            out.push('>');
            if is_void_element(&local_name) {
                return;
            }
            let child_mode = escaping_mode_for_name(&local_name);
            // Extract the children while the borrow is live, then release it
            // before recursing (a template serializes its content fragment).
            let children: Vec<Rc<RefCell<Node>>> = if is_template {
                e.template_content
                    .as_ref()
                    .map(|c| c.borrow().child_nodes().to_vec())
                    .unwrap_or_default()
            } else {
                borrowed.child_nodes().to_vec()
            };
            drop(borrowed);
            for child in &children {
                serialize_node(child, child_mode, out);
            }
            out.push_str("</");
            out.push_str(&local_name);
            out.push('>');
        }
        NodeKind::Text(t) => escape_text(&t.data, mode, out),
        NodeKind::Comment(c) => {
            out.push_str("<!--");
            out.push_str(&c.data);
            out.push_str("-->");
        }
        NodeKind::ProcessingInstruction(pi) => {
            out.push_str("<?");
            out.push_str(&pi.target);
            out.push(' ');
            out.push_str(&pi.data);
            out.push_str("?>");
        }
        NodeKind::DocumentType(d) => {
            out.push_str("<!DOCTYPE ");
            out.push_str(&d.name);
            if !d.public_id.is_empty() || !d.system_id.is_empty() {
                out.push_str(" \"");
                out.push_str(&d.public_id);
                out.push_str("\" \"");
                out.push_str(&d.system_id);
                out.push('"');
            }
            out.push('>');
        }
        // Document / DocumentFragment: serialize each child with the same
        // inherited escaping mode.
        _ if matches!(
            borrowed.node_type,
            NodeType::Document | NodeType::DocumentFragment
        ) =>
        {
            let children: Vec<Rc<RefCell<Node>>> = borrowed.child_nodes().to_vec();
            drop(borrowed);
            for child in &children {
                serialize_node(child, mode, out);
            }
        }
        _ => {}
    }
}

/// Escape character data (§13.6.5 step for Text nodes).
fn escape_text(s: &str, mode: EscapingMode, out: &mut String) {
    for c in s.chars() {
        match (c, mode) {
            ('\u{0}', _) => out.push('\u{FFFD}'),
            ('&', EscapingMode::Normal) => out.push_str("&amp;"),
            ('&', EscapingMode::EscapableRawText) => out.push_str("&amp;"),
            ('\u{A0}', EscapingMode::Normal) => out.push_str("&nbsp;"),
            ('<', EscapingMode::Normal) => out.push_str("&lt;"),
            ('<', EscapingMode::EscapableRawText) => out.push_str("&lt;"),
            ('>', EscapingMode::Normal) => out.push_str("&gt;"),
            ('>', EscapingMode::EscapableRawText) => out.push_str("&gt;"),
            (c, _) => out.push(c),
        }
    }
}

/// Escape an attribute value (§13.6.5).
fn escape_attribute_value(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\u{0}' => out.push('\u{FFFD}'),
            '&' => out.push_str("&amp;"),
            '\u{A0}' => out.push_str("&nbsp;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muskitty_dom::{Attribute, Node};

    fn div(html: &str) -> Rc<RefCell<Node>> {
        let doc = Node::new_document();
        let node = Node::new_element_html("div", vec![], &doc);
        set_inner_html(&node, html);
        node
    }

    #[test]
    fn escapes_text_in_normal_mode() {
        // `&`, `<`, `>` and NBSP are escaped; a `"` inside a text node is not.
        let node = div("a & b < c > d \u{A0}");
        assert_eq!(inner_html(&node), "a &amp; b &lt; c &gt; d &nbsp;");
    }

    #[test]
    fn raw_text_not_escaped() {
        let doc = Node::new_document();
        let script = Node::new_element_html("script", vec![], &doc);
        set_inner_html(&script, "if (a<b && c) {}");
        assert_eq!(
            inner_html(&script),
            "if (a<b && c) {}",
            "script is raw text"
        );
        assert_eq!(outer_html(&script), "<script>if (a<b && c) {}</script>");
    }

    #[test]
    fn escapable_raw_text_escapes_amp_and_lt() {
        let doc = Node::new_document();
        let textarea = Node::new_element_html("textarea", vec![], &doc);
        set_inner_html(&textarea, "a & b < c \u{A0}");
        // Escapable raw text escapes & < > but not NBSP.
        assert_eq!(inner_html(&textarea), "a &amp; b &lt; c \u{A0}");
    }

    #[test]
    fn void_element_has_no_closing_tag() {
        let node = div("<br><img src=x>");
        assert_eq!(inner_html(&node), "<br><img src=\"x\">");
    }

    #[test]
    fn attribute_escaping() {
        let node = div("<div a=\"x&amp;&quot;y\"></div>");
        assert_eq!(inner_html(&node), "<div a=\"x&amp;&quot;y\"></div>");
    }

    #[test]
    fn attribute_document_order_preserved() {
        let doc = Node::new_document();
        let el = Node::new_element_html(
            "div",
            vec![Attribute::new("id", "a"), Attribute::new("class", "b")],
            &doc,
        );
        assert_eq!(outer_html(&el), "<div id=\"a\" class=\"b\"></div>");
    }

    #[test]
    fn template_serializes_content() {
        let node = div("<template><span>hi</span></template>");
        assert_eq!(inner_html(&node), "<template><span>hi</span></template>");
    }

    #[test]
    fn outer_html_roundtrip() {
        let node = div("<p class=\"x\">hello</p>");
        let p = node.borrow().child_nodes()[0].clone();
        assert_eq!(outer_html(&p), "<p class=\"x\">hello</p>");
    }

    #[test]
    fn set_inner_html_replaces_existing_children() {
        let node = div("<span>old</span>");
        assert_eq!(inner_html(&node), "<span>old</span>");
        set_inner_html(&node, "<b>new</b>");
        assert_eq!(inner_html(&node), "<b>new</b>");
    }

    #[test]
    fn set_inner_html_adopts_into_node_document() {
        // Hold the document so the element's Weak owner_document stays valid.
        let doc = Node::new_document();
        let node = Node::new_element_html("div", vec![], &doc);
        let doc_ptr = Rc::as_ptr(&doc);
        set_inner_html(&node, "<span>hi</span>");
        let span = node.borrow().child_nodes()[0].clone();
        let span_doc = span.borrow().owner_document.upgrade().unwrap();
        assert_eq!(
            Rc::as_ptr(&span_doc),
            doc_ptr,
            "adopted into node's document"
        );
    }

    #[test]
    fn comment_and_doctype_serialized() {
        let doc = Node::new_document();
        let el = Node::new_element_html("div", vec![], &doc);
        set_inner_html(&el, "<!-- c -->");
        assert_eq!(inner_html(&el), "<!-- c -->");
    }
}
