//! 关系（《产品设计书》§3）：**智能的真正来源**。
//!
//! 没有关系，只是数据库；有了关系，才开始产生智能。
//! 采用泛化边表（source/target + type），而非为每种关系建表——
//! 关系类型会随阶段演进不断增加，泛化表让新增类型零迁移成本。

use serde::{Deserialize, Serialize};

/// 关系两端对象的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Project,
    Asset,
    Capability,
    Knowledge,
    Experience,
    Idea,
    Technology,
    Problem,
}

impl EntityKind {
    /// 全部实体类型，**按图谱图例顺序**排列（项目/资产在前，抽象概念在后）。
    pub fn all() -> &'static [EntityKind] {
        &[
            Self::Project,
            Self::Asset,
            Self::Capability,
            Self::Knowledge,
            Self::Experience,
            Self::Idea,
            Self::Technology,
            Self::Problem,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Asset => "asset",
            Self::Capability => "capability",
            Self::Knowledge => "knowledge",
            Self::Experience => "experience",
            Self::Idea => "idea",
            Self::Technology => "technology",
            Self::Problem => "problem",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "project" => Self::Project,
            "asset" => Self::Asset,
            "capability" => Self::Capability,
            "knowledge" => Self::Knowledge,
            "experience" => Self::Experience,
            "idea" => Self::Idea,
            "technology" => Self::Technology,
            "problem" => Self::Problem,
            _ => return None,
        })
    }

    /// 图谱节点配色键（官方色板见 `GRAPH_COLORS`）。
    pub fn color_key(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Asset => "code",
            Self::Capability => "capability",
            Self::Knowledge => "knowledge",
            Self::Experience => "experience",
            Self::Idea => "idea",
            Self::Technology => "technology",
            Self::Problem => "problem",
        }
    }

    /// 中文标签（图谱节点 tooltip / 图例）。
    ///
    /// 与 `HitKind::label_zh` 是两套东西，刻意不合并：
    /// `HitKind` 描述"搜索命中的是什么"（没有 Technology/Problem），
    /// `EntityKind` 描述"图谱里的节点是什么"。
    /// 两者变体集合不同，强行共用一个枚举会让某一侧多出无意义的分支。
    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Project => "项目",
            Self::Asset => "资产",
            Self::Capability => "能力",
            Self::Knowledge => "知识",
            Self::Experience => "经验",
            Self::Idea => "创意",
            Self::Technology => "技术",
            Self::Problem => "问题",
        }
    }
}

/// 关系类型。《产品设计书》§3 定义的 MVP 关系全集。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    // Project 出发
    Contains,
    Implements,
    Uses,
    Produces,
    Generates,
    ResultedIn,
    // Code / Asset 出发
    SimilarTo,
    ReusedBy,
    // Capability 之间
    DependsOn,
    CombinesWith,
    // Knowledge 出发
    DerivedFrom,
    Supports,
    RelatedTo,
    // Experience 出发
    HappenedIn,
    Solves,
    Informs,
    // Idea 出发
    CanCombineWith,
}

impl RelationType {
    /// 全部关系类型，**按边的语义分组顺序**排列。
    ///
    /// 图谱的边类型筛选器要遍历它。刻意不用 `list_all()` 的结果去重推导：
    /// 库里当前有哪些边是**数据状态**，不是类型全集——
    /// 用它生成筛选器的话，某种关系一旦被全部删掉，选项就从 UI 上消失了。
    pub fn all() -> &'static [RelationType] {
        &[
            Self::Contains,
            Self::Implements,
            Self::Uses,
            Self::Produces,
            Self::Generates,
            Self::ResultedIn,
            Self::SimilarTo,
            Self::ReusedBy,
            Self::DependsOn,
            Self::CombinesWith,
            Self::DerivedFrom,
            Self::Supports,
            Self::RelatedTo,
            Self::HappenedIn,
            Self::Solves,
            Self::Informs,
            Self::CanCombineWith,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Implements => "implements",
            Self::Uses => "uses",
            Self::Produces => "produces",
            Self::Generates => "generates",
            Self::ResultedIn => "resulted_in",
            Self::SimilarTo => "similar_to",
            Self::ReusedBy => "reused_by",
            Self::DependsOn => "depends_on",
            Self::CombinesWith => "combines_with",
            Self::DerivedFrom => "derived_from",
            Self::Supports => "supports",
            Self::RelatedTo => "related_to",
            Self::HappenedIn => "happened_in",
            Self::Solves => "solves",
            Self::Informs => "informs",
            Self::CanCombineWith => "can_combine_with",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "contains" => Self::Contains,
            "implements" => Self::Implements,
            "uses" => Self::Uses,
            "produces" => Self::Produces,
            "generates" => Self::Generates,
            "resulted_in" => Self::ResultedIn,
            "similar_to" => Self::SimilarTo,
            "reused_by" => Self::ReusedBy,
            "depends_on" => Self::DependsOn,
            "combines_with" => Self::CombinesWith,
            "derived_from" => Self::DerivedFrom,
            "supports" => Self::Supports,
            "related_to" => Self::RelatedTo,
            "happened_in" => Self::HappenedIn,
            "solves" => Self::Solves,
            "informs" => Self::Informs,
            "can_combine_with" => Self::CanCombineWith,
            _ => return None,
        })
    }

    /// 中文说明（图谱边 tooltip / Evidence 展示）。
    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Contains => "包含",
            Self::Implements => "实现了",
            Self::Uses => "使用了",
            Self::Produces => "产出了",
            Self::Generates => "产生了",
            Self::ResultedIn => "最终结果为",
            Self::SimilarTo => "相似于",
            Self::ReusedBy => "被复用于",
            Self::DependsOn => "依赖",
            Self::CombinesWith => "可组合",
            Self::DerivedFrom => "源自",
            Self::Supports => "支撑",
            Self::RelatedTo => "相关于",
            Self::HappenedIn => "发生于",
            Self::Solves => "解决了",
            Self::Informs => "影响了",
            Self::CanCombineWith => "可与之组合",
        }
    }
}

/// 一条关系边。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub id: String,
    pub source_id: String,
    pub source_type: EntityKind,
    pub relation_type: RelationType,
    pub target_id: String,
    pub target_type: EntityKind,
    /// 0.0-1.0
    pub confidence: f64,
    /// 支撑该关系的真实文件/commit；空表示纯结构推断（如 contains）
    pub evidence: Vec<String>,
}

impl Relation {
    pub fn new(
        id: impl Into<String>,
        source_id: impl Into<String>,
        source_type: EntityKind,
        relation_type: RelationType,
        target_id: impl Into<String>,
        target_type: EntityKind,
        confidence: f64,
    ) -> Self {
        Self {
            id: id.into(),
            source_id: source_id.into(),
            source_type,
            relation_type,
            target_id: target_id.into(),
            target_type,
            confidence: confidence.clamp(0.0, 1.0),
            evidence: Vec::new(),
        }
    }

    pub fn with_evidence(mut self, files: impl IntoIterator<Item = String>) -> Self {
        self.evidence = files.into_iter().collect();
        self
    }

    /// 是否涉及给定实体（任一端）。图谱"点击节点看关联"用。
    pub fn touches(&self, id: &str) -> bool {
        self.source_id == id || self.target_id == id
    }

    /// 取另一端 id。
    pub fn other_end(&self, id: &str) -> Option<&str> {
        if self.source_id == id {
            Some(&self.target_id)
        } else if self.target_id == id {
            Some(&self.source_id)
        } else {
            None
        }
    }
}

/// 图谱节点分类色（官方色板，与设计图图例一一对应）。
///
/// 集中在此处作为**单一数据源**：首页图例、图谱页、项目相关图共用，
/// 避免出现原型期"健康环硬编码色"那类散落裸值的问题。
pub mod colors {
    /// (键, 十六进制色值) —— 顺序即图例展示顺序。
    pub const GRAPH_COLORS: &[(&str, &str)] = &[
        ("capability", "#a855f7"), // 能力 - 紫
        ("project", "#22c55e"),    // 项目 - 绿
        ("code", "#3b82f6"),       // 代码 - 蓝
        ("knowledge", "#eab308"),  // 知识 - 黄
        ("experience", "#06b6d4"), // 经验 - 青
        ("idea", "#ec4899"),       // 创意 - 粉
        ("technology", "#f97316"), // 技术 - 橙
        ("problem", "#64748b"),    // 问题 - 灰
        ("relation", "#64748b"),   // 关系(边) - 灰
    ];

    /// 按键取色；未知键回退到中性灰（不 panic，图谱不因单个未知类型而白屏）。
    pub fn for_key(key: &str) -> &'static str {
        GRAPH_COLORS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .unwrap_or("#64748b")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_type_roundtrips() {
        for t in [
            RelationType::Contains,
            RelationType::Implements,
            RelationType::SimilarTo,
            RelationType::CanCombineWith,
            RelationType::ResultedIn,
        ] {
            assert_eq!(RelationType::parse(t.as_str()), Some(t));
        }
        assert_eq!(RelationType::parse("nope"), None);
    }

    #[test]
    fn entity_kind_roundtrips() {
        for k in [
            EntityKind::Project,
            EntityKind::Asset,
            EntityKind::Capability,
            EntityKind::Problem,
        ] {
            assert_eq!(EntityKind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn every_relation_type_has_label() {
        for t in [
            RelationType::Contains,
            RelationType::Implements,
            RelationType::Uses,
            RelationType::Produces,
            RelationType::Generates,
            RelationType::ResultedIn,
            RelationType::SimilarTo,
            RelationType::ReusedBy,
            RelationType::DependsOn,
            RelationType::CombinesWith,
            RelationType::DerivedFrom,
            RelationType::Supports,
            RelationType::RelatedTo,
            RelationType::HappenedIn,
            RelationType::Solves,
            RelationType::Informs,
            RelationType::CanCombineWith,
        ] {
            assert!(!t.label_zh().is_empty(), "{:?}", t);
        }
    }

    #[test]
    fn confidence_is_clamped() {
        let r = Relation::new("r", "a", EntityKind::Project, RelationType::Implements, "b", EntityKind::Capability, 5.0);
        assert_eq!(r.confidence, 1.0);
        let r2 = Relation::new("r", "a", EntityKind::Project, RelationType::Implements, "b", EntityKind::Capability, -1.0);
        assert_eq!(r2.confidence, 0.0);
    }

    #[test]
    fn touches_and_other_end() {
        let r = Relation::new("r1", "p1", EntityKind::Project, RelationType::SimilarTo, "p2", EntityKind::Project, 0.8);
        assert!(r.touches("p1"));
        assert!(r.touches("p2"));
        assert!(!r.touches("p3"));
        assert_eq!(r.other_end("p1"), Some("p2"));
        assert_eq!(r.other_end("p2"), Some("p1"));
        assert_eq!(r.other_end("p9"), None);
    }

    #[test]
    fn with_evidence_attaches_files() {
        let r = Relation::new("r1", "p1", EntityKind::Project, RelationType::SimilarTo, "p2", EntityKind::Project, 0.8)
            .with_evidence(["a/src/x.py".to_string(), "b/src/y.py".to_string()]);
        assert_eq!(r.evidence.len(), 2);
    }

    #[test]
    fn color_lookup_falls_back_gracefully() {
        assert_eq!(colors::for_key("capability"), "#a855f7");
        assert_eq!(colors::for_key("project"), "#22c55e");
        assert_eq!(colors::for_key("unknown-kind"), "#64748b");
    }

    #[test]
    fn entity_color_key_is_known() {
        for k in [
            EntityKind::Project,
            EntityKind::Asset,
            EntityKind::Capability,
            EntityKind::Knowledge,
            EntityKind::Experience,
            EntityKind::Idea,
        ] {
            let c = colors::for_key(k.color_key());
            assert_ne!(c, "#64748b", "{:?} 应命中专属色而非回退灰", k);
        }
    }
}
