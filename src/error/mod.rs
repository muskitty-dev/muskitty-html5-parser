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
    /// 输入字节数超过 `MAX_INPUT_BYTES`。
    /// 触发后解析立即停止，返回部分结果（仅含截止点已构建的 DOM）。
    /// 参考 Chromium `kMaxHTMLDocumentSize`、WebKit 的输入大小保护策略。
    InputTooLarge { actual: usize, limit: usize },
    /// open elements 栈深度超过 `MAX_OPEN_ELEMENTS`。
    /// 触发后当前元素的 push 被跳过，但解析继续（参考 WHATWG §13.2.6
    /// 错误恢复语义：parser 不应因资源限制而崩溃）。跳过 push 意味着
    /// 后续 end tag 可能匹配错误节点，但这是降级可接受代价。
    /// 参考 Chromium `kMaxHTMLParserDOMDepth = 512`、WebKit `maxDOMTreeDepth = 500`。
    DomDepthExceeded { depth: usize, limit: usize },
    /// 同一 token 的 reprocess 次数超过 `MAX_REPROCESS_COUNT`。
    /// 触发后停止处理当前 token（等价于 WHATWG §13.2.6 "stop parsing"
    /// 的降级恢复语义），解析继续处理后续 token，而不是 panic。
    ReprocessLimitExceeded { limit: u32 },
}
