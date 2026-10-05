//! 符号抽取：从真实源码文件中提取函数 / 类 / 接口等符号。
//!
//! # 与《技术设计书》§13 的关系（一处刻意的阶段性取舍）
//! 设计书指定 Level 1 用 Tree-sitter 做 AST 解析。本模块**先落地启发式抽取器**，
//! 理由与边界：
//!
//! - 各语言 grammar（tree-sitter-rust / -python / -typescript …）都是 C 代码，
//!   会显著拉长构建时间，并给开源贡献者增加 C 工具链门槛
//!   （本项目已实测：Windows 上缺 MSVC 时连 rusqlite 都编译不过）
//! - 启发式抽取是**真实解析**：从文件内容按语言语法提取符号名、行号、签名、文档注释，
//!   不是 mock，足以支撑 reuse_score 评分与 Evidence 生成
//!
//! 🔑 关键设计：符号来源被抽象为 [`SymbolExtractor`] trait。
//! 将来接 Tree-sitter 只需新增一个实现，`projectassests-asset` 的评分/关系逻辑与
//! 上层 API **完全不用改**。这是"可替换"而非"永久将就"。
//!
//! # 诚实性纪律
//! 抽取不到就是空结果，**绝不编造符号**。
//! 无法识别的语言返回 `Vec::new()`，上层据此显示"未解析"而非假列表。

use serde::{Deserialize, Serialize};

/// 符号种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    /// 模块级常量 / 顶层导出值
    Constant,
    /// React/Vue 组件（前端资产的主体）
    Component,
    /// API 路由处理器（FastAPI/Express/Axum 等）
    ApiEndpoint,
    Type,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Constant => "constant",
            Self::Component => "component",
            Self::ApiEndpoint => "api_endpoint",
            Self::Type => "type",
        }
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Function => "函数",
            Self::Method => "方法",
            Self::Class => "类",
            Self::Struct => "结构体",
            Self::Enum => "枚举",
            Self::Trait => "Trait",
            Self::Interface => "接口",
            Self::Constant => "常量",
            Self::Component => "组件",
            Self::ApiEndpoint => "API 端点",
            Self::Type => "类型",
        }
    }

    /// 是否属于"可独立复用"的粒度。
    ///
    /// 常量与类型别名通常太小、依附于上下文，不应作为可复用资产推荐，
    /// 否则资产库会被 `MAX_RETRY` 这类噪音淹没。
    pub fn is_reusable_unit(&self) -> bool {
        !matches!(self, Self::Constant | Self::Type)
    }
}

/// 一个被抽取出的符号。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub language: String,
    /// 相对项目根的路径（存相对路径：库可跨机器复制，且不泄漏用户目录结构）
    pub file_path: String,
    /// 1-based 行号
    pub line: usize,
    /// 签名行（截断到合理长度，供 Evidence 展示）
    pub signature: String,
    /// 紧邻上方的文档注释（`///`、`/** */`、`"""`）
    pub doc_comment: Option<String>,
    /// 符号体行数（近似：到下一个符号或文件末尾）。用于评分与"太小不值得复用"判定。
    pub body_lines: usize,
    /// 所属的父符号（方法属于类）。用于生成 `Class.method` 形式的全名。
    pub parent: Option<String>,
    /// 该符号是否被同一文件内其他符号调用（被引用 = 有复用价值的信号）
    pub referenced_locally: bool,
}

impl Symbol {
    /// 全限定名：`Class.method` 或 `function`。
    pub fn qualified_name(&self) -> String {
        match &self.parent {
            Some(p) => format!("{p}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// 是否带文档注释（可维护性信号，参与评分）。
    pub fn is_documented(&self) -> bool {
        self.doc_comment
            .as_ref()
            .is_some_and(|d| !d.trim().is_empty())
    }

    /// 符号名是否为"一次性/调试"性质（这类不该进资产库）。
    ///
    /// 判据来自《docs/参考/计划.md》§2：`debug_test_2025()` 这类是临时代码，
    /// 而 `batch_process()` 才有通用价值。
    pub fn looks_disposable(&self) -> bool {
        let n = self.name.to_ascii_lowercase();
        // 含年份/日期的名字几乎都是临时脚本
        if (2000..=2099).any(|y| n.contains(&y.to_string())) {
            return true;
        }
        DISPOSABLE_PREFIXES.iter().any(|p| n.starts_with(p))
            || DISPOSABLE_SUBSTR.iter().any(|s| n.contains(s))
    }
}

/// 一次性代码的名字前缀。
const DISPOSABLE_PREFIXES: &[&str] = &[
    "tmp", "temp", "debug_", "dbg_", "scratch", "wip_", "hack_", "xxx",
    "test_tmp", "foo", "bar", "baz", "dummy", "todo_", "fixme_",
    "old_", "backup_", "copy_of_", "untitled",
];

/// 一次性代码的名字片段。
const DISPOSABLE_SUBSTR: &[&str] = &["_bak", "_old", "_copy", "_v1_final", "_deprecated", "_unused"];

/// 符号抽取器抽象。
///
/// 存在的意义：将来接 Tree-sitter 时新增实现即可，上层无需改动。
pub trait SymbolExtractor: Send + Sync {
    /// 抽取器名称（用于审计与"generated_by"标记）。
    fn name(&self) -> &'static str;

    /// 是否支持该语言。
    fn supports(&self, language: &str) -> bool;

    /// 从文件内容抽取符号。
    ///
    /// `file_path` 是相对项目根的路径，直接写入 `Symbol.file_path`。
    /// 返回空 Vec 表示"该语言不支持"或"文件里没有符号"，两者都**不是错误**。
    fn extract(&self, language: &str, file_path: &str, content: &str) -> Vec<Symbol>;
}

/// 启发式抽取器：按语言的声明语法逐行识别。
///
/// 局限（诚实记录）：不做完整词法分析，因此
/// - 字符串字面量里出现的 `def foo(` 会被误判为符号（概率低，且不影响评分方向）
/// - 宏生成的符号（Rust `macro_rules!` 展开）无法识别
///
/// 这些局限只影响**召回率**，不会产生假的"高价值资产"，方向是安全的。
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicExtractor;

impl HeuristicExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl SymbolExtractor for HeuristicExtractor {
    fn name(&self) -> &'static str {
        "heuristic-v1"
    }

    fn supports(&self, language: &str) -> bool {
        matches!(
            language,
            "Python"
                | "Rust"
                | "JavaScript"
                | "TypeScript"
                | "Go"
                | "Java"
                | "Kotlin"
                | "C"
                | "C++"
                | "C#"
                | "Ruby"
                | "PHP"
                | "Swift"
                | "Vue"
        )
    }

    fn extract(&self, language: &str, file_path: &str, content: &str) -> Vec<Symbol> {
        if !self.supports(language) {
            return Vec::new();
        }
        let lines: Vec<&str> = content.lines().collect();
        let mut symbols: Vec<Symbol> = Vec::new();
        let mut pending_doc: Option<String> = None;
        let mut doc_buf: Vec<String> = Vec::new();
        // 记录最近的类/结构体名，作为后续方法的 parent
        let mut current_parent: Option<(String, usize)> = None;

        for (idx, raw) in lines.iter().enumerate() {
            let line = raw.trim();
            let lineno = idx + 1;

            // ── 收集文档注释 ─────────────────────────────────────
            if let Some(doc) = doc_line(language, line) {
                doc_buf.push(doc);
                continue;
            }
            if !doc_buf.is_empty() && !line.is_empty() && !is_declaration_start(language, line) {
                // 注释与声明之间隔了空行/其他内容 → 该注释不属于任何符号
                doc_buf.clear();
            }
            if !doc_buf.is_empty() {
                pending_doc = Some(doc_buf.join(" "));
                doc_buf.clear();
            }

            if line.is_empty() {
                continue;
            }

            let Some(parsed) = parse_declaration(language, line) else {
                continue;
            };

            // 缩进 > 0 且当前有父类 → 判定为方法
            let indent = raw.len() - raw.trim_start().len();
            let parent = if matches!(parsed.kind, SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait) {
                None // 类本身没有父
            } else if indent > 0 || is_method_kind(language, &parsed.kind) {
                current_parent.as_ref().map(|(name, _)| name.clone())
            } else {
                None
            };

            let kind = if parent.is_some() && parsed.kind == SymbolKind::Function {
                SymbolKind::Method
            } else {
                parsed.kind
            };

            // 计算 body_lines：到下一个同级或更高级声明之前
            let body_lines = estimate_body_lines(&lines, idx, indent);

            symbols.push(Symbol {
                name: parsed.name,
                kind,
                language: language.to_string(),
                file_path: file_path.to_string(),
                line: lineno,
                signature: truncate_sig(line),
                doc_comment: pending_doc.take(),
                body_lines,
                parent,
                referenced_locally: false, // 第二遍填充
            });

            // 更新当前父级（类/结构体）
            if let Some(last) = symbols.last()
                && matches!(
                    last.kind,
                    SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait
                )
            {
                current_parent = Some((last.name.clone(), lineno));
            }
            // 顶层声明（indent == 0）且不是类 → 退出父级作用域
            if indent == 0
                && !matches!(
                    symbols.last().map(|s| s.kind),
                    Some(SymbolKind::Class | SymbolKind::Struct)
                )
            {
                current_parent = None;
            }
        }

        // Python 的文档串在声明**之后**（PEP 257：docstring 必须是 body 首条语句），
        // 与 Rust `///`、JSDoc 的前置注释相反，因此需要单独的后置回填 pass。
        if language == "Python" {
            backfill_python_docstrings(&lines, &mut symbols);
        }
        mark_local_references(content, &mut symbols);
        symbols
    }
}

/// 回填 Python 文档串（声明之后的首条语句）。
///
/// 为什么要独立成 pass 而不是在主循环里顺手做：
/// 主循环是"注释在前、声明在后"的单向扫描，而 docstring 在声明之后。
/// 若在主循环里把 `"""..."""` 也收进 `doc_buf`，它会被**下一个**符号抢走——
/// 实测 `class A:` 的文档串会错误地挂到其后的 `def f()` 上。
fn backfill_python_docstrings(lines: &[&str], symbols: &mut [Symbol]) {
    for sym in symbols.iter_mut() {
        // 已有前置注释（`#`）的就不覆盖
        if sym.doc_comment.is_some() {
            continue;
        }
        let start = sym.line.saturating_sub(1); // Symbol.line 是 1-based
        if start >= lines.len() {
            continue;
        }
        // 先找声明结束行：def/class 声明以 ':' 收尾，但可能跨多行
        // （如 `async def f(\n    self,\n    x: int,\n) -> Y:`）。限制扫描范围防止异常文件卡住。
        let scan_to = lines.len().min(start + 30);
        let decl_end = lines[start..scan_to]
            .iter()
            .position(|l| l.trim_end().ends_with(':'))
            .map_or(scan_to.saturating_sub(1), |offset| start + offset);

        // 声明之后的第一个非空行：按 PEP 257 它必须是 docstring 才算。
        // 只看这一行——非 docstring 说明该符号没有文档串，继续往下找会误抓
        // 函数体里的其它字符串字面量。
        let limit = lines.len().min(decl_end + 12);
        let window = &lines[(decl_end + 1).min(lines.len())..limit];
        if let Some(offset) = window.iter().position(|l| !l.trim().is_empty()) {
            let doc_idx = decl_end + 1 + offset;
            if let Some(doc) = python_docstring_at(lines, doc_idx) {
                sym.doc_comment = Some(doc);
            }
        }
    }
}

/// 解析位于 `idx` 行的 Python 文档串（支持单行与多行形态）。
fn python_docstring_at(lines: &[&str], idx: usize) -> Option<String> {
    let t = lines[idx].trim();
    let quote = if t.starts_with("\"\"\"") {
        "\"\"\""
    } else if t.starts_with("'''") {
        "'''"
    } else {
        return None;
    };
    // 引号是 ASCII，按字节切片安全
    let after_open = &t[quote.len()..];

    // 单行形态："""文本"""
    if let Some(close) = after_open.find(quote) {
        let text = after_open[..close].trim();
        return (!text.is_empty()).then(|| text.to_string());
    }

    // 多行形态：收集到闭合引号为止（限制行数，防止未闭合的异常文件读到底）
    let mut parts: Vec<String> = Vec::new();
    if !after_open.trim().is_empty() {
        parts.push(after_open.trim().to_string());
    }
    let limit = lines.len().min(idx + 60);
    let from = (idx + 1).min(lines.len());
    for l in lines[from..limit].iter().map(|l| l.trim()) {
        if let Some(close) = l.find(quote) {
            let text = l[..close].trim();
            if !text.is_empty() {
                parts.push(text.to_string());
            }
            break;
        }
        if !l.is_empty() {
            parts.push(l.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// 声明解析结果。
#[derive(Debug, Clone, PartialEq)]
struct Declaration {
    name: String,
    kind: SymbolKind,
}

/// 按语言解析一行声明，返回符号名与种类。
///
/// 只做**前缀匹配 + 名字提取**，不建完整语法树：
/// 对"找出有哪些符号"这个目标足够，且每种语言的规则都能被单测覆盖。
fn parse_declaration(language: &str, line: &str) -> Option<Declaration> {
    match language {
        "Python" => parse_python(line),
        "Rust" => parse_rust(line),
        "JavaScript" | "TypeScript" => parse_js_ts(line),
        "Go" => parse_go(line),
        "Java" | "Kotlin" | "C#" => parse_java_like(line),
        "C" | "C++" => parse_c_like(line),
        "Ruby" => parse_ruby(line),
        "PHP" => parse_php(line),
        "Swift" => parse_swift(line),
        "Vue" => parse_js_ts(line), // SFC 的 <script> 块按 JS/TS 处理
        _ => None,
    }
}

/// Python：`def foo(`、`async def foo(`、`class Foo:`
fn parse_python(line: &str) -> Option<Declaration> {
    // 跳过装饰器（@app.get 会在下面按 API 端点处理）
    if let Some(rest) = line.strip_prefix("def ").or_else(|| {
        line.strip_prefix("async def ")
    }) {
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Function });
        }
    }
    if let Some(rest) = line.strip_prefix("class ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Class });
        }
    }
    // 模块级常量：全大写名（PEP 8）
    if let Some((name, _)) = line.split_once('=') {
        let name = name.trim();
        if name.len() > 2
            && name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
            && !name.starts_with('_')
        {
            return Some(Declaration {
                name: name.to_string(),
                kind: SymbolKind::Constant,
            });
        }
    }
    None
}

/// Rust：`fn`、`pub fn`、`async fn`、`struct`、`enum`、`trait`、`impl`、`type`
fn parse_rust(line: &str) -> Option<Declaration> {
    let l = strip_rust_qualifiers(line);

    for (kw, kind) in [
        ("struct ", SymbolKind::Struct),
        ("enum ", SymbolKind::Enum),
        ("trait ", SymbolKind::Trait),
        ("union ", SymbolKind::Struct),
    ] {
        if let Some(rest) = l.strip_prefix(kw) {
            let name = take_ident(rest);
            if !name.is_empty() {
                return Some(Declaration { name, kind });
            }
        }
    }
    if let Some(rest) = l.strip_prefix("fn ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Function });
        }
    }
    if let Some(rest) = l.strip_prefix("type ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Type });
        }
    }
    // `impl Trait for Type` / `impl Type` → 记为 Type（trait 实现的宿主类型）
    if let Some(rest) = l.strip_prefix("impl") {
        let rest = rest.trim();
        if !rest.is_empty() && !rest.starts_with('<') {
            // 取最后一个标识符（`impl Foo for Bar` → Bar；`impl Bar` → Bar）
            let target = rest
                .split(" for ")
                .last()
                .unwrap_or(rest)
                .split_whitespace()
                .next()
                .unwrap_or("");
            let name = take_ident(target);
            if !name.is_empty() {
                return Some(Declaration { name, kind: SymbolKind::Type });
            }
        }
    }
    // const 常量
    if let Some(rest) = l.strip_prefix("const ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Constant });
        }
    }
    None
}

/// 去掉 Rust 的可见性/属性修饰，便于统一匹配关键字。
fn strip_rust_qualifiers(line: &str) -> &str {
    let mut l = line.trim();
    loop {
        let before = l;
        for q in ["pub(crate) ", "pub(super) ", "pub ", "unsafe ", "async ", "default ", "extern "] {
            if let Some(rest) = l.strip_prefix(q) {
                l = rest;
            }
        }
        if l == before {
            break;
        }
    }
    l
}

/// JS/TS：`function foo`、`class Foo`、`const foo = (`、`export ...`、箭头函数、React 组件
fn parse_js_ts(line: &str) -> Option<Declaration> {
    let l = strip_js_qualifiers(line);

    if let Some(rest) = l.strip_prefix("class ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Class });
        }
    }
    if let Some(rest) = l.strip_prefix("function ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            let kind = if is_react_component(&name) {
                SymbolKind::Component
            } else {
                SymbolKind::Function
            };
            return Some(Declaration { name, kind });
        }
    }
    // `const Foo = (...) => ...` / `const foo = function` / `export const foo = async () =>`
    for kw in ["const ", "let ", "var "] {
        if let Some(rest) = l.strip_prefix(kw) {
            let name = take_ident(rest);
            if name.is_empty() {
                continue;
            }
            let after_name = rest[name.len()..].trim_start();
            // 必须是赋值且右侧像函数：`= (` / `= async (` / `= function` / `= () =>`
            let is_fn = after_name.starts_with("= (")
                || after_name.starts_with("= async")
                || after_name.starts_with("= function")
                || after_name.starts_with("= ()")
                || after_name.contains("=> ")
                || after_name.starts_with(": (")
                || after_name.starts_with("= use") // hooks 封装
                || after_name.contains("= (");
            if is_fn {
                let kind = if is_react_component(&name) {
                    SymbolKind::Component
                } else if after_name.starts_with('=') && !after_name.contains("=>") && !after_name.contains("function") {
                    SymbolKind::Constant
                } else {
                    SymbolKind::Function
                };
                // 纯常量（= 数字/字符串）不作为函数符号
                if kind == SymbolKind::Constant && !is_constant_assignment(after_name) {
                    continue;
                }
                return Some(Declaration { name, kind });
            }
            // 普通常量声明
            if after_name.starts_with('=') && is_constant_assignment(after_name) {
                return Some(Declaration { name, kind: SymbolKind::Constant });
            }
        }
    }
    // TS 接口与类型别名
    if let Some(rest) = l.strip_prefix("interface ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Interface });
        }
    }
    if let Some(rest) = l.strip_prefix("type ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Type });
        }
    }
    // Express 风格路由：app.get('/path', handler) / router.post(...)
    if let Some(ep) = parse_http_route(line) {
        return Some(ep);
    }
    None
}

/// 去掉 JS/TS 的 export/default/async 修饰。
fn strip_js_qualifiers(line: &str) -> &str {
    let mut l = line.trim();
    loop {
        let before = l;
        for q in [
            "export default ",
            "export async ",
            "export ",
            "declare ",
            "async ",
            "default ",
        ] {
            if let Some(rest) = l.strip_prefix(q) {
                l = rest;
            }
        }
        if l == before {
            break;
        }
    }
    l
}

/// 是否为 React/Vue 组件名（大写开头的函数）。
fn is_react_component(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// 右侧是否为纯常量赋值（数字/字符串/布尔/数组/对象字面量）。
fn is_constant_assignment(after_name: &str) -> bool {
    let v = after_name.trim_start_matches('=').trim();
    v.starts_with('"')
        || v.starts_with('\'')
        || v.starts_with('`')
        || v.starts_with('[')
        || v.starts_with('{')
        || v.starts_with("true")
        || v.starts_with("false")
        || v.starts_with("null")
        || v.starts_with("undefined")
        || v.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-')
}

/// Go：`func foo(`、`func (r *T) foo(`、`type Foo struct`
fn parse_go(line: &str) -> Option<Declaration> {
    let l = line.trim();
    if let Some(rest) = l.strip_prefix("func ") {
        // 方法：`func (r *Router) Serve(` → 名字是 Serve，parent 是 Router
        if rest.starts_with('(') {
            let after_recv = rest.split_once(')')?.1.trim();
            let name = take_ident(after_recv);
            if !name.is_empty() {
                return Some(Declaration { name, kind: SymbolKind::Method });
            }
            return None;
        }
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Function });
        }
    }
    if let Some(rest) = l.strip_prefix("type ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            let kind = if rest.contains("interface") {
                SymbolKind::Interface
            } else if rest.contains("struct") {
                SymbolKind::Struct
            } else {
                SymbolKind::Type
            };
            return Some(Declaration { name, kind });
        }
    }
    None
}

/// Java / Kotlin / C#：类、接口、枚举、方法
fn parse_java_like(line: &str) -> Option<Declaration> {
    let l = strip_java_qualifiers(line);

    for (kw, kind) in [
        ("class ", SymbolKind::Class),
        ("interface ", SymbolKind::Interface),
        ("enum ", SymbolKind::Enum),
        ("record ", SymbolKind::Class),
        ("struct ", SymbolKind::Struct), // C#
    ] {
        if let Some(rest) = l.strip_prefix(kw) {
            let name = take_ident(rest);
            if !name.is_empty() {
                return Some(Declaration { name, kind });
            }
        }
    }
    // 注解行（@GetMapping 等）→ API 端点
    if l.starts_with('@') {
        if let Some(ep) = parse_java_annotation_route(l) {
            return Some(ep);
        }
        return None;
    }
    // 方法：`修饰符 返回类型 名字(` —— 要求以 '(' 结尾的参数列表且以 '{' 或 ';' 收尾
    if l.contains('(') && (l.ends_with('{') || l.ends_with(';') || l.ends_with(')')) {
        // 排除控制流关键字
        let first = l.split_whitespace().next().unwrap_or("");
        if matches!(first, "if" | "for" | "while" | "switch" | "catch" | "return" | "new") {
            return None;
        }
        // 名字 = '(' 之前的最后一个标识符
        let before_paren = l.split('(').next().unwrap_or("");
        let name = before_paren
            .split_whitespace()
            .last()
            .map(take_ident)
            .filter(|n| !n.is_empty())?;
        // 必须至少有两个 token（返回类型 + 名字），否则是构造调用之类
        if before_paren.split_whitespace().count() >= 2 && is_valid_symbol_name(&name) {
            return Some(Declaration { name, kind: SymbolKind::Method });
        }
    }
    None
}

fn strip_java_qualifiers(line: &str) -> &str {
    let mut l = line.trim();
    loop {
        let before = l;
        for q in [
            "public ", "private ", "protected ", "static ", "final ", "abstract ",
            "synchronized ", "native ", "sealed ", "open ", "internal ", "override ",
            "virtual ", "async ", "data ", "value ",
        ] {
            if let Some(rest) = l.strip_prefix(q) {
                l = rest;
            }
        }
        if l == before {
            break;
        }
    }
    l
}

/// C / C++：class、struct、函数定义
fn parse_c_like(line: &str) -> Option<Declaration> {
    let l = line.trim();
    for (kw, kind) in [
        ("class ", SymbolKind::Class),
        ("struct ", SymbolKind::Struct),
        ("enum ", SymbolKind::Enum),
        ("namespace ", SymbolKind::Type),
    ] {
        if let Some(rest) = l.strip_prefix(kw) {
            let name = take_ident(rest);
            // 排除 `struct foo *bar` 这种变量声明
            if !name.is_empty() && !rest[name.len()..].trim_start().starts_with('*') {
                return Some(Declaration { name, kind });
            }
        }
    }
    // 函数定义：以 `{` 或 `)` 结尾，含 `(`，且首 token 不是控制流
    if l.contains('(') && (l.ends_with('{') || l.ends_with(')') || l.ends_with(';')) {
        let first = l.split_whitespace().next().unwrap_or("");
        if matches!(first, "if" | "for" | "while" | "switch" | "return" | "#include" | "#define") {
            return None;
        }
        if first.starts_with('#') {
            return None;
        }
        let before_paren = l.split('(').next().unwrap_or("");
        let name = before_paren
            .split_whitespace()
            .last()
            .map(|t| t.trim_end_matches(['*', '&']))
            .map(take_ident)
            .filter(|n| !n.is_empty())?;
        if before_paren.split_whitespace().count() >= 2 && is_valid_symbol_name(&name) {
            return Some(Declaration { name, kind: SymbolKind::Function });
        }
    }
    None
}

/// Ruby：`def foo`、`class Foo`、`module Foo`
fn parse_ruby(line: &str) -> Option<Declaration> {
    let l = line.trim();
    if let Some(rest) = l.strip_prefix("def ") {
        let name = take_ident(rest.trim_start_matches("self."));
        if !name.is_empty() {
            let kind = if l.contains("def self.") {
                SymbolKind::Function
            } else {
                SymbolKind::Method
            };
            return Some(Declaration { name, kind });
        }
    }
    for (kw, kind) in [("class ", SymbolKind::Class), ("module ", SymbolKind::Type)] {
        if let Some(rest) = l.strip_prefix(kw) {
            let name = take_ident(rest);
            if !name.is_empty() {
                return Some(Declaration { name, kind });
            }
        }
    }
    None
}

/// PHP：`function foo`、`class Foo`、`public function bar`
fn parse_php(line: &str) -> Option<Declaration> {
    let l = line.trim().trim_start_matches("<?php").trim();
    let mut l = l;
    loop {
        let before = l;
        for q in ["public ", "private ", "protected ", "static ", "abstract ", "final "] {
            if let Some(rest) = l.strip_prefix(q) {
                l = rest;
            }
        }
        if l == before {
            break;
        }
    }
    if let Some(rest) = l.strip_prefix("function ") {
        let name = take_ident(rest.trim_start_matches('&'));
        if !name.is_empty() {
            return Some(Declaration { name, kind: SymbolKind::Function });
        }
    }
    for (kw, kind) in [
        ("class ", SymbolKind::Class),
        ("interface ", SymbolKind::Interface),
        ("trait ", SymbolKind::Trait),
        ("enum ", SymbolKind::Enum),
    ] {
        if let Some(rest) = l.strip_prefix(kw) {
            let name = take_ident(rest);
            if !name.is_empty() {
                return Some(Declaration { name, kind });
            }
        }
    }
    None
}

/// Swift：`func foo`、`struct Foo`、`class Foo`、`protocol Foo`、`enum Foo`
fn parse_swift(line: &str) -> Option<Declaration> {
    let mut l = line.trim();
    loop {
        let before = l;
        for q in ["public ", "private ", "internal ", "fileprivate ", "open ", "static ", "final ", "override "] {
            if let Some(rest) = l.strip_prefix(q) {
                l = rest;
            }
        }
        if l == before {
            break;
        }
    }
    for (kw, kind) in [
        ("func ", SymbolKind::Function),
        ("struct ", SymbolKind::Struct),
        ("class ", SymbolKind::Class),
        ("protocol ", SymbolKind::Interface),
        ("enum ", SymbolKind::Enum),
    ] {
        if let Some(rest) = l.strip_prefix(kw) {
            let name = take_ident(rest);
            if !name.is_empty() {
                return Some(Declaration { name, kind });
            }
        }
    }
    None
}

/// 该语言的这种符号是否天然是"方法"（有接收者）。
fn is_method_kind(language: &str, kind: &SymbolKind) -> bool {
    *kind == SymbolKind::Method
        || (matches!(language, "Go" | "Ruby" | "Java" | "Kotlin" | "C#" | "PHP")
            && *kind == SymbolKind::Function)
}

/// Express 风格路由：`app.get('/users', ...)` / `router.post("/x", ...)`
fn parse_http_route(line: &str) -> Option<Declaration> {
    let methods = ["get", "post", "put", "patch", "delete", "options", "head"];
    for prefix in ["app.", "router.", "server.", "this."] {
        let Some(rest) = line.trim().strip_prefix(prefix) else {
            continue;
        };
        for m in methods {
            let Some(after) = rest.strip_prefix(&format!("{m}(")) else {
                continue;
            };
            // 提取第一个字符串参数作为路径
            let path = extract_first_string_lit(after)?;
            return Some(Declaration {
                name: format!("{} {}", m.to_ascii_uppercase(), path),
                kind: SymbolKind::ApiEndpoint,
            });
        }
    }
    None
}

/// Java/Spring 注解路由：`@GetMapping("/users")`
fn parse_java_annotation_route(line: &str) -> Option<Declaration> {
    let mapping = [
        ("@GetMapping", "GET"),
        ("@PostMapping", "POST"),
        ("@PutMapping", "PUT"),
        ("@PatchMapping", "PATCH"),
        ("@DeleteMapping", "DELETE"),
    ];
    for (anno, verb) in mapping {
        if line.starts_with(anno) {
            let inner = line.trim_start_matches(anno).trim_start_matches('(');
            let path = extract_first_string_lit(inner).unwrap_or_else(|| "/".to_string());
            return Some(Declaration {
                name: format!("{verb} {path}"),
                kind: SymbolKind::ApiEndpoint,
            });
        }
    }
    None
}

/// 提取第一个字符串字面量（支持单双引号）。
fn extract_first_string_lit(s: &str) -> Option<String> {
    let bytes: Vec<char> = s.chars().collect();
    let start = bytes.iter().position(|c| *c == '"' || *c == '\'')?;
    let quote = bytes[start];
    let rest = &bytes[start + 1..];
    let end = rest.iter().position(|c| *c == quote)?;
    Some(rest[..end].iter().collect())
}

/// 提取一行中的文档注释内容；非注释行返回 None。
///
/// 🔴 Python 特例：这里**只**认 `#` 注释，不认 `"""` 文档串。
/// 文档串在声明之后（body 首行），若在此处收集，会被**下一个**符号抢走——
/// 实测 `class A:` 的文档串会错误地挂到其后的 `def f()` 上。
/// docstring 由 `backfill_python_docstrings` 在声明之后回填。
fn doc_line(language: &str, line: &str) -> Option<String> {
    let t = line.trim();
    match language {
        "Python" => t
            .strip_prefix('#')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        "Rust" | "Swift" => {
            if let Some(rest) = t.strip_prefix("///") {
                return Some(rest.trim().to_string());
            }
            if let Some(rest) = t.strip_prefix("//!") {
                return Some(rest.trim().to_string());
            }
            None
        }
        "Ruby" | "Shell" | "YAML" => t
            .strip_prefix('#')
            .map(|s| s.trim().to_string()),
        _ => {
            // C 风格：/** */、/* */、//
            if t.starts_with("/**") || t.starts_with("/*!") {
                let inner = t.trim_start_matches('/').trim_start_matches('*').trim_start_matches('!');
                let inner = inner.trim_end_matches('/').trim_end_matches('*');
                return Some(inner.trim().to_string());
            }
            if t.starts_with('*') && !t.starts_with("*/") {
                let inner = t.trim_start_matches('*').trim_end_matches('/').trim_end_matches('*');
                return Some(inner.trim().to_string());
            }
            if t.starts_with("//") {
                return Some(t.trim_start_matches('/').trim().to_string());
            }
            None
        }
    }
}

/// 该行是否像声明开头（用于判断注释是否紧邻声明）。
fn is_declaration_start(language: &str, line: &str) -> bool {
    parse_declaration(language, line).is_some()
}

/// 从一行开头取标识符。
fn take_ident(s: &str) -> String {
    s.trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
        .collect()
}

/// 是否为合法符号名（排除关键字、纯数字、过长串）。
fn is_valid_symbol_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 {
        return false;
    }
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
    {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '-')
        && !RESERVED_WORDS.contains(&name)
}

/// 常见保留字/关键字，避免把它们当符号名。
const RESERVED_WORDS: &[&str] = &[
    "if", "else", "for", "while", "return", "switch", "case", "break", "continue",
    "function", "class", "struct", "enum", "interface", "trait", "impl", "type",
    "var", "let", "const", "new", "delete", "this", "self", "super", "import",
    "from", "export", "default", "public", "private", "protected", "static",
    "void", "int", "str", "string", "bool", "true", "false", "null", "None",
    "def", "async", "await", "try", "catch", "finally", "throw", "throws",
    "package", "func", "go", "chan", "map", "range", "select", "defer",
];

/// 估算符号体行数：从声明行开始，到下一个缩进 ≤ 当前缩进的声明行为止。
///
/// 这是**近似值**（不做括号配平），但对评分足够：
/// 我们只需要区分"3 行的琐碎函数"和"80 行的实质实现"。
fn estimate_body_lines(lines: &[&str], start_idx: usize, start_indent: usize) -> usize {
    let mut count = 1;
    for raw in lines.iter().skip(start_idx + 1) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            count += 1;
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        // 遇到同级或更外层的非注释行 → 当前符号结束
        if indent <= start_indent && !trimmed.starts_with("//") && !trimmed.starts_with('#') {
            break;
        }
        count += 1;
    }
    count
}

/// 标记"被同文件内其他符号引用"的符号。
///
/// 这是 reuse_score 的重要信号：被本文件多处调用的函数，
/// 说明它承担了实际职责，而非一次性胶水代码。
fn mark_local_references(content: &str, symbols: &mut [Symbol]) {
    for sym in symbols.iter_mut() {
        // 统计名字出现次数：>1 表示除声明外还有引用
        // 用词边界匹配，避免 `foo` 命中 `foobar`
        let pattern = format!(r"\b{}\b", regex_escape(&sym.name));
        let count = count_matches(content, &pattern);
        sym.referenced_locally = count > 1;
    }
}

/// 转义正则元字符（符号名可能含 `$`，如 JS 的 `$ref`）。
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 统计模式出现次数（手写词边界匹配，避免为这一个用途引入 regex crate）。
fn count_matches(content: &str, pattern: &str) -> usize {
    // pattern 形如 `\bname\b`，取出裸名字
    let name = pattern.trim_start_matches("\\b").trim_end_matches("\\b");
    if name.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut search_from = 0;
    let bytes = content.as_bytes();
    while let Some(rel) = content[search_from..].find(name) {
        let start = search_from + rel;
        let end = start + name.len();
        // 词边界检查：前后都不能是标识符字符
        let left_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let right_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        if left_ok && right_ok {
            count += 1;
        }
        search_from = end;
        if search_from >= content.len() {
            break;
        }
    }
    count
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// 截断签名行（避免把整段代码塞进 Evidence）。
fn truncate_sig(line: &str) -> String {
    let t = line.trim();
    if t.chars().count() <= 160 {
        t.to_string()
    } else {
        let cut: String = t.chars().take(157).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(syms: &[Symbol]) -> Vec<String> {
        syms.iter().map(|s| s.name.clone()).collect()
    }

    fn find<'a>(syms: &'a [Symbol], name: &str) -> &'a Symbol {
        syms.iter().find(|s| s.name == name).unwrap_or_else(|| {
            panic!("未找到符号 {name}，实际有: {:?}", names(syms))
        })
    }

    // ── Python ──────────────────────────────────────────────────────

    #[test]
    fn extracts_python_functions_and_classes() {
        let src = r#"
class VideoService:
    """视频生成服务"""

    def __init__(self):
        self.queue = TaskQueue()

    async def generate_video(self, prompt: str) -> VideoResult:
        """根据提示词生成视频"""
        return await self.queue.submit(prompt)

def standalone_helper(x):
    return x * 2
"#;
        let syms = HeuristicExtractor.extract("Python", "svc.py", src);
        assert!(names(&syms).contains(&"VideoService".to_string()));
        assert!(names(&syms).contains(&"generate_video".to_string()));
        assert!(names(&syms).contains(&"standalone_helper".to_string()));

        let cls = find(&syms, "VideoService");
        assert_eq!(cls.kind, SymbolKind::Class);
        assert_eq!(cls.line, 2);

        // 类内的函数应识别为方法并带 parent
        let m = find(&syms, "generate_video");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent.as_deref(), Some("VideoService"));
        assert_eq!(m.qualified_name(), "VideoService.generate_video");

        // 顶层函数无 parent
        let h = find(&syms, "standalone_helper");
        assert_eq!(h.kind, SymbolKind::Function);
        assert!(h.parent.is_none());
    }

    #[test]
    fn extracts_python_docstrings() {
        let src = "class A:\n    \"\"\"类文档\"\"\"\n\n    def f(self):\n        \"\"\"方法文档\"\"\"\n        pass\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert_eq!(find(&syms, "A").doc_comment.as_deref(), Some("类文档"));
        assert!(find(&syms, "f").is_documented());
    }

    /// 🔴 回归测试：类的 docstring 不得被其后的方法抢走。
    ///
    /// 曾经的实现把 `"""..."""` 当作"前置注释"收集，
    /// 于是 `class A:` 的文档串挂到了紧随其后的 `def f()` 上，
    /// 而 `A` 自己的 doc_comment 为 None。
    #[test]
    fn class_docstring_is_not_stolen_by_following_method() {
        let src = "class A:\n    \"\"\"类文档\"\"\"\n\n    def f(self):\n        pass\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert_eq!(
            find(&syms, "A").doc_comment.as_deref(),
            Some("类文档"),
            "docstring 属于 class A"
        );
        assert!(
            find(&syms, "f").doc_comment.is_none(),
            "方法 f 没有自己的 docstring，不应继承类的"
        );
    }

    /// 方法自己的 docstring 必须归方法，而不是被下一个符号抢走。
    #[test]
    fn method_docstring_belongs_to_that_method() {
        let src = "class A:\n    def f(self):\n        \"\"\"F 的文档\"\"\"\n        return 1\n\n    def g(self):\n        return 2\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert_eq!(find(&syms, "f").doc_comment.as_deref(), Some("F 的文档"));
        assert!(find(&syms, "g").doc_comment.is_none(), "g 无文档串");
    }

    #[test]
    fn extracts_multiline_python_docstring() {
        let src = "def f():\n    \"\"\"第一行说明\n\n    Args:\n        x: 参数\n    \"\"\"\n    return 1\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        let doc = find(&syms, "f").doc_comment.clone().unwrap();
        assert!(doc.contains("第一行说明"), "应含首行: {doc}");
        assert!(doc.contains("参数"), "应含后续行: {doc}");
        assert!(!doc.contains("\"\"\""), "不应残留引号");
    }

    #[test]
    fn single_quote_docstring_supported() {
        let src = "def f():\n    '''单引号文档'''\n    pass\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert_eq!(find(&syms, "f").doc_comment.as_deref(), Some("单引号文档"));
    }

    /// 声明后第一条语句不是 docstring 时，不得编造文档。
    #[test]
    fn non_docstring_first_statement_yields_no_doc() {
        let src = "def f():\n    x = 1\n    return x\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert!(find(&syms, "f").doc_comment.is_none());
    }

    /// `#` 注释仍在声明之前，照常工作。
    #[test]
    fn python_hash_comment_before_declaration() {
        let src = "# 这是说明\ndef f():\n    pass\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert_eq!(find(&syms, "f").doc_comment.as_deref(), Some("这是说明"));
    }

    /// 跨多行的函数签名（类型注解常见）也能正确回填 docstring。
    #[test]
    fn multiline_signature_then_docstring() {
        let src = "async def generate(\n    self,\n    prompt: str,\n) -> Result:\n    \"\"\"生成视频\"\"\"\n    return None\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert_eq!(find(&syms, "generate").doc_comment.as_deref(), Some("生成视频"));
    }

    /// 未闭合的 docstring 不得让抽取器读穿整个文件或 panic。
    #[test]
    fn unterminated_docstring_does_not_hang() {
        let src = "def f():\n    \"\"\"没有闭合\n    x = 1\n    y = 2\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert_eq!(syms.len(), 1, "仍应抽出符号");
    }

    #[test]
    fn extracts_python_module_constants() {
        let src = "MAX_RETRY = 3\nAPI_BASE_URL = \"http://x\"\nlocal_var = 1\n";
        let syms = HeuristicExtractor.extract("Python", "c.py", src);
        assert!(names(&syms).contains(&"MAX_RETRY".to_string()));
        assert!(names(&syms).contains(&"API_BASE_URL".to_string()));
        assert!(!names(&syms).contains(&"local_var".to_string()), "小写局部变量不是模块常量");
        assert_eq!(find(&syms, "MAX_RETRY").kind, SymbolKind::Constant);
    }

    // ── Rust ────────────────────────────────────────────────────────

    #[test]
    fn extracts_rust_items() {
        let src = r#"
/// 文档注释
pub fn export_projects(db: &Pool) -> Result<Vec<Project>> {
    Ok(vec![])
}

pub(crate) struct Scanner { config: ScanConfig }

enum Status { Active, Paused }

pub trait Extractor {
    fn extract(&self, s: &str) -> Vec<Symbol>;
}

impl Extractor for Heuristic {
    fn extract(&self, s: &str) -> Vec<Symbol> { vec![] }
}

const MAX_DEPTH: usize = 6;
"#;
        let syms = HeuristicExtractor.extract("Rust", "lib.rs", src);
        let f = find(&syms, "export_projects");
        assert_eq!(f.kind, SymbolKind::Function);
        assert_eq!(f.doc_comment.as_deref(), Some("文档注释"));
        assert!(f.signature.contains("pub fn export_projects"));

        assert_eq!(find(&syms, "Scanner").kind, SymbolKind::Struct);
        assert_eq!(find(&syms, "Status").kind, SymbolKind::Enum);
        assert_eq!(find(&syms, "Extractor").kind, SymbolKind::Trait);
        assert_eq!(find(&syms, "MAX_DEPTH").kind, SymbolKind::Constant);
    }

    #[test]
    fn rust_qualifiers_are_stripped() {
        for decl in [
            "pub fn a() {}",
            "pub(crate) fn b() {}",
            "async fn c() {}",
            "pub async fn d() {}",
            "unsafe fn e() {}",
        ] {
            let syms = HeuristicExtractor.extract("Rust", "x.rs", decl);
            assert_eq!(syms.len(), 1, "应识别: {decl}");
            assert_eq!(syms[0].kind, SymbolKind::Function);
        }
    }

    #[test]
    fn rust_impl_block_maps_to_host_type() {
        let src = "impl Scanner {\n    fn run(&self) {}\n}\n";
        let syms = HeuristicExtractor.extract("Rust", "x.rs", src);
        assert!(names(&syms).contains(&"run".to_string()));
    }

    // ── TypeScript / JavaScript ─────────────────────────────────────

    #[test]
    fn extracts_ts_functions_and_components() {
        let src = r#"
export function calculateTotal(items: Item[]): number {
  return items.reduce((a, b) => a + b.price, 0);
}

export const ProjectCard = ({ project }: Props) => {
  return <div>{project.name}</div>;
};

const helper = async () => { await fetch('/x'); };

class AssetRepo {
  list() { return []; }
}

export interface SearchQuery { q: string }
export type SortBy = 'relevance' | 'date';
"#;
        let syms = HeuristicExtractor.extract("TypeScript", "a.tsx", src);
        assert_eq!(find(&syms, "calculateTotal").kind, SymbolKind::Function);
        // 大写开头的箭头函数 = React 组件
        assert_eq!(find(&syms, "ProjectCard").kind, SymbolKind::Component);
        assert_eq!(find(&syms, "helper").kind, SymbolKind::Function);
        assert_eq!(find(&syms, "AssetRepo").kind, SymbolKind::Class);
        assert_eq!(find(&syms, "SearchQuery").kind, SymbolKind::Interface);
        assert_eq!(find(&syms, "SortBy").kind, SymbolKind::Type);
    }

    #[test]
    fn lowercase_arrow_is_function_not_component() {
        let src = "export const useTheme = () => useState('dark');\n";
        let syms = HeuristicExtractor.extract("TypeScript", "h.ts", src);
        assert_eq!(syms[0].kind, SymbolKind::Function, "小写开头不是组件");
    }

    #[test]
    fn extracts_express_routes_as_api_endpoints() {
        let src = r#"
app.get('/api/projects', listProjects);
router.post("/api/scan", startScan);
app.delete('/api/projects/:id', removeProject);
"#;
        let syms = HeuristicExtractor.extract("JavaScript", "routes.js", src);
        let eps: Vec<&Symbol> = syms.iter().filter(|s| s.kind == SymbolKind::ApiEndpoint).collect();
        assert_eq!(eps.len(), 3);
        assert!(eps.iter().any(|e| e.name == "GET /api/projects"));
        assert!(eps.iter().any(|e| e.name == "POST /api/scan"));
        assert!(eps.iter().any(|e| e.name == "DELETE /api/projects/:id"));
    }

    // ── Go / Java / C++ ─────────────────────────────────────────────

    #[test]
    fn extracts_go_funcs_and_methods() {
        let src = r#"
func NewServer(cfg Config) *Server {
	return &Server{}
}

func (s *Server) Serve() error {
	return nil
}

type Handler interface {
	Handle()
}

type Config struct {
	Port int
}
"#;
        let syms = HeuristicExtractor.extract("Go", "main.go", src);
        assert_eq!(find(&syms, "NewServer").kind, SymbolKind::Function);
        assert_eq!(find(&syms, "Serve").kind, SymbolKind::Method);
        assert_eq!(find(&syms, "Handler").kind, SymbolKind::Interface);
        assert_eq!(find(&syms, "Config").kind, SymbolKind::Struct);
    }

    #[test]
    fn extracts_java_classes_methods_and_routes() {
        let src = r#"
@RestController
public class ProjectController {
    @GetMapping("/api/projects")
    public List<Project> list() {
        return service.findAll();
    }

    private void helper() {}
}
"#;
        let syms = HeuristicExtractor.extract("Java", "C.java", src);
        assert_eq!(find(&syms, "ProjectController").kind, SymbolKind::Class);
        assert!(names(&syms).contains(&"list".to_string()));
        let eps: Vec<&Symbol> = syms.iter().filter(|s| s.kind == SymbolKind::ApiEndpoint).collect();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].name, "GET /api/projects");
    }

    #[test]
    fn extracts_cpp_class_and_function() {
        let src = "class Pipeline {\npublic:\n    void run();\n};\n\nint compute_sum(int a, int b) {\n    return a + b;\n}\n";
        let syms = HeuristicExtractor.extract("C++", "p.cpp", src);
        assert_eq!(find(&syms, "Pipeline").kind, SymbolKind::Class);
        assert!(names(&syms).contains(&"compute_sum".to_string()));
    }

    // ── 不支持的语言 ────────────────────────────────────────────────

    /// 不支持的语言必须返回空，绝不编造符号。
    #[test]
    fn unsupported_language_yields_nothing() {
        assert!(!HeuristicExtractor.supports("Brainfuck"));
        assert!(HeuristicExtractor.extract("Brainfuck", "a.bf", "++++").is_empty());
        assert!(HeuristicExtractor.extract("Markdown", "a.md", "# 标题").is_empty());
        assert!(HeuristicExtractor.extract("JSON", "a.json", "{}").is_empty());
    }

    #[test]
    fn supported_languages_are_declared() {
        for l in ["Python", "Rust", "TypeScript", "JavaScript", "Go", "Java", "C++", "Ruby", "PHP", "Swift", "Kotlin", "C#", "Vue", "C"] {
            assert!(HeuristicExtractor.supports(l), "{l} 应被支持");
        }
    }

    // ── 控制流不应被当成符号 ────────────────────────────────────────

    #[test]
    fn control_flow_is_not_extracted_as_symbol() {
        let src = r#"
public class A {
    public void m() {
        if (x > 0) { return; }
        for (int i = 0; i < 10; i++) { }
        while (running) { }
        switch (v) { }
        try { } catch (Exception e) { }
    }
}
"#;
        let syms = HeuristicExtractor.extract("Java", "A.java", src);
        let ns = names(&syms);
        for kw in ["if", "for", "while", "switch", "try", "catch"] {
            assert!(!ns.contains(&kw.to_string()), "{kw} 不应被识别为符号");
        }
    }

    // ── 一次性代码识别 ──────────────────────────────────────────────

    /// 《计划.md》§2：`debug_test_2025()` 是一次性代码，不该进资产库。
    #[test]
    fn detects_disposable_names() {
        let disposable = [
            "debug_test_2025", "tmp_helper", "scratch_thing", "wip_parser",
            "hack_fix", "foo", "bar", "dummy_data", "old_impl", "backup_v2",
            "xxx_todo", "untitled_script", "parser_bak", "thing_deprecated",
        ];
        for n in disposable {
            let s = Symbol {
                name: n.to_string(),
                kind: SymbolKind::Function,
                language: "Python".into(),
                file_path: "a.py".into(),
                line: 1,
                signature: String::new(),
                doc_comment: None,
                body_lines: 5,
                parent: None,
                referenced_locally: false,
            };
            assert!(s.looks_disposable(), "{n} 应判定为一次性代码");
        }
    }

    #[test]
    fn real_symbols_are_not_disposable() {
        for n in ["batch_process", "resize_image", "generate_thumbnail", "TaskQueue", "export_projects"] {
            let s = Symbol {
                name: n.to_string(),
                kind: SymbolKind::Function,
                language: "Python".into(),
                file_path: "a.py".into(),
                line: 1,
                signature: String::new(),
                doc_comment: None,
                body_lines: 5,
                parent: None,
                referenced_locally: false,
            };
            assert!(!s.looks_disposable(), "{n} 是真实符号");
        }
    }

    // ── 引用标记 ────────────────────────────────────────────────────

    #[test]
    fn marks_locally_referenced_symbols() {
        let src = "def helper():\n    return 1\n\ndef main():\n    a = helper()\n    b = helper()\n    return a + b\n\ndef unused_fn():\n    pass\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert!(find(&syms, "helper").referenced_locally, "被调用两次应标记");
        assert!(!find(&syms, "unused_fn").referenced_locally, "未被引用不应标记");
    }

    /// 词边界匹配：`foo` 不应被 `foobar` 命中。
    #[test]
    fn reference_matching_respects_word_boundaries() {
        let src = "def foo():\n    pass\n\ndef foobar():\n    return 1\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert!(!find(&syms, "foo").referenced_locally, "foobar 不算引用 foo");
    }

    // ── 行号与签名 ──────────────────────────────────────────────────

    #[test]
    fn line_numbers_are_one_based_and_accurate() {
        let src = "import os\n\n\ndef first():\n    pass\n\n\ndef second():\n    pass\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        assert_eq!(find(&syms, "first").line, 4);
        assert_eq!(find(&syms, "second").line, 8);
    }

    #[test]
    fn signature_is_truncated() {
        let long_arg = "x".repeat(300);
        let src = format!("def f({long_arg}):\n    pass\n");
        let syms = HeuristicExtractor.extract("Python", "a.py", &src);
        assert!(syms[0].signature.chars().count() <= 160);
        assert!(syms[0].signature.ends_with('…'));
    }

    #[test]
    fn body_lines_estimate_size() {
        let src = "def small():\n    return 1\n\ndef big():\n    a = 1\n    b = 2\n    c = 3\n    d = 4\n    return a+b+c+d\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        let small = find(&syms, "small").body_lines;
        let big = find(&syms, "big").body_lines;
        assert!(big > small, "big={big} small={small}");
        assert!(small >= 1);
    }

    // ── 文件路径 ────────────────────────────────────────────────────

    #[test]
    fn symbol_carries_relative_file_path() {
        let syms = HeuristicExtractor.extract("Python", "src/services/video.py", "def f():\n    pass\n");
        assert_eq!(syms[0].file_path, "src/services/video.py");
        assert_eq!(syms[0].language, "Python");
    }

    // ── 辅助函数 ────────────────────────────────────────────────────

    #[test]
    fn take_ident_stops_at_non_ident_chars() {
        assert_eq!(take_ident("foo_bar(x: int)"), "foo_bar");
        assert_eq!(take_ident("Bar: "), "Bar");
        assert_eq!(take_ident("$ref = 1"), "$ref");
        assert_eq!(take_ident("(self)"), "");
    }

    #[test]
    fn valid_symbol_name_rejects_keywords_and_junk() {
        assert!(is_valid_symbol_name("compute"));
        assert!(is_valid_symbol_name("_private"));
        assert!(!is_valid_symbol_name("if"));
        assert!(!is_valid_symbol_name("return"));
        assert!(!is_valid_symbol_name(""));
        assert!(!is_valid_symbol_name("123abc"));
    }

    #[test]
    fn regex_escape_handles_metacharacters() {
        assert_eq!(regex_escape("$ref"), "\\$ref");
        assert_eq!(regex_escape("plain"), "plain");
        assert_eq!(regex_escape("a.b"), "a\\.b");
    }

    #[test]
    fn count_matches_finds_word_occurrences() {
        assert_eq!(count_matches("foo foo foobar", r"\bfoo\b"), 2);
        assert_eq!(count_matches("no match", r"\bfoo\b"), 0);
        assert_eq!(count_matches("", r"\b\b"), 0);
    }

    #[test]
    fn extract_first_string_lit_handles_quotes() {
        assert_eq!(extract_first_string_lit("'/api/x', handler)").as_deref(), Some("/api/x"));
        assert_eq!(extract_first_string_lit("\"/api/y\")").as_deref(), Some("/api/y"));
        assert_eq!(extract_first_string_lit("no quotes"), None);
    }

    #[test]
    fn symbol_kind_metadata() {
        for k in [
            SymbolKind::Function, SymbolKind::Method, SymbolKind::Class, SymbolKind::Struct,
            SymbolKind::Enum, SymbolKind::Trait, SymbolKind::Interface, SymbolKind::Constant,
            SymbolKind::Component, SymbolKind::ApiEndpoint, SymbolKind::Type,
        ] {
            assert!(!k.as_str().is_empty());
            assert!(!k.label_zh().is_empty());
        }
        assert!(SymbolKind::Function.is_reusable_unit());
        assert!(SymbolKind::Component.is_reusable_unit());
        assert!(!SymbolKind::Constant.is_reusable_unit(), "常量不该作为可复用资产");
        assert!(!SymbolKind::Type.is_reusable_unit());
    }

    #[test]
    fn extractor_name_is_stable() {
        assert_eq!(HeuristicExtractor::new().name(), "heuristic-v1");
    }

    #[test]
    fn empty_file_yields_no_symbols() {
        assert!(HeuristicExtractor.extract("Python", "a.py", "").is_empty());
        assert!(HeuristicExtractor.extract("Rust", "a.rs", "\n\n\n").is_empty());
    }

    #[test]
    fn comments_only_file_yields_no_symbols() {
        let src = "// 只是一个注释\n// 没有代码\n";
        assert!(HeuristicExtractor.extract("Rust", "a.rs", src).is_empty());
    }

    #[test]
    fn qualified_name_format() {
        let mut s = Symbol {
            name: "save".into(),
            kind: SymbolKind::Method,
            language: "Python".into(),
            file_path: "a.py".into(),
            line: 1,
            signature: String::new(),
            doc_comment: None,
            body_lines: 1,
            parent: Some("Repo".into()),
            referenced_locally: false,
        };
        assert_eq!(s.qualified_name(), "Repo.save");
        s.parent = None;
        assert_eq!(s.qualified_name(), "save");
    }
}
