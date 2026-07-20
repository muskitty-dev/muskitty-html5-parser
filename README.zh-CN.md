# muskitty-html5-parser

[English](README.md) | [简体中文](README.zh-CN.md)

[![crates.io](https://img.shields.io/crates/v/muskitty-html5-parser.svg)](https://crates.io/crates/muskitty-html5-parser)
[![Documentation](https://docs.rs/muskitty-html5-parser/badge.svg)](https://docs.rs/muskitty-html5-parser)
[![License](https://img.shields.io/crates/l/muskitty-html5-parser.svg)](https://github.com/muskitty-dev/muskitty-html5-parser/blob/main/LICENSE)
[![CI](https://github.com/muskitty-dev/muskitty-html5-parser/actions/workflows/ci.yml/badge.svg)](https://github.com/muskitty-dev/muskitty-html5-parser/actions/workflows/ci.yml)

一个用纯 Rust 从零实现的 HTML5 解析器，严格遵循 [WHATWG HTML Living Standard](https://html.spec.whatwg.org/)，零运行时依赖。

本项目是 [MusKitty](https://github.com/muskitty-dev) 浏览器引擎项目的一部分。

## 状态

| 组件 | 规范覆盖率 | 测试通过率 |
|-----------|---------------|----------------|
| **Tokenizer** (§13.2.5) | 85/85 状态 | [99.8%](https://github.com/html5lib/html5lib-tests) (7022/7036) |
| **Tree Construction** (§13.2.6) | 21/21 插入模式 | [100%](https://github.com/html5lib/html5lib-tests) (1716/1716) |

- 零 `unsafe` 代码
- 零 C/C++ 依赖
- 仅使用 Rust 稳定版工具链
- 以 html5lib-tests 测试套件作为基准

## 安装

在你的 `Cargo.toml` 中添加：

```toml
[dependencies]
muskitty-html5-parser = "0.1.0"
```

或运行：

```bash
cargo add muskitty-html5-parser
```

## 快速开始

```rust
use muskitty_html5_parser::parse;

let document = parse("<!DOCTYPE html><html><head><title>Hello</title></head><body><p>World</p></body></html>");
// 返回 Rc<RefCell<Node>> DOM 树
```

## 架构

```
muskitty-html5-parser/
  src/
    tokenizer/
      types.rs          Token, TagToken, DoctypeToken, State 定义
      trait_def.rs      Tokenizer trait（支持重入）
      impls.rs          HtmlTokenizer — 85 状态机（约 6000 行）
      entities.rs       2231 个 WHATWG 命名字符引用
    parser/
      mod.rs            HtmlTreeConstructor 入口
      dispatch.rs       插入模式调度器（21 种模式）
      helpers.rs        作用域检查、foster parenting、adoption agency
      insertion_mode.rs InsertionMode 枚举
      foreign.rs        SVG/MathML 外部内容处理
    dom/                DOM 节点类型（通过 muskitty-dom）
    lib.rs              公共 API：parse() 入口
```

### 两阶段流水线

```
输入码点 → Tokenizer (§13.2.5) → Token 流 → Tree Construction (§13.2.6) → DOM
```

1. **Tokenizer**：确定性状态机，消费 Unicode 码点并输出 Token（Doctype、Tag、Comment、Character、EOF、ProcessingInstruction）。
2. **Tree Construction**：消费 Token，应用插入模式逻辑，使用打开元素栈、活动格式化元素栈和 foster parenting 机制构建 DOM 树。

## 已实现内容

### Tokenizer (§13.2.5)

- 全部 85 个词法状态
- 内容模型切换（RCDATA、RAWTEXT、ScriptData、PLAINTEXT）
- 字符引用解析（命名引用 + 数值引用，十进制 + 十六进制）
- 2231 个 WHATWG 命名实体，使用二分查找
- Windows-1252 替换表
- 处理指令状态
- CDATA 段状态（外部内容）
- 重入设计：树构建阶段可暂停/恢复 tokenizer

### Tree Construction (§13.2.6)

- 全部 21 种插入模式
- Adoption agency 算法（格式化元素）
- Foster parenting（表格上下文）
- 外部内容（SVG/MathML）及命名空间处理
- Template 插入模式
- Reset insertion mode
- 作用域检查（button、list、table、default）

## 构建

```bash
cargo check                          # 整个 workspace 检查（必须零警告）
cargo check -p muskitty-html5-parser # 仅检查解析器 crate
```

## 测试

```bash
# 单元测试（145 个测试）
cargo test -p muskitty-html5-parser --lib

# html5lib tokenizer 测试套件（7036 个测试）
cargo test --test html5lib_tokenizer -- --nocapture

# html5lib tree construction 测试套件（1920 个测试，1716 个非跳过）
cargo test --test html5lib_tree_construction -- --nocapture

# 全部测试
cargo test
```

### 测试夹具

测试使用 [html5lib-tests](https://github.com/html5lib/html5lib-tests) 套件：

- `tests/data/tokenizer/*.test` — 14 个 tokenizer 夹具文件
- `tests/data/tree_construction/*.test` — 68 个 tree construction 夹具文件

## 设计原则

1. **以 WHATWG 为准** — 实现严格遵循规范。WPT 和 Chromium 仅作为次要参考。
2. **符合规范，而非符合测试** — 测试用于验证代码；除非规范证明测试有误，否则绝不为了通过测试而修改代码。
3. **最小依赖** — 仅依赖 `muskitty-dom`（兄弟 crate）和 `serde_json`（dev-dependency，用于测试夹具）。
4. **零 unsafe** — 纯 safe Rust。
5. **外科手术式修改** — 每次改动都尽可能小，仅满足任务所需。

## 规范参考

本实现参考：

- [WHATWG HTML Living Standard](https://html.spec.whatwg.org/) — 主要权威
  - §13.2.5: Tokenization
  - §13.2.6: Tree Construction
- [html5lib-tests](https://github.com/html5lib/html5lib-tests) — 测试基准

## License

基于 Apache License, Version 2.0 授权，详见 [LICENSE](LICENSE)。

Copyright 2026 MusCat / MusKitty Bit-Torch Community
