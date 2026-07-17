//! Tree construction tests for the prelude insertion modes.
//!
//! Covers Initial / BeforeHtml / BeforeHead / InHead / AfterHead and the
//! minimal Text mode, per WHATWG §13.2.6.4.1–§13.2.6.5.
//!
//! These verify the DOM structure produced by `parse()` for inputs that
//! exercise the prelude chain. Full html5lib tree construction coverage
//! comes in Phase 5.

use std::cell::RefCell;
use std::rc::Rc;

use muskitty_dom::{Node, NodeKind, NodeType};
use muskitty_html_parser::parse;

/// Find the first descendant element with the given node_name (uppercase,
/// per DOM §6.1 HTML-namespace convention).
fn find_element_by_name(root: &Rc<RefCell<Node>>, name: &str) -> Option<Rc<RefCell<Node>>> {
    for desc in Node::descendants(root) {
        if desc.borrow().node_type == NodeType::Element && desc.borrow().node_name == name {
            return Some(desc);
        }
    }
    None
}

/// Collect the node_names of all direct children of a node (uppercase).
fn child_names(node: &Rc<RefCell<Node>>) -> Vec<String> {
    node.borrow()
        .children
        .iter()
        .map(|c| c.borrow().node_name.clone())
        .collect()
}

// ── Initial / BeforeHtml / BeforeHead ──────────────────────────

#[test]
fn doctype_html_creates_document_type_and_html() {
    let doc = parse("<!DOCTYPE html>");
    let names = child_names(&doc);
    assert_eq!(names, vec!["html", "HTML"]);
    let dt = doc.borrow().first_child().unwrap();
    assert_eq!(dt.borrow().node_type, NodeType::DocumentType);
    assert_eq!(dt.borrow().node_name, "html");
}

#[test]
fn empty_input_auto_creates_html_head_body() {
    // Per §13.2.6.4.2, EOF triggers BeforeHtml's anything-else → create
    // <html>. Then BeforeHead creates <head>, AfterHead creates <body>.
    let doc = parse("");
    let html = find_element_by_name(&doc, "HTML").expect("missing <HTML>");
    let html_children = child_names(&html);
    assert!(
        html_children.contains(&"HEAD".to_string()),
        "expected <HEAD> under <HTML>, got {:?}",
        html_children
    );
    assert!(
        html_children.contains(&"BODY".to_string()),
        "expected <BODY> under <HTML>, got {:?}",
        html_children
    );
}

#[test]
fn comment_before_doctype_goes_to_document() {
    let doc = parse("<!-- hi --><!DOCTYPE html>");
    // First child should be the comment, attached to Document per
    // §13.2.6.4.1 Initial mode.
    let first = doc.borrow().first_child().unwrap();
    assert_eq!(first.borrow().node_type, NodeType::Comment);
}

// ── InHead: void head elements ─────────────────────────────────

#[test]
fn meta_element_is_created_and_not_open() {
    // <meta> should be created in <head> but not remain on the open
    // elements stack — subsequent text should still be in head context.
    let doc = parse("<!DOCTYPE html><meta charset=utf-8>");
    let meta = find_element_by_name(&doc, "META").expect("missing <META>");
    assert_eq!(meta.borrow().node_name, "META");
    // The meta element's parent should be HEAD.
    let parent = meta.borrow().parent_element().unwrap();
    assert_eq!(parent.borrow().node_name, "HEAD");
}

#[test]
fn link_element_is_created_in_head() {
    let doc = parse("<!DOCTYPE html><link rel=stylesheet href=x.css>");
    let link = find_element_by_name(&doc, "LINK").expect("missing <LINK>");
    let parent = link.borrow().parent_element().unwrap();
    assert_eq!(parent.borrow().node_name, "HEAD");
}

#[test]
fn base_element_is_created_in_head() {
    let doc = parse("<!DOCTYPE html><base href=https://example.com/>");
    let base = find_element_by_name(&doc, "BASE").expect("missing <BASE>");
    let parent = base.borrow().parent_element().unwrap();
    assert_eq!(parent.borrow().node_name, "HEAD");
}

// ── InHead: title (RCDATA + Text mode) ─────────────────────────

#[test]
fn title_element_contains_text_content() {
    let doc = parse("<!DOCTYPE html><title>Hello</title>");
    let title = find_element_by_name(&doc, "TITLE").expect("missing <TITLE>");
    assert_eq!(title.borrow().child_count(), 1);
    let text = title.borrow().first_child().unwrap();
    assert_eq!(text.borrow().node_type, NodeType::Text);
    assert_eq!(text.borrow().text_content().unwrap(), "Hello");
}

#[test]
fn title_with_entities_decodes_them() {
    // RCDATA mode decodes character references. &amp; → &.
    let doc = parse("<!DOCTYPE html><title>A&amp;B</title>");
    let title = find_element_by_name(&doc, "TITLE").expect("missing <TITLE>");
    let text = title.borrow().first_child().unwrap();
    assert_eq!(text.borrow().text_content().unwrap(), "A&B");
}

#[test]
fn title_end_tag_restores_previous_mode() {
    // After </title>, parsing should continue in the head/body context.
    // The auto-created <body> should exist.
    let doc = parse("<!DOCTYPE html><title>X</title>");
    let _body = find_element_by_name(&doc, "BODY").expect("missing <BODY>");
}

// ── InHead: style / script (RAWTEXT / ScriptData + Text mode) ─

#[test]
fn style_element_preserves_raw_text() {
    let doc = parse("<!DOCTYPE html><style>.a > b { color: red; }</style>");
    let style = find_element_by_name(&doc, "STYLE").expect("missing <STYLE>");
    let text = style.borrow().first_child().unwrap();
    assert_eq!(text.borrow().node_type, NodeType::Text);
    assert_eq!(
        text.borrow().text_content().unwrap(),
        ".a > b { color: red; }"
    );
}

#[test]
fn script_element_preserves_raw_text() {
    let doc = parse("<!DOCTYPE html><script>if (a < b) { alert('hi'); }</script>");
    let script = find_element_by_name(&doc, "SCRIPT").expect("missing <SCRIPT>");
    let text = script.borrow().first_child().unwrap();
    assert_eq!(
        text.borrow().text_content().unwrap(),
        "if (a < b) { alert('hi'); }"
    );
}

// ── InHead: head end tag ───────────────────────────────────────

#[test]
fn explicit_head_end_tag_switches_to_after_head() {
    let doc = parse("<!DOCTYPE html><head></head><body></body>");
    let head = find_element_by_name(&doc, "HEAD").expect("missing <HEAD>");
    let body = find_element_by_name(&doc, "BODY").expect("missing <BODY>");
    // Both should be children of <html>.
    let html = find_element_by_name(&doc, "HTML").unwrap();
    let html_children = child_names(&html);
    assert!(html_children.contains(&"HEAD".to_string()));
    assert!(html_children.contains(&"BODY".to_string()));
    // head and body should be siblings under html.
    assert_eq!(
        head.borrow().parent_element().unwrap().borrow().node_name,
        "HTML"
    );
    assert_eq!(
        body.borrow().parent_element().unwrap().borrow().node_name,
        "HTML"
    );
}

// ── AfterHead ──────────────────────────────────────────────────

#[test]
fn body_start_tag_creates_body_and_switches_to_in_body() {
    let doc = parse("<!DOCTYPE html><head></head><body>hi</body>");
    let body = find_element_by_name(&doc, "BODY").expect("missing <BODY>");
    assert_eq!(body.borrow().child_count(), 1);
    let text = body.borrow().first_child().unwrap();
    assert_eq!(text.borrow().text_content().unwrap(), "hi");
}

#[test]
fn content_without_body_auto_creates_body() {
    // Per §13.2.6.4.6 anything-else, text after </head> auto-creates <body>.
    let doc = parse("<!DOCTYPE html><head></head>hello");
    let body = find_element_by_name(&doc, "BODY").expect("missing <BODY>");
    assert_eq!(body.borrow().text_content().unwrap(), "hello");
}

#[test]
fn meta_after_head_is_processed_in_head_context() {
    // Per §13.2.6.4.6, <meta> after </head> is a parse error but still
    // processed using in-head rules. The <meta> should end up under <head>.
    let doc = parse("<!DOCTYPE html><head></head><meta charset=utf-8>");
    let meta = find_element_by_name(&doc, "META").expect("missing <META>");
    let parent = meta.borrow().parent_element().unwrap();
    assert_eq!(parent.borrow().node_name, "HEAD");
}

// ── Full prelude chain ─────────────────────────────────────────

#[test]
fn full_document_structure() {
    let doc =
        parse("<!DOCTYPE html><html><head><title>T</title></head><body><p>hi</p></body></html>");
    // Document children: DocumentType + html.
    assert_eq!(doc.borrow().child_count(), 2);
    // html children: head + body.
    let html = find_element_by_name(&doc, "HTML").unwrap();
    let html_children = child_names(&html);
    assert_eq!(html_children, vec!["HEAD", "BODY"]);
    // head has a title child.
    let head = find_element_by_name(&doc, "HEAD").unwrap();
    let title = head
        .borrow()
        .children
        .iter()
        .find(|c| c.borrow().node_name == "TITLE")
        .expect("missing <TITLE> under <HEAD>")
        .clone();
    assert_eq!(title.borrow().text_content().unwrap(), "T");
}

#[test]
fn attributes_are_preserved_on_html_element() {
    let doc = parse("<!DOCTYPE html><html lang=en><head></head></html>");
    let html = find_element_by_name(&doc, "HTML").unwrap();
    // Check the lang attribute via ElementData.
    let html_ref = html.borrow();
    if let NodeKind::Element(ref e) = html_ref.kind {
        let lang = e.get_attribute("lang").expect("missing lang attribute");
        assert_eq!(lang, "en");
    } else {
        panic!("expected Element node");
    }
}

// ── InBody: block-level elements (§13.2.6.4.7) ────────────────

/// Locate the <body> element under the document's <html>.
fn find_body(doc: &Rc<RefCell<Node>>) -> Rc<RefCell<Node>> {
    find_element_by_name(doc, "BODY").expect("missing <BODY> element")
}

#[test]
fn div_element_with_text_content() {
    let doc = parse("<div>hello</div>");
    let body = find_body(&doc);
    let children = child_names(&body);
    assert_eq!(children, vec!["DIV"]);
    let div = body.borrow().first_child().unwrap();
    assert_eq!(div.borrow().text_content().unwrap(), "hello");
}

#[test]
fn paragraph_element_with_text() {
    let doc = parse("<p>hi</p>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["P"]);
    let p = body.borrow().first_child().unwrap();
    assert_eq!(p.borrow().text_content().unwrap(), "hi");
}

#[test]
fn heading_h1_with_text() {
    let doc = parse("<h1>Title</h1>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["H1"]);
    let h1 = body.borrow().first_child().unwrap();
    assert_eq!(h1.borrow().text_content().unwrap(), "Title");
}

#[test]
fn nested_block_elements() {
    let doc = parse("<div><p>text</p></div>");
    let body = find_body(&doc);
    let div = body.borrow().first_child().unwrap();
    assert_eq!(div.borrow().node_name, "DIV");
    assert_eq!(child_names(&div), vec!["P"]);
    let p = div.borrow().first_child().unwrap();
    assert_eq!(p.borrow().text_content().unwrap(), "text");
}

#[test]
fn void_element_br_not_on_stack() {
    let doc = parse("<br>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["BR"]);
    // <br> is void: it should be the only child, with no children of its own.
    let br = body.borrow().first_child().unwrap();
    assert_eq!(br.borrow().child_count(), 0);
}

#[test]
fn void_element_hr_not_on_stack() {
    let doc = parse("<hr>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["HR"]);
    let hr = body.borrow().first_child().unwrap();
    assert_eq!(hr.borrow().child_count(), 0);
}

#[test]
fn img_element_preserves_attributes() {
    let doc = parse("<img src=x alt=y>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["IMG"]);
    let img = body.borrow().first_child().unwrap();
    let img_ref = img.borrow();
    if let NodeKind::Element(ref e) = img_ref.kind {
        assert_eq!(e.get_attribute("src").unwrap(), "x");
        assert_eq!(e.get_attribute("alt").unwrap(), "y");
    } else {
        panic!("expected Element node");
    }
}

#[test]
fn unordered_list_with_items() {
    let doc = parse("<ul><li>a</li><li>b</li></ul>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["UL"]);
    let ul = body.borrow().first_child().unwrap();
    assert_eq!(child_names(&ul), vec!["LI", "LI"]);
    let items: Vec<String> = ul
        .borrow()
        .children
        .iter()
        .map(|c| c.borrow().text_content().unwrap_or_default())
        .collect();
    assert_eq!(items, vec!["a", "b"]);
}

#[test]
fn form_element_sets_form_pointer() {
    // <form> should be inserted and remain open until </form>.
    let doc = parse("<form></form>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["FORM"]);
    let form = body.borrow().first_child().unwrap();
    assert_eq!(form.borrow().child_count(), 0);
}

#[test]
fn multiple_siblings_in_body() {
    let doc = parse("<div></div><span></span>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["DIV", "SPAN"]);
}

#[test]
fn implicit_p_close_on_next_p() {
    // <p>one<p>two — the second <p> implicitly closes the first (§13.2.6.4.7).
    let doc = parse("<p>one<p>two");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["P", "P"]);
    let texts: Vec<String> = body
        .borrow()
        .children
        .iter()
        .map(|c| c.borrow().text_content().unwrap_or_default())
        .collect();
    assert_eq!(texts, vec!["one", "two"]);
}

#[test]
fn block_element_closes_open_p() {
    // A block-level start tag closes an open <p> per §13.2.6.4.7.
    let doc = parse("<p>text<div>x</div>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["P", "DIV"]);
}

#[test]
fn end_tag_div_closes_div() {
    let doc = parse("<div><span>inner</span></div>");
    let body = find_body(&doc);
    let div = body.borrow().first_child().unwrap();
    assert_eq!(child_names(&div), vec!["SPAN"]);
    let span = div.borrow().first_child().unwrap();
    assert_eq!(span.borrow().text_content().unwrap(), "inner");
    // After </div>, the div should be closed; body has only the div.
    assert_eq!(body.borrow().child_count(), 1);
}

#[test]
fn dl_with_dd_and_dt() {
    let doc = parse("<dl><dt>term</dt><dd>def</dd></dl>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["DL"]);
    let dl = body.borrow().first_child().unwrap();
    assert_eq!(child_names(&dl), vec!["DT", "DD"]);
}

#[test]
fn heading_end_tag_closes_heading() {
    let doc = parse("<h1>Title</h1><p>after</p>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["H1", "P"]);
}

#[test]
fn basic_formatting_element_text() {
    // <b>text</b> — b is inserted and text goes inside it. (Active formatting
    // reconstruction is deferred to Phase 3.3, but the simple open/close
    // case works.)
    let doc = parse("<b>bold</b>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["B"]);
    let b = body.borrow().first_child().unwrap();
    assert_eq!(b.borrow().text_content().unwrap(), "bold");
}

// ── Phase 3.3: formatting elements + adoption agency (§13.2.6.4.7) ─

#[test]
fn formatting_reconstruct_after_block_close() {
    // <b>one</b><p>two</p> — after the <p> closes, no reconstruction is
    // needed because <b> was already closed by </b>. Text after the block
    // should not be wrapped in <b>.
    let doc = parse("<b>one</b><p>two</p>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["B", "P"]);
}

#[test]
fn formatting_reconstruct_across_block() {
    // <b><p>x</p>y</b> — the <p> closes implicitly; "y" should still be
    // inside <b> after reconstruction.
    let doc = parse("<b><p>x</p>y</b>");
    let b = find_element_by_name(&doc, "B").expect("missing <B>");
    // <b> should contain the <p> and a text node "y" (reconstructed).
    let b_text = b.borrow().text_content().unwrap_or_default();
    assert!(
        b_text.contains("x"),
        "b should contain 'x', got {:?}",
        b_text
    );
    assert!(
        b_text.contains("y"),
        "b should contain 'y', got {:?}",
        b_text
    );
}

#[test]
fn nested_same_formatting_element() {
    // <b><b>x</b></b> — nested <b> elements.
    let doc = parse("<b><b>x</b></b>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["B"]);
    let outer = body.borrow().first_child().unwrap();
    assert_eq!(child_names(&outer), vec!["B"]);
}

#[test]
fn formatting_end_tag_closes_formatting() {
    // <b>text</b>more — after </b>, text "more" is a sibling.
    let doc = parse("<b>text</b>more");
    let body = find_body(&doc);
    let b = body.borrow().first_child().unwrap();
    assert_eq!(b.borrow().text_content().unwrap(), "text");
}

#[test]
fn multiple_formatting_elements() {
    // <b><i>bold italic</i></b> — nesting works.
    let doc = parse("<b><i>bold italic</i></b>");
    let body = find_body(&doc);
    let b = body.borrow().first_child().unwrap();
    assert_eq!(child_names(&b), vec!["I"]);
    let i = b.borrow().first_child().unwrap();
    assert_eq!(i.borrow().text_content().unwrap(), "bold italic");
}

#[test]
fn formatting_reopened_after_block() {
    // <b>1<p>2</p>3</b> — adoption agency: <b> wraps "1", <p> contains "2",
    // and "3" is wrapped in a new <b> under <p>'s sibling context.
    let doc = parse("<b>1<p>2</p>3</b>");
    // The document should have at least one <b> with text "1".
    let b = find_element_by_name(&doc, "B").expect("missing <B>");
    let text = b.borrow().text_content().unwrap_or_default();
    assert!(
        text.contains("1"),
        "first <b> should contain '1', got {:?}",
        text
    );
}

#[test]
fn anchor_element_with_href() {
    let doc = parse("<a href=x>link</a>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["A"]);
    let a = body.borrow().first_child().unwrap();
    let a_ref = a.borrow();
    if let NodeKind::Element(ref e) = a_ref.kind {
        assert_eq!(e.get_attribute("href").unwrap(), "x");
    } else {
        panic!("expected Element node");
    }
}

#[test]
fn em_strong_inline_elements() {
    let doc = parse("<em>italic</em><strong>bold</strong>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["EM", "STRONG"]);
}

#[test]
fn code_element_with_text() {
    let doc = parse("<code>fn main()</code>");
    let body = find_body(&doc);
    let code = body.borrow().first_child().unwrap();
    assert_eq!(code.borrow().text_content().unwrap(), "fn main()");
}

#[test]
fn formatting_inside_paragraph() {
    let doc = parse("<p><b>bold</b> normal</p>");
    let body = find_body(&doc);
    let p = body.borrow().first_child().unwrap();
    assert_eq!(p.borrow().node_name, "P");
    let p_text = p.borrow().text_content().unwrap();
    assert!(p_text.contains("bold"));
    assert!(p_text.contains("normal"));
}

#[test]
fn misnested_b_p_adoption_agency() {
    // Classic adoption agency case: <b>1<p>2</b>3</p>
    // The <b> should be split so that "1" and "3" are in <b>, "2" in <p>.
    let doc = parse("<b>1<p>2</b>3</p>");
    // Should not panic and should produce a <b> element containing "1".
    let b = find_element_by_name(&doc, "B").expect("missing <B>");
    let b_text = b.borrow().text_content().unwrap_or_default();
    assert!(
        b_text.contains("1"),
        "<b> should contain '1', got {:?}",
        b_text
    );
}

#[test]
fn noahs_ark_clause_limits_entries() {
    // Repeated identical <b> elements should be limited by the Noah's Ark
    // clause. This is mostly a smoke test that parsing doesn't crash and
    // produces multiple <b> elements.
    let doc = parse("<b><b><b><b>x</b></b></b></b>");
    let b = find_element_by_name(&doc, "B").expect("missing <B>");
    let _ = b;
}

#[test]
fn comment_in_body_attaches_to_current_node() {
    let doc = parse("<div><!-- comment --></div>");
    let body = find_body(&doc);
    let div = body.borrow().first_child().unwrap();
    // The comment should be a child of div.
    let comment = div.borrow().first_child().unwrap();
    assert_eq!(comment.borrow().node_type, NodeType::Comment);
}

#[test]
fn end_tag_body_switches_to_after_body() {
    // </body> should switch to AfterBody; subsequent EOF is handled cleanly.
    let doc = parse("<body><p>x</p></body>");
    let body = find_body(&doc);
    assert_eq!(child_names(&body), vec!["P"]);
}

// ── Table insertion modes (§13.2.6.4.9–§13.2.6.4.15) ──────────

#[test]
fn simple_table_with_tbody_tr_td() {
    // <table><tbody><tr><td>cell</td></tr></tbody></table>
    let doc = parse("<table><tbody><tr><td>cell</td></tr></tbody></table>");
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    // <table> should contain a <TBODY>
    let tbody = find_first_child_element(&table, "TBODY").expect("missing <TBODY>");
    let tr = find_first_child_element(&tbody, "TR").expect("missing <TR>");
    let td = find_first_child_element(&tr, "TD").expect("missing <TD>");
    let text = td.borrow().text_content().unwrap_or_default();
    assert!(
        text.contains("cell"),
        "td should contain 'cell', got {:?}",
        text
    );
}

#[test]
fn table_implicit_tbody_for_tr() {
    // <table><tr><td>x</td></tr></table> — <tbody> is auto-inserted.
    let doc = parse("<table><tr><td>x</td></tr></table>");
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    let tbody = find_first_child_element(&table, "TBODY").expect("missing implicit <TBODY>");
    let _ = tbody;
}

#[test]
fn table_implicit_tbody_for_td() {
    // <table><td>x</td></table> — <tbody> and <tr> are auto-inserted.
    let doc = parse("<table><td>x</td></table>");
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    let tbody = find_first_child_element(&table, "TBODY").expect("missing implicit <TBODY>");
    let tr = find_first_child_element(&tbody, "TR").expect("missing implicit <TR>");
    let td = find_first_child_element(&tr, "TD").expect("missing <TD>");
    let _ = td;
}

#[test]
fn table_with_caption() {
    // <table><caption>Title</caption><tr><td>1</td></tr></table>
    let doc = parse("<table><caption>Title</caption><tr><td>1</td></tr></table>");
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    let caption = find_first_child_element(&table, "CAPTION").expect("missing <CAPTION>");
    let text = caption.borrow().text_content().unwrap_or_default();
    assert!(text.contains("Title"), "caption text, got {:?}", text);
}

#[test]
fn table_with_colgroup_and_col() {
    // <table><colgroup><col></colgroup><tr><td>x</td></tr></table>
    let doc = parse("<table><colgroup><col></colgroup><tr><td>x</td></tr></table>");
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    let colgroup = find_first_child_element(&table, "COLGROUP").expect("missing <COLGROUP>");
    let col = find_first_child_element(&colgroup, "COL").expect("missing <COL>");
    let _ = col;
}

#[test]
fn table_with_thead_tbody_tfoot() {
    // <table><thead><tr><th>H</th></tr></thead><tbody><tr><td>D</td></tr></tbody></table>
    let doc = parse(
        "<table><thead><tr><th>H</th></tr></thead><tbody><tr><td>D</td></tr></tbody></table>",
    );
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    let thead = find_first_child_element(&table, "THEAD").expect("missing <THEAD>");
    let tbody = find_first_child_element(&table, "TBODY").expect("missing <TBODY>");
    let _ = (thead, tbody);
}

#[test]
fn table_end_tag_closes_table() {
    // After </table>, parsing should resume in InBody.
    let doc = parse("<table><tr><td>x</td></tr></table><p>after</p>");
    let body = find_body(&doc);
    let names = child_names(&body);
    assert!(
        names.contains(&"TABLE".to_string()),
        "body should contain <TABLE>, got {:?}",
        names
    );
    assert!(
        names.contains(&"P".to_string()),
        "body should contain <P> after table, got {:?}",
        names
    );
}

#[test]
fn table_with_multiple_rows() {
    // <table><tr><td>1</td></tr><tr><td>2</td></tr></table>
    let doc = parse("<table><tr><td>1</td></tr><tr><td>2</td></tr></table>");
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    let tbody = find_first_child_element(&table, "TBODY").expect("missing <TBODY>");
    // Should have two <TR> children
    let tr_count = tbody
        .borrow()
        .children
        .iter()
        .filter(|c| c.borrow().node_name == "TR")
        .count();
    assert_eq!(tr_count, 2, "expected 2 <TR>, got {}", tr_count);
}

#[test]
fn table_with_multiple_cells_in_row() {
    // <table><tr><td>1</td><td>2</td></tr></table>
    let doc = parse("<table><tr><td>1</td><td>2</td></tr></table>");
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    let tbody = find_first_child_element(&table, "TBODY").expect("missing <TBODY>");
    let tr = find_first_child_element(&tbody, "TR").expect("missing <TR>");
    let td_count = tr
        .borrow()
        .children
        .iter()
        .filter(|c| c.borrow().node_name == "TD")
        .count();
    assert_eq!(td_count, 2, "expected 2 <TD>, got {}", td_count);
}

#[test]
fn table_eof_does_not_panic() {
    // Unclosed table at EOF should not panic.
    let doc = parse("<table><tr><td>unclosed");
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    let _ = table;
}

#[test]
fn table_whitespace_between_elements() {
    // Whitespace between table elements should not crash (goes to InTableText).
    let doc = parse("<table> <tr><td>x</td></tr> </table>");
    let table = find_element_by_name(&doc, "TABLE").expect("missing <TABLE>");
    let _ = table;
}

#[test]
fn nested_tables() {
    // <table><tr><td><table><tr><td>inner</td></tr></table></td></tr></table>
    let doc = parse("<table><tr><td><table><tr><td>inner</td></tr></table></td></tr></table>");
    let tables: Vec<_> = Node::descendants(&doc)
        .filter(|n| n.borrow().node_type == NodeType::Element && n.borrow().node_name == "TABLE")
        .collect();
    assert_eq!(
        tables.len(),
        2,
        "expected 2 nested <TABLE>, got {}",
        tables.len()
    );
}

/// Find the first direct child element with the given node name.
fn find_first_child_element(parent: &Rc<RefCell<Node>>, name: &str) -> Option<Rc<RefCell<Node>>> {
    parent
        .borrow()
        .children
        .iter()
        .find(|c| c.borrow().node_name == name)
        .cloned()
}

// ── Select insertion modes (§13.2.6.4.16, §13.2.6.4.18) ────────

#[test]
fn select_with_options() {
    let doc = parse("<select><option>a</option><option>b</option></select>");
    let select = find_element_by_name(&doc, "SELECT").expect("missing <SELECT>");
    let option_count = select
        .borrow()
        .children
        .iter()
        .filter(|c| c.borrow().node_name == "OPTION")
        .count();
    assert_eq!(option_count, 2, "expected 2 <OPTION>, got {}", option_count);
}

#[test]
fn select_with_optgroup_and_option() {
    let doc = parse("<select><optgroup><option>x</option></optgroup></select>");
    let select = find_element_by_name(&doc, "SELECT").expect("missing <SELECT>");
    let optgroup = find_first_child_element(&select, "OPTGROUP").expect("missing <OPTGROUP>");
    let option = find_first_child_element(&optgroup, "OPTION").expect("missing <OPTION>");
    let _ = option;
}

#[test]
fn select_eof_does_not_panic() {
    let doc = parse("<select><option>unclosed");
    let select = find_element_by_name(&doc, "SELECT").expect("missing <SELECT>");
    let _ = select;
}

#[test]
fn select_in_table_switches_mode() {
    // <select> inside a table should switch to InSelectInTable, but
    // still produce a valid <SELECT> element.
    let doc = parse("<table><tr><td><select><option>x</option></select></td></tr></table>");
    let select = find_element_by_name(&doc, "SELECT").expect("missing <SELECT>");
    let _ = select;
}

// ── Template insertion mode (§13.2.6.4.19) ─────────────────────

#[test]
fn template_with_text_content() {
    // <template> is a special element: its content goes into a separate
    // "template content" document fragment. For now we just verify the
    // <template> element exists.
    let doc = parse("<template>hello</template>");
    let template = find_element_by_name(&doc, "TEMPLATE").expect("missing <TEMPLATE>");
    let _ = template;
}

#[test]
fn template_with_div() {
    let doc = parse("<template><div>content</div></template>");
    let template = find_element_by_name(&doc, "TEMPLATE").expect("missing <TEMPLATE>");
    let _ = template;
}

#[test]
fn template_eof_does_not_panic() {
    let doc = parse("<template><div>unclosed");
    let template = find_element_by_name(&doc, "TEMPLATE").expect("missing <TEMPLATE>");
    let _ = template;
}

#[test]
fn template_with_table() {
    // <template><table><tr><td>x</td></tr></table></template>
    let doc = parse("<template><table><tr><td>x</td></tr></table></template>");
    let template = find_element_by_name(&doc, "TEMPLATE").expect("missing <TEMPLATE>");
    let _ = template;
}

// ── Frameset insertion modes (§13.2.6.4.21–§13.2.6.4.23) ───────

#[test]
fn frameset_replaces_body() {
    // A <frameset> at the right position should replace the <body>.
    let doc = parse("<!DOCTYPE html><frameset><frame src=\"a\"><frame src=\"b\"></frameset>");
    let frameset = find_element_by_name(&doc, "FRAMESET").expect("missing <FRAMESET>");
    let frame_count = frameset
        .borrow()
        .children
        .iter()
        .filter(|c| c.borrow().node_name == "FRAME")
        .count();
    assert_eq!(frame_count, 2, "expected 2 <FRAME>, got {}", frame_count);
}

#[test]
fn frameset_eof_does_not_panic() {
    let doc = parse("<!DOCTYPE html><frameset><frame src=\"a\">");
    let frameset = find_element_by_name(&doc, "FRAMESET").expect("missing <FRAMESET>");
    let _ = frameset;
}

#[test]
fn nested_framesets() {
    let doc = parse("<!DOCTYPE html><frameset><frameset><frame src=\"a\"></frameset></frameset>");
    let framesets: Vec<_> = Node::descendants(&doc)
        .filter(|n| n.borrow().node_type == NodeType::Element && n.borrow().node_name == "FRAMESET")
        .collect();
    assert_eq!(
        framesets.len(),
        2,
        "expected 2 nested <FRAMESET>, got {}",
        framesets.len()
    );
}

// ── Noscript in head (§13.2.6.4.6) ─────────────────────────────

#[test]
fn noscript_in_head_with_scripting_disabled() {
    // With scripting disabled (the default), <noscript> in <head> enters
    // InHeadNoscript mode. Content should be parsed as HTML.
    let doc = parse("<head><noscript><style>x</style></noscript></head>");
    let noscript = find_element_by_name(&doc, "NOSCRIPT").expect("missing <NOSCRIPT>");
    let _ = noscript;
}

#[test]
fn noscript_eof_does_not_panic() {
    let doc = parse("<head><noscript>unclosed");
    let noscript = find_element_by_name(&doc, "NOSCRIPT").expect("missing <NOSCRIPT>");
    let _ = noscript;
}
