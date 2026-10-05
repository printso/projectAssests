//! AI 分析师的领域类型（《技术设计书》§16「AI 分析师」）。
//!
//! # 两条产品红线，都落在这几个类型的字段上
//! 1. **无 LLM 时不得白屏**：`generated_by` 记录回答来源
//!    （模型名 / `deterministic`），前端据此显示"本次为离线检索式回答"，
//!    而不是假装是 AI 生成的。
//! 2. **回答必须可溯源**：`citations` 指向真实项目/资产，
//!    用户点一下就跳到出处——没有出处的"AI 结论"等于幻觉。

use serde::{Deserialize, Serialize};

use crate::search::HitLink;

/// AI 分析师的一次回答。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalystAnswer {
    /// 回答正文（Markdown，前端渲染）
    pub content: String,
    /// 生成方式。
    ///
    /// 🔴 决定前端如何措辞：
    /// - `Model` → "由 qwen3:8b 生成"
    /// - `Deterministic` → "离线检索式回答（未配置模型）"
    ///
    /// 绝不能把确定性回答包装成"AI 生成"，那是欺骗用户。
    pub generated_by: AnswerSource,
    /// 引用的真实实体（可点击跳转）
    pub citations: Vec<Citation>,
    /// 后续建议问题（引导用户深入，降低"不知道还能问什么"的门槛）
    pub followups: Vec<String>,
    /// 耗时（毫秒）
    pub took_ms: u64,
}

impl AnalystAnswer {
    /// 是否为离线确定性回答（未走任何 LLM）。
    pub fn is_deterministic(&self) -> bool {
        self.generated_by == AnswerSource::Deterministic
    }

    /// 是否有可展示的出处。
    ///
    /// 无出处的回答只在"纯闲聊/能力说明"类问题上允许；
    /// 涉及具体项目的回答必须有 citation，否则前端应标注"无法溯源"。
    pub fn has_citations(&self) -> bool {
        !self.citations.is_empty()
    }
}

/// 回答的生成来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerSource {
    /// 由 LLM 生成（`model` 字段给出模型标识）
    Model(String),
    /// 离线确定性回答：直接从数据库检索并套用固定模板，未调用任何模型
    Deterministic,
}

impl AnswerSource {
    /// 审计与展示用的标识。
    ///
    /// 🔴 不含 API Key、不含请求原文，只有 provider:model 或 "deterministic"。
    pub fn label(&self) -> String {
        match self {
            Self::Model(m) => m.clone(),
            Self::Deterministic => "deterministic".to_string(),
        }
    }

    pub fn is_model(&self) -> bool {
        matches!(self, Self::Model(_))
    }
}

/// 一条引用（回答里某个论断的出处）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Citation {
    /// 引用类型
    pub kind: CitationKind,
    /// 展示文本（项目名 / 资产名 / 文件相对路径）
    pub label: String,
    /// 点击跳转目标
    pub link: HitLink,
    /// 该引用支撑的具体论断（一句话），让用户知道"为什么提到它"
    pub supports: Option<String>,
}

/// 引用类型。
///
/// 🔴 前端据此选图标与措辞，所以必须与实际实体一一对应。
/// 曾经 `HitKind` 新增变体时，映射函数用 `_ => File` 兜底，
/// 于是新类型被静默标成"文件"——引用的图标说是文件，
/// 点进去却跳到洞察页，用户无法信任任何引用标注。
/// 因此映射函数刻意写成**穷举**（无通配符），加变体时编译器会强制处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationKind {
    Project,
    Asset,
    Capability,
    /// 洞察（系统给出的结论）
    Insight,
    /// 组合机会（把历史资产拼成新项目的建议）
    Opportunity,
    File,
}

impl CitationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Asset => "asset",
            Self::Capability => "capability",
            Self::Insight => "insight",
            Self::Opportunity => "opportunity",
            Self::File => "file",
        }
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Project => "项目",
            Self::Asset => "资产",
            Self::Capability => "能力",
            Self::Insight => "洞察",
            Self::Opportunity => "机会",
            Self::File => "文件",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "project" => Self::Project,
            "asset" => Self::Asset,
            "capability" => Self::Capability,
            "insight" => Self::Insight,
            "opportunity" => Self::Opportunity,
            "file" => Self::File,
            _ => return None,
        })
    }
}

/// 分析师的一轮对话请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalystQuery {
    /// 用户问题
    pub question: String,
    /// 对话历史（多轮上下文）；空表示首轮
    pub history: Vec<AnalystTurn>,
    /// 可选：限定在某个项目上下文内提问（项目详情页的"问 AI"）
    pub project_id: Option<String>,
    /// 该项目是否敏感（决定能否走云端，见 `ResolvedModel::resolve`）
    pub project_sensitive: bool,
}

/// 对话历史的最大轮数。
///
/// 超过就丢弃最早的：本地小模型上下文窗口有限（常见 4k~8k token），
/// 塞太多历史会挤掉真正重要的检索结果，反而答得更差。
pub const MAX_HISTORY_TURNS: usize = 6;

impl AnalystQuery {
    /// 归一化后的问题（trim + 折叠空白）。
    pub fn normalized_question(&self) -> String {
        self.question.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// 空问题不发起任何模型调用（省钱且避免 provider 400）。
    pub fn is_blank(&self) -> bool {
        self.normalized_question().is_empty()
    }

    /// 裁剪到窗口内的历史（保留最近 N 轮）。
    pub fn trimmed_history(&self) -> Vec<&AnalystTurn> {
        let n = self.history.len();
        let start = n.saturating_sub(MAX_HISTORY_TURNS);
        self.history[start..].iter().collect()
    }
}

/// 历史对话的一轮。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalystTurn {
    /// "user" 或 "assistant"
    pub role: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(source: AnswerSource, citations: usize) -> AnalystAnswer {
        AnalystAnswer {
            content: "回答内容".into(),
            generated_by: source,
            citations: (0..citations)
                .map(|i| Citation {
                    kind: CitationKind::Project,
                    label: format!("项目{i}"),
                    link: HitLink {
                        page: "project".into(),
                        param: Some(format!("p{i}")),
                    },
                    supports: None,
                })
                .collect(),
            followups: vec!["还想了解什么？".into()],
            took_ms: 42,
        }
    }

    /// 🔴 确定性回答必须能被识别出来，前端才不会被误标成"AI 生成"。
    #[test]
    fn deterministic_answer_is_identifiable() {
        let a = answer(AnswerSource::Deterministic, 2);
        assert!(a.is_deterministic());
        assert!(!a.generated_by.is_model());
        assert_eq!(a.generated_by.label(), "deterministic");
    }

    #[test]
    fn model_answer_carries_model_name() {
        let a = answer(AnswerSource::Model("qwen3:8b".into()), 1);
        assert!(!a.is_deterministic());
        assert!(a.generated_by.is_model());
        assert_eq!(a.generated_by.label(), "qwen3:8b");
    }

    /// 审计标签绝不能含密钥或请求原文。
    #[test]
    fn answer_source_label_has_no_secrets() {
        let label = AnswerSource::Model("openai:gpt-5-mini".into()).label();
        assert!(!label.contains("sk-"));
        assert_eq!(label, "openai:gpt-5-mini");
    }

    #[test]
    fn citations_are_detected() {
        assert!(answer(AnswerSource::Deterministic, 1).has_citations());
        assert!(!answer(AnswerSource::Deterministic, 0).has_citations());
    }

    #[test]
    fn citation_kind_roundtrips() {
        for k in [
            CitationKind::Project,
            CitationKind::Asset,
            CitationKind::Capability,
            CitationKind::File,
        ] {
            assert_eq!(CitationKind::parse(k.as_str()), Some(k));
            assert!(!k.label_zh().is_empty());
        }
        assert_eq!(CitationKind::parse("bogus"), None);
    }

    #[test]
    fn blank_question_is_detected() {
        let q = AnalystQuery {
            question: "   ".into(),
            history: vec![],
            project_id: None,
            project_sensitive: false,
        };
        assert!(q.is_blank());
        assert_eq!(q.normalized_question(), "");
    }

    #[test]
    fn question_is_normalized() {
        let q = AnalystQuery {
            question: "  这个   项目 做什么 ".into(),
            history: vec![],
            project_id: None,
            project_sensitive: false,
        };
        assert_eq!(q.normalized_question(), "这个 项目 做什么");
        assert!(!q.is_blank());
    }

    /// 历史必须裁剪到窗口内：小模型上下文有限，塞太多会挤掉检索结果。
    #[test]
    fn history_is_trimmed_to_window() {
        let q = AnalystQuery {
            question: "现在呢".into(),
            history: (0..20)
                .map(|i| AnalystTurn {
                    role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                    content: format!("第{i}轮"),
                })
                .collect(),
            project_id: None,
            project_sensitive: false,
        };
        let trimmed = q.trimmed_history();
        assert_eq!(trimmed.len(), MAX_HISTORY_TURNS);
        // 保留的是**最近**的，不是最早的
        assert_eq!(trimmed.last().unwrap().content, "第19轮");
        assert_eq!(trimmed.first().unwrap().content, format!("第{}轮", 20 - MAX_HISTORY_TURNS));
    }

    #[test]
    fn short_history_is_untouched() {
        let q = AnalystQuery {
            question: "hi".into(),
            history: vec![AnalystTurn {
                role: "user".into(),
                content: "之前的问题".into(),
            }],
            project_id: None,
            project_sensitive: false,
        };
        assert_eq!(q.trimmed_history().len(), 1);
    }

    #[test]
    fn answer_roundtrips_through_json() {
        let a = answer(AnswerSource::Model("m".into()), 2);
        let json = serde_json::to_string(&a).unwrap();
        let back: AnalystAnswer = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content, a.content);
        assert_eq!(back.citations.len(), 2);
        assert_eq!(back.generated_by.label(), "m");
    }
}
