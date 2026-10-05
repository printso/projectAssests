//! Spolia AI 层：模型抽象、路由、连接自检、确定性回退。
//!
//! # 分层
//! | 模块 | 职责 |
//! |---|---|
//! | `provider` | `LlmProvider` trait + 消息/请求/响应类型 + `ResolvedModel` 路由解析 |
//! | `openai` | OpenAI-compatible 后端（OpenAI/通义/DeepSeek/vLLM/LM Studio） |
//! | `ollama` | Ollama 原生后端 |
//! | `router` | 组装 provider、强制 Local-First、写审计、**确定性回退** |
//! | `fallback` | 无 LLM 时的检索式回答（带证据，不编造） |
//!
//! # 三条不可动摇的约束
//! 1. **Local-First**：敏感项目数据绝不进云端上下文。约束在 `ResolvedModel::resolve`
//!    落地，而非依赖每个调用点自觉检查。
//! 2. **可审计**：每次模型调用写一条 `AuditEntry`（provider:model + 摘要），
//!    但**绝不含** API Key 与代码原文。
//! 3. **可降级**：LLM 不可用时回退到 `fallback` 的检索式回答，
//!    而不是让 AI 分析师页面白屏。降级回答必须标注 `Deterministic`，
//!    不冒充"AI 生成"。

mod fallback;
mod ollama;
mod openai;
mod provider;
mod router;

// 测试桩只在 `cargo test` 时编译：它带一个后台监听线程，
// 不该以任何形式进入发布二进制。
#[cfg(test)]
mod teststub;

pub use fallback::{deterministic_answer, FallbackContext};
pub use ollama::{OllamaProvider, DEFAULT_PORT, PROVIDER_NAME as OLLAMA_PROVIDER_NAME};
pub use openai::{
    build_request_body as build_openai_request, OpenAiCompatibleProvider,
    PROVIDER_NAME as OPENAI_PROVIDER_NAME,
};
pub use provider::{
    timeout_for, ChatMessage, CompletionRequest, CompletionResponse, LlmProvider, ProviderHealth,
    ResolvedModel, Role, DEFAULT_MAX_TOKENS, DEFAULT_TIMEOUT_SECS,
};
pub use router::{AiRouter, RouterConfig, TestConnectionOutcome};

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::{AiError, LlmSettings, LocalBackend, RouteTarget};

    fn local_settings() -> LlmSettings {
        LlmSettings {
            local_base_url: "http://127.0.0.1:11434".into(),
            local_model: "qwen3:8b".into(),
            local_backend: LocalBackend::Ollama,
            ..LlmSettings::default()
        }
    }

    #[test]
    fn public_api_is_accessible() {
        assert_eq!(OPENAI_PROVIDER_NAME, "openai-compatible");
        assert_eq!(OLLAMA_PROVIDER_NAME, "ollama");
        assert_eq!(DEFAULT_PORT, 11434);
        assert_eq!(DEFAULT_MAX_TOKENS, 8192);
        assert_eq!(DEFAULT_TIMEOUT_SECS, 120);

        // 消息构造
        let msg = ChatMessage::user("hi");
        assert_eq!(msg.role, Role::User);
        assert!(msg.is_valid());

        // 路由解析
        let resolved =
            ResolvedModel::resolve(&local_settings(), spolia_domain::JobType::AnalyzeProject, false)
                .unwrap();
        assert!(resolved.is_local());

        // provider 构造（不实际连接）
        let _oai = OpenAiCompatibleProvider::new("http://127.0.0.1:1/v1", "m", None);
        let _ollama = OllamaProvider::new("http://127.0.0.1:11434", "qwen3:8b");
    }

    #[test]
    fn router_builds_for_local_settings() {
        let router = AiRouter::new(RouterConfig::default());
        let provider = router.provider_for(&local_settings(), spolia_domain::JobType::AnalyzeProject, false);
        // 本地后端 Ollama → 应得到 ollama provider
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().name(), OLLAMA_PROVIDER_NAME);
    }

    /// 敏感项目 + 云端路由 → 必须降级本地，这是安全红线。
    #[test]
    fn router_forces_local_for_sensitive_project() {
        let mut s = local_settings();
        s.cloud_base_url = "https://api.example.com/v1".into();
        s.cloud_api_key = "sk-x".into();
        s.cloud_model = "gpt-5-mini".into();
        s.route_deep = RouteTarget::Cloud;
        s.sensitive_local_only = true;

        let router = AiRouter::new(RouterConfig::default());
        let p = router
            .provider_for(&s, spolia_domain::JobType::DiscoverOpportunity, true)
            .unwrap();
        // 敏感项目深度分析：路由本想走云端，必须被拉回本地
        assert_eq!(p.name(), OLLAMA_PROVIDER_NAME, "敏感项目不得走云端 provider");
    }

    #[test]
    fn router_returns_none_when_unconfigured() {
        let mut s = local_settings();
        s.local_base_url = String::new();
        s.route_fast = RouteTarget::Local;
        let router = AiRouter::new(RouterConfig::default());
        assert!(
            router
                .provider_for(&s, spolia_domain::JobType::AnalyzeProject, false)
                .is_none(),
            "未配置时不该产出 provider"
        );
    }

    #[test]
    fn ai_error_degradable_classification() {
        assert!(AiError::NotConfigured.is_degradable());
        assert!(AiError::Connection("x".into()).is_degradable());
        assert!(!AiError::SensitiveBlocked.is_degradable(), "安全拦截不该被降级绕过");
        assert!(!AiError::Cancelled.is_degradable());
    }
}
