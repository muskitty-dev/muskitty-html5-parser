//! Parse error types for the HTML parser.
//!
//! See WHATWG HTML §13.2.6 for the list of parse errors that can be
//! emitted during tree construction.

/// A parse error encountered during tree construction.
///
/// The specific error types follow the naming used in WHATWG §13.2.6.
/// Not all error types are implemented yet; the skeleton uses `Generic`
/// for errors that will be specialized in Phase 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The DOCTYPE is invalid (wrong name, public ID, or system ID).
    /// Per §13.2.6.2 Initial insertion mode.
    InvalidDoctype,
    /// A character was found where it's not expected.
    UnexpectedCharacter(char),
    /// A start tag was found where it's not expected.
    UnexpectedStartTag(String),
    /// An end tag was found where it's not expected.
    UnexpectedEndTag(String),
    /// Generic parse error with a static description.
    Generic(&'static str),
}
