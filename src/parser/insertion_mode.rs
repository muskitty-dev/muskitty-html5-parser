//! Tree construction insertion modes.
//!
//! See WHATWG HTML §13.2.6.1: "The insertion mode is a state variable that
//! controls the primary operation of the tree construction stage."

/// The 23 insertion modes defined in WHATWG §13.2.6.1.
///
/// Each variant corresponds to a section of the spec that defines how tokens
/// are handled in that mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionMode {
    /// §13.2.6.2 — Initial insertion mode.
    Initial,
    /// §13.2.6.3 — Before html insertion mode.
    BeforeHtml,
    /// §13.2.6.4 — Before head insertion mode.
    BeforeHead,
    /// §13.2.6.5 — In head insertion mode.
    InHead,
    /// §13.2.6.6 — In head noscript insertion mode.
    InHeadNoscript,
    /// §13.2.6.7 — After head insertion mode.
    AfterHead,
    /// §13.2.6.4 — In body insertion mode (the most complex mode).
    InBody,
    /// §13.2.6.5 — Text insertion mode.
    Text,
    /// §13.2.6.7 — In table insertion mode.
    InTable,
    /// §13.2.6.8 — In table text insertion mode.
    InTableText,
    /// §13.2.6.9 — In caption insertion mode.
    InCaption,
    /// §13.2.6.10 — In column group insertion mode.
    InColumnGroup,
    /// §13.2.6.11 — In table body insertion mode.
    InTableBody,
    /// §13.2.6.12 — In row insertion mode.
    InRow,
    /// §13.2.6.13 — In cell insertion mode.
    InCell,
    /// §13.2.6.16 — In template insertion mode.
    InTemplate,
    /// §13.2.6.17 — After body insertion mode.
    AfterBody,
    /// §13.2.6.18 — In frameset insertion mode.
    InFrameset,
    /// §13.2.6.19 — After frameset insertion mode.
    AfterFrameset,
    /// §13.2.6.20 — After after body insertion mode.
    AfterAfterBody,
    /// §13.2.6.21 — After after frameset insertion mode.
    AfterAfterFrameset,
}
