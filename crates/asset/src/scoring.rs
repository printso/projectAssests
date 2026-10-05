//! 资产评分：`reuse_score` / `generality` / `stability`。
//!
//! # 设计原则（《产品设计书》§2 核心判断 #4：确定性引擎优先）
//! 三个分数**全部由确定性规则计算**，不调用 LLM：
//! - 可解释：每个分数都能拆解为具体因子，写进 Evidence 的 reasoning
//! - 可复现：同一份代码两次评分结果相同，可做回归测试
//! - 可审计：用户点开"为什么认为它可复用"能看到真实依据
//!
//! 🔴 红线：**绝不给没有证据的资产高分**。
//! `Evidence::is_sufficient()` 为 false 的资产在存储层就会被拒绝入库
//! （见 `projectassests-storage::AssetRepo::upsert_conn`），评分层也不应为它们背书。

use serde::{Deserialize, Serialize};

use crate::symbols::{Symbol, SymbolKind};

/// 评分权重。集中定义便于调参与审查——
/// 散落各处的魔法数字是"评分不可解释"的根源。
#[derive(Debug, Clone, Serialize)]
pub struct ScoreWeights {
    /// 体量：太小的函数没有独立复用价值
    pub size: f64,
    /// 通用性：命名与参数是否泛化（不含项目专有名词）
    pub generality: f64,
    /// 文档：有文档串说明作者认为它值得被他人理解
    pub documentation: f64,
    /// 被引用：文件内被调用说明承担实际职责
    pub referenced: f64,
    /// 种类：类/组件/Trait 比裸函数更有复用价值
    pub kind_bonus: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            size: 0.25,
            generality: 0.30,
            documentation: 0.15,
            referenced: 0.20,
            kind_bonus: 0.10,
        }
    }
}

impl ScoreWeights {
    /// 权重之和必须为 1.0，否则分数会系统性偏移。
    ///
    /// 这不是形式主义：`ScoreWeights` 是可配置的，
    /// 用户/贡献者调参后若和不为 1，reuse_score 可能超过 1.0，
    /// 前端进度条与徽章阈值全部失效。
    pub fn total(&self) -> f64 {
        self.size + self.generality + self.documentation + self.referenced + self.kind_bonus
    }

    /// 权重之和是否等于 1.0（在浮点容差内）。
    ///
    /// 🔴 容差必须是 `WEIGHT_TOLERANCE`（1e-9），**不能用 `f64::EPSILON`**：
    /// 五个 0.05~0.30 的数相加，实测结果是 `1.0000000000000002`，
    /// 偏差 2.2e-16 恰好略大于 EPSILON(2.22e-16) —— 用 EPSILON 判等会误报"权重非法"。
    /// EPSILON 的语义是"1.0 附近的最小可分辨间隔"，不是"累加误差上限"。
    pub fn is_valid(&self) -> bool {
        (self.total() - 1.0).abs() < WEIGHT_TOLERANCE
    }
}

/// 权重和的浮点容差。
///
/// 取 1e-9：远大于多次加法可能累积的误差（~1e-16），
/// 又远小于任何有意义的配置错误（人手写错权重至少差 0.01）。
pub const WEIGHT_TOLERANCE: f64 = 1e-9;

/// 评分结果（含可解释的因子明细）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Score {
    /// 综合复用价值 0.0-1.0
    pub reuse_score: f64,
    /// 通用性 0.0-1.0（与业务逻辑的解耦程度）
    pub generality: f64,
    /// 稳定性 0.0-1.0（是否像"已经定型"的代码）
    pub stability: f64,
    /// 置信度 0.0-1.0（对本次评分结果的确信程度）
    pub confidence: f64,
    /// 人类可读的判定理由（写入 Evidence.reasoning）
    pub reasoning: Vec<String>,
    /// 是否值得进入资产库（低于门槛的符号会被丢弃，避免噪音）
    pub worth_tracking: bool,
}

/// 评分器。
#[derive(Debug, Clone)]
pub struct Scorer {
    weights: ScoreWeights,
    /// 低于此 reuse_score 的符号不入库（《技术设计书》§25：低置信度不进图谱）
    pub threshold: f64,
}

impl Default for Scorer {
    fn default() -> Self {
        Self::new()
    }
}

impl Scorer {
    pub fn new() -> Self {
        Self {
            weights: ScoreWeights::default(),
            threshold: DEFAULT_THRESHOLD,
        }
    }

    pub fn with_weights(weights: ScoreWeights) -> Self {
        Self { weights, threshold: DEFAULT_THRESHOLD }
    }

    /// 入库门槛。低于此分数的符号视为噪音。
    pub const DEFAULT_THRESHOLD: f64 = 0.42;

    /// 为一个符号评分。
    ///
    /// `project_hint` 是当前项目名，用于检测"名字里带项目专有名词"
    /// （如 `YingtechVideoService`），这类符号通用性低。
    pub fn score_symbol(&self, sym: &Symbol, project_hint: Option<&str>) -> Score {
        let mut reasoning: Vec<String> = Vec::new();

        // ── 一次性代码：直接判死，不参与加权 ──────────────────────
        // `debug_test_2025()` 这类即使体量大、被引用多，也没有复用价值。
        if sym.looks_disposable() {
            return Score {
                reuse_score: 0.0,
                generality: 0.0,
                stability: 0.0,
                confidence: 0.9, // 判定"无价值"本身是高置信的
                reasoning: vec![format!(
                    "命名「{}」呈现一次性/调试特征，判定为不可复用",
                    sym.name
                )],
                worth_tracking: false,
            };
        }

        // ── 因子 1：体量 ────────────────────────────────────────────
        let size_score = score_body_size(sym.body_lines);
        if sym.body_lines >= 8 {
            reasoning.push(format!("实现体量 {} 行，具备独立价值", sym.body_lines));
        } else if sym.body_lines <= 2 {
            reasoning.push(format!("实现仅 {} 行，过于琐碎", sym.body_lines));
        }

        // 🔴 硬性体量下限：1-2 行的实现（getter / 转发 / 占位）无论其它因子多高，
        // 都不值得作为独立资产入库。
        //
        // 没有这条时的真实后果：一个 2 行、带文档、被引用的 Class
        // 会算出 0.446 > 门槛 0.42 而入库。用户打开资产库看到一堆
        // `def get_id(self): return self._id` —— 这正是《产品设计书》§12
        // 列出的头号风险「资产库全是噪音」，会让产品失去可信度。
        let substantial = sym.body_lines >= MIN_SUBSTANTIAL_BODY_LINES;

        // ── 因子 2：通用性 ──────────────────────────────────────────
        let (gen_score, gen_reasons) = score_generality(sym, project_hint);
        reasoning.extend(gen_reasons);

        // ── 因子 3：文档 ────────────────────────────────────────────
        let doc_score = if sym.is_documented() { 1.0 } else { 0.25 };
        if sym.is_documented() {
            reasoning.push("带有文档注释，作者已表达复用意图".to_string());
        }

        // ── 因子 4：被引用 ──────────────────────────────────────────
        let ref_score = if sym.referenced_locally { 1.0 } else { 0.35 };
        if sym.referenced_locally {
            reasoning.push("在同文件内被其他代码调用".to_string());
        } else {
            reasoning.push("未被同文件其他代码引用".to_string());
        }

        // ── 因子 5：符号种类 ────────────────────────────────────────
        let kind_score = score_kind(sym.kind);

        // ── 加权汇总 ────────────────────────────────────────────────
        let w = &self.weights;
        // 权重和被显式校验：配置错误时归一化而不是产出越界分数
        let wsum = w.total().max(f64::EPSILON);
        let raw = size_score * w.size
            + gen_score * w.generality
            + doc_score * w.documentation
            + ref_score * w.referenced
            + kind_score * w.kind_bonus;
        let reuse_score = (raw / wsum).clamp(0.0, 1.0);

        // ── 稳定性：命名规范 + 体量 + 有文档 → 像已定型的代码 ────────
        let stability = compute_stability(sym, doc_score, size_score).clamp(0.0, 1.0);

        // ── 置信度：信息越完整越确信 ────────────────────────────────
        // 启发式抽取有已知局限（见 symbols.rs），因此置信度上限设为 0.92，
        // 不给 1.0——诚实地表达"这是基于规则的推断，不是 AST 级精确分析"。
        let mut confidence: f64 = 0.6;
        if sym.is_documented() {
            confidence += 0.15;
        }
        if sym.body_lines > 0 {
            confidence += 0.1;
        }
        if !sym.signature.is_empty() {
            confidence += 0.07;
        }
        let confidence = confidence.min(0.92);

        let worth_tracking =
            reuse_score >= self.threshold && sym.kind.is_reusable_unit() && substantial;
        if !worth_tracking {
            if !sym.kind.is_reusable_unit() {
                reasoning.push(format!(
                    "「{}」粒度过细（{}），不作为独立资产",
                    sym.name,
                    sym.kind.label_zh()
                ));
            } else if !substantial {
                reasoning.push(format!(
                    "实现不足 {} 行，属于 getter/转发一类的琐碎代码",
                    MIN_SUBSTANTIAL_BODY_LINES
                ));
            } else {
                reasoning.push(format!(
                    "综合评分 {:.2} 低于入库门槛 {:.2}",
                    reuse_score, self.threshold
                ));
            }
        }

        Score {
            reuse_score: round2(reuse_score),
            generality: round2(gen_score),
            stability: round2(stability),
            confidence: round2(confidence),
            reasoning,
            worth_tracking,
        }
    }
}

/// 默认入库门槛。
pub const DEFAULT_THRESHOLD: f64 = Scorer::DEFAULT_THRESHOLD;

/// 资产入库的最小实现体量（行）。
///
/// 1-2 行的 getter / 转发 / 占位实现没有独立复用价值。
/// 设为硬性下限而非仅靠加权：加权分可能被"有文档 + 被引用 + 是类"
/// 这几个因子抬过门槛，从而把琐碎代码放进资产库。
pub const MIN_SUBSTANTIAL_BODY_LINES: usize = 3;

/// 体量评分。
///
/// 曲线设计：1-2 行（getter/转发）几乎无价值；8-120 行是"能独立成模块"的甜区；
/// 超过 300 行往往是上帝类，复用性反而下降（要先拆分）。
fn score_body_size(body_lines: usize) -> f64 {
    match body_lines {
        0 => 0.0,
        1..=2 => 0.25,
        3..=7 => 0.6,
        8..=120 => 1.0,
        121..=300 => 0.85,
        _ => 0.6, // 过大：可能是上帝类或生成物
    }
}

/// 符号种类评分。
fn score_kind(kind: SymbolKind) -> f64 {
    match kind {
        // 组件与 Trait/接口：天然为复用而设计
        SymbolKind::Component | SymbolKind::Trait | SymbolKind::Interface => 1.0,
        SymbolKind::Class | SymbolKind::Struct | SymbolKind::Enum => 0.9,
        // API 端点：可复用但通常与业务路由耦合
        SymbolKind::ApiEndpoint => 0.7,
        SymbolKind::Method | SymbolKind::Function => 0.8,
        SymbolKind::Constant | SymbolKind::Type => 0.35,
    }
}

/// 通用性评分：命名与签名是否泛化。
///
/// 返回 `(分数, 理由)`——理由必须一并产出，因为产品纪律要求
/// "AI 的任何结论都要能回溯到依据"，评分也不例外。
fn score_generality(sym: &Symbol, project_hint: Option<&str>) -> (f64, Vec<String>) {
    let mut score = 0.5;
    let mut reasons: Vec<String> = Vec::new();
    let lower = sym.name.to_ascii_lowercase();
    let sig_lower = sym.signature.to_ascii_lowercase();

    // 减分：项目专有名词
    if let Some(proj) = project_hint {
        let norm = proj.to_ascii_lowercase().replace(['-', '_', ' '], "");
        // 只检查长度 ≥4 的项目名，避免 "app"/"api" 这类通用词误伤
        if norm.len() >= 4 && lower.contains(&norm) {
            score -= 0.3;
            reasons.push(format!("命名含项目专有词「{proj}」，跨项目复用性低"));
        }
    }

    // 减分：业务实体名词（订单/用户/支付等强业务耦合）
    let business_hits: Vec<&str> = BUSINESS_TERMS
        .iter()
        .filter(|t| lower.contains(**t))
        .copied()
        .collect();
    if !business_hits.is_empty() {
        score -= 0.18 * (business_hits.len().min(2) as f64);
        reasons.push(format!(
            "命名含业务专有词（{}），与具体业务耦合",
            business_hits.join("、")
        ));
    }

    // 加分：通用技术词（这些是跨项目复用的强信号）
    let generic_hits: Vec<&str> = GENERIC_TERMS
        .iter()
        .filter(|t| lower.contains(**t))
        .copied()
        .collect();
    if !generic_hits.is_empty() {
        score += 0.12 * (generic_hits.len().min(3) as f64);
        reasons.push(format!(
            "命名体现通用能力（{}）",
            generic_hits.join("、")
        ));
    }

    // 减分：签名里出现具体业务类型/绝对路径
    if sig_lower.contains("c:/") || sig_lower.contains("d:/") || sig_lower.contains("/users/") {
        score -= 0.2;
        reasons.push("签名中出现硬编码路径，复用前需参数化".to_string());
    }

    // 加分：签名含类型注解（接口明确 = 易复用）
    if sym.signature.contains("-> ") || sym.signature.contains(": ") && sym.signature.contains('(') {
        score += 0.1;
        reasons.push("签名带类型注解，输入输出明确".to_string());
    }

    // 减分：名字过短（`f`、`go`）通常是局部胶水
    if sym.name.chars().count() <= 2 && sym.parent.is_none() {
        score -= 0.2;
        reasons.push("命名过短，语义不明".to_string());
    }

    // 加分：公开 API 命名风格（Rust pub / TS export 已在签名里体现）
    if sym.signature.contains("pub ") || sym.signature.contains("export ") {
        score += 0.1;
        reasons.push("作为公开接口导出".to_string());
    }

    (score.clamp(0.0, 1.0), reasons)
}

/// 稳定性评分：像"已定型"的代码而非实验性草稿。
fn compute_stability(sym: &Symbol, doc_score: f64, size_score: f64) -> f64 {
    let mut s = 0.5;
    // 有文档 → 作者已认真维护
    s += (doc_score - 0.25) * 0.3;
    // 体量适中 → 不是一次性草稿也不是失控的巨物
    s += (size_score - 0.5) * 0.25;
    // 命名规范（非 disposable，已在评分前置检查中排除）
    if sym.name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
        s += 0.1;
    }
    // 有父级（属于某个类/模块）→ 有组织归属，比游离函数稳定
    if sym.parent.is_some() {
        s += 0.1;
    }
    s.clamp(0.0, 1.0)
}

/// 保留两位小数。
///
/// 必须显式处理：`0.30000000000000004` 这类浮点尾数会让前端
/// `toFixed(2)` 之外的直接展示出现难看的长串。
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// 业务专有词（命中即降低通用性）。
const BUSINESS_TERMS: &[&str] = &[
    "order", "invoice", "payment", "billing", "customer", "tenant",
    "employee", "salary", "contract", "cart", "checkout", "loyalty",
    "yingtech", "settlement", "warehouse", "shipment",
];

/// 通用技术词（命中即提升通用性）。
const GENERIC_TERMS: &[&str] = &[
    "queue", "cache", "retry", "batch", "pipeline", "parser", "pool",
    "throttle", "debounce", "validator", "sanitize", "encode", "decode",
    "compress", "hash", "uuid", "pagination", "ratelimit", "circuit",
    "scheduler", "worker", "stream", "buffer", "tokenizer", "embedding",
    "http", "client", "logger", "config", "registry", "factory", "adapter",
    "middleware", "resolver", "dispatcher", "iterator", "merkle", "diff",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str, kind: SymbolKind, body: usize) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            language: "Python".into(),
            file_path: "src/a.py".into(),
            line: 1,
            signature: format!("def {name}(self):"),
            doc_comment: None,
            body_lines: body,
            parent: None,
            referenced_locally: false,
        }
    }

    // ── 权重 ────────────────────────────────────────────────────────

    /// 权重之和必须为 1，否则分数系统性偏移、可能越界。
    ///
    /// 注意用 `WEIGHT_TOLERANCE` 而非 `f64::EPSILON`：五个权重相加实测得
    /// 1.0000000000000002，偏差略大于 EPSILON，用 EPSILON 判等会误报。
    #[test]
    fn default_weights_sum_to_one() {
        let w = ScoreWeights::default();
        assert!(
            (w.total() - 1.0).abs() < WEIGHT_TOLERANCE,
            "权重和 = {}",
            w.total()
        );
        assert!(w.is_valid());
    }

    #[test]
    fn invalid_weights_are_detected() {
        let w = ScoreWeights { size: 0.5, generality: 0.5, documentation: 0.5, referenced: 0.5, kind_bonus: 0.5 };
        assert!(!w.is_valid());
    }

    /// 即使配置了非法权重，分数也必须被归一化到 0-1，不能越界。
    #[test]
    fn score_never_exceeds_one_even_with_bad_weights() {
        let scorer = Scorer::with_weights(ScoreWeights {
            size: 2.0, generality: 2.0, documentation: 2.0, referenced: 2.0, kind_bonus: 2.0,
        });
        let mut s = sym("TaskQueue", SymbolKind::Class, 60);
        s.doc_comment = Some("任务队列".into());
        s.referenced_locally = true;
        let score = scorer.score_symbol(&s, None);
        assert!(score.reuse_score <= 1.0, "越界: {}", score.reuse_score);
        assert!(score.reuse_score >= 0.0);
    }

    // ── 分数范围 ────────────────────────────────────────────────────

    #[test]
    fn all_scores_in_unit_interval() {
        let scorer = Scorer::new();
        for (name, kind, body) in [
            ("f", SymbolKind::Function, 1),
            ("TaskQueue", SymbolKind::Class, 60),
            ("MAX_RETRY", SymbolKind::Constant, 1),
            ("ProjectCard", SymbolKind::Component, 40),
        ] {
            let s = scorer.score_symbol(&sym(name, kind, body), Some("demo"));
            for v in [s.reuse_score, s.generality, s.stability, s.confidence] {
                assert!((0.0..=1.0).contains(&v), "{name} 分数越界: {v}");
            }
        }
    }

    // ── 一次性代码 ──────────────────────────────────────────────────

    /// 《计划.md》§2 的原始例子：`debug_test_2025()` 不该有高价值。
    #[test]
    fn disposable_names_score_zero() {
        let scorer = Scorer::new();
        for n in ["debug_test_2025", "tmp_helper", "scratch_thing", "foo"] {
            let mut s = sym(n, SymbolKind::Function, 80);
            s.doc_comment = Some("有文档也没用".into());
            s.referenced_locally = true;
            let score = scorer.score_symbol(&s, None);
            assert_eq!(score.reuse_score, 0.0, "{n} 应判 0 分");
            assert!(!score.worth_tracking, "{n} 不应入库");
            assert!(score.reasoning[0].contains("一次性"), "理由应说明原因: {:?}", score.reasoning);
        }
    }

    #[test]
    fn real_names_are_not_penalized_as_disposable() {
        let scorer = Scorer::new();
        let s = scorer.score_symbol(&sym("batch_process", SymbolKind::Function, 40), None);
        assert!(s.reuse_score > 0.0);
        assert!(s.worth_tracking);
    }

    // ── 体量因子 ────────────────────────────────────────────────────

    #[test]
    fn body_size_curve_is_sensible() {
        assert_eq!(score_body_size(0), 0.0);
        assert!(score_body_size(1) < score_body_size(5));
        assert!(score_body_size(5) < score_body_size(40));
        assert_eq!(score_body_size(40), 1.0, "甜区应为满分");
        assert!(score_body_size(500) < score_body_size(40), "上帝类应降分");
    }

    #[test]
    fn bigger_implementation_scores_higher() {
        let scorer = Scorer::new();
        let tiny = scorer.score_symbol(&sym("ImagePipeline", SymbolKind::Class, 2), None);
        let solid = scorer.score_symbol(&sym("ImagePipeline", SymbolKind::Class, 45), None);
        assert!(solid.reuse_score > tiny.reuse_score);
        assert!(!tiny.worth_tracking, "2 行的类不值得单独追踪");
    }

    /// 🔴 回归测试：体量硬下限优先于加权分数。
    ///
    /// 没有 `MIN_SUBSTANTIAL_BODY_LINES` 时，一个 2 行、带文档、被引用的 Class
    /// 会算出 0.446 > 门槛 0.42 而入库，资产库随即被 getter 淹没。
    #[test]
    fn trivial_body_never_tracked_regardless_of_other_signals() {
        let scorer = Scorer::new();
        let mut s = sym("Thing", SymbolKind::Class, 2);
        // 把其它加分因子全部拉满
        s.doc_comment = Some("有完整文档".into());
        s.referenced_locally = true;
        s.signature = "pub struct Thing {".into();
        let score = scorer.score_symbol(&s, None);
        assert!(score.reuse_score > scorer.threshold, "加权分确实越过了门槛");
        assert!(!score.worth_tracking, "但体量不足必须拦住");
        assert!(
            score.reasoning.iter().any(|r| r.contains("琐碎")),
            "理由应说明是体量问题: {:?}",
            score.reasoning
        );
    }

    /// 刚好达到下限的实现应被接受（边界值测试）。
    #[test]
    fn body_at_minimum_threshold_is_accepted() {
        let scorer = Scorer::new();
        let mut s = sym("batch_process", SymbolKind::Function, MIN_SUBSTANTIAL_BODY_LINES);
        s.doc_comment = Some("批量处理".into());
        s.referenced_locally = true;
        let score = scorer.score_symbol(&s, None);
        assert!(score.worth_tracking, "恰好 {} 行应入库", MIN_SUBSTANTIAL_BODY_LINES);
    }

    /// 断言确切值而非范围：`>= 2` 这类对常量的判断恒真，没有守护意义。
    /// 写成确切值后，有人误改这个阈值会立刻让测试失败，迫使其确认影响面
    /// （该常量直接决定多少符号被挡在资产库之外）。
    #[test]
    fn minimum_body_lines_constant_is_sane() {
        assert_eq!(MIN_SUBSTANTIAL_BODY_LINES, 3);
    }

    // ── 种类因子 ────────────────────────────────────────────────────

    #[test]
    fn component_and_trait_score_highest() {
        assert_eq!(score_kind(SymbolKind::Component), 1.0);
        assert_eq!(score_kind(SymbolKind::Trait), 1.0);
        assert!(score_kind(SymbolKind::Class) > score_kind(SymbolKind::Function));
        assert!(score_kind(SymbolKind::Constant) < score_kind(SymbolKind::Function));
    }

    /// 常量/类型别名粒度过细，不该作为独立资产（否则资产库被 MAX_RETRY 淹没）。
    #[test]
    fn constants_are_not_worth_tracking() {
        let scorer = Scorer::new();
        let s = scorer.score_symbol(&sym("MAX_RETRY", SymbolKind::Constant, 1), None);
        assert!(!s.worth_tracking);
        assert!(s.reasoning.iter().any(|r| r.contains("粒度过细")));
    }

    // ── 通用性因子 ──────────────────────────────────────────────────

    /// 项目专有名词必须降低通用性——这是"能否跨项目复用"的核心判据。
    #[test]
    fn project_specific_name_lowers_generality() {
        let scorer = Scorer::new();
        let generic = scorer.score_symbol(&sym("VideoService", SymbolKind::Class, 50), Some("yingtech"));
        let specific = scorer.score_symbol(&sym("YingtechVideoService", SymbolKind::Class, 50), Some("yingtech"));
        assert!(
            specific.generality < generic.generality,
            "specific={} generic={}",
            specific.generality,
            generic.generality
        );
        assert!(specific.reasoning.iter().any(|r| r.contains("专有词")));
    }

    /// 短项目名（app/api）不该触发专有词判定，否则大量正常符号被误伤。
    #[test]
    fn short_project_name_does_not_trigger_specificity_penalty() {
        let scorer = Scorer::new();
        let with_short = scorer.score_symbol(&sym("AppService", SymbolKind::Class, 50), Some("app"));
        let without = scorer.score_symbol(&sym("AppService", SymbolKind::Class, 50), None);
        assert_eq!(with_short.generality, without.generality);
    }

    #[test]
    fn business_terms_lower_generality() {
        let scorer = Scorer::new();
        let neutral = scorer.score_symbol(&sym("OrderProcessor", SymbolKind::Class, 50), None);
        let generic = scorer.score_symbol(&sym("BatchProcessor", SymbolKind::Class, 50), None);
        assert!(
            generic.generality > neutral.generality,
            "BatchProcessor 应比 OrderProcessor 更通用"
        );
    }

    #[test]
    fn generic_tech_terms_raise_generality() {
        let scorer = Scorer::new();
        let plain = scorer.score_symbol(&sym("Processor", SymbolKind::Class, 50), None);
        let queue = scorer.score_symbol(&sym("RetryQueue", SymbolKind::Class, 50), None);
        assert!(queue.generality > plain.generality);
        assert!(queue.reasoning.iter().any(|r| r.contains("通用能力")));
    }

    #[test]
    fn hardcoded_path_lowers_generality() {
        let scorer = Scorer::new();
        let mut s = sym("Loader", SymbolKind::Function, 30);
        s.signature = "def load(path='D:/data/x.csv'):".into();
        let score = scorer.score_symbol(&s, None);
        assert!(score.reasoning.iter().any(|r| r.contains("硬编码路径")));

        let mut clean = sym("Loader", SymbolKind::Function, 30);
        clean.signature = "def load(path: str) -> DataFrame:".into();
        assert!(scorer.score_symbol(&clean, None).generality > score.generality);
    }

    #[test]
    fn type_annotated_signature_raises_generality() {
        let scorer = Scorer::new();
        let mut annotated = sym("resize", SymbolKind::Function, 20);
        annotated.signature = "def resize(img: Image, size: int) -> Image:".into();
        let mut plain = sym("resize", SymbolKind::Function, 20);
        plain.signature = "def resize(img, size):".into();
        assert!(scorer.score_symbol(&annotated, None).generality > scorer.score_symbol(&plain, None).generality);
    }

    #[test]
    fn very_short_name_lowers_generality() {
        let scorer = Scorer::new();
        let short = scorer.score_symbol(&sym("go", SymbolKind::Function, 30), None);
        let named = scorer.score_symbol(&sym("execute_pipeline", SymbolKind::Function, 30), None);
        assert!(named.generality > short.generality);
        assert!(short.reasoning.iter().any(|r| r.contains("命名过短")));
    }

    #[test]
    fn exported_symbol_raises_generality() {
        let scorer = Scorer::new();
        let mut exported = sym("TaskQueue", SymbolKind::Class, 40);
        exported.signature = "export class TaskQueue {".into();
        let plain = scorer.score_symbol(&sym("TaskQueue", SymbolKind::Class, 40), None);
        assert!(scorer.score_symbol(&exported, None).generality > plain.generality);
    }

    // ── 文档与引用因子 ──────────────────────────────────────────────

    #[test]
    fn documented_symbol_scores_higher() {
        let scorer = Scorer::new();
        let mut documented = sym("VideoPipeline", SymbolKind::Class, 50);
        documented.doc_comment = Some("视频生成管道，支持多模型".into());
        let bare = sym("VideoPipeline", SymbolKind::Class, 50);
        let d = scorer.score_symbol(&documented, None);
        let b = scorer.score_symbol(&bare, None);
        assert!(d.reuse_score > b.reuse_score);
        assert!(d.reasoning.iter().any(|r| r.contains("文档注释")));
        assert!(d.confidence > b.confidence, "有文档应更确信");
    }

    #[test]
    fn referenced_symbol_scores_higher() {
        let scorer = Scorer::new();
        let mut referenced = sym("helper", SymbolKind::Function, 30);
        referenced.referenced_locally = true;
        let orphan = sym("helper", SymbolKind::Function, 30);
        assert!(
            scorer.score_symbol(&referenced, None).reuse_score
                > scorer.score_symbol(&orphan, None).reuse_score
        );
    }

    // ── 置信度 ──────────────────────────────────────────────────────

    /// 启发式抽取有已知局限，置信度必须诚实地低于 1.0。
    #[test]
    fn confidence_never_claims_certainty() {
        let scorer = Scorer::new();
        let mut s = sym("TaskQueue", SymbolKind::Class, 60);
        s.doc_comment = Some("队列".into());
        s.referenced_locally = true;
        s.signature = "pub struct TaskQueue {".into();
        let score = scorer.score_symbol(&s, None);
        assert!(score.confidence <= 0.92, "启发式抽取不应声称 100% 确信: {}", score.confidence);
        assert!(score.confidence >= 0.6);
    }

    // ── 稳定性 ──────────────────────────────────────────────────────

    #[test]
    fn documented_and_sized_symbols_are_more_stable() {
        let scorer = Scorer::new();
        let mut solid = sym("TaskQueue", SymbolKind::Class, 50);
        solid.doc_comment = Some("稳定实现".into());
        solid.parent = Some("core".into());
        let draft = sym("TaskQueue", SymbolKind::Class, 2);
        assert!(scorer.score_symbol(&solid, None).stability > scorer.score_symbol(&draft, None).stability);
    }

    #[test]
    fn stability_in_range() {
        let scorer = Scorer::new();
        for body in [0, 1, 5, 50, 500] {
            let s = scorer.score_symbol(&sym("Thing", SymbolKind::Class, body), None);
            assert!((0.0..=1.0).contains(&s.stability), "body={body} 越界");
        }
    }

    // ── 门槛 ────────────────────────────────────────────────────────

    #[test]
    fn threshold_filters_noise() {
        let scorer = Scorer::new();
        assert_eq!(scorer.threshold, DEFAULT_THRESHOLD);
        // 琐碎符号不入库
        let trivial = scorer.score_symbol(&sym("get", SymbolKind::Function, 1), None);
        assert!(!trivial.worth_tracking);
        // 实质符号入库
        let mut solid = sym("batch_process", SymbolKind::Function, 40);
        solid.doc_comment = Some("批量处理".into());
        solid.referenced_locally = true;
        assert!(scorer.score_symbol(&solid, None).worth_tracking);
    }

    #[test]
    fn reasoning_is_always_provided() {
        let scorer = Scorer::new();
        for (n, k, b) in [
            ("f", SymbolKind::Function, 1),
            ("TaskQueue", SymbolKind::Class, 50),
            ("debug_tmp", SymbolKind::Function, 50),
        ] {
            let s = scorer.score_symbol(&sym(n, k, b), None);
            assert!(!s.reasoning.is_empty(), "{n} 必须给出理由（产品纪律：给证据不给玄学）");
        }
    }

    // ── 纯函数 ──────────────────────────────────────────────────────

    #[test]
    fn round2_eliminates_float_noise() {
        assert_eq!(round2(0.30000000000000004), 0.3);
        assert_eq!(round2(0.123456), 0.12);
        assert_eq!(round2(1.0), 1.0);
        assert_eq!(round2(0.0), 0.0);
    }

    #[test]
    fn term_lists_are_nonempty_and_lowercase() {
        assert!(!BUSINESS_TERMS.is_empty());
        assert!(!GENERIC_TERMS.is_empty());
        for t in BUSINESS_TERMS.iter().chain(GENERIC_TERMS.iter()) {
            assert_eq!(t, &t.to_ascii_lowercase(), "词表必须小写: {t}");
            assert!(!t.is_empty());
        }
    }

    /// 两个词表不得重叠（否则同一词既加分又减分，自相矛盾）。
    #[test]
    fn term_lists_do_not_overlap() {
        for b in BUSINESS_TERMS {
            assert!(
                !GENERIC_TERMS.contains(b),
                "{b} 同时出现在业务词表与通用词表中"
            );
        }
    }

    /// 评分必须确定性：同一输入两次调用结果完全一致。
    #[test]
    fn scoring_is_deterministic() {
        let scorer = Scorer::new();
        let mut s = sym("VideoPipeline", SymbolKind::Class, 50);
        s.doc_comment = Some("管道".into());
        s.referenced_locally = true;
        let a = scorer.score_symbol(&s, Some("yingtech"));
        let b = scorer.score_symbol(&s, Some("yingtech"));
        assert_eq!(a.reuse_score, b.reuse_score);
        assert_eq!(a.generality, b.generality);
        assert_eq!(a.stability, b.stability);
        assert_eq!(a.confidence, b.confidence);
        assert_eq!(a.reasoning, b.reasoning);
    }

    #[test]
    fn score_serializes() {
        let s = Scorer::new().score_symbol(&sym("TaskQueue", SymbolKind::Class, 50), None);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("reuse_score"));
        assert!(json.contains("worth_tracking"));
        let back: Score = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reuse_score, s.reuse_score);
    }
}
