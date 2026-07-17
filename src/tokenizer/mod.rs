//! HTML Tokenizer types and trait.
//!
//! Implements the tokenization stage of the WHATWG HTML parsing model
//! (§13.2.5 Tokenization).
//!
//! The tokenizer consumes a stream of Unicode code points and emits
//! tokens (start tags, end tags, comments, characters, DOCTYPEs, EOF).
//! These tokens are consumed by the tree construction stage to build
//! the DOM tree.

mod entities;
mod impls;
mod trait_def;
mod types;

pub use impls::HtmlTokenizer;
pub use trait_def::Tokenizer;
pub use types::{DoctypeToken, State, TagKind, TagToken, Token};
