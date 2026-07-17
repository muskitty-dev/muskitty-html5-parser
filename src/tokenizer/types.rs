//! Token and state types for the HTML tokenizer.
//!
//! Defined per WHATWG HTML Spec §13.2.5 Tokenization.

/// A token emitted by the tokenizer.
///
/// WHATWG §13.2.5 defines token kinds: DOCTYPE, start tag, end tag,
/// comment, character, end-of-file, and processing instruction.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// A DOCTYPE token (§13.2.5.53–§13.2.5.68).
    Doctype(DoctypeToken),
    /// A start or end tag token, discriminated by [`TagToken::kind`].
    Tag(TagToken),
    /// A comment token. The String is the comment content.
    Comment(String),
    /// A character token carrying a single Unicode code point.
    Character(char),
    /// End-of-file token. Emitted when the input stream is exhausted.
    EOF,
    /// A processing instruction token (§13.2.5.72–§13.2.5.76).
    /// Carries the PI target and data.
    ProcessingInstruction { target: String, data: String },
}

/// Distinguishes start tags from end tags.
///
/// WHATWG §13.2.5: start tags and end tags share the same token structure but
/// differ in how tree construction handles them — end tag attributes are
/// ignored (parse error `end-tag-with-attributes`), and the self-closing flag
/// on an end tag is meaningless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    /// `<tag>`
    Start,
    /// `</tag>`
    End,
}

/// A tag token (used for both start and end tags, distinguished by [`TagKind`]).
///
/// WHATWG §13.2.5: start tags have a tag name, a self-closing flag (set when
/// the tag ends with `/>`), and a list of attributes. End tags have the same
/// structure, though attributes on end tags are a parse error and the
/// self-closing flag is ignored in tree construction.
#[derive(Debug, Clone, PartialEq)]
pub struct TagToken {
    /// Whether this is a start tag or end tag.
    pub kind: TagKind,
    /// The tag name (lowercased by the tokenizer).
    pub name: String,
    /// Attribute name-value pairs, in source order.
    pub attrs: Vec<(String, String)>,
    /// Whether the tag ends with `/>` (the self-closing solidus).
    /// Only meaningful for start tags on void elements (§13.2.6).
    pub self_closing: bool,
}

/// A DOCTYPE token.
///
/// WHATWG §13.2.5.53–§13.2.5.68.
#[derive(Debug, Clone, PartialEq)]
pub struct DoctypeToken {
    /// The DOCTYPE name (e.g. "html"), or None if absent.
    pub name: Option<String>,
    /// The public identifier, or None if absent.
    pub public_id: Option<String>,
    /// The system identifier, or None if absent.
    pub system_id: Option<String>,
    /// Whether force-quirks was set during DOCTYPE parsing.
    pub force_quirks: bool,
}

/// Tokenizer states.
///
/// WHATWG §13.2.5 defines 80 states. Every state is present in this enum so
/// the compiler enforces exhaustive `match` arms — no state can be forgotten.
///
/// States marked `TODO: not yet implemented` are reserved; their variant
/// exists but the tokenizer will panic if it transitions into one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    // ── Content model states ──────────────────────────────
    /// §13.2.5.1 Data state
    Data,
    /// §13.2.5.2 RCDATA state
    RCDATA,
    /// §13.2.5.3 RAWTEXT state
    RAWTEXT,
    /// §13.2.5.4 Script data state
    ScriptData,
    /// §13.2.5.5 PLAINTEXT state
    PLAINTEXT,

    // ── Tag open / close states ───────────────────────────
    /// §13.2.5.6 Tag open state
    TagOpen,
    /// §13.2.5.7 End tag open state
    EndTagOpen,
    /// §13.2.5.8 Tag name state
    TagName,

    // ── RCDATA states ─────────────────────────────────────
    /// §13.2.5.9 RCDATA less-than sign state
    RCDATALessThanSign,
    /// §13.2.5.10 RCDATA end tag open state
    RCDATAEndTagOpen,
    /// §13.2.5.11 RCDATA end tag name state
    RCDATAEndTagName,

    // ── RAWTEXT states ────────────────────────────────────
    /// §13.2.5.12 RAWTEXT less-than sign state
    RAWTEXTLessThanSign,
    /// §13.2.5.13 RAWTEXT end tag open state
    RAWTEXTEndTagOpen,
    /// §13.2.5.14 RAWTEXT end tag name state
    RAWTEXTEndTagName,

    // ── Script data states ────────────────────────────────
    /// §13.2.5.15 Script data less-than sign state
    ScriptDataLessThanSign,
    /// §13.2.5.16 Script data end tag open state
    ScriptDataEndTagOpen,
    /// §13.2.5.17 Script data end tag name state
    ScriptDataEndTagName,
    /// §13.2.5.18 Script data escape start state
    ScriptDataEscapeStart,
    /// §13.2.5.19 Script data escape start dash state
    ScriptDataEscapeStartDash,
    /// §13.2.5.20 Script data escaped state
    ScriptDataEscaped,
    /// §13.2.5.21 Script data escaped dash state
    ScriptDataEscapedDash,
    /// §13.2.5.22 Script data escaped dash dash state
    ScriptDataEscapedDashDash,
    /// §13.2.5.23 Script data escaped less-than sign state
    ScriptDataEscapedLessThanSign,
    /// §13.2.5.24 Script data escaped end tag open state
    ScriptDataEscapedEndTagOpen,
    /// §13.2.5.25 Script data escaped end tag name state
    ScriptDataEscapedEndTagName,
    /// §13.2.5.26 Script data double escape start state
    ScriptDataDoubleEscapeStart,
    /// §13.2.5.27 Script data double escaped state
    ScriptDataDoubleEscaped,
    /// §13.2.5.28 Script data double escaped dash state
    ScriptDataDoubleEscapedDash,
    /// §13.2.5.29 Script data double escaped dash dash state
    ScriptDataDoubleEscapedDashDash,
    /// §13.2.5.30 Script data double escaped less-than sign state
    ScriptDataDoubleEscapedLessThanSign,
    /// §13.2.5.31 Script data double escape end state
    ScriptDataDoubleEscapeEnd,

    // ── Attribute states ──────────────────────────────────
    /// §13.2.5.32 Before attribute name state
    BeforeAttributeName,
    /// §13.2.5.33 Attribute name state
    AttributeName,
    /// §13.2.5.34 After attribute name state
    AfterAttributeName,
    /// §13.2.5.35 Before attribute value state
    BeforeAttributeValue,
    /// §13.2.5.36 Attribute value (double-quoted) state
    AttributeValueDoubleQuoted,
    /// §13.2.5.37 Attribute value (single-quoted) state
    AttributeValueSingleQuoted,
    /// §13.2.5.38 Attribute value (unquoted) state
    AttributeValueUnquoted,
    /// §13.2.5.39 After attribute value (quoted) state
    AfterAttributeValueQuoted,
    /// §13.2.5.40 Self-closing start tag state
    SelfClosingStartTag,

    // ── Comment states ────────────────────────────────────
    /// §13.2.5.41 Bogus comment state
    BogusComment,
    /// §13.2.5.42 Markup declaration open state
    MarkupDeclarationOpen,
    /// §13.2.5.43 Comment start state
    CommentStart,
    /// §13.2.5.44 Comment start dash state
    CommentStartDash,
    /// §13.2.5.45 Comment state
    Comment,
    /// §13.2.5.46 Comment less-than sign state
    CommentLessThanSign,
    /// §13.2.5.47 Comment less-than sign bang state
    CommentLessThanSignBang,
    /// §13.2.5.48 Comment less-than sign bang dash state
    CommentLessThanSignBangDash,
    /// §13.2.5.49 Comment less-than sign bang dash dash state
    CommentLessThanSignBangDashDash,
    /// §13.2.5.50 Comment end dash state
    CommentEndDash,
    /// §13.2.5.51 Comment end state
    CommentEnd,
    /// §13.2.5.52 Comment end bang state
    CommentEndBang,

    // ── DOCTYPE states ────────────────────────────────────
    /// §13.2.5.53 DOCTYPE state
    Doctype,
    /// §13.2.5.54 Before DOCTYPE name state
    BeforeDoctypeName,
    /// §13.2.5.55 DOCTYPE name state
    DoctypeName,
    /// §13.2.5.56 After DOCTYPE name state
    AfterDoctypeName,
    /// §13.2.5.57 After DOCTYPE public keyword state
    AfterDoctypePublicKeyword,
    /// §13.2.5.58 Before DOCTYPE public identifier state
    BeforeDoctypePublicId,
    /// §13.2.5.59 DOCTYPE public identifier (double-quoted) state
    DoctypePublicIdDoubleQuoted,
    /// §13.2.5.60 DOCTYPE public identifier (single-quoted) state
    DoctypePublicIdSingleQuoted,
    /// §13.2.5.61 After DOCTYPE public identifier state
    AfterDoctypePublicId,
    /// §13.2.5.62 Between DOCTYPE public and system identifiers state
    BetweenDoctypePublicAndSystemIds,
    /// §13.2.5.63 After DOCTYPE system keyword state
    AfterDoctypeSystemKeyword,
    /// §13.2.5.64 Before DOCTYPE system identifier state
    BeforeDoctypeSystemId,
    /// §13.2.5.65 DOCTYPE system identifier (double-quoted) state
    DoctypeSystemIdDoubleQuoted,
    /// §13.2.5.66 DOCTYPE system identifier (single-quoted) state
    DoctypeSystemIdSingleQuoted,
    /// §13.2.5.67 After DOCTYPE system identifier state
    AfterDoctypeSystemId,
    /// §13.2.5.68 Bogus DOCTYPE state
    BogusDoctype,

    // ── CDATA section states ──────────────────────────────
    /// §13.2.5.69 CDATA section state
    CDATASection,
    /// §13.2.5.70 CDATA section bracket state
    CDATASectionBracket,
    /// §13.2.5.71 CDATA section end state
    CDATASectionEnd,

    // ── Processing instruction states (§13.2.5.72–§13.2.5.76) ──
    /// §13.2.5.72 Processing instruction open state
    ProcessingInstructionOpen,
    /// §13.2.5.73 Processing instruction target state
    ProcessingInstructionTarget,
    /// §13.2.5.74 After processing instruction target state
    AfterProcessingInstructionTarget,
    /// §13.2.5.75 Processing instruction data state
    ProcessingInstructionData,
    /// §13.2.5.76 Processing instruction questionable state
    ProcessingInstructionQuestionable,

    // ── Character reference states ────────────────────────
    /// §13.2.5.77 Character reference state
    CharacterReference,
    /// §13.2.5.78 Named character reference state
    NamedCharacterReference,
    /// §13.2.5.79 Ambiguous ampersand state
    AmbiguousAmpersand,
    /// §13.2.5.80 Numeric character reference state
    NumericCharacterReference,
    /// §13.2.5.81 Hexadecimal character reference start state
    HexCharacterReferenceStart,
    /// §13.2.5.82 Decimal character reference start state
    DecimalCharacterReferenceStart,
    /// §13.2.5.83 Hexadecimal character reference state
    HexCharacterReference,
    /// §13.2.5.84 Decimal character reference state
    DecimalCharacterReference,
    /// §13.2.5.85 Numeric character reference end state
    NumericCharacterReferenceEnd,
}
