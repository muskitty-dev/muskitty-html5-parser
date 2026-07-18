//! Foreign content (MathML / SVG) tree construction (§13.2.6.5).
//!
//! Implements:
//! - The tree construction dispatcher's foreign-content branch (§13.2.6).
//! - Integration point predicates (MathML text integration point, HTML
//!   integration point).
//! - `insert a foreign element` (§13.2.6.1).
//! - `adjust MathML attributes`, `adjust SVG attributes`,
//!   `adjust foreign attributes` (§13.2.6.1).
//! - The "rules for parsing tokens in foreign content" (§13.2.6.5) for
//!   character, comment, PI, DOCTYPE, start tag, and end tag tokens.

use std::cell::RefCell;
use std::rc::Rc;

use muskitty_dom::{Attribute, Namespace, Node, NodeKind};

use super::helpers;
use super::HtmlTreeConstructor;
use crate::error::ParseError;
use muskitty_html5_tokenizer::{TagKind, TagToken, Token, Tokenizer};

/// XLink namespace URI.
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
/// XML namespace URI.
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
/// XMLNS namespace URI.
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

// ── Integration points (§13.2.6) ─────────────────────────────────────

/// Return the adjusted current node (§13.2.6.1).
///
/// "The adjusted current node is the context element if the parser was
/// created as part of the HTML fragment parsing algorithm and the stack of
/// open elements has only one element (fragment case); otherwise, the
/// adjusted current node is the current node."
///
/// This implementation does not support fragment parsing, so the adjusted
/// current node is always the current node.
fn adjusted_current_node(parser: &HtmlTreeConstructor) -> Rc<RefCell<Node>> {
    parser.current_node()
}

/// Whether `node` is a MathML text integration point (§13.2.6).
///
/// A node is a MathML text integration point if it is one of:
/// `mi`, `mo`, `mn`, `ms`, `mtext` (in the MathML namespace).
pub fn is_mathml_text_integration_point(node: &Rc<RefCell<Node>>) -> bool {
    let n = node.borrow();
    if let NodeKind::Element(e) = &n.kind {
        if e.namespace == Namespace::MathMl {
            return matches!(e.local_name.as_str(), "mi" | "mo" | "mn" | "ms" | "mtext");
        }
    }
    false
}

/// Whether `node` is an HTML integration point (§13.2.6).
///
/// A node is an HTML integration point if it is one of:
/// - A MathML `annotation-xml` element whose start tag had an attribute
///   `encoding` whose value is an ASCII case-insensitive match for
///   "text/html" or "application/xhtml+xml".
/// - An SVG `foreignObject`, `desc`, or `title` element.
pub fn is_html_integration_point(node: &Rc<RefCell<Node>>) -> bool {
    let n = node.borrow();
    if let NodeKind::Element(e) = &n.kind {
        match e.namespace {
            Namespace::MathMl => {
                if e.local_name == "annotation-xml" {
                    // Check the encoding attribute (ASCII case-insensitive).
                    return e
                        .attributes
                        .iter()
                        .find(|a| a.local_name == "encoding")
                        .map(|a| {
                            let v = a.value.to_ascii_lowercase();
                            v == "text/html" || v == "application/xhtml+xml"
                        })
                        .unwrap_or(false);
                }
                false
            }
            Namespace::Svg => matches!(e.local_name.as_str(), "foreignObject" | "desc" | "title"),
            Namespace::Html => false,
        }
    } else {
        false
    }
}

/// Whether the adjusted current node is in the HTML namespace.
fn adjusted_current_node_is_html(parser: &HtmlTreeConstructor) -> bool {
    let node = adjusted_current_node(parser);
    let n = node.borrow();
    matches!(&n.kind, NodeKind::Element(e) if e.namespace == Namespace::Html)
}

/// The tree construction dispatcher (§13.2.6).
///
/// Returns `true` if the token should be processed by the rules for parsing
/// tokens in foreign content; `false` if it should be processed by the
/// current insertion mode in HTML content.
///
/// The dispatcher routes a token to foreign content only when the adjusted
/// current node is *not* an HTML-namespace element and none of the
/// integration-point escape hatches apply.
pub fn dispatcher_routes_to_foreign(parser: &HtmlTreeConstructor, token: &Token) -> bool {
    // If the stack of open elements is empty → HTML content.
    if parser.open_elements.is_empty() {
        return false;
    }
    let current = adjusted_current_node(parser);

    // If the adjusted current node is an element in the HTML namespace →
    // HTML content.
    if adjusted_current_node_is_html(parser) {
        return false;
    }

    // MathML text integration point escape hatches.
    if is_mathml_text_integration_point(&current) {
        match token {
            // Start tag whose tag name is neither "mglyph" nor "malignmark"
            // → HTML content.
            Token::Tag(t)
                if t.kind == TagKind::Start && t.name != "mglyph" && t.name != "malignmark" =>
            {
                return false;
            }
            // Character token → HTML content.
            Token::Character(_) => return false,
            _ => {}
        }
    }

    // MathML annotation-xml + svg start tag → HTML content.
    if let Token::Tag(t) = token {
        if t.kind == TagKind::Start {
            let n = current.borrow();
            if let NodeKind::Element(e) = &n.kind {
                if e.namespace == Namespace::MathMl
                    && e.local_name == "annotation-xml"
                    && t.name == "svg"
                {
                    return false;
                }
            }
        }
    }

    // HTML integration point escape hatches.
    if is_html_integration_point(&current) {
        match token {
            // Start tag → HTML content.
            Token::Tag(t) if t.kind == TagKind::Start => return false,
            // Character token → HTML content.
            Token::Character(_) => return false,
            _ => {}
        }
    }

    // EOF → HTML content (so that the EOF handler runs and pops everything).
    if matches!(token, Token::EOF) {
        return false;
    }

    // Otherwise → foreign content.
    true
}

// ── Attribute adjustment (§13.2.6.1) ─────────────────────────────────

/// Adjust MathML attributes for the token (§13.2.6.1).
///
/// If the token has an attribute named `definitionurl`, change its name to
/// `definitionURL` (note the case difference).
pub fn adjust_mathml_attributes(tag: &mut TagToken) {
    for (name, _) in tag.attrs.iter_mut() {
        if *name == "definitionurl" {
            *name = "definitionURL".to_string();
        }
    }
}

/// Adjust SVG attributes for the token (§13.2.6.1).
///
/// For each attribute whose name is in the first column of the spec table,
/// change the name to the corresponding second-column value.
pub fn adjust_svg_attributes(tag: &mut TagToken) {
    for (name, _) in tag.attrs.iter_mut() {
        let new_name = match name.as_str() {
            "attributename" => "attributeName",
            "attributetype" => "attributeType",
            "basefrequency" => "baseFrequency",
            "baseprofile" => "baseProfile",
            "calcmode" => "calcMode",
            "clippathunits" => "clipPathUnits",
            "diffuseconstant" => "diffuseConstant",
            "edgemode" => "edgeMode",
            "filterunits" => "filterUnits",
            "glyphref" => "glyphRef",
            "gradienttransform" => "gradientTransform",
            "gradientunits" => "gradientUnits",
            "kernelmatrix" => "kernelMatrix",
            "kernelunitlength" => "kernelUnitLength",
            "keypoints" => "keyPoints",
            "keysplines" => "keySplines",
            "keytimes" => "keyTimes",
            "lengthadjust" => "lengthAdjust",
            "limitingconeangle" => "limitingConeAngle",
            "markerheight" => "markerHeight",
            "markerunits" => "markerUnits",
            "markerwidth" => "markerWidth",
            "maskcontentunits" => "maskContentUnits",
            "maskunits" => "maskUnits",
            "numoctaves" => "numOctaves",
            "pathlength" => "pathLength",
            "patterncontentunits" => "patternContentUnits",
            "patterntransform" => "patternTransform",
            "patternunits" => "patternUnits",
            "pointsatx" => "pointsAtX",
            "pointsaty" => "pointsAtY",
            "pointsatz" => "pointsAtZ",
            "preservealpha" => "preserveAlpha",
            "preserveaspectratio" => "preserveAspectRatio",
            "primitiveunits" => "primitiveUnits",
            "refx" => "refX",
            "refy" => "refY",
            "repeatcount" => "repeatCount",
            "repeatdur" => "repeatDur",
            "requiredextensions" => "requiredExtensions",
            "requiredfeatures" => "requiredFeatures",
            "specularconstant" => "specularConstant",
            "specularexponent" => "specularExponent",
            "spreadmethod" => "spreadMethod",
            "startoffset" => "startOffset",
            "stddeviation" => "stdDeviation",
            "stitchtiles" => "stitchTiles",
            "surfacescale" => "surfaceScale",
            "systemlanguage" => "systemLanguage",
            "tablevalues" => "tableValues",
            "targetx" => "targetX",
            "targety" => "targetY",
            "textlength" => "textLength",
            "viewbox" => "viewBox",
            "viewtarget" => "viewTarget",
            "xchannelselector" => "xChannelSelector",
            "ychannelselector" => "yChannelSelector",
            "zoomandpan" => "zoomAndPan",
            _ => continue,
        };
        *name = new_name.to_string();
    }
}

/// Adjust foreign attributes for the token (§13.2.6.1).
///
/// For each attribute whose name matches the first column of the spec table,
/// convert it into a namespaced attribute with the given prefix, local name,
/// and namespace URI.
pub fn adjust_foreign_attributes(tag: &mut TagToken) {
    // We rewrite attrs in place; the tokenizer stores attrs as Vec<(String,
    // String)>. The foreign-attribute adjustment changes an attribute into a
    // namespaced attribute, which we represent by storing the prefix and
    // namespace in a side channel. Since the Attribute struct in muskitty-dom
    // carries prefix/namespace_uri, we defer the actual namespacing to
    // `create_foreign_element`, which reads a marker stored alongside the
    // attribute name.
    //
    // To keep the change localized, we encode the adjusted name as
    // "\u{0}PREFIX\u{0}LOCALNAME\u{0}NS_URI" when the attribute is a foreign
    // attribute, and `create_foreign_element` decodes it. This keeps the
    // tokenizer's Vec<(String,String)> shape unchanged while carrying the
    // adjustment information.
    //
    // This is an internal encoding; it is never serialized to output because
    // `create_foreign_element` decodes it before building the Attribute.
    for (name, _value) in tag.attrs.iter_mut() {
        let (prefix, local_name, ns_uri) = match name.as_str() {
            "xlink:actuate" => ("xlink", "actuate", XLINK_NS),
            "xlink:arcrole" => ("xlink", "arcrole", XLINK_NS),
            "xlink:href" => ("xlink", "href", XLINK_NS),
            "xlink:role" => ("xlink", "role", XLINK_NS),
            "xlink:show" => ("xlink", "show", XLINK_NS),
            "xlink:title" => ("xlink", "title", XLINK_NS),
            "xlink:type" => ("xlink", "type", XLINK_NS),
            "xml:lang" => ("xml", "lang", XML_NS),
            "xml:space" => ("xml", "space", XML_NS),
            "xmlns" => ("", "xmlns", XMLNS_NS),
            "xmlns:xlink" => ("xmlns", "xlink", XMLNS_NS),
            _ => continue,
        };
        // Encode prefix + local_name + ns_uri using a NUL separator.
        // Empty prefix (for `xmlns`) is encoded as the empty string.
        *name = format!("\u{0}{prefix}\u{0}{local_name}\u{0}{ns_uri}");
    }
}

/// SVG element name corrections (§13.2.6.5).
///
/// When the adjusted current node is in the SVG namespace and the token's
/// tag name is one of the first-column entries, change the tag name to the
/// corresponding second-column entry.
pub fn adjust_svg_tag_name(name: &str) -> &str {
    match name {
        "altglyph" => "altGlyph",
        "altglyphdef" => "altGlyphDef",
        "altglyphitem" => "altGlyphItem",
        "animatecolor" => "animateColor",
        "animatemotion" => "animateMotion",
        "animatetransform" => "animateTransform",
        "clippath" => "clipPath",
        "feblend" => "feBlend",
        "fecolormatrix" => "feColorMatrix",
        "fecomponenttransfer" => "feComponentTransfer",
        "fecomposite" => "feComposite",
        "feconvolvematrix" => "feConvolveMatrix",
        "fediffuselighting" => "feDiffuseLighting",
        "fedisplacementmap" => "feDisplacementMap",
        "fedistantlight" => "feDistantLight",
        "fedropshadow" => "feDropShadow",
        "feflood" => "feFlood",
        "fefunca" => "feFuncA",
        "fefuncb" => "feFuncB",
        "fefuncg" => "feFuncG",
        "fefuncr" => "feFuncR",
        "fegaussianblur" => "feGaussianBlur",
        "feimage" => "feImage",
        "femerge" => "feMerge",
        "femergenode" => "feMergeNode",
        "femorphology" => "feMorphology",
        "feoffset" => "feOffset",
        "fepointlight" => "fePointLight",
        "fespecularlighting" => "feSpecularLighting",
        "fespotlight" => "feSpotLight",
        "fetile" => "feTile",
        "feturbulence" => "feTurbulence",
        "foreignobject" => "foreignObject",
        "glyphref" => "glyphRef",
        "lineargradient" => "linearGradient",
        "radialgradient" => "radialGradient",
        "textpath" => "textPath",
        _ => name,
    }
}

// ── Insert a foreign element (§13.2.6.1) ─────────────────────────────

/// Decode a possibly-adjusted attribute name back into a full `Attribute`.
///
/// `adjust_foreign_attributes` encodes foreign attributes as
/// `"\u{0}PREFIX\u{0}LOCALNAME\u{0}NS_URI"`. This function decodes that
/// encoding; plain attribute names pass through unchanged (becoming HTML
/// attributes with no prefix/namespace).
fn build_attribute(name: &str, value: &str) -> Attribute {
    if name.starts_with('\u{0}') {
        // Encoding: \0 prefix \0 local_name \0 ns_uri
        let rest = &name[3..]; // skip leading \0
        let mut parts = rest.splitn(3, '\u{0}');
        let prefix = parts.next().unwrap_or("");
        let local_name = parts.next().unwrap_or("");
        let ns_uri = parts.next().unwrap_or("");
        let prefix = if prefix.is_empty() {
            None
        } else {
            Some(prefix.to_string())
        };
        let namespace_uri = if ns_uri.is_empty() {
            None
        } else {
            Some(ns_uri.to_string())
        };
        Attribute {
            prefix,
            namespace_uri,
            local_name: local_name.to_string(),
            value: value.to_string(),
        }
    } else {
        Attribute::new(name, value)
    }
}

/// Insert a foreign element for the token (§13.2.6.1).
///
/// Creates an element in `namespace`, inserts it at the adjusted insertion
/// location, and pushes it onto the stack of open elements. The boolean
/// `only_add_to_element_stack` is used by the template insertion algorithm;
/// when true, the element is *not* attached to the DOM (only pushed on the
/// stack).
pub fn insert_foreign_element(
    parser: &mut HtmlTreeConstructor,
    token: &TagToken,
    namespace: Namespace,
    only_add_to_element_stack: bool,
) -> Rc<RefCell<Node>> {
    let attrs: Vec<Attribute> = token
        .attrs
        .iter()
        .map(|(name, value)| build_attribute(name, value))
        .collect();
    let element =
        Node::new_element_ns(token.name.clone(), namespace, None, attrs, &parser.document);
    if !only_add_to_element_stack {
        helpers::insert_node(parser, &element);
    }
    parser.open_elements.push(element.clone());
    element
}

// ── Rules for parsing tokens in foreign content (§13.2.6.5) ──────────

/// Process a token using the rules for parsing tokens in foreign content
/// (§13.2.6.5).
///
/// Returns `Step::Done` (token consumed) or `Step::Reprocess` (token should
/// be reprocessed in the current insertion mode in HTML content, after the
/// dispatcher re-evaluation).
pub fn process_in_foreign_content(
    parser: &mut HtmlTreeConstructor,
    token: &Token,
    tokenizer: &mut dyn Tokenizer,
) -> super::dispatch::Step {
    use super::dispatch::Step;
    match token {
        // A character token that is U+0000 NULL → parse error, insert U+FFFD.
        Token::Character('\0') => {
            parser
                .errors
                .push(ParseError::Generic("unexpected-null-character"));
            helpers::insert_character(parser, '\u{FFFD}');
            Step::Done
        }
        // A character token that is whitespace → insert the character.
        Token::Character(c) if matches!(c, '\t' | '\n' | '\u{000C}' | '\r' | ' ') => {
            helpers::insert_character(parser, *c);
            Step::Done
        }
        // Any other character token → insert the character, set frameset-ok
        // to "not ok".
        Token::Character(c) => {
            helpers::insert_character(parser, *c);
            parser.frameset_ok = false;
            Step::Done
        }
        // A comment token → insert a comment.
        Token::Comment(data) => {
            helpers::insert_comment(parser, data);
            Step::Done
        }
        // A processing instruction token → insert a processing instruction.
        Token::ProcessingInstruction { target, data } => {
            helpers::insert_processing_instruction(parser, target, data);
            Step::Done
        }
        // A DOCTYPE token → parse error, ignore.
        Token::Doctype(_) => {
            parser
                .errors
                .push(ParseError::Generic("unexpected-doctype-in-foreign-content"));
            Step::Done
        }
        Token::Tag(tag) => {
            if tag.kind == TagKind::Start {
                process_start_tag_in_foreign(parser, tag, tokenizer)
            } else {
                process_end_tag_in_foreign(parser, tag, tokenizer)
            }
        }
        Token::EOF => {
            // EOF is routed to HTML content by the dispatcher, so this is
            // unreachable. Treat as done to be safe.
            Step::Done
        }
    }
}

/// Process a start tag in foreign content (§13.2.6.5 "Any other start tag"
/// and the HTML-breakout list).
fn process_start_tag_in_foreign(
    parser: &mut HtmlTreeConstructor,
    tag: &TagToken,
    _tokenizer: &mut dyn Tokenizer,
) -> super::dispatch::Step {
    use super::dispatch::Step;
    let name = tag.name.as_str();

    // HTML breakout elements (§13.2.6.5): these tags, plus <font> with
    // color/face/size attributes, break out of foreign content back into
    // HTML. Also </br> and </p> end tags (handled in end-tag path).
    let is_html_breakout = matches!(
        name,
        "b" | "big"
            | "blockquote"
            | "body"
            | "br"
            | "center"
            | "code"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "em"
            | "embed"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "hr"
            | "i"
            | "img"
            | "li"
            | "listing"
            | "menu"
            | "meta"
            | "nobr"
            | "ol"
            | "p"
            | "pre"
            | "ruby"
            | "s"
            | "small"
            | "span"
            | "strong"
            | "strike"
            | "sub"
            | "sup"
            | "table"
            | "tt"
            | "u"
            | "ul"
            | "var"
    ) || (name == "font"
        && tag
            .attrs
            .iter()
            .any(|(n, _)| matches!(n.as_str(), "color" | "face" | "size")));

    if is_html_breakout {
        // Parse error. Pop elements until the current node is a MathML text
        // integration point, an HTML integration point, or an element in the
        // HTML namespace. Then reprocess the token in the current insertion
        // mode (HTML content).
        parser
            .errors
            .push(ParseError::Generic("html-element-in-foreign-content"));
        loop {
            let current = parser.current_node();
            let should_stop = {
                let n = current.borrow();
                match &n.kind {
                    NodeKind::Element(e) if e.namespace == Namespace::Html => true,
                    _ => {
                        is_mathml_text_integration_point(&current)
                            || is_html_integration_point(&current)
                    }
                }
            };
            if should_stop {
                break;
            }
            parser.open_elements.pop();
        }
        // Reprocess the token in the current insertion mode (HTML content).
        return Step::Reprocess;
    }

    // Any other start tag (§13.2.6.5):
    // 1. If the adjusted current node is in the MathML namespace, adjust
    //    MathML attributes for the token.
    // 2. If the adjusted current node is in the SVG namespace and the
    //    token's tag name is in the SVG element-name table, change the tag
    //    name to the corrected form.
    // 3. If the adjusted current node is in the SVG namespace, adjust SVG
    //    attributes for the token.
    // 4. Adjust foreign attributes for the token.
    // 5. Insert a foreign element for the token, with the adjusted current
    //    node's namespace and false.
    // 6. If self-closing: script-in-SVG special case, else pop + ack.
    let current = parser.current_node();
    let current_ns = {
        let n = current.borrow();
        match &n.kind {
            NodeKind::Element(e) => e.namespace,
            _ => Namespace::Html,
        }
    };

    let mut adjusted_tag = tag.clone();
    if current_ns == Namespace::MathMl {
        adjust_mathml_attributes(&mut adjusted_tag);
    }
    if current_ns == Namespace::Svg {
        // Adjust the tag name (§13.2.6.5 element-name table).
        let new_name = adjust_svg_tag_name(&adjusted_tag.name).to_string();
        adjusted_tag.name = new_name;
        adjust_svg_attributes(&mut adjusted_tag);
    }
    adjust_foreign_attributes(&mut adjusted_tag);

    // Insert a foreign element with the adjusted current node's namespace.
    let new_element = insert_foreign_element(parser, &adjusted_tag, current_ns, false);

    if tag.self_closing {
        // §13.2.6.5: If the token has its self-closing flag set:
        // - If the token's tag name is "script" and the new current node is
        //   an SVG script element → ack the flag and run the script end-tag
        //   steps. (We don't execute scripts; just pop.)
        // - Otherwise → pop the current node and acknowledge the flag.
        let is_svg_script = {
            let n = new_element.borrow();
            matches!(&n.kind, NodeKind::Element(e) if e.namespace == Namespace::Svg && e.local_name == "script")
        };
        // Acknowledge the self-closing flag (tokenizer state); for our
        // tokenizer this is a no-op since the flag is per-token.
        if is_svg_script {
            // Act as described in the steps for a "script" end tag below:
            // pop the current node. (Script execution is not supported.)
            parser.open_elements.pop();
        } else {
            parser.open_elements.pop();
        }
    }
    Step::Done
}

/// Process an end tag in foreign content (§13.2.6.5).
fn process_end_tag_in_foreign(
    parser: &mut HtmlTreeConstructor,
    tag: &TagToken,
    tokenizer: &mut dyn Tokenizer,
) -> super::dispatch::Step {
    use super::dispatch::Step;
    let name = tag.name.as_str();

    // </br> or </p> → HTML breakout (§13.2.6.5 lists these with the start
    // tags above).
    if name == "br" || name == "p" {
        parser
            .errors
            .push(ParseError::Generic("html-element-in-foreign-content"));
        loop {
            let current = parser.current_node();
            let should_stop = {
                let n = current.borrow();
                match &n.kind {
                    NodeKind::Element(e) if e.namespace == Namespace::Html => true,
                    _ => {
                        is_mathml_text_integration_point(&current)
                            || is_html_integration_point(&current)
                    }
                }
            };
            if should_stop {
                break;
            }
            parser.open_elements.pop();
        }
        return Step::Reprocess;
    }

    // </script> if the current node is an SVG script element → pop.
    if name == "script" {
        let current = parser.current_node();
        let is_svg_script = {
            let n = current.borrow();
            matches!(&n.kind, NodeKind::Element(e) if e.namespace == Namespace::Svg && e.local_name == "script")
        };
        if is_svg_script {
            parser.open_elements.pop();
            // (Script execution steps are not supported; skip the
            // insertion-point / nesting-level bookkeeping.)
            let _ = tokenizer; // tokenizer would be touched here for script exec
            return Step::Done;
        }
    }

    // Any other end tag (§13.2.6.5):
    // 1. Initialize node to be the current node.
    // 2. If node's tag name (lowercased) is not the same as the token's,
    //    parse error.
    // 3. Loop: if node is the topmost element, return (fragment case).
    // 4. If node's tag name (lowercased) matches the token's, pop until
    //    node has been popped, return.
    // 5. Set node to the previous entry.
    // 6. If node is not in the HTML namespace, return to the loop.
    // 7. Otherwise, process the token in the current insertion mode (HTML
    //    content).
    let mut idx = parser.open_elements.len();
    if idx == 0 {
        return Step::Done;
    }
    idx -= 1;

    // Step 2: compare lowercased tag names.
    {
        let node = &parser.open_elements[idx];
        let n = node.borrow();
        if let NodeKind::Element(e) = &n.kind {
            // Foreign element tag names are case-sensitive; the spec says to
            // compare "converted to ASCII lowercase" for both. For SVG
            // elements like `foreignObject`, the local name keeps its case,
            // but the end tag in the markup is lowercased by the tokenizer.
            // The spec's ASCII-lowercase comparison means `</foreignobject>`
            // matches `foreignObject` because both lowercased to
            // `foreignobject`.
            let node_lower = e.local_name.to_ascii_lowercase();
            let token_lower = name.to_ascii_lowercase();
            if node_lower != token_lower {
                parser.errors.push(ParseError::Generic(
                    "end-tag-name-mismatch-in-foreign-content",
                ));
            }
        }
    }

    loop {
        // Step 3: if node is the topmost element, return (fragment case).
        if idx == 0 {
            return Step::Done;
        }
        // Step 4: if node's tag name (lowercased) matches, pop until node
        // has been popped.
        let node_lower = {
            let node = &parser.open_elements[idx];
            let n = node.borrow();
            match &n.kind {
                NodeKind::Element(e) => Some(e.local_name.to_ascii_lowercase()),
                _ => None,
            }
        };
        if node_lower.as_deref() == Some(&name.to_ascii_lowercase()) {
            // Pop until idx has been popped.
            while parser.open_elements.len() > idx {
                parser.open_elements.pop();
            }
            return Step::Done;
        }
        // Step 5: set node to the previous entry.
        idx -= 1;
        // Step 6: if node is not in the HTML namespace, loop.
        let is_html = {
            let node = &parser.open_elements[idx];
            let n = node.borrow();
            matches!(&n.kind, NodeKind::Element(e) if e.namespace == Namespace::Html)
        };
        if !is_html {
            continue;
        }
        // Step 7: process the token using the rules for the current insertion
        // mode (HTML content). Per §13.2.6.5, this must NOT re-enter the tree
        // construction dispatcher — the foreign element is still on top of
        // the stack, so the dispatcher would route the token back here,
        // causing an infinite loop. Call the current-mode handler directly.
        let token = Token::Tag(tag.clone());
        return super::dispatch::dispatch_in_current_mode(parser, &token, tokenizer);
    }
}
