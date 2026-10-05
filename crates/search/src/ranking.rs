//! 相关性排序（纯函数，无 IO）。
//!
//! 《产品设计书》对搜索有一条硬要求：**结果必须带排序理由**。
//! 用户不接受"因为分数 0.87 所以排第一"——那是玄学。
//! 因此本模块的每个权重项都会同步产出一条人类可读的理由，
//! 分数与理由由同一段代码生成，不可能互相矛盾。
//!
//! # 排序模型
//! ```text
//! score = 0.55 * relevance        // 召回阶段的相关性（FTS bm25 或 LIKE 命中字段）
//!       + 0.25 * quality          // 实体自身质量（reuse_score / project_count / health）
//!       + 0.20 * recency          // 新鲜度（最近更新的优先）
//! ```
//! 三项都归一到 0..=1。权重和恰为 1，故最终分也在 0..=1，
//! 前端可直接当进度条宽度用，无需二次换算。
//!
//! 用户显式选择排序方式（复用价值/最近更新/置信度）时，
//! 主排序键换成对应字段，相关性退为次键——**尊重用户意图优先于算法判断**。

use spolia_domain::{Asset, Capability, Insight, Opportunity, Project, SortBy};

/// 相关性权重。
pub const W_RELEVANCE: f64 = 0.55;
/// 实体质量权重。
pub const W_QUALITY: f64 = 0.25;
/// 新鲜度权重。
pub const W_RECENCY: f64 = 0.20;

/// "最近"的天数窗口：30 天内视为满分，之后线性衰减到 0。
///
/// 为什么是 30 天：项目盘点场景里，一个月内动过的项目才值得优先推荐复用；
/// 半年前的项目即使质量高，其代码也大概率已与当前技术栈脱节。
pub const RECENCY_WINDOW_DAYS: i64 = 30;

/// 无独立时间线的实体（如资产）在"最近更新"排序下的中性分。
///
/// 取 0.5 而非 0：让它与项目可比，而不是无条件沉底。
pub const NEUTRAL_RECENCY: f64 = 0.5;

/// 排序输入：召回分数 + 实体质量 + 新鲜度。
#[derive(Debug, Clone)]
pub struct RankInput {
    /// 召回阶段的相关性分数（0..=1，越大越相关）
    pub relevance: f64,
    /// 实体质量分数（0..=1）
    pub quality: f64,
    /// 新鲜度分数（0..=1）
    pub recency: f64,
    /// 是否走了 LIKE 子串回退（影响理由措辞，不影响分数）
    pub from_substring: bool,
}

impl RankInput {
    /// 综合分与排序理由。
    pub fn score(&self) -> f64 {
        (W_RELEVANCE * self.relevance + W_QUALITY * self.quality + W_RECENCY * self.recency)
            .clamp(0.0, 1.0)
    }

    /// 生成排序理由（最多 3 条，按贡献度降序）。
    ///
    /// 🔴 纪律：理由必须来自**实际参与计算**的项。
    /// 若某项分数为 0，就不要说"质量较高"——那是编造。
    pub fn reasons(&self, quality_label: &str) -> Vec<String> {
        let mut items: Vec<(f64, String)> = Vec::with_capacity(3);

        if self.relevance > 0.0 {
            let detail = if self.from_substring {
                "按子串匹配命中"
            } else {
                "关键词命中"
            };
            items.push((self.relevance, detail.to_string()));
        }
        if self.quality > 0.0 {
            items.push((self.quality, quality_label.to_string()));
        }
        if self.recency > 0.0 {
            items.push((self.recency, "近期有更新".to_string()));
        }

        // 按贡献降序：用户最该先看到"它为什么排在这"的主因
        items.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        items.into_iter().map(|(_, s)| s).take(3).collect()
    }
}

/// 项目质量分：健康度为主，代码规模为辅。
///
/// 规模只做小幅加成（上限 0.15）：大项目不等于好项目，
/// 但太小的项目（几十行的脚本）确实复用价值有限。
pub fn project_quality(p: &Project) -> f64 {
    let health = (f64::from(p.health_score) / 100.0).clamp(0.0, 1.0);
    let scale = size_bonus(p.stats.loc);
    (health * 0.85 + scale).clamp(0.0, 1.0)
}

/// 资产质量分：直接采用复用评分（已由 spolia-asset 确定性计算）。
pub fn asset_quality(a: &Asset) -> f64 {
    a.reuse_score.clamp(0.0, 1.0)
}

/// 能力质量分：关联项目数越多越可信。
///
/// 用 `min(count, 5)/5` 饱和：5 个以上项目都在用，再多也不应继续加分，
/// 否则单个巨型项目会让某能力的分数压倒一切。
pub fn capability_quality(c: &Capability) -> f64 {
    (f64::from(c.project_count.min(5)) / 5.0).clamp(0.0, 1.0)
}

/// 洞察质量分：直接采用置信度。
///
/// 🔴 置信度是洞察引擎**确定性算出**的（证据数、跨项目重合度等因子加权），
/// 这里不再叠加任何主观加成——否则同一条洞察在"质量分"和
/// 详情页显示的"置信度"会对不上，用户无法解释为什么它排在这里。
pub fn insight_quality(i: &Insight) -> f64 {
    i.confidence.clamp(0.0, 1.0)
}

/// 机会质量分：由星级与覆盖度共同决定。
///
/// 星级（1-5）是"值不值得做"，覆盖度是"做起来有多容易"——
/// 二者都要：一个 5 星但覆盖度 10% 的机会（几乎全要从零写）
/// 不该排在 4 星、覆盖度 80%（大部分能直接复用）的机会前面。
///
/// 权重 0.7/0.3 偏向价值判断：用户搜"有什么可以做的项目"时，
/// 首先关心的是值不值得，其次才是省力程度。
pub fn opportunity_quality(o: &Opportunity) -> f64 {
    // 星级归一到 0..=1：1 星 → 0.0，5 星 → 1.0
    let stars = (f64::from(o.rating.clamp(1, 5)) - 1.0) / 4.0;
    (stars * 0.7 + o.coverage.clamp(0.0, 1.0) * 0.3).clamp(0.0, 1.0)
}

/// 代码行数 → 规模加成（0..=0.15）。
fn size_bonus(loc: usize) -> f64 {
    // 对数刻度：1k 行与 10k 行的差距，不该比 100 行与 1k 行大十倍
    let scaled = (loc as f64).log10().max(0.0) / 4.0; // 10^4 = 1 万行封顶
    (scaled * 0.15).clamp(0.0, 0.15)
}

/// 由"距今天数"计算新鲜度分数。`None`（无时间信息）视为最旧。
///
/// 无时间信息时给 0 而非 0.5：宁可让它排在后面，
/// 也不要用一个编造的中位数把"确实活跃的项目"挤下去。
pub fn recency_score(days_since: Option<i64>) -> f64 {
    let Some(d) = days_since else {
        return 0.0;
    };
    if d <= 0 {
        return 1.0;
    }
    if d >= RECENCY_WINDOW_DAYS {
        return 0.0;
    }
    1.0 - (d as f64 / RECENCY_WINDOW_DAYS as f64)
}

/// 实体在**显式排序维度**下的可比较属性。
///
/// # 🔴 为什么用这个结构体而不是一串 `Option<&Entity>` 参数
/// 旧签名是 `explicit_sort_key(sort, project, asset, recency)`，
/// 把具体实体类型写死在参数表里。这有两个后果：
/// 1. 每新增一类可检索实体（洞察、机会……）就要加一个参数，
///    签名越来越长，调用点要传一堆 `None`。
/// 2. 排序逻辑与实体类型耦合，无法单测"给定这些属性该怎么排"，
///    只能构造真实实体再调。
///
/// 改成传"排序需要的三个数字"之后，排序函数只关心数值语义，
/// 新增实体类型**完全不需要改动它**——只需在构造 `SortFacts` 时
/// 说明该实体在这三个维度上分别是什么。
pub struct SortFacts {
    /// 复用价值分。
    ///
    /// 资产 = `reuse_score`；机会 = `rating/5`；项目与洞察没有这个维度 → `None`。
    pub reuse_score: Option<f64>,
    /// 置信度分。
    ///
    /// 资产/洞察 = `confidence`；项目 = `health_score/100`；机会 = `rating/5`。
    /// 各实体都映射到自己的"可信程度"指标，使跨类型结果在同一维度下可比。
    pub confidence: Option<f64>,
    /// "最近更新"排序下使用的键值。
    ///
    /// 🔴 这里必须由调用方决定，不能一律传新鲜度：
    /// - **有真实时间线**的实体（项目 `updated_at`、洞察/机会 `created_at`）传真实新鲜度分。
    ///   洞察的 `created_at` 是"这条结论何时得出"，语义上正是它的时效。
    /// - **没有真实时间线**的实体传 [`NEUTRAL_RECENCY`]。典型是资产：
    ///   它的 `created_at` 是**抽取时间**（重新索引就变成今天），
    ///   不代表代码新旧。若当成新鲜度，刚索引过的十年老代码会排在昨天改的代码前面。
    ///
    /// 取中性分 0.5 而非 0，是为了让它与项目可比而不是无条件沉底。
    pub recency_key: f64,
}

impl SortFacts {
    /// 有真实时间线的实体（项目、洞察、机会）。
    pub fn with_timeline(reuse: Option<f64>, confidence: Option<f64>, recency: f64) -> Self {
        Self {
            reuse_score: reuse,
            confidence,
            recency_key: recency,
        }
    }

    /// 没有真实时间线的实体（资产、能力）。
    pub fn without_timeline(reuse: Option<f64>, confidence: Option<f64>) -> Self {
        Self {
            reuse_score: reuse,
            confidence,
            recency_key: NEUTRAL_RECENCY,
        }
    }
}

/// 用户显式排序时的主键值（越大越靠前）。
///
/// 时间维度由调用方预先算好（见 [`recency_score`]）：日期解析需要"当前时间"，
/// 那是引擎的职责，不该渗进这个纯函数模块。
///
/// 🔴 返回 `None` 表示"该实体没有这个维度的数据"，
/// 此时应退到相关性排序，而不是把它当成 0 分沉底——
/// 用户按"复用价值"排序时，一个项目（没有 reuse_score）不该凭空消失。
pub fn explicit_sort_key(sort: SortBy, facts: &SortFacts) -> Option<f64> {
    match sort {
        SortBy::Relevance => None,
        SortBy::ReuseScore => facts.reuse_score,
        SortBy::Confidence => facts.confidence,
        SortBy::RecentlyUpdated => Some(facts.recency_key),
    }
}

/// 稳定排序：主键相同时用次键，次键也相同则按 id。
///
/// 🔴 必须按 id 兜底：否则同一份数据两次搜索的顺序可能不同
/// （Rust 的 sort_by 是稳定的，但输入顺序本身来自 HashMap 时会变），
/// 用户会看到"刷新一次顺序就变"，无法建立对结果的信任。
pub fn sort_hits<T, K>(items: &mut [T], primary: impl Fn(&T) -> K, tiebreak: impl Fn(&T) -> String)
where
    K: PartialOrd,
{
    items.sort_by(|a, b| {
        primary(b)
            .partial_cmp(&primary(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| tiebreak(a).cmp(&tiebreak(b)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::{
        AssetType, CapabilityLayer, CodeStats, Evidence, EvidenceItem, EvidenceKind, Insight,
        InsightType, Opportunity, OpportunityStatus, ProjectStatus, UserFeedback,
    };

    fn project(health: u8, loc: usize, days: Option<i64>) -> Project {
        Project {
            id: "p1".into(),
            name: "demo".into(),
            path: "/tmp/p1".into(),
            description: "d".into(),
            language: "Python".into(),
            framework: "FastAPI".into(),
            created_at: None,
            updated_at: days.map(date_before),
            last_commit_at: days.map(date_before),
            status: ProjectStatus::Active,
            health_score: health,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats {
                files: 10,
                loc,
                symbols: 5,
                modules: 2,
                languages: vec![],
            },
            scan: spolia_domain::ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn date_before(days: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string()
    }

    fn asset(reuse: f64) -> Asset {
        Asset {
            id: "a1".into(),
            project_id: "p1".into(),
            asset_type: AssetType::Component,
            name: "Pipeline".into(),
            description: "d".into(),
            content: None,
            source_path: "src/a.py".into(),
            confidence: 0.9,
            reuse_score: reuse,
            generality: 0.7,
            stability: 0.6,
            tags: vec![],
            created_at: "2026-09-01".into(),
            evidence: Evidence::default(),
            user_feedback: None,
        }
    }

    fn cap(count: u32) -> Capability {
        let mut c =
            Capability::new("c1", "RAG", CapabilityLayer::Capability, Some("cap_domain_ai".into()), 0.9)
                .unwrap();
        c.project_count = count;
        c
    }

    /// 洞察构造：证据非空且置信度过门槛，满足入库校验（见 `Insight::validate`）。
    ///
    /// 🔴 只接受 `confidence` 一个可变参数：质量分的唯一输入就是它，
    /// 让调用方传别的字段反而会掩盖"质量分到底由什么决定"。
    fn insight(confidence: f64) -> Insight {
        Insight {
            id: "i1".into(),
            insight_type: InsightType::DuplicateCapability,
            title: "你在 2 个项目中重复实现了「Task Queue」".into(),
            description: "建议抽取为独立组件复用".into(),
            evidence: vec![EvidenceItem {
                kind: EvidenceKind::File,
                label: "src/task_queue.rs".into(),
                target: None,
            }],
            confidence,
            created_at: "2026-09-30".into(),
            user_feedback: None,
            tags: vec!["queue".into()],
            related_project_ids: vec!["p1".into()],
            related_asset_ids: vec![],
        }
    }

    /// 机会构造：只暴露影响质量分的两个维度（星级与覆盖度）。
    fn opportunity(rating: u8, coverage: f64) -> Opportunity {
        Opportunity {
            id: "o1".into(),
            title: "Web 领域的组合机会".into(),
            description: "把历史资产拼成新项目".into(),
            source_project_ids: vec!["p1".into()],
            source_asset_ids: vec![],
            required_capabilities: vec!["任务队列".into()],
            missing_capabilities: vec!["统一配置".into()],
            coverage,
            rating,
            why: "2 个项目存在能力重合".into(),
            evidence: vec!["src/queue.rs".into()],
            status: OpportunityStatus::New,
            created_at: "2026-09-30".into(),
        }
    }

    // ── 权重 ─────────────────────────────────────────────────────

    /// 权重和必须为 1，否则综合分不在 0..=1，前端进度条会溢出。
    #[test]
    fn weights_sum_to_one() {
        let sum = W_RELEVANCE + W_QUALITY + W_RECENCY;
        assert!((sum - 1.0).abs() < 1e-9, "权重和 = {sum}");
    }

    #[test]
    fn score_is_clamped_to_unit_interval() {
        let input = RankInput {
            relevance: 1.0,
            quality: 1.0,
            recency: 1.0,
            from_substring: false,
        };
        let s = input.score();
        assert!((s - 1.0).abs() < 1e-9);
        // 越界输入也不得产出越界分数
        let bad = RankInput {
            relevance: 5.0,
            quality: 5.0,
            recency: 5.0,
            from_substring: false,
        };
        assert!(bad.score() <= 1.0, "越界输入被钳住: {}", bad.score());
    }

    #[test]
    fn all_zero_input_scores_zero() {
        let input = RankInput {
            relevance: 0.0,
            quality: 0.0,
            recency: 0.0,
            from_substring: false,
        };
        assert_eq!(input.score(), 0.0);
        assert!(input.reasons("质量较高").is_empty(), "分数全 0 时不该编造理由");
    }

    /// 相关性必须主导：命中度高但质量低的结果，应排在命中度低但质量高的之前。
    #[test]
    fn relevance_dominates_quality() {
        let strong_match = RankInput {
            relevance: 1.0,
            quality: 0.2,
            recency: 0.0,
            from_substring: false,
        };
        let weak_match = RankInput {
            relevance: 0.2,
            quality: 1.0,
            recency: 0.0,
            from_substring: false,
        };
        assert!(strong_match.score() > weak_match.score());
    }

    // ── 理由（产品硬要求）────────────────────────────────────────

    #[test]
    fn reasons_only_cite_contributing_factors() {
        let input = RankInput {
            relevance: 0.8,
            quality: 0.0,
            recency: 0.0,
            from_substring: false,
        };
        let reasons = input.reasons("复用价值高");
        assert_eq!(reasons, vec!["关键词命中"], "不得提及质量与新鲜度: {reasons:?}");
    }

    #[test]
    fn reasons_are_ordered_by_contribution() {
        let input = RankInput {
            relevance: 0.2,
            quality: 0.9,
            recency: 0.5,
            from_substring: false,
        };
        let reasons = input.reasons("复用价值高");
        assert_eq!(reasons[0], "复用价值高", "贡献最大的应排第一: {reasons:?}");
        assert!(reasons.len() <= 3, "理由不超过 3 条");
    }

    /// LIKE 回退时理由措辞必须不同：子串匹配的可靠性低于分词匹配，
    /// 如实告知用户才不会让他误以为找到了精确结果。
    #[test]
    fn substring_fallback_says_so_in_reasons() {
        let input = RankInput {
            relevance: 0.5,
            quality: 0.0,
            recency: 0.0,
            from_substring: true,
        };
        assert_eq!(input.reasons("q"), vec!["按子串匹配命中"]);
    }

    // ── 质量分 ───────────────────────────────────────────────────

    #[test]
    fn project_quality_tracks_health() {
        let low = project_quality(&project(20, 1000, None));
        let high = project_quality(&project(90, 1000, None));
        assert!(high > low);
        assert!(high <= 1.0);
    }

    #[test]
    fn project_quality_gives_small_scale_bonus() {
        let tiny = project_quality(&project(80, 50, None));
        let big = project_quality(&project(80, 100_000, None));
        assert!(big > tiny, "规模应有小幅加成");
        assert!(big - tiny <= 0.15 + 1e-9, "加成不得超过 0.15: {}", big - tiny);
    }

    #[test]
    fn capability_quality_saturates_at_five_projects() {
        assert_eq!(capability_quality(&cap(0)), 0.0);
        assert_eq!(capability_quality(&cap(1)), 0.2);
        assert_eq!(capability_quality(&cap(5)), 1.0);
        assert_eq!(capability_quality(&cap(50)), 1.0, "5 个以上应饱和");
    }

    #[test]
    fn asset_quality_uses_reuse_score() {
        assert_eq!(asset_quality(&asset(0.75)), 0.75);
        assert_eq!(asset_quality(&asset(2.0)), 1.0, "越界值被钳住");
    }

    // ── 新鲜度 ───────────────────────────────────────────────────

    #[test]
    fn recency_decays_linearly_then_zero() {
        assert_eq!(recency_score(Some(0)), 1.0);
        assert!((recency_score(Some(15)) - 0.5).abs() < 1e-9);
        assert_eq!(recency_score(Some(30)), 0.0);
        assert_eq!(recency_score(Some(365)), 0.0);
    }

    /// 无时间信息不得给中位数分——那会把确实活跃的项目挤下去。
    #[test]
    fn unknown_recency_scores_zero() {
        assert_eq!(recency_score(None), 0.0);
    }

    #[test]
    fn negative_days_treated_as_now() {
        assert_eq!(recency_score(Some(-5)), 1.0);
    }

    #[test]
    fn recency_window_is_a_month() {
        assert_eq!(RECENCY_WINDOW_DAYS, 30);
    }

    // ── 显式排序 ─────────────────────────────────────────────────
    //
    // 🔴 这些测试只传数值、不构造实体——这正是把签名从
    // `Option<&Project>, Option<&Asset>` 改成 `SortFacts` 的收益：
    // 排序规则本身可以被直接穷举，新增实体类型不必回来改这些测试。

    #[test]
    fn relevance_sort_has_no_explicit_key() {
        let facts = SortFacts::with_timeline(Some(0.9), Some(0.8), 0.7);
        assert_eq!(explicit_sort_key(SortBy::Relevance, &facts), None);
    }

    #[test]
    fn reuse_score_sort_reads_the_reuse_dimension() {
        let a = asset(0.8);
        let facts = SortFacts::without_timeline(Some(a.reuse_score), Some(a.confidence));
        assert_eq!(
            explicit_sort_key(SortBy::ReuseScore, &facts),
            Some(0.8)
        );
        // 没有该维度的实体（项目/洞察）：None 让调用方退回相关性排序，
        // 而不是当 0 分沉底——用户按"复用价值"排序时项目不该凭空消失
        let none = SortFacts::with_timeline(None, Some(0.8), 0.5);
        assert_eq!(explicit_sort_key(SortBy::ReuseScore, &none), None);
    }

    #[test]
    fn confidence_sort_reads_the_confidence_dimension() {
        let a = asset(0.8);
        let asset_facts = SortFacts::without_timeline(Some(a.reuse_score), Some(a.confidence));
        assert_eq!(
            explicit_sort_key(SortBy::Confidence, &asset_facts),
            Some(0.9)
        );
        // 项目映射 health_score/100 作为"可信度"，与资产可比
        let p = project(80, 100, None);
        let project_facts =
            SortFacts::with_timeline(None, Some(f64::from(p.health_score) / 100.0), 0.0);
        assert_eq!(
            explicit_sort_key(SortBy::Confidence, &project_facts),
            Some(0.8)
        );
    }

    /// 有真实时间线的实体（项目/洞察/机会）按真实新鲜度排序。
    #[test]
    fn recently_updated_uses_real_recency_for_timelined_entities() {
        let p = project(80, 100, Some(0)); // 今天更新 → recency=1.0
        let recency = recency_score(p.days_since_update(chrono::Utc::now()));
        assert!(recency > 0.9);
        let facts = SortFacts::with_timeline(None, None, recency);
        assert_eq!(
            explicit_sort_key(SortBy::RecentlyUpdated, &facts),
            Some(recency)
        );
    }

    /// 资产无独立时间线 → 中性分，与项目可比而非一律沉底。
    ///
    /// 🔴 `without_timeline` 必须填 NEUTRAL_RECENCY：
    /// 资产的 created_at 是**抽取时间**，重新索引就变成今天。
    /// 若把它当新鲜度，刚索引过的十年老代码会排在昨天改的代码前面。
    #[test]
    fn recently_updated_gives_untimelined_entities_neutral_score() {
        let facts = SortFacts::without_timeline(Some(0.8), Some(0.9));
        assert_eq!(
            explicit_sort_key(SortBy::RecentlyUpdated, &facts),
            Some(NEUTRAL_RECENCY)
        );
        assert_eq!(NEUTRAL_RECENCY, 0.5);
    }

    // ── 洞察与机会的质量分 ──────────────────────────────────────

    /// 洞察质量分必须**就是**置信度，不得叠加主观加成：
    /// 否则同一条洞察在排序里的"质量"与详情页显示的"置信度"对不上，
    /// 用户无法解释它为什么排在这里。
    #[test]
    fn insight_quality_is_exactly_confidence() {
        let mut i = insight(0.73);
        assert!((insight_quality(&i) - 0.73).abs() < f64::EPSILON);
        // 脏数据必须钳位，不能让越界值把综合分顶出 0..=1
        i.confidence = 1.9;
        assert_eq!(insight_quality(&i), 1.0);
        i.confidence = -0.5;
        assert_eq!(insight_quality(&i), 0.0);
    }

    /// 机会质量分：星级为主（0.7）、覆盖度为辅（0.3）。
    ///
    /// 🔴 必须两者都算：5 星但覆盖度 10%（几乎全要从零写）
    /// 不该排在 4 星、覆盖度 80%（大部分能直接复用）的机会前面。
    #[test]
    fn opportunity_quality_combines_rating_and_coverage() {
        let high_both = opportunity(5, 1.0);
        assert!((opportunity_quality(&high_both) - 1.0).abs() < f64::EPSILON);

        let low_both = opportunity(1, 0.0);
        assert_eq!(opportunity_quality(&low_both), 0.0);

        // 5 星 + 覆盖度 0 → 0.7（星级满分但一点都复用不上）
        let stars_only = opportunity(5, 0.0);
        assert!((opportunity_quality(&stars_only) - 0.7).abs() < 1e-9);
        // 1 星 + 覆盖度 1.0 → 0.3（全能复用但不值得做）
        let coverage_only = opportunity(1, 1.0);
        assert!((opportunity_quality(&coverage_only) - 0.3).abs() < 1e-9);
        // 星级权重必须高于覆盖度
        assert!(opportunity_quality(&stars_only) > opportunity_quality(&coverage_only));
    }

    /// 4 星满覆盖 vs 5 星低覆盖：前者应排前面（这是权重的实际效果）。
    #[test]
    fn opportunity_quality_prefers_reusable_over_purely_ambitious() {
        let reusable = opportunity(4, 0.9);
        let ambitious = opportunity(5, 0.1);
        assert!(
            opportunity_quality(&reusable) > opportunity_quality(&ambitious),
            "{} vs {}",
            opportunity_quality(&reusable),
            opportunity_quality(&ambitious)
        );
    }

    #[test]
    fn opportunity_quality_clamps_dirty_values() {
        let mut o = opportunity(9, 1.7); // 星级与覆盖度都越界
        assert_eq!(opportunity_quality(&o), 1.0);
        o = opportunity(0, -0.3);
        assert_eq!(opportunity_quality(&o), 0.0);
    }

    // ── 稳定排序 ─────────────────────────────────────────────────

    #[test]
    fn sort_is_deterministic_with_id_tiebreak() {
        let mut items = vec![
            (0.5_f64, "b".to_string()),
            (0.5, "a".to_string()),
            (0.9, "c".to_string()),
        ];
        sort_hits(&mut items, |x| x.0, |x| x.1.clone());
        let ids: Vec<&str> = items.iter().map(|(_, id)| id.as_str()).collect();
        assert_eq!(ids, vec!["c", "a", "b"], "同分必须按 id 稳定排序");
    }

    #[test]
    fn sort_descends_by_primary_key() {
        // 显式标注元组类型：用 "x".into() 会让推断陷入循环
        // （sort_hits 的泛型参数依赖元素类型，而元素类型又依赖 into 的目标）
        let mut items: Vec<(f64, String)> = vec![
            (0.1, "x".to_string()),
            (0.7, "y".to_string()),
            (0.3, "z".to_string()),
        ];
        sort_hits(&mut items, |i| i.0, |i| i.1.clone());
        let keys: Vec<f64> = items.iter().map(|i| i.0).collect();
        assert_eq!(keys, vec![0.7, 0.3, 0.1]);
    }

    #[test]
    fn sort_handles_empty_and_single() {
        let mut empty: Vec<(f64, String)> = vec![];
        sort_hits(&mut empty, |i| i.0, |i| i.1.clone());
        assert!(empty.is_empty());

        let mut one = vec![(0.5_f64, "only".to_string())];
        sort_hits(&mut one, |i| i.0, |i| i.1.clone());
        assert_eq!(one.len(), 1);
    }

    /// NaN 不得让排序 panic 或产生未定义顺序。
    #[test]
    fn sort_survives_nan() {
        let mut items = vec![
            (f64::NAN, "a".to_string()),
            (0.5, "b".to_string()),
        ];
        sort_hits(&mut items, |i| i.0, |i| i.1.clone());
        assert_eq!(items.len(), 2, "不得 panic");
    }

    #[test]
    fn unused_domain_imports_are_used() {
        // 保证测试模块导入的类型都被用到（AssetType/UserFeedback 在 fixture 里）
        assert_eq!(AssetType::Component.label_zh(), "组件");
        assert!(UserFeedback::Useful.as_str() == "useful");
    }
}
