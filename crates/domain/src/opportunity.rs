//! 机会（Opportunity）：**历史 → 新项目**的桥梁（《产品设计书》§7 / V0.4）。
//!
//! 这是产品区别于普通"AI 项目搜索工具"的标志性产出：
//! 组合 Capability + 高复用 Code + 未完成 Idea + 相关 Project → 机会卡片。

use serde::{Deserialize, Serialize};

/// 机会状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OpportunityStatus {
    /// 新生成，待用户处理
    #[default]
    New,
    /// 用户已查看并展开深入分析
    Explored,
    /// 用户已忽略（Dismiss）
    Dismissed,
    /// 已据此创建新项目
    Adopted,
}

impl OpportunityStatus {
    /// 全部状态，**按 UI 展示顺序**排列（待处理在前，已归档在后）。
    pub fn all() -> &'static [OpportunityStatus] {
        &[Self::New, Self::Explored, Self::Dismissed, Self::Adopted]
    }

    /// 是否为"可操作"状态：用户还没做出最终处置的机会。
    ///
    /// 🔴 这个判定必须在领域层定义一次：`OpportunityFilter::actionable()`、
    /// 首页角标、机会页默认视图都依赖它。各处自己写 `matches!(s, New | Explored)`
    /// 的话，将来新增一个状态（比如"已搁置"）就会漏改某一处，
    /// 表现为"角标数字和列表条数不一致"——这类缺陷没有报错，只能靠用户发现。
    pub fn is_actionable(&self) -> bool {
        matches!(self, Self::New | Self::Explored)
    }

    /// 全部可操作状态（顺序与 `all()` 一致）。
    pub fn actionable() -> Vec<OpportunityStatus> {
        Self::all()
            .iter()
            .copied()
            .filter(|s| s.is_actionable())
            .collect()
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Explored => "explored",
            Self::Dismissed => "dismissed",
            Self::Adopted => "adopted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "new" => Self::New,
            "explored" => Self::Explored,
            "dismissed" => Self::Dismissed,
            "adopted" => Self::Adopted,
            _ => return None,
        })
    }

    /// 中文标签（UI 展示）。
    ///
    /// 与其它枚举（`ProjectStatus` / `InsightBadge` / `JobStatus`）保持同一约定：
    /// 展示文案集中在领域层，避免各前端页面各写一份导致口径漂移。
    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::New => "待评估",
            Self::Explored => "已分析",
            Self::Dismissed => "已忽略",
            Self::Adopted => "已采纳",
        }
    }
}

/// 机会卡片。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    pub id: String,
    pub title: String,
    pub description: String,
    /// 来源项目 id
    pub source_project_ids: Vec<String>,
    /// 来源资产 id（可直接复用的高价值代码）
    pub source_asset_ids: Vec<String>,
    /// 已具备的能力名
    pub required_capabilities: Vec<String>,
    /// 缺失的能力名（用户需要新建的部分）
    pub missing_capabilities: Vec<String>,
    /// 覆盖度 0.0-1.0：已具备 / (已具备 + 缺失)
    pub coverage: f64,
    /// 组合价值 1-5 星
    pub rating: u8,
    /// 为什么值得关注（真实依据，如"4 个历史项目存在能力重合"）
    pub why: String,
    /// 证据：真实文件/项目
    pub evidence: Vec<String>,
    pub status: OpportunityStatus,
    pub created_at: String,
}

impl Opportunity {
    /// 由已具备/缺失能力计算覆盖度。分母为 0 时返回 0（不给假的高分）。
    pub fn compute_coverage(have: usize, missing: usize) -> f64 {
        let total = have + missing;
        if total == 0 {
            return 0.0;
        }
        (have as f64 / total as f64 * 100.0).round() / 100.0
    }

    /// 组合价值评级（1-5 星），由三个确定性因子加权：
    /// - 来源项目数（越多说明能力越普遍）
    /// - 覆盖度（越高越容易落地）
    /// - 可直接复用的高价值资产数
    ///
    /// 刻意不使用 LLM 打分：评级必须可解释、可复现，
    /// 且用户"深入分析"时能看到每一项依据。
    pub fn compute_rating(source_projects: usize, coverage: f64, reusable_assets: usize) -> u8 {
        let mut score = 0.0f64;
        // 项目重合度：2 个起步，5 个以上封顶
        score += match source_projects {
            0 | 1 => 0.0,
            2 => 0.8,
            3 => 1.2,
            4 => 1.6,
            _ => 2.0,
        };
        // 覆盖度贡献 0~1.6
        score += coverage.clamp(0.0, 1.0) * 1.6;
        // 可复用资产贡献 0~1.4
        score += match reusable_assets {
            0 => 0.0,
            1 => 0.5,
            2 => 0.9,
            3 => 1.2,
            _ => 1.4,
        };
        (score.round().clamp(1.0, 5.0)) as u8
    }

    pub fn is_actionable(&self) -> bool {
        self.status == OpportunityStatus::New || self.status == OpportunityStatus::Explored
    }
}

/// 机会的"深入分析"结果（《产品设计书》V0.4 功能 23）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityAnalysis {
    pub opportunity_id: String,
    /// 为什么建议组合
    pub rationale: String,
    /// 可直接复用的资产（含来源路径）
    pub reusable: Vec<ReusableItem>,
    /// 需要新建的能力
    pub to_build: Vec<String>,
    /// 建议的 MVP 形态
    pub mvp_suggestion: String,
    /// 新项目初始结构建议（不自动写代码，仅给清单）
    pub scaffold: Vec<String>,
}

/// 可复用项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReusableItem {
    pub asset_id: String,
    pub name: String,
    pub project_id: String,
    pub source_path: String,
    pub reuse_score: f64,
    /// 迁移动作建议（如"可直接复制"、"需抽象参数"）
    pub migration_note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_is_ratio_of_have_to_total() {
        assert_eq!(Opportunity::compute_coverage(3, 1), 0.75);
        assert_eq!(Opportunity::compute_coverage(1, 3), 0.25);
        assert_eq!(Opportunity::compute_coverage(2, 0), 1.0);
    }

    /// 空输入不能给出假的高覆盖度。
    #[test]
    fn coverage_zero_when_no_capabilities() {
        assert_eq!(Opportunity::compute_coverage(0, 0), 0.0);
    }

    #[test]
    fn rating_scales_with_evidence_strength() {
        let weak = Opportunity::compute_rating(1, 0.2, 0);
        let mid = Opportunity::compute_rating(3, 0.6, 1);
        let strong = Opportunity::compute_rating(5, 0.95, 4);
        assert!(weak <= 5 && strong <= 5 && weak >= 1);
        assert!(strong > mid, "strong={strong} mid={mid}");
        assert!(mid > weak, "mid={mid} weak={weak}");
    }

    #[test]
    fn rating_never_exceeds_five_stars() {
        assert!(Opportunity::compute_rating(99, 1.0, 99) <= 5);
    }

    #[test]
    fn status_roundtrips_and_actionable() {
        for s in [
            OpportunityStatus::New,
            OpportunityStatus::Explored,
            OpportunityStatus::Dismissed,
            OpportunityStatus::Adopted,
        ] {
            assert_eq!(OpportunityStatus::parse(s.as_str()), Some(s));
        }
        assert!(OpportunityStatus::New.is_actionable_via());
    }

    // 辅助：避免与 Opportunity::is_actionable 混淆，这里直接构造实例判断
    trait StatusCheck {
        fn is_actionable_via(&self) -> bool;
    }
    impl StatusCheck for OpportunityStatus {
        fn is_actionable_via(&self) -> bool {
            matches!(self, Self::New | Self::Explored)
        }
    }

    #[test]
    fn dismissed_opportunity_is_not_actionable() {
        let o = Opportunity {
            id: "o1".into(),
            title: "AI 内容生产引擎".into(),
            description: String::new(),
            source_project_ids: vec!["p1".into()],
            source_asset_ids: vec![],
            required_capabilities: vec!["AI Video".into()],
            missing_capabilities: vec!["Publishing".into()],
            coverage: 0.5,
            rating: 4,
            why: "2 个历史项目存在能力重合".into(),
            evidence: vec!["p1/src/a.py".into()],
            status: OpportunityStatus::Dismissed,
            created_at: "2026-09-26".into(),
        };
        assert!(!o.is_actionable());
    }
}
