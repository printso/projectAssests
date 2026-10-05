//! 洞察（Insight）：系统**主动发现**的结论（《产品设计书》§7-⑤）。
//!
//! 产品纪律：每条 Insight 必须带 Evidence 与 Confidence，
//! 且用户可对每条标记 有用/无用（反馈回流调整评分权重）。

use serde::{Deserialize, Serialize};

use crate::feedback::{Feedbackable, UserFeedback};

/// 洞察类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightType {
    /// 重复实现的能力："你在 4 个项目中重复实现了任务队列"
    DuplicateCapability,
    /// 高复用组件
    ReusableComponent,
    /// 可复用的历史经验
    ReusableExperience,
    /// 被遗忘的资产
    ForgottenAsset,
    /// 持续的技术方向
    TechDirection,
    /// 组合机会提示（与 Opportunity 表区分：这是线索，Opportunity 是成型卡片）
    OpportunityHint,
}

impl InsightType {
    /// 全部洞察类型，**按 UI 展示顺序**排列。
    ///
    /// 🔴 筛选 chips 必须遍历它而非"当前有数据的类型"：
    /// 只列非零项的话，用户勾掉某个类型后该 chip 就消失了，再也点不回来。
    pub fn all() -> &'static [InsightType] {
        &[
            Self::DuplicateCapability,
            Self::ReusableComponent,
            Self::ReusableExperience,
            Self::ForgottenAsset,
            Self::TechDirection,
            Self::OpportunityHint,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DuplicateCapability => "duplicate_capability",
            Self::ReusableComponent => "reusable_component",
            Self::ReusableExperience => "reusable_experience",
            Self::ForgottenAsset => "forgotten_asset",
            Self::TechDirection => "tech_direction",
            Self::OpportunityHint => "opportunity_hint",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "duplicate_capability" => Self::DuplicateCapability,
            "reusable_component" => Self::ReusableComponent,
            "reusable_experience" => Self::ReusableExperience,
            "forgotten_asset" => Self::ForgottenAsset,
            "tech_direction" => Self::TechDirection,
            "opportunity_hint" => Self::OpportunityHint,
            _ => return None,
        })
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::DuplicateCapability => "重复能力",
            Self::ReusableComponent => "可复用组件",
            Self::ReusableExperience => "历史经验",
            Self::ForgottenAsset => "遗忘资产",
            Self::TechDirection => "技术方向",
            Self::OpportunityHint => "组合机会",
        }
    }
}

/// 价值分档（UI 徽章）。由 confidence 确定性映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightBadge {
    High,
    Potent,
    Info,
}

impl InsightBadge {
    pub fn from_confidence(c: f64) -> Self {
        if c >= 0.85 {
            Self::High
        } else if c >= 0.7 {
            Self::Potent
        } else {
            Self::Info
        }
    }

    /// 稳定的机器可读值。
    ///
    /// 🔴 前端要按档位选配色/图标时必须用它，不能用 `label_zh()`：
    /// 中文文案一改（"建议查看" → "可参考"），所有靠字符串匹配的分支会静默失效。
    /// 这也是 `AssetType` / `RelationType` / `UserFeedback` 等枚举都提供 `as_str` 的同一理由。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Potent => "potent",
            Self::Info => "info",
        }
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::High => "高价值",
            Self::Potent => "高潜力",
            Self::Info => "建议查看",
        }
    }
}

/// 一条洞察。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Insight {
    pub id: String,
    #[serde(rename = "type")]
    pub insight_type: InsightType,
    pub title: String,
    pub description: String,
    /// 证据：**真实**文件路径 / 项目名 / commit。
    /// 产品红线：`evidence` 为空的洞察不允许入库（见 `Insight::validate`）。
    pub evidence: Vec<EvidenceItem>,
    /// 0.0-1.0
    pub confidence: f64,
    pub created_at: String,
    pub user_feedback: Option<UserFeedback>,
    /// 标签（用于 chips 过滤）
    pub tags: Vec<String>,
    /// 关联实体（点"查看"可跳转）
    pub related_project_ids: Vec<String>,
    pub related_asset_ids: Vec<String>,
}

/// 单条证据。结构化而非裸字符串，便于前端渲染"项目 / 文件 / commit"三类图标。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceItem {
    /// 证据种类
    pub kind: EvidenceKind,
    /// 展示文本（文件名 / 项目名 / commit 短哈希）
    pub label: String,
    /// 可跳转目标：项目 id 或 "project_id:相对路径"
    pub target: Option<String>,
}

/// 证据种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    File,
    Project,
    Commit,
    Symbol,
}

impl EvidenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Project => "project",
            Self::Commit => "commit",
            Self::Symbol => "symbol",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "file" => Self::File,
            "project" => Self::Project,
            "commit" => Self::Commit,
            "symbol" => Self::Symbol,
            _ => return None,
        })
    }
}

/// 洞察校验错误。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum InsightError {
    /// 产品红线：无证据的结论一律不展示。
    #[error("洞察「{0}」缺少证据，按产品纪律不予入库")]
    MissingEvidence(String),
    #[error("洞察「{0}」的置信度 {1} 低于展示门槛 {2}")]
    LowConfidence(String, f64, f64),
    #[error("洞察标题不能为空")]
    EmptyTitle,
}

/// 洞察展示门槛：低于此置信度不进图谱、不展示（《技术设计书》§25 风险应对）。
pub const CONFIDENCE_THRESHOLD: f64 = 0.55;

impl Insight {
    /// 校验产品红线。所有引擎在入库前**必须**调用。
    pub fn validate(&self) -> Result<(), InsightError> {
        if self.title.trim().is_empty() {
            return Err(InsightError::EmptyTitle);
        }
        if self.evidence.is_empty() {
            return Err(InsightError::MissingEvidence(self.title.clone()));
        }
        if self.confidence < CONFIDENCE_THRESHOLD {
            return Err(InsightError::LowConfidence(
                self.title.clone(),
                self.confidence,
                CONFIDENCE_THRESHOLD,
            ));
        }
        Ok(())
    }

    pub fn badge(&self) -> InsightBadge {
        InsightBadge::from_confidence(self.confidence)
    }
}

impl Feedbackable for Insight {
    fn feedback(&self) -> Option<UserFeedback> {
        self.user_feedback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::Feedbackable;

    fn sample(evidence: Vec<EvidenceItem>, conf: f64) -> Insight {
        Insight {
            id: "i1".into(),
            insight_type: InsightType::DuplicateCapability,
            title: "重复实现的能力".into(),
            description: "你在 3 个项目中实现了相似的图片处理 Pipeline".into(),
            evidence,
            confidence: conf,
            created_at: "2026-09-26".into(),
            user_feedback: None,
            tags: vec!["图片处理".into()],
            related_project_ids: vec![],
            related_asset_ids: vec![],
        }
    }

    fn file_evidence() -> Vec<EvidenceItem> {
        vec![EvidenceItem {
            kind: EvidenceKind::File,
            label: "image-tool/pipeline.py".into(),
            target: Some("p_image_tool:image-tool/pipeline.py".into()),
        }]
    }

    /// 产品红线：无证据的洞察必须被拒绝。
    #[test]
    fn insight_without_evidence_is_rejected() {
        let i = sample(vec![], 0.9);
        assert_eq!(
            i.validate().unwrap_err(),
            InsightError::MissingEvidence("重复实现的能力".into())
        );
    }

    #[test]
    fn low_confidence_insight_is_rejected() {
        let i = sample(file_evidence(), 0.3);
        assert!(matches!(i.validate(), Err(InsightError::LowConfidence(..))));
    }

    #[test]
    fn empty_title_is_rejected() {
        let mut i = sample(file_evidence(), 0.9);
        i.title = "   ".into();
        assert_eq!(i.validate().unwrap_err(), InsightError::EmptyTitle);
    }

    #[test]
    fn valid_insight_passes() {
        let i = sample(file_evidence(), 0.91);
        assert!(i.validate().is_ok());
        assert_eq!(i.badge(), InsightBadge::High);
        assert!(i.is_unread());
    }

    #[test]
    fn badge_thresholds() {
        assert_eq!(InsightBadge::from_confidence(0.9), InsightBadge::High);
        assert_eq!(InsightBadge::from_confidence(0.75), InsightBadge::Potent);
        assert_eq!(InsightBadge::from_confidence(0.6), InsightBadge::Info);
    }

    #[test]
    fn feedback_marks_as_read() {
        let mut i = sample(file_evidence(), 0.9);
        i.user_feedback = Some(UserFeedback::Useful);
        assert!(!i.is_unread());
    }

    #[test]
    fn types_and_kinds_roundtrip() {
        for t in [
            InsightType::DuplicateCapability,
            InsightType::ReusableComponent,
            InsightType::ReusableExperience,
            InsightType::ForgottenAsset,
            InsightType::TechDirection,
            InsightType::OpportunityHint,
        ] {
            assert_eq!(InsightType::parse(t.as_str()), Some(t));
            assert!(!t.label_zh().is_empty());
        }
        for k in [
            EvidenceKind::File,
            EvidenceKind::Project,
            EvidenceKind::Commit,
            EvidenceKind::Symbol,
        ] {
            assert_eq!(EvidenceKind::parse(k.as_str()), Some(k));
        }
        for f in [UserFeedback::Useful, UserFeedback::Useless, UserFeedback::Ignored] {
            assert_eq!(UserFeedback::parse(f.as_str()), Some(f));
        }
    }
}
