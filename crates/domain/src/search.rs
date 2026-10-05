//! 检索的领域类型（《技术设计书》§12）：**混合检索，而非纯向量**。
//!
//! ```text
//! Query → FTS5 关键词召回 + 向量语义召回 + 结构化过滤 → 合并重排 → 结果 + 排序理由 + Evidence
//! ```

use serde::{Deserialize, Serialize};

use crate::asset::AssetType;
use crate::project::ProjectStatus;

/// 检索范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SearchScope {
    /// 全部（项目 + 资产 + 能力 + 洞察与机会）
    #[default]
    All,
    Projects,
    Assets,
    Capabilities,
    /// 洞察与机会。
    ///
    /// 🔴 一个范围覆盖两类实体，而非拆成 `Insights` + `Opportunities`：
    /// 二者在 UI 上是同一个页面（洞察页），service 层也在同一个模块，
    /// 用户的心智模型是"系统给我的结论"这一件事。
    /// 拆成两个 scope 会迫使前端渲染两个 tab、两套 chips，
    /// 而用户其实无法区分"洞察"和"机会"该看哪个。
    ///
    /// 若将来需要细分，再加 `Opportunities` 变体即可，
    /// 届时 `Insights` 的语义收窄不会破坏已有序列化值。
    Insights,
    Knowledge,
}

impl SearchScope {
    /// 全部检索范围，**按 UI 展示顺序**排列。
    ///
    /// 🔴 与 `HitKind::all()` 同理：遍历型代码（roundtrip 测试、scope chips）
    /// 一律用它，不要各处再抄一份字面量数组——那必然在新增变体时漏掉。
    pub fn all() -> &'static [SearchScope] {
        &[
            Self::All,
            Self::Projects,
            Self::Assets,
            Self::Capabilities,
            Self::Insights,
            Self::Knowledge,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Projects => "projects",
            Self::Assets => "assets",
            Self::Capabilities => "capabilities",
            Self::Insights => "insights",
            Self::Knowledge => "knowledge",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "all" => Self::All,
            "projects" => Self::Projects,
            "assets" => Self::Assets,
            "capabilities" => Self::Capabilities,
            "insights" => Self::Insights,
            "knowledge" => Self::Knowledge,
            _ => return None,
        })
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Projects => "项目",
            Self::Assets => "资产",
            Self::Capabilities => "能力",
            Self::Insights => "洞察",
            Self::Knowledge => "知识",
        }
    }
}

/// 结构化过滤条件（《技术设计书》§12 的第三路召回）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilter {
    pub asset_type: Option<AssetType>,
    pub project_status: Option<ProjectStatus>,
    pub language: Option<String>,
    pub project_id: Option<String>,
    /// 仅返回 reuse_score ≥ 此值的资产
    pub min_reuse_score: Option<f64>,
}

impl SearchFilter {
    pub fn is_empty(&self) -> bool {
        self.asset_type.is_none()
            && self.project_status.is_none()
            && self.language.is_none()
            && self.project_id.is_none()
            && self.min_reuse_score.is_none()
    }
}

/// 排序方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    /// 综合相关性（默认）
    #[default]
    Relevance,
    /// 复用价值
    ReuseScore,
    /// 最近更新
    RecentlyUpdated,
    /// 置信度
    Confidence,
}

impl SortBy {
    /// 全部排序方式，**按 UI 下拉顺序**排列（同 `SearchScope::all` 的理由）。
    pub fn all() -> &'static [SortBy] {
        &[
            Self::Relevance,
            Self::ReuseScore,
            Self::RecentlyUpdated,
            Self::Confidence,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::ReuseScore => "reuse_score",
            Self::RecentlyUpdated => "recently_updated",
            Self::Confidence => "confidence",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "relevance" => Self::Relevance,
            "reuse_score" => Self::ReuseScore,
            "recently_updated" => Self::RecentlyUpdated,
            "confidence" => Self::Confidence,
            _ => return None,
        })
    }
}

/// 检索请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// 原始查询串。检索引擎负责转义，**调用方不需要也不应该**预处理。
    pub q: String,
    pub scope: SearchScope,
    pub filter: SearchFilter,
    pub sort: SortBy,
    pub limit: u32,
    pub offset: u32,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            q: String::new(),
            scope: SearchScope::default(),
            filter: SearchFilter::default(),
            sort: SortBy::default(),
            limit: 20,
            offset: 0,
        }
    }
}

impl SearchQuery {
    /// 归一化后的查询词（trim + 折叠空白）。空串表示"仅按过滤条件浏览"。
    pub fn normalized_q(&self) -> String {
        self.q.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    pub fn is_browse_only(&self) -> bool {
        self.normalized_q().is_empty()
    }

    /// limit 上限保护：防止前端传入过大值拖垮本地库。
    pub fn effective_limit(&self) -> u32 {
        self.limit.clamp(1, 200)
    }
}

/// 命中来源（用于"排序理由"，产品要求搜索结果**有排序理由**）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchSource {
    /// FTS5 关键词命中
    Keyword,
    /// 向量语义命中
    Semantic,
    /// 结构化过滤命中（浏览模式）
    Structured,
    /// 名称精确/前缀命中（加权最高）
    NameMatch,
}

impl MatchSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Semantic => "semantic",
            Self::Structured => "structured",
            Self::NameMatch => "name_match",
        }
    }

    /// 人类可读的排序理由片段。
    pub fn reason_zh(&self) -> &'static str {
        match self {
            Self::Keyword => "关键词命中",
            Self::Semantic => "语义相关",
            Self::Structured => "符合筛选条件",
            Self::NameMatch => "名称匹配",
        }
    }
}

/// 单条搜索结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// 命中的实体类型
    pub kind: HitKind,
    pub id: String,
    /// 主标题（项目名 / 资产名 / 能力名）
    pub title: String,
    /// 副标题（路径 / 类型 / 所属项目）
    pub subtitle: String,
    /// 摘要片段（含高亮上下文）
    pub snippet: String,
    /// 0.0-1.0 综合分
    pub score: f64,
    /// 命中来源（可多个：关键词 + 语义）
    pub sources: Vec<MatchSource>,
    /// **排序理由**：为什么排在这个位置（产品硬要求，不是玄学分数）
    pub reasons: Vec<String>,
    /// 前端跳转所需信息
    pub link: HitLink,
}

/// 命中实体类型。
/// 命中实体类型。
///
/// # 🔴 为什么洞察与机会必须在这里
/// 它们曾长期缺席，直接导致对话式分析师答不出库里明明有的结论：
/// 用户问"我有哪些重复实现的代码？"，库里存着一条标题为
/// "你在 2 个项目中重复实现了「Task Queue」"的洞察（带 10 条证据），
/// 但因为 `HitKind` 没有对应变体、检索层无从产出它，分析师只能回答
/// "没有找到相关记录"。
///
/// 洞察与机会恰恰是**结论性**内容——用户提问时最想拿到的就是结论，
/// 而不是让他自己在 21 个资产里翻找。把它们排除在检索之外，
/// 等于把产品最有价值的部分藏了起来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Project,
    Asset,
    Capability,
    /// 洞察（重复能力 / 可复用组件 / 遗忘资产 / 技术方向 …）
    Insight,
    /// 组合机会（把历史资产拼成新项目的建议）
    Opportunity,
    Knowledge,
    Experience,
    Decision,
    Idea,
}

impl HitKind {
    /// 全部实体类型，**按 UI 展示顺序**排列。
    ///
    /// 🔴 顺序在这里定义一次，所有需要"遍历全部类型"的地方都用它：
    /// 类型 tab、chips、分组计数。各处自己排一遍的话，
    /// 同一个页面上 tab 顺序和角标顺序可能不一致。
    ///
    /// 刻意**不用字母序**：用户搜东西时最先关心"是哪个项目"，
    /// 其次才是"哪段代码"。字母序会把 asset 排在 project 前面。
    ///
    /// 洞察与机会排在能力之后、四个"内容型资产"之前：
    /// 它们是系统给出的结论，优先级高于零散的知识条目。
    pub fn all() -> &'static [HitKind] {
        &[
            Self::Project,
            Self::Asset,
            Self::Capability,
            Self::Insight,
            Self::Opportunity,
            Self::Knowledge,
            Self::Experience,
            Self::Decision,
            Self::Idea,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Asset => "asset",
            Self::Capability => "capability",
            Self::Insight => "insight",
            Self::Opportunity => "opportunity",
            Self::Knowledge => "knowledge",
            Self::Experience => "experience",
            Self::Decision => "decision",
            Self::Idea => "idea",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "project" => Self::Project,
            "asset" => Self::Asset,
            "capability" => Self::Capability,
            "insight" => Self::Insight,
            "opportunity" => Self::Opportunity,
            "knowledge" => Self::Knowledge,
            "experience" => Self::Experience,
            "decision" => Self::Decision,
            "idea" => Self::Idea,
            _ => return None,
        })
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Project => "项目",
            Self::Asset => "资产",
            Self::Capability => "能力",
            Self::Insight => "洞察",
            Self::Opportunity => "机会",
            Self::Knowledge => "知识",
            Self::Experience => "经验",
            Self::Decision => "决策",
            Self::Idea => "创意",
        }
    }

    /// 检索命中类型 → 引用类型（对话式分析师用它给每条引用打标签）。
    ///
    /// # 🔴 这个映射的唯一真相源在这里，不在任何 crate 的私有 `citation_kind` 函数里
    /// 早先 `spolia-ai` 与 `spolia-service` **各自**抄了一份 `citation_kind`，
    /// 且都用 `_ => CitationKind::File` 兜底。domain 加了 `Insight`/`Opportunity` 后，
    /// 两处都把结论性实体静默吞成「文件」引用：标签显示 File、链接却跳洞察页，
    /// 自相矛盾，而通配符让编译器**一声不吭**。
    ///
    /// 收敛到 domain 的好处：
    /// 1. 只有一份映射，两个传输/推理路径不可能漂移；
    /// 2. 穷举 match，domain 每加一个变体都会强制这里编译失败，
    ///    逼改动者显式决定它归哪类引用，而不是默默落到 File。
    ///
    /// `HitKind` 与 `CitationKind` 都是 domain 类型，这个关系本就属于 domain 的知识。
    pub fn citation_kind(&self) -> crate::analyst::CitationKind {
        use crate::analyst::CitationKind;
        match self {
            Self::Project => CitationKind::Project,
            Self::Asset => CitationKind::Asset,
            Self::Capability => CitationKind::Capability,
            // 洞察与机会是结论性实体，有各自详情页，不落到文件
            Self::Insight => CitationKind::Insight,
            Self::Opportunity => CitationKind::Opportunity,
            // 知识/经验/决策/创意是资产的子类型，最终都落在具体文件里
            Self::Knowledge | Self::Experience | Self::Decision | Self::Idea => CitationKind::File,
        }
    }
}

/// 跳转信息：前端据此路由，不需要自己拼 URL。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitLink {
    /// 目标页面 key（overview/projects/project/assets/graph/insights/analyst/…）
    pub page: String,
    /// 页面内定位参数（项目 id / 资产 id）
    pub param: Option<String>,
}

/// 检索响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    pub total: usize,
    /// 实际使用的查询串（归一化后），前端回显
    pub query: String,
    /// 是否发生了 LIKE 回退（中文 2 字查询时 trigram 无法命中，需回退）。
    /// 暴露给前端以便在结果区提示"短查询按子串匹配"，避免用户困惑于结果偏多。
    pub used_substring_fallback: bool,
    /// 耗时（毫秒），用于验证"搜索响应 ≤ 1 秒"的产品指标
    pub took_ms: u64,
}

impl SearchResult {
    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_normalizes_whitespace() {
        let q = SearchQuery { q: "  视频   生成 ".into(), ..Default::default() };
        assert_eq!(q.normalized_q(), "视频 生成");
        assert!(!q.is_browse_only());
    }

    #[test]
    fn blank_query_is_browse_only() {
        let q = SearchQuery { q: "   ".into(), ..Default::default() };
        assert!(q.is_browse_only());
    }

    #[test]
    fn limit_is_clamped() {
        assert_eq!(SearchQuery { limit: 0, ..Default::default() }.effective_limit(), 1);
        assert_eq!(SearchQuery { limit: 99999, ..Default::default() }.effective_limit(), 200);
        assert_eq!(SearchQuery { limit: 20, ..Default::default() }.effective_limit(), 20);
    }

    #[test]
    fn filter_emptiness() {
        assert!(SearchFilter::default().is_empty());
        let f = SearchFilter { language: Some("Python".into()), ..Default::default() };
        assert!(!f.is_empty());
    }

    /// 🔴 用 `all()` 遍历，不再硬编码数组。
    ///
    /// 早先这里手抄变体清单，加了 `Insight`/`Opportunity`/`Insights` 后没跟上——
    /// 于是这三个变体的 `as_str`/`parse`/`label_zh` 往返**从未被测到**。
    /// 改用 `all()` 后，domain 新增任何变体都自动纳入往返校验。
    #[test]
    fn scopes_and_kinds_roundtrip() {
        for s in SearchScope::all() {
            assert_eq!(SearchScope::parse(s.as_str()), Some(*s), "scope 往返失败: {s:?}");
            assert!(!s.label_zh().is_empty(), "scope 缺中文标签: {s:?}");
        }
        for k in HitKind::all() {
            assert!(!k.label_zh().is_empty(), "kind 缺中文标签: {k:?}");
            assert!(!k.as_str().is_empty(), "kind 缺 as_str: {k:?}");
            assert_eq!(HitKind::parse(k.as_str()), Some(*k), "kind 往返失败: {k:?}");
        }
        for s in SortBy::all() {
            assert_eq!(SortBy::parse(s.as_str()), Some(*s), "sort 往返失败: {s:?}");
        }
    }

    /// 🔴 锁定 `citation_kind` 的**全变体映射**，防止再被通配符吞掉。
    ///
    /// 这个映射曾是 `spolia-ai` 与 `spolia-service` 各抄一份的重复真相源，
    /// 两处都用 `_ => File` 兜底，导致新增的 `Insight`/`Opportunity`
    /// 被静默归成「文件」引用（标签 File、链接却跳洞察页）。
    /// 收敛到 domain 后，这条测试逐一钉死每个变体的归属：
    /// 将来谁再改映射、或加了变体忘了归类，这里立刻失败。
    #[test]
    fn citation_kind_covers_every_variant() {
        use crate::analyst::CitationKind;
        for k in HitKind::all() {
            // 每个变体都必须有明确映射（穷举 match 已保证，这里再验非空）
            let ck = k.citation_kind();
            assert!(!ck.label_zh().is_empty(), "引用类型缺标签: {k:?}");
            assert_eq!(CitationKind::parse(ck.as_str()), Some(ck), "引用类型往返失败: {ck:?}");
        }
        // 结论性实体必须各自成类，绝不落到 File
        assert_eq!(HitKind::Insight.citation_kind(), CitationKind::Insight);
        assert_eq!(
            HitKind::Opportunity.citation_kind(),
            CitationKind::Opportunity
        );
        // 结构型实体一一对应
        assert_eq!(HitKind::Project.citation_kind(), CitationKind::Project);
        assert_eq!(HitKind::Asset.citation_kind(), CitationKind::Asset);
        assert_eq!(
            HitKind::Capability.citation_kind(),
            CitationKind::Capability
        );
        // 资产子类型才落 File
        for k in [
            HitKind::Knowledge,
            HitKind::Experience,
            HitKind::Decision,
            HitKind::Idea,
        ] {
            assert_eq!(k.citation_kind(), CitationKind::File, "{k:?} 应归为文件引用");
        }
    }

    #[test]
    fn match_source_has_reason() {
        for m in [
            MatchSource::Keyword,
            MatchSource::Semantic,
            MatchSource::Structured,
            MatchSource::NameMatch,
        ] {
            assert!(!m.reason_zh().is_empty());
            assert!(!m.as_str().is_empty());
        }
    }

    #[test]
    fn empty_result_reports_correctly() {
        assert!(SearchResult::default().is_empty());
        assert!(!SearchResult::default().used_substring_fallback);
    }
}
