//! Tree construction mode dispatcher.
//!
//! Each insertion mode has a handler function that receives a token and
//! returns a [`Step`] indicating whether the token was consumed or needs
//! to be reprocessed in the new insertion mode.
//!
//! Phase 3.1 implements the prelude chain (§13.2.6.4.1–§13.2.6.4.6):
//! Initial → BeforeHtml → BeforeHead → InHead → AfterHead → InBody,
//! plus a minimal Text mode to absorb the contents of `<title>`/`<style>`/
//! `<script>` etc. Full InBody handling and remaining modes come in
//! Phase 3.2+.

use std::cell::RefCell;
use std::rc::Rc;

use muskitty_dom::{append_child, remove_child, Attribute, Node, NodeKind};

use crate::error::ParseError;
use crate::tokenizer::{State, TagKind, Token, Tokenizer};

use super::helpers;
use super::insertion_mode::InsertionMode;
use super::{ActiveFormattingEntry, HtmlTreeConstructor};

/// Result of a tree construction step.
pub enum Step {
    /// Token was consumed; get the next token.
    Done,
    /// Switch insertion mode and reprocess the same token.
    Reprocess,
}

/// Dispatch a token to the handler for the parser's current insertion mode.
///
/// The `tokenizer` is passed so handlers can switch the tokenizer's content
/// model (e.g. RCDATA for `<title>`, RAWTEXT for `<style>`, ScriptData for
/// `<script>`, per §13.2.6.4.4).
pub fn dispatch(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    // Tree construction dispatcher (§13.2.6): if the adjusted current node
    // is in foreign content and none of the integration-point escape hatches
    // apply, route the token to the foreign-content rules instead of the
    // current insertion mode. The dispatcher is re-evaluated on every token
    // (and on every reprocess), so once a foreign element is popped off the
    // stack the parser automatically returns to HTML content.
    if super::foreign::dispatcher_routes_to_foreign(parser, token) {
        return super::foreign::process_in_foreign_content(parser, token, tokenizer);
    }
    dispatch_in_current_mode(parser, token, tokenizer)
}

/// Process a token using the rules for the current insertion mode, **without**
/// re-evaluating the tree construction dispatcher (§13.2.6).
///
/// This is used by the foreign-content end-tag handler (§13.2.6.5 "Any other
/// end tag" step 7), which — after walking the stack and finding an HTML
/// element — must hand the token to the current insertion mode directly.
/// Returning `Step::Reprocess` instead would re-enter `dispatch`, which would
/// re-run the dispatcher and route the token back into foreign content (since
/// the foreign element is still on top of the stack), causing an infinite
/// reprocess loop.
pub(crate) fn dispatch_in_current_mode(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match parser.insertion_mode {
        InsertionMode::Initial => handle_initial(parser, token),
        InsertionMode::BeforeHtml => handle_before_html(parser, token),
        InsertionMode::BeforeHead => handle_before_head(parser, token, tokenizer),
        InsertionMode::InHead => handle_in_head(parser, token, tokenizer),
        InsertionMode::AfterHead => handle_after_head(parser, token, tokenizer),
        InsertionMode::InBody => handle_in_body(parser, token, tokenizer),
        InsertionMode::Text => handle_text(parser, token, tokenizer),
        InsertionMode::AfterBody => handle_after_body(parser, token, tokenizer),
        InsertionMode::AfterAfterBody => handle_after_after_body(parser, token, tokenizer),
        InsertionMode::InTable => handle_in_table(parser, token, tokenizer),
        InsertionMode::InTableText => handle_in_table_text(parser, token, tokenizer),
        InsertionMode::InCaption => handle_in_caption(parser, token, tokenizer),
        InsertionMode::InColumnGroup => handle_in_column_group(parser, token, tokenizer),
        InsertionMode::InTableBody => handle_in_table_body(parser, token, tokenizer),
        InsertionMode::InRow => handle_in_row(parser, token, tokenizer),
        InsertionMode::InCell => handle_in_cell(parser, token, tokenizer),
        InsertionMode::InHeadNoscript => handle_in_head_noscript(parser, token, tokenizer),
        InsertionMode::InTemplate => handle_in_template(parser, token, tokenizer),
        InsertionMode::InFrameset => handle_in_frameset(parser, token, tokenizer),
        InsertionMode::AfterFrameset => handle_after_frameset(parser, token, tokenizer),
        InsertionMode::AfterAfterFrameset => handle_after_after_frameset(parser, token, tokenizer),
    }
}

/// Check if a character is a WHATWG whitespace character (§13.2.6.4.1).
fn is_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{000C}' | '\r' | ' ')
}

/// Create an element with the given tag name, insert it at the appropriate
/// insertion location (§13.2.6.2 — handles template content and foster
/// parenting), and push it onto the open elements stack.
fn create_and_push(parser: &mut HtmlTreeConstructor, name: &str) {
    let element = Node::new_element_html(name, vec![], &parser.document);
    helpers::insert_node(parser, &element);
    parser.open_elements.push(element);
}

// ── Initial insertion mode (§13.2.6.4.1) ──────────────────────

fn handle_initial(parser: &mut HtmlTreeConstructor, token: &Token) -> Step {
    match token {
        Token::Character(c) if is_whitespace(*c) => Step::Done,
        Token::Comment(data) => {
            helpers::insert_comment_at(&parser.document, data, &parser.document);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction_at(
                &parser.document,
                target,
                data,
                &parser.document,
            );
            Step::Done
        }
        Token::Doctype(dt) => {
            // Validate DOCTYPE: name must be "html", public ID must be absent,
            // system ID must be absent or "about:legacy-compat".
            if dt.name.as_deref() != Some("html")
                || dt.public_id.is_some()
                || (dt.system_id.is_some()
                    && dt.system_id.as_deref() != Some("about:legacy-compat"))
            {
                parser.errors.push(ParseError::InvalidDoctype);
            }
            // §13.2.6.4.1: Set quirks mode based on force-quirks flag, the
            // DOCTYPE name, and the public/system identifier lists.
            if helpers::detect_quirks_mode(
                dt.force_quirks,
                dt.name.as_deref(),
                dt.public_id.as_deref(),
                dt.system_id.as_deref(),
            ) {
                parser.quirks_mode = true;
            }
            let doctype_node = Node::new_document_type(
                dt.name.as_deref().unwrap_or(""),
                dt.public_id.as_deref().unwrap_or(""),
                dt.system_id.as_deref().unwrap_or(""),
                &parser.document,
            );
            let _ = append_child(&parser.document, doctype_node);
            // §13.2.6.4.1: "Then, switch the insertion mode to 'before html'."
            parser.insertion_mode = InsertionMode::BeforeHtml;
            Step::Done
        }
        _ => {
            // §13.2.6.4.1 "Anything else": no DOCTYPE → quirks mode.
            parser.quirks_mode = true;
            parser.insertion_mode = InsertionMode::BeforeHtml;
            Step::Reprocess
        }
    }
}

// ── Before html insertion mode (§13.2.6.4.2) ──────────────────

fn handle_before_html(parser: &mut HtmlTreeConstructor, token: &Token) -> Step {
    match token {
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE in before html"));
            Step::Done
        }
        Token::Comment(data) => {
            helpers::insert_comment_at(&parser.document, data, &parser.document);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction_at(
                &parser.document,
                target,
                data,
                &parser.document,
            );
            Step::Done
        }
        Token::Character(c) if is_whitespace(*c) => Step::Done,
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "html" => {
            let element = helpers::create_element_for_token(parser, tag);
            let _ = append_child(&parser.document, element.clone());
            parser.open_elements.push(element);
            parser.insertion_mode = InsertionMode::BeforeHead;
            Step::Done
        }
        Token::Tag(tag)
            if tag.kind == TagKind::End
                && matches!(tag.name.as_str(), "head" | "body" | "html" | "br") =>
        {
            // Act as anything-else: create html, switch to BeforeHead, reprocess.
            create_and_push(parser, "html");
            parser.insertion_mode = InsertionMode::BeforeHead;
            Step::Reprocess
        }
        _ => {
            create_and_push(parser, "html");
            parser.insertion_mode = InsertionMode::BeforeHead;
            Step::Reprocess
        }
    }
}

// ── Before head insertion mode (§13.2.6.4.3) ──────────────────

fn handle_before_head(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Character(c) if is_whitespace(*c) => Step::Done,
        Token::Comment(data) => {
            helpers::insert_comment(parser, data);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction(parser, target, data);
            Step::Done
        }
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE in before head"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "html" => {
            // §13.2.6.4.3: Process the token using the rules for the
            // "in body" insertion mode.
            handle_in_body(parser, token, tokenizer)
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "head" => {
            let element = helpers::create_element_for_token(parser, tag);
            let current = parser.current_node();
            let _ = append_child(&current, element.clone());
            parser.open_elements.push(element.clone());
            parser.head_element = Some(element);
            parser.insertion_mode = InsertionMode::InHead;
            Step::Done
        }
        Token::Tag(tag)
            if tag.kind == TagKind::End
                && matches!(tag.name.as_str(), "head" | "body" | "html" | "br") =>
        {
            // Act as anything-else: create head, switch to InHead, reprocess.
            create_and_push(parser, "head");
            parser.head_element = parser.open_elements.last().cloned();
            parser.insertion_mode = InsertionMode::InHead;
            Step::Reprocess
        }
        Token::Tag(tag) if tag.kind == TagKind::End => {
            // §13.2.6.4.3: Any other end tag → Parse error. Ignore.
            parser
                .errors
                .push(ParseError::Generic("unexpected end tag in before head"));
            Step::Done
        }
        _ => {
            create_and_push(parser, "head");
            parser.head_element = parser.open_elements.last().cloned();
            parser.insertion_mode = InsertionMode::InHead;
            Step::Reprocess
        }
    }
}

// ── In head insertion mode (§13.2.6.4.4) ──────────────────────

fn handle_in_head(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Character(c) if is_whitespace(*c) => {
            helpers::insert_character(parser, *c);
            Step::Done
        }
        Token::Comment(data) => {
            helpers::insert_comment(parser, data);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction(parser, target, data);
            Step::Done
        }
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE in head"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "html" => {
            // §13.2.6.4.4: Process the token using the rules for the
            // "in body" insertion mode.
            handle_in_body(parser, token, tokenizer)
        }
        // base / basefont / bgsound / link: insert element, immediately pop.
        Token::Tag(tag)
            if tag.kind == TagKind::Start
                && matches!(tag.name.as_str(), "base" | "basefont" | "bgsound" | "link") =>
        {
            helpers::insert_element(parser, tag);
            parser.open_elements.pop();
            Step::Done
        }
        // meta: insert element, immediately pop. (Charset/pragma processing
        // deferred — skeleton just creates the node.)
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "meta" => {
            helpers::insert_element(parser, tag);
            parser.open_elements.pop();
            Step::Done
        }
        // title: switch tokenizer to RCDATA, insert element, switch to Text.
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "title" => {
            tokenizer.set_appropriate_end_tag_name(Some(&tag.name));
            tokenizer.set_state(State::RCDATA);
            helpers::insert_element(parser, tag);
            parser.original_insertion_mode = Some(parser.insertion_mode);
            parser.insertion_mode = InsertionMode::Text;
            Step::Done
        }
        // noframes / style: switch tokenizer to RAWTEXT, insert element, Text.
        Token::Tag(tag)
            if tag.kind == TagKind::Start && matches!(tag.name.as_str(), "noframes" | "style") =>
        {
            tokenizer.set_appropriate_end_tag_name(Some(&tag.name));
            tokenizer.set_state(State::RAWTEXT);
            helpers::insert_element(parser, tag);
            parser.original_insertion_mode = Some(parser.insertion_mode);
            parser.insertion_mode = InsertionMode::Text;
            Step::Done
        }
        // noscript with scripting disabled: insert element, switch to
        // InHeadNoscript. (Scripting-enabled branch uses RAWTEXT; since the
        // skeleton's scripting_flag defaults to false, only the disabled
        // branch is implemented here. Phase 3.5 will add scripting support.)
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "noscript" => {
            if !parser.scripting_flag {
                helpers::insert_element(parser, tag);
                parser.insertion_mode = InsertionMode::InHeadNoscript;
                Step::Done
            } else {
                tokenizer.set_appropriate_end_tag_name(Some(&tag.name));
                tokenizer.set_state(State::RAWTEXT);
                helpers::insert_element(parser, tag);
                parser.original_insertion_mode = Some(parser.insertion_mode);
                parser.insertion_mode = InsertionMode::Text;
                Step::Done
            }
        }
        // script: switch tokenizer to ScriptData, insert element, Text.
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "script" => {
            tokenizer.set_appropriate_end_tag_name(Some(&tag.name));
            tokenizer.set_state(State::ScriptData);
            helpers::insert_element(parser, tag);
            parser.original_insertion_mode = Some(parser.insertion_mode);
            parser.insertion_mode = InsertionMode::Text;
            Step::Done
        }
        // template (§13.2.6.4.5): add marker to active formatting,
        // insert frame element, push to template insertion mode stack,
        // switch to InTemplate.
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "template" => {
            helpers::add_formatting_marker(parser);
            helpers::insert_element(parser, tag);
            parser.frameset_ok = false;
            parser
                .template_insertion_modes
                .push(InsertionMode::InTemplate);
            parser.insertion_mode = InsertionMode::InTemplate;
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "template" => {
            if !helpers::has_element_in_stack(parser, "template") {
                parser.errors.push(ParseError::Generic(
                    "end template without template in stack",
                ));
                return Step::Done;
            }
            // Generate implied end tags.
            helpers::generate_implied_end_tags(parser, None);
            // Pop until a template element is popped. Per §13.2.6.2, a
            // "template element" is an HTML element whose local name is
            // "template" — an SVG-namespaced `<template>` (e.g. inside
            // `<svg><foo><template>`) must NOT match.
            while let Some(top) = parser.open_elements.pop() {
                let is_html_template = top
                    .borrow()
                    .kind
                    .as_element()
                    .map(|e| {
                        e.local_name == "template" && e.namespace == muskitty_dom::Namespace::Html
                    })
                    .unwrap_or(false);
                if is_html_template {
                    break;
                }
            }
            helpers::clear_active_formatting_to_last_marker(parser);
            // Pop the template insertion mode stack.
            parser.template_insertion_modes.pop();
            // Reset insertion mode.
            reset_insertion_mode(parser);
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "head" => {
            parser
                .errors
                .push(ParseError::Generic("duplicate head start tag"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "head" => {
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::AfterHead;
            Step::Done
        }
        Token::Tag(tag)
            if tag.kind == TagKind::End && matches!(tag.name.as_str(), "body" | "html" | "br") =>
        {
            // Act as anything-else: pop head, switch to AfterHead, reprocess.
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::AfterHead;
            Step::Reprocess
        }
        // Any other start tag → anything-else.
        Token::Tag(tag) if tag.kind == TagKind::Start => {
            let _ = tag;
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::AfterHead;
            Step::Reprocess
        }
        // Any other end tag → parse error, ignore.
        Token::Tag(tag) if tag.kind == TagKind::End => {
            parser
                .errors
                .push(ParseError::UnexpectedEndTag(tag.name.clone()));
            Step::Done
        }
        _ => {
            // Anything else: pop head, switch to AfterHead, reprocess.
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::AfterHead;
            Step::Reprocess
        }
    }
}

// ── After head insertion mode (§13.2.6.4.6) ───────────────────

fn handle_after_head(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Character(c) if is_whitespace(*c) => {
            helpers::insert_character(parser, *c);
            Step::Done
        }
        Token::Comment(data) => {
            helpers::insert_comment(parser, data);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction(parser, target, data);
            Step::Done
        }
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE after head"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "html" => {
            // §13.2.6.4.6: Process the token using the rules for the
            // "in body" insertion mode.
            let _ = tag;
            handle_in_body(parser, token, tokenizer)
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "body" => {
            let element = helpers::create_element_for_token(parser, tag);
            let current = parser.current_node();
            let _ = append_child(&current, element.clone());
            parser.open_elements.push(element);
            parser.frameset_ok = false;
            parser.insertion_mode = InsertionMode::InBody;
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "frameset" => {
            let element = helpers::create_element_for_token(parser, tag);
            let current = parser.current_node();
            let _ = append_child(&current, element.clone());
            parser.open_elements.push(element);
            parser.insertion_mode = InsertionMode::InFrameset;
            Step::Done
        }
        // base/basefont/bgsound/link/meta/noframes/script/style/template/title:
        // parse error. Push the head element back onto the stack, process the
        // token using the "in head" rules, then remove the head element again
        // (§13.2.6.4.6).
        Token::Tag(tag)
            if tag.kind == TagKind::Start
                && matches!(
                    tag.name.as_str(),
                    "base"
                        | "basefont"
                        | "bgsound"
                        | "link"
                        | "meta"
                        | "noframes"
                        | "script"
                        | "style"
                        | "template"
                        | "title"
                ) =>
        {
            parser
                .errors
                .push(ParseError::UnexpectedStartTag(tag.name.clone()));
            if let Some(head) = parser.head_element.clone() {
                let head_ptr = Rc::as_ptr(&head);
                parser.open_elements.push(head);
                // Process the token using InHead rules directly (don't
                // change insertion mode — InHead handler may set it itself,
                // e.g. to Text for <title>/<style>).
                let step = handle_in_head(parser, token, tokenizer);
                // Remove head from the stack (it might not be the current
                // node at this point, e.g. if <title> pushed title on top).
                parser.open_elements.retain(|n| Rc::as_ptr(n) != head_ptr);
                step
            } else {
                Step::Done
            }
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "head" => {
            parser
                .errors
                .push(ParseError::Generic("unexpected head start tag after head"));
            Step::Done
        }
        Token::Tag(tag)
            if tag.kind == TagKind::End && matches!(tag.name.as_str(), "body" | "html" | "br") =>
        {
            // Act as anything-else: insert a fake <body> WITHOUT resetting
            // frameset-ok, switch to InBody, reprocess (§13.2.6.4.6).
            create_and_push(parser, "body");
            parser.insertion_mode = InsertionMode::InBody;
            Step::Reprocess
        }
        // template end tag (§13.2.6.4.6): process using in head rules.
        // The InHead </template> handler scans the whole stack, pops until
        // template, clears formatting to last marker, pops the template
        // insertion mode stack, and resets insertion mode — so we just
        // delegate and return its result.
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "template" => {
            let _ = tag;
            handle_in_head(parser, token, tokenizer)
        }
        // Any other end tag (§13.2.6.4.6): Parse error. Ignore the token.
        Token::Tag(tag) if tag.kind == TagKind::End => {
            parser
                .errors
                .push(ParseError::Generic("unexpected end tag after head"));
            let _ = tag;
            Step::Done
        }
        _ => {
            // Anything else (§13.2.6.4.6): insert a fake <body> WITHOUT
            // resetting frameset-ok, switch to InBody, reprocess.
            create_and_push(parser, "body");
            parser.insertion_mode = InsertionMode::InBody;
            Step::Reprocess
        }
    }
}

// ── Text insertion mode (§13.2.6.5) — minimal ────────────────
//
// Entered after a `<title>`/`<style>`/`<script>`/etc. start tag. Absorbs
// the element's character content until the matching end tag, then pops the
// element and restores the original insertion mode.

fn handle_text(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    _tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Character(c) => {
            helpers::insert_character(parser, *c);
            Step::Done
        }
        Token::EOF => {
            parser
                .errors
                .push(ParseError::Generic("unexpected EOF in text mode"));
            // Pop the open element and reprocess EOF in the original mode.
            parser.open_elements.pop();
            if let Some(orig) = parser.original_insertion_mode.take() {
                parser.insertion_mode = orig;
            }
            Step::Reprocess
        }
        Token::Tag(tag) if tag.kind == TagKind::End => {
            let _ = tag;
            // Pop the current element (the title/style/script/etc.).
            parser.open_elements.pop();
            // Restore the original insertion mode.
            if let Some(orig) = parser.original_insertion_mode.take() {
                parser.insertion_mode = orig;
            }
            // Reset tokenizer to Data state and clear the appropriate end tag
            // name so subsequent `</...>` sequences are parsed as normal tags.
            _tokenizer.set_state(State::Data);
            _tokenizer.set_appropriate_end_tag_name(None);
            Step::Done
        }
        // Any other token (start tags, comments, doctype) is a parse error
        // in Text mode; skeleton ignores them for now.
        _ => {
            parser
                .errors
                .push(ParseError::Generic("unexpected token in text mode"));
            Step::Done
        }
    }
}

// ── In body insertion mode (§13.2.6.4.7) ──────────────────────
//
// Phase 3.2 implements block-level elements, headings, lists, forms,
// void elements, and basic end-tag handling. Formatting elements
// (`<b>`/`<i>`/`<a>`/etc.) and the adoption agency algorithm are
// deferred to Phase 3.3.

/// Tag names that the spec groups under "address/article/aside/...":
/// close a `<p>` if open, then insert a fresh HTML element.
const BLOCK_LEVEL_START_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "search",
    "section",
    "summary",
    "ul",
];

/// Same as BLOCK_LEVEL_START_TAGS, used by the "any other end tag" branch.
const BLOCK_LEVEL_END_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "button",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "listing",
    "main",
    "menu",
    "nav",
    "ol",
    "pre",
    "search",
    "section",
    "select",
    "summary",
    "ul",
];

/// HTML void elements (§13.2.6.2) — inserted and immediately popped.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "img", "keygen", "link", "meta", "param", "source",
    "track", "wbr",
];

fn handle_in_body(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::EOF => {
            // §13.2.6.4.7: If the stack of template insertion modes is not
            // empty, process the token using the rules for the "in template"
            // insertion mode. Otherwise, check for unexpected elements (parse
            // error), then stop parsing.
            if !parser.template_insertion_modes.is_empty() {
                return handle_in_template(parser, token, tokenizer);
            }
            // Parse error if any open element is not in the allowed set.
            let unexpected = parser.open_elements.iter().any(|n| {
                let borrowed = n.borrow();
                let local = borrowed
                    .kind
                    .as_element()
                    .map(|e| e.local_name.as_str())
                    .unwrap_or("");
                !matches!(
                    local,
                    "dd" | "dt"
                        | "li"
                        | "optgroup"
                        | "option"
                        | "p"
                        | "rb"
                        | "rp"
                        | "rt"
                        | "rtc"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                        | "body"
                        | "html"
                )
            });
            if unexpected {
                parser
                    .errors
                    .push(ParseError::Generic("unexpected open element at EOF"));
            }
            Step::Done
        }
        Token::Character(c) if is_whitespace(*c) => {
            helpers::reconstruct_active_formatting_elements(parser);
            helpers::insert_character(parser, *c);
            Step::Done
        }
        Token::Character('\0') => {
            // §13.2.6.4.7: A character token that is U+0000 NULL.
            // Parse error. Ignore the token.
            parser
                .errors
                .push(ParseError::Generic("unexpected null character in body"));
            Step::Done
        }
        Token::Character(c) => {
            helpers::reconstruct_active_formatting_elements(parser);
            parser.frameset_ok = false;
            helpers::insert_character(parser, *c);
            Step::Done
        }
        Token::Comment(data) => {
            helpers::insert_comment(parser, data);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction(parser, target, data);
            Step::Done
        }
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE in body"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start => {
            handle_in_body_start_tag(parser, tag, tokenizer)
        }
        Token::Tag(tag) if tag.kind == TagKind::End => {
            // §13.2.6.4.7: </br> → parse error, act as <br> start tag.
            if tag.name == "br" {
                parser
                    .errors
                    .push(ParseError::Generic("end tag br treated as start tag"));
                let br_tag = crate::tokenizer::TagToken {
                    kind: TagKind::Start,
                    name: "br".to_string(),
                    attrs: vec![],
                    self_closing: false,
                };
                return handle_in_body_start_tag(parser, &br_tag, tokenizer);
            }
            handle_in_body_end_tag(parser, tag)
        }
        _ => Step::Done,
    }
}

fn handle_in_body_start_tag(
    parser: &mut HtmlTreeConstructor,
    tag: &crate::tokenizer::TagToken,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    let name = tag.name.as_str();

    // §13.2.6.4.7: A start tag whose tag name is "image": parse error.
    // Change the token's tag name to "img" and reprocess it. (Don't ask.)
    if name == "image" {
        parser
            .errors
            .push(ParseError::Generic("image start tag treated as img"));
        let img_tag = crate::tokenizer::TagToken {
            kind: tag.kind,
            name: "img".to_string(),
            attrs: tag.attrs.clone(),
            self_closing: tag.self_closing,
        };
        return handle_in_body_start_tag(parser, &img_tag, tokenizer);
    }

    // "html" — merge attributes onto the existing <html> element.
    // §13.2.6.4.7: If there is a template element on the stack of open
    // elements, then ignore the token. Otherwise, merge attributes.
    if name == "html" {
        parser
            .errors
            .push(ParseError::Generic("unexpected <html> start tag in body"));
        if !template_in_stack(parser) {
            if let Some(html) = parser.open_elements.first().cloned() {
                merge_attributes(&html, tag);
            }
        }
        return Step::Done;
    }

    // Head-element start tags: process using the rules for "in head"
    // (§13.2.6.4.7). For void-like elements (base/link/meta/etc.) we
    // insert and pop. For title/style/script/noframes we must switch the
    // tokenizer to the appropriate content model and enter Text mode so
    // their contents are consumed as raw text rather than parsed markup.
    if matches!(
        name,
        "base"
            | "basefont"
            | "bgsound"
            | "link"
            | "meta"
            | "noframes"
            | "script"
            | "style"
            | "title"
    ) {
        if matches!(name, "base" | "basefont" | "bgsound" | "link" | "meta") {
            helpers::insert_element(parser, tag);
            parser.open_elements.pop();
        } else if name == "title" {
            // title: RCDATA (§13.2.6.4.4).
            tokenizer.set_appropriate_end_tag_name(Some(name));
            tokenizer.set_state(State::RCDATA);
            helpers::insert_element(parser, tag);
            parser.original_insertion_mode = Some(parser.insertion_mode);
            parser.insertion_mode = InsertionMode::Text;
        } else if matches!(name, "noframes" | "style") {
            // noframes/style: RAWTEXT (§13.2.6.4.4).
            tokenizer.set_appropriate_end_tag_name(Some(name));
            tokenizer.set_state(State::RAWTEXT);
            helpers::insert_element(parser, tag);
            parser.original_insertion_mode = Some(parser.insertion_mode);
            parser.insertion_mode = InsertionMode::Text;
        } else {
            // script: ScriptData (§13.2.6.4.4).
            tokenizer.set_appropriate_end_tag_name(Some(name));
            tokenizer.set_state(State::ScriptData);
            helpers::insert_element(parser, tag);
            parser.original_insertion_mode = Some(parser.insertion_mode);
            parser.insertion_mode = InsertionMode::Text;
        }
        return Step::Done;
    }

    // textarea (§13.2.6.4.7): switch tokenizer to RCDATA, insert element,
    // skip a leading LF, drop frameset_ok, switch to Text mode.
    if name == "textarea" {
        tokenizer.set_appropriate_end_tag_name(Some(name));
        tokenizer.set_state(State::RCDATA);
        helpers::insert_element(parser, tag);
        parser.skip_next_lf = true;
        parser.frameset_ok = false;
        parser.original_insertion_mode = Some(parser.insertion_mode);
        parser.insertion_mode = InsertionMode::Text;
        return Step::Done;
    }

    // template (§13.2.6.4.7): process using in-head rules. The template
    // start-tag path doesn't need a tokenizer switch, so this is inlined.
    if name == "template" {
        helpers::add_formatting_marker(parser);
        helpers::insert_element(parser, tag);
        parser.frameset_ok = false;
        parser
            .template_insertion_modes
            .push(InsertionMode::InTemplate);
        parser.insertion_mode = InsertionMode::InTemplate;
        return Step::Done;
    }

    // "body" — merge attributes onto the existing <body> element.
    if name == "body" {
        // §13.2.6.4.7: Parse error. If the stack has only one node, or the
        // second element is not a body element, or if there is a template
        // element on the stack, then ignore the token. Otherwise, merge
        // attributes onto the body element (second element on the stack).
        parser
            .errors
            .push(ParseError::Generic("unexpected <body> start tag in body"));
        let should_ignore = parser.open_elements.len() < 2
            || !parser
                .open_elements
                .get(1)
                .and_then(|n| n.borrow().kind.as_element().map(|e| e.local_name == "body"))
                .unwrap_or(false)
            || template_in_stack(parser);
        if !should_ignore {
            if let Some(body) = parser.open_elements.get(1).cloned() {
                merge_attributes(&body, tag);
            }
            parser.frameset_ok = false;
        }
        return Step::Done;
    }

    // "frameset" (§13.2.6.4.7): If the stack has only one node or the second
    // element is not body, ignore. If frameset_ok is false, ignore. Otherwise
    // remove body from its parent, pop all nodes except html, insert frameset,
    // and switch to InFrameset.
    if name == "frameset" {
        if parser.open_elements.len() < 2 {
            parser
                .errors
                .push(ParseError::Generic("frameset with no body on stack"));
            return Step::Done;
        }
        let second_is_body = parser
            .open_elements
            .get(1)
            .map(|n| {
                n.borrow()
                    .kind
                    .as_element()
                    .map(|e| e.local_name == "body")
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !second_is_body {
            parser
                .errors
                .push(ParseError::Generic("frameset with non-body second element"));
            return Step::Done;
        }
        if !parser.frameset_ok {
            parser
                .errors
                .push(ParseError::Generic("frameset after non-whitespace content"));
            return Step::Done;
        }
        // Remove the second element (body) from its parent (html).
        if let (Some(html), Some(body)) = (
            parser.open_elements.first().cloned(),
            parser.open_elements.get(1).cloned(),
        ) {
            let _ = remove_child(&html, &body);
        }
        // Pop all nodes from the stack except the root html element.
        while parser.open_elements.len() > 1 {
            parser.open_elements.pop();
        }
        // Insert the frameset element and switch to InFrameset.
        helpers::insert_element(parser, tag);
        parser.insertion_mode = InsertionMode::InFrameset;
        return Step::Done;
    }

    // §13.2.6.4.7: "caption", "col", "colgroup", "frame", "head", "tbody",
    // "td", "tfoot", "th", "thead", "tr" → Parse error. Ignore the token.
    if matches!(
        name,
        "caption"
            | "col"
            | "colgroup"
            | "frame"
            | "head"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
    ) {
        parser
            .errors
            .push(ParseError::UnexpectedStartTag(tag.name.clone()));
        return Step::Done;
    }

    // Block-level: close <p> if in button scope, insert.
    if BLOCK_LEVEL_START_TAGS.contains(&name) {
        if helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        helpers::insert_element(parser, tag);
        return Step::Done;
    }

    // Headings h1-h6.
    if matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        if helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        // If current node is a heading, parse error: close it.
        if let Some(top) = parser.open_elements.last() {
            let top_name = top.borrow().kind.as_element().map(|e| e.local_name.clone());
            if matches!(
                top_name.as_deref(),
                Some("h1" | "h2" | "h3" | "h4" | "h5" | "h6")
            ) {
                parser
                    .errors
                    .push(ParseError::Generic("heading nested in heading"));
                parser.open_elements.pop();
            }
        }
        helpers::insert_element(parser, tag);
        return Step::Done;
    }

    // pre / listing (§13.2.6.4.7): close p, insert, skip a leading LF,
    // frameset_ok=false.
    if matches!(name, "pre" | "listing") {
        if helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        helpers::insert_element(parser, tag);
        // §13.2.6.4.7: "If the next token is a U+000A LINE FEED (LF)
        // character token, then ignore that token and move on to the next
        // one." The run() loop checks skip_next_lf before dispatching.
        parser.skip_next_lf = true;
        parser.frameset_ok = false;
        return Step::Done;
    }

    // form (§13.2.6.4.7 "A start tag whose tag name is 'form'"):
    // 1. If the form element pointer is not null, and there is no template
    //    element on the stack of open elements, then this is a parse error;
    //    ignore the token.
    // 2. Otherwise: close p if in button scope, insert form, set pointer.
    if name == "form" {
        if parser.form_element.is_some() && !template_in_stack(parser) {
            parser
                .errors
                .push(ParseError::Generic("nested form element"));
            return Step::Done;
        }
        if helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        let element = helpers::create_element_for_token(parser, tag);
        helpers::insert_node(parser, &element);
        parser.open_elements.push(element.clone());
        parser.form_element = Some(element);
        return Step::Done;
    }

    // li: §13.2.6.4.7 loop algorithm — walk stack, close li if found,
    // stop at special elements (except address/div/p).
    if name == "li" {
        parser.frameset_ok = false;
        helpers::close_list_item(parser, &["li"]);
        // §13.2.6.4.7 step 6: close p if in button scope.
        if helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        helpers::insert_element(parser, tag);
        return Step::Done;
    }

    // dd / dt: §13.2.6.4.7 loop algorithm — checks for BOTH dd and dt.
    if matches!(name, "dd" | "dt") {
        parser.frameset_ok = false;
        helpers::close_list_item(parser, &["dd", "dt"]);
        // §13.2.6.4.7 step 7: close p if in button scope.
        if helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        helpers::insert_element(parser, tag);
        return Step::Done;
    }

    // plaintext (§13.2.6.4.7): close p if in button scope, insert element,
    // switch tokenizer to PLAINTEXT. Unlike RCDATA/RAWTEXT there is no end
    // tag to exit this state — it persists until EOF.
    if name == "plaintext" {
        if helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        helpers::insert_element(parser, tag);
        tokenizer.set_state(State::PLAINTEXT);
        return Step::Done;
    }

    // iframe (§13.2.6.4.7): frameset_ok=false, RAWTEXT, insert element.
    if name == "iframe" {
        parser.frameset_ok = false;
        tokenizer.set_appropriate_end_tag_name(Some(name));
        tokenizer.set_state(State::RAWTEXT);
        helpers::insert_element(parser, tag);
        parser.original_insertion_mode = Some(parser.insertion_mode);
        parser.insertion_mode = InsertionMode::Text;
        return Step::Done;
    }

    // xmp (§13.2.6.4.7): close p, reconstruct, frameset_ok=false, RAWTEXT,
    // insert element.
    if name == "xmp" {
        if helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        helpers::reconstruct_active_formatting_elements(parser);
        parser.frameset_ok = false;
        tokenizer.set_appropriate_end_tag_name(Some(name));
        tokenizer.set_state(State::RAWTEXT);
        helpers::insert_element(parser, tag);
        parser.original_insertion_mode = Some(parser.insertion_mode);
        parser.insertion_mode = InsertionMode::Text;
        return Step::Done;
    }

    // noembed (§13.2.6.4.7): always follow the generic raw text element
    // parsing algorithm (switch to RAWTEXT, insert, enter Text mode). Per
    // §13.2.6.4.7, the tokenizer's content model must be set to RAWTEXT
    // AND the appropriate end tag name must be set to "noembed", so that
    // `</noembed>` is recognized as the section terminator (§13.2.5.12-14).
    if name == "noembed" {
        tokenizer.set_appropriate_end_tag_name(Some(name));
        tokenizer.set_state(State::RAWTEXT);
        helpers::insert_element(parser, tag);
        parser.original_insertion_mode = Some(parser.insertion_mode);
        parser.insertion_mode = InsertionMode::Text;
        return Step::Done;
    }

    // button: if button in scope, parse error, pop until button, reprocess.
    if name == "button" {
        if helpers::has_element_in_scope(parser, "button") {
            parser.errors.push(ParseError::Generic("nested button"));
            helpers::generate_implied_end_tags(parser, None);
            while let Some(top) = parser.open_elements.last() {
                let is_button = top
                    .borrow()
                    .kind
                    .as_element()
                    .map(|e| e.local_name.as_str())
                    == Some("button");
                parser.open_elements.pop();
                if is_button {
                    break;
                }
            }
            // Reprocess in InBody.
            return Step::Reprocess;
        }
        helpers::reconstruct_active_formatting_elements(parser);
        helpers::insert_element(parser, tag);
        parser.frameset_ok = false;
        return Step::Done;
    }

    // hr (§13.2.6.4.7): close p, if select in scope generate implied end
    // tags (parse error if option/optgroup in scope), insert, pop,
    // frameset_ok=false.
    if name == "hr" {
        if helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        if helpers::has_element_in_scope(parser, "select") {
            helpers::generate_implied_end_tags(parser, None);
            if helpers::has_element_in_scope(parser, "option")
                || helpers::has_element_in_scope(parser, "optgroup")
            {
                parser
                    .errors
                    .push(ParseError::Generic("hr when option/optgroup in scope"));
            }
        }
        helpers::insert_element(parser, tag);
        parser.open_elements.pop();
        parser.frameset_ok = false;
        return Step::Done;
    }

    // rb / rtc (§13.2.6.4.7): if ruby in scope, generate implied end tags.
    // Then insert element.
    if matches!(name, "rb" | "rtc") {
        if helpers::has_element_in_scope(parser, "ruby") {
            helpers::generate_implied_end_tags(parser, None);
            // If current node is not ruby, parse error.
            let is_ruby = parser
                .open_elements
                .last()
                .and_then(|n| n.borrow().kind.as_element().map(|e| e.local_name == "ruby"))
                .unwrap_or(false);
            if !is_ruby {
                parser
                    .errors
                    .push(ParseError::Generic("rb/rtc not directly in ruby"));
            }
        }
        helpers::insert_element(parser, tag);
        return Step::Done;
    }

    // rp / rt (§13.2.6.4.7): if ruby in scope, generate implied end tags
    // except rtc. Then insert element.
    if matches!(name, "rp" | "rt") {
        if helpers::has_element_in_scope(parser, "ruby") {
            helpers::generate_implied_end_tags(parser, Some("rtc"));
            // If current node is not rtc or ruby, parse error.
            let is_rtc_or_ruby = parser
                .open_elements
                .last()
                .and_then(|n| {
                    n.borrow()
                        .kind
                        .as_element()
                        .map(|e| matches!(e.local_name.as_str(), "rtc" | "ruby"))
                })
                .unwrap_or(false);
            if !is_rtc_or_ruby {
                parser
                    .errors
                    .push(ParseError::Generic("rp/rt not directly in rtc/ruby"));
            }
        }
        helpers::insert_element(parser, tag);
        return Step::Done;
    }

    // input (§13.2.6.4.7): Separate from void elements because it has a
    // "select in scope" check. If select in scope, parse error, pop until
    // select is popped. Then reconstruct, insert, pop, check type=hidden
    // for frameset_ok.
    if name == "input" {
        if helpers::has_element_in_scope(parser, "select") {
            parser.errors.push(ParseError::Generic(
                "unexpected <input> when select in scope",
            ));
            while let Some(top) = parser.open_elements.pop() {
                let is_select = top
                    .borrow()
                    .kind
                    .as_element()
                    .map(|e| e.local_name == "select")
                    .unwrap_or(false);
                if is_select {
                    break;
                }
            }
        }
        helpers::reconstruct_active_formatting_elements(parser);
        helpers::insert_element(parser, tag);
        parser.open_elements.pop();
        let is_hidden = tag
            .attrs
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("type") && v.eq_ignore_ascii_case("hidden"));
        if !is_hidden {
            parser.frameset_ok = false;
        }
        return Step::Done;
    }

    // Void elements (area/base/br/col/embed/img/keygen/link/meta/
    // param/source/track/wbr): reconstruct, insert, pop. Per §13.2.6.4.7
    // (line 4063-4072) ALL of area/br/embed/img/keygen/wbr set
    // frameset_ok=false.
    if VOID_ELEMENTS.contains(&name) {
        helpers::reconstruct_active_formatting_elements(parser);
        helpers::insert_element(parser, tag);
        parser.open_elements.pop();
        if matches!(name, "area" | "br" | "embed" | "img" | "keygen" | "wbr") {
            parser.frameset_ok = false;
        }
        return Step::Done;
    }

    // Formatting elements (a/b/big/code/em/font/i/nobr/s/small/strike/
    // strong/tt/u) — full active formatting bookkeeping + adoption agency.
    if matches!(
        name,
        "a" | "b"
            | "big"
            | "code"
            | "em"
            | "font"
            | "i"
            | "nobr"
            | "s"
            | "small"
            | "strike"
            | "strong"
            | "tt"
            | "u"
    ) {
        // Special case for <a> (§13.2.6.4.7): see below.
        if name == "a" {
            // §13.2.6.4.7: If the list of active formatting elements
            // contains an <a> element between the end of the list and the
            // last marker on the list (or the start of the list if there
            // is no marker), run the adoption agency algorithm for the
            // token, then remove that element from the list of active
            // formatting elements and the stack of open elements if the
            // adoption agency algorithm didn't already remove it (it
            // might not have if the element is not in table scope).
            let a_element = parser
                .active_formatting_elements
                .iter()
                .rev()
                .take_while(|e| !matches!(e, ActiveFormattingEntry::Marker))
                .find_map(|e| match e {
                    ActiveFormattingEntry::Element(el) => {
                        let is_a = el
                            .borrow()
                            .kind
                            .as_element()
                            .map(|e| e.local_name.as_str() == "a")
                            .unwrap_or(false);
                        if is_a {
                            Some(el.clone())
                        } else {
                            None
                        }
                    }
                    _ => None,
                });
            if let Some(a_el) = a_element {
                helpers::adoption_agency(parser, "a");
                // Remove that specific <a> element (by pointer identity)
                // from the AFE and the stack of open elements if the
                // adoption agency algorithm didn't already remove it.
                parser.active_formatting_elements.retain(
                    |e| !matches!(e, ActiveFormattingEntry::Element(el) if Rc::ptr_eq(el, &a_el)),
                );
                parser.open_elements.retain(|n| !Rc::ptr_eq(n, &a_el));
            }
        }
        // Special case for <nobr> (§13.2.6.4.7): if there is a <nobr> in
        // scope, parse error; run the adoption agency algorithm for the
        // token. The shared reconstruct below serves as the "once again
        // reconstruct the active formatting elements" step.
        if name == "nobr" && helpers::has_element_in_scope(parser, "nobr") {
            parser
                .errors
                .push(ParseError::Generic("nested nobr element"));
            helpers::adoption_agency(parser, "nobr");
        }
        helpers::reconstruct_active_formatting_elements(parser);
        let element = helpers::create_element_for_token(parser, tag);
        helpers::insert_node(parser, &element);
        parser.open_elements.push(element.clone());
        helpers::push_formatting_element(parser, element);
        return Step::Done;
    }

    // applet/marquee/object (§13.2.6.4.7 line 4020-4029): reconstruct,
    // insert HTML element, insert AFE marker, set frameset_ok=false.
    if matches!(name, "applet" | "marquee" | "object") {
        helpers::reconstruct_active_formatting_elements(parser);
        helpers::insert_element(parser, tag);
        helpers::add_formatting_marker(parser);
        parser.frameset_ok = false;
        return Step::Done;
    }

    // table (§13.2.6.4.7): in no-quirks mode, close <p> if in button
    // scope; insert <table>, switch to InTable, set frameset_ok=false.
    if name == "table" {
        if !parser.quirks_mode && helpers::has_element_in_button_scope(parser, "p") {
            helpers::close_p_element(parser);
        }
        helpers::insert_element(parser, tag);
        parser.insertion_mode = InsertionMode::InTable;
        parser.frameset_ok = false;
        return Step::Done;
    }

    // select (§13.2.6.4.7): InSelect mode has been removed from the spec.
    // If a select is already in scope, parse error, ignore the token, pop
    // until the previous select is popped. Otherwise: reconstruct, insert,
    // frameset_ok=false. No insertion mode switch.
    if name == "select" {
        if helpers::has_element_in_scope(parser, "select") {
            parser.errors.push(ParseError::Generic(
                "unexpected <select> when select in scope",
            ));
            // Pop until select is popped.
            loop {
                let popped = helpers::pop_open_element(parser);
                match popped {
                    Some(top) => {
                        let is_select = top
                            .borrow()
                            .kind
                            .as_element()
                            .map(|e| e.local_name == "select")
                            .unwrap_or(false);
                        if is_select {
                            break;
                        }
                    }
                    None => break,
                }
            }
            return Step::Done;
        }
        helpers::reconstruct_active_formatting_elements(parser);
        helpers::insert_element(parser, tag);
        // §13.2.6.4.7 "A start tag whose tag name is 'select'": insert a
        // marker at the end of the list of active formatting elements.
        // This prevents the adoption agency algorithm from cloning
        // formatting elements opened before <select> (e.g. <font>) into
        // the select subtree when their end tag is encountered inside
        // <select>. Without this marker, </font> inside <select> would
        // trigger AAA, clone <font>, and reparent <select> into the clone.
        helpers::add_formatting_marker(parser);
        parser.frameset_ok = false;
        return Step::Done;
    }

    // option (§13.2.6.4.7): If select in scope, generate implied end tags
    // except optgroup; if option in scope, parse error. Otherwise, if current
    // node is option, pop it. Then reconstruct and insert.
    if name == "option" {
        if helpers::has_element_in_scope(parser, "select") {
            helpers::generate_implied_end_tags(parser, Some("optgroup"));
            // If option in scope, parse error.
            if helpers::has_element_in_scope(parser, "option") {
                parser
                    .errors
                    .push(ParseError::Generic("option when option in scope"));
            }
        } else {
            // If current node is an option, pop it.
            if let Some(top) = parser.open_elements.last() {
                let is_option = top
                    .borrow()
                    .kind
                    .as_element()
                    .map(|e| e.local_name == "option")
                    .unwrap_or(false);
                if is_option {
                    helpers::pop_open_element(parser);
                }
            }
        }
        helpers::reconstruct_active_formatting_elements(parser);
        helpers::insert_element(parser, tag);
        return Step::Done;
    }

    // optgroup (§13.2.6.4.7): If select in scope, generate implied end tags;
    // if option or optgroup in scope, parse error. Otherwise, if current node
    // is option, pop it. Then reconstruct and insert.
    if name == "optgroup" {
        if helpers::has_element_in_scope(parser, "select") {
            helpers::generate_implied_end_tags(parser, None);
            if helpers::has_element_in_scope(parser, "option")
                || helpers::has_element_in_scope(parser, "optgroup")
            {
                parser.errors.push(ParseError::Generic(
                    "optgroup when option/optgroup in scope",
                ));
            }
        } else {
            // If current node is an option, pop it.
            if let Some(top) = parser.open_elements.last() {
                let is_option = top
                    .borrow()
                    .kind
                    .as_element()
                    .map(|e| e.local_name == "option")
                    .unwrap_or(false);
                if is_option {
                    helpers::pop_open_element(parser);
                }
            }
        }
        helpers::reconstruct_active_formatting_elements(parser);
        helpers::insert_element(parser, tag);
        return Step::Done;
    }

    // frameset (§13.2.6.4.7): if frameset_ok is false OR current node is
    // not html/body, parse error and ignore. Otherwise, replace the body
    // with a frameset and switch to InFrameset.
    if name == "frameset" {
        if !parser.frameset_ok {
            parser
                .errors
                .push(ParseError::Generic("frameset after non-frameset-ok token"));
            return Step::Done;
        }
        // Check that current node is html or body (simplified: if the
        // second element on the stack is body).
        if parser.open_elements.len() < 2 {
            parser
                .errors
                .push(ParseError::Generic("frameset without body"));
            return Step::Done;
        }
        let second_is_body = parser
            .open_elements
            .get(1)
            .and_then(|n| n.borrow().kind.as_element().map(|e| e.local_name == "body"))
            .unwrap_or(false);
        if !second_is_body {
            parser
                .errors
                .push(ParseError::Generic("frameset with non-body current node"));
            return Step::Done;
        }
        // Remove the body element from the stack and from its parent.
        let body = parser.open_elements.remove(1);
        if let Some(parent) = parser.open_elements.first() {
            // Detach body from its parent (html).
            let body_ptr = Rc::as_ptr(&body);
            parent
                .borrow_mut()
                .children
                .retain(|c| Rc::as_ptr(c) != body_ptr);
        }
        // Insert the frameset element and switch to InFrameset.
        helpers::insert_element(parser, tag);
        parser.insertion_mode = InsertionMode::InFrameset;
        return Step::Done;
    }

    // <math> (§13.2.6.4.7): reconstruct active formatting, adjust MathML
    // attributes, adjust foreign attributes, insert a foreign element with
    // the MathML namespace. If self-closing, pop the current node and
    // acknowledge the flag.
    if name == "math" {
        helpers::reconstruct_active_formatting_elements(parser);
        let mut adjusted = tag.clone();
        super::foreign::adjust_mathml_attributes(&mut adjusted);
        super::foreign::adjust_foreign_attributes(&mut adjusted);
        super::foreign::insert_foreign_element(
            parser,
            &adjusted,
            muskitty_dom::Namespace::MathMl,
            false,
        );
        if tag.self_closing {
            parser.open_elements.pop();
        }
        return Step::Done;
    }

    // <svg> (§13.2.6.4.7): reconstruct active formatting, adjust SVG
    // attributes, adjust foreign attributes, insert a foreign element with
    // the SVG namespace. If self-closing, pop the current node and
    // acknowledge the flag.
    if name == "svg" {
        helpers::reconstruct_active_formatting_elements(parser);
        let mut adjusted = tag.clone();
        super::foreign::adjust_svg_attributes(&mut adjusted);
        super::foreign::adjust_foreign_attributes(&mut adjusted);
        super::foreign::insert_foreign_element(
            parser,
            &adjusted,
            muskitty_dom::Namespace::Svg,
            false,
        );
        if tag.self_closing {
            parser.open_elements.pop();
        }
        return Step::Done;
    }

    // Anything else (start tag): reconstruct active formatting, insert.
    helpers::reconstruct_active_formatting_elements(parser);
    helpers::insert_element(parser, tag);
    Step::Done
}

fn handle_in_body_end_tag(
    parser: &mut HtmlTreeConstructor,
    tag: &crate::tokenizer::TagToken,
) -> Step {
    let name = tag.name.as_str();

    // </template> (§13.2.6.4.7): process using in-head rules. Inlined here
    // because handle_in_body_end_tag doesn't receive a tokenizer, and the
    // template end-tag path doesn't need one.
    if name == "template" {
        if !helpers::has_element_in_stack(parser, "template") {
            parser.errors.push(ParseError::Generic(
                "end template without template in stack",
            ));
            return Step::Done;
        }
        helpers::generate_implied_end_tags(parser, None);
        // Pop until an HTML template element is popped. Per §13.2.6.2, a
        // "template element" is an HTML element whose local name is
        // "template" — an SVG-namespaced `<template>` must NOT match.
        while let Some(top) = parser.open_elements.pop() {
            let is_html_template = top
                .borrow()
                .kind
                .as_element()
                .map(|e| e.local_name == "template" && e.namespace == muskitty_dom::Namespace::Html)
                .unwrap_or(false);
            if is_html_template {
                break;
            }
        }
        helpers::clear_active_formatting_to_last_marker(parser);
        parser.template_insertion_modes.pop();
        reset_insertion_mode(parser);
        return Step::Done;
    }

    // </p>: if no p in button scope, parse error, insert <p>, reprocess.
    // Else: generate implied end tags except p, pop until p.
    if name == "p" {
        if !helpers::has_element_in_button_scope(parser, "p") {
            parser
                .errors
                .push(ParseError::Generic("end tag p without open p"));
            let p = muskitty_dom::Node::new_element_html("p", vec![], &parser.document);
            helpers::insert_node(parser, &p);
            parser.open_elements.push(p);
        }
        helpers::close_p_element(parser);
        return Step::Done;
    }

    // </applet>/</marquee>/</object> (§13.2.6.4.7 line 4031-4044): if no
    // matching element in scope, parse error, ignore. Otherwise: generate
    // implied end tags, if current node is not target parse error, pop
    // until target, clear AFE to last marker.
    if matches!(name, "applet" | "marquee" | "object") {
        if !helpers::has_element_in_scope(parser, name) {
            parser
                .errors
                .push(ParseError::UnexpectedEndTag(name.to_string()));
            return Step::Done;
        }
        helpers::generate_implied_end_tags(parser, None);
        // §13.2.6.4.7: "If the current node is not an HTML element with the
        // same tag name as that of the token, then this is a parse error."
        // The HTML-namespace qualifier matters when foreign elements with
        // matching local names (e.g. `<svg object>`) sit above the target.
        let current_is_target = parser
            .open_elements
            .last()
            .map(|n| {
                n.borrow()
                    .kind
                    .as_element()
                    .map(|e| e.namespace == muskitty_dom::Namespace::Html && e.local_name == name)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if !current_is_target {
            parser
                .errors
                .push(ParseError::Generic("end tag not at current node"));
        }
        // Pop until an HTML element with the same tag name is popped.
        while let Some(top) = parser.open_elements.pop() {
            let is_target = top
                .borrow()
                .kind
                .as_element()
                .map(|e| e.namespace == muskitty_dom::Namespace::Html && e.local_name == name)
                .unwrap_or(false);
            if is_target {
                break;
            }
        }
        helpers::clear_active_formatting_to_last_marker(parser);
        return Step::Done;
    }

    // </body>: if body not in scope, parse error, ignore. Else: switch to
    // AfterBody.
    if name == "body" {
        if !helpers::has_element_in_scope(parser, "body") {
            parser
                .errors
                .push(ParseError::Generic("end tag body without body in scope"));
            return Step::Done;
        }
        parser.insertion_mode = InsertionMode::AfterBody;
        return Step::Done;
    }

    // </html>: if body not in scope, parse error, ignore. Else: switch to
    // AfterBody, reprocess.
    if name == "html" {
        if !helpers::has_element_in_scope(parser, "body") {
            parser
                .errors
                .push(ParseError::Generic("end tag html without body in scope"));
            return Step::Done;
        }
        parser.insertion_mode = InsertionMode::AfterBody;
        return Step::Reprocess;
    }

    // Block-level end tags (address/article/aside/blockquote/...): if not in
    // scope, parse error, ignore; else: generate implied end tags, if current
    // is not target, parse error, pop until target.
    if BLOCK_LEVEL_END_TAGS.contains(&name) {
        if !helpers::has_element_in_scope(parser, name) {
            parser
                .errors
                .push(ParseError::UnexpectedEndTag(name.to_string()));
            return Step::Done;
        }
        helpers::generate_implied_end_tags(parser, Some(name));
        // Pop until target.
        while let Some(top) = parser.open_elements.last() {
            let top_name = top.borrow().kind.as_element().map(|e| e.local_name.clone());
            let is_target = top_name.as_deref() == Some(name);
            helpers::pop_open_element(parser);
            if is_target {
                break;
            }
        }
        return Step::Done;
    }

    // </form>: if form_element is None, parse error, ignore. Else: set
    // form_element to None; if form not in scope, parse error; else:
    // generate implied end tags, pop until form.
    if name == "form" {
        // §13.2.6.4.7: If no template on stack, use form element pointer.
        let has_template = helpers::has_element_in_stack(parser, "template");
        if !has_template {
            let node = parser.form_element.clone();
            parser.form_element = None;
            let node = match node {
                Some(n) => n,
                None => {
                    parser
                        .errors
                        .push(ParseError::Generic("end tag form without open form"));
                    return Step::Done;
                }
            };
            let node_ptr = Rc::as_ptr(&node);
            // Check if node is in scope.
            let in_scope = parser.open_elements.iter().any(|n| {
                Rc::as_ptr(n) == node_ptr
                    && n.borrow()
                        .kind
                        .as_element()
                        .map(|e| e.local_name == "form")
                        .unwrap_or(false)
            });
            // Simplified scope check: if node is on the stack at all.
            let on_stack = parser
                .open_elements
                .iter()
                .any(|n| Rc::as_ptr(n) == node_ptr);
            if !on_stack {
                parser
                    .errors
                    .push(ParseError::Generic("end tag form without form in scope"));
                return Step::Done;
            }
            let _ = in_scope;
            helpers::generate_implied_end_tags(parser, None);
            // If current node is not node, parse error.
            let current_is_node = parser
                .open_elements
                .last()
                .map(|n| Rc::as_ptr(n) == node_ptr)
                .unwrap_or(false);
            if !current_is_node {
                parser
                    .errors
                    .push(ParseError::Generic("end tag form not at current node"));
            }
            // Remove node from the stack (not pop until).
            parser.open_elements.retain(|n| Rc::as_ptr(n) != node_ptr);
            return Step::Done;
        }
        // Template on stack: use the "has form in scope" path.
        if !helpers::has_element_in_scope(parser, "form") {
            parser
                .errors
                .push(ParseError::Generic("end tag form without form in scope"));
            return Step::Done;
        }
        helpers::generate_implied_end_tags(parser, None);
        // If current node is not form, parse error.
        let current_is_form = parser
            .open_elements
            .last()
            .and_then(|n| n.borrow().kind.as_element().map(|e| e.local_name == "form"))
            .unwrap_or(false);
        if !current_is_form {
            parser
                .errors
                .push(ParseError::Generic("end tag form not at current node"));
        }
        // Pop until form.
        while let Some(top) = parser.open_elements.last() {
            let is_form = top
                .borrow()
                .kind
                .as_element()
                .map(|e| e.local_name.as_str())
                == Some("form");
            parser.open_elements.pop();
            if is_form {
                break;
            }
        }
        return Step::Done;
    }

    // </li>/</dd>/</dt>: if not in list/scope, parse error, ignore; else:
    // generate implied end tags except tag, if current != tag, parse error,
    // pop until tag.
    if matches!(name, "li") {
        if !helpers::has_element_in_list_scope(parser, "li") {
            parser
                .errors
                .push(ParseError::UnexpectedEndTag(name.to_string()));
            return Step::Done;
        }
        helpers::generate_implied_end_tags(parser, Some("li"));
        while let Some(top) = parser.open_elements.last() {
            let is_target = top
                .borrow()
                .kind
                .as_element()
                .map(|e| e.local_name.as_str())
                == Some("li");
            parser.open_elements.pop();
            if is_target {
                break;
            }
        }
        return Step::Done;
    }
    if matches!(name, "dd" | "dt") {
        if !helpers::has_element_in_scope(parser, name) {
            parser
                .errors
                .push(ParseError::UnexpectedEndTag(name.to_string()));
            return Step::Done;
        }
        helpers::generate_implied_end_tags(parser, Some(name));
        while let Some(top) = parser.open_elements.last() {
            let top_name = top.borrow().kind.as_element().map(|e| e.local_name.clone());
            let is_target = top_name.as_deref() == Some(name);
            parser.open_elements.pop();
            if is_target {
                break;
            }
        }
        return Step::Done;
    }

    // </h1>-</h6>: if no heading in scope, parse error; else: generate
    // implied end tags, if current is not heading, parse error, pop until
    // heading.
    if matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        let heading_in_scope = ["h1", "h2", "h3", "h4", "h5", "h6"]
            .iter()
            .any(|h| helpers::has_element_in_scope(parser, h));
        if !heading_in_scope {
            parser
                .errors
                .push(ParseError::UnexpectedEndTag(name.to_string()));
            return Step::Done;
        }
        helpers::generate_implied_end_tags(parser, None);
        while let Some(top) = parser.open_elements.last() {
            let top_name = top.borrow().kind.as_element().map(|e| e.local_name.clone());
            let is_heading = matches!(
                top_name.as_deref(),
                Some("h1" | "h2" | "h3" | "h4" | "h5" | "h6")
            );
            parser.open_elements.pop();
            if is_heading {
                break;
            }
        }
        return Step::Done;
    }

    // Formatting end tags (a/b/i/em/strong/code/etc.) — run the adoption
    // agency algorithm (§13.2.6.4.7).
    if matches!(
        name,
        "a" | "b"
            | "big"
            | "code"
            | "em"
            | "font"
            | "i"
            | "nobr"
            | "s"
            | "small"
            | "strike"
            | "strong"
            | "tt"
            | "u"
    ) {
        helpers::adoption_agency(parser, name);
        return Step::Done;
    }

    // Any other end tag: walk the stack from top to bottom.
    // For each node: if name matches, generate implied end tags except name,
    // if current != name, parse error, pop until name, break.
    // Else if node is special (in default scope set), parse error, return.
    for (i, node) in parser.open_elements.iter().enumerate().rev() {
        let node_name = node
            .borrow()
            .kind
            .as_element()
            .map(|e| e.local_name.clone());
        if node_name.as_deref() == Some(name) {
            helpers::generate_implied_end_tags(parser, Some(name));
            // Pop until we've popped the matching node at index i.
            while parser.open_elements.len() > i {
                helpers::pop_open_element(parser);
            }
            return Step::Done;
        }
        // Special element (in default scope list) blocks the search.
        if let Some(n) = node_name.as_deref() {
            if helpers::SPECIAL_ELEMENTS.contains(&n) {
                parser
                    .errors
                    .push(ParseError::UnexpectedEndTag(name.to_string()));
                return Step::Done;
            }
        }
    }

    // No match on the stack: parse error, ignore.
    parser
        .errors
        .push(ParseError::UnexpectedEndTag(name.to_string()));
    Step::Done
}

/// Merge the attributes from `tag` onto `element`, skipping any whose name
/// already exists on the element (per "adjust the attributes" §13.2.6.2).
fn merge_attributes(element: &Rc<RefCell<muskitty_dom::Node>>, tag: &crate::tokenizer::TagToken) {
    let mut e = element.borrow_mut();
    if let NodeKind::Element(ref mut data) = e.kind {
        for (name, value) in &tag.attrs {
            let exists = data
                .attributes
                .iter()
                .any(|a| a.local_name.eq_ignore_ascii_case(name));
            if !exists {
                data.attributes.push(Attribute::new(name, value));
            }
        }
    }
}

// ── After body insertion mode (§13.2.6.4.17) ──────────────────

fn handle_after_body(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Character(c) if is_whitespace(*c) => {
            // Process using the rules for "in body".
            handle_in_body(parser, token, tokenizer)
        }
        Token::Comment(data) => {
            // Insert a comment as the last child of the first element in the
            // open elements stack (the <html> element).
            let html = parser
                .open_elements
                .first()
                .cloned()
                .unwrap_or_else(|| parser.document.clone());
            helpers::insert_comment_at(&html, data, &parser.document);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            // Insert a PI as the last child of the <html> element.
            let html = parser
                .open_elements
                .first()
                .cloned()
                .unwrap_or_else(|| parser.document.clone());
            helpers::insert_processing_instruction_at(&html, target, data, &parser.document);
            Step::Done
        }
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE after body"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "html" => {
            // Process using the rules for "in body".
            handle_in_body(parser, token, tokenizer)
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "body" => {
            // If body not in scope, parse error, ignore. Otherwise switch to
            // "after after body".
            if !helpers::has_element_in_scope(parser, "body") {
                parser
                    .errors
                    .push(ParseError::Generic("end tag body without body in scope"));
                return Step::Done;
            }
            parser.insertion_mode = InsertionMode::AfterAfterBody;
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "html" => {
            // Process the token as if it were an end tag body token, then
            // switch to "after after body". Since `</body>` just switches
            // mode (above), we replicate that here.
            if !helpers::has_element_in_scope(parser, "body") {
                parser
                    .errors
                    .push(ParseError::Generic("end tag html without body in scope"));
                return Step::Done;
            }
            parser.insertion_mode = InsertionMode::AfterAfterBody;
            Step::Done
        }
        Token::EOF => Step::Done,
        _ => {
            // Parse error; switch to "in body", reprocess.
            parser
                .errors
                .push(ParseError::Generic("unexpected token after body"));
            parser.insertion_mode = InsertionMode::InBody;
            Step::Reprocess
        }
    }
}

// ── After after body insertion mode (§13.2.6.4.20) ────────────

fn handle_after_after_body(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Comment(data) => {
            // Insert a comment at the Document.
            helpers::insert_comment_at(&parser.document, data, &parser.document);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            // Insert a PI at the Document.
            helpers::insert_processing_instruction_at(
                &parser.document,
                target,
                data,
                &parser.document,
            );
            Step::Done
        }
        Token::Doctype(_) => {
            // Process using the rules for "in body".
            handle_in_body(parser, token, tokenizer)
        }
        Token::Character(c) if is_whitespace(*c) => {
            // Process using the rules for "in body".
            handle_in_body(parser, token, tokenizer)
        }
        Token::EOF => Step::Done,
        _ => {
            // Parse error; switch to "in body", reprocess.
            parser
                .errors
                .push(ParseError::Generic("unexpected token after after body"));
            parser.insertion_mode = InsertionMode::InBody;
            Step::Reprocess
        }
    }
}

// ── Table insertion modes (§13.2.6.4.9–§13.2.6.4.15) ──────────

/// Pop elements from the open elements stack until a table-context
/// element is current. Per §13.2.6.4.9 (line 4638), the table-context
/// elements are: `table`, `template`, `html` only.
fn clear_stack_to_table_context(parser: &mut HtmlTreeConstructor) {
    const TABLE_CONTEXT: &[&str] = &["table", "template", "html"];
    while let Some(top) = parser.open_elements.last() {
        let is_ctx = top
            .borrow()
            .kind
            .as_element()
            .map(|e| TABLE_CONTEXT.contains(&e.local_name.as_str()))
            .unwrap_or(false);
        if is_ctx {
            break;
        }
        parser.open_elements.pop();
    }
}

/// Pop elements from the open elements stack until a table row context
/// element is current (§13.2.6.4.13 "clear the stack back to a table
/// body context"). Row-context elements: `tbody`, `tfoot`, `thead`,
/// `html`, `template`.
fn clear_stack_to_table_body_context(parser: &mut HtmlTreeConstructor) {
    const BODY_CONTEXT: &[&str] = &["tbody", "tfoot", "thead", "html", "template"];
    while let Some(top) = parser.open_elements.last() {
        let is_ctx = top
            .borrow()
            .kind
            .as_element()
            .map(|e| BODY_CONTEXT.contains(&e.local_name.as_str()))
            .unwrap_or(false);
        if is_ctx {
            break;
        }
        parser.open_elements.pop();
    }
}

/// Pop elements from the open elements stack until a table row element
/// is current (§13.2.6.4.14 "clear the stack back to a table row
/// context"). Row elements: `tr`, `html`, `template`.
fn clear_stack_to_row_context(parser: &mut HtmlTreeConstructor) {
    const ROW_CONTEXT: &[&str] = &["tr", "html", "template"];
    while let Some(top) = parser.open_elements.last() {
        let is_ctx = top
            .borrow()
            .kind
            .as_element()
            .map(|e| ROW_CONTEXT.contains(&e.local_name.as_str()))
            .unwrap_or(false);
        if is_ctx {
            break;
        }
        parser.open_elements.pop();
    }
}

// ── InTable insertion mode (§13.2.6.4.9) ────────────────────────

fn handle_in_table(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        // §13.2.6.4.9: A character token, if the current node is table,
        // tbody, template, tfoot, thead, or tr → switch to InTableText.
        Token::Character(c) => {
            let current_is_table_context = parser
                .open_elements
                .last()
                .and_then(|n| {
                    n.borrow().kind.as_element().map(|e| {
                        matches!(
                            e.local_name.as_str(),
                            "table" | "tbody" | "template" | "tfoot" | "thead" | "tr"
                        )
                    })
                })
                .unwrap_or(false);
            if current_is_table_context {
                parser.pending_table_text.push(*c);
                // Save the current insertion mode (which may be InRow,
                // InCaption, etc. if handle_in_table was called via
                // "process using in table rules" from another mode) so
                // that InTableText restores to the correct mode.
                let prev_mode = parser.insertion_mode;
                parser.insertion_mode = InsertionMode::InTableText;
                parser.original_insertion_mode = Some(prev_mode);
                Step::Done
            } else {
                // Foster parent: process using InBody rules (§13.2.6.4.9
                // "Anything else" → foster parenting).
                parser.foster_parenting = true;
                let step = handle_in_body(parser, token, tokenizer);
                parser.foster_parenting = false;
                step
            }
        }
        Token::Comment(data) => {
            helpers::insert_comment(parser, data);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction(parser, target, data);
            Step::Done
        }
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE in table"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start => {
            let name = tag.name.as_str();
            match name {
                "caption" => {
                    clear_stack_to_table_context(parser);
                    helpers::add_formatting_marker(parser);
                    helpers::insert_element(parser, tag);
                    parser.insertion_mode = InsertionMode::InCaption;
                    Step::Done
                }
                "colgroup" => {
                    clear_stack_to_table_context(parser);
                    helpers::insert_element(parser, tag);
                    parser.insertion_mode = InsertionMode::InColumnGroup;
                    Step::Done
                }
                "col" => {
                    clear_stack_to_table_context(parser);
                    create_and_push(parser, "colgroup");
                    parser.insertion_mode = InsertionMode::InColumnGroup;
                    Step::Reprocess
                }
                "tbody" | "tfoot" | "thead" => {
                    clear_stack_to_table_context(parser);
                    helpers::insert_element(parser, tag);
                    parser.insertion_mode = InsertionMode::InTableBody;
                    Step::Done
                }
                "td" | "th" | "tr" => {
                    clear_stack_to_table_context(parser);
                    create_and_push(parser, "tbody");
                    parser.insertion_mode = InsertionMode::InTableBody;
                    Step::Reprocess
                }
                "style" | "script" | "template" => {
                    // Process using the rules for "in head" (§13.2.6.4.9).
                    handle_in_head(parser, token, tokenizer)
                }
                "input" => {
                    // §13.2.6.4.9: If type=hidden (case-insensitive), parse
                    // error, insert element, pop, acknowledge self-closing.
                    // Otherwise, foster parent.
                    let is_hidden = tag.attrs.iter().any(|(k, v)| {
                        k.eq_ignore_ascii_case("type") && v.eq_ignore_ascii_case("hidden")
                    });
                    if is_hidden {
                        parser
                            .errors
                            .push(ParseError::Generic("hidden input in table"));
                        helpers::insert_element(parser, tag);
                        parser.open_elements.pop();
                        Step::Done
                    } else {
                        foster_parent_in_body(parser, token, tokenizer)
                    }
                }
                "table" => {
                    // §13.2.6.4.9: Parse error. If no table in table scope,
                    // ignore. Otherwise: pop until table popped, reset
                    // insertion mode, reprocess.
                    parser
                        .errors
                        .push(ParseError::Generic("unexpected table start tag in table"));
                    if helpers::has_element_in_table_scope(parser, "table") {
                        while let Some(top) = parser.open_elements.pop() {
                            let is_table = top
                                .borrow()
                                .kind
                                .as_element()
                                .map(|e| e.local_name == "table")
                                .unwrap_or(false);
                            if is_table {
                                break;
                            }
                        }
                        reset_insertion_mode(parser);
                        Step::Reprocess
                    } else {
                        Step::Done
                    }
                }
                "form" => {
                    // §13.2.6.4.9: Parse error. If there is a template
                    // element on the stack, or the form element pointer is
                    // not null, ignore the token. Otherwise: insert an HTML
                    // element for the token, set the form element pointer to
                    // point to it, and pop that form element off the stack.
                    parser
                        .errors
                        .push(ParseError::Generic("unexpected form start tag in table"));
                    let has_template = helpers::has_element_in_stack(parser, "template");
                    if has_template || parser.form_element.is_some() {
                        Step::Done
                    } else {
                        let element = helpers::create_element_for_token(parser, tag);
                        helpers::insert_node(parser, &element);
                        parser.open_elements.push(element.clone());
                        parser.form_element = Some(element);
                        parser.open_elements.pop();
                        Step::Done
                    }
                }
                _ => foster_parent_in_body(parser, token, tokenizer),
            }
        }
        Token::Tag(tag) if tag.kind == TagKind::End => match tag.name.as_str() {
            "table" => {
                // §13.2.6.4.9: If no table in table scope, parse error,
                // ignore. Otherwise: pop until table popped, reset insertion
                // mode appropriately.
                if !helpers::has_element_in_table_scope(parser, "table") {
                    parser.errors.push(ParseError::Generic(
                        "end table without table in table scope",
                    ));
                    return Step::Done;
                }
                while let Some(top) = parser.open_elements.pop() {
                    let is_table = top
                        .borrow()
                        .kind
                        .as_element()
                        .map(|e| e.local_name == "table")
                        .unwrap_or(false);
                    if is_table {
                        break;
                    }
                }
                reset_insertion_mode(parser);
                Step::Done
            }
            "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot" | "th"
            | "thead" | "tr" => {
                parser
                    .errors
                    .push(ParseError::Generic("unexpected end tag in table"));
                Step::Done
            }
            "template" => handle_in_head(parser, token, tokenizer),
            _ => foster_parent_in_body(parser, token, tokenizer),
        },
        Token::EOF => {
            // Reprocess in InBody for EOF handling (template/fragment checks).
            parser.insertion_mode = InsertionMode::InBody;
            Step::Reprocess
        }
        _ => foster_parent_in_body(parser, token, tokenizer),
    }
}

/// Foster-parent a token by enabling foster parenting, processing it as
/// InBody, then disabling foster parenting (§13.2.6.4.9 "anything else").
fn foster_parent_in_body(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    parser
        .errors
        .push(ParseError::Generic("foster parenting in table"));
    parser.foster_parenting = true;
    let step = handle_in_body(parser, token, tokenizer);
    parser.foster_parenting = false;
    step
}

// ── InTableText insertion mode (§13.2.6.4.10) ───────────────────

fn handle_in_table_text(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Character('\0') => {
            parser
                .errors
                .push(ParseError::Generic("unexpected null in table text"));
            Step::Done
        }
        Token::Character(c) => {
            parser.pending_table_text.push(*c);
            Step::Done
        }
        _ => {
            // Process the pending table character tokens (§13.2.6.4.10):
            // - If any is non-whitespace → foster-parent the whole run.
            // - If all are whitespace → insert into the current node (table/
            //   tbody/tr etc.), preserving layout whitespace.
            let pending = std::mem::take(&mut parser.pending_table_text);
            let has_non_ws = pending.chars().any(|c| !is_whitespace(c));
            if has_non_ws {
                parser.foster_parenting = true;
                for c in pending.chars() {
                    handle_in_body(parser, &Token::Character(c), tokenizer);
                }
                parser.foster_parenting = false;
            } else {
                for c in pending.chars() {
                    helpers::insert_character(parser, c);
                }
            }
            // Restore original insertion mode (InTable) and reprocess.
            parser.insertion_mode = parser
                .original_insertion_mode
                .take()
                .unwrap_or(InsertionMode::InTable);
            Step::Reprocess
        }
    }
}

// ── InCaption insertion mode (§13.2.6.4.11) ─────────────────────

fn handle_in_caption(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "caption" => {
            if !helpers::has_element_in_scope(parser, "caption") {
                parser
                    .errors
                    .push(ParseError::Generic("end caption without caption in scope"));
                return Step::Done;
            }
            helpers::generate_implied_end_tags(parser, None);
            // Pop until a caption element is popped.
            while let Some(top) = parser.open_elements.pop() {
                let is_caption = top
                    .borrow()
                    .kind
                    .as_element()
                    .map(|e| e.local_name == "caption")
                    .unwrap_or(false);
                if is_caption {
                    break;
                }
            }
            helpers::clear_active_formatting_to_last_marker(parser);
            parser.insertion_mode = InsertionMode::InTable;
            Step::Done
        }
        Token::Tag(tag)
            if tag.kind == TagKind::Start
                && matches!(
                    tag.name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
        {
            // Parse error; act as if </caption> was seen, then reprocess.
            parser
                .errors
                .push(ParseError::Generic("unexpected table start tag in caption"));
            close_caption(parser);
            Step::Reprocess
        }
        Token::Tag(tag)
            if tag.kind == TagKind::End
                && matches!(
                    tag.name.as_str(),
                    "table" | "tbody" | "tfoot" | "thead" | "tr" | "td" | "th"
                ) =>
        {
            parser
                .errors
                .push(ParseError::Generic("unexpected table end tag in caption"));
            close_caption(parser);
            Step::Reprocess
        }
        _ => {
            // Anything else: process using the rules for InBody.
            handle_in_body(parser, token, tokenizer)
        }
    }
}

/// Close the current caption (used by InCaption's "act as </caption>").
fn close_caption(parser: &mut HtmlTreeConstructor) {
    if helpers::has_element_in_scope(parser, "caption") {
        helpers::generate_implied_end_tags(parser, None);
        while let Some(top) = parser.open_elements.pop() {
            let is_caption = top
                .borrow()
                .kind
                .as_element()
                .map(|e| e.local_name == "caption")
                .unwrap_or(false);
            if is_caption {
                break;
            }
        }
        helpers::clear_active_formatting_to_last_marker(parser);
        parser.insertion_mode = InsertionMode::InTable;
    }
}

// ── InColumnGroup insertion mode (§13.2.6.4.12) ─────────────────

fn handle_in_column_group(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Character(c) if is_whitespace(*c) => {
            helpers::insert_character(parser, *c);
            Step::Done
        }
        Token::Comment(data) => {
            helpers::insert_comment(parser, data);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction(parser, target, data);
            Step::Done
        }
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE in colgroup"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start => match tag.name.as_str() {
            "html" => handle_in_body(parser, token, tokenizer),
            "col" => {
                helpers::insert_element(parser, tag);
                parser.open_elements.pop();
                Step::Done
            }
            "template" => handle_in_head(parser, token, tokenizer),
            _ => close_colgroup_and_reprocess(parser),
        },
        Token::Tag(tag) if tag.kind == TagKind::End => match tag.name.as_str() {
            "colgroup" => {
                if !helpers::has_element_in_scope(parser, "colgroup") {
                    parser.errors.push(ParseError::Generic(
                        "end colgroup without colgroup in scope",
                    ));
                    return Step::Done;
                }
                parser.open_elements.pop();
                parser.insertion_mode = InsertionMode::InTable;
                Step::Done
            }
            "col" => {
                parser.errors.push(ParseError::Generic("end col; ignored"));
                Step::Done
            }
            "template" => handle_in_head(parser, token, tokenizer),
            _ => close_colgroup_and_reprocess(parser),
        },
        Token::EOF => handle_in_body(parser, token, tokenizer),
        _ => close_colgroup_and_reprocess(parser),
    }
}

/// Close the colgroup (if present) and reprocess in InTable.
fn close_colgroup_and_reprocess(parser: &mut HtmlTreeConstructor) -> Step {
    if helpers::has_element_in_scope(parser, "colgroup") {
        parser.open_elements.pop();
        parser.insertion_mode = InsertionMode::InTable;
        Step::Reprocess
    } else {
        // No colgroup in scope: parse error, ignore the token.
        parser.errors.push(ParseError::Generic(
            "unexpected token; no colgroup in scope",
        ));
        Step::Done
    }
}

// ── InTableBody insertion mode (§13.2.6.4.13) ───────────────────

fn handle_in_table_body(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "tr" => {
            clear_stack_to_table_body_context(parser);
            helpers::insert_element(parser, tag);
            parser.insertion_mode = InsertionMode::InRow;
            Step::Done
        }
        Token::Tag(tag)
            if tag.kind == TagKind::Start && matches!(tag.name.as_str(), "td" | "th") =>
        {
            clear_stack_to_table_body_context(parser);
            create_and_push(parser, "tr");
            parser.insertion_mode = InsertionMode::InRow;
            Step::Reprocess
        }
        Token::Tag(tag)
            if tag.kind == TagKind::End
                && matches!(tag.name.as_str(), "tbody" | "tfoot" | "thead") =>
        {
            let name = tag.name.clone();
            if !helpers::has_element_in_table_scope(parser, &name) {
                parser.errors.push(ParseError::Generic(
                    "end tag without element in table scope",
                ));
                return Step::Done;
            }
            clear_stack_to_table_body_context(parser);
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::InTable;
            Step::Done
        }
        Token::Tag(tag)
            if (tag.kind == TagKind::Start
                && matches!(
                    tag.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead"
                ))
                || (tag.kind == TagKind::End
                    && matches!(tag.name.as_str(), "table" | "tbody" | "tfoot" | "thead")) =>
        {
            // Act as if </tbody> (or </tfoot>/</thead>) was seen, then
            // reprocess. If no tbody/tfoot/thead is in table scope, ignore.
            if !helpers::has_element_in_table_scope(parser, "tbody")
                && !helpers::has_element_in_table_scope(parser, "tfoot")
                && !helpers::has_element_in_table_scope(parser, "thead")
            {
                parser.errors.push(ParseError::Generic(
                    "unexpected token; no table body in scope",
                ));
                return Step::Done;
            }
            clear_stack_to_table_body_context(parser);
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::InTable;
            Step::Reprocess
        }
        _ => {
            // §13.2.6.4.13 "Anything else": Process the token using the
            // rules for the "in table" insertion mode. Per §13.2.6, "using
            // the rules for m" means invoking m's handler directly WITHOUT
            // changing the current insertion mode (unless m's rules
            // themselves switch it). Switching to InTable and reprocessing
            // here would break the table body context: a subsequent <tr>
            // would be handled by InTable's "td/th/tr" branch (which clears
            // the stack back to a *table* context, popping the existing
            // tbody, and inserts a *new* tbody), instead of InTableBody's
            // "tr" branch (which clears back to a *table body* context,
            // preserving the existing tbody).
            handle_in_table(parser, token, tokenizer)
        }
    }
}

// ── InRow insertion mode (§13.2.6.4.14) ─────────────────────────

fn handle_in_row(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Tag(tag)
            if tag.kind == TagKind::Start && matches!(tag.name.as_str(), "td" | "th") =>
        {
            clear_stack_to_row_context(parser);
            helpers::insert_element(parser, tag);
            parser.insertion_mode = InsertionMode::InCell;
            helpers::add_formatting_marker(parser);
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "tr" => {
            if !helpers::has_element_in_table_scope(parser, "tr") {
                parser
                    .errors
                    .push(ParseError::Generic("end tr without tr in table scope"));
                return Step::Done;
            }
            clear_stack_to_row_context(parser);
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::InTableBody;
            Step::Done
        }
        Token::Tag(tag)
            if (tag.kind == TagKind::Start
                && matches!(
                    tag.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" | "tr"
                ))
                || (tag.kind == TagKind::End
                    && matches!(tag.name.as_str(), "table" | "tbody" | "tfoot" | "thead")) =>
        {
            // §13.2.6.4.14: Act as if </tr> was seen, then reprocess.
            if !helpers::has_element_in_table_scope(parser, "tr") {
                parser.errors.push(ParseError::Generic(
                    "unexpected token; no tr in table scope",
                ));
                return Step::Done;
            }
            clear_stack_to_row_context(parser);
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::InTableBody;
            Step::Reprocess
        }
        _ => {
            // §13.2.6.4.14 "Anything else": Process using InTable rules,
            // but keep insertion mode as InRow (don't switch permanently).
            handle_in_table(parser, token, tokenizer)
        }
    }
}

// ── InCell insertion mode (§13.2.6.4.15) ────────────────────────

/// Close the cell algorithm (§13.2.6.4.15): generate implied end tags,
/// pop until td/th is popped, clear formatting to last marker, switch to
/// InRow. Called from InCell when a start tag or an in-scope end tag
/// requires closing the current cell.
fn close_cell_and_switch_to_in_row(parser: &mut HtmlTreeConstructor) {
    helpers::generate_implied_end_tags(parser, None);
    // Pop until a td or th element has been popped.
    while let Some(top) = parser.open_elements.pop() {
        let is_cell = top
            .borrow()
            .kind
            .as_element()
            .map(|e| matches!(e.local_name.as_str(), "td" | "th"))
            .unwrap_or(false);
        if is_cell {
            break;
        }
    }
    helpers::clear_active_formatting_to_last_marker(parser);
    parser.insertion_mode = InsertionMode::InRow;
}

fn handle_in_cell(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Tag(tag) if tag.kind == TagKind::End && matches!(tag.name.as_str(), "td" | "th") => {
            let name = tag.name.clone();
            if !helpers::has_element_in_table_scope(parser, &name) {
                parser
                    .errors
                    .push(ParseError::Generic("end cell without cell in table scope"));
                return Step::Done;
            }
            helpers::generate_implied_end_tags(parser, None);
            // Pop until an HTML element with the same tag name is popped
            // (§13.2.6.4.15 step 3). The namespace check is essential:
            // foreign elements such as `<svg td>` must NOT match — otherwise
            // the HTML `<td>` and its foreign subtree remain on the stack,
            // and subsequent tokens get routed to foreign content instead
            // of HTML (see namespace-sensitivity.dat #1).
            while let Some(top) = parser.open_elements.pop() {
                let is_target = top
                    .borrow()
                    .kind
                    .as_element()
                    .map(|e| e.namespace == muskitty_dom::Namespace::Html && e.local_name == name)
                    .unwrap_or(false);
                if is_target {
                    break;
                }
            }
            helpers::clear_active_formatting_to_last_marker(parser);
            parser.insertion_mode = InsertionMode::InRow;
            Step::Done
        }
        // Start tags that imply closing the cell (§13.2.6.4.15):
        // caption/col/colgroup/tbody/td/tfoot/th/thead/tr → close cell, reprocess.
        Token::Tag(tag)
            if tag.kind == TagKind::Start
                && matches!(
                    tag.name.as_str(),
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
        {
            let _ = tag;
            close_cell_and_switch_to_in_row(parser);
            Step::Reprocess
        }
        // End tags: table/tbody/tfoot/thead/tr → if the token's tag name is
        // in table scope, close cell and reprocess; else parse error, ignore.
        Token::Tag(tag)
            if tag.kind == TagKind::End
                && matches!(
                    tag.name.as_str(),
                    "table" | "tbody" | "tfoot" | "thead" | "tr"
                ) =>
        {
            let name = tag.name.clone();
            if !helpers::has_element_in_table_scope(parser, &name) {
                parser
                    .errors
                    .push(ParseError::Generic("end tag not in table scope in cell"));
                Step::Done
            } else {
                close_cell_and_switch_to_in_row(parser);
                Step::Reprocess
            }
        }
        _ => {
            // Anything else: process using the rules for InBody.
            handle_in_body(parser, token, tokenizer)
        }
    }
}

// ── Remaining insertion modes (§13.2.6.4.6, §13.2.6.4.16–§13.2.6.4.23) ──

// ── InHeadNoscript insertion mode (§13.2.6.4.6) ────────────────

fn handle_in_head_noscript(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE in noscript"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "html" => {
            handle_in_body(parser, token, tokenizer)
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "noscript" => {
            // Pop the noscript element and switch to InHead.
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::InHead;
            Step::Done
        }
        Token::Character(c) if is_whitespace(*c) => handle_in_head(parser, token, tokenizer),
        Token::Comment(data) => handle_in_head(parser, &Token::Comment(data.clone()), tokenizer),
        Token::ProcessingInstruction { target, data } => handle_in_head(
            parser,
            &Token::ProcessingInstruction {
                target: target.clone(),
                data: data.clone(),
            },
            tokenizer,
        ),
        Token::Tag(tag)
            if tag.kind == TagKind::Start
                && matches!(
                    tag.name.as_str(),
                    "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"
                ) =>
        {
            handle_in_head(parser, token, tokenizer)
        }
        Token::Tag(tag)
            if tag.kind == TagKind::Start && matches!(tag.name.as_str(), "head" | "noscript") =>
        {
            parser
                .errors
                .push(ParseError::Generic("unexpected head/noscript in noscript"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "br" => {
            // </br> is treated as a start tag (anything else).
            handle_anything_else_in_head_noscript(parser, token)
        }
        Token::Tag(tag) if tag.kind == TagKind::End => {
            // §13.2.6.4.5: Any other end tag → Parse error. Ignore.
            let _ = tag;
            parser
                .errors
                .push(ParseError::Generic("unexpected end tag in noscript"));
            Step::Done
        }
        Token::EOF => {
            parser
                .errors
                .push(ParseError::Generic("unexpected EOF in noscript"));
            // Pop noscript, reprocess in InHead.
            parser.open_elements.pop();
            parser.insertion_mode = InsertionMode::InHead;
            Step::Reprocess
        }
        _ => handle_anything_else_in_head_noscript(parser, token),
    }
}

fn handle_anything_else_in_head_noscript(parser: &mut HtmlTreeConstructor, _token: &Token) -> Step {
    parser
        .errors
        .push(ParseError::Generic("unexpected token in noscript"));
    parser.open_elements.pop();
    parser.insertion_mode = InsertionMode::InHead;
    Step::Reprocess
}

// ── InTemplate insertion mode (§13.2.6.4.16) ────────────────────

fn handle_in_template(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    // §13.2.6.4.16: In template insertion mode.
    match token {
        // Character / comment / PI / DOCTYPE → process using InBody rules.
        Token::Character(_)
        | Token::Comment(_)
        | Token::ProcessingInstruction { .. }
        | Token::Doctype(_) => handle_in_body(parser, token, tokenizer),

        // base/.../template/title start tag, or template end tag → InHead.
        Token::Tag(tag)
            if tag.kind == TagKind::Start
                && matches!(
                    tag.name.as_str(),
                    "base"
                        | "basefont"
                        | "bgsound"
                        | "link"
                        | "meta"
                        | "noframes"
                        | "script"
                        | "style"
                        | "template"
                        | "title"
                ) =>
        {
            handle_in_head(parser, token, tokenizer)
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "template" => {
            let _ = tag;
            handle_in_head(parser, token, tokenizer)
        }

        // caption/colgroup/tbody/tfoot/thead start tag:
        // Pop current template insertion mode, push InTable, switch, reprocess.
        Token::Tag(tag)
            if tag.kind == TagKind::Start
                && matches!(
                    tag.name.as_str(),
                    "caption" | "colgroup" | "tbody" | "tfoot" | "thead"
                ) =>
        {
            let _ = tag;
            parser.template_insertion_modes.pop();
            parser.template_insertion_modes.push(InsertionMode::InTable);
            parser.insertion_mode = InsertionMode::InTable;
            Step::Reprocess
        }

        // col start tag: push InColumnGroup, switch, reprocess.
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "col" => {
            let _ = tag;
            parser.template_insertion_modes.pop();
            parser
                .template_insertion_modes
                .push(InsertionMode::InColumnGroup);
            parser.insertion_mode = InsertionMode::InColumnGroup;
            Step::Reprocess
        }

        // tr start tag: push InTableBody, switch, reprocess.
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "tr" => {
            let _ = tag;
            parser.template_insertion_modes.pop();
            parser
                .template_insertion_modes
                .push(InsertionMode::InTableBody);
            parser.insertion_mode = InsertionMode::InTableBody;
            Step::Reprocess
        }

        // td/th start tag: push InRow, switch, reprocess.
        Token::Tag(tag)
            if tag.kind == TagKind::Start && matches!(tag.name.as_str(), "td" | "th") =>
        {
            let _ = tag;
            parser.template_insertion_modes.pop();
            parser.template_insertion_modes.push(InsertionMode::InRow);
            parser.insertion_mode = InsertionMode::InRow;
            Step::Reprocess
        }

        // Any other start tag: push InBody, switch, reprocess.
        Token::Tag(tag) if tag.kind == TagKind::Start => {
            let _ = tag;
            parser.template_insertion_modes.pop();
            parser.template_insertion_modes.push(InsertionMode::InBody);
            parser.insertion_mode = InsertionMode::InBody;
            Step::Reprocess
        }

        // Any other end tag: parse error, ignore.
        Token::Tag(tag) if tag.kind == TagKind::End => {
            parser
                .errors
                .push(ParseError::Generic("unexpected end tag in template"));
            let _ = tag;
            Step::Done
        }

        // EOF: if no template in stack, stop parsing (fragment case).
        // Otherwise: parse error, pop until template popped, clear to last
        // marker, pop template insertion mode, reset insertion mode, reprocess.
        Token::EOF => {
            if !template_in_stack(parser) {
                Step::Done
            } else {
                parser
                    .errors
                    .push(ParseError::Generic("unexpected EOF in template"));
                while let Some(top) = parser.open_elements.pop() {
                    let is_html_template = top
                        .borrow()
                        .kind
                        .as_element()
                        .map(|e| {
                            e.local_name == "template"
                                && e.namespace == muskitty_dom::Namespace::Html
                        })
                        .unwrap_or(false);
                    if is_html_template {
                        break;
                    }
                }
                helpers::clear_active_formatting_to_last_marker(parser);
                parser.template_insertion_modes.pop();
                reset_insertion_mode(parser);
                Step::Reprocess
            }
        }

        // Unreachable: TagKind only has Start and End, both handled above.
        _ => Step::Done,
    }
}

/// Check if there is an HTML template element on the stack of open elements.
///
/// Per §13.2.6.2, a "template element" is an HTML element whose local name
/// is "template". SVG-namespaced `<template>` elements (which can appear
/// inside `<svg>`) do not count.
fn template_in_stack(parser: &HtmlTreeConstructor) -> bool {
    parser.open_elements.iter().any(|n| {
        n.borrow()
            .kind
            .as_element()
            .map(|e| e.local_name == "template" && e.namespace == muskitty_dom::Namespace::Html)
            .unwrap_or(false)
    })
}

// ── InFrameset insertion mode (§13.2.6.4.21) ────────────────────

fn handle_in_frameset(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Character(c) if is_whitespace(*c) => {
            helpers::insert_character(parser, *c);
            Step::Done
        }
        Token::Comment(data) => {
            helpers::insert_comment(parser, data);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction(parser, target, data);
            Step::Done
        }
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE in frameset"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "html" => {
            handle_in_body(parser, token, tokenizer)
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "frameset" => {
            helpers::insert_element(parser, tag);
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "frameset" => {
            if parser.open_elements.len() == 1 {
                parser
                    .errors
                    .push(ParseError::Generic("unexpected </frameset> at root"));
                return Step::Done;
            }
            parser.open_elements.pop();
            // If current node is not a frameset, switch to AfterFrameset.
            let is_frameset = parser
                .open_elements
                .last()
                .and_then(|n| {
                    n.borrow()
                        .kind
                        .as_element()
                        .map(|e| e.local_name == "frameset")
                })
                .unwrap_or(false);
            if !is_frameset {
                parser.insertion_mode = InsertionMode::AfterFrameset;
            }
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "frame" => {
            helpers::insert_element(parser, tag);
            parser.open_elements.pop();
            parser.frameset_ok = false;
            Step::Done
        }
        // §13.2.6.4.18: Only <noframes> is delegated to InHead. The real
        // tokenizer must be passed so that InHead can switch it to RAWTEXT
        // (§13.2.6.4.4) before entering Text mode.
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "noframes" => {
            handle_in_head(parser, token, tokenizer)
        }
        Token::EOF => {
            if parser.open_elements.len() != 1 {
                parser
                    .errors
                    .push(ParseError::Generic("unexpected EOF in frameset"));
            }
            Step::Done
        }
        _ => {
            parser
                .errors
                .push(ParseError::Generic("unexpected token in frameset"));
            Step::Done
        }
    }
}

// ── AfterFrameset insertion mode (§13.2.6.4.22) ─────────────────

fn handle_after_frameset(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Character(c) if is_whitespace(*c) => {
            helpers::insert_character(parser, *c);
            Step::Done
        }
        Token::Character('\0') => {
            // §13.2.6.4.18: Parse error. Insert U+FFFD.
            parser.errors.push(ParseError::Generic(
                "unexpected null character after frameset",
            ));
            helpers::insert_character(parser, '\u{FFFD}');
            Step::Done
        }
        Token::Comment(data) => {
            helpers::insert_comment(parser, data);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction(parser, target, data);
            Step::Done
        }
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected DOCTYPE after frameset"));
            Step::Done
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "html" => {
            handle_in_body(parser, token, tokenizer)
        }
        Token::Tag(tag) if tag.kind == TagKind::End && tag.name == "html" => {
            parser.insertion_mode = InsertionMode::AfterAfterFrameset;
            Step::Done
        }
        // §13.2.6.4.19: Only <noframes> is delegated to InHead. The real
        // tokenizer must be passed so that InHead can switch it to RAWTEXT
        // (§13.2.6.4.4) before entering Text mode.
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "noframes" => {
            handle_in_head(parser, token, tokenizer)
        }
        Token::EOF => Step::Done,
        _ => {
            parser
                .errors
                .push(ParseError::Generic("unexpected token after frameset"));
            Step::Done
        }
    }
}

// ── AfterAfterFrameset insertion mode (§13.2.6.4.23) ────────────

fn handle_after_after_frameset(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> Step {
    match token {
        Token::Comment(data) => {
            // Insert at the Document.
            helpers::insert_comment_at(&parser.document, data, &parser.document);
            Step::Done
        }
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction_at(
                &parser.document,
                target,
                data,
                &parser.document,
            );
            Step::Done
        }
        // §13.2.6.4.21 (line 5278-5283): DOCTYPE, whitespace character,
        // and <html> start tag are all processed using the "in body" rules.
        Token::Doctype(_) | Token::Character(_) if matches!(token, Token::Character(c) if is_whitespace(*c)) => {
            handle_in_body(parser, token, tokenizer)
        }
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "html" => {
            handle_in_body(parser, token, tokenizer)
        }
        Token::EOF => Step::Done,
        // §13.2.6.4.21: Only <noframes> is delegated to InHead. The real
        // tokenizer must be passed so that InHead can switch it to RAWTEXT
        // (§13.2.6.4.4) before entering Text mode.
        Token::Tag(tag) if tag.kind == TagKind::Start && tag.name == "noframes" => {
            handle_in_head(parser, token, tokenizer)
        }
        _ => {
            parser
                .errors
                .push(ParseError::Generic("unexpected token after after frameset"));
            Step::Done
        }
    }
}

// ── Insertion mode reset (§13.2.6.4.2) ──────────────────────────

/// Reset the insertion mode appropriately (§13.2.6.4.2).
///
/// Walks the stack of open elements from bottom to top. The first matching
/// condition sets the new insertion mode. If no condition matches, the
/// insertion mode is set to InBody.
pub fn reset_insertion_mode(parser: &mut HtmlTreeConstructor) {
    // §13.2.6.4.2: walk the stack of open elements from the top (last
    // node) downward. `last` is true when processing the FIRST node in
    // the stack (index 0), per §13.2.6.2 step 3: "If node is the first
    // node in the stack of open elements, then set last to true."
    // Iterating forward would incorrectly match <head> before <body>.
    for (i, node) in parser.open_elements.iter().enumerate().rev() {
        let is_last = i == 0;
        let local = node
            .borrow()
            .kind
            .as_element()
            .map(|e| e.local_name.clone());
        let local = local.as_deref().unwrap_or("");
        match local {
            // §13.2.6.4.2: No "select" case — InSelect mode has been removed
            // from the spec. <select> content is handled by InBody with
            // "select in scope" checks on specific start tags.
            "td" | "th" if !is_last => {
                parser.insertion_mode = InsertionMode::InCell;
                return;
            }
            "tr" => {
                parser.insertion_mode = InsertionMode::InRow;
                return;
            }
            "tbody" | "thead" | "tfoot" => {
                parser.insertion_mode = InsertionMode::InTableBody;
                return;
            }
            "caption" => {
                parser.insertion_mode = InsertionMode::InCaption;
                return;
            }
            "colgroup" => {
                parser.insertion_mode = InsertionMode::InColumnGroup;
                return;
            }
            "table" => {
                parser.insertion_mode = InsertionMode::InTable;
                return;
            }
            "template" => {
                // Per §13.2.6.2, a "template element" is an HTML element
                // whose local name is "template". An SVG-namespaced
                // `<template>` (inside `<svg>`) must NOT trigger the
                // template insertion-mode reset — skip it and keep
                // searching down the stack.
                let is_html_template = node
                    .borrow()
                    .kind
                    .as_element()
                    .map(|e| e.namespace == muskitty_dom::Namespace::Html)
                    .unwrap_or(false);
                if is_html_template {
                    // Use the template insertion mode stack if available.
                    if let Some(&mode) = parser.template_insertion_modes.last() {
                        parser.insertion_mode = mode;
                    } else {
                        parser.insertion_mode = InsertionMode::InTemplate;
                    }
                    return;
                }
            }
            "head" if !is_last => {
                parser.insertion_mode = InsertionMode::InHead;
                return;
            }
            "body" => {
                parser.insertion_mode = InsertionMode::InBody;
                return;
            }
            "frameset" => {
                parser.insertion_mode = InsertionMode::InFrameset;
                return;
            }
            "html" => {
                if parser.head_element.is_none() {
                    parser.insertion_mode = InsertionMode::BeforeHead;
                } else {
                    parser.insertion_mode = InsertionMode::AfterHead;
                }
                return;
            }
            _ => {}
        }
    }
    parser.insertion_mode = InsertionMode::InBody;
}
