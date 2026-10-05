//! 用户反馈（跨资产与洞察共用）。
//!
//! 为什么单独成模块：反馈是北极星指标 **Rediscovered Value** 的数据来源
//! （《产品介绍总纲》§5：只看"用户重新发现了多少个有价值的资产"）。
//! 资产与洞察都要记录它，放在任一实体模块里都会造成循环依赖。
//!
//! 🔴 关键纪律：**重新分析不得覆盖用户反馈**。
//! AI 的打分每轮都会变，但用户的判断是事实。存储层在 UPSERT 时
//! 刻意把 `user_feedback` 排除在 UPDATE 列表之外（见 `spolia-storage::AssetRepo`）。

use serde::{Deserialize, Serialize};

/// 用户对一条结论的评价。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserFeedback {
    /// 有用：计入 Rediscovered Value
    Useful,
    /// 无用：反馈回流，降低同类结论的评分权重
    Useless,
    /// 已忽略：不再展示，但不参与调权（用户可能只是暂时不关心）
    Ignored,
}

impl UserFeedback {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Useful => "useful",
            Self::Useless => "useless",
            Self::Ignored => "ignored",
        }
    }

    /// 解析数据库值。未知值返回 `None`（视为无反馈），而非报错——
    /// 旧库中可能存有已废弃的取值。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "useful" => Self::Useful,
            "useless" => Self::Useless,
            "ignored" => Self::Ignored,
            _ => return None,
        })
    }

    /// 是否计入"重新发现价值"指标。
    pub fn counts_as_rediscovered(&self) -> bool {
        matches!(self, Self::Useful)
    }

    /// 是否会触发评分权重调整。
    pub fn affects_scoring(&self) -> bool {
        matches!(self, Self::Useful | Self::Useless)
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Useful => "有用",
            Self::Useless => "无用",
            Self::Ignored => "已忽略",
        }
    }

    pub fn all() -> &'static [UserFeedback] {
        &[Self::Useful, Self::Useless, Self::Ignored]
    }
}

/// 带反馈的实体 trait：让洞察与资产共用"是否已读/是否计入指标"的判断逻辑，
/// 避免同一段判定在两处各写一遍（将来加第三类可反馈实体时也不会漏）。
pub trait Feedbackable {
    fn feedback(&self) -> Option<UserFeedback>;

    /// 用户尚未处理过（首页"新发现"只展示未处理的）。
    fn is_unread(&self) -> bool {
        self.feedback().is_none()
    }

    /// 是否计入 Rediscovered Value。
    fn is_rediscovered(&self) -> bool {
        self.feedback().is_some_and(|f| f.counts_as_rediscovered())
    }

    /// 是否应参与评分调权。
    fn affects_scoring(&self) -> bool {
        self.feedback().is_some_and(|f| f.affects_scoring())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_all_variants() {
        for f in UserFeedback::all() {
            assert_eq!(UserFeedback::parse(f.as_str()), Some(*f));
            assert!(!f.label_zh().is_empty());
        }
    }

    #[test]
    fn unknown_value_parses_to_none() {
        assert_eq!(UserFeedback::parse("deprecated-value"), None);
        assert_eq!(UserFeedback::parse(""), None);
    }

    #[test]
    fn serializes_snake_case() {
        assert_eq!(serde_json::to_string(&UserFeedback::Useless).unwrap(), "\"useless\"");
    }

    #[test]
    fn only_useful_counts_as_rediscovered() {
        assert!(UserFeedback::Useful.counts_as_rediscovered());
        assert!(!UserFeedback::Useless.counts_as_rediscovered());
        assert!(!UserFeedback::Ignored.counts_as_rediscovered());
    }

    /// Ignored 不参与调权：用户"暂时不关心"不等于"结论是错的"。
    #[test]
    fn ignored_does_not_affect_scoring() {
        assert!(UserFeedback::Useful.affects_scoring());
        assert!(UserFeedback::Useless.affects_scoring());
        assert!(!UserFeedback::Ignored.affects_scoring());
    }

    struct Sample(Option<UserFeedback>);
    impl Feedbackable for Sample {
        fn feedback(&self) -> Option<UserFeedback> {
            self.0
        }
    }

    #[test]
    fn feedbackable_defaults() {
        assert!(Sample(None).is_unread());
        assert!(!Sample(None).is_rediscovered());
        assert!(!Sample(None).affects_scoring());

        assert!(!Sample(Some(UserFeedback::Useful)).is_unread());
        assert!(Sample(Some(UserFeedback::Useful)).is_rediscovered());
        assert!(!Sample(Some(UserFeedback::Ignored)).is_rediscovered());
        assert!(!Sample(Some(UserFeedback::Ignored)).affects_scoring());
    }
}
