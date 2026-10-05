//! LLM Provider 抽象与配置解析。
//!
//! # 为什么需要 trait 边界
//! 1. **可测试**：所有上层逻辑（分析师、画像、审计）都对着 trait 编程，
//!    单测注入假 provider 即可，不需要真实网络与 API Key。
//! 2. **可替换**：OpenAI-compatible / Ollama / LM Studio 协议各不相同，
//!    但对上层是同一个 `complete()`。加一个新后端只需实现本 trait。
//!
//! # Local-First 硬约束（《技术设计书》§23）
//! 标记为敏感的项目，其任何数据都**不得**进入云端模型上下文。
//! 该约束由 [`ResolvedModel`] 在配置解析阶段就强制生效——
//! 而不是靠每个调用点自己记得检查（那必然会漏）。

use serde::{Deserialize, Serialize};
use projectassests_domain::{AiError, JobType, LlmSettings, RouteTarget};

/// LLM 对话消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// 一条对话消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }

    /// 内容非空校验：空消息会让某些 provider 直接报 400，
    /// 错误信息还很含糊，不如在这里挡掉。
    pub fn is_valid(&self) -> bool {
        !self.content.trim().is_empty()
    }
}

/// 一次补全请求。
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub messages: Vec<ChatMessage>,
    /// 采样温度。0.0 = 尽量确定（用于结构化抽取），
    /// 0.7 左右 = 更自然（用于对话式分析）。
    pub temperature: f32,
    /// 最大输出 token 数。必须有限：本地小模型不设上限时会一直生成到爆显存。
    pub max_tokens: u32,
    /// 强制输出 JSON（provider 支持时启用，可显著降低解析失败率）。
    pub json_mode: bool,
}

impl Default for CompletionRequest {
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            temperature: 0.2,
            max_tokens: DEFAULT_MAX_TOKENS,
            json_mode: false,
        }
    }
}

/// 默认最大输出 token。
///
/// 8192 足够输出一份项目画像或机会分析；
/// 再大只会让慢模型拖更久而内容并不更好。
pub const DEFAULT_MAX_TOKENS: u32 = 8192;

/// 默认请求超时（秒）。
///
/// 本地 7B 模型在 CPU 上跑 8k token 可能要几分钟，
/// 但对话式分析通常几百 token 就够——按用途区分超时见 `timeout_for`。
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

impl CompletionRequest {
    pub fn with_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.messages = messages;
        self
    }

    /// 采样温度（wire 格式用的 f64）。
    ///
    /// 🔴 不能直接 `f64::from(self.temperature)`：`0.3f32` 提升后是
    /// `0.30000001192092896`，会原样出现在请求体、日志与测试断言里。
    /// 温度只有 2~3 位小数有意义，四舍五入到 3 位即可得到干净的 `0.3`。
    ///
    /// 两个 provider（OpenAI-compatible / Ollama）共用本方法，
    /// 保证它们发出的温度值口径一致。
    pub fn temperature_f64(&self) -> f64 {
        if self.temperature.is_nan() {
            return 0.2;
        }
        (f64::from(self.temperature) * 1000.0).round() / 1000.0
    }

    pub fn temperature(mut self, t: f32) -> Self {
        // NaN 会让 provider 报错或行为未定义，钳到合法区间
        self.temperature = if t.is_nan() { 0.2 } else { t.clamp(0.0, 2.0) };
        self
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n.clamp(1, 32_768);
        self
    }

    pub fn json_mode(mut self, on: bool) -> Self {
        self.json_mode = on;
        self
    }

    /// 请求是否可用（至少一条消息且消息内容非空）。
    pub fn validate(&self) -> Result<(), AiError> {
        if self.messages.is_empty() {
            return Err(AiError::MalformedResponse("请求不含任何消息".to_string()));
        }
        if self.messages.iter().any(|m| !m.is_valid()) {
            return Err(AiError::MalformedResponse("存在内容为空的消息".to_string()));
        }
        Ok(())
    }
}

/// 一次补全的结果。
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    /// 模型输出文本
    pub text: String,
    /// 实际使用的模型标识（写入审计日志与 `generated_by`）
    pub model: String,
    /// token 用量（provider 不报则为 `None`，不编造）
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    /// 请求耗时（毫秒）
    pub took_ms: u64,
}

impl CompletionResponse {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// 总 token 数；任一项缺失时返回 `None`（不用 0 冒充）。
    pub fn total_tokens(&self) -> Option<u32> {
        Some(self.prompt_tokens? + self.completion_tokens?)
    }
}

/// Provider 能力边界。
///
/// 实现者负责：拼协议、发请求、把 HTTP 错误映射成 [`AiError`]。
/// 实现者**不负责**：路由决策、敏感项目拦截、审计记录
/// ——那些属于 [`crate::router`]，否则每个后端都要重写一遍且容易漏。
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// 后端名称（写入审计日志，如 "ollama"、"openai-compatible"）。
    fn name(&self) -> &'static str;

    /// 执行补全。
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, AiError>;

    /// 连接自检（设置页「测试连接」按钮）。
    ///
    /// 与 `complete` 分开：自检要快（列模型 / 极短补全），
    /// 且失败时要给出**可操作**的提示（"Ollama 未启动"而非"connection refused"）。
    async fn health_check(&self) -> Result<ProviderHealth, AiError>;
}

/// 连接自检结果。
#[derive(Debug, Clone, Default)]
pub struct ProviderHealth {
    pub ok: bool,
    /// 面向用户的结论（"已连接 qwen3:8b"）
    pub message: String,
    /// 可用模型列表（provider 支持时填充）
    pub models: Vec<String>,
    pub latency_ms: u64,
}

/// 解析后的模型配置：一次调用的全部要素。
///
/// # 为什么要这个中间层
/// `LlmSettings` 是**用户意图**（路由策略 + 两套端点配置），
/// 而 provider 需要的是**具体端点**（base_url + model + key）。
/// 中间这层负责把意图落成具体值，并在落地时强制 Local-First 约束。
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub route: RouteTarget,
    pub base_url: String,
    pub model: String,
    /// 云端密钥；本地后端为 `None`
    pub api_key: Option<String>,
    /// 🔴 是否因"敏感项目"被强制降级到本地。
    /// 必须记录下来：审计日志要能回答"这次为什么走了本地"。
    pub forced_local: bool,
}

impl ResolvedModel {
    /// 按任务类型与项目敏感性解析出实际使用的模型。
    ///
    /// 🔴 敏感项目 + `sensitive_local_only` ⇒ **无条件本地**，
    /// 即使用户把深度分析路由设成了云端。这是硬约束不是偏好。
    pub fn resolve(
        settings: &LlmSettings,
        job: JobType,
        project_sensitive: bool,
    ) -> Result<Self, AiError> {
        let route = settings.route_for(job, project_sensitive);
        let forced_local = project_sensitive
            && settings.sensitive_local_only
            && route == RouteTarget::Local
            && settings.route_for(job, false) == RouteTarget::Cloud;

        match route {
            RouteTarget::Cloud => {
                if !settings.cloud_configured() {
                    // 不给静默降级：用户明确选了云端却降级到本地，
                    // 会让人误以为在用 GPT 而实际在用 7B 小模型（质量骤降且无从察觉）。
                    // 明确报错让上层决定是提示用户配置，还是显式回退到确定性回答。
                    return Err(AiError::NotConfigured);
                }
                Ok(Self {
                    route,
                    base_url: settings.cloud_base_url.trim().trim_end_matches('/').to_string(),
                    model: settings.cloud_model.clone(),
                    api_key: Some(settings.cloud_api_key.clone()),
                    forced_local,
                })
            }
            RouteTarget::Local => {
                if !settings.local_configured() {
                    return Err(AiError::NotConfigured);
                }
                Ok(Self {
                    route,
                    base_url: settings.local_base_url.trim().trim_end_matches('/').to_string(),
                    model: settings.local_model.clone(),
                    api_key: None,
                    forced_local,
                })
            }
        }
    }

    /// 该配置是否指向本地后端（审计与 UI 展示用）。
    pub fn is_local(&self) -> bool {
        self.route == RouteTarget::Local
    }

    /// 审计用的 provider 标识（`route:model`，不含密钥）。
    ///
    /// 🔴 绝不把 api_key 放进任何日志或审计字段。
    pub fn audit_label(&self) -> String {
        format!("{}:{}", self.route.as_str(), self.model)
    }
}

/// 按用途给出超时秒数。
///
/// 结构化抽取（画像/机会）输出长，需要更宽的时间；
/// 对话式回答若等两分钟，用户早就以为卡死了。
pub fn timeout_for(json_mode: bool) -> u64 {
    if json_mode {
        DEFAULT_TIMEOUT_SECS
    } else {
        60
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{CloudProvider, LocalBackend};

    fn local_settings() -> LlmSettings {
        LlmSettings {
            local_base_url: "http://127.0.0.1:11434".into(),
            local_model: "qwen3:8b".into(),
            local_backend: LocalBackend::Ollama,
            ..LlmSettings::default()
        }
    }

    fn cloud_settings() -> LlmSettings {
        LlmSettings {
            cloud_base_url: "https://api.example.com/v1".into(),
            cloud_model: "gpt-5-mini".into(),
            cloud_api_key: "sk-test-1234567890".into(),
            cloud_provider: CloudProvider::OpenAiCompatible,
            route_fast: RouteTarget::Cloud,
            route_deep: RouteTarget::Cloud,
            ..LlmSettings::default()
        }
    }

    // ── 消息与请求校验 ───────────────────────────────────────────

    #[test]
    fn message_constructors_set_roles() {
        assert_eq!(ChatMessage::system("a").role, Role::System);
        assert_eq!(ChatMessage::user("b").role, Role::User);
        assert_eq!(ChatMessage::assistant("c").role, Role::Assistant);
        assert_eq!(Role::System.as_str(), "system");
    }

    #[test]
    fn blank_message_is_invalid() {
        assert!(!ChatMessage::user("").is_valid());
        assert!(!ChatMessage::user("   \n\t ").is_valid());
        assert!(ChatMessage::user("有内容").is_valid());
    }

    #[test]
    fn request_rejects_empty_messages() {
        let req = CompletionRequest::default();
        assert!(matches!(req.validate(), Err(AiError::MalformedResponse(_))));
    }

    /// 空消息必须被挡住：某些 provider 会返回含糊的 400，排查成本远高于此处校验。
    #[test]
    fn request_rejects_blank_message_content() {
        let req = CompletionRequest::default()
            .with_messages(vec![ChatMessage::system("你是助手"), ChatMessage::user("  ")]);
        assert!(matches!(req.validate(), Err(AiError::MalformedResponse(_))));
    }

    #[test]
    fn request_accepts_valid_messages() {
        let req = CompletionRequest::default()
            .with_messages(vec![ChatMessage::user("这个项目在做什么？")]);
        assert!(req.validate().is_ok());
    }

    #[test]
    fn temperature_is_clamped_and_nan_safe() {
        assert_eq!(CompletionRequest::default().temperature(5.0).temperature, 2.0);
        assert_eq!(CompletionRequest::default().temperature(-1.0).temperature, 0.0);
        assert!(!CompletionRequest::default().temperature(f32::NAN).temperature.is_nan());
        assert_eq!(CompletionRequest::default().temperature(0.7).temperature, 0.7);
    }

    #[test]
    fn max_tokens_is_clamped() {
        assert_eq!(CompletionRequest::default().max_tokens(0).max_tokens, 1);
        assert_eq!(CompletionRequest::default().max_tokens(999_999).max_tokens, 32_768);
        assert_eq!(DEFAULT_MAX_TOKENS, 8192);
    }

    #[test]
    fn json_mode_toggles() {
        assert!(CompletionRequest::default().json_mode(true).json_mode);
        assert!(!CompletionRequest::default().json_mode);
    }

    #[test]
    fn timeout_differs_by_purpose() {
        assert_eq!(timeout_for(true), DEFAULT_TIMEOUT_SECS);
        assert!(
            timeout_for(false) < timeout_for(true),
            "对话式超时应更短，否则用户以为卡死"
        );
    }

    // ── 响应 ─────────────────────────────────────────────────────

    #[test]
    fn response_detects_empty_text() {
        let r = CompletionResponse {
            text: "  ".into(),
            model: "m".into(),
            prompt_tokens: Some(1),
            completion_tokens: Some(2),
            took_ms: 10,
        };
        assert!(r.is_empty());
        assert_eq!(r.total_tokens(), Some(3));
    }

    /// token 用量缺失时必须返回 None，不能用 0 冒充（会污染成本统计）。
    #[test]
    fn response_total_tokens_is_none_when_partial() {
        let r = CompletionResponse {
            text: "x".into(),
            model: "m".into(),
            prompt_tokens: Some(1),
            completion_tokens: None,
            took_ms: 10,
        };
        assert_eq!(r.total_tokens(), None);
    }

    // ── 路由解析（Local-First 硬约束）────────────────────────────

    #[test]
    fn resolves_local_model() {
        let s = local_settings();
        let m = ResolvedModel::resolve(&s, JobType::AnalyzeProject, false).unwrap();
        assert!(m.is_local());
        assert_eq!(m.model, "qwen3:8b");
        assert_eq!(m.base_url, "http://127.0.0.1:11434");
        assert!(m.api_key.is_none(), "本地后端不需要密钥");
        assert!(!m.forced_local);
    }

    #[test]
    fn resolves_cloud_model_with_key() {
        let s = cloud_settings();
        let m = ResolvedModel::resolve(&s, JobType::AnalyzeProject, false).unwrap();
        assert!(!m.is_local());
        assert_eq!(m.api_key.as_deref(), Some("sk-test-1234567890"));
    }

    /// 🔴 核心安全约束：敏感项目即使路由设为云端，也必须落到本地。
    #[test]
    fn sensitive_project_is_forced_local() {
        let mut s = cloud_settings();
        s.sensitive_local_only = true;
        let m = ResolvedModel::resolve(&s, JobType::AnalyzeProject, true).unwrap();
        assert!(m.is_local(), "敏感项目不得走云端");
        assert!(m.forced_local, "必须记录这是强制降级，供审计回答'为什么走本地'");
        assert!(m.api_key.is_none(), "降级到本地后不得携带云端密钥");
    }

    /// 未开启"敏感仅本地"开关时，敏感项目仍可走云端（用户显式授权）。
    #[test]
    fn sensitive_project_uses_cloud_when_switch_off() {
        let mut s = cloud_settings();
        s.sensitive_local_only = false;
        let m = ResolvedModel::resolve(&s, JobType::AnalyzeProject, true).unwrap();
        assert!(!m.is_local());
        assert!(!m.forced_local);
    }

    /// 深度分析任务走 route_deep，快速分析走 route_fast。
    #[test]
    fn route_depends_on_job_depth() {
        let mut s = cloud_settings();
        s.route_fast = RouteTarget::Local;
        s.route_deep = RouteTarget::Cloud;
        s.local_base_url = "http://127.0.0.1:11434".into();

        let fast = ResolvedModel::resolve(&s, JobType::AnalyzeProject, false).unwrap();
        assert!(fast.is_local(), "画像属快速分析");
        let deep = ResolvedModel::resolve(&s, JobType::DiscoverOpportunity, false).unwrap();
        assert!(!deep.is_local(), "机会发现属深度分析");
    }

    /// 未配置就选云端 → 明确报错，不静默降级。
    /// 静默降级会让用户以为在用 GPT 而实际在用 7B 模型，质量骤降且无从察觉。
    #[test]
    fn unconfigured_cloud_fails_loudly() {
        let mut s = local_settings();
        s.route_fast = RouteTarget::Cloud;
        s.cloud_api_key = String::new(); // 没有 key
        let err = ResolvedModel::resolve(&s, JobType::AnalyzeProject, false).unwrap_err();
        assert!(matches!(err, AiError::NotConfigured));
        assert!(err.is_degradable(), "上层应能降级到确定性回答");
    }

    /// 云端 base_url 非法（不是 http）同样视为未配置。
    #[test]
    fn invalid_cloud_url_is_not_configured() {
        let mut s = cloud_settings();
        s.cloud_base_url = "api.example.com/v1".into(); // 缺 scheme
        assert!(matches!(
            ResolvedModel::resolve(&s, JobType::AnalyzeProject, false).unwrap_err(),
            AiError::NotConfigured
        ));
    }

    #[test]
    fn unconfigured_local_fails() {
        let mut s = cloud_settings();
        s.local_base_url = String::new();
        s.route_fast = RouteTarget::Local;
        assert!(matches!(
            ResolvedModel::resolve(&s, JobType::AnalyzeProject, false).unwrap_err(),
            AiError::NotConfigured
        ));
    }

    /// base_url 尾部斜杠必须去掉，否则拼出 `/v1//chat/completions`。
    #[test]
    fn trailing_slash_is_stripped() {
        let mut s = local_settings();
        s.local_base_url = "http://127.0.0.1:11434/".into();
        let m = ResolvedModel::resolve(&s, JobType::AnalyzeProject, false).unwrap();
        assert_eq!(m.base_url, "http://127.0.0.1:11434");
    }

    /// 🔴 审计标识绝不能包含密钥。
    #[test]
    fn audit_label_never_leaks_api_key() {
        let s = cloud_settings();
        let m = ResolvedModel::resolve(&s, JobType::AnalyzeProject, false).unwrap();
        let label = m.audit_label();
        assert!(!label.contains("sk-test"), "审计标识泄漏了密钥: {label}");
        assert_eq!(label, "cloud:gpt-5-mini");
    }

    #[test]
    fn audit_label_shows_route_and_model() {
        let s = local_settings();
        let m = ResolvedModel::resolve(&s, JobType::AnalyzeProject, false).unwrap();
        assert_eq!(m.audit_label(), "local:qwen3:8b");
    }
}
