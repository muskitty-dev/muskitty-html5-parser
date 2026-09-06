//! Tree construction stage (§13.2.6).
//!
//! Consumes the token stream produced by the tokenizer and builds the DOM
//! tree per the WHATWG HTML insertion mode state machine.
//!
//! # Architecture
//!
//! - [`HtmlTreeConstructor`] holds the parser state: open elements stack,
//!   active formatting elements list, current insertion mode, and flags.
//! - [`dispatch`] routes each token to the handler for the current insertion
//!   mode.
//! - [`helpers`] contains the "insert a node" / "create an element" helper
//!   algorithms from §13.2.6.2.
//! - [`insertion_mode`] defines the 23 insertion modes from §13.2.6.1.

pub(crate) mod dispatch;
mod foreign;
mod helpers;
mod insertion_mode;

pub use insertion_mode::InsertionMode;

use std::cell::RefCell;
use std::rc::Rc;

use muskitty_dom::Node;

use crate::error::ParseError;
use muskitty_html5_tokenizer::{Token, Tokenizer};

/// 同一 token 最大 reprocess 次数。
///
/// WHATWG §13.2.6 中 reprocess 是状态机正常机制：每次 reprocess 都会
/// 切换 insertion mode 后重新处理同一 token。若某个畸形输入导致
/// insertion mode 无法收敛（例如 Text mode 中无 original mode 的 EOF），
/// 连续 reprocess 会形成死循环。超过此上限后停止处理当前 token
/// （等价于规范允许的 "stop parsing" 降级），避免无限循环/panic。
pub const MAX_REPROCESS_COUNT: u32 = 50;

/// 活动格式化元素列表（AFE, §13.2.4.3）硬上限。
///
/// 审计 F-7：Noah's Ark 子句只逐出第 3 个"完全相同"的条目，`<b a=1><b
/// a=2><b a=3>…`（属性各异）令列表无界增长，而每个后续格式化 start/end
/// tag 都要全列表扫描（Noah's Ark / `find_formatting_element` / AAA 的
/// `in_afe`）——O(n²) 挂起（~17 万条目即数十秒）。超限移除最后一个
/// marker 之后的最早元素（Noah's Ark 作用域一致），与浏览器的
/// "Noah's Ark + 列表上限"实践对齐。256 远超真实页面的活动格式化元素数。
pub const MAX_ACTIVE_FORMATTING_ELEMENTS: usize = 256;

/// An entry in the list of active formatting elements (§13.2.6.2).
///
/// The list holds either a reference to an element on the open elements
/// stack, or a marker that delimits a section (pushed when entering table
/// contexts, template content, etc.).
#[derive(Clone)]
pub enum ActiveFormattingEntry {
    /// A marker entry, used to delimit sections of the list.
    Marker,
    /// An element entry, holding a reference to the formatting element.
    Element(Rc<RefCell<Node>>),
}

/// The HTML tree construction stage.
///
/// Holds the state of the insertion mode state machine (§13.2.6) and the
/// DOM tree being built. The `document` field is the output root; the
/// `open_elements` stack tracks the current open element chain.
pub struct HtmlTreeConstructor {
    /// The output Document node. Inserted elements are ultimately attached
    /// here (directly or via the `<html>` / `<head>` / `<body>` chain).
    pub document: Rc<RefCell<Node>>,
    /// The stack of open elements (§13.2.6.2). The top is the current node.
    pub open_elements: Vec<Rc<RefCell<Node>>>,
    /// The list of active formatting elements (§13.2.6.2). Used by the
    /// adoption agency algorithm; populated in Phase 3.3.
    pub active_formatting_elements: Vec<ActiveFormattingEntry>,
    /// The current insertion mode (§13.2.6.1).
    pub insertion_mode: InsertionMode,
    /// The original insertion mode, saved when entering Text mode or
    /// template content (§13.2.6.5, §13.2.6.16).
    pub original_insertion_mode: Option<InsertionMode>,
    /// The `<head>` element pointer, set in BeforeHead mode (§13.2.6.4).
    pub head_element: Option<Rc<RefCell<Node>>>,
    /// The `<form>` element pointer, updated in InBody mode (§13.2.6.4).
    pub form_element: Option<Rc<RefCell<Node>>>,
    /// Whether foster parenting is active (§13.2.6.3). Used by table
    /// insertion modes; deferred to Phase 3.4.
    pub foster_parenting: bool,
    /// Pending character tokens accumulated in InTableText mode
    /// (§13.2.6.4.10). Flushed when a non-character token is seen.
    pub pending_table_text: String,
    /// The stack of template insertion modes (§13.2.6.4.19). Pushed when
    /// entering `<template>` content, popped when leaving.
    pub template_insertion_modes: Vec<InsertionMode>,
    /// The "frameset-ok" flag (§13.2.6.1). Initially true; set to false by
    /// certain tokens that prevent subsequent `<frameset>`.
    pub frameset_ok: bool,
    /// The scripting flag (§13.2.6.1). Defaults to false for non-scripting
    /// parsers; affects `<noscript>` handling and template content.
    pub scripting_flag: bool,
    /// Parse errors accumulated during tree construction (§13.2.6).
    pub errors: Vec<ParseError>,
    /// Whether the next U+000A LF character token should be ignored.
    /// Set by `<pre>`/`<listing>` start tags per §13.2.6.4.7: "If the next
    /// token is a U+000A LINE FEED (LF) character token, then ignore that
    /// token and move on to the next one."
    pub skip_next_lf: bool,
    /// Whether the Document is in quirks mode (§13.2.6.4.1). Set by the
    /// DOCTYPE token in Initial mode, or by the "anything else" branch of
    /// Initial mode (no DOCTYPE → quirks). Affects `<table>` handling in
    /// InBody (§13.2.6.4.7: in quirks mode, `<p>` is not closed before a
    /// `<table>`).
    pub quirks_mode: bool,
    /// open elements 栈深度上限。超过时 push 被跳过并记录
    /// `ParseError::DomDepthExceeded`，解析继续（参考 WHATWG §13.2.6
    /// 错误恢复语义）。由 `parse_with_limits` 设置；默认为
    /// [`crate::MAX_OPEN_ELEMENTS`]。
    pub max_open_elements: usize,
    /// The recreated context element (§13.4.2 step 6). Set only during
    /// fragment parsing; used by "reset the insertion mode appropriately"
    /// (§13.2.6.4.1) to substitute the stack root's local name.
    pub fragment_context: Option<Rc<RefCell<Node>>>,
    /// One-shot flag: the next dispatch of the current token must skip the
    /// foreign-content dispatcher and go straight to the insertion-mode
    /// rules. Set by the foreign-content HTML breakout steps (§13.2.6.5
    /// "reprocess the token"): without it the reprocessed token would hit
    /// the adjusted current node (the fragment context) again and loop.
    pub skip_foreign_dispatch_once: bool,
    /// The DocumentFragment being built (§13.4.2). Set only during fragment
    /// parsing; the parsed content is unwrapped into it at the end.
    pub fragment_root: Option<Rc<RefCell<Node>>>,
    /// selectedness 算法的增量备忘录（审计 F-8，见
    /// [`helpers::on_option_inserted_with_memo`]）。解析器内部状态。
    pub select_selectedness_memo: Option<SelectSelectednessMemo>,
    /// selectedcontent 查找备忘录（审计 P-1，见
    /// [`helpers::maybe_clone_option_into_selectedcontent`]）。解析器内部状态。
    pub select_selectedcontent_memo: Option<SelectSelectedcontentMemo>,
}

/// [`on_option_inserted_with_memo`](crate::parser::helpers) 的备忘录条目
/// （审计 F-8）。`select` 以 `Weak` 持有：升级失败或非同一 select 即失效，
/// 防 Rc 地址复用误命中。
pub struct SelectSelectednessMemo {
    /// 备忘录所属的 `<select>`。
    pub select: std::rc::Weak<RefCell<Node>>,
    /// 截至上次完整算法执行后，该 select 下是否存在 selectedness=true
    /// 的 option。
    pub has_selected: bool,
    /// 截至上次完整算法执行后，是否全部 option 均 disabled（仅在
    /// `!has_selected` 时被消费）。
    pub all_disabled: bool,
    /// `has_selected` 时指向唯一选中的 option（P-1）。算法 step 2 恒保
    /// 树序最后一个，故至多一个选中；带 `selected` 的新插入据此做 O(1)
    /// 定点取消旧选中，免去全子树扫描。`Weak` 防 Rc 地址复用误命中，
    /// 升级失败时调用方回退完整算法。
    pub last_selected: std::rc::Weak<RefCell<Node>>,
}

/// [`helpers::maybe_clone_option_into_selectedcontent`] 的 selectedcontent
/// 查找备忘录（审计 P-1）。option pop 钩子对每个**仍选中**的 option 查找
/// select 下首个 `selectedcontent`，原实现全子树 DFS——`<option selected>`
/// × 65k 时每次 pop 时该 option 尚未被后续插入取消选中，构成残余 O(n²)
/// （~2×10⁹ 次节点访问）。备忘录缓存查找结果：
///
/// - `Absent`：上次查找确认该 select 子树内无 selectedcontent。此后仅有
///   **新的 selectedcontent 插入**能使其失效——由
///   [`helpers::push_open_element`] 在插入时翻转为 `Present`（解析器
///   只 append，树序首个一旦存在即不变）。
/// - `Present(Weak)`：树序首个 selectedcontent；升级失败（解析期节点
///   只增不删，不应发生）回退全子树重扫。
///
/// 单槽备忘录正确性：解析器同一时刻只向**当前打开**的 select 子树插入
/// （select 关闭后插入点永不再进入其子树），所有写入方（插入钩子/重扫）
/// 均针对当前打开的 select。
pub struct SelectSelectedcontentMemo {
    /// 备忘录所属的 `<select>`（`Weak` 防地址复用误命中）。
    pub select: std::rc::Weak<RefCell<Node>>,
    /// 查找结果缓存。
    pub lookup: SelectedcontentLookup,
}

/// selectedcontent 查找结果缓存（P-1）。
pub enum SelectedcontentLookup {
    /// 该 select 子树内确认无 selectedcontent。
    Absent,
    /// 树序首个 selectedcontent。
    Present(std::rc::Weak<RefCell<Node>>),
}

impl HtmlTreeConstructor {
    /// Create a new tree constructor that will build into `document`.
    ///
    /// Per §13.2.6.1, the initial insertion mode is `Initial`, the
    /// frameset-ok flag is true, and the scripting flag defaults to false.
    pub fn new(document: Rc<RefCell<Node>>) -> Self {
        Self {
            document,
            open_elements: Vec::new(),
            active_formatting_elements: Vec::new(),
            insertion_mode: InsertionMode::Initial,
            original_insertion_mode: None,
            head_element: None,
            form_element: None,
            foster_parenting: false,
            pending_table_text: String::new(),
            template_insertion_modes: Vec::new(),
            frameset_ok: true,
            scripting_flag: false,
            errors: Vec::new(),
            skip_next_lf: false,
            quirks_mode: false,
            max_open_elements: crate::MAX_OPEN_ELEMENTS,
            fragment_context: None,
            skip_foreign_dispatch_once: false,
            fragment_root: None,
            select_selectedness_memo: None,
            select_selectedcontent_memo: None,
        }
    }

    /// Return the current node (§13.2.6.2).
    ///
    /// The current node is the top of the open elements stack. If the
    /// stack is empty (before any element is pushed), the current node is
    /// the Document itself.
    pub fn current_node(&self) -> Rc<RefCell<Node>> {
        self.open_elements
            .last()
            .cloned()
            .unwrap_or_else(|| self.document.clone())
    }

    /// Whether the adjusted current node is in foreign content (i.e., not
    /// in the HTML namespace).
    ///
    /// Used by the tokenizer to decide between CDATA section state and
    /// bogus comment state when encountering `<![CDATA[` (§13.2.5.42).
    /// Returns `false` when the stack is empty (no adjusted current node)
    /// or the current node is in the HTML namespace.
    ///
    /// §13.2.4 fragment case: when the stack holds only the synthetic
    /// `<html>` root, the adjusted current node is the fragment context
    /// element, so an `svg`/`math` context routes `<![CDATA[` to the CDATA
    /// section state.
    pub fn current_node_in_foreign_content(&self) -> bool {
        if self.fragment_context.is_some() && self.open_elements.len() == 1 {
            if let Some(ctx) = &self.fragment_context {
                let n = ctx.borrow();
                return matches!(&n.kind, muskitty_dom::NodeKind::Element(e)
                    if e.namespace != muskitty_dom::Namespace::Html);
            }
        }
        match self.open_elements.last() {
            Some(node) => {
                let n = node.borrow();
                matches!(&n.kind, muskitty_dom::NodeKind::Element(e)
                    if e.namespace != muskitty_dom::Namespace::Html)
            }
            None => false,
        }
    }

    /// Feed a single token to the tree construction state machine.
    ///
    /// Dispatches the token to the handler for the current insertion mode.
    /// If the handler returns `Step::Reprocess`, the same token is fed again
    /// to the (now switched) insertion mode. This loop terminates because
    /// every reprocess step must change `insertion_mode` or return `Done`.
    ///
    /// The `tokenizer` is passed so handlers can switch the tokenizer's
    /// content model (e.g. to RCDATA for `<title>`, per §13.2.6.4.4).
    pub fn run(&mut self, token: &Token, tokenizer: &mut dyn Tokenizer) {
        // §13.2.6.4.7: pre/listing start tags cause the parser to skip a
        // single leading U+000A LF character token (authoring convenience).
        if self.skip_next_lf {
            self.skip_next_lf = false;
            if let Token::Character('\n') = token {
                return;
            }
        }
        let mut reprocess_count = 0u32;
        loop {
            match dispatch::dispatch(self, token, tokenizer) {
                dispatch::Step::Done => return,
                dispatch::Step::Reprocess => {
                    reprocess_count += 1;
                    if reprocess_count > MAX_REPROCESS_COUNT {
                        // §13.2.6 错误恢复语义：不因畸形输入崩溃，记录
                        // 错误并停止处理当前 token，继续后续 token。
                        self.errors.push(ParseError::ReprocessLimitExceeded {
                            limit: MAX_REPROCESS_COUNT,
                        });
                        return;
                    }
                    continue;
                }
            }
        }
    }

    /// Run the "stop parsing" finalization step (§13.2.7 step 4): pop all
    /// nodes off the stack of open elements. This fires the maybe-clone
    /// hook (§4.10.10) for any open `<option>` elements, ensuring their
    /// content is mirrored into `<selectedcontent>` before the document
    /// is returned.
    pub fn finalize(&mut self) {
        while !self.open_elements.is_empty() {
            helpers::pop_open_element(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ParseError;
    use muskitty_dom::Node;
    use muskitty_html5_tokenizer::HtmlTokenizer;

    #[test]
    fn reprocess_limit_records_error_instead_of_panicking() {
        // Text mode 中 EOF 且无 original mode：每次 reprocess 后仍停留在
        // Text mode，触发 reprocess 死循环 → 应记录 ReprocessLimitExceeded
        // 而非 panic（回归 H-3）。
        let document = Node::new_document();
        let mut constructor = HtmlTreeConstructor::new(document);
        constructor.insertion_mode = InsertionMode::Text;

        let mut tokenizer = HtmlTokenizer::new("");
        constructor.run(&Token::EOF, &mut tokenizer);

        assert!(
            constructor
                .errors
                .iter()
                .any(|e| matches!(e, ParseError::ReprocessLimitExceeded { .. })),
            "expected ReprocessLimitExceeded error, got {:?}",
            constructor.errors
        );
    }

    // —— F-7: AFE 列表硬上限 ——

    #[test]
    fn afe_list_capped_under_distinct_formatting_elements() {
        // `<b a=i>` 属性各异绕过 Noah's Ark 逐出（它只逐第 3 个完全相同
        // 条目），修复前列表无界增长；白盒断言上限生效。
        let mut input = String::new();
        for i in 0..600 {
            input.push_str(&format!("<b a={i}>"));
        }
        let document = Node::new_document();
        let mut constructor = HtmlTreeConstructor::new(document);
        let mut tokenizer = HtmlTokenizer::new(&input);
        while let Some(tok) = tokenizer.next_token() {
            if matches!(tok, muskitty_html5_tokenizer::Token::EOF) {
                break;
            }
            constructor.run(&tok, &mut tokenizer);
        }
        assert_eq!(
            constructor.active_formatting_elements.len(),
            crate::parser::MAX_ACTIVE_FORMATTING_ELEMENTS,
            "AFE list must be capped at MAX_ACTIVE_FORMATTING_ELEMENTS"
        );
    }

    #[test]
    fn large_distinct_formatting_input_parses_completely() {
        // 端到端冒烟：3000 个属性各异的 `<b>` 正常构建（修复前 3k 规模
        // 的 Noah's Ark 扫描已 ~4.5×10^6 次条目比较）。更大规模（5 万）
        // 的剩余成本来自 reconstruct 放大（审计 H-M1，P2 项，超本轮
        // 范围），不放进 CI。
        let mut input = String::new();
        for i in 0..3_000 {
            input.push_str(&format!("<b a={i}>"));
        }
        let doc = crate::parse(&input);
        assert!(
            doc.borrow().first_element_child().is_some(),
            "document must still build normally"
        );
    }

    // —— F-8: selectedness 增量备忘录 ——

    fn collect_options(
        node: &std::rc::Rc<std::cell::RefCell<Node>>,
        out: &mut Vec<std::rc::Rc<std::cell::RefCell<Node>>>,
    ) {
        let children: Vec<_> = node.borrow().child_nodes().to_vec();
        for c in children {
            let is_option = c
                .borrow()
                .kind
                .as_element()
                .map(|e| e.local_name == "option")
                .unwrap_or(false);
            if is_option {
                out.push(c.clone());
            }
            collect_options(&c, out);
        }
    }

    /// 解析 html 并按文档顺序返回各 `<option>` 的 selectedness。
    fn option_selected_states(html: &str) -> Vec<bool> {
        let doc = crate::parse(html);
        let mut opts = Vec::new();
        collect_options(&doc, &mut opts);
        opts.iter()
            .map(|o| {
                o.borrow()
                    .kind
                    .as_element()
                    .map(|e| e.selectedness)
                    .unwrap_or(false)
            })
            .collect()
    }

    #[test]
    fn option_first_plain_becomes_selected() {
        // §4.10.10 step 1：无选中且 display size 1 → 首个非 disabled 选中。
        assert_eq!(
            option_selected_states("<select><option>a<option>b</select>"),
            vec![true, false]
        );
    }

    #[test]
    fn option_disabled_first_selects_second() {
        assert_eq!(
            option_selected_states("<select><option disabled>a<option>b</select>"),
            vec![false, true]
        );
    }

    #[test]
    fn option_all_disabled_none_selected() {
        assert_eq!(
            option_selected_states("<select><option disabled>a<option disabled>b</select>"),
            vec![false, false]
        );
    }

    #[test]
    fn option_selected_attr_and_last_selected_kept() {
        // selected 属性 → 初始选中；step 2 保最后一个。
        assert_eq!(
            option_selected_states("<select><option selected>a<option>b</select>"),
            vec![true, false]
        );
        assert_eq!(
            option_selected_states("<select><option selected>a<option selected>b</select>"),
            vec![false, true]
        );
    }

    #[test]
    fn option_selected_after_step1_auto_select_deselects_previous() {
        // P-1 快路径 2：首个 option 无 selected（step 1 自动选中），随后
        // 带 selected 的插入须定点取消 step 1 的选中。修复前带 selected
        // 的插入恒走慢路径；快路径若漏取消会留下两个选中。
        assert_eq!(
            option_selected_states("<select><option>a<option selected>b</select>"),
            vec![false, true]
        );
    }

    #[test]
    fn option_size_not_1_selected_pair_keeps_last() {
        // P-1：size≠1 时 step 1 不跑，step 2 仍保最后一个。同时锁定
        // selectedness_setting_algorithm 的 has_selected 修正（恰 1 个
        // selected 时旧实现错报 false，会让快路径留下两个并存选中）。
        assert_eq!(
            option_selected_states("<select size=2><option selected>a<option selected>b</select>"),
            vec![false, true]
        );
        // 已有选中 + 无 selected 插入：no-op。
        assert_eq!(
            option_selected_states("<select size=2><option selected>a<option>b</select>"),
            vec![true, false]
        );
    }

    #[test]
    fn option_two_selects_independent_memos() {
        // 两个 select：第二个的备忘录失效重建，互不串扰。
        assert_eq!(
            option_selected_states("<select><option selected>a</select><select><option>b</select>"),
            vec![true, true]
        );
    }

    #[test]
    fn option_multiple_select_no_adjustment() {
        assert_eq!(
            option_selected_states("<select multiple><option selected>a<option>b</select>"),
            vec![true, false]
        );
    }

    #[test]
    fn many_plain_options_parse_fast_via_memo() {
        // 65k plain option：修复前每次插入全子树扫描 O(n²)（~2×10^9 次
        // 节点访问，分钟级挂起）；修复后仅首次走慢路径，整体 O(n)。
        // 本测试同时是隐式性能回归测试——O(n²) 回归会令 CI 超时。
        let mut input = String::with_capacity(65_000 * 16);
        input.push_str("<select>");
        for _ in 0..65_000 {
            input.push_str("<option>x</option>");
        }
        input.push_str("</select>");
        let doc = crate::parse(&input);
        let mut opts = Vec::new();
        collect_options(&doc, &mut opts);
        assert_eq!(opts.len(), 65_000, "all options must be in the tree");
        assert!(
            opts[0]
                .borrow()
                .kind
                .as_element()
                .map(|e| e.selectedness)
                .unwrap_or(false),
            "first option must be selected (step 1)"
        );
    }

    #[test]
    fn many_selected_options_parse_fast_via_memo() {
        // P-1（审计 F-8 半修复复核）：`<option selected>` 修复前恒走慢
        // 路径——每次插入全子树 DFS，65k 个 ≈ 8.5×10^9 次节点访问，
        // ~1 MB 输入即可分钟级挂起。修复后带 selected 插入走 O(1) 定点
        // 取消（备忘录 last_selected），整体 O(n)。本测试同时是隐式
        // 性能回归测试——O(n²) 回归会令 CI 超时。
        let mut input = String::with_capacity(65_000 * 26);
        input.push_str("<select>");
        for _ in 0..65_000 {
            input.push_str("<option selected>x</option>");
        }
        input.push_str("</select>");
        let doc = crate::parse(&input);
        let mut opts = Vec::new();
        collect_options(&doc, &mut opts);
        assert_eq!(opts.len(), 65_000, "all options must be in the tree");
        // step 2 保最后一个：前 64999 个被定点取消。
        let selected: Vec<bool> = opts
            .iter()
            .map(|o| {
                o.borrow()
                    .kind
                    .as_element()
                    .map(|e| e.selectedness)
                    .unwrap_or(false)
            })
            .collect();
        assert!(selected[65_000 - 1], "last option must stay selected");
        assert!(
            !selected[..65_000 - 1].iter().any(|&s| s),
            "all earlier options must be deselected"
        );
    }

    #[test]
    fn normal_input_never_triggers_reprocess_limit() {
        let document = Node::new_document();
        let mut constructor = HtmlTreeConstructor::new(document);
        let mut tokenizer = HtmlTokenizer::new("");
        // 正常 token 流（Character + EOF）应直接消费完毕
        constructor.run(&Token::Character('a'), &mut tokenizer);
        constructor.run(&Token::EOF, &mut tokenizer);
        assert!(!constructor
            .errors
            .iter()
            .any(|e| matches!(e, ParseError::ReprocessLimitExceeded { .. })));
    }
}
