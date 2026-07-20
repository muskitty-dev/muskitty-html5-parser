//! Skeleton tests for the tree construction entry point.
//!
//! These verify that the `parse()` function produces a Document with the
//! expected top-level children for trivial inputs. Full tree construction
//! correctness is validated against html5lib in Phase 5.
//!
//! Per WHATWG §13.2.6.3 (Before html), any non-DOCTYPE/comment/whitespace
//! token — including EOF — triggers creation of the `<html>` element, so
//! even empty input yields a Document with an `<html>` child (which in turn
//! contains auto-created `<head>` and `<body>` per §13.2.6.4/§13.2.6.7).

use muskitty_dom::NodeType;
use muskitty_html5_parser::parse;

#[test]
fn parse_empty_string_produces_document_with_html() {
    let doc = parse("");
    assert_eq!(doc.borrow().node_type, NodeType::Document);
    // Per §13.2.6.3, EOF in Initial mode falls through to BeforeHtml, whose
    // anything-else branch creates an <html> element. Empty input therefore
    // yields a Document with exactly one child (the <html> element).
    assert_eq!(doc.borrow().child_count(), 1);
    let html = doc.borrow().first_child().unwrap();
    assert_eq!(html.borrow().node_type, NodeType::Element);
    // HTML-namespace element node_name is uppercase per DOM §6.1.
    assert_eq!(html.borrow().node_name, "HTML");
}

#[test]
fn parse_doctype_produces_document_type_then_html() {
    let doc = parse("<!DOCTYPE html>");
    let children = doc.borrow().child_nodes().to_vec();
    // DocumentType first, then <html> (auto-created on EOF).
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].borrow().node_type, NodeType::DocumentType);
    assert_eq!(children[1].borrow().node_name, "HTML");
}

#[test]
fn parse_minimal_html_produces_html_element() {
    let doc = parse("<html></html>");
    // BeforeHtml mode creates an <html> element and attaches it to the
    // Document per §13.2.6.3.
    let has_html = doc
        .borrow()
        .child_nodes()
        .iter()
        .any(|c| c.borrow().node_name == "HTML");
    assert!(has_html, "Document should have an <HTML> child");
}
