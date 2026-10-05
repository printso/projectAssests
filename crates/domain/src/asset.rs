//! 八类资产模型（《产品设计书》§2）——产品的数据地基。
//!
//! 设计要点（《技术设计书》§11）：**单表 + type 判别**，而非八张表。
//! MVP 阶段各类资产字段高度重合，单表让 Pipeline 迭代快得多；
//! 某类资产字段显著分化时再拆表。

use serde::{Deserialize, Serialize};

use crate::feedback::{Feedbackable, UserFeedback};

/// 资产类型。与数据库 `assets.type` 列的取值一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    /// 模块 / 函数 / 脚本
    Code,
    /// 可复用组件
    Component,
    /// 对外接口
    Api,
    /// Prompt 模板
    Prompt,
    /// 知识（Concept / Pattern / Solution / Constraint / Rule）
    Knowledge,
    /// 决策（为什么这么做、放弃了什么）
    Decision,
    /// 经验（尝试过 A/B/C，最终 D 最好）
    Experience,
    /// 未做完的想法
    Idea,
    /// 结果资产（阶段二）
    Outcome,
    /// AI 产物
    AiArtifact,
}

impl AssetType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Component => "component",
            Self::Api => "api",
            Self::Prompt => "prompt",
            Self::Knowledge => "knowledge",
            Self::Decision => "decision",
            Self::Experience => "experience",
            Self::Idea => "idea",
            Self::Outcome => "outcome",
            Self::AiArtifact => "ai_artifact",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "code" => Self::Code,
            "component" => Self::Component,
            "api" => Self::Api,
            "prompt" => Self::Prompt,
            "knowledge" => Self::Knowledge,
            "decision" => Self::Decision,
            "experience" => Self::Experience,
            "idea" => Self::Idea,
            "outcome" => Self::Outcome,
            "ai_artifact" => Self::AiArtifact,
            _ => return None,
        })
    }

    /// 全部类型，供筛选 chips 与枚举遍历使用（顺序即 UI 展示顺序）。
    pub fn all() -> &'static [AssetType] {
        &[
            Self::Code,
            Self::Component,
            Self::Api,
            Self::Prompt,
            Self::Knowledge,
            Self::Decision,
            Self::Experience,
            Self::Idea,
            Self::Outcome,
            Self::AiArtifact,
        ]
    }

    /// 中文标签（UI 展示）。
    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Code => "代码",
            Self::Component => "组件",
            Self::Api => "API",
            Self::Prompt => "Prompt",
            Self::Knowledge => "知识",
            Self::Decision => "决策",
            Self::Experience => "经验",
            Self::Idea => "创意",
            Self::Outcome => "结果",
            Self::AiArtifact => "AI 产物",
        }
    }
}

/// 一条资产。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub project_id: String,
    #[serde(rename = "type")]
    pub asset_type: AssetType,
    pub name: String,
    pub description: String,
    /// 符号签名或摘录片段（可选，避免存全量代码）
    pub content: Option<String>,
    /// **相对项目根**的路径。存相对路径而非绝对路径：数据库可跨机器复制，
    /// 且避免把用户目录结构写进派生数据（隐私考量）。
    pub source_path: String,
    /// 0.0-1.0
    pub confidence: f64,
    /// 0.0-1.0
    pub reuse_score: f64,
    /// 通用性：与业务逻辑的解耦程度
    pub generality: f64,
    /// 稳定性：被修改的频率（越稳定越高）
    pub stability: f64,
    pub tags: Vec<String>,
    pub created_at: String,
    /// 证据链：为什么认为它有复用价值
    pub evidence: Evidence,
    /// 用户反馈。这是北极星指标 **Rediscovered Value** 的数据来源，
    /// 因此属于资产的持久状态（而非临时 UI 态）。
    ///
    /// 🔴 重新抽取资产时**不得**覆盖此字段：用户的历史判断比新一轮 AI 打分更有价值。
    pub user_feedback: Option<UserFeedback>,
}

impl Feedbackable for Asset {
    fn feedback(&self) -> Option<UserFeedback> {
        self.user_feedback
    }
}

/// 证据链（《产品设计书》§8）——横切所有页面。
///
/// 产品纪律 #1：**每个 AI 结论都要有 Evidence，无证据的结论一律不展示。**
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Evidence {
    /// 来源文件（相对路径）
    pub files: Vec<String>,
    /// 相关 commit（短哈希）
    pub commits: Vec<String>,
    /// 被哪些函数/模块调用
    pub used_by: Vec<String>,
    /// 判定为可复用的理由条目（人类可读）
    pub reasoning: Vec<String>,
}

impl Evidence {
    /// 是否足以支撑结论展示。空证据的结论**必须被过滤掉**，
    /// 这是产品红线，因此在领域层提供判定入口而非散落在各引擎里。
    pub fn is_sufficient(&self) -> bool {
        !self.files.is_empty() || !self.commits.is_empty()
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

/// 复用价值分档，由 reuse_score 确定性映射（阈值集中于此，避免散落各页面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseTier {
    High,
    Medium,
    Low,
}

impl ReuseTier {
    pub fn from_score(score: f64) -> Self {
        if score >= 0.85 {
            Self::High
        } else if score >= 0.70 {
            Self::Medium
        } else {
            Self::Low
        }
    }

    /// 稳定的机器可读值（与 `serde` 的 `rename_all = "snake_case"` 一致）。
    ///
    /// 🔴 API 响应里的 `tier` 字段必须用它，不能用 `label_zh()`：
    /// 中文标签会随文案调整而变，前端拿它做条件分支就会在改文案后静默失效。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => return None,
        })
    }

    /// 该档位的 reuse_score 下限。
    ///
    /// 阈值集中在此处而非散落在 service 的筛选逻辑里：
    /// `from_score` 与"按档位筛选"必须用同一套数字，
    /// 各写一遍的话改了分档却忘了改筛选，用户勾"高价值"会看到中档资产。
    pub fn min_score(&self) -> f64 {
        match self {
            Self::High => 0.85,
            Self::Medium => 0.70,
            Self::Low => 0.0,
        }
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::High => "高价值",
            Self::Medium => "重要",
            Self::Low => "一般",
        }
    }
}

impl std::fmt::Display for ReuseTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_type_roundtrips() {
        for t in AssetType::all() {
            assert_eq!(AssetType::parse(t.as_str()), Some(*t), "{:?}", t);
        }
        assert_eq!(AssetType::parse("nope"), None);
    }

    #[test]
    fn asset_type_serializes_snake_case() {
        let json = serde_json::to_string(&AssetType::AiArtifact).unwrap();
        assert_eq!(json, "\"ai_artifact\"");
    }

    #[test]
    fn every_type_has_chinese_label() {
        for t in AssetType::all() {
            assert!(!t.label_zh().is_empty());
        }
    }

    #[test]
    fn reuse_tier_thresholds() {
        assert_eq!(ReuseTier::from_score(0.91), ReuseTier::High);
        assert_eq!(ReuseTier::from_score(0.85), ReuseTier::High);
        assert_eq!(ReuseTier::from_score(0.75), ReuseTier::Medium);
        assert_eq!(ReuseTier::from_score(0.60), ReuseTier::Low);
    }

    #[test]
    fn empty_evidence_is_insufficient() {
        assert!(!Evidence::default().is_sufficient());
        let e = Evidence {
            files: vec!["src/a.py".into()],
            ..Default::default()
        };
        assert!(e.is_sufficient());
        assert_eq!(e.file_count(), 1);
    }
}
