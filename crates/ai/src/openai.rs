//! OpenAI-compatible provider（`/v1/chat/completions`）。
//!
//! 覆盖 OpenAI、通义千问、DeepSeek、硅基流动、vLLM、以及 LM Studio 的
//! OpenAI 兼容端点——它们的请求/响应结构一致，只差 base_url 与模型名。
//!
//! # 可测试性设计
//! 请求构造与响应解析都是**纯函数**（[`build_request_body`] / [`parse_response`]），
//! HTTP 只是把它们串起来的薄层。这样协议细节可以被穷举单测，
//! 而不需要起真实服务器或打网络——网络测试在 CI 上既慢又不稳定。

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use spolia_domain::AiError;

use crate::provider::{
    timeout_for, ChatMessage, CompletionRequest, CompletionResponse, LlmProvider, ProviderHealth,
};

/// 该后端在审计日志中的名字。
pub const PROVIDER_NAME: &str = "openai-compatible";

/// OpenAI-compatible provider。
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleProvider {
    base_url: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        let base_url = base_url.into();
        Self {
            // 🔴 本地端点必须禁用代理，理由见 `build_client`
            client: build_client(is_local_endpoint(&base_url)),
            base_url,
            model: model.into(),
            api_key,
        }
    }

    /// 聊天补全端点。
    pub fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// 模型列表端点（连接自检用）。
    pub fn models_url(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    /// 列出端点上的模型（自检第一步）。
    async fn list_models(&self) -> Result<Vec<String>, AiError> {
        let mut builder = self.client.get(self.models_url()).timeout(Duration::from_secs(10));
        if let Some(key) = &self.api_key
            && !key.trim().is_empty()
        {
            builder = builder.bearer_auth(key);
        }

        let resp = builder.send().await.map_err(|e| {
            if e.is_connect() {
                AiError::Connection(format!(
                    "无法连接到 {}（服务未启动或地址错误）",
                    self.base_url
                ))
            } else {
                AiError::Connection(e.to_string())
            }
        })?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(map_status(status, &text));
        }
        Ok(parse_model_list(&text))
    }

    /// 极短推理探针（自检第二步）：一句话、最小 `max_tokens`。
    ///
    /// 🔴 不开 `json_mode`：部分自建端点不认 `response_format`，
    /// 开了会把"可用"误报成"不可用"——假红和假绿一样有害。
    async fn probe_completion(&self) -> Result<(), AiError> {
        let req = CompletionRequest::default()
            .with_messages(vec![ChatMessage::user("ping")])
            .temperature(0.0)
            .max_tokens(8);

        // 🔴 外面再包一层短超时：`complete` 的默认上限是 60s，
        // 但自检是"点一下就要看到结果"的交互，等一分钟等于卡死界面。
        // `/models` 正常而 `/chat/completions` 挂起（网关配错）时真会发生。
        match tokio::time::timeout(Duration::from_secs(PROBE_TIMEOUT_SECS), self.complete(&req)).await
        {
            Ok(r) => r.map(|_| ()),
            Err(_) => Err(AiError::Timeout(PROBE_TIMEOUT_SECS)),
        }
    }
}

/// 推理探针的超时上限（秒）。
const PROBE_TIMEOUT_SECS: u64 = 30;

/// 是否为本地回环端点。
///
/// 判定依据是 **host** 而非"有没有 api_key"：LM Studio 同样无需密钥，
/// 但它也是本地服务，同样不能被代理劫持。
pub(crate) fn is_local_endpoint(base_url: &str) -> bool {
    // 去掉 scheme → 取 host:port → 去 userinfo → 去端口
    let authority = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let host = authority
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority)
        .to_lowercase();
    // IPv6 字面量带方括号：[::1]:1234
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.split(']').next())
        .unwrap_or_else(|| host.rsplit_once(':').map(|(h, _)| h).unwrap_or(&host));
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0")
}

/// 构造 HTTP 客户端。
///
/// 🔴 **本地端点禁用代理**（`no_proxy()`），云端沿用系统代理。
///
/// 这不是洁癖，是一个会真实发生的故障：reqwest 默认继承
/// `HTTP_PROXY`/`HTTPS_PROXY` 环境变量，而公司网络里这两个变量几乎总被设置。
/// 结果是用户明明已经 `ollama serve` 起好了本地服务，请求却被送进代理，
/// 得到 `502 upstream connect failed: 由于目标计算机积极拒绝，无法连接`——
/// 错误信息还指向一个用户从未配置过的地址，几乎无法自查。
///
/// 附带的安全收益：本地模型的请求内容（含项目代码摘要）不该无谓地流经代理。
fn build_client(local: bool) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        // 连接池复用：每次新建 client 会重做 TLS 握手，连续多次分析时延迟翻倍
        .pool_idle_timeout(Duration::from_secs(90));
    let builder = if local { builder.no_proxy() } else { builder };
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// 本地服务专用客户端（禁用代理）。
///
/// pub(crate)：Ollama provider 复用同一份配置，
/// 避免"本地端点禁用代理"这条规则在两处各写一遍而日后只改一处。
pub(crate) fn build_local_client() -> reqwest::Client {
    build_client(true)
}

/// 请求体（wire 格式）。
///
/// 独立结构体而非 `serde_json::json!` 宏：
/// 字段名拼错时宏不报错，只会在运行时得到 400；
/// 结构体 + serde rename 让协议契约显式且可单测。
#[derive(Debug, Serialize)]
struct ChatRequestBody<'a> {
    model: &'a str,
    messages: &'a [WireMessage],
    /// f64 而非 f32：`serde_json::to_value` 会把 f32 提升为 f64，
    /// `0.3f32` 变成 `0.30000001192092896` 并原样发到 wire 上，
    /// 污染请求体、审计日志与调试输出。温度只需 2~3 位小数，
    /// 构造时四舍五入到 3 位（见 `rounded_temperature`）。
    temperature: f64,
    max_tokens: u32,
    /// 仅在需要时出现：部分自建端点不认识 `response_format`，
    /// 无条件发送会让它们直接 400。
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

/// `response_format` 字段值。
#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

/// wire 格式的消息。
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct WireMessage {
    role: String,
    content: String,
}

/// 响应体（wire 格式）。
///
/// `#[serde(default)]` 是必需的：不同 provider 返回的字段子集不同
/// （自建端点常缺 `usage`），缺字段不该导致整个响应解析失败。
#[derive(Debug, Deserialize)]
struct ChatResponseBody {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
    /// 错误响应：部分 provider 用 200 + error 字段表达失败
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Debug, Deserialize)]
struct WireChoice {
    #[serde(default)]
    message: Option<WireMessage>,
    /// 流式或简化实现可能只有 text
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct WireError {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

/// 构造请求体 JSON（纯函数，可单测）。
pub fn build_request_body(model: &str, req: &CompletionRequest) -> serde_json::Value {
    let messages: Vec<WireMessage> = req
        .messages
        .iter()
        .map(|m| WireMessage {
            role: m.role.as_str().to_string(),
            content: m.content.clone(),
        })
        .collect();

    let body = ChatRequestBody {
        model,
        messages: &messages,
        temperature: req.temperature_f64(),
        max_tokens: req.max_tokens,
        response_format: req.json_mode.then_some(ResponseFormat { kind: "json_object" }),
    };
    // 序列化一个已知结构体不会失败；失败说明 serde 派生写错了，属编程错误
    serde_json::to_value(body).unwrap_or_else(|_| serde_json::json!({}))
}

/// 解析响应体（纯函数，可单测）。
///
/// 覆盖多种 provider 的实际差异：
/// - 标准：`choices[0].message.content`
/// - 简化：`choices[0].text`
/// - 200 但带 `error` 字段（部分自建网关的行为）
pub fn parse_response(
    body: &str,
    fallback_model: &str,
    took_ms: u64,
) -> Result<CompletionResponse, AiError> {
    let parsed: ChatResponseBody = serde_json::from_str(body)
        .map_err(|e| AiError::MalformedResponse(format!("响应不是合法 JSON: {e}")))?;

    // 200 + error 字段：必须当失败处理，否则会把错误信息当成模型输出展示给用户
    if let Some(err) = parsed.error {
        let msg = err.message.unwrap_or_else(|| "未知错误".to_string());
        return Err(classify_error_message(&msg, err.code.as_deref()));
    }

    let text = parsed
        .choices
        .first()
        .and_then(|c| c.message.as_ref().map(|m| m.content.clone()).or_else(|| c.text.clone()))
        .unwrap_or_default();

    if text.trim().is_empty() {
        // 空输出不是"成功但没内容"：上层会拿它去解析 JSON 然后失败，
        // 报错信息会变得完全看不出原因。在这里就明确失败。
        return Err(AiError::MalformedResponse(
            "模型返回了空内容（可能被内容过滤或 max_tokens 过小）".to_string(),
        ));
    }

    Ok(CompletionResponse {
        text,
        model: parsed.model.unwrap_or_else(|| fallback_model.to_string()),
        prompt_tokens: parsed.usage.as_ref().and_then(|u| u.prompt_tokens),
        completion_tokens: parsed.usage.as_ref().and_then(|u| u.completion_tokens),
        took_ms,
    })
}

/// 把 HTTP 状态码映射成 [`AiError`]（纯函数，可单测）。
///
/// 🔴 401 必须映射成 `Unauthorized` 而非泛化的 `Provider`：
/// 设置页要据此提示"API Key 无效"，而不是让用户去猜网络问题。
pub fn map_status(status: u16, body: &str) -> AiError {
    match status {
        401 | 403 => AiError::Unauthorized,
        429 => AiError::RateLimited,
        408 => AiError::Timeout(DEFAULT_TIMEOUT_FALLBACK),
        s if s >= 500 => AiError::Provider(format!("服务端错误 {s}: {}", brief_message(body))),
        s => AiError::Provider(format!("请求被拒绝 ({s}): {}", brief_message(body))),
    }
}

/// 状态码映射超时错误时使用的秒数（无法从响应得知真实配置）。
const DEFAULT_TIMEOUT_FALLBACK: u64 = 120;

/// 从错误消息文本推断错误类型（部分 provider 用 200 + error 表达失败）。
fn classify_error_message(msg: &str, code: Option<&str>) -> AiError {
    let lower = msg.to_lowercase();
    let code_lower = code.unwrap_or_default().to_lowercase();
    if lower.contains("incorrect api key") || lower.contains("invalid api key") || code_lower == "invalid_api_key" {
        return AiError::Unauthorized;
    }
    if lower.contains("rate limit") || lower.contains("too many requests") || code_lower == "rate_limit_exceeded" {
        return AiError::RateLimited;
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return AiError::Timeout(DEFAULT_TIMEOUT_FALLBACK);
    }
    AiError::Provider(brief_message(msg))
}

/// 截断长错误体，避免把整页 HTML 错误页塞进用户提示。
///
/// pub(crate)：Ollama provider 复用同一套截断规则，
/// 保证两个后端的错误提示长度一致（否则用户会困惑于同样故障提示详略不一）。
pub(crate) fn brief_message(body: &str) -> String {
    let t = body.trim();
    let head: String = t.chars().take(180).collect();
    if t.chars().count() > 180 {
        format!("{head}…")
    } else {
        head
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &'static str {
        PROVIDER_NAME
    }

    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, AiError> {
        req.validate()?;
        let started = Instant::now();
        let body = build_request_body(&self.model, req);

        let mut builder = self
            .client
            .post(self.chat_url())
            .timeout(Duration::from_secs(timeout_for(req.json_mode)))
            .json(&body);
        if let Some(key) = &self.api_key
            && !key.trim().is_empty()
        {
            builder = builder.bearer_auth(key);
        }

        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                AiError::Timeout(timeout_for(req.json_mode))
            } else if e.is_connect() {
                AiError::Connection(format!(
                    "无法连接到 {}（请确认服务已启动、地址正确）",
                    self.base_url
                ))
            } else {
                AiError::Connection(e.to_string())
            }
        })?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(map_status(status, &text));
        }
        parse_response(&text, &self.model, started.elapsed().as_millis() as u64)
    }

    async fn health_check(&self) -> Result<ProviderHealth, AiError> {
        let started = Instant::now();

        // 第一步：列模型——探端点连通性与鉴权。
        // 失败就直接 `Err`：此时连模型列表都拿不到，前端也没有别的可推荐。
        let listed = self.list_models().await?;

        // 第二步：真实推理探针。
        //
        // 🔴 只做第一步会**假绿**：`/models` 返回的是端点上*存在*哪些模型，
        // 不代表当前配置的这个*能被调用*。阿里云百炼的专属工作空间就是活例子——
        // `ZHIPU/GLM-5.3-FlashX` 明明在 `/models` 列表里，
        // 打 `/chat/completions` 却返回 400 `InvalidParameter:
        // The product is not activated`（该模型未在此工作空间开通）。
        // 同一把 key、同一端点上 `glm-5.3`、`qwen-plus` 全部 200 正常。
        // 于是设置页显示绿灯「已连接」，用户一点「生成画像」就炸，且无从自查。
        //
        // 探针复用 `complete()` 而不是另写一遍请求：自检必须与真实调用
        // 共用同一条代码路径，否则两者会随时间分裂，又变回假绿。
        let probed = self.probe_completion().await;
        let latency_ms = started.elapsed().as_millis() as u64;

        Ok(match probed {
            Ok(()) => ProviderHealth {
                ok: true,
                message: format!("已连接 {}（推理探针通过）", self.model),
                models: listed,
                latency_ms,
            },
            // 🔴 用 `Ok(ok=false)` 而非 `Err`：模型列表必须留着。
            // 前端在 `ok=false` 且列表非空时会显示"后端探测到这些模型，
            // 是否想用其中之一？"的可点击 chips——正是用户此刻唯一的出路。
            // 返回 `Err` 会把列表丢掉，用户只看到红灯却不知道能换成什么。
            Err(err) => ProviderHealth {
                ok: false,
                message: probe_failure_message(&err, &self.model),
                models: listed,
                latency_ms,
            },
        })
    }
}

/// 把探针失败翻译成**可操作**的提示（纯函数，可单测）。
///
/// 🔴 每条都必须点名模型：用户要能立刻判断是"这个模型"的问题，
/// 而不是端点或密钥的问题——否则会去反复重填本来有效的 API Key。
pub fn probe_failure_message(err: &AiError, model: &str) -> String {
    match err {
        AiError::Provider(msg) if mentions_not_activated(msg) => format!(
            "模型 {model} 在该端点上未开通：服务方返回「product is not activated」。\
             请到服务商控制台为该工作空间开通此模型，或从下方列表换一个已开通的。"
        ),
        AiError::Provider(msg) if mentions_model_not_found(msg) => format!(
            "该端点上不存在模型 {model}。请检查模型名拼写，或从下方列表选一个。"
        ),
        AiError::Unauthorized => format!(
            "API Key 无效，或无权调用模型 {model}。请在 设置 → 大模型配置 中重新填写。"
        ),
        AiError::RateLimited => format!("模型 {model} 被限流，请稍后重试或换一个模型。"),
        AiError::Timeout(s) => format!("模型 {model} 推理超时（{s}s）：端点可达但未在时限内返回。"),
        other => format!("模型 {model} 无法完成推理：{other}"),
    }
}

/// 服务方是否在说"该模型/产品未开通"。各家措辞不一，但都落在 activated / 开通 上。
fn mentions_not_activated(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("not activated")
        || lower.contains("not been activated")
        || msg.contains("未开通")
}

/// 服务方是否在说"模型不存在"。
fn mentions_model_not_found(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("model_not_found")
        || lower.contains("model not found")
        || lower.contains("does not exist")
        || msg.contains("模型不存在")
}

/// 从 `/models` 响应里提取模型名（纯函数，可单测）。
///
/// 不同实现的外层结构不同（`{"data":[{"id":…}]}` 或裸数组），
/// 两种都支持；解析不出就返回空列表——自检成功与否不取决于此。
pub fn parse_model_list(body: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct List {
        #[serde(default)]
        data: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default)]
        id: Option<String>,
    }

    if let Ok(list) = serde_json::from_str::<List>(body) {
        let names: Vec<String> = list.data.into_iter().filter_map(|i| i.id).collect();
        if !names.is_empty() {
            return names;
        }
    }
    if let Ok(arr) = serde_json::from_str::<Vec<Item>>(body) {
        return arr.into_iter().filter_map(|i| i.id).collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage, Role};

    fn req() -> CompletionRequest {
        CompletionRequest::default()
            .with_messages(vec![
                ChatMessage::system("你是代码分析助手"),
                ChatMessage::user("这个项目在做什么？"),
            ])
            .temperature(0.3)
            .max_tokens(1024)
    }

    // ── 请求构造 ─────────────────────────────────────────────────

    #[test]
    fn request_body_has_required_fields() {
        let body = build_request_body("gpt-5-mini", &req());
        assert_eq!(body["model"], "gpt-5-mini");
        assert_eq!(body["temperature"], 0.3);
        assert_eq!(body["max_tokens"], 1024);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "你是代码分析助手");
        assert_eq!(msgs[1]["role"], "user");
    }

    /// `response_format` 只在 json_mode 时出现：
    /// 部分自建端点不认识该字段，无条件发送会直接 400。
    #[test]
    fn response_format_only_in_json_mode() {
        let normal = build_request_body("m", &req());
        assert!(
            normal.get("response_format").is_none(),
            "非 JSON 模式不该发送 response_format"
        );

        let json = build_request_body("m", &req().json_mode(true));
        assert_eq!(json["response_format"]["type"], "json_object");
    }

    #[test]
    fn request_body_serializes_to_valid_json() {
        let body = build_request_body("qwen3:8b", &req());
        let text = serde_json::to_string(&body).unwrap();
        assert!(text.contains("\"model\":\"qwen3:8b\""));
        assert!(text.contains("\"messages\""));
        // 中文内容必须原样保留（不得被转义成 \uXXXX 导致本地模型误读）
        assert!(text.contains("你是代码分析助手"), "实际: {text}");
    }

    // ── 响应解析 ─────────────────────────────────────────────────

    #[test]
    fn parses_standard_openai_response() {
        let body = r#"{
            "model": "gpt-5-mini-2026",
            "choices": [{"message": {"role":"assistant","content":"这是一个视频生成管线"}}],
            "usage": {"prompt_tokens": 120, "completion_tokens": 45}
        }"#;
        let r = parse_response(body, "gpt-5-mini", 830).unwrap();
        assert_eq!(r.text, "这是一个视频生成管线");
        assert_eq!(r.model, "gpt-5-mini-2026", "应优先用响应里的实际模型名");
        assert_eq!(r.total_tokens(), Some(165));
        assert_eq!(r.took_ms, 830);
    }

    /// 简化实现只给 `text` 不给 `message`，必须同样能解析。
    #[test]
    fn parses_text_only_choice() {
        let body = r#"{"choices":[{"text":"直接文本"}]}"#;
        let r = parse_response(body, "m", 10).unwrap();
        assert_eq!(r.text, "直接文本");
        assert_eq!(r.model, "m", "响应无 model 时回退到请求的模型名");
    }

    /// provider 缺字段是常态（自建端点常不返回 usage），不该整体失败。
    #[test]
    fn tolerates_missing_optional_fields() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
        let r = parse_response(body, "m", 5).unwrap();
        assert_eq!(r.text, "ok");
        assert_eq!(r.total_tokens(), None, "usage 缺失应为 None 而非 0");
    }

    /// 空输出必须报错：否则上层拿空串去解析 JSON，
    /// 报错信息会变成"JSON 解析失败"，完全看不出是模型没返回内容。
    #[test]
    fn empty_content_is_an_error() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"   "}}]}"#;
        let err = parse_response(body, "m", 5).unwrap_err();
        assert!(matches!(err, AiError::MalformedResponse(_)));
        assert!(err.to_string().contains("空内容"));
    }

    #[test]
    fn no_choices_is_an_error() {
        let err = parse_response(r#"{"choices":[]}"#, "m", 5).unwrap_err();
        assert!(matches!(err, AiError::MalformedResponse(_)));
    }

    #[test]
    fn malformed_json_is_reported() {
        let err = parse_response("<html>502 Bad Gateway</html>", "m", 5).unwrap_err();
        assert!(matches!(err, AiError::MalformedResponse(_)));
        assert!(err.to_string().contains("JSON"));
    }

    /// 🔴 部分网关用 HTTP 200 + error 字段表达失败。
    /// 若不当失败处理，错误信息会被当成模型输出展示给用户。
    #[test]
    fn error_field_in_200_response_is_failure() {
        let body = r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key"}}"#;
        let err = parse_response(body, "m", 5).unwrap_err();
        assert!(matches!(err, AiError::Unauthorized), "应识别为认证失败: {err:?}");
    }

    #[test]
    fn error_field_rate_limit_is_recognized() {
        let body = r#"{"error":{"message":"Rate limit exceeded for requests"}}"#;
        assert!(matches!(
            parse_response(body, "m", 5).unwrap_err(),
            AiError::RateLimited
        ));
    }

    // ── HTTP 状态映射 ────────────────────────────────────────────

    #[test]
    fn status_maps_to_actionable_errors() {
        assert!(matches!(map_status(401, ""), AiError::Unauthorized));
        assert!(matches!(map_status(403, ""), AiError::Unauthorized));
        assert!(matches!(map_status(429, ""), AiError::RateLimited));
        assert!(matches!(map_status(500, "boom"), AiError::Provider(_)));
        assert!(matches!(map_status(503, ""), AiError::Provider(_)));
        assert!(matches!(map_status(404, "not found"), AiError::Provider(_)));
    }

    /// 401 的提示必须直接说"API Key"，否则用户会去排查网络。
    #[test]
    fn unauthorized_message_mentions_api_key() {
        let msg = map_status(401, "").to_string();
        assert!(msg.contains("API Key"), "实际: {msg}");
    }

    /// 长错误体（例如整页 HTML 错误页）必须被截断。
    #[test]
    fn long_error_body_is_truncated() {
        let html = "<html>".to_string() + &"x".repeat(5000) + "</html>";
        let msg = map_status(502, &html).to_string();
        assert!(msg.chars().count() < 300, "错误提示过长: {} 字", msg.chars().count());
        assert!(msg.contains('…'));
    }

    /// 所有 AI 错误都应是可展示的中文，不含内部实现细节。
    #[test]
    fn errors_are_user_facing_chinese() {
        for err in [
            map_status(401, ""),
            map_status(429, ""),
            map_status(500, "internal"),
            AiError::Connection("无法连接".into()),
        ] {
            let msg = err.to_string();
            assert!(!msg.is_empty());
            assert!(!msg.contains("reqwest"), "不应泄漏内部库名: {msg}");
        }
    }

    // ── 模型列表解析 ─────────────────────────────────────────────

    #[test]
    fn parses_openai_style_model_list() {
        let body = r#"{"object":"list","data":[{"id":"gpt-5-mini"},{"id":"gpt-5"}]}"#;
        assert_eq!(parse_model_list(body), vec!["gpt-5-mini", "gpt-5"]);
    }

    #[test]
    fn parses_bare_array_model_list() {
        let body = r#"[{"id":"qwen3:8b"},{"id":"llama3"}]"#;
        assert_eq!(parse_model_list(body), vec!["qwen3:8b", "llama3"]);
    }

    /// 解析不出模型列表不该让自检失败（有些端点不提供 /models 详情）。
    #[test]
    fn unparsable_model_list_is_empty_not_error() {
        assert!(parse_model_list("{}").is_empty());
        assert!(parse_model_list("not json").is_empty());
        assert!(parse_model_list(r#"{"data":[]}"#).is_empty());
    }

    // ── URL 构造 ─────────────────────────────────────────────────

    #[test]
    fn urls_are_built_from_base() {
        let p = OpenAiCompatibleProvider::new("https://api.example.com/v1", "m", None);
        assert_eq!(p.chat_url(), "https://api.example.com/v1/chat/completions");
        assert_eq!(p.models_url(), "https://api.example.com/v1/models");
    }

    /// 尾部斜杠不得产生双斜杠路径（部分网关对此返回 404）。
    #[test]
    fn trailing_slash_does_not_double() {
        let p = OpenAiCompatibleProvider::new("https://api.example.com/v1/", "m", None);
        assert_eq!(p.chat_url(), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn provider_name_is_stable() {
        let p = OpenAiCompatibleProvider::new("http://x", "m", None);
        assert_eq!(p.name(), PROVIDER_NAME);
        assert_eq!(PROVIDER_NAME, "openai-compatible");
    }

    // ── 无网络下的失败路径（不依赖外部服务）──────────────────────

    /// 连不上的地址必须给出可操作提示，而不是底层 socket 错误。
    #[tokio::test]
    async fn unreachable_endpoint_reports_actionable_error() {
        // 127.0.0.1:1 几乎不可能有服务在听
        let p = OpenAiCompatibleProvider::new("http://127.0.0.1:1/v1", "m", None);
        let err = p.complete(&req()).await.unwrap_err();
        assert!(
            matches!(err, AiError::Connection(_)),
            "应为连接错误，实际 {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("无法连接") || msg.contains("连接"),
            "提示应说明是连接问题: {msg}"
        );
        assert!(err.is_degradable(), "连接失败应允许降级到确定性回答");
    }

    #[tokio::test]
    async fn health_check_on_unreachable_endpoint_fails_cleanly() {
        let p = OpenAiCompatibleProvider::new("http://127.0.0.1:1/v1", "m", None);
        let err = p.health_check().await.unwrap_err();
        assert!(matches!(err, AiError::Connection(_)));
    }

    /// 空消息请求应在**发网络请求之前**就被挡掉（省一次往返）。
    #[tokio::test]
    async fn invalid_request_fails_before_network() {
        let p = OpenAiCompatibleProvider::new("http://127.0.0.1:1/v1", "m", None);
        let started = Instant::now();
        let err = p.complete(&CompletionRequest::default()).await.unwrap_err();
        assert!(matches!(err, AiError::MalformedResponse(_)));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "校验失败应立即返回，不该等网络超时"
        );
    }

    #[test]
    fn wire_message_roundtrips() {
        let m = WireMessage {
            role: Role::Assistant.as_str().to_string(),
            content: "hi".into(),
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: WireMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    // ── 回归守护：本地端点识别（代理劫持 bug）──────────────────────
    //
    // 🔴 这两个 bug 都是**静默失效**型：编译、测试都不报错，
    // 只在用户机器上（设了 HTTP_PROXY 的公司网络）表现为
    // "Ollama 明明起着，却报 502 upstream connect failed"。
    // 没有测试守护，下次重构极易把它们改回去。

    #[test]
    fn local_endpoints_are_recognized() {
        for url in [
            "http://127.0.0.1:11434",
            "http://localhost:1234",
            "http://localhost:1234/v1",
            "http://127.0.0.1:8080/v1/",
            "http://0.0.0.0:11434",
            "http://[::1]:11434",
            // 大写 host 也必须识别（Windows 上环境变量常是大写）
            "http://LOCALHOST:1234/v1",
        ] {
            assert!(is_local_endpoint(url), "应识别为本地端点: {url}");
        }
    }

    /// 云端端点绝不能被误判为本地——否则云端请求会绕过公司代理，
    /// 在需要代理才能出网的环境里直接连不上。
    #[test]
    fn cloud_endpoints_are_not_local() {
        for url in [
            "https://api.openai.com/v1",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "http://192.168.1.50:11434", // 局域网另一台机器的 Ollama：不是回环
            "http://ollama.internal.corp/v1",
        ] {
            assert!(!is_local_endpoint(url), "不应判为本地: {url}");
        }
    }

    /// host 里含 "localhost" 子串但并非回环时不得误判。
    #[test]
    fn substring_lookalikes_are_not_local() {
        assert!(!is_local_endpoint("https://notlocalhost.example.com/v1"));
        assert!(!is_local_endpoint("https://localhost.example.com/v1"));
        // 带 userinfo 的形式：host 在 @ 之后
        assert!(is_local_endpoint("http://user:pass@127.0.0.1:11434/v1"));
        assert!(!is_local_endpoint("http://user@10.0.0.5:11434/v1"));
    }

    /// 本地 provider 构造出的 client 必须真的禁用了代理。
    /// reqwest 不暴露代理配置，这里通过"构造不 panic + 端点判定正确"间接守护，
    /// 并直接断言 Ollama（恒本地）与 LM Studio（本地）走的都是 no_proxy 分支。
    #[test]
    fn local_provider_construction_uses_no_proxy_path() {
        // 这两条构造路径内部都会调用 build_client(true)
        let _ollama = crate::ollama::OllamaProvider::new("http://127.0.0.1:11434", "qwen3:8b");
        let lmstudio = OpenAiCompatibleProvider::new("http://localhost:1234/v1", "m", None);
        assert!(is_local_endpoint(&lmstudio.base_url));
        // 云端构造走 build_client(false)，保留系统代理
        let cloud = OpenAiCompatibleProvider::new("https://api.example.com/v1", "m", Some("sk-x".into()));
        assert!(!is_local_endpoint(&cloud.base_url));
    }

    // ── 回归守护：温度精度（f32→f64 提升 bug）─────────────────────

    /// 🔴 温度必须以干净的十进制出现在请求体里。
    /// `0.3f32 as f64` 是 `0.30000001192092896`，直接发出去会污染
    /// 请求体、审计日志，也让协议快照测试无法比对。
    #[test]
    fn temperature_is_serialized_cleanly() {
        let body = build_request_body("m", &req());
        let t = body["temperature"].as_f64().unwrap();
        assert!((t - 0.3).abs() < 1e-9, "温度应精确为 0.3，实际 {t}");
        // 序列化后的文本里也不得出现长尾小数
        let text = serde_json::to_string(&body).unwrap();
        assert!(
            !text.contains("0.30000001"),
            "请求体含 f32 提升产生的长尾小数: {text}"
        );
    }

    #[test]
    fn temperature_rounding_covers_common_values() {
        for (input, expected) in [
            (0.0_f32, 0.0_f64),
            (0.2, 0.2),
            (0.7, 0.7),
            (1.0, 1.0),
            (0.33333, 0.333), // 四舍五入到 3 位
        ] {
            let r = CompletionRequest::default().temperature(input);
            assert!(
                (r.temperature_f64() - expected).abs() < 1e-9,
                "温度 {input} 应为 {expected}，实际 {}",
                r.temperature_f64()
            );
        }
    }

    /// NaN 温度必须回退到默认值，否则 provider 会 400。
    #[test]
    fn nan_temperature_falls_back_to_default() {
        let r = CompletionRequest::default().temperature(f32::NAN);
        assert!(!r.temperature.is_nan(), "构造时就该被钳掉");
        assert!(!r.temperature_f64().is_nan());
    }

    // ── 连接自检：推理探针 ───────────────────────────────────────
    //
    // 这组测试对应一个真实线上故障：设置页「测试连接」报绿灯
    // 「已连接 ZHIPU/GLM-5.3-FlashX」，用户点「生成画像」却得到
    // 400 InvalidParameter: The product is not activated。
    // 成因是旧的 health_check 只 GET /models——列表里*有*这个模型，
    // 但该模型在这个百炼工作空间上*未开通*，压根不能推理。

    use crate::teststub::{
        StubServer, CHAT_OK_BODY, MODELS_WITH_UNUSABLE_BODY, NOT_ACTIVATED_BODY,
    };

    /// 🔴 核心回归：端点通 + 模型在列表里，但推理被拒 ⇒ 自检必须报**失败**。
    ///
    /// 这条测试就是本 bug 的最小复现。改回"只列模型"的旧实现，它必然变红。
    #[tokio::test]
    async fn health_check_fails_when_model_cannot_reason() {
        let stub = StubServer::start(vec![
            ("/models", 200, MODELS_WITH_UNUSABLE_BODY),
            ("/chat/completions", 400, NOT_ACTIVATED_BODY),
        ]);
        let p = OpenAiCompatibleProvider::new(
            stub.base_url.clone(),
            "ZHIPU/GLM-5.3-FlashX",
            Some("sk-test".into()),
        );

        // 注意是 Ok(health) 而非 Err：模型列表要留给前端展示可选项
        let h = p.health_check().await.expect("不该是传输层错误");
        assert!(!h.ok, "模型不能推理时自检必须报失败，绝不能假绿");
        assert!(
            h.message.contains("ZHIPU/GLM-5.3-FlashX"),
            "提示必须点名是哪个模型，否则用户会去重填本来有效的 API Key：{}",
            h.message
        );
        assert!(
            h.message.contains("未开通"),
            "应翻译成可操作的中文提示，而非原样抛出服务方英文：{}",
            h.message
        );
    }

    /// 自检失败时**必须保留模型列表**：那是用户唯一的出路。
    ///
    /// 前端在 `ok=false` 且列表非空时显示"后端探测到这些模型，
    /// 是否想用其中之一？"的可点击 chips。丢了列表，用户只看到红灯。
    #[tokio::test]
    async fn health_check_keeps_model_list_when_probe_fails() {
        let stub = StubServer::start(vec![
            ("/models", 200, MODELS_WITH_UNUSABLE_BODY),
            ("/chat/completions", 400, NOT_ACTIVATED_BODY),
        ]);
        let p = OpenAiCompatibleProvider::new(stub.base_url.clone(), "ZHIPU/GLM-5.3-FlashX", None);

        let h = p.health_check().await.expect("不该是传输层错误");
        assert!(!h.ok);
        assert!(
            h.models.contains(&"glm-5.3".to_string()),
            "自检失败也要把可选模型带回来，实际：{:?}",
            h.models
        );
    }

    /// 探针必须**真的发出去**：只列模型不推理 = 假绿。
    ///
    /// 这条测试盯住"探针被优化掉"的回归——若有人觉得多一次请求太慢
    /// 而删掉它，这里会立刻失败。
    #[tokio::test]
    async fn health_check_actually_probes_chat_endpoint() {
        let stub = StubServer::start(vec![
            ("/models", 200, MODELS_WITH_UNUSABLE_BODY),
            ("/chat/completions", 200, CHAT_OK_BODY),
        ]);
        let p = OpenAiCompatibleProvider::new(stub.base_url.clone(), "glm-5.3", None);

        let h = p.health_check().await.expect("两步都该成功");
        assert!(h.ok, "推理通过时应报绿：{}", h.message);
        assert!(
            stub.hit("/chat/completions"),
            "自检必须真的打一次推理端点，只列模型不够"
        );
        assert!(stub.hit("/models"), "列表仍要拉取，前端要靠它推荐可选模型");
    }

    /// 推理正常 ⇒ 报绿，且延迟是两步总耗时。
    #[tokio::test]
    async fn health_check_passes_when_probe_succeeds() {
        let stub = StubServer::start(vec![
            ("/models", 200, MODELS_WITH_UNUSABLE_BODY),
            ("/chat/completions", 200, CHAT_OK_BODY),
        ]);
        let p = OpenAiCompatibleProvider::new(stub.base_url.clone(), "qwen-plus", None);

        let h = p.health_check().await.expect("应成功");
        assert!(h.ok);
        assert!(h.message.contains("qwen-plus"), "应点名模型：{}", h.message);
        assert_eq!(h.models.len(), 3);
    }

    /// 端点整体不可达 ⇒ 仍是 `Err`（连模型列表都没有，前端无从推荐）。
    ///
    /// 与"推理被拒"严格区分：后者用 `Ok(ok=false)` 保住列表。
    #[tokio::test]
    async fn health_check_errors_when_models_endpoint_down() {
        let p = OpenAiCompatibleProvider::new("http://127.0.0.1:1/v1", "m", None);
        let err = p.health_check().await.unwrap_err();
        assert!(matches!(err, AiError::Connection(_)), "实际：{err:?}");
    }

    /// 鉴权失败 ⇒ 提示应指向 API Key 而非"模型有问题"。
    #[tokio::test]
    async fn health_check_reports_key_problem_on_401() {
        let stub = StubServer::start(vec![
            ("/models", 200, MODELS_WITH_UNUSABLE_BODY),
            ("/chat/completions", 401, r#"{"error":{"message":"Incorrect API key provided"}}"#),
        ]);
        let p = OpenAiCompatibleProvider::new(stub.base_url.clone(), "glm-5.3", Some("sk-bad".into()));

        let h = p.health_check().await.expect("不该是传输层错误");
        assert!(!h.ok);
        assert!(
            h.message.contains("API Key"),
            "401 该引导用户检查密钥：{}",
            h.message
        );
    }

    // ── 探针提示文案（纯函数）──────────────────────────────────

    /// 各家措辞不一的"未开通"都要能被识别并翻译。
    #[test]
    fn probe_message_recognizes_not_activated_variants() {
        for raw in [
            "The product is not activated, please confirm that you have activated products",
            "Model has not been activated for this workspace",
            "该模型未开通，请前往控制台开通",
        ] {
            let msg = probe_failure_message(&AiError::Provider(raw.into()), "m1");
            assert!(msg.contains("未开通"), "应识别为未开通：{raw} → {msg}");
            assert!(msg.contains("m1"), "应点名模型：{msg}");
        }
    }

    /// "模型不存在"要给不同于"未开通"的引导（前者改拼写，后者去控制台）。
    #[test]
    fn probe_message_distinguishes_missing_model() {
        let msg = probe_failure_message(
            &AiError::Provider("The model `foo` does not exist".into()),
            "foo",
        );
        assert!(msg.contains("不存在"), "实际：{msg}");
        assert!(!msg.contains("未开通"), "不该误判成未开通：{msg}");
    }

    /// 其余错误也要点名模型，并保留原始错误信息（不吞掉线索）。
    #[test]
    fn probe_message_always_names_model() {
        let cases = vec![
            AiError::Provider("boom".into()),
            AiError::Unauthorized,
            AiError::RateLimited,
            AiError::Timeout(30),
            AiError::Connection("x".into()),
        ];
        for err in cases {
            let msg = probe_failure_message(&err, "some-model");
            assert!(msg.contains("some-model"), "应点名模型：{msg}");
            assert!(!msg.is_empty());
        }
    }
}
