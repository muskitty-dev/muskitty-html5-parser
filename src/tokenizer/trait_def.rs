//! Tokenizer trait definition.
//!
//! WHATWG HTML Spec §13.2.5 Tokenization.

use super::types::{State, Token};

/// The HTML tokenizer.
///
/// Implements the WHATWG tokenization state machine (§13.2.5).
///
/// The tokenizer consumes Unicode code points from an input stream and emits
/// tokens (start tags, end tags, comments, characters, DOCTYPEs, EOF). The
/// tree construction stage consumes these tokens to build the DOM.
///
/// # Reentrancy
///
/// Per §13.2.1, the tokenizer is reentrant: the tree construction stage may
/// pause parsing (e.g. for `<script>` execution), then resume the tokenizer.
/// Implementations must support querying and mutating the tokenizer state so
/// the tree construction stage can switch content models (e.g., Data →
/// RCDATA when entering `<title>`).
pub trait Tokenizer {
    /// Advance the tokenizer and return the next token from the input stream.
    ///
    /// Implements the main loop of §13.2.5: consume the current input
    /// character, follow the state transition for the current state, and
    /// return the emitted token (if any). Some state transitions emit
    /// tokens immediately; others set the current token and continue
    /// consuming input until a token is ready.
    ///
    /// Returns `None` after `Token::EOF` has been emitted — the input
    /// stream is exhausted and no further tokens will be produced.
    fn next_token(&mut self) -> Option<Token>;

    /// Set the current tokenizer state.
    ///
    /// §13.2.5: The tree construction stage sets the tokenizer state to
    /// switch content models. For example:
    /// - Encountering `<title>` → state set to `State::RCDATA`
    /// - Encountering `<script>` → state set to `State::ScriptData`
    /// - Encountering `<textarea>` → state set to `State::RCDATA`
    fn set_state(&mut self, state: State);

    /// Return the current tokenizer state.
    fn state(&self) -> State;

    /// Reset the tokenizer to its initial state.
    ///
    /// Clears any partially-built token, current input character tracking,
    /// and resets the state machine to the [`State::Data`] state.
    ///
    /// Used for fragment parsing (§13.2.5): when the tree construction stage
    /// inserts a new input stream (e.g., via `document.write()`), the
    /// tokenizer must be reset to process the new fragment from a known
    /// starting state without artifacts from the previous stream.
    fn reset(&mut self);

    /// Set the "appropriate end tag name" used by RCDATA/RAWTEXT/ScriptData
    /// end tag matching (§13.2.5.9–§13.2.5.14).
    ///
    /// The tree construction stage sets this when switching the tokenizer to
    /// a content model that can contain end tags. For example, when entering
    /// `<title>`, tree construction sets the appropriate end tag name to
    /// `"title"` so that the tokenizer recognises `</title>` as an end tag
    /// token rather than literal text. Pass `None` to clear when leaving the
    /// content model.
    fn set_appropriate_end_tag_name(&mut self, name: Option<&str>);

    /// Notify the tokenizer whether the adjusted current node is in foreign
    /// content (not in the HTML namespace).
    ///
    /// The tree construction stage sets this before requesting the next token
    /// so that the markup declaration open state (§13.2.5.42) can decide
    /// between the CDATA section state (foreign content) and the bogus
    /// comment state (HTML content) when encountering `<![CDATA[`.
    ///
    /// Defaults to `false` (HTML content).
    fn set_foreign_content(&mut self, in_foreign: bool);
}
