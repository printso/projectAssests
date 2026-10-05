//! 模型路由：把"用户意图"落到"具体后端"，并强制 Local-First 与审计。
//!
//! # 为什么路由独立成一层
//! provider 只管协议（怎么发请求），settings 只管存储（用户配了什么）。
//! 路由是二者之间的**决策层**：这次调用该走本地还是云端、用哪个模型、
//! 敏感项目要不要拦。把这些决策集中在一个地方，
//! 才能保证 Local-First 约束不会在某个新增调用点被漏掉。

use std::sync::Arc;

use spolia_domain::{AiError, JobType, LlmSettings, LocalBackend, RouteTarget};

use crate::ollama::OllamaProvider;
use crate::openai::OpenAiCompatibleProvider;
use crate::provider::{LlmProvider, ProviderHealth, ResolvedModel};

/// 路由配置。
///
/// 目前只有一个开关，但独立成结构体是为了将来加"按任务类型指定不同本地模型"
/// 之类的需求时不必改函数签名（开源项目里签名变更的破坏性远大于加字段）。
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// 云端不可用时是否自动回退到本地（若本地已配置）。
    ///
    /// 默认 `true`：用户配了云端但网络不通时，用本地小模型给出降级回答，
    /// 比直接报错的体验好。回退会在审计里如实记录。
    pub cloud_fallback_to_local: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            cloud_fallback_to_local: true,
        }
    }
}

/// 连接自检结果（设置页「测试连接」的返回）。
#[derive(Debug, Clone)]
pub struct TestConnectionOutcome {
    pub ok: bool,
    /// 面向用户的结论（成功/失败原因）
    pub message: String,
    /// 实际测试的后端名
    pub backend: String,
    /// 被测模型
    pub model: String,
    pub route: RouteTarget,
    /// 可用模型列表（本地后端能列出，云端通常不返回）
    pub models: Vec<String>,
    pub latency_ms: u64,
}

impl TestConnectionOutcome {
    /// 由 provider 自检结果构造。
    fn from_health(health: ProviderHealth, backend: &str, model: &str, route: RouteTarget) -> Self {
        Self {
            ok: health.ok,
            message: health.message,
            backend: backend.to_string(),
            model: model.to_string(),
            route,
            models: health.models,
            latency_ms: health.latency_ms,
        }
    }

    /// 由错误构造（连接失败等）。
    fn from_error(err: &AiError, backend: &str, model: &str, route: RouteTarget) -> Self {
        Self {
            ok: false,
            message: err.to_string(),
            backend: backend.to_string(),
            model: model.to_string(),
            route,
            models: Vec::new(),
            latency_ms: 0,
        }
    }
}

/// 模型路由器。无状态，可克隆。
#[derive(Debug, Clone, Default)]
pub struct AiRouter {
    config: RouterConfig,
}

impl AiRouter {
    pub fn new(config: RouterConfig) -> Self {
        Self { config }
    }

    /// 解析本次调用应使用的模型配置。
    ///
    /// 🔴 Local-First 在这里强制生效：敏感项目 + `sensitive_local_only`
    /// 一律落到本地，无论用户把路由设成了什么。
    ///
    /// 返回 `Err(NotConfigured)` 表示所选路由未配置好——
    /// 上层据此决定是提示用户配置，还是走 `crate::fallback` 的确定性回答。
    pub fn resolve(
        &self,
        settings: &LlmSettings,
        job: JobType,
        project_sensitive: bool,
    ) -> Result<ResolvedModel, AiError> {
        let resolved = ResolvedModel::resolve(settings, job, project_sensitive);

        // 云端未配置且允许回退时，尝试本地：
        // 用户配了本地模型但把路由指向云端（常见于"想试试 GPT 但没填 key"），
        // 直接报错不如用本地给出可用结果，并在回答里说明是降级。
        if let Err(AiError::NotConfigured) = resolved
            && self.config.cloud_fallback_to_local
            && settings.local_configured()
            && settings.route_for(job, project_sensitive) == RouteTarget::Cloud
        {
            return Ok(ResolvedModel {
                route: RouteTarget::Local,
                base_url: settings.local_base_url.trim().trim_end_matches('/').to_string(),
                model: settings.local_model.clone(),
                api_key: None,
                forced_local: false,
            });
        }
        resolved
    }

    /// 按配置构造具体 provider。未配置时返回 `None`。
    pub fn provider_for(
        &self,
        settings: &LlmSettings,
        job: JobType,
        project_sensitive: bool,
    ) -> Option<Arc<dyn LlmProvider>> {
        let resolved = self.resolve(settings, job, project_sensitive).ok()?;
        Some(build_provider(settings, &resolved))
    }

    /// 连接自检（设置页「测试连接」按钮）。
    ///
    /// `route` 指定测哪个后端；`None` 表示测"当前默认路由"。
    /// 🔴 自检失败必须返回**可操作**的提示（"请确认 Ollama 已启动"），
    /// 而不是把 `connection refused` 直接抛给用户。
    pub async fn test_connection(
        &self,
        settings: &LlmSettings,
        route: Option<RouteTarget>,
    ) -> TestConnectionOutcome {
        // 用 AnalyzeProject 作为探针任务类型：它是"快速分析"路由，
        // 自检关心的是端点通不通，与具体任务无关
        let job = JobType::AnalyzeProject;
        let resolved = match route {
            Some(RouteTarget::Cloud) => cloud_probe(settings),
            Some(RouteTarget::Local) => local_probe(settings),
            None => self.resolve(settings, job, false),
        };

        let Ok(resolved) = resolved else {
            return TestConnectionOutcome {
                ok: false,
                message: not_configured_message(settings, route),
                backend: backend_name(settings, route).to_string(),
                model: route
                    .map(|r| model_for_route(settings, r))
                    .unwrap_or_default(),
                route: route.unwrap_or(RouteTarget::Local),
                models: Vec::new(),
                latency_ms: 0,
            };
        };

        let provider = build_provider(settings, &resolved);
        let backend = provider.name().to_string();
        let model = resolved.model.clone();
        let route_actual = resolved.route;

        match provider.health_check().await {
            Ok(health) => TestConnectionOutcome::from_health(health, &backend, &model, route_actual),
            Err(err) => TestConnectionOutcome::from_error(&err, &backend, &model, route_actual),
        }
    }
}

/// 按解析结果构造具体 provider 实例。
fn build_provider(settings: &LlmSettings, resolved: &ResolvedModel) -> Arc<dyn LlmProvider> {
    match resolved.route {
        RouteTarget::Local => match settings.local_backend {
            // 只有 Ollama 用原生协议；llama.cpp / LM Studio 都提供 /v1 兼容端点
            LocalBackend::Ollama => Arc::new(OllamaProvider::new(
                resolved.base_url.clone(),
                resolved.model.clone(),
            )),
            LocalBackend::LlamaCpp | LocalBackend::LmStudio => Arc::new(
                OpenAiCompatibleProvider::new(
                    openai_style_base(&resolved.base_url),
                    resolved.model.clone(),
                    None, // 本地后端无需密钥
                ),
            ),
        },
        RouteTarget::Cloud => Arc::new(OpenAiCompatibleProvider::new(
            resolved.base_url.clone(),
            resolved.model.clone(),
            resolved.api_key.clone(),
        )),
    }
}

/// 把用户填的 base_url 规整成 OpenAI 风格（以 `/v1` 结尾）。
///
/// LM Studio 默认给 `http://localhost:1234`（不带 /v1），
/// llama.cpp server 也一样。不补的话拼出来是 `/chat/completions`，404。
/// 用户已经带了 `/v1` 或 `/v1/` 的则原样保留。
fn openai_style_base(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/v1") {
        b.to_string()
    } else {
        format!("{b}/v1")
    }
}

/// 云端探针配置（自检时强制测云端）。
fn cloud_probe(settings: &LlmSettings) -> Result<ResolvedModel, AiError> {
    if !settings.cloud_configured() {
        return Err(AiError::NotConfigured);
    }
    Ok(ResolvedModel {
        route: RouteTarget::Cloud,
        base_url: settings.cloud_base_url.trim().trim_end_matches('/').to_string(),
        model: settings.cloud_model.clone(),
        api_key: Some(settings.cloud_api_key.clone()),
        forced_local: false,
    })
}

/// 本地探针配置（自检时强制测本地）。
fn local_probe(settings: &LlmSettings) -> Result<ResolvedModel, AiError> {
    if !settings.local_configured() {
        return Err(AiError::NotConfigured);
    }
    Ok(ResolvedModel {
        route: RouteTarget::Local,
        base_url: settings.local_base_url.trim().trim_end_matches('/').to_string(),
        model: settings.local_model.clone(),
        api_key: None,
        forced_local: false,
    })
}

/// 指定路由对应的模型名（自检失败时也要告诉用户测的是哪个模型）。
fn model_for_route(settings: &LlmSettings, route: RouteTarget) -> String {
    match route {
        RouteTarget::Cloud => settings.cloud_model.clone(),
        RouteTarget::Local => settings.local_model.clone(),
    }
}

/// 指定路由对应的后端名。
fn backend_name(settings: &LlmSettings, route: Option<RouteTarget>) -> &'static str {
    match route.unwrap_or(RouteTarget::Local) {
        RouteTarget::Cloud => crate::openai::PROVIDER_NAME,
        RouteTarget::Local => match settings.local_backend {
            LocalBackend::Ollama => crate::ollama::PROVIDER_NAME,
            _ => crate::openai::PROVIDER_NAME,
        },
    }
}

/// 未配置时给用户的引导文案（按路由区分，指出去哪里填）。
fn not_configured_message(settings: &LlmSettings, route: Option<RouteTarget>) -> String {
    match route.unwrap_or(RouteTarget::Local) {
        RouteTarget::Cloud => {
            if settings.cloud_api_key.trim().is_empty() {
                "云端模型未配置：请在 设置 → 大模型配置 填写 API Key".to_string()
            } else {
                "云端模型地址无效：请填写以 http(s):// 开头的完整地址".to_string()
            }
        }
        RouteTarget::Local => match settings.local_backend {
            LocalBackend::Ollama => {
                "本地模型未配置：请确认 Ollama 已启动（ollama serve），地址默认为 http://127.0.0.1:11434".to_string()
            }
            _ => "本地模型未配置：请填写 LM Studio / llama.cpp 的服务地址".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::{CloudProvider, JobType, RouteTarget};

    fn ollama_settings() -> LlmSettings {
        LlmSettings {
            local_backend: LocalBackend::Ollama,
            local_base_url: "http://127.0.0.1:11434".into(),
            local_model: "qwen3:8b".into(),
            ..LlmSettings::default()
        }
    }

    fn lmstudio_settings() -> LlmSettings {
        LlmSettings {
            local_backend: LocalBackend::LmStudio,
            local_base_url: "http://localhost:1234".into(),
            local_model: "qwen2.5-coder".into(),
            ..LlmSettings::default()
        }
    }

    fn cloud_settings() -> LlmSettings {
        LlmSettings {
            cloud_provider: CloudProvider::OpenAiCompatible,
            cloud_base_url: "https://api.example.com/v1".into(),
            cloud_model: "gpt-5-mini".into(),
            cloud_api_key: "sk-test-key".into(),
            route_fast: RouteTarget::Cloud,
            route_deep: RouteTarget::Cloud,
            ..LlmSettings::default()
        }
    }

    // ── base_url 规整 ────────────────────────────────────────────

    /// LM Studio / llama.cpp 默认地址不带 /v1，必须补上否则 404。
    #[test]
    fn openai_base_appends_v1_when_missing() {
        assert_eq!(openai_style_base("http://localhost:1234"), "http://localhost:1234/v1");
        assert_eq!(openai_style_base("http://localhost:1234/"), "http://localhost:1234/v1");
    }

    #[test]
    fn openai_base_keeps_existing_v1() {
        assert_eq!(openai_style_base("http://x/v1"), "http://x/v1");
        assert_eq!(openai_style_base("http://x/v1/"), "http://x/v1");
    }

    // ── provider 选择 ────────────────────────────────────────────

    #[test]
    fn ollama_backend_uses_native_provider() {
        let r = AiRouter::new(RouterConfig::default());
        let p = r
            .provider_for(&ollama_settings(), JobType::AnalyzeProject, false)
            .unwrap();
        assert_eq!(p.name(), crate::ollama::PROVIDER_NAME);
    }

    /// LM Studio 走 OpenAI 兼容协议，且 base_url 必须被补上 /v1。
    #[test]
    fn lmstudio_backend_uses_openai_compatible() {
        let r = AiRouter::new(RouterConfig::default());
        let p = r
            .provider_for(&lmstudio_settings(), JobType::AnalyzeProject, false)
            .unwrap();
        assert_eq!(p.name(), crate::openai::PROVIDER_NAME);
    }

    #[test]
    fn cloud_route_uses_openai_compatible() {
        let r = AiRouter::new(RouterConfig::default());
        let p = r
            .provider_for(&cloud_settings(), JobType::AnalyzeProject, false)
            .unwrap();
        assert_eq!(p.name(), crate::openai::PROVIDER_NAME);
    }

    // ── Local-First 硬约束 ───────────────────────────────────────

    /// 🔴 安全红线：敏感项目在云端路由下必须被拉回本地。
    #[test]
    fn sensitive_project_never_reaches_cloud_provider() {
        let mut s = ollama_settings();
        s.cloud_base_url = "https://api.example.com/v1".into();
        s.cloud_api_key = "sk-x".into();
        s.cloud_model = "gpt-5".into();
        s.route_deep = RouteTarget::Cloud;
        s.sensitive_local_only = true;

        let r = AiRouter::new(RouterConfig::default());
        let resolved = r
            .resolve(&s, JobType::DiscoverOpportunity, true)
            .unwrap();
        assert!(resolved.is_local(), "敏感项目必须走本地");
        assert!(resolved.api_key.is_none(), "不得携带云端密钥");
        assert!(
            resolved.forced_local,
            "必须标记为强制降级，审计才能回答'为什么走本地'"
        );
    }

    /// 关闭开关后，敏感项目仍按用户设定的路由走（用户显式授权）。
    #[test]
    fn sensitive_local_only_switch_respected_when_off() {
        let mut s = cloud_settings();
        s.sensitive_local_only = false;
        let r = AiRouter::new(RouterConfig::default());
        let resolved = r.resolve(&s, JobType::AnalyzeProject, true).unwrap();
        assert!(!resolved.is_local());
        assert!(!resolved.forced_local);
    }

    // ── 云端未配置时的回退 ───────────────────────────────────────

    /// 用户指向云端但没填 key，且本地可用 → 回退本地而非直接报错。
    #[test]
    fn cloud_unconfigured_falls_back_to_local() {
        let mut s = ollama_settings();
        s.route_fast = RouteTarget::Cloud;
        s.cloud_api_key = String::new(); // 没填 key

        let r = AiRouter::new(RouterConfig::default());
        let resolved = r.resolve(&s, JobType::AnalyzeProject, false).unwrap();
        assert!(resolved.is_local(), "应回退到本地");
    }

    /// 关掉回退开关后，云端未配置就应如实报错。
    #[test]
    fn cloud_fallback_can_be_disabled() {
        let mut s = ollama_settings();
        s.route_fast = RouteTarget::Cloud;
        s.cloud_api_key = String::new();

        let r = AiRouter::new(RouterConfig {
            cloud_fallback_to_local: false,
        });
        assert!(matches!(
            r.resolve(&s, JobType::AnalyzeProject, false).unwrap_err(),
            AiError::NotConfigured
        ));
    }

    /// 本地也没配时，回退同样失败——必须报错而不是给出一个连不上的 provider。
    #[test]
    fn no_fallback_target_reports_not_configured() {
        let mut s = ollama_settings();
        s.local_base_url = String::new();
        s.route_fast = RouteTarget::Cloud;
        s.cloud_api_key = String::new();
        let r = AiRouter::new(RouterConfig::default());
        assert!(r.resolve(&s, JobType::AnalyzeProject, false).is_err());
        assert!(r.provider_for(&s, JobType::AnalyzeProject, false).is_none());
    }

    // ── 路由按任务深度分流 ───────────────────────────────────────

    #[test]
    fn deep_analysis_uses_deep_route() {
        let mut s = cloud_settings();
        s.route_fast = RouteTarget::Local;
        s.route_deep = RouteTarget::Cloud;
        s.local_base_url = "http://127.0.0.1:11434".into();

        let r = AiRouter::new(RouterConfig::default());
        let fast = r.resolve(&s, JobType::AnalyzeProject, false).unwrap();
        assert!(fast.is_local(), "画像属快速分析");
        let deep = r.resolve(&s, JobType::DiscoverOpportunity, false).unwrap();
        assert!(!deep.is_local(), "机会发现属深度分析");
    }

    // ── 自检文案 ─────────────────────────────────────────────────

    /// 自检失败必须给出可操作提示，不能把底层错误直接抛给用户。
    #[tokio::test]
    async fn test_connection_reports_unconfigured_clearly() {
        let s = LlmSettings {
            local_base_url: String::new(),
            ..LlmSettings::default()
        };
        let r = AiRouter::new(RouterConfig::default());
        let out = r.test_connection(&s, Some(RouteTarget::Local)).await;
        assert!(!out.ok);
        assert!(
            out.message.contains("设置") || out.message.contains("Ollama") || out.message.contains("地址"),
            "提示应可操作: {}",
            out.message
        );
    }

    #[tokio::test]
    async fn test_connection_cloud_without_key_explains() {
        let mut s = ollama_settings();
        s.cloud_api_key = String::new();
        let r = AiRouter::new(RouterConfig::default());
        let out = r.test_connection(&s, Some(RouteTarget::Cloud)).await;
        assert!(!out.ok);
        assert!(out.message.contains("API Key"), "实际: {}", out.message);
        assert_eq!(out.route, RouteTarget::Cloud);
    }

    /// 连不上的本地端点：自检应失败并提示启动 Ollama。
    #[tokio::test]
    async fn test_connection_unreachable_local() {
        let s = LlmSettings {
            local_backend: LocalBackend::Ollama,
            local_base_url: "http://127.0.0.1:1".into(),
            local_model: "qwen3:8b".into(),
            ..LlmSettings::default()
        };
        let r = AiRouter::new(RouterConfig::default());
        let out = r.test_connection(&s, Some(RouteTarget::Local)).await;
        assert!(!out.ok);
        assert!(out.message.contains("ollama serve"), "实际: {}", out.message);
        assert_eq!(out.model, "qwen3:8b");
    }

    #[test]
    fn backend_name_matches_settings() {
        assert_eq!(
            backend_name(&ollama_settings(), Some(RouteTarget::Local)),
            crate::ollama::PROVIDER_NAME
        );
        assert_eq!(
            backend_name(&lmstudio_settings(), Some(RouteTarget::Local)),
            crate::openai::PROVIDER_NAME
        );
        assert_eq!(
            backend_name(&cloud_settings(), Some(RouteTarget::Cloud)),
            crate::openai::PROVIDER_NAME
        );
    }

    #[test]
    fn model_for_route_picks_right_field() {
        let s = cloud_settings();
        assert_eq!(model_for_route(&s, RouteTarget::Cloud), "gpt-5-mini");
        let l = ollama_settings();
        assert_eq!(model_for_route(&l, RouteTarget::Local), "qwen3:8b");
    }

    #[test]
    fn router_config_defaults_to_allowing_fallback() {
        assert!(RouterConfig::default().cloud_fallback_to_local);
    }
}
