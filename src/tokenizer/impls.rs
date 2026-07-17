//! Concrete [`HtmlTokenizer`] implementation of the [`Tokenizer`] trait.
//!
//! The tokenizer processes one code point per `next_token()` call,
//! following the state machine defined in WHATWG §13.2.5.

use super::trait_def::Tokenizer;
use super::types::{DoctypeToken, State, TagKind, TagToken, Token};

/// A concrete HTML tokenizer.
///
/// Consumes a sequence of Unicode code points and emits [`Token`]s
/// according to the WHATWG tokenization state machine (§13.2.5).
///
/// # Usage
///
/// ```ignore
/// let mut t = HtmlTokenizer::new("<p>hello</p>");
/// while let Some(token) = t.next_token() {
///     // process token
/// }
/// ```
///
/// After `Token::EOF` is emitted, subsequent calls return `None`.
pub struct HtmlTokenizer {
    /// Input code points.
    input: Vec<char>,
    /// Current position in `input`.
    pos: usize,
    /// Current tokenizer state (§13.2.5).
    state: State,
    /// Whether `Token::EOF` has been emitted.
    eof_emitted: bool,
    /// When true, the next call to `next_char()` returns the current character
    /// without advancing `pos`. Used by states that "reconsume" the character
    /// in a different state (§13.2.5 convention).
    reconsume: bool,
    /// When true, the most recent `next_char()` consumed the end of the input
    /// (returned `None`). Combined with [`reconsume`](Self::reconsume), this
    /// makes a "reconsume in <state>" transition from an EOF arm replay the
    /// EOF (return `None`) rather than re-emitting the last real code point.
    ///
    /// Without this, any EOF arm that sets `reconsume = true` would loop
    /// forever: `next_char()` replays `input[pos-1]` (the final char), so the
    /// return state re-processes that char and may route right back, emitting
    /// a token each iteration without ever advancing — the input never ends.
    /// This was the root cause of the html5lib harness OOM (20 GB) on
    /// `&`-at-EOF inputs.
    last_was_eof: bool,
    /// The tag token currently being built, if any.
    /// Set when entering TagName state, emitted when tag is complete.
    current_tag: Option<TagToken>,
    /// The comment data currently being accumulated.
    current_comment: String,
    /// The processing instruction token currently being built
    /// (§13.2.5.72–§13.2.5.76). Set when a valid PI target is identified
    /// in the ProcessingInstructionTarget state.
    current_pi: Option<(String, String)>,
    /// The attribute name currently being accumulated (AttributeName state).
    current_attr_name: String,
    /// The attribute value currently being accumulated (attribute value states).
    current_attr_value: String,
    /// The DOCTYPE token currently being built (§13.2.5.53–§13.2.5.68).
    /// Reset when entering DOCTYPE states via MarkupDeclarationOpen.
    current_doctype: DoctypeToken,
    /// The tag name that can close the current RCDATA/RAWTEXT section.
    /// Set by tree construction before switching to RCDATA/RAWTEXT state.
    /// e.g. `Some("title")` when entering `<title>`, `Some("textarea")`
    /// when entering `<textarea>`.
    appropriate_end_tag_name: Option<String>,
    /// Temporary buffer for RCDATA/RAWTEXT end tag name accumulation.
    /// Stores original-case characters of the candidate end tag name.
    /// On match failure, these chars are emitted back as character tokens.
    temporary_buffer: String,
    /// Queue of tokens to emit before consuming the next input character.
    /// Used when a single state transition needs to emit multiple tokens
    /// (e.g. "anything else" in RCDATA/RAWTEXT end tag name).
    pending_tokens: Vec<Token>,
    /// The return state for character references (§13.2.5.72–§13.2.5.80).
    /// Set before entering CharacterReference state so the tokenizer knows
    /// where to return after resolving the reference.
    return_state: Option<State>,
    /// Accumulator for numeric character reference value
    /// (§13.2.5.78–§13.2.5.79).
    character_reference_code: u32,
    /// The `x` or `X` character consumed when entering a hexadecimal
    /// character reference (§13.2.5.75). Preserved so that "flush code
    /// points consumed as a character reference" (§13.2.5.81 anything-else)
    /// can replay the original case rather than always emitting lowercase.
    char_ref_hex_prefix: char,
    /// Whether the adjusted current node is in foreign content (not in the
    /// HTML namespace). Set by tree construction before requesting the next
    /// token so that the markup declaration open state (§13.2.5.42) can
    /// decide between CDATA section state (foreign) and bogus comment
    /// state (HTML) when encountering `<![CDATA[`.
    in_foreign_content: bool,
}

impl HtmlTokenizer {
    /// Create a new tokenizer from a string input.
    ///
    /// The tokenizer starts in [`State::Data`] (§13.2.5.1).
    pub fn new(input: &str) -> Self {
        Self {
            input: input.chars().collect(),
            pos: 0,
            state: State::Data,
            eof_emitted: false,
            reconsume: false,
            last_was_eof: false,
            current_tag: None,
            current_comment: String::new(),
            current_pi: None,
            current_attr_name: String::new(),
            current_attr_value: String::new(),
            current_doctype: DoctypeToken {
                name: None,
                public_id: None,
                system_id: None,
                force_quirks: false,
            },
            appropriate_end_tag_name: None,
            temporary_buffer: String::new(),
            pending_tokens: Vec::new(),
            return_state: None,
            character_reference_code: 0,
            char_ref_hex_prefix: 'x',
            in_foreign_content: false,
        }
    }

    /// Peek at the current input character without consuming it.
    fn current_char(&self) -> Option<char> {
        if self.pos < self.input.len() {
            Some(self.input[self.pos])
        } else {
            None
        }
    }

    /// Consume and return the current input character, advancing `pos`.
    fn consume(&mut self) -> Option<char> {
        let c = self.current_char();
        if c.is_some() {
            self.pos += 1;
            self.last_was_eof = false;
        } else {
            self.last_was_eof = true;
        }
        c
    }

    /// Return the next input character, respecting the reconsume flag.
    ///
    /// If [`reconsume`](Self::reconsume) is true, returns the previously
    /// consumed character (at `pos - 1`) without advancing. Otherwise,
    /// consumes and advances as usual.
    ///
    /// This implements the "reconsume the current input character" convention
    /// used throughout §13.2.5.
    ///
    /// # EOF reconsume
    ///
    /// When an EOF arm sets `reconsume = true`, the spec's "reconsume the
    /// current input character" means the return state must see the EOF
    /// again — not the last real code point. [`last_was_eof`](Self::last_was_eof)
    /// records whether the most recent consume hit EOF so this can replay
    /// `None` instead of `input[pos-1]`. Without this, every EOF reconsume
    /// would loop forever (see the field doc).
    fn next_char(&mut self) -> Option<char> {
        if self.reconsume {
            self.reconsume = false;
            if self.last_was_eof {
                // Reconsuming EOF: there is no code point to replay.
                return None;
            }
            // The character to reconsume was already consumed — it's at pos-1.
            if self.pos > 0 && self.pos <= self.input.len() {
                Some(self.input[self.pos - 1])
            } else {
                None
            }
        } else {
            self.consume()
        }
    }
}

impl HtmlTokenizer {
    /// Advance the state machine by exactly one step and return whatever
    /// the active state handler produces (a token, or `None` when this
    /// step only switched state without emitting).
    ///
    /// This is the single-step primitive. The public [`Tokenizer::next_token`]
    /// loops over this until a real token drops out, so callers never observe
    /// the intermediate `None`s. Kept as a separate method so unit tests can
    /// still assert on individual state transitions when that granularity is
    /// useful.
    fn step(&mut self) -> Option<Token> {
        // After EOF has been emitted, the stream is exhausted.
        if self.eof_emitted {
            return None;
        }

        // Drain pending tokens (multi-token emission queue) first.
        if let Some(token) = self.pending_tokens.pop() {
            return Some(token);
        }

        match self.state {
            State::Data => self.handle_data_state(),
            State::TagOpen => self.handle_tag_open_state(),
            State::EndTagOpen => self.handle_end_tag_open_state(),
            State::TagName => self.handle_tag_name_state(),
            State::SelfClosingStartTag => self.handle_self_closing_start_tag_state(),
            State::MarkupDeclarationOpen => self.handle_markup_declaration_open_state(),
            State::Doctype => self.handle_doctype_state(),
            State::BeforeDoctypeName => self.handle_before_doctype_name_state(),
            State::DoctypeName => self.handle_doctype_name_state(),
            State::AfterDoctypeName => self.handle_after_doctype_name_state(),
            State::AfterDoctypePublicKeyword => self.handle_after_doctype_public_keyword_state(),
            State::BeforeDoctypePublicId => self.handle_before_doctype_public_id_state(),
            State::DoctypePublicIdDoubleQuoted => {
                self.handle_doctype_public_id_double_quoted_state()
            }
            State::DoctypePublicIdSingleQuoted => {
                self.handle_doctype_public_id_single_quoted_state()
            }
            State::AfterDoctypePublicId => self.handle_after_doctype_public_id_state(),
            State::BetweenDoctypePublicAndSystemIds => {
                self.handle_between_doctype_public_and_system_ids_state()
            }
            State::AfterDoctypeSystemKeyword => self.handle_after_doctype_system_keyword_state(),
            State::BeforeDoctypeSystemId => self.handle_before_doctype_system_id_state(),
            State::DoctypeSystemIdDoubleQuoted => {
                self.handle_doctype_system_id_double_quoted_state()
            }
            State::DoctypeSystemIdSingleQuoted => {
                self.handle_doctype_system_id_single_quoted_state()
            }
            State::AfterDoctypeSystemId => self.handle_after_doctype_system_id_state(),
            State::BogusDoctype => self.handle_bogus_doctype_state(),
            State::BogusComment => self.handle_bogus_comment_state(),
            State::CommentStart => self.handle_comment_start_state(),
            State::CommentStartDash => self.handle_comment_start_dash_state(),
            State::Comment => self.handle_comment_state(),
            State::CommentLessThanSign => self.handle_comment_less_than_sign_state(),
            State::CommentLessThanSignBang => self.handle_comment_less_than_sign_bang_state(),
            State::CommentLessThanSignBangDash => {
                self.handle_comment_less_than_sign_bang_dash_state()
            }
            State::CommentLessThanSignBangDashDash => {
                self.handle_comment_less_than_sign_bang_dash_dash_state()
            }
            State::CommentEndDash => self.handle_comment_end_dash_state(),
            State::CommentEnd => self.handle_comment_end_state(),
            State::CommentEndBang => self.handle_comment_end_bang_state(),
            State::BeforeAttributeName => self.handle_before_attribute_name_state(),
            State::AttributeName => self.handle_attribute_name_state(),
            State::AfterAttributeName => self.handle_after_attribute_name_state(),
            State::BeforeAttributeValue => self.handle_before_attribute_value_state(),
            State::AttributeValueDoubleQuoted => self.handle_attribute_value_double_quoted_state(),
            State::AttributeValueSingleQuoted => self.handle_attribute_value_single_quoted_state(),
            State::AttributeValueUnquoted => self.handle_attribute_value_unquoted_state(),
            State::AfterAttributeValueQuoted => self.handle_after_attribute_value_quoted_state(),
            State::RCDATA => self.handle_rcdata_state(),
            State::RAWTEXT => self.handle_rawtext_state(),
            State::PLAINTEXT => self.handle_plaintext_state(),
            State::RCDATALessThanSign => self.handle_rcdata_less_than_sign_state(),
            State::RCDATAEndTagOpen => self.handle_rcdata_end_tag_open_state(),
            State::RCDATAEndTagName => self.handle_rcdata_end_tag_name_state(),
            State::RAWTEXTLessThanSign => self.handle_rawtext_less_than_sign_state(),
            State::RAWTEXTEndTagOpen => self.handle_rawtext_end_tag_open_state(),
            State::RAWTEXTEndTagName => self.handle_rawtext_end_tag_name_state(),
            State::ScriptData => self.handle_script_data_state(),
            State::ScriptDataLessThanSign => self.handle_script_data_less_than_sign_state(),
            State::ScriptDataEndTagOpen => self.handle_script_data_end_tag_open_state(),
            State::ScriptDataEndTagName => self.handle_script_data_end_tag_name_state(),
            State::ScriptDataEscapeStart => self.handle_script_data_escape_start_state(),
            State::ScriptDataEscapeStartDash => self.handle_script_data_escape_start_dash_state(),
            State::ScriptDataEscaped => self.handle_script_data_escaped_state(),
            State::ScriptDataEscapedDash => self.handle_script_data_escaped_dash_state(),
            State::ScriptDataEscapedDashDash => self.handle_script_data_escaped_dash_dash_state(),
            State::ScriptDataEscapedLessThanSign => {
                self.handle_script_data_escaped_less_than_sign_state()
            }
            State::ScriptDataEscapedEndTagOpen => {
                self.handle_script_data_escaped_end_tag_open_state()
            }
            State::ScriptDataEscapedEndTagName => {
                self.handle_script_data_escaped_end_tag_name_state()
            }
            State::ScriptDataDoubleEscapeStart => {
                self.handle_script_data_double_escape_start_state()
            }
            State::ScriptDataDoubleEscaped => self.handle_script_data_double_escaped_state(),
            State::ScriptDataDoubleEscapedDash => {
                self.handle_script_data_double_escaped_dash_state()
            }
            State::ScriptDataDoubleEscapedDashDash => {
                self.handle_script_data_double_escaped_dash_dash_state()
            }
            State::ScriptDataDoubleEscapedLessThanSign => {
                self.handle_script_data_double_escaped_less_than_sign_state()
            }
            State::ScriptDataDoubleEscapeEnd => self.handle_script_data_double_escape_end_state(),
            State::CharacterReference => self.handle_character_reference_state(),
            State::NumericCharacterReference => self.handle_numeric_character_reference_state(),
            State::HexCharacterReferenceStart => self.handle_hex_character_reference_start_state(),
            State::DecimalCharacterReferenceStart => {
                self.handle_decimal_character_reference_start_state()
            }
            State::HexCharacterReference => self.handle_hex_character_reference_state(),
            State::DecimalCharacterReference => self.handle_decimal_character_reference_state(),
            State::NumericCharacterReferenceEnd => {
                self.handle_numeric_character_reference_end_state()
            }
            State::NamedCharacterReference => self.handle_named_character_reference_state(),
            State::AmbiguousAmpersand => self.handle_ambiguous_ampersand_state(),
            State::CDATASection => self.handle_cdata_section_state(),
            State::CDATASectionBracket => self.handle_cdata_section_bracket_state(),
            State::CDATASectionEnd => self.handle_cdata_section_end_state(),
            State::ProcessingInstructionOpen => self.handle_processing_instruction_open_state(),
            State::ProcessingInstructionTarget => self.handle_processing_instruction_target_state(),
            State::AfterProcessingInstructionTarget => {
                self.handle_after_processing_instruction_target_state()
            }
            State::ProcessingInstructionData => self.handle_processing_instruction_data_state(),
            State::ProcessingInstructionQuestionable => {
                self.handle_processing_instruction_questionable_state()
            }
        }
    }
}

impl Tokenizer for HtmlTokenizer {
    fn next_token(&mut self) -> Option<Token> {
        // Loop over single steps until a real token is produced. State
        // handlers that only switch state return `None`; those intermediate
        // steps are invisible to the caller. The loop terminates because
        // every state path makes progress — it either consumes a code point,
        // sets a reconsume that flips into a consuming state, emits a token,
        // or reaches EOF (which sets eof_emitted and returns EOF, after
        // which the step guard returns None and we break out). A safety
        // bound guards against any latent state-machine bug hanging forever.
        let mut bound = 1_000_000;
        while bound > 0 {
            bound -= 1;
            match self.step() {
                Some(token) => return Some(token),
                None => {
                    // No token this step. If the stream is exhausted, stop;
                    // otherwise the step changed state and we keep going.
                    if self.eof_emitted {
                        return None;
                    }
                }
            }
        }
        // Unreachable for a conforming input — every transition terminates.
        // Reached only if a state-machine bug causes a non-productive cycle.
        // Surface it loudly rather than spinning silently.
        panic!(
            "tokenizer state machine made no progress; probable reconsume/infinite-transition bug"
        );
    }

    fn set_state(&mut self, state: State) {
        self.state = state;
    }

    fn state(&self) -> State {
        self.state
    }

    fn reset(&mut self) {
        self.pos = 0;
        self.state = State::Data;
        self.eof_emitted = false;
        self.reconsume = false;
        self.last_was_eof = false;
        self.current_tag = None;
        self.current_comment.clear();
        self.current_pi = None;
        self.current_attr_name.clear();
        self.current_attr_value.clear();
        self.current_doctype = DoctypeToken {
            name: None,
            public_id: None,
            system_id: None,
            force_quirks: false,
        };
        self.appropriate_end_tag_name = None;
        self.temporary_buffer.clear();
        self.pending_tokens.clear();
        self.return_state = None;
        self.character_reference_code = 0;
        self.char_ref_hex_prefix = 'x';
        self.in_foreign_content = false;
    }

    fn set_appropriate_end_tag_name(&mut self, name: Option<&str>) {
        self.appropriate_end_tag_name = name.map(|s| s.to_string());
    }

    fn set_foreign_content(&mut self, in_foreign: bool) {
        self.in_foreign_content = in_foreign;
    }
}

// ── State handlers ────────────────────────────────────────────────

impl HtmlTokenizer {
    /// §13.2.5.1 Data state
    ///
    /// Consume the next input character:
    /// - U+0026 AMPERSAND (&) → switch to character reference state
    /// - U+003C LESS-THAN SIGN (<) → switch to tag open state
    /// - U+0000 NULL → parse error (unexpected-null-character);
    ///   emit the current input character as a character token
    /// - EOF → emit an end-of-file token
    /// - Anything else → emit the current input character as a character token
    fn handle_data_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('&') => {
                self.return_state = Some(State::Data);
                self.state = State::CharacterReference;
                None // no token emitted yet — CharacterReference will emit one
            }
            Some('<') => {
                self.state = State::TagOpen;
                None // no token emitted yet — TagOpen will emit one
            }
            Some('\0') => {
                // §13.2.5.1: unexpected-null-character parse error.
                // Emit the current input character as a character token.
                // TODO: record parse error (unexpected-null-character)
                Some(Token::Character('\0'))
            }
            Some(c) => {
                // Any other character: emit as a character token.
                Some(Token::Character(c))
            }
            None => {
                // EOF: emit end-of-file token.
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    // ── Content model states (§13.2.5.2–§13.2.5.5) ──────────────────

    /// §13.2.5.2 RCDATA state
    ///
    /// Consume the next input character:
    /// - U+0026 AMPERSAND (&) → character reference state
    /// - U+003C LESS-THAN SIGN (<) → RCDATA less-than sign state
    /// - U+0000 NULL → parse error; emit U+FFFD character token
    /// - EOF → emit end-of-file token
    /// - Anything else → emit character token
    fn handle_rcdata_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('&') => {
                self.return_state = Some(State::RCDATA);
                self.state = State::CharacterReference;
                None
            }
            Some('<') => {
                self.state = State::RCDATALessThanSign;
                None
            }
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => Some(Token::Character(c)),
            None => {
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.3 RAWTEXT state
    ///
    /// Consume the next input character:
    /// - U+003C LESS-THAN SIGN (<) → RAWTEXT less-than sign state
    /// - U+0000 NULL → parse error; emit U+FFFD character token
    /// - EOF → emit end-of-file token
    /// - Anything else → emit character token
    ///
    /// Note: RAWTEXT does NOT handle `&` — character references are not resolved.
    fn handle_rawtext_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('<') => {
                self.state = State::RAWTEXTLessThanSign;
                None
            }
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => Some(Token::Character(c)),
            None => {
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.5 PLAINTEXT state
    ///
    /// Consume the next input character:
    /// - U+0000 NULL → parse error; emit U+FFFD character token
    /// - EOF → emit end-of-file token
    /// - Anything else → emit character token
    ///
    /// Note: PLAINTEXT treats EVERYTHING as literal text — even `<` and `&`.
    /// There is no end tag to close this state; it persists until EOF.
    fn handle_plaintext_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => Some(Token::Character(c)),
            None => {
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    // ── RCDATA tag detection states (§13.2.5.9–§13.2.5.11) ─────────

    /// §13.2.5.9 RCDATA less-than sign state
    ///
    /// Consume the next input character:
    /// - U+002F SOLIDUS (/) → clear temporary buffer, switch to RCDATA end tag open
    /// - Anything else → emit `<` character token, reconsume in RCDATA
    fn handle_rcdata_less_than_sign_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('/') => {
                self.temporary_buffer.clear();
                self.state = State::RCDATAEndTagOpen;
                None
            }
            Some(_c) => {
                self.state = State::RCDATA;
                self.reconsume = true;
                Some(Token::Character('<'))
            }
            None => {
                // EOF: emit `<` then return to RCDATA (next call emits EOF)
                self.state = State::RCDATA;
                Some(Token::Character('<'))
            }
        }
    }

    /// §13.2.5.10 RCDATA end tag open state
    ///
    /// Consume the next input character:
    /// - ASCII alpha → create end tag token (empty name), append lowercase,
    ///   append original to temporary buffer, switch to RCDATA end tag name
    /// - Anything else → emit `<` + `/` char tokens, reconsume in RCDATA
    fn handle_rcdata_end_tag_open_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_alphabetic() => {
                let mut name = String::new();
                name.push(c.to_ascii_lowercase());
                self.temporary_buffer.push(c);
                self.current_tag = Some(TagToken {
                    kind: TagKind::End,
                    name,
                    attrs: Vec::new(),
                    self_closing: false,
                });
                self.state = State::RCDATAEndTagName;
                None
            }
            Some(_c) => {
                // Emit `<` + `/`, reconsume in RCDATA
                // Push in reverse order so pop() yields correct sequence
                self.pending_tokens.push(Token::Character('/'));
                self.pending_tokens.push(Token::Character('<'));
                self.state = State::RCDATA;
                self.reconsume = true;
                None
            }
            None => {
                // Emit `<` + `/`, reconsume EOF in RCDATA
                self.pending_tokens.push(Token::Character('/'));
                self.pending_tokens.push(Token::Character('<'));
                self.state = State::RCDATA;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.11 RCDATA end tag name state
    ///
    /// Consume the next input character:
    /// - TAB/LF/FF/SPACE → if appropriate end tag, switch to BeforeAttributeName;
    ///   else "anything else"
    /// - `/` → if appropriate end tag, switch to SelfClosingStartTag;
    ///   else "anything else"
    /// - `>` → if appropriate end tag, emit tag token, switch to Data;
    ///   else "anything else"
    /// - ASCII upper alpha → append lowercase to tag name, append original to
    ///   temporary buffer
    /// - ASCII lower alpha → append to tag name, append to temporary buffer
    /// - Anything else → emit `<` + `/` + temporary buffer chars, reconsume in
    ///   RCDATA
    fn handle_rcdata_end_tag_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            // Whitespace → check if appropriate, then switch to BeforeAttributeName
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                if self.is_appropriate_end_tag() {
                    self.state = State::BeforeAttributeName;
                    None
                } else {
                    self.rcdata_end_tag_name_backout()
                }
            }
            Some('/') => {
                if self.is_appropriate_end_tag() {
                    self.state = State::SelfClosingStartTag;
                    None
                } else {
                    self.rcdata_end_tag_name_backout()
                }
            }
            Some('>') => {
                if self.is_appropriate_end_tag() {
                    let tag = self.current_tag.take().unwrap();
                    self.temporary_buffer.clear();
                    self.state = State::Data;
                    Some(Token::Tag(tag))
                } else {
                    self.rcdata_end_tag_name_backout()
                }
            }
            Some(c) if c.is_ascii_uppercase() => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c.to_ascii_lowercase());
                }
                self.temporary_buffer.push(c);
                None
            }
            Some(c) if c.is_ascii_lowercase() => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c);
                }
                self.temporary_buffer.push(c);
                None
            }
            Some(_c) => {
                // Non-alpha character → "anything else" backout
                self.rcdata_end_tag_name_backout()
            }
            None => {
                // EOF → "anything else" backout to RCDATA
                self.rcdata_end_tag_name_backout()
            }
        }
    }

    /// Helper: check if the current tag being built matches the appropriate end tag.
    fn is_appropriate_end_tag(&self) -> bool {
        match (&self.appropriate_end_tag_name, &self.current_tag) {
            (Some(expected), Some(tag)) => *expected == tag.name,
            _ => false,
        }
    }

    /// Helper: "anything else" backout for RCDATA end tag name state.
    ///
    /// Emits `<` + `/` + each char in the temporary buffer as character tokens,
    /// then reconsume the current character back in RCDATA state.
    fn rcdata_end_tag_name_backout(&mut self) -> Option<Token> {
        // Push tokens in reverse order (pop yields forward order)
        // temp buffer chars first (they come after `<` and `/`)
        for ch in self.temporary_buffer.chars().rev() {
            self.pending_tokens.push(Token::Character(ch));
        }
        self.pending_tokens.push(Token::Character('/'));
        self.pending_tokens.push(Token::Character('<'));
        // Discard the partial end tag
        self.current_tag = None;
        self.temporary_buffer.clear();
        self.state = State::RCDATA;
        self.reconsume = true;
        None
    }

    // ── RAWTEXT tag detection states (§13.2.5.12–§13.2.5.14) ──────

    /// §13.2.5.12 RAWTEXT less-than sign state
    ///
    /// Consume the next input character:
    /// - U+002F SOLIDUS (/) → clear temporary buffer, switch to RAWTEXT end tag open
    /// - Anything else → emit `<` character token, reconsume in RAWTEXT
    fn handle_rawtext_less_than_sign_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('/') => {
                self.temporary_buffer.clear();
                self.state = State::RAWTEXTEndTagOpen;
                None
            }
            Some(_c) => {
                self.state = State::RAWTEXT;
                self.reconsume = true;
                Some(Token::Character('<'))
            }
            None => {
                self.state = State::RAWTEXT;
                Some(Token::Character('<'))
            }
        }
    }

    /// §13.2.5.13 RAWTEXT end tag open state
    ///
    /// Consume the next input character:
    /// - ASCII alpha → create end tag token (empty name), append lowercase,
    ///   append original to temporary buffer, switch to RAWTEXT end tag name
    /// - Anything else → emit `<` + `/` char tokens, reconsume in RAWTEXT
    fn handle_rawtext_end_tag_open_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_alphabetic() => {
                let mut name = String::new();
                name.push(c.to_ascii_lowercase());
                self.temporary_buffer.push(c);
                self.current_tag = Some(TagToken {
                    kind: TagKind::End,
                    name,
                    attrs: Vec::new(),
                    self_closing: false,
                });
                self.state = State::RAWTEXTEndTagName;
                None
            }
            Some(_c) => {
                self.pending_tokens.push(Token::Character('/'));
                self.pending_tokens.push(Token::Character('<'));
                self.state = State::RAWTEXT;
                self.reconsume = true;
                None
            }
            None => {
                self.pending_tokens.push(Token::Character('/'));
                self.pending_tokens.push(Token::Character('<'));
                self.state = State::RAWTEXT;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.14 RAWTEXT end tag name state
    ///
    /// Consume the next input character:
    /// - TAB/LF/FF/SPACE → if appropriate end tag, switch to BeforeAttributeName;
    ///   else "anything else"
    /// - `/` → if appropriate end tag, switch to SelfClosingStartTag;
    ///   else "anything else"
    /// - `>` → if appropriate end tag, emit tag token, switch to Data;
    ///   else "anything else"
    /// - ASCII upper alpha → append lowercase to tag name, append original to
    ///   temporary buffer
    /// - ASCII lower alpha → append to tag name, append to temporary buffer
    /// - Anything else → emit `<` + `/` + temporary buffer chars, reconsume in
    ///   RAWTEXT
    /// - EOF → parse error; switch to Data; emit `<` + `/`; reconsume EOF
    fn handle_rawtext_end_tag_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                if self.is_appropriate_end_tag() {
                    self.state = State::BeforeAttributeName;
                    None
                } else {
                    self.rawtext_end_tag_name_backout()
                }
            }
            Some('/') => {
                if self.is_appropriate_end_tag() {
                    self.state = State::SelfClosingStartTag;
                    None
                } else {
                    self.rawtext_end_tag_name_backout()
                }
            }
            Some('>') => {
                if self.is_appropriate_end_tag() {
                    let tag = self.current_tag.take().unwrap();
                    self.temporary_buffer.clear();
                    self.state = State::Data;
                    Some(Token::Tag(tag))
                } else {
                    self.rawtext_end_tag_name_backout()
                }
            }
            Some(c) if c.is_ascii_uppercase() => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c.to_ascii_lowercase());
                }
                self.temporary_buffer.push(c);
                None
            }
            Some(c) if c.is_ascii_lowercase() => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c);
                }
                self.temporary_buffer.push(c);
                None
            }
            Some(_c) => self.rawtext_end_tag_name_backout(),
            None => {
                // §13.2.5.14: 规范没有 EOF arm — EOF 走 "Anything else"：
                // emit `<` + `/` + temp buffer 全部字符，reconsume in RAWTEXT。
                // 与 RCDATA/ScriptData 同名状态一致，调用 backout 即可。
                self.rawtext_end_tag_name_backout()
            }
        }
    }

    /// Helper: "anything else" backout for RAWTEXT end tag name state.
    ///
    /// Emits `<` + `/` + each char in the temporary buffer as character tokens,
    /// then reconsume the current character back in RAWTEXT state.
    fn rawtext_end_tag_name_backout(&mut self) -> Option<Token> {
        for ch in self.temporary_buffer.chars().rev() {
            self.pending_tokens.push(Token::Character(ch));
        }
        self.pending_tokens.push(Token::Character('/'));
        self.pending_tokens.push(Token::Character('<'));
        self.current_tag = None;
        self.temporary_buffer.clear();
        self.state = State::RAWTEXT;
        self.reconsume = true;
        None
    }

    // ── Script data states (§13.2.5.4, §13.2.5.15–§13.2.5.17) ────

    /// §13.2.5.4 Script data state
    ///
    /// Consume the next input character:
    /// - U+003C LESS-THAN SIGN (<) → ScriptDataLessThanSign
    /// - U+0000 NULL → parse error; emit U+FFFD character token
    /// - EOF → emit end-of-file token
    /// - Anything else → emit character token
    ///
    /// Note: ScriptData does NOT handle `&` — no character references.
    fn handle_script_data_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('<') => {
                self.state = State::ScriptDataLessThanSign;
                None
            }
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => Some(Token::Character(c)),
            None => {
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.15 Script data less-than sign state
    ///
    /// Consume the next input character:
    /// - U+002F SOLIDUS (/) → clear temporary buffer, ScriptDataEndTagOpen
    /// - U+0021 EXCLAMATION MARK (!) → ScriptDataEscapeStart, emit `<`
    /// - Anything else → emit `<` character token, reconsume in ScriptData
    fn handle_script_data_less_than_sign_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('/') => {
                self.temporary_buffer.clear();
                self.state = State::ScriptDataEndTagOpen;
                None
            }
            Some('!') => {
                // §13.2.5.15: Switch to ScriptDataEscapeStart.
                // Emit `<` and `!` character tokens (both consumed chars).
                self.state = State::ScriptDataEscapeStart;
                self.pending_tokens.push(Token::Character('!'));
                Some(Token::Character('<'))
            }
            Some(_c) => {
                self.state = State::ScriptData;
                self.reconsume = true;
                Some(Token::Character('<'))
            }
            None => {
                self.state = State::ScriptData;
                Some(Token::Character('<'))
            }
        }
    }

    /// §13.2.5.16 Script data end tag open state
    ///
    /// Consume the next input character:
    /// - ASCII alpha → create end tag token (empty name), append lowercase,
    ///   append original to temporary buffer, ScriptDataEndTagName
    /// - Anything else → emit `<` + `/` char tokens, reconsume in ScriptData
    fn handle_script_data_end_tag_open_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_alphabetic() => {
                let mut name = String::new();
                name.push(c.to_ascii_lowercase());
                self.temporary_buffer.push(c);
                self.current_tag = Some(TagToken {
                    kind: TagKind::End,
                    name,
                    attrs: Vec::new(),
                    self_closing: false,
                });
                self.state = State::ScriptDataEndTagName;
                None
            }
            Some(_c) => {
                self.pending_tokens.push(Token::Character('/'));
                self.pending_tokens.push(Token::Character('<'));
                self.state = State::ScriptData;
                self.reconsume = true;
                None
            }
            None => {
                self.pending_tokens.push(Token::Character('/'));
                self.pending_tokens.push(Token::Character('<'));
                self.state = State::ScriptData;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.17 Script data end tag name state
    ///
    /// Consume the next input character:
    /// - TAB/LF/FF/SPACE → if appropriate end tag, BeforeAttributeName;
    ///   else "anything else"
    /// - `/` → if appropriate end tag, SelfClosingStartTag; else "anything else"
    /// - `>` → if appropriate end tag, emit tag, Data; else "anything else"
    /// - ASCII upper alpha → append lowercase to tag name, append original to
    ///   temporary buffer
    /// - ASCII lower alpha → append to tag name, append to temporary buffer
    /// - Anything else → emit `<` + `/` + temporary buffer chars, reconsume in
    ///   ScriptData
    fn handle_script_data_end_tag_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                if self.is_appropriate_end_tag() {
                    self.state = State::BeforeAttributeName;
                    None
                } else {
                    self.script_data_end_tag_name_backout()
                }
            }
            Some('/') => {
                if self.is_appropriate_end_tag() {
                    self.state = State::SelfClosingStartTag;
                    None
                } else {
                    self.script_data_end_tag_name_backout()
                }
            }
            Some('>') => {
                if self.is_appropriate_end_tag() {
                    let tag = self.current_tag.take().unwrap();
                    self.temporary_buffer.clear();
                    self.state = State::Data;
                    Some(Token::Tag(tag))
                } else {
                    self.script_data_end_tag_name_backout()
                }
            }
            Some(c) if c.is_ascii_uppercase() => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c.to_ascii_lowercase());
                }
                self.temporary_buffer.push(c);
                None
            }
            Some(c) if c.is_ascii_lowercase() => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c);
                }
                self.temporary_buffer.push(c);
                None
            }
            Some(_c) => self.script_data_end_tag_name_backout(),
            None => self.script_data_end_tag_name_backout(),
        }
    }

    /// §13.2.5.18 Script data escape start state
    ///
    /// Consume the next input character:
    /// - U+002D HYPHEN-MINUS (-) → ScriptDataEscapeStartDash, emit `-`
    /// - Anything else → reconsume in ScriptData (no emit — `<!` already emitted)
    fn handle_script_data_escape_start_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                // §13.2.5.18: Switch to ScriptDataEscapeStartDash.
                // Emit a `-` character token.
                self.state = State::ScriptDataEscapeStartDash;
                Some(Token::Character('-'))
            }
            Some(_c) => {
                // §13.2.5.18: Reconsume in ScriptData. (No emit — `<` and `!`
                // were already emitted by §13.2.5.15.)
                self.state = State::ScriptData;
                self.reconsume = true;
                None
            }
            None => {
                // §13.2.5.18: EOF — reconsume in ScriptData. (No emit.)
                self.state = State::ScriptData;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.19 Script data escape start dash state
    ///
    /// Consume the next input character:
    /// - U+002D HYPHEN-MINUS (-) → ScriptDataEscapedDashDash, emit `-`
    /// - Anything else → reconsume in ScriptData (no emit — `<!-` already emitted)
    fn handle_script_data_escape_start_dash_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                // §13.2.5.19: Switch to ScriptDataEscapedDashDash.
                // Emit a `-` character token.
                self.state = State::ScriptDataEscapedDashDash;
                Some(Token::Character('-'))
            }
            Some(_c) => {
                // §13.2.5.19: Reconsume in ScriptData. (No emit — `<!-` was
                // already emitted by §13.2.5.15/18.)
                self.state = State::ScriptData;
                self.reconsume = true;
                None
            }
            None => {
                // §13.2.5.19: EOF — reconsume in ScriptData. (No emit.)
                self.state = State::ScriptData;
                self.reconsume = true;
                None
            }
        }
    }

    // ── Script data escaped states (§13.2.5.20–§13.2.5.25) ──────

    /// §13.2.5.20 Script data escaped state
    ///
    /// Consume the next input character:
    /// - U+002D HYPHEN-MINUS (-) → ScriptDataEscapedDash, emit `-`
    /// - U+003C LESS-THAN SIGN (<) → ScriptDataEscapedLessThanSign (no emit)
    /// - U+0000 NULL → parse error; emit U+FFFD character token
    /// - EOF → parse error; emit end-of-file token
    /// - Anything else → emit character token
    fn handle_script_data_escaped_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                // §13.2.5.20: Switch to ScriptDataEscapedDash.
                // Emit a `-` character token.
                self.state = State::ScriptDataEscapedDash;
                Some(Token::Character('-'))
            }
            Some('<') => {
                // §13.2.5.20: Switch to ScriptDataEscapedLessThanSign.
                // (No emit — `<` will be emitted by the less-than-sign state's
                // alpha or anything-else branch.)
                self.state = State::ScriptDataEscapedLessThanSign;
                None
            }
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => Some(Token::Character(c)),
            None => {
                // TODO: record parse error (eof-in-script-html-comment-like-text)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.21 Script data escaped dash state
    ///
    /// Consume the next input character:
    /// - U+002D HYPHEN-MINUS (-) → ScriptDataEscapedDashDash, emit `-`
    /// - U+003C LESS-THAN SIGN (<) → ScriptDataEscapedLessThanSign (no emit)
    /// - U+0000 NULL → parse error; switch to ScriptDataEscaped; emit U+FFFD
    /// - EOF → parse error; emit end-of-file token
    /// - Anything else → switch to ScriptDataEscaped; emit current char
    fn handle_script_data_escaped_dash_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                // §13.2.5.20: Switch to ScriptDataEscapedDashDash.
                // Emit a `-` character token.
                self.state = State::ScriptDataEscapedDashDash;
                Some(Token::Character('-'))
            }
            Some('<') => {
                // §13.2.5.21: Switch to ScriptDataEscapedLessThanSign.
                // (No emit — `<` is emitted by the less-than-sign state's
                // alpha or anything-else branch, per §13.2.5.23.)
                self.state = State::ScriptDataEscapedLessThanSign;
                None
            }
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                // §13.2.5.20: Switch to ScriptDataEscaped; emit U+FFFD.
                self.state = State::ScriptDataEscaped;
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => {
                // §13.2.5.20: Switch to ScriptDataEscaped.
                // Emit the current input character as a character token.
                self.state = State::ScriptDataEscaped;
                Some(Token::Character(c))
            }
            None => {
                // TODO: record parse error (eof-in-script-html-comment-like-text)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.22 Script data escaped dash dash state
    ///
    /// Consume the next input character:
    /// - U+002D HYPHEN-MINUS (-) → emit `-`
    /// - U+003C LESS-THAN SIGN (<) → ScriptDataEscapedLessThanSign (no emit)
    /// - U+003E GREATER-THAN SIGN (>) → emit `>`, switch to ScriptData
    /// - U+0000 NULL → parse error; switch to ScriptDataEscaped; emit U+FFFD
    /// - EOF → parse error; emit end-of-file token
    /// - Anything else → switch to ScriptDataEscaped; emit current char
    fn handle_script_data_escaped_dash_dash_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => Some(Token::Character('-')),
            Some('<') => {
                // §13.2.5.22: Switch to ScriptDataEscapedLessThanSign.
                // (No emit — `<` is emitted by the less-than-sign state's
                // alpha or anything-else branch, per §13.2.5.23.)
                self.state = State::ScriptDataEscapedLessThanSign;
                None
            }
            Some('>') => {
                self.state = State::ScriptData;
                Some(Token::Character('>'))
            }
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                // §13.2.5.21: Switch to ScriptDataEscaped; emit U+FFFD.
                self.state = State::ScriptDataEscaped;
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => {
                // §13.2.5.21: Switch to ScriptDataEscaped.
                // Emit the current input character as a character token.
                self.state = State::ScriptDataEscaped;
                Some(Token::Character(c))
            }
            None => {
                // TODO: record parse error (eof-in-script-html-comment-like-text)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.23 Script data escaped less-than sign state
    ///
    /// Consume the next input character:
    /// - U+002F SOLIDUS (/) → clear temporary buffer, ScriptDataEscapedEndTagOpen
    /// - ASCII alpha → clear temp buffer, append lowercase version to temp
    ///   buffer, emit `<` + current input character, ScriptDataDoubleEscapeStart
    /// - Anything else → emit `<` character token, reconsume in ScriptDataEscaped
    /// - EOF → emit `<` character token, reconsume in ScriptDataEscaped
    ///
    /// NOTE: §13.2.5.20 `<` branch does NOT emit `<` ("Nothing emitted").
    /// The `<` must be emitted by this state's alpha or anything-else branch,
    /// otherwise test cases like `<!-- <test> -->` would lose the `<`.
    fn handle_script_data_escaped_less_than_sign_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('/') => {
                self.temporary_buffer.clear();
                self.state = State::ScriptDataEscapedEndTagOpen;
                None
            }
            Some(_c) if _c.is_ascii_alphabetic() => {
                // §13.2.5.23: Set the temporary buffer to the empty string.
                // Emit a U+003C LESS-THAN SIGN character token. Reconsume in
                // the script data double escape start state.
                //
                // The `<` that led here was NOT emitted by the source state
                // (§13.2.5.20/§13.2.5.21/§13.2.5.22 `<` branches all switch
                // without emitting). The alpha char is reconsumed by
                // ScriptDataDoubleEscapeStart, which appends it to the temp
                // buffer and emits it.
                self.temporary_buffer.clear();
                self.state = State::ScriptDataDoubleEscapeStart;
                self.reconsume = true;
                Some(Token::Character('<'))
            }
            Some(_c) => {
                // §13.2.5.23: Anything else — Emit `<` character token.
                // Reconsume in ScriptDataEscaped.
                self.state = State::ScriptDataEscaped;
                self.reconsume = true;
                Some(Token::Character('<'))
            }
            None => {
                // §13.2.5.23: EOF — falls under "anything else": emit `<`,
                // reconsume in ScriptDataEscaped (which will emit EOF).
                self.state = State::ScriptDataEscaped;
                self.reconsume = true;
                Some(Token::Character('<'))
            }
        }
    }

    /// §13.2.5.24 Script data escaped end tag open state
    ///
    /// Consume the next input character:
    /// - ASCII alpha → create end tag (empty name), append lowercase + original
    ///   to temp, ScriptDataEscapedEndTagName
    /// - Anything else → emit `<` + `/`, reconsume in ScriptDataEscaped
    fn handle_script_data_escaped_end_tag_open_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_alphabetic() => {
                let mut name = String::new();
                name.push(c.to_ascii_lowercase());
                self.temporary_buffer.push(c);
                self.current_tag = Some(TagToken {
                    kind: TagKind::End,
                    name,
                    attrs: Vec::new(),
                    self_closing: false,
                });
                self.state = State::ScriptDataEscapedEndTagName;
                None
            }
            Some(_c) => {
                self.pending_tokens.push(Token::Character('/'));
                self.pending_tokens.push(Token::Character('<'));
                self.state = State::ScriptDataEscaped;
                self.reconsume = true;
                None
            }
            None => {
                self.pending_tokens.push(Token::Character('/'));
                self.pending_tokens.push(Token::Character('<'));
                self.state = State::ScriptDataEscaped;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.25 Script data escaped end tag name state
    ///
    /// Same pattern as other end tag name states but returns to
    /// ScriptDataEscaped on backout.
    fn handle_script_data_escaped_end_tag_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                if self.is_appropriate_end_tag() {
                    self.state = State::BeforeAttributeName;
                    None
                } else {
                    self.script_data_escaped_end_tag_name_backout()
                }
            }
            Some('/') => {
                if self.is_appropriate_end_tag() {
                    self.state = State::SelfClosingStartTag;
                    None
                } else {
                    self.script_data_escaped_end_tag_name_backout()
                }
            }
            Some('>') => {
                if self.is_appropriate_end_tag() {
                    let tag = self.current_tag.take().unwrap();
                    self.temporary_buffer.clear();
                    self.state = State::Data;
                    Some(Token::Tag(tag))
                } else {
                    self.script_data_escaped_end_tag_name_backout()
                }
            }
            Some(c) if c.is_ascii_uppercase() => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c.to_ascii_lowercase());
                }
                self.temporary_buffer.push(c);
                None
            }
            Some(c) if c.is_ascii_lowercase() => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c);
                }
                self.temporary_buffer.push(c);
                None
            }
            Some(_c) => self.script_data_escaped_end_tag_name_backout(),
            None => self.script_data_escaped_end_tag_name_backout(),
        }
    }

    // ── Script data double escaped states (§13.2.5.26–§13.2.5.31) ─

    /// §13.2.5.26 Script data double escape start state
    ///
    /// Checks whether the accumulated temporary buffer matches "script".
    /// If so, enters double-escaped mode. Otherwise returns to escaped.
    ///
    /// - TAB/LF/FF/SPACE, `/`, or `>` → if temp buffer is "script",
    ///   ScriptDataDoubleEscaped; else ScriptDataEscaped. Emit current char.
    /// - ASCII upper alpha → append lowercase to temp buffer, emit current char
    /// - ASCII lower alpha → append to temp buffer, emit current char
    /// - Anything else → reconsume in ScriptDataEscaped
    fn handle_script_data_double_escape_start_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if matches!(c, '\t' | '\n' | '\u{000C}' | ' ' | '/' | '>') => {
                // §13.2.5.26: If temp buffer is "script", switch to
                // ScriptDataDoubleEscaped; otherwise switch to ScriptDataEscaped.
                // Emit the current input character.
                if self.temporary_buffer == "script" {
                    self.state = State::ScriptDataDoubleEscaped;
                } else {
                    self.state = State::ScriptDataEscaped;
                }
                Some(Token::Character(c))
            }
            Some(c) if c.is_ascii_uppercase() => {
                // §13.2.5.26: Append lowercase version to temp buffer.
                // Emit the current input character.
                self.temporary_buffer.push(c.to_ascii_lowercase());
                Some(Token::Character(c))
            }
            Some(c) if c.is_ascii_lowercase() => {
                // §13.2.5.26: Append current input character to temp buffer.
                // Emit the current input character.
                self.temporary_buffer.push(c);
                Some(Token::Character(c))
            }
            Some(_c) => {
                // §13.2.5.26: Reconsume in the script data escaped state.
                self.state = State::ScriptDataEscaped;
                self.reconsume = true;
                None
            }
            None => {
                // EOF: "anything else" → reconsume in ScriptDataEscaped, which
                // will emit EOF on the next step.
                self.state = State::ScriptDataEscaped;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.27 Script data double escaped state
    ///
    /// Consume the next input character:
    /// - U+002D HYPHEN-MINUS (-) → ScriptDataDoubleEscapedDash, emit `-`
    /// - U+003C LESS-THAN SIGN (<) → ScriptDataDoubleEscapedLessThanSign,
    ///   emit `<`
    /// - U+0000 NULL → parse error; emit U+FFFD character token
    /// - EOF → parse error; emit end-of-file token
    /// - Anything else → emit character token
    fn handle_script_data_double_escaped_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                self.state = State::ScriptDataDoubleEscapedDash;
                Some(Token::Character('-'))
            }
            Some('<') => {
                self.state = State::ScriptDataDoubleEscapedLessThanSign;
                Some(Token::Character('<'))
            }
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => Some(Token::Character(c)),
            None => {
                // TODO: record parse error (eof-in-script-html-comment-like-text)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.28 Script data double escaped dash state
    ///
    /// Consume the next input character:
    /// - U+002D HYPHEN-MINUS (-) → ScriptDataDoubleEscapedDashDash, emit `-`
    /// - U+003C LESS-THAN SIGN (<) → ScriptDataDoubleEscapedLessThanSign,
    ///   emit `<`
    /// - U+0000 NULL → parse error; switch to ScriptDataDoubleEscaped; emit U+FFFD
    /// - EOF → parse error; emit end-of-file token
    /// - Anything else → switch to ScriptDataDoubleEscaped; emit current char
    fn handle_script_data_double_escaped_dash_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                self.state = State::ScriptDataDoubleEscapedDashDash;
                Some(Token::Character('-'))
            }
            Some('<') => {
                self.state = State::ScriptDataDoubleEscapedLessThanSign;
                Some(Token::Character('<'))
            }
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                // §13.2.5.28: Switch to ScriptDataDoubleEscaped; emit U+FFFD.
                self.state = State::ScriptDataDoubleEscaped;
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => {
                // §13.2.5.28: Switch to ScriptDataDoubleEscaped.
                // Emit the current input character as a character token.
                self.state = State::ScriptDataDoubleEscaped;
                Some(Token::Character(c))
            }
            None => {
                // TODO: record parse error (eof-in-script-html-comment-like-text)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.29 Script data double escaped dash dash state
    ///
    /// Consume the next input character:
    /// - U+002D HYPHEN-MINUS (-) → emit `-`
    /// - U+003C LESS-THAN SIGN (<) → ScriptDataDoubleEscapedLessThanSign,
    ///   emit `<`
    /// - U+003E GREATER-THAN SIGN (>) → emit `>`, switch to ScriptData
    /// - U+0000 NULL → parse error; switch to ScriptDataDoubleEscaped; emit U+FFFD
    /// - EOF → parse error; emit end-of-file token
    /// - Anything else → switch to ScriptDataDoubleEscaped; emit current char
    fn handle_script_data_double_escaped_dash_dash_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => Some(Token::Character('-')),
            Some('<') => {
                self.state = State::ScriptDataDoubleEscapedLessThanSign;
                Some(Token::Character('<'))
            }
            Some('>') => {
                self.state = State::ScriptData;
                Some(Token::Character('>'))
            }
            Some('\0') => {
                // TODO: record parse error (unexpected-null-character)
                // §13.2.5.29: Switch to ScriptDataDoubleEscaped; emit U+FFFD.
                self.state = State::ScriptDataDoubleEscaped;
                Some(Token::Character('\u{FFFD}'))
            }
            Some(c) => {
                // §13.2.5.29: Switch to ScriptDataDoubleEscaped.
                // Emit the current input character as a character token.
                self.state = State::ScriptDataDoubleEscaped;
                Some(Token::Character(c))
            }
            None => {
                // TODO: record parse error (eof-in-script-html-comment-like-text)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.30 Script data double escaped less-than sign state
    ///
    /// Consume the next input character:
    /// - U+002F SOLIDUS (/) → clear temporary buffer, emit `/`,
    ///   ScriptDataDoubleEscapeEnd
    /// - Anything else → emit `<`, reconsume in ScriptDataDoubleEscaped
    fn handle_script_data_double_escaped_less_than_sign_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('/') => {
                self.temporary_buffer.clear();
                self.state = State::ScriptDataDoubleEscapeEnd;
                // Emit `/` via pending — the spec says "Emit a U+002F SOLIDUS
                // character token."
                self.pending_tokens.push(Token::Character('/'));
                None
            }
            Some(_c) => {
                // §13.2.5.30: Anything else — Reconsume in the script data
                // double escaped state. (No emit — the `<` was already emitted
                // by §13.2.5.27's `<` branch.)
                self.state = State::ScriptDataDoubleEscaped;
                self.reconsume = true;
                None
            }
            None => {
                // §13.2.5.30: EOF falls under "anything else" — reconsume in
                // ScriptDataDoubleEscaped (which will emit EOF). No `<` emit.
                self.state = State::ScriptDataDoubleEscaped;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.31 Script data double escape end state
    ///
    /// Checks whether the accumulated temporary buffer matches "script".
    /// If so, exits double-escaped mode back to escaped. Otherwise stays
    /// in double-escaped mode.
    ///
    /// - TAB/LF/FF/SPACE, `/`, or `>` → if temp buffer is "script",
    ///   ScriptDataEscaped; else ScriptDataDoubleEscaped. Emit current char.
    /// - ASCII upper alpha → append lowercase to temp buffer, emit current char
    /// - ASCII lower alpha → append to temp buffer, emit current char
    /// - Anything else → reconsume in ScriptDataDoubleEscaped
    fn handle_script_data_double_escape_end_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if matches!(c, '\t' | '\n' | '\u{000C}' | ' ' | '/' | '>') => {
                // §13.2.5.31: If temp buffer is "script", switch to
                // ScriptDataEscaped; otherwise switch to ScriptDataDoubleEscaped.
                // Emit the current input character.
                if self.temporary_buffer == "script" {
                    self.state = State::ScriptDataEscaped;
                } else {
                    self.state = State::ScriptDataDoubleEscaped;
                }
                Some(Token::Character(c))
            }
            Some(c) if c.is_ascii_uppercase() => {
                // §13.2.5.31: Append lowercase version to temp buffer.
                // Emit the current input character.
                self.temporary_buffer.push(c.to_ascii_lowercase());
                Some(Token::Character(c))
            }
            Some(c) if c.is_ascii_lowercase() => {
                // §13.2.5.31: Append current input character to temp buffer.
                // Emit the current input character.
                self.temporary_buffer.push(c);
                Some(Token::Character(c))
            }
            Some(_c) => {
                // §13.2.5.31: Reconsume in the script data double escaped state.
                self.state = State::ScriptDataDoubleEscaped;
                self.reconsume = true;
                None
            }
            None => {
                // EOF: "anything else" → reconsume in ScriptDataDoubleEscaped,
                // which will emit EOF on the next step.
                self.state = State::ScriptDataDoubleEscaped;
                self.reconsume = true;
                None
            }
        }
    }

    // ── Script data backout helpers ─────────────────────────────

    /// Helper: "anything else" backout for ScriptData end tag name state.
    fn script_data_end_tag_name_backout(&mut self) -> Option<Token> {
        for ch in self.temporary_buffer.chars().rev() {
            self.pending_tokens.push(Token::Character(ch));
        }
        self.pending_tokens.push(Token::Character('/'));
        self.pending_tokens.push(Token::Character('<'));
        self.current_tag = None;
        self.temporary_buffer.clear();
        self.state = State::ScriptData;
        self.reconsume = true;
        None
    }

    /// Helper: "anything else" backout for ScriptDataEscaped end tag name.
    fn script_data_escaped_end_tag_name_backout(&mut self) -> Option<Token> {
        for ch in self.temporary_buffer.chars().rev() {
            self.pending_tokens.push(Token::Character(ch));
        }
        self.pending_tokens.push(Token::Character('/'));
        self.pending_tokens.push(Token::Character('<'));
        self.current_tag = None;
        self.temporary_buffer.clear();
        self.state = State::ScriptDataEscaped;
        self.reconsume = true;
        None
    }

    /// §13.2.5.6 Tag open state
    ///
    /// Consume the next input character:
    /// - `!` → switch to markup declaration open state
    /// - `/` → switch to end tag open state
    /// - ASCII alpha → create a new start tag token with the current
    ///   character as its tag name, switch to tag name state
    /// - `?` → parse error; create a comment token (data = "?"), switch
    ///   to bogus comment state
    /// - EOF → parse error; emit `<` character token + EOF
    /// - Anything else → parse error; emit `<` character token, reconsume
    ///   the current character in the data state
    fn handle_tag_open_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('!') => {
                self.state = State::MarkupDeclarationOpen;
                None
            }
            Some('/') => {
                self.state = State::EndTagOpen;
                None
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let mut name = String::new();
                name.push(c.to_ascii_lowercase());
                self.current_tag = Some(TagToken {
                    kind: TagKind::Start,
                    name,
                    attrs: Vec::new(),
                    self_closing: false,
                });
                self.state = State::TagName;
                None
            }
            Some('?') => {
                // §13.2.5.6: U+003F QUESTION MARK (?)
                // Set the temporary buffer to the empty string. Switch to the
                // processing instruction open state.
                self.temporary_buffer.clear();
                self.state = State::ProcessingInstructionOpen;
                None
            }
            Some(_c) => {
                self.state = State::Data;
                self.reconsume = true;
                Some(Token::Character('<'))
            }
            None => {
                self.state = State::Data;
                Some(Token::Character('<'))
            }
        }
    }

    /// §13.2.5.7 End tag open state
    ///
    /// - ASCII alpha → create end tag token, append lowercased char to name, switch to TagName
    /// - `>` → parse error, switch to Data (no emit)
    /// - EOF → parse error, emit `<` + EOF
    /// - Anything else → parse error, emit `<` and `/`, reconsume in Data
    fn handle_end_tag_open_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_alphabetic() => {
                let mut name = String::new();
                name.push(c.to_ascii_lowercase());
                self.current_tag = Some(TagToken {
                    kind: TagKind::End,
                    name,
                    attrs: Vec::new(),
                    self_closing: false,
                });
                self.state = State::TagName;
                None
            }
            None => {
                // §13.2.5.7: Parse error (eof-before-tag-name). Emit `<` and
                // `/` character tokens. Reconsume in the data state (which
                // emits EOF next). Verified by html5lib: `</` → Character "</".
                // TODO: parse error (eof-before-tag-name)
                self.state = State::Data;
                self.reconsume = true;
                self.pending_tokens.push(Token::Character('/'));
                self.pending_tokens.push(Token::Character('<'));
                None
            }
            Some('>') => {
                // §13.2.5.7: Parse error (missing-end-tag-name). Switch to Data.
                self.state = State::Data;
                None
            }
            Some(_c) => {
                // §13.2.5.7: Parse error. Create a comment token whose data is
                // the empty string. Switch to the bogus comment state. Reconsume
                // the current input character. Verified by html5lib:
                // `</\t` (EOF) → Comment "\t"; `</x` (EOF) → Comment "x".
                // TODO: parse error (invalid-first-character-of-tag-name)
                self.current_comment.clear();
                self.state = State::BogusComment;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.8 Tag name state
    ///
    /// Consume the next input character:
    /// - ASCII alpha/upper → append lowercase to tag name, stay in TagName
    /// - NULL → append U+FFFD, stay in TagName
    /// - TAB/LF/FF/SPACE → switch to BeforeAttributeName
    /// - `/` → switch to SelfClosingStartTag
    /// - `>` → emit current tag token, switch to Data
    /// - EOF → discard tag, emit EOF
    /// - Anything else → append to tag name, stay in TagName
    fn handle_tag_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_alphabetic() => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c.to_ascii_lowercase());
                }
                self.state = State::TagName;
                None
            }
            Some('\0') => {
                // unexpected-null-character parse error
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push('\u{FFFD}');
                }
                self.state = State::TagName;
                None
            }
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.state = State::BeforeAttributeName;
                None
            }
            Some('/') => {
                self.state = State::SelfClosingStartTag;
                None
            }
            Some('>') => {
                let tag = self.current_tag.take().unwrap();
                self.state = State::Data;
                Some(Token::Tag(tag))
            }
            None => {
                // EOF: discard incomplete tag, emit EOF
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(c) => {
                if let Some(ref mut tag) = self.current_tag {
                    tag.name.push(c);
                }
                self.state = State::TagName;
                None
            }
        }
    }

    /// §13.2.5.40 Self-closing start tag state
    ///
    /// Consume the next input character:
    /// - `>` → set self_closing flag, emit current tag token, switch to Data
    /// - Anything else → parse error, switch to BeforeAttributeName, reconsume
    /// - EOF → discard tag, emit EOF
    fn handle_self_closing_start_tag_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('>') => {
                let mut tag = self.current_tag.take().unwrap();
                tag.self_closing = true;
                self.state = State::Data;
                Some(Token::Tag(tag))
            }
            None => {
                // EOF: discard incomplete tag, emit EOF
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(_c) => {
                // unexpected-solidus-in-tag parse error
                self.state = State::BeforeAttributeName;
                self.reconsume = true;
                None
            }
        }
    }

    // ── Character reference states (§13.2.5.72–§13.2.5.80) ─────－

    /// §13.2.5.72 Character reference state
    ///
    /// Dispatches the character following `&`:
    /// - U+0009 TAB → emit `&`, reconsume in return_state
    /// - U+000A LF → emit `&`, reconsume in return_state
    /// - U+000C FF → emit `&`, reconsume in return_state
    /// - U+0020 SPACE → emit `&`, reconsume in return_state
    /// - U+003C LESS-THAN SIGN (<) → emit `&`, reconsume in return_state
    /// - U+0026 AMPERSAND (&) → emit `&`, reconsume in return_state
    /// - EOF → parse error; emit `&`, reconsume EOF
    /// - U+0023 NUMBER SIGN (#) → NumericCharacterReference
    /// - ASCII alphanumeric → NamedCharacterReference
    /// - Anything else → emit `&`, reconsume in return_state
    fn handle_character_reference_state(&mut self) -> Option<Token> {
        match self.next_char() {
            // Characters that cause immediate fallback (not a valid char ref)
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') | Some('<') | Some('&') => {
                self.emit_ampersand_and_return()
            }
            None => {
                // TODO: record parse error (eof-in-tag — actually not in a tag,
                // but spec says this is an error)
                self.emit_ampersand_and_return()
            }
            Some('#') => {
                self.state = State::NumericCharacterReference;
                None
            }
            Some(c) if c.is_ascii_alphanumeric() => {
                self.state = State::NamedCharacterReference;
                // The character was already consumed — we'll handle it
                // in NamedCharacterReference. For the spec-compliant approach,
                // we should reconsume it rather than consume it.
                // But since NamedCharacterReference needs to build up the name,
                // we reconsume so it can consume this char.
                self.reconsume = true;
                None
            }
            Some(_c) => {
                // Non-alphanumeric, non-# → not a character reference
                self.emit_ampersand_and_return()
            }
        }
    }

    /// §13.2.5.75 Numeric character reference state
    ///
    /// Consume the next input character:
    /// - U+0078 x / U+0058 X → HexCharacterReferenceStart
    /// - ASCII digit → reconsume in DecimalCharacterReferenceStart
    /// - Anything else → parse error; emit `&#`, reconsume
    fn handle_numeric_character_reference_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c == 'x' || c == 'X' => {
                self.character_reference_code = 0;
                self.char_ref_hex_prefix = c;
                self.state = State::HexCharacterReferenceStart;
                None
            }
            Some(c) if c.is_ascii_digit() => {
                self.character_reference_code = 0;
                self.state = State::DecimalCharacterReferenceStart;
                self.reconsume = true;
                None
            }
            Some(_c) => {
                // TODO: record parse error (absence-of-digits-in-numeric-character-reference)
                self.emit_ampersand_hash_and_return()
            }
            None => {
                // TODO: record parse error
                self.emit_ampersand_hash_and_return()
            }
        }
    }

    /// §13.2.5.76 Hexadecimal character reference start state
    ///
    /// Consume the next input character:
    /// - ASCII hex digit → reconsume in HexCharacterReference
    /// - Anything else → parse error; emit `&#x`, reconsume
    fn handle_hex_character_reference_start_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_hexdigit() => {
                self.state = State::HexCharacterReference;
                self.reconsume = true;
                None
            }
            Some(_c) => {
                // TODO: record parse error (absence-of-digits-in-numeric-character-reference)
                self.emit_ampersand_hash_x_and_return()
            }
            None => self.emit_ampersand_hash_x_and_return(),
        }
    }

    /// §13.2.5.77 Decimal character reference start state
    ///
    /// Consume the next input character:
    /// - ASCII digit → reconsume in DecimalCharacterReference
    /// - Anything else → parse error; emit `&#`, reconsume
    fn handle_decimal_character_reference_start_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_digit() => {
                self.state = State::DecimalCharacterReference;
                self.reconsume = true;
                None
            }
            Some(_c) => {
                // TODO: record parse error
                self.emit_ampersand_hash_and_return()
            }
            None => self.emit_ampersand_hash_and_return(),
        }
    }

    /// §13.2.5.78 Hexadecimal character reference state
    ///
    /// Consume the next input character:
    /// - ASCII hex digit → multiply code by 16, add digit value, stay
    /// - U+003B SEMICOLON (;) → switch to NumericCharacterReferenceEnd
    /// - Anything else → parse error; reconsume in NumericCharacterReferenceEnd
    fn handle_hex_character_reference_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_hexdigit() => {
                let digit = match c {
                    '0'..='9' => c as u32 - '0' as u32,
                    'a'..='f' => c as u32 - 'a' as u32 + 10,
                    'A'..='F' => c as u32 - 'A' as u32 + 10,
                    _ => unreachable!(),
                };
                self.character_reference_code = self
                    .character_reference_code
                    .saturating_mul(16)
                    .saturating_add(digit);
                None
            }
            Some(';') => {
                self.state = State::NumericCharacterReferenceEnd;
                None
            }
            Some(_c) => {
                // TODO: record parse error (missing-semicolon-after-character-reference)
                self.state = State::NumericCharacterReferenceEnd;
                self.reconsume = true;
                None
            }
            None => {
                // TODO: record parse error
                self.state = State::NumericCharacterReferenceEnd;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.79 Decimal character reference state
    ///
    /// Consume the next input character:
    /// - ASCII digit → multiply code by 10, add digit value, stay
    /// - U+003B SEMICOLON (;) → switch to NumericCharacterReferenceEnd
    /// - Anything else → parse error; reconsume in NumericCharacterReferenceEnd
    fn handle_decimal_character_reference_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_digit() => {
                let digit = c as u32 - '0' as u32;
                self.character_reference_code = self
                    .character_reference_code
                    .saturating_mul(10)
                    .saturating_add(digit);
                None
            }
            Some(';') => {
                self.state = State::NumericCharacterReferenceEnd;
                None
            }
            Some(_c) => {
                // TODO: record parse error (missing-semicolon-after-character-reference)
                self.state = State::NumericCharacterReferenceEnd;
                self.reconsume = true;
                None
            }
            None => {
                self.state = State::NumericCharacterReferenceEnd;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.80 Numeric character reference end state
    ///
    /// Validate the accumulated code point and emit:
    /// - 0x00 → parse error; emit U+FFFD
    /// - 0x80–0x9F → replace with Windows-1252 equivalent (0x80 → €, etc.)
    /// - Surrogate (0xD800–0xDFFF) → parse error; emit U+FFFD
    /// - > 0x10FFFF → parse error; emit U+FFFD
    /// - Valid → emit the resolved character, return to return_state
    fn handle_numeric_character_reference_end_state(&mut self) -> Option<Token> {
        let code = self.character_reference_code;
        self.character_reference_code = 0;

        let ch = match code {
            0x00 => {
                // TODO: record parse error (null-character-reference)
                '\u{FFFD}'
            }
            // Surrogate range
            0xD800..=0xDFFF => {
                // TODO: record parse error
                '\u{FFFD}'
            }
            // Beyond Unicode
            c if c > 0x10FFFF => {
                // TODO: record parse error
                '\u{FFFD}'
            }
            // Windows-1252 replacement range (0x80–0x9F, except some)
            0x80 => '\u{20AC}', // €
            0x82 => '\u{201A}', // ‚
            0x83 => '\u{0192}', // ƒ
            0x84 => '\u{201E}', // „
            0x85 => '\u{2026}', // …
            0x86 => '\u{2020}', // †
            0x87 => '\u{2021}', // ‡
            0x88 => '\u{02C6}', // ˆ
            0x89 => '\u{2030}', // ‰
            0x8A => '\u{0160}', // Š
            0x8B => '\u{2039}', // ‹
            0x8C => '\u{0152}', // Œ
            0x8E => '\u{017D}', // Ž
            0x91 => '\u{2018}', // '
            0x92 => '\u{2019}', // '
            0x93 => '\u{201C}', // "
            0x94 => '\u{201D}', // "
            0x95 => '\u{2022}', // •
            0x96 => '\u{2013}', // –
            0x97 => '\u{2014}', // —
            0x98 => '\u{02DC}', // ˜
            0x99 => '\u{2122}', // ™
            0x9A => '\u{0161}', // š
            0x9B => '\u{203A}', // ›
            0x9C => '\u{0153}', // œ
            0x9E => '\u{017E}', // ž
            0x9F => '\u{0178}', // Ÿ
            // Valid Unicode
            _ => {
                // Safety: we already checked surrogates and >0x10FFFF
                char::from_u32(code).unwrap_or('\u{FFFD}')
            }
        };

        let state = self.return_state.take().unwrap_or(State::Data);
        self.state = state;
        // §13.2.5.85 + §13.2.5 flush 定义：在属性值上下文下，flush 只 append
        // 到 current_attr_value，不 emit character token。
        match state {
            State::AttributeValueDoubleQuoted
            | State::AttributeValueSingleQuoted
            | State::AttributeValueUnquoted => {
                self.current_attr_value.push(ch);
                None
            }
            _ => Some(Token::Character(ch)),
        }
    }

    // ── Named entity table ────────────────────────────────────
    // Moved to src/tokenizer/entities.rs — full WHATWG entity table
    // with 2,231 entries using sorted array + binary search.

    /// Resolve a named character reference to its Unicode string.
    /// Delegates to the generated entity table in `entities.rs`.
    /// Returns `Some(&str)` with 1-2 code points if found, `None` otherwise.
    fn resolve_named_entity(name: &str) -> Option<&'static str> {
        crate::tokenizer::entities::resolve_named_entity(name)
    }

    /// §13.2.5.78 Named character reference state
    ///
    /// "Consume the maximum number of characters possible, where the consumed
    /// characters are one of the identifiers in the first column of the named
    /// character references table. Append each character to the temporary
    /// buffer when it's consumed."
    ///
    /// Strategy: greedily consume ASCII alphanumeric chars into the buffer,
    /// then find the longest prefix that matches an entity name. If the next
    /// input char is `;` and `prefix + ";"` matches, prefer that form (it is
    /// one char longer and avoids the missing-semicolon parse error).
    ///
    /// Match examples (§13.2.5.78):
    ///   `&notit;` → match `not` (legacy), emit `¬`, reconsume `it;` in
    ///   return state. `&notin;` → match `notin;`, emit `∉`, no parse error.
    ///
    /// In attribute value context, resolved chars and literal flushes go ONLY
    /// to `current_attr_value` — no Character tokens are emitted. In body
    /// context, they go ONLY to the token stream.
    fn handle_named_character_reference_state(&mut self) -> Option<Token> {
        // The first alphanumeric was reconsumed by CharacterReference state —
        // consume it now and seed the buffer.
        if let Some(c) = self.next_char() {
            self.temporary_buffer.push(c);
        }

        // Consume remaining ASCII alphanumeric chars greedily (§13.2.5.78:
        // "maximum number of characters possible").
        while let Some(&c) = self.input.get(self.pos) {
            if !c.is_ascii_alphanumeric() {
                break;
            }
            self.temporary_buffer.push(c);
            self.pos += 1;
            self.last_was_eof = false;
        }

        // Peek the next input char (do not consume yet).
        let next_is_semicolon = self.input.get(self.pos).copied() == Some(';');

        // Find the longest prefix of `temporary_buffer` that matches an
        // entity name. The `prefix + ";"` form is only valid when the prefix
        // length equals the full buffer length, because `;` (if present) is
        // adjacent only to the end of the buffer — not to any shorter prefix.
        // For shorter prefixes, the char immediately after the prefix is
        // another alphanumeric, never `;`. `match_len` counts total consumed
        // chars including `;` when `has_semi` is true.
        let buf_len = self.temporary_buffer.len();
        let mut best_match: Option<(usize, &'static str, bool)> = None;
        for l in (1..=buf_len).rev() {
            if best_match.is_some() {
                break;
            }
            if next_is_semicolon && l == buf_len {
                let mut with_semi = String::with_capacity(l + 1);
                with_semi.push_str(&self.temporary_buffer);
                with_semi.push(';');
                if let Some(s) = Self::resolve_named_entity(&with_semi) {
                    best_match = Some((l + 1, s, true));
                    continue;
                }
            }
            if let Some(s) = Self::resolve_named_entity(&self.temporary_buffer[..l]) {
                best_match = Some((l, s, false));
            }
        }

        let return_state = self.return_state.take().unwrap_or(State::Data);
        let in_attr = matches!(
            return_state,
            State::AttributeValueDoubleQuoted
                | State::AttributeValueSingleQuoted
                | State::AttributeValueUnquoted
        );

        match best_match {
            Some((match_len, resolved, has_semi)) => {
                // If the match includes `;`, consume it now.
                if has_semi {
                    self.pos += 1;
                    self.last_was_eof = false;
                    self.temporary_buffer.push(';');
                }
                // Rewind position to the end of the match so that any
                // consumed chars beyond the match are reprocessed in the
                // return state (§13.2.5.78 "maximum number" rule).
                let extra = self.temporary_buffer.len() - match_len;
                if extra > 0 {
                    self.pos -= extra;
                    self.temporary_buffer.truncate(match_len);
                }

                // §13.2.5.78 attr-context historical rule: if consumed as
                // part of an attribute, last matched char is not `;`, and
                // the next input char is `=` or ASCII alphanumeric, flush
                // as literal (don't resolve the entity).
                let next_after_match = self.input.get(self.pos).copied();
                if in_attr
                    && !has_semi
                    && (next_after_match == Some('=')
                        || matches!(next_after_match, Some(c) if c.is_ascii_alphanumeric()))
                {
                    self.flush_literal_ampersand_and_name(return_state);
                    self.state = return_state;
                    return None;
                }

                // Normal path: emit resolved char(s). (Missing-semicolon
                // parse error per §13.2.5.78 step 1 is not recorded here.)
                self.temporary_buffer.clear();
                self.state = return_state;
                self.emit_resolved(resolved, return_state)
            }
            None => {
                // No match: flush `&` + consumed name as literal. The next
                // input char (boundary) is consumed fresh in the return
                // state — functionally equivalent to the spec's "flush +
                // switch to ambiguous ampersand state" path, since all
                // consumed chars are ASCII alphanumeric and would be
                // emitted/appended one-by-one in ambiguous ampersand state.
                self.flush_literal_ampersand_and_name(return_state);
                self.state = return_state;
                None
            }
        }
    }

    /// Emit the first char of a resolved entity string, pushing remaining
    /// chars to pending_tokens.
    fn emit_multi_char(&mut self, s: &str) -> Option<Token> {
        let mut chars = s.chars();
        let first = chars.next().unwrap();
        // Push remaining chars in reverse for correct pop order
        for ch in chars.rev() {
            self.pending_tokens.push(Token::Character(ch));
        }
        Some(Token::Character(first))
    }

    /// §13.2.5.74 Ambiguous ampersand state
    ///
    /// Consume the next input character:
    /// - ASCII alphanumeric → emit the character, stay
    /// - U+003B SEMICOLON (;) → emit `;`, switch to return_state
    /// - Anything else → reconsume in return_state
    fn handle_ambiguous_ampersand_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_alphanumeric() => Some(Token::Character(c)),
            Some(';') => {
                let state = self.return_state.take().unwrap_or(State::Data);
                self.state = state;
                Some(Token::Character(';'))
            }
            Some(_c) => {
                let state = self.return_state.take().unwrap_or(State::Data);
                self.state = state;
                self.reconsume = true;
                None
            }
            None => {
                let state = self.return_state.take().unwrap_or(State::Data);
                self.state = state;
                None
            }
        }
    }

    /// Emit resolved entity character(s), handling attribute vs body context.
    /// In attribute value state: append to `current_attr_value` only (no token).
    /// In other states: emit as Character token(s) via [`emit_multi_char`].
    fn emit_resolved(&mut self, s: &str, return_state: State) -> Option<Token> {
        match return_state {
            State::AttributeValueDoubleQuoted
            | State::AttributeValueSingleQuoted
            | State::AttributeValueUnquoted => {
                self.current_attr_value.push_str(s);
                None
            }
            _ => self.emit_multi_char(s),
        }
    }

    /// Flush `&` + `temporary_buffer` contents as literal characters.
    /// In attribute value state: append to `current_attr_value` only.
    /// In other states: push as pending Character tokens (`&` pops first,
    /// then name chars in forward order).
    fn flush_literal_ampersand_and_name(&mut self, return_state: State) {
        match return_state {
            State::AttributeValueDoubleQuoted
            | State::AttributeValueSingleQuoted
            | State::AttributeValueUnquoted => {
                self.current_attr_value.push('&');
                self.current_attr_value.push_str(&self.temporary_buffer);
            }
            _ => {
                // Push in reverse so `&` pops first, then name chars forward.
                for ch in self.temporary_buffer.chars().rev() {
                    self.pending_tokens.push(Token::Character(ch));
                }
                self.pending_tokens.push(Token::Character('&'));
            }
        }
        self.temporary_buffer.clear();
    }

    // ── Character reference fallback helpers ──────────────────

    /// Emit `&` and return to the return_state.
    /// Used when an `&` is not followed by a valid character reference.
    /// Per spec §13.2.5.77 "Anything else → flush code points consumed as
    /// a character reference. Reconsume in the return state." 临时缓冲区
    /// 只有 `&`，按 §13.2.5 flush 定义：属性上下文 append 到 attr value 并返回
    /// None；其他上下文 emit character token。
    fn emit_ampersand_and_return(&mut self) -> Option<Token> {
        let state = self.return_state.take().unwrap_or(State::Data);
        self.state = state;
        self.reconsume = true;
        match state {
            State::AttributeValueDoubleQuoted
            | State::AttributeValueSingleQuoted
            | State::AttributeValueUnquoted => {
                self.current_attr_value.push('&');
                None
            }
            _ => Some(Token::Character('&')),
        }
    }

    /// Emit `&#` and return. §13.2.5.80/82 "absence-of-digits-in-numeric-
    /// character-reference parse error. Flush code points consumed as a
    /// character reference. Reconsume in the return state." 临时缓冲区是
    /// `&#`，按 §13.2.5 flush 定义：属性上下文 append 到 attr value；其他上下文
    /// push 到 pending_tokens（`&` 先 pop，再 `#`）。
    fn emit_ampersand_hash_and_return(&mut self) -> Option<Token> {
        let state = self.return_state.take().unwrap_or(State::Data);
        self.state = state;
        self.reconsume = true;
        match state {
            State::AttributeValueDoubleQuoted
            | State::AttributeValueSingleQuoted
            | State::AttributeValueUnquoted => {
                self.current_attr_value.push('&');
                self.current_attr_value.push('#');
            }
            _ => {
                self.pending_tokens.push(Token::Character('#'));
                self.pending_tokens.push(Token::Character('&'));
            }
        }
        None
    }

    /// Emit `&#x` (or `&#X`) and return. §13.2.5.81 "absence-of-digits-in-
    /// numeric-character-reference parse error. Flush code points consumed
    /// as a character reference. Reconsume in the return state." 临时缓冲区
    /// 是 `&#x`，用 `char_ref_hex_prefix` 保留原 case。按 §13.2.5 flush 定义：
    /// 属性上下文 append 到 attr value；其他上下文 push 到 pending_tokens。
    fn emit_ampersand_hash_x_and_return(&mut self) -> Option<Token> {
        let state = self.return_state.take().unwrap_or(State::Data);
        self.state = state;
        self.reconsume = true;
        match state {
            State::AttributeValueDoubleQuoted
            | State::AttributeValueSingleQuoted
            | State::AttributeValueUnquoted => {
                self.current_attr_value.push('&');
                self.current_attr_value.push('#');
                self.current_attr_value.push(self.char_ref_hex_prefix);
            }
            _ => {
                self.pending_tokens
                    .push(Token::Character(self.char_ref_hex_prefix));
                self.pending_tokens.push(Token::Character('#'));
                self.pending_tokens.push(Token::Character('&'));
            }
        }
        None
    }

    // ── Attribute state helpers ───────────────────────────────────

    /// Push the currently accumulated attribute (name + value) into the tag.
    fn emit_current_attribute(&mut self) {
        if let Some(ref mut tag) = self.current_tag {
            let name = std::mem::take(&mut self.current_attr_name);
            let value = std::mem::take(&mut self.current_attr_value);
            // §13.2.5.32 / §13.2.6.3: duplicate attribute names are a parse
            // error; the new (duplicate) attribute is dropped, keeping the
            // first. Verified by html5lib: `<h a='b' a='d'>` → one attr a="b".
            if tag.attrs.iter().any(|(n, _)| *n == name) {
                // TODO: parse error (duplicate-attribute)
                return;
            }
            tag.attrs.push((name, value));
        }
    }

    // ── Markup declaration (§13.2.5.42) ──────────────────────────

    /// §13.2.5.42 Markup declaration open state
    ///
    /// Dispatches `<!` to comment, DOCTYPE, CDATA, or bogus comment.
    fn handle_markup_declaration_open_state(&mut self) -> Option<Token> {
        // 检查 "--"（注释起始，2 字符）
        if self.pos + 1 < self.input.len()
            && self.input[self.pos] == '-'
            && self.input[self.pos + 1] == '-'
        {
            self.pos += 2;
            self.current_comment.clear();
            self.state = State::CommentStart;
            return None;
        }

        // 检查 "DOCTYPE"（大小写不敏感，7 字符）
        if self.pos + 6 < self.input.len() {
            let slice: String = self.input[self.pos..self.pos + 7].iter().collect();
            if slice.eq_ignore_ascii_case("DOCTYPE") {
                self.pos += 7;
                // TODO: Step 1.4 — DOCTYPE 状态尚未实现
                self.state = State::Doctype;
                return None;
            }
        }

        // 检查 "[CDATA["（7 字符）
        // §13.2.5.42: If the adjusted current node is not in the HTML
        // namespace, switch to CDATA section state. Otherwise, this is a
        // cdata-in-html-content parse error: create a comment token with
        // data "[CDATA[" and switch to bogus comment state.
        if self.pos + 6 < self.input.len() {
            let slice: String = self.input[self.pos..self.pos + 7].iter().collect();
            if slice == "[CDATA[" {
                self.pos += 7;
                if self.in_foreign_content {
                    // Foreign content (SVG/MathML): switch to CDATA section
                    // state. The CDATA section state emits character tokens
                    // that tree construction inserts as text content.
                    self.state = State::CDATASection;
                } else {
                    // HTML content: cdata-in-html-content parse error.
                    // Create a comment token whose data is "[CDATA[" and
                    // switch to bogus comment state.
                    self.current_comment.clear();
                    self.current_comment.push_str("[CDATA[");
                    self.state = State::BogusComment;
                }
                return None;
            }
        }

        // 都不匹配 → parse error → BogusComment
        // 注意：不消费任何字符，BogusComment 会自行消费
        self.state = State::BogusComment;
        None
    }

    // ── CDATA section states (§13.2.5.69–§13.2.5.71) ────────────

    /// §13.2.5.69 CDATA section state
    ///
    /// Consume the next input character:
    /// - U+005D RIGHT SQUARE BRACKET (]) → CDATASectionBracket
    /// - EOF → parse error (eof-in-cdata); emit end-of-file token
    /// - Anything else → emit character token
    ///
    /// 注意：§13.2.5.69 规范明确说明 "U+0000 NULL characters are handled in
    /// the tree construction stage, as part of the in foreign content
    /// insertion mode"。tokenizer 不对 NUL 做特殊处理，按 "Anything else"
    /// emit NUL 字符 token，由 tree construction 阶段处理。
    fn handle_cdata_section_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(']') => {
                self.state = State::CDATASectionBracket;
                None
            }
            Some(c) => Some(Token::Character(c)),
            None => {
                // TODO: record parse error (eof-in-cdata)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
        }
    }

    /// §13.2.5.70 CDATA section bracket state
    ///
    /// Consume the next input character:
    /// - U+005D RIGHT SQUARE BRACKET (]) → CDATASectionEnd
    /// - Anything else → emit `]`, reconsume in CDATASection
    fn handle_cdata_section_bracket_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(']') => {
                self.state = State::CDATASectionEnd;
                None
            }
            Some(_c) => {
                self.state = State::CDATASection;
                self.reconsume = true;
                Some(Token::Character(']'))
            }
            None => {
                // §13.2.5.70: Parse error (eof-in-cdata). Emit `]` character
                // token. Reconsume in the CDATA section state (which will then
                // emit EOF on the next step).
                self.state = State::CDATASection;
                self.reconsume = true;
                Some(Token::Character(']'))
            }
        }
    }

    /// §13.2.5.71 CDATA section end state
    ///
    /// Consume the next input character:
    /// - U+003E GREATER-THAN SIGN (>) → switch to Data (CDATA closed)
    /// - U+005D RIGHT SQUARE BRACKET (]) → emit `]`, stay
    /// - Anything else → emit `]]`, reconsume in CDATASection
    fn handle_cdata_section_end_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('>') => {
                self.state = State::Data;
                None // CDATA closed — no token emitted
            }
            Some(']') => {
                // Emit `]`, stay in CDATASectionEnd (handles `]]]`)
                Some(Token::Character(']'))
            }
            Some(_c) => {
                // Emit `]]` via pending, reconsume in CDATASection
                self.pending_tokens.push(Token::Character(']'));
                self.pending_tokens.push(Token::Character(']'));
                self.state = State::CDATASection;
                self.reconsume = true;
                None
            }
            None => {
                // TODO: record parse error (eof-in-cdata)
                self.pending_tokens.push(Token::Character(']'));
                self.pending_tokens.push(Token::Character(']'));
                self.state = State::CDATASection;
                self.reconsume = true;
                None
            }
        }
    }

    // ── Processing instruction states (§13.2.5.72–§13.2.5.76) ──────

    /// Convert the temporary buffer to a comment token (§13.2.5).
    ///
    /// Per the spec: "create a comment token whose data is the concatenation
    /// of '?' and the code points in the temporary buffer". Used when a
    /// processing instruction is found to have an invalid target and is
    /// instead treated as a bogus comment. The current character is reconsumed
    /// in the bogus comment state, so the buffer's content becomes the
    /// initial comment data and the reconsumed char is appended next.
    fn convert_temporary_buffer_to_comment(&mut self) {
        // The BogusComment state appends to `current_comment`, so seed it with
        // "?" + temporary buffer contents. The reconsumed character will be
        // appended by the bogus comment handler on the next step.
        self.current_comment.clear();
        self.current_comment.push('?');
        self.current_comment
            .push_str(&self.temporary_buffer.clone());
        self.temporary_buffer.clear();
        self.state = State::BogusComment;
        self.reconsume = true;
    }

    /// §13.2.5.72 Processing instruction open state
    ///
    /// Consume the next input character:
    /// - ASCII alpha or U+005F LOW LINE (_) → reconsume in PI target state
    /// - EOF → parse error (eof-in-processing-instruction), emit EOF
    /// - Anything else → parse error, convert temporary buffer to comment,
    ///   reconsume in bogus comment state
    fn handle_processing_instruction_open_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                // Reconsume in the processing instruction target state.
                self.state = State::ProcessingInstructionTarget;
                self.reconsume = true;
                None
            }
            None => {
                // TODO: parse error (eof-in-processing-instruction)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(_) => {
                // TODO: parse error (invalid-first-character-of-processing-instruction-target)
                self.convert_temporary_buffer_to_comment();
                None
            }
        }
    }

    /// §13.2.5.73 Processing instruction target state
    ///
    /// Accumulates the PI target name in the temporary buffer until a
    /// delimiter (whitespace, '?', or '>') is found. On delimiter, the target
    /// is checked against "xml"/"xml-stylesheet" (disallowed) and either
    /// converted to a comment or used to create a PI token.
    fn handle_processing_instruction_target_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t' | '\n' | '\u{000C}' | ' ' | '?' | '>') => {
                let target = self.temporary_buffer.clone();
                // Disallowed targets: "xml" or "xml-stylesheet" (ASCII
                // case-insensitive) → parse error, convert to comment.
                if target.eq_ignore_ascii_case("xml")
                    || target.eq_ignore_ascii_case("xml-stylesheet")
                {
                    // TODO: parse error (disallowed-processing-instruction-target)
                    self.convert_temporary_buffer_to_comment();
                    return None;
                }
                // Valid target: create a PI token with empty data, reconsume
                // in the after-target state.
                self.current_pi = Some((target, String::new()));
                self.temporary_buffer.clear();
                self.state = State::AfterProcessingInstructionTarget;
                self.reconsume = true;
                None
            }
            Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_' => {
                self.temporary_buffer.push(c);
                None
            }
            None => {
                // TODO: parse error (eof-in-processing-instruction)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(_) => {
                // TODO: parse error (invalid-processing-instruction-target)
                self.convert_temporary_buffer_to_comment();
                None
            }
        }
    }

    /// §13.2.5.74 After processing instruction target state
    ///
    /// Skips whitespace between the target and data. Any non-whitespace
    /// character is reconsumed in the data state.
    fn handle_after_processing_instruction_target_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t' | '\n' | '\u{000C}' | ' ') => {
                // Ignore the character.
                None
            }
            _ => {
                // Reconsume in the processing instruction data state.
                self.state = State::ProcessingInstructionData;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.75 Processing instruction data state
    ///
    /// Accumulates PI data until '?' (→ questionable state) or '>'
    /// (emit the PI token).
    fn handle_processing_instruction_data_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('?') => {
                self.state = State::ProcessingInstructionQuestionable;
                None
            }
            Some('>') => {
                self.state = State::Data;
                let (target, data) = self
                    .current_pi
                    .take()
                    .unwrap_or((String::new(), String::new()));
                Some(Token::ProcessingInstruction { target, data })
            }
            None => {
                // TODO: parse error (eof-in-processing-instruction)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(c) => {
                if let Some((_, data)) = self.current_pi.as_mut() {
                    data.push(c);
                }
                None
            }
        }
    }

    /// §13.2.5.76 Processing instruction questionable state
    ///
    /// After a '?' in data: '>' ends the PI, anything else appends '?' to
    /// data and reconsumes in the data state.
    fn handle_processing_instruction_questionable_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('>') => {
                self.state = State::Data;
                let (target, data) = self
                    .current_pi
                    .take()
                    .unwrap_or((String::new(), String::new()));
                Some(Token::ProcessingInstruction { target, data })
            }
            None => {
                // TODO: parse error (eof-in-processing-instruction)
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            _ => {
                if let Some((_, data)) = self.current_pi.as_mut() {
                    data.push('?');
                }
                self.state = State::ProcessingInstructionData;
                self.reconsume = true;
                None
            }
        }
    }

    // ── DOCTYPE helpers ───────────────────────────────────────────

    /// Emit the accumulated `current_doctype` as `Token::Doctype`，
    /// replace with a fresh default, and switch to Data state.
    fn emit_current_doctype(&mut self) -> Token {
        let doctype = std::mem::replace(
            &mut self.current_doctype,
            DoctypeToken {
                name: None,
                public_id: None,
                system_id: None,
                force_quirks: false,
            },
        );
        self.state = State::Data;
        Token::Doctype(doctype)
    }

    // ── DOCTYPE state handlers (§13.2.5.53–§13.2.5.68) ───────────

    /// §13.2.5.53 DOCTYPE state
    ///
    /// 入口：MarkupDeclarationOpen 识别 "DOCTYPE" 后。跳过空白，非空白 reconsume
    /// 到 BeforeDoctypeName。
    fn handle_doctype_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                // 跳过空白
                None
            }
            Some(_c) => {
                // 非空白字符 → reconsume 到 BeforeDoctypeName
                self.state = State::BeforeDoctypeName;
                self.reconsume = true;
                None
            }
            None => {
                // TODO: parse error (eof-in-doctype)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
        }
    }

    /// §13.2.5.54 Before DOCTYPE name state
    fn handle_before_doctype_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                // 跳过空白
                None
            }
            Some('\0') => {
                // TODO: parse error (unexpected-null-character)
                self.current_doctype.name = Some(String::from("\u{FFFD}"));
                self.state = State::DoctypeName;
                None
            }
            Some('>') => {
                // TODO: parse error (missing-doctype-name)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            None => {
                // TODO: parse error (eof-in-doctype)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(c) => {
                // 创建 DOCTYPE name（ASCII 大写→小写）
                let ch = if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c
                };
                self.current_doctype.name = Some(String::from(ch));
                self.state = State::DoctypeName;
                None
            }
        }
    }

    /// §13.2.5.55 DOCTYPE name state
    fn handle_doctype_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.state = State::AfterDoctypeName;
                None
            }
            Some('>') => {
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some('\0') => {
                // TODO: parse error (unexpected-null-character)
                if let Some(ref mut name) = self.current_doctype.name {
                    name.push('\u{FFFD}');
                }
                None
            }
            None => {
                // TODO: parse error (eof-in-doctype)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(c) => {
                let ch = if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c
                };
                if let Some(ref mut name) = self.current_doctype.name {
                    name.push(ch);
                }
                None
            }
        }
    }

    /// §13.2.5.56 After DOCTYPE name state
    fn handle_after_doctype_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                None // stay in AfterDoctypeName
            }
            Some('>') => {
                let token = self.emit_current_doctype();
                Some(token)
            }
            None => {
                // TODO: parse error (eof-in-doctype)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(_c) => {
                // 尝试匹配 "PUBLIC" 或 "SYSTEM"（大小写不敏感，6 字符）
                // 注意：_c 已经被 next_char() 消费，需从 pos-1 开始比较
                if self.pos + 5 <= self.input.len() {
                    let start = self.pos - 1;
                    let slice: String = self.input[start..start + 6].iter().collect();
                    if slice.eq_ignore_ascii_case("PUBLIC") {
                        self.pos = start + 6;
                        self.state = State::AfterDoctypePublicKeyword;
                        return None;
                    }
                    if slice.eq_ignore_ascii_case("SYSTEM") {
                        self.pos = start + 6;
                        self.state = State::AfterDoctypeSystemKeyword;
                        return None;
                    }
                }
                // 不匹配 → BogusDoctype（force_quirks=true）
                self.current_doctype.force_quirks = true;
                self.state = State::BogusDoctype;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.68 Bogus DOCTYPE state
    fn handle_bogus_doctype_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('>') => {
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some('\0') => {
                // TODO: parse error (unexpected-null-character)
                // 忽略字符，不追加
                None
            }
            None => {
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(_) => {
                // 忽略其他字符
                None
            }
        }
    }

    // ── DOCTYPE PUBLIC 标识符路径 (§13.2.5.57–§13.2.5.62) ───────

    /// §13.2.5.57 After DOCTYPE public keyword state
    fn handle_after_doctype_public_keyword_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.state = State::BeforeDoctypePublicId;
                None
            }
            Some('"') => {
                // §13.2.5.57: Parse error (missing-whitespace-after-doctype-public-keyword).
                // Set public_id to empty string, switch to DOCTYPE public ID
                // (double-quoted) state. Do NOT set force_quirks or emit.
                // Verified by html5lib: `<!DOCTYPE a PUBLIC"` → public_id="".
                // TODO: parse error (missing-whitespace-after-doctype-public-keyword)
                self.current_doctype.public_id = Some(String::new());
                self.state = State::DoctypePublicIdDoubleQuoted;
                None
            }
            Some('\'') => {
                // §13.2.5.57: Same as `"` but single-quoted.
                // TODO: parse error (missing-whitespace-after-doctype-public-keyword)
                self.current_doctype.public_id = Some(String::new());
                self.state = State::DoctypePublicIdSingleQuoted;
                None
            }
            Some('>') => {
                // TODO: parse error (missing-doctype-public-identifier)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            None => {
                // TODO: parse error (eof-in-doctype)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(_) => {
                self.current_doctype.force_quirks = true;
                self.state = State::BogusDoctype;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.58 Before DOCTYPE public identifier state
    fn handle_before_doctype_public_id_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                None // skip whitespace
            }
            Some('"') => {
                self.current_doctype.public_id = Some(String::new());
                self.state = State::DoctypePublicIdDoubleQuoted;
                None
            }
            Some('\'') => {
                self.current_doctype.public_id = Some(String::new());
                self.state = State::DoctypePublicIdSingleQuoted;
                None
            }
            Some('>') | None => {
                // TODO: parse error
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(_) => {
                // TODO: parse error
                self.current_doctype.force_quirks = true;
                self.state = State::BogusDoctype;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.59 DOCTYPE public identifier (double-quoted) state
    fn handle_doctype_public_id_double_quoted_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('"') => {
                self.state = State::AfterDoctypePublicId;
                None
            }
            Some('>') | None => {
                // TODO: parse error (eof-in-doctype / abrupt-doctype-public-identifier)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some('\0') => {
                // TODO: parse error (unexpected-null-character)
                if let Some(ref mut id) = self.current_doctype.public_id {
                    id.push('\u{FFFD}');
                }
                None
            }
            Some(c) => {
                if let Some(ref mut id) = self.current_doctype.public_id {
                    id.push(c);
                }
                None
            }
        }
    }

    /// §13.2.5.60 DOCTYPE public identifier (single-quoted) state
    fn handle_doctype_public_id_single_quoted_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\'') => {
                self.state = State::AfterDoctypePublicId;
                None
            }
            Some('>') | None => {
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some('\0') => {
                if let Some(ref mut id) = self.current_doctype.public_id {
                    id.push('\u{FFFD}');
                }
                None
            }
            Some(c) => {
                if let Some(ref mut id) = self.current_doctype.public_id {
                    id.push(c);
                }
                None
            }
        }
    }

    /// §13.2.5.61 After DOCTYPE public identifier state
    fn handle_after_doctype_public_id_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.state = State::BetweenDoctypePublicAndSystemIds;
                None
            }
            Some('>') => {
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some('"') => {
                // §13.2.5.61: Parse error. Set system identifier to empty string.
                // Switch to DOCTYPE system identifier (double-quoted) state.
                self.current_doctype.system_id = Some(String::new());
                self.state = State::DoctypeSystemIdDoubleQuoted;
                None
            }
            Some('\'') => {
                // §13.2.5.61: Parse error. Set system identifier to empty string.
                // Switch to DOCTYPE system identifier (single-quoted) state.
                self.current_doctype.system_id = Some(String::new());
                self.state = State::DoctypeSystemIdSingleQuoted;
                None
            }
            None => {
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(_) => {
                self.current_doctype.force_quirks = true;
                self.state = State::BogusDoctype;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.62 Between DOCTYPE public and system identifiers state
    fn handle_between_doctype_public_and_system_ids_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => None,
            Some('>') => {
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some('"') => {
                self.current_doctype.system_id = Some(String::new());
                self.state = State::DoctypeSystemIdDoubleQuoted;
                None
            }
            Some('\'') => {
                self.current_doctype.system_id = Some(String::new());
                self.state = State::DoctypeSystemIdSingleQuoted;
                None
            }
            None => {
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(_c) => {
                // §13.2.5.62: Parse error. Set force-quirks. Switch to bogus DOCTYPE state.
                // (No SYSTEM keyword matching in this state — that's §13.2.5.55/57's job.)
                self.current_doctype.force_quirks = true;
                self.state = State::BogusDoctype;
                self.reconsume = true;
                None
            }
        }
    }

    // ── DOCTYPE SYSTEM 标识符路径 (§13.2.5.63–§13.2.5.67) ───────

    /// §13.2.5.63 After DOCTYPE system keyword state
    fn handle_after_doctype_system_keyword_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.state = State::BeforeDoctypeSystemId;
                None
            }
            Some('"') => {
                // §13.2.5.63: Parse error (missing-whitespace-after-doctype-system-keyword).
                // Set system_id to empty string, switch to DOCTYPE system ID
                // (double-quoted) state. Do NOT set force_quirks or emit.
                // Verified by html5lib: `<!DOCTYPE a SYSTEM"` → system_id="".
                // TODO: parse error (missing-whitespace-after-doctype-system-keyword)
                self.current_doctype.system_id = Some(String::new());
                self.state = State::DoctypeSystemIdDoubleQuoted;
                None
            }
            Some('\'') => {
                // §13.2.5.63: Same as `"` but single-quoted.
                // TODO: parse error (missing-whitespace-after-doctype-system-keyword)
                self.current_doctype.system_id = Some(String::new());
                self.state = State::DoctypeSystemIdSingleQuoted;
                None
            }
            Some('>') => {
                // TODO: parse error (missing-doctype-system-identifier)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            None => {
                // TODO: parse error (eof-in-doctype)
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(_) => {
                self.current_doctype.force_quirks = true;
                self.state = State::BogusDoctype;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.64 Before DOCTYPE system identifier state
    fn handle_before_doctype_system_id_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => None,
            Some('"') => {
                self.current_doctype.system_id = Some(String::new());
                self.state = State::DoctypeSystemIdDoubleQuoted;
                None
            }
            Some('\'') => {
                self.current_doctype.system_id = Some(String::new());
                self.state = State::DoctypeSystemIdSingleQuoted;
                None
            }
            Some('>') | None => {
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(_) => {
                self.current_doctype.force_quirks = true;
                self.state = State::BogusDoctype;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.65 DOCTYPE system identifier (double-quoted) state
    fn handle_doctype_system_id_double_quoted_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('"') => {
                self.state = State::AfterDoctypeSystemId;
                None
            }
            Some('>') | None => {
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some('\0') => {
                if let Some(ref mut id) = self.current_doctype.system_id {
                    id.push('\u{FFFD}');
                }
                None
            }
            Some(c) => {
                if let Some(ref mut id) = self.current_doctype.system_id {
                    id.push(c);
                }
                None
            }
        }
    }

    /// §13.2.5.66 DOCTYPE system identifier (single-quoted) state
    fn handle_doctype_system_id_single_quoted_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\'') => {
                self.state = State::AfterDoctypeSystemId;
                None
            }
            Some('>') | None => {
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some('\0') => {
                if let Some(ref mut id) = self.current_doctype.system_id {
                    id.push('\u{FFFD}');
                }
                None
            }
            Some(c) => {
                if let Some(ref mut id) = self.current_doctype.system_id {
                    id.push(c);
                }
                None
            }
        }
    }

    /// §13.2.5.67 After DOCTYPE system identifier state
    fn handle_after_doctype_system_id_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => None,
            Some('>') => {
                let token = self.emit_current_doctype();
                Some(token)
            }
            None => {
                self.current_doctype.force_quirks = true;
                let token = self.emit_current_doctype();
                Some(token)
            }
            Some(_) => {
                // TODO: parse error (unexpected-character-after-doctype-system-identifier)
                // §13.2.5.67: Reconsume in the bogus DOCTYPE state.
                self.state = State::BogusDoctype;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.41 Bogus comment state
    ///
    /// 累积字符直到 `>`，作为 `Token::Comment` 发出。
    /// 入口：TagOpen 遇到 `?`，或 MarkupDeclarationOpen 无法匹配。
    fn handle_bogus_comment_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('>') => {
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
            Some('\0') => {
                // TODO: parse error (unexpected-null-character)
                self.current_comment.push('\u{FFFD}');
                None
            }
            None => {
                // 发出 Comment，切换到 Data 让 Data 状态在下一次调用时发出 EOF
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
            Some(c) => {
                self.current_comment.push(c);
                None
            }
        }
    }

    // ── Comment state handlers (§13.2.5.43–§13.2.5.45) ───────────

    /// §13.2.5.43 Comment start state
    ///
    /// 进入时机：`<!--` 已消费，当前字符为注释内容第一个字符。
    fn handle_comment_start_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                self.state = State::CommentStartDash;
                None
            }
            Some('>') => {
                // 空注释
                self.current_comment.clear();
                self.state = State::Data;
                Some(Token::Comment(String::new()))
            }
            Some('<') => {
                // §13.2.5.43: Append `<` to comment data.
                // Switch to CommentLessThanSign.
                self.current_comment.push('<');
                self.state = State::CommentLessThanSign;
                None
            }
            Some('\0') => {
                // TODO: parse error (unexpected-null-character)
                self.current_comment.push('\u{FFFD}');
                self.state = State::Comment;
                None
            }
            None => {
                // TODO: parse error (eof-in-comment)
                self.current_comment.clear();
                self.state = State::Data;
                Some(Token::Comment(String::new()))
            }
            Some(c) => {
                self.current_comment.push(c);
                self.state = State::Comment;
                None
            }
        }
    }

    /// §13.2.5.44 Comment start dash state
    fn handle_comment_start_dash_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                self.state = State::CommentEnd;
                None
            }
            Some('>') => {
                // TODO: parse error (abrupt-closing-of-empty-comment)
                self.current_comment.clear();
                self.state = State::Data;
                Some(Token::Comment(String::new()))
            }
            None => {
                // TODO: parse error (eof-in-comment)
                self.current_comment.clear();
                self.state = State::Data;
                Some(Token::Comment(String::new()))
            }
            // §13.2.5.44: `<` has no independent branch — falls through to
            // anything else (append `-`, reconsume in Comment where `<` is
            // appended and switches to CommentLessThanSign). Verified by
            // html5lib: `<!---<` → Comment "-<".
            Some(_c) => {
                self.current_comment.push('-');
                self.reconsume = true;
                self.state = State::Comment;
                None
            }
        }
    }

    /// §13.2.5.45 Comment state
    fn handle_comment_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('<') => {
                // §13.2.5.45: Append the current input character to the comment
                // token's data. Switch to the comment less-than sign state.
                self.current_comment.push('<');
                self.state = State::CommentLessThanSign;
                None
            }
            Some('-') => {
                self.state = State::CommentEndDash;
                None
            }
            Some('\0') => {
                // TODO: parse error (unexpected-null-character)
                self.current_comment.push('\u{FFFD}');
                None
            }
            None => {
                // TODO: parse error (eof-in-comment)
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
            Some(c) => {
                self.current_comment.push(c);
                None
            }
        }
    }

    // ── Comment < 系列 (§13.2.5.46–§13.2.5.49) ─────────────────

    /// §13.2.5.46 Comment less-than sign state
    fn handle_comment_less_than_sign_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('!') => {
                // §13.2.5.46: Append `!` to comment data (`<` was already
                // appended by the state that switched here). Switch to
                // CommentLessThanSignBang. Verified by html5lib:
                // `<!-- <!--` → Comment " <!" (EOF exposes the missing `!`).
                self.current_comment.push('!');
                self.state = State::CommentLessThanSignBang;
                None
            }
            Some('<') => {
                self.current_comment.push('<');
                None
            }
            Some(_c) => {
                self.reconsume = true;
                self.state = State::Comment;
                None
            }
            None => {
                // TODO: parse error (eof-in-comment)
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
        }
    }

    /// §13.2.5.47 Comment less-than sign bang state
    fn handle_comment_less_than_sign_bang_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                self.state = State::CommentLessThanSignBangDash;
                None
            }
            Some(_c) => {
                // §13.2.5.47: `!` was already appended by §13.2.5.46 `!`
                // branch. Just reconsume in Comment. Verified by html5lib:
                // `<!-- <!test-->` → Comment " <!test" (only one `!`).
                self.reconsume = true;
                self.state = State::Comment;
                None
            }
            None => {
                // TODO: parse error (eof-in-comment)
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
        }
    }

    /// §13.2.5.48 Comment less-than sign bang dash state
    fn handle_comment_less_than_sign_bang_dash_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                self.state = State::CommentLessThanSignBangDashDash;
                None
            }
            Some(_c) => {
                // §13.2.5.48: `!` was appended by §13.2.5.46, `-` consumed
                // by §13.2.5.47 `-` branch needs catching up. Append `-`,
                // reconsume in Comment. Verified by html5lib:
                // `<!-- <!-test-->` → Comment " <!-test".
                self.current_comment.push('-');
                self.reconsume = true;
                self.state = State::Comment;
                None
            }
            None => {
                // TODO: parse error (eof-in-comment)
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
        }
    }

    /// §13.2.5.49 Comment less-than sign bang dash dash state
    fn handle_comment_less_than_sign_bang_dash_dash_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('>') => {
                // §13.2.5.49: Parse error. Switch to Data. Emit comment.
                // No append — `!` was already appended by §13.2.5.46, and
                // the two `-`s consumed to reach this state are discarded
                // (consistent with `>` closing the comment). Verified by
                // html5lib: `<!--<!-->` → Comment "<!".
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
            Some(_c) => {
                // §13.2.5.49: "Anything else: This is a nested-comment
                // parse error. Reconsume in the comment end state."
                //
                // No append here: the two `-`s consumed by §13.2.5.47/.48
                // `-` branches (to reach this state) are caught up by the
                // comment END state's own anything-else arm, which appends
                // `--` (its catch-up for entering CommentEnd). Appending
                // `--` here too would double the catch-up.
                //
                // Why CommentEnd (not Comment): the nested-comment feature
                // exists so `<!--<!--` can be closed by a following `>`
                // (CommentEnd `>` emits; CommentEnd `!`→CommentEndBang `>`
                // emits). Reconsuming in Comment would append `>` literally
                // and the comment would never close, e.g. `<!--<!--!>`
                // must close at `>` rather than swallowing it as content.
                //
                // html5lib coverage: every test1.test case reaching this
                // branch reconsumes an ordinary char (`t`), so CommentEnd
                // appends `--` (catch-up) then reconsumes `t` in Comment —
                // yielding `<!-- <!--test-->` → `Comment " <!--test"`. The
                // `!`/`-` differentiating paths are untested upstream.
                self.reconsume = true;
                self.state = State::CommentEnd;
                None
            }
            None => {
                // §13.2.5.49: Parse error (eof-in-comment). Switch to Data.
                // Emit comment. No append (same as `>` branch).
                // Verified by html5lib: `<!-- <!--` → Comment " <!".
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
        }
    }

    // ── Comment end 系列 (§13.2.5.50–§13.2.5.52) ────────────────

    /// §13.2.5.50 Comment end dash state
    fn handle_comment_end_dash_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                self.state = State::CommentEnd;
                None
            }
            Some(_c) => {
                self.current_comment.push('-');
                self.reconsume = true;
                self.state = State::Comment;
                None
            }
            None => {
                // TODO: parse error (eof-in-comment)
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
        }
    }

    /// §13.2.5.51 Comment end state
    fn handle_comment_end_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('>') => {
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
            Some('!') => {
                self.state = State::CommentEndBang;
                None
            }
            Some('-') => {
                // 吃掉多余的 '-'
                self.current_comment.push('-');
                None
            }
            Some(_c) => {
                self.current_comment.push_str("--");
                self.reconsume = true;
                self.state = State::Comment;
                None
            }
            None => {
                // TODO: parse error (eof-in-comment)
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
        }
    }

    /// §13.2.5.52 Comment end bang state
    fn handle_comment_end_bang_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('-') => {
                // §13.2.5.52: Append "--!" to comment. Switch to comment end dash state.
                self.current_comment.push_str("--!");
                self.state = State::CommentEndDash;
                None
            }
            Some('>') => {
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
            Some(_c) => {
                self.current_comment.push_str("--!");
                self.reconsume = true;
                self.state = State::Comment;
                None
            }
            None => {
                // TODO: parse error (eof-in-comment)
                let comment = std::mem::take(&mut self.current_comment);
                self.state = State::Data;
                Some(Token::Comment(comment))
            }
        }
    }

    // ── Attribute state handlers (§13.2.5.32–§13.2.5.39) ────────

    /// §13.2.5.32 Before attribute name state
    fn handle_before_attribute_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.state = State::BeforeAttributeName;
                None
            }
            Some('/') => {
                self.state = State::SelfClosingStartTag;
                None
            }
            Some('>') => {
                let tag = self.current_tag.take().unwrap();
                self.state = State::Data;
                Some(Token::Tag(tag))
            }
            None => {
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some('=') => {
                // §13.2.5.32: Parse error (unexpected-equals-sign-before-attribute-name).
                // Start a new attribute with name `=`, value empty. Switch to
                // AttributeName (so a following `=` ends the name and opens value).
                // Verified by html5lib: `<z =>` → attr {"=": ""}.
                // TODO: parse error (unexpected-equals-sign-before-attribute-name)
                self.current_attr_name.push('=');
                self.state = State::AttributeName;
                None
            }
            Some(_c) => {
                self.state = State::AttributeName;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.33 Attribute name state
    fn handle_attribute_name_state(&mut self) -> Option<Token> {
        let ch = self.next_char();
        match ch {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.state = State::AfterAttributeName;
                None
            }
            Some('/') => {
                self.emit_current_attribute();
                self.state = State::SelfClosingStartTag;
                None
            }
            Some('=') => {
                self.state = State::BeforeAttributeValue;
                None
            }
            Some('>') => {
                self.emit_current_attribute();
                let tag = self.current_tag.take().unwrap();
                self.state = State::Data;
                Some(Token::Tag(tag))
            }
            Some('\0') => {
                self.current_attr_name.push('\u{FFFD}');
                self.state = State::AttributeName;
                None
            }
            None => {
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(c) => {
                // §13.2.5.33: U+0022/U+0027/U+003C → parse error, fall through to append.
                // ASCII upper-alpha → append lowercase; anything else → append as-is.
                if c.is_ascii_uppercase() {
                    self.current_attr_name.push(c.to_ascii_lowercase());
                } else {
                    self.current_attr_name.push(c);
                }
                self.state = State::AttributeName;
                None
            }
        }
    }

    /// §13.2.5.34 After attribute name state
    fn handle_after_attribute_name_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.state = State::AfterAttributeName;
                None
            }
            Some('/') => {
                self.emit_current_attribute();
                self.state = State::SelfClosingStartTag;
                None
            }
            Some('=') => {
                self.state = State::BeforeAttributeValue;
                None
            }
            Some('>') => {
                self.emit_current_attribute();
                let tag = self.current_tag.take().unwrap();
                self.state = State::Data;
                Some(Token::Tag(tag))
            }
            None => {
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(_c) => {
                // §13.2.5.34: Parse error. Start a new attribute in the current tag token.
                // Set name to current char, value to empty. Switch to AttributeName.
                self.emit_current_attribute();
                self.state = State::AttributeName;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.35 Before attribute value state
    fn handle_before_attribute_value_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.state = State::BeforeAttributeValue;
                None
            }
            Some('"') => {
                self.current_attr_value.clear();
                self.state = State::AttributeValueDoubleQuoted;
                None
            }
            Some('\'') => {
                self.current_attr_value.clear();
                self.state = State::AttributeValueSingleQuoted;
                None
            }
            Some('>') => {
                // missing-attribute-value parse error: emit attr with empty value
                self.emit_current_attribute();
                let tag = self.current_tag.take().unwrap();
                self.state = State::Data;
                Some(Token::Tag(tag))
            }
            None => {
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(_c) => {
                self.current_attr_value.clear();
                self.state = State::AttributeValueUnquoted;
                self.reconsume = true;
                None
            }
        }
    }

    /// §13.2.5.36 Attribute value (double-quoted) state
    fn handle_attribute_value_double_quoted_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('"') => {
                self.state = State::AfterAttributeValueQuoted;
                None
            }
            Some('&') => {
                self.return_state = Some(State::AttributeValueDoubleQuoted);
                self.state = State::CharacterReference;
                None
            }
            Some('\0') => {
                // unexpected-null-character parse error
                self.current_attr_value.push('\u{FFFD}');
                self.state = State::AttributeValueDoubleQuoted;
                None
            }
            None => {
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(c) => {
                self.current_attr_value.push(c);
                self.state = State::AttributeValueDoubleQuoted;
                None
            }
        }
    }

    /// §13.2.5.37 Attribute value (single-quoted) state
    fn handle_attribute_value_single_quoted_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\'') => {
                self.state = State::AfterAttributeValueQuoted;
                None
            }
            Some('&') => {
                self.return_state = Some(State::AttributeValueSingleQuoted);
                self.state = State::CharacterReference;
                None
            }
            Some('\0') => {
                self.current_attr_value.push('\u{FFFD}');
                self.state = State::AttributeValueSingleQuoted;
                None
            }
            None => {
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(c) => {
                self.current_attr_value.push(c);
                self.state = State::AttributeValueSingleQuoted;
                None
            }
        }
    }

    /// §13.2.5.38 Attribute value (unquoted) state
    fn handle_attribute_value_unquoted_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.emit_current_attribute();
                self.state = State::BeforeAttributeName;
                None
            }
            Some('&') => {
                self.return_state = Some(State::AttributeValueUnquoted);
                self.state = State::CharacterReference;
                None
            }
            Some('>') => {
                self.emit_current_attribute();
                let tag = self.current_tag.take().unwrap();
                self.state = State::Data;
                Some(Token::Tag(tag))
            }
            Some('\0') => {
                self.current_attr_value.push('\u{FFFD}');
                self.state = State::AttributeValueUnquoted;
                None
            }
            None => {
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(c) => {
                self.current_attr_value.push(c);
                self.state = State::AttributeValueUnquoted;
                None
            }
        }
    }

    /// §13.2.5.39 After attribute value (quoted) state
    fn handle_after_attribute_value_quoted_state(&mut self) -> Option<Token> {
        match self.next_char() {
            Some('\t') | Some('\n') | Some('\u{000C}') | Some(' ') => {
                self.emit_current_attribute();
                self.state = State::BeforeAttributeName;
                None
            }
            Some('/') => {
                self.emit_current_attribute();
                self.state = State::SelfClosingStartTag;
                None
            }
            Some('>') => {
                self.emit_current_attribute();
                let tag = self.current_tag.take().unwrap();
                self.state = State::Data;
                Some(Token::Tag(tag))
            }
            None => {
                self.current_tag = None;
                self.eof_emitted = true;
                Some(Token::EOF)
            }
            Some(_c) => {
                // Unexpected-character-after-quoted-attribute-value parse error
                self.emit_current_attribute();
                self.state = State::BeforeAttributeName;
                self.reconsume = true;
                None
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_state_emits_character_for_letter() {
        let mut t = HtmlTokenizer::new("a");
        let token = t.next_token();
        assert_eq!(token, Some(Token::Character('a')));
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn data_state_switches_to_tag_open_on_less_than() {
        let mut t = HtmlTokenizer::new("<");
        let token = t.step();
        assert_eq!(token, None); // no token emitted — state changed
        assert_eq!(t.state(), State::TagOpen);
    }

    #[test]
    fn data_state_switches_to_character_reference_on_ampersand() {
        let mut t = HtmlTokenizer::new("&");
        let token = t.step();
        assert_eq!(token, None); // no token emitted — state changed
        assert_eq!(t.state(), State::CharacterReference);
    }

    #[test]
    fn data_state_emits_eof_on_empty_input() {
        let mut t = HtmlTokenizer::new("");
        let token = t.next_token();
        assert_eq!(token, Some(Token::EOF));

        // Subsequent call → None (stream exhausted)
        let token2 = t.next_token();
        assert_eq!(token2, None);
    }

    #[test]
    fn data_state_emits_eof_after_last_char() {
        let mut t = HtmlTokenizer::new("x");
        let first = t.next_token();
        assert_eq!(first, Some(Token::Character('x')));
        assert_eq!(t.state(), State::Data);

        let second = t.next_token();
        assert_eq!(second, Some(Token::EOF));
    }

    #[test]
    fn data_state_handles_null_character() {
        let mut t = HtmlTokenizer::new("\0");
        let token = t.next_token();
        // §13.2.5.1: U+0000 NULL emits the current input character as a char token
        assert_eq!(token, Some(Token::Character('\0')));
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn data_state_emits_multiple_characters() {
        let mut t = HtmlTokenizer::new("abc");
        assert_eq!(t.next_token(), Some(Token::Character('a')));
        assert_eq!(t.next_token(), Some(Token::Character('b')));
        assert_eq!(t.next_token(), Some(Token::Character('c')));
        assert_eq!(t.next_token(), Some(Token::EOF));
        assert_eq!(t.step(), None); // stream done
    }

    // ── EOF reconsume regression tests ─────────────────────────────
    //
    // The root cause of the html5lib harness OOM (20 GB) was that any EOF
    // arm which set `reconsume = true` replayed the *last real code point*
    // instead of EOF, sending the state machine into an infinite
    // character-emitting loop. These cases guard the fix: `&` (or other
    // EOF-reconsume triggers) at the end of input must terminate cleanly.

    #[test]
    fn data_state_ampersand_at_eof_emits_amp_then_eof() {
        // `&` alone in Data: '&' → CharacterReference; EOF → flush '&' and
        // reconsume EOF in Data → EOF token.
        let mut t = HtmlTokenizer::new("&");
        assert_eq!(t.next_token(), Some(Token::Character('&')));
        assert_eq!(t.next_token(), Some(Token::EOF));
        assert_eq!(t.next_token(), None);
    }

    #[test]
    fn data_state_double_ampersand_at_eof() {
        // `&&`: first '&' → CharacterReference; second '&' → flush first '&'
        // and reconsume; Data sees '&' → CharacterReference; EOF → flush
        // second '&'; EOF → EOF token.
        let mut t = HtmlTokenizer::new("&&");
        assert_eq!(t.next_token(), Some(Token::Character('&')));
        assert_eq!(t.next_token(), Some(Token::Character('&')));
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    #[test]
    fn data_state_ampersand_hash_at_eof_emits_hash_amp_then_eof() {
        // `&#` at EOF: '&' → CharacterReference → '#' → NumericCharacterReference;
        // EOF → flush '&#' (via pending tokens) and reconsume EOF in Data → EOF.
        let mut t = HtmlTokenizer::new("&#");
        assert_eq!(t.next_token(), Some(Token::Character('&')));
        assert_eq!(t.next_token(), Some(Token::Character('#')));
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    #[test]
    fn data_state_ampersand_hash_x_at_eof() {
        // `&#x` at EOF flushes '&#x' then emits EOF.
        let mut t = HtmlTokenizer::new("&#x");
        assert_eq!(t.next_token(), Some(Token::Character('&')));
        assert_eq!(t.next_token(), Some(Token::Character('#')));
        assert_eq!(t.next_token(), Some(Token::Character('x')));
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    #[test]
    fn rcdata_ampersand_at_eof_terminates() {
        let mut t = enter_content_model("&", State::RCDATA, Some("title"));
        assert_eq!(t.next_token(), Some(Token::Character('&')));
        assert_eq!(t.next_token(), Some(Token::EOF));
        assert_eq!(t.next_token(), None);
    }

    #[test]
    fn doctype_public_id_ampersand_then_eof_terminates() {
        // §13.2.5.57 After DOCTYPE public keyword state: `"` (without
        // preceding whitespace) is a parse error but still opens the public
        // identifier (double-quoted) state — it does NOT set force_quirks or
        // emit immediately. `<!DOCTYPE a PUBLIC"&`: `"` opens public_id,
        // `&` is appended to the id, EOF sets force_quirks and emits the
        // doctype. This also guards against the original OOM loop bug.
        // Verified against html5lib: `<!DOCTYPE a PUBLIC"&` → public_id="&".
        let mut t = HtmlTokenizer::new("<!DOCTYPE a PUBLIC\"&");
        let tok = t.next_token();
        match tok {
            Some(Token::Doctype(d)) => {
                assert!(d.force_quirks);
                assert_eq!(d.name.as_deref(), Some("a"));
                assert_eq!(d.public_id.as_deref(), Some("&"));
            }
            other => panic!("expected Doctype, got {other:?}"),
        }
        assert_eq!(t.next_token(), Some(Token::EOF));
        assert_eq!(t.next_token(), None);
    }

    #[test]
    fn doctype_public_id_ampersand_inside_then_eof_terminates() {
        // `<!DOCTYPE a PUBLIC "x&` — '&' is now inside the public id string
        // (a literal char), and EOF forces quirks.
        let mut t = HtmlTokenizer::new("<!DOCTYPE a PUBLIC \"x&");
        let tok = t.next_token();
        match tok {
            Some(Token::Doctype(d)) => {
                assert!(d.force_quirks);
                assert_eq!(d.name.as_deref(), Some("a"));
                assert_eq!(d.public_id.as_deref(), Some("x&"));
            }
            other => panic!("expected Doctype with public_id=\"x&\", got {other:?}"),
        }
        assert_eq!(t.next_token(), Some(Token::EOF));
        assert_eq!(t.next_token(), None);
    }

    // ── TagName tests (§13.2.5.8) ──────────────────────────────

    /// Helper: create a tokenizer in TagName state with a start tag already built.
    fn enter_tag_name(input: &str) -> HtmlTokenizer {
        let mut t = HtmlTokenizer::new(input);
        // Data → TagOpen (on `<`)
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::TagOpen);
        // TagOpen → creates start tag + TagName (on first alpha)
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::TagName);
        // Verify the tag name was initialized with the first char
        // (we can't check current_tag directly since it's private, but we
        // trust TagOpen's existing test covers this)
        t
    }

    #[test]
    fn tag_name_emits_start_tag_on_greater_than() {
        // `<a>` should emit a start tag token with name "a"
        let mut t = enter_tag_name("<a>");
        let token = t.next_token();
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "a".into(),
                attrs: Vec::new(),
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn tag_name_emits_tag_with_lowercased_name() {
        // `<DIV>` → tag name should be "div"
        let mut t = HtmlTokenizer::new("<DIV>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.state(), State::TagOpen);
        assert_eq!(t.step(), None); // TagOpen → TagName, name="d"
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // 'I' → append 'i'
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // 'V' → append 'v'
        assert_eq!(t.state(), State::TagName);
        // '>' → emit tag
        let token = t.next_token();
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "div".into(),
                attrs: Vec::new(),
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn tag_name_appends_lowercased_uppercase_chars() {
        // `<AbC>` → tag name should be "abc"
        let mut t = HtmlTokenizer::new("<AbC>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.state(), State::TagOpen);
        assert_eq!(t.step(), None); // TagOpen → TagName, name="a"
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // 'b' → append, still TagName
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // 'C' → append 'c', still TagName
        assert_eq!(t.state(), State::TagName);
        // '>' → emit tag
        let token = t.next_token();
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "abc".into(),
                attrs: Vec::new(),
                self_closing: false,
            }))
        );
    }

    #[test]
    fn tag_name_switches_to_self_closing_on_solidus() {
        // `<br/>` → after "br", '/' switches to SelfClosingStartTag
        let mut t = HtmlTokenizer::new("<br/>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.state(), State::TagOpen);
        assert_eq!(t.step(), None); // TagOpen → TagName, name="b"
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // 'r' appended, still TagName
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // '/' → SelfClosingStartTag
        assert_eq!(t.state(), State::SelfClosingStartTag);
        // Now '/' is consumed, next char is '>'
        let token = t.next_token();
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "br".into(),
                attrs: Vec::new(),
                self_closing: true,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    // ── EndTagOpen tests (§13.2.5.7) ─────────────────────────────

    #[test]
    fn end_tag_open_creates_end_tag_on_alpha() {
        // `</div>`: `<` → TagOpen, `/` → EndTagOpen, `d` → creates end tag + TagName
        let mut t = HtmlTokenizer::new("</div>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.state(), State::TagOpen);
        assert_eq!(t.step(), None); // TagOpen → EndTagOpen
        assert_eq!(t.state(), State::EndTagOpen);
        assert_eq!(t.step(), None); // EndTagOpen → TagName, name="d"
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // 'i' → append
        assert_eq!(t.step(), None); // 'v' → append
        let token = t.next_token(); // '>' → emit
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::End,
                name: "div".into(),
                attrs: Vec::new(),
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn end_tag_open_lowercases_tag_name() {
        let mut t = HtmlTokenizer::new("</DIV>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → EndTagOpen
        assert_eq!(t.step(), None); // EndTagOpen → TagName, name="d"
        assert_eq!(t.step(), None); // 'i'
        assert_eq!(t.step(), None); // 'v'
        let token = t.next_token(); // '>' → emit
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::End,
                name: "div".into(),
                attrs: Vec::new(),
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn end_tag_open_non_alpha_switches_to_data() {
        // `</>` → not alpha, switch to Data, don't emit anything
        let mut t = HtmlTokenizer::new("</>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → EndTagOpen
                                    // EndTagOpen sees `>`: not alpha → switch to Data, no emit. Under the
                                    // public contract, next_token() loops until a token drops out — the
                                    // following EOF is that token.
        assert_eq!(t.next_token(), Some(Token::EOF));
        assert_eq!(t.state(), State::Data);
        assert_eq!(t.next_token(), None); // stream done
    }

    #[test]
    fn end_tag_open_non_alpha_emits_lt_and_solidus() {
        // §13.2.5.7 End tag open state — anything else branch.
        // `</5` is not alpha, not `>`, not EOF: create an empty comment,
        // switch to BogusComment, reconsume `5`. `5` is appended to the
        // comment, then EOF emits the comment. Verified by html5lib:
        // `</\t` (EOF) → Comment "\t" (same bogus-comment path).
        let mut t = HtmlTokenizer::new("</5");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → EndTagOpen
                                    // EndTagOpen sees `5`: bogus comment, reconsume
        assert_eq!(t.next_token(), Some(Token::Comment("5".to_string())));
        assert_eq!(t.next_token(), Some(Token::EOF));
        assert_eq!(t.state(), State::Data);
        assert_eq!(t.next_token(), None); // stream done
    }

    #[test]
    fn end_tag_open_eof_emits_lt_then_eof() {
        // `</` + EOF → §13.2.5.7 EOF branch: emit `<` and `/` character
        // tokens, reconsume EOF in Data (which emits EOF next).
        let mut t = HtmlTokenizer::new("</");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → EndTagOpen
        assert_eq!(t.next_token(), Some(Token::Character('<')));
        assert_eq!(t.next_token(), Some(Token::Character('/')));
        assert_eq!(t.state(), State::Data);
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    // ── End-to-end integration tests ─────────────────────────────

    #[test]
    fn e2e_simple_open_close_tag() {
        // `<p>hello</p>` → start tag, chars, end tag, EOF
        let mut t = HtmlTokenizer::new("<p>hello</p>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName, name="p"
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "p".into(),
                attrs: Vec::new(),
                self_closing: false
            }))
        ); // '>' → emit
        assert_eq!(t.next_token(), Some(Token::Character('h')));
        assert_eq!(t.next_token(), Some(Token::Character('e')));
        assert_eq!(t.next_token(), Some(Token::Character('l')));
        assert_eq!(t.next_token(), Some(Token::Character('l')));
        assert_eq!(t.next_token(), Some(Token::Character('o')));
        assert_eq!(t.step(), None); // '<' → TagOpen
        assert_eq!(t.step(), None); // '/' → EndTagOpen
        assert_eq!(t.step(), None); // 'p' → TagName, name="p"
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::End,
                name: "p".into(),
                attrs: Vec::new(),
                self_closing: false
            }))
        ); // '>' → emit
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    #[test]
    fn e2e_self_closing_tag() {
        // `<br/>` → self-closing start tag, EOF
        let mut t = HtmlTokenizer::new("<br/>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName, name="b"
        assert_eq!(t.step(), None); // 'r' → append
        assert_eq!(t.step(), None); // '/' → SelfClosingStartTag
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "br".into(),
                attrs: Vec::new(),
                self_closing: true
            }))
        ); // '>' → emit
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    #[test]
    fn e2e_tag_space_then_chars() {
        // `<div>text</div>` → start tag, text chars, end tag, EOF
        let mut t = HtmlTokenizer::new("<div>text</div>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName, name="d"
        assert_eq!(t.step(), None); // 'i'
        assert_eq!(t.step(), None); // 'v'
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "div".into(),
                attrs: Vec::new(),
                self_closing: false
            }))
        ); // '>' → emit
        assert_eq!(t.next_token(), Some(Token::Character('t')));
        assert_eq!(t.next_token(), Some(Token::Character('e')));
        assert_eq!(t.next_token(), Some(Token::Character('x')));
        assert_eq!(t.next_token(), Some(Token::Character('t')));
        assert_eq!(t.step(), None); // '<' → TagOpen
        assert_eq!(t.step(), None); // '/' → EndTagOpen
        assert_eq!(t.step(), None); // 'd' → TagName, name="d"
        assert_eq!(t.step(), None); // 'i'
        assert_eq!(t.step(), None); // 'v'
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::End,
                name: "div".into(),
                attrs: Vec::new(),
                self_closing: false
            }))
        ); // '>' → emit
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    #[test]
    fn tag_name_switches_to_before_attribute_name_on_space() {
        // `<div class="x">` → space switches to BeforeAttributeName
        let mut t = HtmlTokenizer::new("<div class=\"x\">");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.state(), State::TagOpen);
        assert_eq!(t.step(), None); // TagOpen → TagName, name="d"
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // 'i' → append
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // 'v' → append
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // ' ' → BeforeAttributeName
        assert_eq!(t.state(), State::BeforeAttributeName);
    }

    #[test]
    fn tag_name_appends_non_ascii_chars() {
        // Non-ASCII characters in TagOpen fall through to Data (correct per spec).
        // `<日本語>` → '<' is emitted as text, then Japanese chars are character tokens.
        let mut t = HtmlTokenizer::new("<日本語>");
        assert_eq!(t.step(), None); // Data → TagOpen (no emit)
        assert_eq!(t.state(), State::TagOpen);
        assert_eq!(t.next_token(), Some(Token::Character('<'))); // TagOpen: not alpha, emit '<', reconsume
        assert_eq!(t.state(), State::Data);
        assert_eq!(t.next_token(), Some(Token::Character('日'))); // re-consumed in Data
        assert_eq!(t.next_token(), Some(Token::Character('本')));
        assert_eq!(t.next_token(), Some(Token::Character('語')));
        assert_eq!(t.next_token(), Some(Token::Character('>')));
    }

    #[test]
    fn tag_name_appends_non_ascii_after_entering_tag_name() {
        // Non-ASCII chars ARE appended to the tag name once we're in TagName state.
        // `<a日本語>`: 'a' enters TagName, then '日', '本', '語' are appended.
        let mut t = HtmlTokenizer::new("<a日本語>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.state(), State::TagOpen);
        assert_eq!(t.step(), None); // TagOpen → TagName, name="a"
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // '日' → append
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // '本' → append
        assert_eq!(t.state(), State::TagName);
        assert_eq!(t.step(), None); // '語' → append
        assert_eq!(t.state(), State::TagName);
        // '>' → emit tag with name "a日本語"
        let token = t.next_token();
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "a日本語".into(),
                attrs: Vec::new(),
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn tag_name_handles_null_character() {
        // `<a\x00>` → NULL in TagName should append U+FFFD, then '>' emits tag
        let mut t = HtmlTokenizer::new("<a\x00>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.state(), State::TagOpen);
        assert_eq!(t.step(), None); // TagOpen → TagName, name="a"
        assert_eq!(t.state(), State::TagName);
        // '\0' in TagName: append U+FFFD (parse error), stay in TagName
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::TagName);
        // '>' → emit tag with name "a\u{FFFD}"
        let token = t.next_token();
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "a\u{FFFD}".into(),
                attrs: Vec::new(),
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    // ── Markup declaration tests (§13.2.5.42) ─────────────────────

    #[test]
    fn markup_declaration_open_dash_dash_to_comment_start() {
        // `<!--` → MarkupDeclarationOpen → CommentStart
        let mut t = HtmlTokenizer::new("<!--");
        assert_eq!(t.step(), None); // Data → TagOpen ('<')
        assert_eq!(t.step(), None); // TagOpen → MarkupDeclarationOpen ('!')
        assert_eq!(t.state(), State::MarkupDeclarationOpen);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart ("--")
        assert_eq!(t.state(), State::CommentStart);
    }

    #[test]
    fn markup_declaration_open_doctype() {
        // `<!DOCTYPE` → MarkupDeclarationOpen → Doctype → EOF → emit force_quirks Doctype
        let mut t = HtmlTokenizer::new("<!DOCTYPE");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → MarkupDeclarationOpen
        assert_eq!(t.state(), State::MarkupDeclarationOpen);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → Doctype
                                    // Doctype 遇到 EOF：force_quirks=true, emit
        assert_eq!(
            t.next_token(),
            Some(Token::Doctype(DoctypeToken {
                name: None,
                public_id: None,
                system_id: None,
                force_quirks: true,
            }))
        );
    }

    // ── DOCTYPE 测试 (§13.2.5.53) ─────────────────────────────────

    /// 辅助：推进到 Doctype 状态（!DOCTYPE 已消费）
    fn enter_doctype(input: &str) -> HtmlTokenizer {
        let mut t = HtmlTokenizer::new(input);
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → MarkupDeclarationOpen
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → Doctype
        assert_eq!(t.state(), State::Doctype);
        t
    }

    #[test]
    fn doctype_entry_skips_whitespace() {
        // `<!DOCTYPE html>` → 跳过 Doctype 中的空白，进入 BeforeDoctypeName
        let mut t = enter_doctype("<!DOCTYPE html>");
        // ' ' → stay in Doctype
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::Doctype);
        // 'h' → reconsume → BeforeDoctypeName
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::BeforeDoctypeName);
    }

    #[test]
    fn doctype_entry_non_whitespace_immediate() {
        // `<!DOCTYPEhtml>` → 直接进入 BeforeDoctypeName（reconsume）
        let mut t = enter_doctype("<!DOCTYPEhtml>");
        // 'h' → reconsume → BeforeDoctypeName
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::BeforeDoctypeName);
    }

    // ── DOCTYPE 名称测试 (§13.2.5.54–§13.2.5.55) ─────────────────

    /// 辅助：推进到 BeforeDoctypeName（Doctype 已跳过空白）
    fn enter_before_doctype_name(input: &str) -> HtmlTokenizer {
        let mut t = enter_doctype(input);
        while t.state() == State::Doctype {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.state(), State::BeforeDoctypeName);
        t
    }

    #[test]
    fn doctype_name_simple_html() {
        let mut t = enter_before_doctype_name("<!DOCTYPE html>");
        assert_eq!(t.step(), None); // 'h' → DoctypeName
        assert_eq!(t.state(), State::DoctypeName);
        for _ in 0..3 {
            assert_eq!(t.step(), None);
        } // 't','m','l'
        assert_eq!(
            t.next_token(),
            Some(Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: None,
                system_id: None,
                force_quirks: false,
            }))
        );
    }

    #[test]
    fn doctype_name_uppercase() {
        let mut t = enter_before_doctype_name("<!DOCTYPE HTML>");
        assert_eq!(t.step(), None); // 'H'→'h'
        assert_eq!(t.step(), None); // 'T'→'t'
        assert_eq!(t.step(), None); // 'M'→'m'
        assert_eq!(t.step(), None); // 'L'→'l'
        assert_eq!(
            t.next_token(),
            Some(Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: None,
                system_id: None,
                force_quirks: false,
            }))
        );
    }

    #[test]
    fn doctype_name_null_char() {
        let mut t = enter_before_doctype_name("<!DOCTYPE html\0x>");
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        } // 'h','t','m','l'
        assert_eq!(t.step(), None); // '\0' → U+FFFD
        assert_eq!(t.step(), None); // 'x'
        assert_eq!(
            t.next_token(),
            Some(Token::Doctype(DoctypeToken {
                name: Some("html\u{FFFD}x".into()),
                public_id: None,
                system_id: None,
                force_quirks: false,
            }))
        );
    }

    #[test]
    fn doctype_before_name_empty_gt() {
        let mut t = enter_doctype("<!DOCTYPE >");
        assert_eq!(t.step(), None); // ' ' → stay Doctype
        assert_eq!(t.step(), None); // '>' → reconsume, BeforeDoctypeName
        assert_eq!(
            t.next_token(), // BeforeDoctypeName 处理 '>' → force_quirks emit
            Some(Token::Doctype(DoctypeToken {
                name: None,
                public_id: None,
                system_id: None,
                force_quirks: true,
            }))
        );
    }

    #[test]
    fn doctype_name_eof() {
        let mut t = enter_before_doctype_name("<!DOCTYPE html");
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(
            t.next_token(), // EOF → force_quirks emit
            Some(Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: None,
                system_id: None,
                force_quirks: true,
            }))
        );
    }

    // ── AfterDoctypeName + BogusDoctype 测试 (§13.2.5.56, §13.2.5.68) ─

    #[test]
    fn doctype_after_name_public_keyword() {
        let mut t = enter_before_doctype_name("<!DOCTYPE html PUBLIC \"-//EN\">");
        // 'h','t','m','l'
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        // ' ' → AfterDoctypeName
        assert_eq!(t.step(), None); // skip whitespace in AfterDoctypeName
        assert_eq!(t.state(), State::AfterDoctypeName);
        // 'P' → matches "PUBLIC"
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::AfterDoctypePublicKeyword);
    }

    #[test]
    fn doctype_after_name_system_keyword() {
        let mut t = enter_before_doctype_name("<!DOCTYPE html SYSTEM \"about:\">");
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // ' ' → AfterDoctypeName
        assert_eq!(t.state(), State::AfterDoctypeName);
        assert_eq!(t.step(), None); // 'S' → matches "SYSTEM"
        assert_eq!(t.state(), State::AfterDoctypeSystemKeyword);
    }

    #[test]
    fn doctype_after_name_gt_emits() {
        let mut t = enter_before_doctype_name("<!DOCTYPE html>");
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(
            t.next_token(),
            Some(Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: None,
                system_id: None,
                force_quirks: false,
            }))
        );
    }

    #[test]
    fn doctype_after_name_unknown_to_bogus() {
        // `<!DOCTYPE html x>` → 'x' 不匹配 PUBLIC/SYSTEM → BogusDoctype
        let mut t = enter_before_doctype_name("<!DOCTYPE html x>");
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // ' ' → AfterDoctypeName
        assert_eq!(t.step(), None); // 'x' → BogusDoctype (reconsume)
        assert_eq!(t.state(), State::BogusDoctype);
        // BogusDoctype: ignore 'x' (reconsumed) → '>' emit
        assert_eq!(t.step(), None); // 'x' ignored by BogusDoctype
        assert_eq!(
            t.next_token(), // '>' emit
            Some(Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: None,
                system_id: None,
                force_quirks: true,
            }))
        );
    }

    #[test]
    fn doctype_bogus_ignores_chars() {
        let mut t = enter_before_doctype_name("<!DOCTYPE html foo>");
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // ' ' → AfterDoctypeName
        assert_eq!(t.step(), None); // 'f' → BogusDoctype (reconsume)
                                    // BogusDoctype 忽略 'f','o','o'
        for _ in 0..3 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(
            t.next_token(), // '>' emit
            Some(Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: None,
                system_id: None,
                force_quirks: true,
            }))
        );
    }

    // ── DOCTYPE PUBLIC/SYSTEM 标识符集成测试 ──────────────────────

    #[test]
    fn doctype_public_id_double_quoted() {
        let mut t = HtmlTokenizer::new("<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\">");
        assert_eq!(
            next_real_token(&mut t),
            Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: Some("-//W3C//DTD HTML 4.01//EN".into()),
                system_id: None,
                force_quirks: false,
            })
        );
    }

    #[test]
    fn doctype_system_id_double_quoted() {
        let mut t = HtmlTokenizer::new("<!DOCTYPE html SYSTEM \"about:legacy-compat\">");
        assert_eq!(
            next_real_token(&mut t),
            Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: None,
                system_id: Some("about:legacy-compat".into()),
                force_quirks: false,
            })
        );
    }

    #[test]
    fn doctype_full_public_and_system() {
        let mut t = HtmlTokenizer::new(
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \
             \"http://www.w3.org/TR/html4/strict.dtd\">",
        );
        assert_eq!(
            next_real_token(&mut t),
            Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: Some("-//W3C//DTD HTML 4.01//EN".into()),
                system_id: Some("http://www.w3.org/TR/html4/strict.dtd".into()),
                force_quirks: false,
            })
        );
    }

    #[test]
    fn doctype_public_id_single_quoted() {
        let mut t = HtmlTokenizer::new("<!DOCTYPE html PUBLIC '-//W3C//DTD XHTML 1.0//EN'>");
        assert_eq!(
            next_real_token(&mut t),
            Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: Some("-//W3C//DTD XHTML 1.0//EN".into()),
                system_id: None,
                force_quirks: false,
            })
        );
    }

    #[test]
    fn doctype_system_id_single_quoted() {
        let mut t = HtmlTokenizer::new("<!DOCTYPE html SYSTEM 'about:legacy-compat'>");
        assert_eq!(
            next_real_token(&mut t),
            Token::Doctype(DoctypeToken {
                name: Some("html".into()),
                public_id: None,
                system_id: Some("about:legacy-compat".into()),
                force_quirks: false,
            })
        );
    }

    #[test]
    fn markup_declaration_open_bogus() {
        // `<!foo` → MarkupDeclarationOpen → BogusComment
        let mut t = HtmlTokenizer::new("<!foo");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → MarkupDeclarationOpen
        assert_eq!(t.step(), None); // → BogusComment
        assert_eq!(t.state(), State::BogusComment);
    }

    // ── Bogus comment tests (§13.2.5.41) ──────────────────────────

    #[test]
    fn bogus_comment_emits_on_greater_than() {
        // `<?xml>` → Token::Comment("?xml")
        let mut t = HtmlTokenizer::new("<?xml>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → BogusComment ('?')
                                    // 'x', 'm', 'l'
        for _ in 0..3 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Comment("?xml".into())));
    }

    #[test]
    fn bogus_comment_handles_null() {
        // `<?a\0b>` → Token::Comment("?a\u{FFFD}b")
        let mut t = HtmlTokenizer::new("<?a\0b>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → BogusComment ('?')
        assert_eq!(t.step(), None); // 'a'
        assert_eq!(t.step(), None); // '\0' → U+FFFD
        assert_eq!(t.step(), None); // 'b'
        assert_eq!(t.next_token(), Some(Token::Comment("?a\u{FFFD}b".into())));
    }

    #[test]
    fn bogus_comment_eof() {
        // `<?x` + EOF: Per WHATWG §13.2.5.6, `?` in TagOpen switches to
        // ProcessingInstructionOpen (§13.2.5.72). `x` is alphabetic, so we
        // enter ProcessingInstructionTarget (§13.2.5.73). EOF in that state
        // emits EOF directly (no comment is produced).
        let mut t = HtmlTokenizer::new("<?x");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → ProcessingInstructionOpen
        assert_eq!(t.step(), None); // PIOpen 'x' → PITarget (reconsume)
        assert_eq!(t.step(), None); // PITarget 'x' → append to temp buffer
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    #[test]
    fn bogus_comment_from_bang() {
        // `<!foo>` → MarkupDeclarationOpen → BogusComment → Token::Comment("foo")
        let mut t = HtmlTokenizer::new("<!foo>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → MarkupDeclarationOpen
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → BogusComment
                                    // 'f', 'o', 'o'
        for _ in 0..3 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Comment("foo".into())));
    }

    // ── Comment state tests (§13.2.5.43–§13.2.5.45) ──────────────

    /// 辅助：推进 tokenizer 到 MarkupDeclarationOpen 的 '!' 之后
    /// 调用后 pos 在 '!' 之后，state = TagOpen 刚设置 MarkupDeclarationOpen 但还未执行
    fn enter_markup_declaration(t: &mut HtmlTokenizer) {
        // Data → TagOpen → MarkupDeclarationOpen (doesn't consume yet)
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → MarkupDeclarationOpen (sees '!')
    }

    #[test]
    fn comment_start_dash_to_comment_start_dash() {
        let mut t = HtmlTokenizer::new("<!---");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart ("--")
        assert_eq!(t.step(), None); // CommentStart → CommentStartDash ('-')
        assert_eq!(t.state(), State::CommentStartDash);
    }

    #[test]
    fn comment_start_empty_comment_on_gt() {
        let mut t = HtmlTokenizer::new("<!-->");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.next_token(), Some(Token::Comment("".into()))); // '>' → emit empty
    }

    #[test]
    fn comment_start_lt_to_comment_lt_sign() {
        let mut t = HtmlTokenizer::new("<!--<");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // '<' → CommentLessThanSign
        assert_eq!(t.state(), State::CommentLessThanSign);
    }

    #[test]
    fn comment_start_null_to_comment() {
        let mut t = HtmlTokenizer::new("<!--\0");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // '\0' → Comment (with U+FFFD)
        assert_eq!(t.state(), State::Comment);
    }

    #[test]
    fn comment_start_dash_gt_emits_empty() {
        let mut t = HtmlTokenizer::new("<!--->");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // '-' → CommentStartDash
        assert_eq!(t.next_token(), Some(Token::Comment("".into()))); // '>' → emit
    }

    #[test]
    fn comment_start_dash_other_to_comment() {
        let mut t = HtmlTokenizer::new("<!---a");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // '-' → CommentStartDash
        assert_eq!(t.step(), None); // 'a' → Comment (appends "-a")
        assert_eq!(t.state(), State::Comment);
    }

    #[test]
    fn comment_state_appends_chars() {
        let mut t = HtmlTokenizer::new("<!--abc-->");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // 'a' → Comment
        assert_eq!(t.step(), None); // 'b'
        assert_eq!(t.step(), None); // 'c'
                                    // '-->' closing
        assert_eq!(t.step(), None); // '-' → CommentEndDash
        assert_eq!(t.step(), None); // '-' → CommentEnd
        assert_eq!(t.next_token(), Some(Token::Comment("abc".into()))); // '>' emit
    }

    #[test]
    fn comment_state_lt_switches() {
        let mut t = HtmlTokenizer::new("<!--a<");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // 'a' → Comment
        assert_eq!(t.step(), None); // '<' → CommentLessThanSign
        assert_eq!(t.state(), State::CommentLessThanSign);
    }

    #[test]
    fn comment_state_dash_switches() {
        let mut t = HtmlTokenizer::new("<!--a-");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // 'a' → Comment
        assert_eq!(t.step(), None); // '-' → CommentEndDash
        assert_eq!(t.state(), State::CommentEndDash);
    }

    #[test]
    fn comment_state_null_handles() {
        let mut t = HtmlTokenizer::new("<!--a\0b-->");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // 'a' → Comment
        assert_eq!(t.step(), None); // '\0' → U+FFFD
        assert_eq!(t.step(), None); // 'b'
                                    // '-->'
        assert_eq!(t.step(), None); // '-' → CommentEndDash
        assert_eq!(t.step(), None); // '-' → CommentEnd
        assert_eq!(t.next_token(), Some(Token::Comment("a\u{FFFD}b".into())));
    }

    // ── CommentLessThanSign 系列测试 (§13.2.5.46–§13.2.5.49) ────

    #[test]
    fn comment_lt_sign_excl_to_bang() {
        let mut t = HtmlTokenizer::new("<!--a<!--b-->");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // CommentStart → Comment ('a')
        assert_eq!(t.step(), None); // Comment → CommentLessThanSign ('<')
        assert_eq!(t.step(), None); // CommentLessThanSign → Bang ('!')
        assert_eq!(t.state(), State::CommentLessThanSignBang);
    }

    #[test]
    fn comment_lt_sign_lt_stays() {
        let mut t = HtmlTokenizer::new("<!--a<<b-->");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // CommentStart → Comment ('a')
        assert_eq!(t.step(), None); // Comment → CommentLessThanSign ('<')
        assert_eq!(t.step(), None); // LessThanSign → stay, append '<'
        assert_eq!(t.state(), State::CommentLessThanSign);
    }

    #[test]
    fn comment_lt_bang_dash_chain() {
        let mut t = HtmlTokenizer::new("<!--a<!-b-->");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // CommentStart → Comment ('a')
        assert_eq!(t.step(), None); // Comment → CommentLessThanSign ('<')
        assert_eq!(t.step(), None); // LessThanSign → Bang ('!')
        assert_eq!(t.step(), None); // Bang → BangDash ('-')
        assert_eq!(t.state(), State::CommentLessThanSignBangDash);
    }

    #[test]
    fn comment_lt_bang_dash_dash_to_dashdash() {
        let mut t = HtmlTokenizer::new("<!--<!--b-->");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // CommentStart → Comment (no chars before '<')
        assert_eq!(t.step(), None); // Comment → CommentLessThanSign ('<')
        assert_eq!(t.step(), None); // LessThanSign → Bang ('!')
        assert_eq!(t.step(), None); // Bang → BangDash ('-')
        assert_eq!(t.step(), None); // BangDash → BangDashDash ('-')
                                    // §13.2.5.49: anything else reconsumes in CommentEnd
                                    // (not Comment). CommentEnd's anything-else arm appends
                                    // `--` as catch-up, then reconsumes `b` in Comment.
        assert_eq!(t.state(), State::CommentEnd);
    }

    #[test]
    fn comment_nested_open_not_close() {
        // `<!-- a<!--> b -->` → §13.2.5.49: `>` in CommentLessThanSignBangDashDash
        // appends "!" and EMITS the comment (not silently consumed).
        // `<` was appended by Comment state (§13.2.5.45), so comment = " a<!".
        // html5lib evidence: `<!--<!-->` → ["Comment", "<!"]
        let mut t = HtmlTokenizer::new("<!-- a<!--> b -->");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // ' ' → Comment
        assert_eq!(t.step(), None); // 'a'
                                    // '<', '!', '-', '-' → LessThanSign → Bang → BangDash → BangDashDash
        assert_eq!(t.step(), None); // '<' → LessThanSign (appended '<')
        assert_eq!(t.step(), None); // '!' → Bang
        assert_eq!(t.step(), None); // '-' → BangDash
        assert_eq!(t.step(), None); // '-' → BangDashDash
                                    // `>` → §13.2.5.49: append "!", emit comment " a<!", switch to Data
        assert_eq!(t.next_token(), Some(Token::Comment(" a<!".into())));
        assert_eq!(t.state(), State::Data);
    }

    // ── CommentEnd 系列测试 (§13.2.5.50–§13.2.5.52) ────────────

    #[test]
    fn comment_end_gt_emits() {
        let mut t = HtmlTokenizer::new("<!--hello-->");
        enter_markup_declaration(&mut t);
        // CommentStart + 'h','e','l','l','o' = 1 + 5 = 6 calls
        for _ in 0..6 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '-' → CommentEndDash
        assert_eq!(t.step(), None); // '-' → CommentEnd
        assert_eq!(t.next_token(), Some(Token::Comment("hello".into()))); // '>'
    }

    #[test]
    fn comment_end_bang_gt_emits() {
        // `<!--hello--!>` → emit "hello"
        let mut t = HtmlTokenizer::new("<!--hello--!>");
        enter_markup_declaration(&mut t);
        for _ in 0..6 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '-' → CommentEndDash
        assert_eq!(t.step(), None); // '-' → CommentEnd
        assert_eq!(t.step(), None); // '!' → CommentEndBang
        assert_eq!(t.next_token(), Some(Token::Comment("hello".into()))); // '>'
    }

    #[test]
    fn comment_end_bang_dash_to_end() {
        // `<!--hello--!->` → §13.2.5.52 CommentEndBang `-`: append "--!", switch to
        // CommentEndDash. §13.2.5.50 CommentEndDash `>`: "anything else" — append "-",
        // reconsume in Comment. Comment `>`: "anything else" — append ">".
        // html5lib evidence: `<!----! >` → ["Comment", "--! >"]
        let mut t = HtmlTokenizer::new("<!--hello--!->");
        enter_markup_declaration(&mut t);
        for _ in 0..6 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '-' → CommentEndDash
        assert_eq!(t.step(), None); // '-' → CommentEnd
        assert_eq!(t.step(), None); // '!' → CommentEndBang
        assert_eq!(t.step(), None); // '-' → CommentEndDash (appended '--!')
        assert_eq!(t.step(), None); // '>' → Comment (appended '-', reconsume)
        assert_eq!(t.step(), None); // '>' in Comment → append '>'
        assert_eq!(t.next_token(), Some(Token::Comment("hello--!->".into()))); // EOF emit
    }

    #[test]
    fn comment_extra_dashes() {
        // `<!--hello---->` → 多余的 '-'
        let mut t = HtmlTokenizer::new("<!--hello---->");
        enter_markup_declaration(&mut t);
        for _ in 0..6 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '-' → CommentEndDash (1st)
        assert_eq!(t.step(), None); // '-' → CommentEnd (2nd)
        assert_eq!(t.step(), None); // '-' → stay, append '-' (3rd)
        assert_eq!(t.step(), None); // '-' → stay, append '-' (4th)
        assert_eq!(t.next_token(), Some(Token::Comment("hello--".into()))); // '>'
    }

    #[test]
    fn comment_state_eof() {
        let mut t = HtmlTokenizer::new("<!--abc");
        enter_markup_declaration(&mut t);
        assert_eq!(t.step(), None); // MarkupDeclarationOpen → CommentStart
        assert_eq!(t.step(), None); // 'a' → Comment
        assert_eq!(t.step(), None); // 'b'
        assert_eq!(t.step(), None); // 'c'
        assert_eq!(t.next_token(), Some(Token::Comment("abc".into()))); // EOF → emit
    }

    // ── Comment 集成测试 ──────────────────────────────────────────

    /// 辅助：跳过 None，直达下一个 Some(token)（常用于集成测试）
    fn next_real_token(t: &mut HtmlTokenizer) -> Token {
        loop {
            match t.next_token() {
                Some(token) => return token,
                None => continue,
            }
        }
    }

    #[test]
    fn comment_e2e_simple() {
        // `<!-- hello world -->`
        let mut t = HtmlTokenizer::new("<!-- hello world -->");
        assert_eq!(
            next_real_token(&mut t),
            Token::Comment(" hello world ".into())
        );
    }

    #[test]
    fn comment_e2e_empty() {
        // `<!---->` → 空注释
        let mut t = HtmlTokenizer::new("<!---->");
        assert_eq!(next_real_token(&mut t), Token::Comment("".into()));
    }

    #[test]
    fn comment_e2e_followed_by_tag() {
        // `<!-- comment --><div>` → Comment + Tag
        let mut t = HtmlTokenizer::new("<!-- comment --><div>");
        assert_eq!(next_real_token(&mut t), Token::Comment(" comment ".into()));
        assert_eq!(
            next_real_token(&mut t),
            Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "div".into(),
                attrs: vec![],
                self_closing: false,
            })
        );
    }

    #[test]
    fn comment_e2e_nested() {
        // `<!-- <!-- nested --> -->`
        // §13.2.5.45: `<` appended to comment. §13.2.5.49 "anything else": appends "!--".
        // So `<!--` inside comment becomes part of content: "<" + "!--" = "<!--".
        // html5lib evidence: `<!-- <!--test-->` → ["Comment", " <!--test"]
        let mut t = HtmlTokenizer::new("<!-- <!-- nested --> -->");
        assert_eq!(
            next_real_token(&mut t),
            Token::Comment(" <!-- nested ".into())
        );
    }

    // ── Attribute tests (§13.2.5.32–§13.2.5.39) ──────────────────

    #[test]
    fn attr_single_double_quoted_value() {
        // `<div class="x">` → start tag with one double-quoted attribute
        let mut t = HtmlTokenizer::new("<div class=\"x\">");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName, name="d"
        assert_eq!(t.step(), None); // 'i'
        assert_eq!(t.step(), None); // 'v'
        assert_eq!(t.step(), None); // ' ' → BeforeAttributeName
        assert_eq!(t.state(), State::BeforeAttributeName);
        assert_eq!(t.step(), None); // 'c' → BA reconsume (call 6)
        assert_eq!(t.step(), None); // 'l' (call 7, reconsume 'c')
        assert_eq!(t.step(), None); // 'a' (call 8)
        assert_eq!(t.step(), None); // 's' (call 9)
        assert_eq!(t.step(), None); // 's' (call 10, last char)
        assert_eq!(t.step(), None); // 's' (call 11 — 第二个 's')
        assert_eq!(t.step(), None); // '=' → BeforeAttributeValue (call 12)
        assert_eq!(t.state(), State::BeforeAttributeValue);
        assert_eq!(t.step(), None); // '"' → AttributeValueDoubleQuoted (call 13)
        assert_eq!(t.state(), State::AttributeValueDoubleQuoted);
        assert_eq!(t.step(), None); // 'x' → append
        assert_eq!(t.state(), State::AttributeValueDoubleQuoted);
        assert_eq!(t.step(), None); // '"' → AfterAttributeValueQuoted, emit attr
        assert_eq!(t.state(), State::AfterAttributeValueQuoted);
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "div".into(),
                attrs: vec![("class".into(), "x".into())],
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn attr_single_quoted_value() {
        // `<input type='text'>` → single-quoted attribute
        let mut t = HtmlTokenizer::new("<input type='text'>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName, name="i"
                                    // 'n', 'p', 'u', 't'
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // ' ' → BeforeAttributeName
                                    // 't', 'y', 'p', 'e' — 需要 5 次调用（BeforeAttributeName reconsume 占用 1 次）
                                    // BeforeAttributeName → reconsume 't' → AttributeName → 'y','p','e'
        for _ in 0..5 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '=' → BeforeAttributeValue
        assert_eq!(t.state(), State::BeforeAttributeValue);
        assert_eq!(t.step(), None); // '\'' → AttributeValueSingleQuoted
        assert_eq!(t.state(), State::AttributeValueSingleQuoted);
        // 't', 'e', 'x', 't'
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '\'' → AfterAttributeValueQuoted, emit attr
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "input".into(),
                attrs: vec![("type".into(), "text".into())],
                self_closing: false,
            }))
        );
    }

    #[test]
    fn attr_unquoted_value() {
        // `<a href=x>` → unquoted attribute value
        let mut t = HtmlTokenizer::new("<a href=x>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName, name="a"
        assert_eq!(t.step(), None); // ' ' → BeforeAttributeName
        assert_eq!(t.state(), State::BeforeAttributeName);
        // 'h', 'r', 'e', 'f' — 需要 5 次（BA reconsume + 'h' reconsume + 3 剩余字符）
        for _ in 0..5 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '=' → BeforeAttributeValue
        assert_eq!(t.state(), State::BeforeAttributeValue);
        assert_eq!(t.step(), None); // 'x' → AttributeValueUnquoted
        assert_eq!(t.state(), State::AttributeValueUnquoted);
        assert_eq!(t.step(), None); // '>' → emit attr, emit tag
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "a".into(),
                attrs: vec![("href".into(), "x".into())],
                self_closing: false,
            }))
        );
    }

    #[test]
    fn attr_multiple_attributes() {
        // `<div id="a" class="b">` → two attributes
        let mut t = HtmlTokenizer::new("<div id=\"a\" class=\"b\">");
        // Skip to tag name done: `<` → TagOpen, `d` → name, `i`, `v`
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName, name="d"
        assert_eq!(t.step(), None); // 'i'
        assert_eq!(t.step(), None); // 'v'
                                    // ' ' → BeforeAttributeName
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::BeforeAttributeName);
        // 'i', 'd' — 需要 3 次（BA reconsume + 'i' reconsume + 'd'）
        assert_eq!(t.step(), None); // BA → reconsume 'i'
        assert_eq!(t.step(), None); // reconsume 'i'
        assert_eq!(t.step(), None); // 'd'
                                    // '=' → BeforeAttributeValue
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::BeforeAttributeValue);
        // '"' → AttributeValueDoubleQuoted
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::AttributeValueDoubleQuoted);
        // 'a'
        assert_eq!(t.step(), None);
        // '"' → AfterAttributeValueQuoted, emit attr("id","a")
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::AfterAttributeValueQuoted);
        // ' ' → BeforeAttributeName
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::BeforeAttributeName);
        // 'c', 'l', 'a', 's', 's' — 需要 6 次（BA reconsume + 'c' reconsume + 4 剩余字符）
        for _ in 0..6 {
            assert_eq!(t.step(), None);
        }
        // '=' → BeforeAttributeValue
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::BeforeAttributeValue);
        // '"' → AttributeValueDoubleQuoted
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::AttributeValueDoubleQuoted);
        // 'b'
        assert_eq!(t.step(), None);
        // '"' → AfterAttributeValueQuoted, emit attr("class","b")
        assert_eq!(t.step(), None);
        assert_eq!(t.state(), State::AfterAttributeValueQuoted);
        // '>' → emit tag
        let token = t.next_token();
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "div".into(),
                attrs: vec![("id".into(), "a".into()), ("class".into(), "b".into())],
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn attr_boolean_attribute() {
        let mut t = HtmlTokenizer::new("<input disabled>");
        // Skip to tag name done
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName, name="i"
                                    // 'n', 'p', 'u', 't'
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        // ' ' → BeforeAttributeName
        assert_eq!(t.step(), None);
        // 'd', 'i', 's', 'a', 'b', 'l', 'e', 'd' — 需要 9 次（BA reconsume + 'd' reconsume + 7 剩余字符）
        for _ in 0..9 {
            assert_eq!(t.step(), None);
        }
        // '>' → AfterAttributeName → emit attr, emit tag
        let token = t.next_token();
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "input".into(),
                attrs: vec![("disabled".into(), "".into())],
                self_closing: false,
            }))
        );
    }

    #[test]
    fn attr_name_lowercases_ascii_upper() {
        // §13.2.5.33: ASCII uppercase → lowercase in attribute names
        let mut t = HtmlTokenizer::new("<div CLASS=\"x\">");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName 'd'
        assert_eq!(t.step(), None); // 'i'
        assert_eq!(t.step(), None); // 'v'
        assert_eq!(t.step(), None); // ' ' → BeforeAttributeName
                                    // 'C','L','A','S','S' — 6 calls (BA reconsume + 5 chars)
        for _ in 0..6 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '=' → BeforeAttributeValue
        assert_eq!(t.step(), None); // '"' → AttributeValueDoubleQuoted
        assert_eq!(t.step(), None); // 'x'
        assert_eq!(t.step(), None); // '"' → AfterAttributeValueQuoted
        let token = t.next_token(); // '>'
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "div".into(),
                attrs: vec![("class".into(), "x".into())],
                self_closing: false,
            }))
        );
    }

    #[test]
    fn attr_name_preserves_non_ascii() {
        // Non-ASCII chars in attribute names should be preserved as-is
        let mut t = HtmlTokenizer::new("<div café=\"oui\">");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName 'd'
        assert_eq!(t.step(), None); // 'i'
        assert_eq!(t.step(), None); // 'v'
        assert_eq!(t.step(), None); // ' ' → BeforeAttributeName
                                    // 'c','a','f','é' — 5 calls (BA reconsume + 4 chars)
        for _ in 0..5 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '=' → BeforeAttributeValue
        assert_eq!(t.step(), None); // '"' → AttributeValueDoubleQuoted
        assert_eq!(t.step(), None); // 'o'
        assert_eq!(t.step(), None); // 'u'
        assert_eq!(t.step(), None); // 'i'
        assert_eq!(t.step(), None); // '"' → AfterAttributeValueQuoted
        let token = t.next_token(); // '>'
        assert_eq!(
            token,
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "div".into(),
                attrs: vec![("café".into(), "oui".into())],
                self_closing: false,
            }))
        );
    }

    #[test]
    fn e2e_attr_and_self_closing() {
        // `<input type='text'/>` → attribute + self-closing
        let mut t = HtmlTokenizer::new("<input type='text'/>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → TagName, name="i"
                                    // 'n', 'p', 'u', 't'
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // ' ' → BeforeAttributeName
                                    // 't', 'y', 'p', 'e' — 需要 5 次（BA reconsume + 't' reconsume + 3 剩余字符）
        for _ in 0..5 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '=' → BeforeAttributeValue
        assert_eq!(t.step(), None); // '\'' → AttributeValueSingleQuoted
                                    // 't', 'e', 'x', 't'
        for _ in 0..4 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.step(), None); // '\'' → AfterAttributeValueQuoted
        assert_eq!(t.step(), None); // '/' → SelfClosingStartTag
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::Start,
                name: "input".into(),
                attrs: vec![("type".into(), "text".into())],
                self_closing: true,
            }))
        );
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    // ── Content model tests (§13.2.5.2–§13.2.5.5) ────────────────

    /// Helper: create a tokenizer in a specific content model state.
    fn enter_content_model(input: &str, state: State, end_tag: Option<&str>) -> HtmlTokenizer {
        let mut t = HtmlTokenizer::new(input);
        t.set_state(state);
        t.set_appropriate_end_tag_name(end_tag);
        t
    }

    #[test]
    fn rcdata_emits_characters() {
        let mut t = enter_content_model("hello", State::RCDATA, Some("title"));
        assert_eq!(t.next_token(), Some(Token::Character('h')));
        assert_eq!(t.next_token(), Some(Token::Character('e')));
        assert_eq!(t.next_token(), Some(Token::Character('l')));
        assert_eq!(t.next_token(), Some(Token::Character('l')));
        assert_eq!(t.next_token(), Some(Token::Character('o')));
    }

    #[test]
    fn rcdata_lt_switches_to_less_than_sign() {
        let mut t = enter_content_model("<", State::RCDATA, Some("title"));
        assert_eq!(t.step(), None); // '<' → RCDATALessThanSign
        assert_eq!(t.state(), State::RCDATALessThanSign);
    }

    #[test]
    fn rcdata_ampersand_switches_to_charref() {
        let mut t = enter_content_model("&", State::RCDATA, Some("title"));
        assert_eq!(t.step(), None); // '&' → CharacterReference
        assert_eq!(t.state(), State::CharacterReference);
    }

    #[test]
    fn rcdata_null_emits_replacement_char() {
        let mut t = enter_content_model("\0", State::RCDATA, Some("title"));
        assert_eq!(t.next_token(), Some(Token::Character('\u{FFFD}')));
    }

    #[test]
    fn rcdata_eof_emits_eof() {
        let mut t = enter_content_model("", State::RCDATA, Some("title"));
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    #[test]
    fn rcdata_end_tag_match() {
        // `</title>` in RCDATA with appropriate_end_tag_name = "title"
        let mut t = enter_content_model("</title>", State::RCDATA, Some("title"));
        // '<' → RCDATALessThanSign
        assert_eq!(t.step(), None);
        // '/' → RCDATAEndTagOpen
        assert_eq!(t.step(), None);
        // 't' → RCDATAEndTagName (create end tag "t")
        assert_eq!(t.step(), None);
        // 'i' → append to end tag "ti"
        assert_eq!(t.step(), None);
        // 't' → append to end tag "tit"
        assert_eq!(t.step(), None);
        // 'l' → append to end tag "titl"
        assert_eq!(t.step(), None);
        // 'e' → append to end tag "title"
        assert_eq!(t.step(), None);
        // '>' → matches appropriate end tag → emit tag, switch to Data
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::End,
                name: "title".into(),
                attrs: vec![],
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn rcdata_end_tag_no_match_backout() {
        // `</div>` in RCDATA with appropriate_end_tag_name = "title" — does not match
        let mut t = enter_content_model("</div>x", State::RCDATA, Some("title"));
        // '<' → RCDATALessThanSign
        assert_eq!(t.step(), None);
        // '/' → RCDATAEndTagOpen
        assert_eq!(t.step(), None);
        // 'd' → RCDATAEndTagName (create end tag, name="d", buf="d")
        assert_eq!(t.step(), None);
        // 'i' → append
        assert_eq!(t.step(), None);
        // 'v' → append
        assert_eq!(t.step(), None);
        // '>' → not appropriate (name="div" != expected="title") → backout
        // Handler returns None but pushes pending tokens
        assert_eq!(t.step(), None);
        // Pending tokens drained: '<' '/' 'd' 'i' 'v' (forward order)
        assert_eq!(t.next_token(), Some(Token::Character('<')));
        assert_eq!(t.next_token(), Some(Token::Character('/')));
        assert_eq!(t.next_token(), Some(Token::Character('d')));
        assert_eq!(t.next_token(), Some(Token::Character('i')));
        assert_eq!(t.next_token(), Some(Token::Character('v')));
        // '>' is re-consumed in RCDATA as character token
        assert_eq!(t.next_token(), Some(Token::Character('>')));
        // Then 'x'
        assert_eq!(t.state(), State::RCDATA);
        assert_eq!(t.next_token(), Some(Token::Character('x')));
    }

    #[test]
    fn rawtext_emits_characters() {
        let mut t = enter_content_model("hello", State::RAWTEXT, Some("style"));
        assert_eq!(t.next_token(), Some(Token::Character('h')));
        assert_eq!(t.next_token(), Some(Token::Character('e')));
        assert_eq!(t.next_token(), Some(Token::Character('l')));
        assert_eq!(t.next_token(), Some(Token::Character('l')));
        assert_eq!(t.next_token(), Some(Token::Character('o')));
    }

    #[test]
    fn rawtext_ampersand_is_literal() {
        // RAWTEXT does NOT handle `&` — it's emitted as a regular character
        let mut t = enter_content_model("&amp;", State::RAWTEXT, Some("style"));
        assert_eq!(t.next_token(), Some(Token::Character('&')));
        assert_eq!(t.next_token(), Some(Token::Character('a')));
        assert_eq!(t.next_token(), Some(Token::Character('m')));
        assert_eq!(t.next_token(), Some(Token::Character('p')));
        assert_eq!(t.next_token(), Some(Token::Character(';')));
    }

    #[test]
    fn rawtext_lt_switches_to_less_than_sign() {
        let mut t = enter_content_model("<", State::RAWTEXT, Some("style"));
        assert_eq!(t.step(), None); // '<' → RAWTEXTLessThanSign
        assert_eq!(t.state(), State::RAWTEXTLessThanSign);
    }

    #[test]
    fn rawtext_end_tag_match() {
        // `</style>` in RAWTEXT with appropriate_end_tag_name = "style"
        let mut t = enter_content_model("</style>", State::RAWTEXT, Some("style"));
        // '<' → RAWTEXTLessThanSign
        assert_eq!(t.step(), None);
        // '/' → RAWTEXTEndTagOpen
        assert_eq!(t.step(), None);
        // 's','t','y','l','e' → RAWTEXTEndTagName
        for _ in 0..5 {
            assert_eq!(t.step(), None);
        }
        // '>' → matches → emit tag, switch to Data
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::End,
                name: "style".into(),
                attrs: vec![],
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn rawtext_end_tag_no_match_backout() {
        // `</div>x` in RAWTEXT with expected "style" — should back out
        let mut t = enter_content_model("</div>x", State::RAWTEXT, Some("style"));
        // '<' → RAWTEXTLessThanSign
        assert_eq!(t.step(), None);
        // '/' → RAWTEXTEndTagOpen
        assert_eq!(t.step(), None);
        // 'd','i','v' → RAWTEXTEndTagName
        for _ in 0..3 {
            assert_eq!(t.step(), None);
        }
        // '>' → not appropriate → backout (returns None, pushes pending)
        assert_eq!(t.step(), None);
        // Pending tokens drained
        assert_eq!(t.next_token(), Some(Token::Character('<')));
        assert_eq!(t.next_token(), Some(Token::Character('/')));
        assert_eq!(t.next_token(), Some(Token::Character('d')));
        assert_eq!(t.next_token(), Some(Token::Character('i')));
        assert_eq!(t.next_token(), Some(Token::Character('v')));
        // '>' re-consumed in RAWTEXT
        assert_eq!(t.next_token(), Some(Token::Character('>')));
        assert_eq!(t.state(), State::RAWTEXT);
        assert_eq!(t.next_token(), Some(Token::Character('x')));
    }

    #[test]
    fn rawtext_end_tag_no_appropriate_end_tag() {
        // No appropriate_end_tag_name set — nothing matches
        let mut t = enter_content_model("</style>x", State::RAWTEXT, None);
        // '<' → RAWTEXTLessThanSign
        assert_eq!(t.step(), None);
        // '/' → RAWTEXTEndTagOpen
        assert_eq!(t.step(), None);
        // 's','t','y','l','e' → RAWTEXTEndTagName
        for _ in 0..5 {
            assert_eq!(t.step(), None);
        }
        // '>' → not appropriate (None is not "style") → backout (returns None)
        assert_eq!(t.step(), None);
        // Pending tokens drained
        assert_eq!(t.next_token(), Some(Token::Character('<')));
        assert_eq!(t.next_token(), Some(Token::Character('/')));
        assert_eq!(t.next_token(), Some(Token::Character('s')));
        assert_eq!(t.next_token(), Some(Token::Character('t')));
        assert_eq!(t.next_token(), Some(Token::Character('y')));
        assert_eq!(t.next_token(), Some(Token::Character('l')));
        assert_eq!(t.next_token(), Some(Token::Character('e')));
    }

    #[test]
    fn rawtext_end_tag_name_eof() {
        // §13.2.5.14: EOF has no dedicated arm — falls to "Anything else":
        // emit '<', '/', and each char in temp buffer. Reconsume in RAWTEXT.
        // §13.2.5.3: RAWTEXT on EOF emits EOF token.
        let mut t = enter_content_model("</sty", State::RAWTEXT, Some("style"));
        // '<' → RAWTEXTLessThanSign
        assert_eq!(t.step(), None);
        // '/' → RAWTEXTEndTagOpen
        assert_eq!(t.step(), None);
        // 's','t','y' → RAWTEXTEndTagName
        for _ in 0..3 {
            assert_eq!(t.step(), None);
        }
        // EOF → backout: pushes '<', '/', 's', 't', 'y' to pending, returns None
        assert_eq!(t.step(), None);
        // Pending tokens drained (in order: '<', '/', 's', 't', 'y')
        assert_eq!(t.next_token(), Some(Token::Character('<')));
        assert_eq!(t.next_token(), Some(Token::Character('/')));
        assert_eq!(t.next_token(), Some(Token::Character('s')));
        assert_eq!(t.next_token(), Some(Token::Character('t')));
        assert_eq!(t.next_token(), Some(Token::Character('y')));
        // State is RAWTEXT (not Data), reconsume EOF → RAWTEXT emits EOF
        assert_eq!(t.state(), State::RAWTEXT);
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    #[test]
    fn plaintext_emits_everything_literally() {
        // PLAINTEXT treats `<`, `&`, everything as literal characters
        let mut t = enter_content_model("<div>&amp;</div>", State::PLAINTEXT, None);
        for expected in "<div>&amp;</div>".chars() {
            assert_eq!(t.next_token(), Some(Token::Character(expected)));
        }
    }

    #[test]
    fn plaintext_null_emits_replacement_char() {
        let mut t = enter_content_model("\0x", State::PLAINTEXT, None);
        assert_eq!(t.next_token(), Some(Token::Character('\u{FFFD}')));
        assert_eq!(t.next_token(), Some(Token::Character('x')));
    }

    #[test]
    fn plaintext_eof_emits_eof() {
        let mut t = enter_content_model("", State::PLAINTEXT, None);
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    // ── Script data tests (§13.2.5.4, §13.2.5.15–§13.2.5.31) ────

    #[test]
    fn script_data_emits_characters() {
        let mut t = enter_content_model("abc", State::ScriptData, Some("script"));
        assert_eq!(t.next_token(), Some(Token::Character('a')));
        assert_eq!(t.next_token(), Some(Token::Character('b')));
        assert_eq!(t.next_token(), Some(Token::Character('c')));
    }

    #[test]
    fn script_data_null_emits_replacement_char() {
        let mut t = enter_content_model("\0", State::ScriptData, Some("script"));
        assert_eq!(t.next_token(), Some(Token::Character('\u{FFFD}')));
    }

    #[test]
    fn script_data_ampersand_is_literal() {
        // ScriptData does NOT handle `&` — no character references
        let mut t = enter_content_model("&amp;", State::ScriptData, Some("script"));
        assert_eq!(t.next_token(), Some(Token::Character('&')));
        assert_eq!(t.next_token(), Some(Token::Character('a')));
    }

    #[test]
    fn script_data_lt_switches_to_less_than_sign() {
        let mut t = enter_content_model("<", State::ScriptData, Some("script"));
        assert_eq!(t.step(), None); // '<' → ScriptDataLessThanSign
        assert_eq!(t.state(), State::ScriptDataLessThanSign);
    }

    #[test]
    fn script_data_lt_excl_to_escape_start() {
        // `<!` in ScriptData: '<' → LessThanSign (no emit per §13.2.5.4),
        // '!' → EscapeStart. §13.2.5.15: `!` emits `<` then `!` (via pending_tokens).
        let mut t = enter_content_model("<!", State::ScriptData, Some("script"));
        assert_eq!(t.step(), None); // '<' → LessThanSign (§13.2.5.4: no emit)
        assert_eq!(t.step(), Some(Token::Character('<'))); // '!' → EscapeStart, emit `<`, push `!`
        assert_eq!(t.step(), Some(Token::Character('!'))); // drain `!` from pending_tokens
        assert_eq!(t.state(), State::ScriptDataEscapeStart);
    }

    #[test]
    fn script_data_escape_start_dash_chain() {
        // `<!--` in ScriptData → escape start chain.
        // §13.2.5.15: `!` emits `<` then `!` (via pending_tokens).
        // §13.2.5.18: `-` emits `-`.
        // §13.2.5.19: `-` emits `-`.
        let mut t = enter_content_model("<!--", State::ScriptData, Some("script"));
        assert_eq!(t.step(), None); // '<' → LessThanSign (§13.2.5.4: no emit)
        assert_eq!(t.step(), Some(Token::Character('<'))); // '!' → EscapeStart, emit `<`, push `!`
        assert_eq!(t.step(), Some(Token::Character('!'))); // drain `!` from pending_tokens
        assert_eq!(t.step(), Some(Token::Character('-'))); // '-' → EscapeStartDash, emit '-'
        assert_eq!(t.step(), Some(Token::Character('-'))); // '-' → EscapedDashDash, emit '-'
        assert_eq!(t.state(), State::ScriptDataEscapedDashDash);
    }

    #[test]
    fn script_data_end_tag_match() {
        // `</script>` in ScriptData matches appropriate end tag
        let mut t = enter_content_model("</script>", State::ScriptData, Some("script"));
        // '<' → LessThanSign
        assert_eq!(t.step(), None);
        // '/' → EndTagOpen
        assert_eq!(t.step(), None);
        // 's','c','r','i','p','t' → EndTagName
        for _ in 0..6 {
            assert_eq!(t.step(), None);
        }
        // '>' → match → emit tag, Data
        assert_eq!(
            t.next_token(),
            Some(Token::Tag(TagToken {
                kind: TagKind::End,
                name: "script".into(),
                attrs: vec![],
                self_closing: false,
            }))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn script_data_end_tag_no_match_backout() {
        // `</div>x` in ScriptData — not appropriate, backout
        let mut t = enter_content_model("</div>x", State::ScriptData, Some("script"));
        assert_eq!(t.step(), None); // '<' → LessThanSign
        assert_eq!(t.step(), None); // '/' → EndTagOpen
        for _ in 0..3 {
            assert_eq!(t.step(), None);
        } // 'd','i','v'
        assert_eq!(t.step(), None); // '>' → not appropriate → backout
                                    // Pending tokens drained
        assert_eq!(t.next_token(), Some(Token::Character('<')));
        assert_eq!(t.next_token(), Some(Token::Character('/')));
        assert_eq!(t.next_token(), Some(Token::Character('d')));
        assert_eq!(t.next_token(), Some(Token::Character('i')));
        assert_eq!(t.next_token(), Some(Token::Character('v')));
        // '>' re-consumed in ScriptData
        assert_eq!(t.next_token(), Some(Token::Character('>')));
        assert_eq!(t.state(), State::ScriptData);
    }

    #[test]
    fn script_data_escaped_emits_chars() {
        let mut t = enter_content_model("x", State::ScriptDataEscaped, Some("script"));
        assert_eq!(t.next_token(), Some(Token::Character('x')));
    }

    #[test]
    fn script_data_escaped_dash_dash_gt_exits_escape() {
        // `-->` in escaped mode. §13.2.5.20: `-` emits `-`.
        // §13.2.5.21: `-` emits `-`. §13.2.5.22: `>` emits `>` → ScriptData.
        let mut t = enter_content_model("-->", State::ScriptDataEscaped, Some("script"));
        assert_eq!(t.next_token(), Some(Token::Character('-'))); // '-' → EscapedDash, emit '-'
        assert_eq!(t.next_token(), Some(Token::Character('-'))); // '-' → EscapedDashDash, emit '-'
        assert_eq!(t.next_token(), Some(Token::Character('>'))); // '>' → emit, back to ScriptData
        assert_eq!(t.state(), State::ScriptData);
    }

    #[test]
    fn script_data_escaped_lt_alpha_to_double_escape_start() {
        // `<s` in escaped. §13.2.5.20: `<` does NOT emit (Nothing emitted),
        // switches to EscapedLessThanSign. §13.2.5.23: alpha branch emits `<`
        // and reconsumes in ScriptDataDoubleEscapeStart; the reconsumed `s`
        // is then appended to the temp buffer and emitted.
        let mut t = enter_content_model("<s", State::ScriptDataEscaped, Some("script"));
        assert_eq!(t.step(), None); // '<' → EscapedLessThanSign (no emit)
        assert_eq!(t.step(), Some(Token::Character('<'))); // 's' → alpha, emit '<', reconsume
        assert_eq!(t.step(), Some(Token::Character('s'))); // 's' reconsumed → DoubleEscapeStart, emit 's'
        assert_eq!(t.state(), State::ScriptDataDoubleEscapeStart);
    }

    #[test]
    fn script_data_double_escape_start_script_match() {
        // In DoubleEscapeStart, `script` + `/` → enter DoubleEscaped.
        // §13.2.5.26: each alpha emits the current input character;
        // `/` emits the current input character too.
        let mut t = enter_content_model(
            "script/",
            State::ScriptDataDoubleEscapeStart,
            Some("script"),
        );
        for ch in "script".chars() {
            assert_eq!(t.next_token(), Some(Token::Character(ch)));
        }
        assert_eq!(t.next_token(), Some(Token::Character('/'))); // '/' → DoubleEscaped
        assert_eq!(t.state(), State::ScriptDataDoubleEscaped);
    }

    #[test]
    fn script_data_double_escape_start_not_script() {
        // In DoubleEscapeStart, `foo ` → not "script" → back to Escaped.
        // §13.2.5.26: each alpha and the space emit the current input character.
        let mut t = enter_content_model("foo ", State::ScriptDataDoubleEscapeStart, Some("script"));
        for ch in "foo".chars() {
            assert_eq!(t.next_token(), Some(Token::Character(ch)));
        }
        assert_eq!(t.next_token(), Some(Token::Character(' '))); // ' ' → not "script" → Escaped
        assert_eq!(t.state(), State::ScriptDataEscaped);
    }

    #[test]
    fn script_data_double_escaped_emits_chars() {
        let mut t = enter_content_model("hi", State::ScriptDataDoubleEscaped, Some("script"));
        assert_eq!(t.next_token(), Some(Token::Character('h')));
        assert_eq!(t.next_token(), Some(Token::Character('i')));
    }

    #[test]
    fn script_data_double_escaped_lt_emits_lt() {
        // In DoubleEscaped, '<' emits '<' immediately, switches to DoubleEscapedLessThanSign
        let mut t = enter_content_model("<", State::ScriptDataDoubleEscaped, Some("script"));
        assert_eq!(t.next_token(), Some(Token::Character('<')));
        assert_eq!(t.state(), State::ScriptDataDoubleEscapedLessThanSign);
    }

    #[test]
    fn script_data_double_escaped_dash_dash_gt_to_script_data() {
        // `-->` in double escaped → exit to ScriptData
        let mut t = enter_content_model("-->", State::ScriptDataDoubleEscaped, Some("script"));
        assert_eq!(t.next_token(), Some(Token::Character('-'))); // '-' → DoubleEscapedDash, emit '-'
        assert_eq!(t.next_token(), Some(Token::Character('-'))); // '-' → DoubleEscapedDashDash, emit '-'
        assert_eq!(t.next_token(), Some(Token::Character('>'))); // '>' → emit '>', ScriptData
        assert_eq!(t.state(), State::ScriptData);
    }

    #[test]
    fn script_data_double_escape_end_script_match() {
        // `</script/` in DoubleEscaped. `<` emits (§13.2.5.27), `/` switches
        // to DoubleEscapeEnd and emits `/` (§13.2.5.30). Then `script` chars
        // each emit (§13.2.5.31), and the final `/` emits and exits to Escaped.
        // No tag token is emitted — double-escape is state-only.
        let mut t =
            enter_content_model("</script/", State::ScriptDataDoubleEscaped, Some("script"));
        // '<' → emit '<', switch to DoubleEscapedLessThanSign
        assert_eq!(t.next_token(), Some(Token::Character('<')));
        assert_eq!(t.state(), State::ScriptDataDoubleEscapedLessThanSign);
        // '/' → push '/', switch to DoubleEscapeEnd
        assert_eq!(t.step(), None);
        // pending pops '/'
        assert_eq!(t.next_token(), Some(Token::Character('/')));
        assert_eq!(t.state(), State::ScriptDataDoubleEscapeEnd);
        // 's','c','r','i','p','t' → each emits (§13.2.5.31)
        for ch in "script".chars() {
            assert_eq!(t.next_token(), Some(Token::Character(ch)));
        }
        // '/' → temp buffer == "script" → switch to Escaped, emit '/'
        assert_eq!(t.next_token(), Some(Token::Character('/')));
        assert_eq!(t.state(), State::ScriptDataEscaped);
    }

    // ── Character reference tests (§13.2.5.72–§13.2.5.80) ──────

    #[test]
    fn char_ref_named_amp() {
        // `&amp;` → `&`: Data→CharRef (None), CharRef→NamedCharRef (None),
        // NamedCharRef greedily consumes `amp;` and emits `&` (Some).
        let mut t = HtmlTokenizer::new("&amp;");
        for _ in 0..2 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Character('&')));
    }

    #[test]
    fn char_ref_named_lt() {
        // `&lt;` → `<`: 2 Nones (Data, CharRef), then NamedCharRef emits.
        let mut t = HtmlTokenizer::new("&lt;");
        for _ in 0..2 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Character('<')));
    }

    #[test]
    fn char_ref_named_gt() {
        // `&gt;` → `>`: 2 Nones (Data, CharRef), then NamedCharRef emits.
        let mut t = HtmlTokenizer::new("&gt;");
        for _ in 0..2 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Character('>')));
    }

    #[test]
    fn char_ref_named_quot() {
        // `&quot;` → `"`: 2 Nones (Data, CharRef), then NamedCharRef emits.
        let mut t = HtmlTokenizer::new("&quot;");
        for _ in 0..2 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Character('"')));
    }

    #[test]
    fn char_ref_numeric_decimal() {
        let mut t = HtmlTokenizer::new("&#60;");
        for _ in 0..7 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Character('<')));
    }

    #[test]
    fn char_ref_numeric_hex() {
        let mut t = HtmlTokenizer::new("&#x3C;");
        for _ in 0..7 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Character('<')));
    }

    #[test]
    fn char_ref_not_a_ref_space() {
        // `& a` → emit `&`, then ` ` in Data
        let mut t = HtmlTokenizer::new("& a");
        assert_eq!(t.step(), None); // '&' → CharacterReference
        assert_eq!(t.next_token(), Some(Token::Character('&'))); // ' ' → fallback
        assert_eq!(t.next_token(), Some(Token::Character(' '))); // reconsume ' '
        assert_eq!(t.next_token(), Some(Token::Character('a')));
    }

    #[test]
    fn char_ref_ampersand_not_ref() {
        // `&&` → emit `&`, reconsume `&`
        let mut t = HtmlTokenizer::new("&&");
        assert_eq!(t.step(), None); // '&' → CharacterReference
        assert_eq!(t.next_token(), Some(Token::Character('&'))); // '&' → fallback
        assert_eq!(t.step(), None); // reconsume '&' → CharacterReference
        assert_eq!(t.next_token(), Some(Token::Character('&'))); // at EOF → fallback
    }

    #[test]
    fn char_ref_numeric_null() {
        // `&#x00;` → U+FFFD: '&' '#' 'x' '0' '0' ';' (5 Nones → Some)
        let mut t = HtmlTokenizer::new("&#x00;");
        for _ in 0..7 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Character('\u{FFFD}')));
    }

    #[test]
    fn char_ref_numeric_win1252() {
        let mut t = HtmlTokenizer::new("&#x80;");
        for _ in 0..7 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Character('\u{20AC}')));
    }

    #[test]
    fn char_ref_unknown_entity_ambiguous() {
        // `&unknownfoo;` — not in entity table → flush `&` + name as literal
        let mut t = HtmlTokenizer::new("&unknownfoo;");
        assert_eq!(next_real_token(&mut t), Token::Character('&'));
        assert_eq!(next_real_token(&mut t), Token::Character('u'));
        assert_eq!(next_real_token(&mut t), Token::Character('n'));
        assert_eq!(next_real_token(&mut t), Token::Character('k'));
        assert_eq!(next_real_token(&mut t), Token::Character('n'));
        assert_eq!(next_real_token(&mut t), Token::Character('o'));
        assert_eq!(next_real_token(&mut t), Token::Character('w'));
        assert_eq!(next_real_token(&mut t), Token::Character('n'));
        assert_eq!(next_real_token(&mut t), Token::Character('f'));
        assert_eq!(next_real_token(&mut t), Token::Character('o'));
        assert_eq!(next_real_token(&mut t), Token::Character('o'));
        assert_eq!(next_real_token(&mut t), Token::Character(';'));
    }

    #[test]
    fn char_ref_notin_entity() {
        // `&notin;` → ∉ (U+2209): §13.2.5.78 longest match picks `notin;`
        // over legacy `not`. 2 Nones (Data, CharRef), then NamedCharRef emits.
        let mut t = HtmlTokenizer::new("&notin;");
        for _ in 0..2 {
            assert_eq!(t.step(), None);
        }
        assert_eq!(t.next_token(), Some(Token::Character('\u{2209}')));
    }

    #[test]
    fn char_ref_e2e_data_state() {
        // `&lt;a&gt;` → `<a>`
        let mut t = HtmlTokenizer::new("&lt;a&gt;");
        for _ in 0..2 {
            assert_eq!(t.step(), None);
        } // &lt; resolved
        assert_eq!(t.next_token(), Some(Token::Character('<')));
        assert_eq!(t.next_token(), Some(Token::Character('a')));
        for _ in 0..2 {
            assert_eq!(t.step(), None);
        } // &gt; resolved
        assert_eq!(t.next_token(), Some(Token::Character('>')));
        assert_eq!(t.next_token(), Some(Token::EOF));
    }

    // ── CDATA section tests (§13.2.5.69–§13.2.5.71) ─────────

    #[test]
    fn cdata_emits_characters() {
        // §13.2.5.42: `<![CDATA[hello]]>` in HTML content is a
        // cdata-in-html-content parse error. A comment token with data
        // "[CDATA[" is created, and the tokenizer switches to bogus
        // comment state. The bogus comment state appends remaining
        // characters until `>`, then emits the comment.
        let mut t = HtmlTokenizer::new("<![CDATA[hello]]>");
        assert_eq!(t.step(), None); // Data → TagOpen
        assert_eq!(t.step(), None); // TagOpen → MarkupDeclarationOpen
        assert_eq!(t.step(), None); // "[CDATA[" → BogusComment (comment data = "[CDATA[")
                                    // BogusComment consumes "hello]]" and emits on ">"
        assert_eq!(
            t.next_token(),
            Some(Token::Comment("[CDATA[hello]]".to_string()))
        );
        assert_eq!(t.state(), State::Data);
    }

    #[test]
    fn cdata_null_emits_character() {
        // §13.2.5.69 CDATA section state has no NUL arm — NUL falls to
        // "Anything else" → emit as character token. NUL→FFFD replacement
        // happens in tree construction, not tokenization.
        let mut t = enter_content_model("\0x", State::CDATASection, None);
        assert_eq!(t.next_token(), Some(Token::Character('\0')));
        assert_eq!(t.next_token(), Some(Token::Character('x')));
    }

    #[test]
    fn cdata_bracket_not_followed_by_bracket() {
        // `]x` in CDATA: `]` → Bracket, `x` → emit `]` + reconsume
        let mut t = enter_content_model("]x", State::CDATASection, None);
        assert_eq!(t.step(), None); // ']' → Bracket
        assert_eq!(t.next_token(), Some(Token::Character(']'))); // 'x' → emit ']'
        assert_eq!(t.next_token(), Some(Token::Character('x'))); // reconsume 'x' in CDATA
    }

    #[test]
    fn cdata_double_bracket_then_not_gt() {
        // `]]x` in CDATA: ']' → Bracket, ']' → End, 'x' → emit ']]' + reconsume
        let mut t = enter_content_model("]]x", State::CDATASection, None);
        assert_eq!(t.step(), None); // ']' → Bracket
        assert_eq!(t.step(), None); // ']' → End
        assert_eq!(t.step(), None); // 'x' → emit ']]' (pending), reconsume
        assert_eq!(t.next_token(), Some(Token::Character(']'))); // pending ']'
        assert_eq!(t.next_token(), Some(Token::Character(']'))); // pending ']'
        assert_eq!(t.next_token(), Some(Token::Character('x'))); // reconsume 'x'
    }

    #[test]
    fn cdata_end_extra_bracket() {
        // `]]]` → third bracket emitted, stays in End
        let mut t = enter_content_model("]]]", State::CDATASection, None);
        assert_eq!(t.step(), None); // ']' → Bracket
        assert_eq!(t.step(), None); // ']' → End
        assert_eq!(t.next_token(), Some(Token::Character(']'))); // ']' → emit, stay in End
    }

    // ── Named character reference tests (§13.2.5.77) ──────────────

    /// Helper: collect all tokens from a tokenizer into a Vec.
    fn collect_tokens(input: &str) -> Vec<Token> {
        let mut t = HtmlTokenizer::new(input);
        let mut out = Vec::new();
        while let Some(tok) = t.next_token() {
            out.push(tok);
        }
        out
    }

    #[test]
    fn named_entity_legacy_with_semi_resolves() {
        // &amp; → & (legacy entity, both forms exist in table)
        let tokens = collect_tokens("&amp;");
        assert_eq!(tokens, vec![Token::Character('&'), Token::EOF]);
    }

    #[test]
    fn named_entity_legacy_without_semi_resolves() {
        // &amp → & (legacy entity, no semicolon needed)
        let tokens = collect_tokens("&amp");
        assert_eq!(tokens, vec![Token::Character('&'), Token::EOF]);
    }

    #[test]
    fn non_legacy_entity_with_semi_resolves() {
        // &Abreve; → Ă (non-legacy, REQUIRES semicolon)
        let tokens = collect_tokens("&Abreve;");
        assert_eq!(tokens, vec![Token::Character('\u{0102}'), Token::EOF]);
    }

    #[test]
    fn non_legacy_entity_without_semi_is_literal() {
        // &Abreve (no semicolon) → literal "&Abreve" (not in table without ;)
        let tokens = collect_tokens("&Abreve");
        let mut expected: Vec<Token> = "&Abreve".chars().map(Token::Character).collect();
        expected.push(Token::EOF);
        assert_eq!(tokens, expected);
    }

    #[test]
    fn unknown_entity_with_semi_is_literal() {
        // &rrrraannddom; → literal "&rrrraannddom;" (unknown entity)
        let tokens = collect_tokens("&rrrraannddom;");
        let mut expected: Vec<Token> = "&rrrraannddom;".chars().map(Token::Character).collect();
        expected.push(Token::EOF);
        assert_eq!(tokens, expected);
    }

    #[test]
    fn attr_value_entity_no_extra_character_token() {
        // <h a="&amp;"> → only StartTag, NO extra Character token emitted
        // by the character reference resolution in attribute context.
        let tokens = collect_tokens("<h a=\"&amp;\">");
        assert_eq!(tokens.len(), 2);
        match &tokens[0] {
            Token::Tag(tg) => {
                assert_eq!(tg.name, "h");
                assert_eq!(tg.kind, TagKind::Start);
                assert_eq!(tg.attrs.len(), 1);
                assert_eq!(tg.attrs[0].0, "a");
                assert_eq!(tg.attrs[0].1, "&");
            }
            other => panic!("expected StartTag, got {:?}", other),
        }
        assert_eq!(tokens[1], Token::EOF);
    }

    #[test]
    fn attr_context_not_equals_is_literal() {
        // <h a="&not="> → attr value is literal "&not="
        // (attr-context rule: legacy `not` without `;` followed by `=`)
        let tokens = collect_tokens("<h a=\"&not=\">");
        assert_eq!(tokens.len(), 2);
        match &tokens[0] {
            Token::Tag(tg) => {
                assert_eq!(tg.name, "h");
                assert_eq!(tg.attrs[0].0, "a");
                assert_eq!(tg.attrs[0].1, "&not=");
            }
            other => panic!("expected StartTag, got {:?}", other),
        }
    }
}
