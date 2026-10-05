//! Ollama 原生 provider（`/api/chat`、`/api/tags`）。
//!
//! # 为什么不直接复用 OpenAI-compatible
//! Ollama 确实在 `/v1` 暴露了兼容端点，但原生端点有三点实际优势：
//! 1. **无需鉴权头**：本地服务不接受 Bearer，多发一个空 header 反而可能 401
//! 2. **`/api/tags` 稳定可用**：列出已拉取的模型，连接自检能给出
//!    "已连接，本地有 3 个模型"这种有用结论，而不只是"端口通了"
//! 3. **响应字段更全**：`eval_count` 等用量信息在原生格式里才有
//!
//! `LocalBackend::LlamaCpp` / `LmStudio` 走 OpenAI-compatible（它们本就提供 `/v1`），
//! 只有 `Ollama` 用本模块——路由逻辑见 [`crate::router`]。

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use projectassests_domain::AiError;

use crate::provider::{
    timeout_for, CompletionRequest, CompletionResponse, LlmProvider, ProviderHealth,
};

/// 该后端在审计日志中的名字。
pub const PROVIDER_NAME: &str = "ollama";

/// Ollama 默认端口。
pub const DEFAULT_PORT: u16 = 11434;

/// Ollama provider。
#[derive(Debug, Clone)]
pub struct OllamaProvider {
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            // Ollama 恒为本地服务：客户端必须禁用代理，否则被 HTTP_PROXY 劫持，
            // 用户会看到"502 upstream connect failed"却查不出原因（见 openai::build_client）
            client: crate::openai::build_local_client(),
        }
    }

    pub fn chat_url(&self) -> String {
        format!("{}/api/chat", self.base_url.trim_end_matches('/'))
    }

    pub fn tags_url(&self) -> String {
        format!("{}/api/tags", self.base_url.trim_end_matches('/'))
    }

    /// 当前配置的模型名。
    pub fn model(&self) -> &str {
        &self.model
    }
}

/// 请求体（wire 格式）。
#[derive(Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [OllamaMessage],
    /// `false` 关闭流式：我们要一次性拿完整结果再解析 JSON，
    /// 流式需要额外的增量拼接逻辑且对结构化输出没有好处。
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
    /// 强制 JSON 输出（Ollama 原生支持，比在 prompt 里恳求可靠得多）
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<&'static str>,
}

/// 采样参数。
#[derive(Debug, Serialize)]
struct OllamaOptions {
    /// f64 + 四舍五入，理由同 `openai::ChatRequestBody::temperature`
    temperature: f64,
    num_predict: u32,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct OllamaMessage {
    role: String,
    content: String,
}

/// 响应体。
///
/// `#[serde(default)]`：Ollama 版本之间字段有增减，
/// 缺字段不该让整个响应解析失败（尤其 `message` 在错误响应里不存在）。
#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    #[serde(default)]
    message: Option<OllamaMessage>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// 输入 token 数
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    /// 输出 token 数
    #[serde(default)]
    eval_count: Option<u32>,
}

/// `/api/tags` 响应。
#[derive(Debug, Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaModel {
    #[serde(default)]
    name: Option<String>,
}

/// 构造请求体（纯函数，可单测）。
pub fn build_request_body(model: &str, req: &CompletionRequest) -> serde_json::Value {
    let messages: Vec<OllamaMessage> = req
        .messages
        .iter()
        .map(|m| OllamaMessage {
            role: m.role.as_str().to_string(),
            content: m.content.clone(),
        })
        .collect();

    let body = OllamaChatRequest {
        model,
        messages: &messages,
        stream: false,
        options: Some(OllamaOptions {
            temperature: req.temperature_f64(),
            num_predict: req.max_tokens,
        }),
        format: req.json_mode.then_some("json"),
    };
    serde_json::to_value(body).unwrap_or_else(|_| serde_json::json!({}))
}

/// 解析响应（纯函数，可单测）。
pub fn parse_response(
    body: &str,
    fallback_model: &str,
    took_ms: u64,
) -> Result<CompletionResponse, AiError> {
    let parsed: OllamaChatResponse = serde_json::from_str(body)
        .map_err(|e| AiError::MalformedResponse(format!("Ollama 响应不是合法 JSON: {e}")))?;

    // Ollama 用 200 + error 字段报告模型级错误（最常见：模型未拉取）
    if let Some(err) = parsed.error {
        return Err(classify_ollama_error(&err));
    }

    let text = parsed
        .message
        .map(|m| m.content)
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err(AiError::MalformedResponse(
            "模型返回了空内容（可能未加载成功或 num_predict 过小）".to_string(),
        ));
    }

    Ok(CompletionResponse {
        text,
        model: parsed.model.unwrap_or_else(|| fallback_model.to_string()),
        prompt_tokens: parsed.prompt_eval_count,
        completion_tokens: parsed.eval_count,
        took_ms,
    })
}

/// 把 Ollama 的错误文本映射成可操作的提示。
///
/// 🔴 "模型未拉取"必须给出**具体命令**：用户看到 `model not found` 不知所措，
/// 看到 `请先运行 ollama pull qwen3:8b` 就能立刻解决。
fn classify_ollama_error(err: &str) -> AiError {
    let lower = err.to_lowercase();
    if lower.contains("not found") || lower.contains("does not exist") || lower.contains("no model")
    {
        return AiError::Provider(format!(
            "本地模型不可用，请先拉取：ollama pull …（原始信息: {}）",
            err
        ));
    }
    if lower.contains("out of memory") || lower.contains("oom") {
        return AiError::Provider("本地显存/内存不足，请换用更小的模型".to_string());
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return AiError::Timeout(timeout_for(false));
    }
    AiError::Provider(crate::openai::brief_message(err))
}

/// 解析 `/api/tags`（纯函数，可单测）。
pub fn parse_tags(body: &str) -> Vec<String> {
    serde_json::from_str::<OllamaTags>(body)
        .map(|t| t.models.into_iter().filter_map(|m| m.name).collect())
        .unwrap_or_default()
}

#[async_trait::async_trait]
impl LlmProvider for OllamaProvider {
    fn name(&self) -> &'static str {
        PROVIDER_NAME
    }

    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, AiError> {
        req.validate()?;
        let started = Instant::now();
        let body = build_request_body(&self.model, req);

        let resp = self
            .client
            .post(self.chat_url())
            .timeout(Duration::from_secs(timeout_for(req.json_mode)))
            .json(&body)
            .send()
            .await
            .map_err(|e| map_transport_error(&e, &self.base_url))?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(crate::openai::map_status(status, &text));
        }
        parse_response(&text, &self.model, started.elapsed().as_millis() as u64)
    }

    async fn health_check(&self) -> Result<ProviderHealth, AiError> {
        let started = Instant::now();
        let resp = self
            .client
            .get(self.tags_url())
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| map_transport_error(&e, &self.base_url))?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(crate::openai::map_status(status, &text));
        }

        let models = parse_tags(&text);
        let latency_ms = started.elapsed().as_millis() as u64;
        // 配置的模型是否在本地存在：不在的话自检"成功"但一调用就失败，
        // 那是最让人困惑的状态，必须在这里就说清楚。
        let configured_present = models
            .iter()
            .any(|m| m == &self.model || m.starts_with(&format!("{}:", self.model)));

        // 🔴 `ok` 必须反映"这个模型能不能用"，而不是"Ollama 起没起来"。
        // 以前这里恒为 `true`：本地一个模型都没 pull、或缺少配置的那个，
        // 设置页照样显示绿灯「已连接」，用户点「生成画像」才炸。
        //
        // `/api/tags` 对 Ollama 是**权威**的（它就是已拉取模型的真身），
        // 所以模型缺失 = 必然调不通，无需再发一次推理探针。
        // 不像 OpenAI 兼容端点那样"列表里有≠能调用"（见 openai.rs 的探针）。
        //
        // 这里刻意**不**加推理探针：本地 7B 在 CPU 上冷启动加载可能远超
        // 任何合理的探针超时，会把"其实可用、只是慢"误报成红灯——
        // 假红和假绿一样有害。
        let ok = configured_present;

        Ok(ProviderHealth {
            ok,
            message: if models.is_empty() {
                format!("Ollama 已启动，但本地还没有任何模型（ollama pull {}）", self.model)
            } else if configured_present {
                format!("已连接，本地有 {} 个模型（含 {}）", models.len(), self.model)
            } else {
                format!(
                    "Ollama 已启动，但缺少配置的模型 {}（本地有: {}）。可执行 ollama pull {} 拉取",
                    self.model,
                    models.join(", "),
                    self.model
                )
            },
            models,
            latency_ms,
        })
    }
}

/// 传输层错误 → 面向用户的提示。
fn map_transport_error(e: &reqwest::Error, base_url: &str) -> AiError {
    if e.is_timeout() {
        return AiError::Timeout(timeout_for(false));
    }
    if e.is_connect() {
        // Ollama 未启动是本地场景最高频的故障，提示要直接给出启动方式
        return AiError::Connection(format!(
            "无法连接到 Ollama（{base_url}）。请确认已启动：ollama serve"
        ));
    }
    AiError::Connection(crate::openai::brief_message(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ChatMessage;

    fn req() -> CompletionRequest {
        CompletionRequest::default()
            .with_messages(vec![
                ChatMessage::system("你是助手"),
                ChatMessage::user("分析这个项目"),
            ])
            .temperature(0.2)
            .max_tokens(2048)
    }

    // ── 请求构造 ─────────────────────────────────────────────────

    #[test]
    fn request_body_uses_ollama_shape() {
        let body = build_request_body("qwen3:8b", &req());
        assert_eq!(body["model"], "qwen3:8b");
        assert_eq!(body["stream"], false, "必须关闭流式");
        // Ollama 用 options.num_predict 而非顶层 max_tokens
        assert_eq!(body["options"]["num_predict"], 2048);
        assert_eq!(body["options"]["temperature"], 0.2);
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][0]["role"], "system");
    }

    /// 顶层不该出现 OpenAI 的字段名，否则 Ollama 会忽略或报错。
    #[test]
    fn request_body_has_no_openai_fields() {
        let body = build_request_body("m", &req());
        assert!(body.get("max_tokens").is_none(), "那是 OpenAI 的字段名");
        assert!(body.get("response_format").is_none());
    }

    /// 🔴 回归守护：温度必须是干净的十进制。
    /// Ollama 走独立的 `options` 路径（而非顶层字段），
    /// 所以必须单独测——共用 `temperature_f64()` 不代表序列化位置也一样正确。
    #[test]
    fn temperature_in_options_is_serialized_cleanly() {
        let body = build_request_body("m", &req().temperature(0.3));
        let t = body["options"]["temperature"].as_f64().unwrap();
        assert!((t - 0.3).abs() < 1e-9, "实际 {t}");
        let text = serde_json::to_string(&body).unwrap();
        assert!(
            !text.contains("0.30000001"),
            "请求体含 f32 提升的长尾小数: {text}"
        );
    }

    #[test]
    fn json_mode_uses_ollama_format_field() {
        assert!(build_request_body("m", &req()).get("format").is_none());
        assert_eq!(build_request_body("m", &req().json_mode(true))["format"], "json");
    }

    // ── 响应解析 ─────────────────────────────────────────────────

    #[test]
    fn parses_ollama_response() {
        let body = r#"{
            "model": "qwen3:8b",
            "message": {"role":"assistant","content":"这是一个 FastAPI 后端"},
            "prompt_eval_count": 88,
            "eval_count": 31
        }"#;
        let r = parse_response(body, "qwen3:8b", 1200).unwrap();
        assert_eq!(r.text, "这是一个 FastAPI 后端");
        assert_eq!(r.model, "qwen3:8b");
        assert_eq!(r.total_tokens(), Some(119));
        assert_eq!(r.took_ms, 1200);
    }

    #[test]
    fn tolerates_missing_usage() {
        let body = r#"{"message":{"role":"assistant","content":"ok"}}"#;
        let r = parse_response(body, "m", 5).unwrap();
        assert_eq!(r.text, "ok");
        assert_eq!(r.model, "m");
        assert_eq!(r.total_tokens(), None);
    }

    /// 🔴 Ollama 用 200 + error 报告"模型未拉取"，
    /// 必须转成带具体命令的提示。
    #[test]
    fn missing_model_error_gives_pull_command() {
        let body = r#"{"error":"model 'qwen3:8b' not found, try pulling it first"}"#;
        let err = parse_response(body, "qwen3:8b", 5).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ollama pull"), "应给出拉取命令: {msg}");
        assert!(matches!(err, AiError::Provider(_)));
    }

    #[test]
    fn out_of_memory_error_is_explained() {
        let body = r#"{"error":"out of memory"}"#;
        let msg = parse_response(body, "m", 5).unwrap_err().to_string();
        assert!(msg.contains("内存") || msg.contains("显存"), "实际: {msg}");
    }

    #[test]
    fn empty_content_is_an_error() {
        let body = r#"{"message":{"role":"assistant","content":"  "}}"#;
        assert!(matches!(
            parse_response(body, "m", 5).unwrap_err(),
            AiError::MalformedResponse(_)
        ));
    }

    #[test]
    fn malformed_json_is_reported() {
        let err = parse_response("not json at all", "m", 5).unwrap_err();
        assert!(matches!(err, AiError::MalformedResponse(_)));
        assert!(err.to_string().contains("Ollama"));
    }

    // ── tags 解析 ────────────────────────────────────────────────

    #[test]
    fn parses_tags_response() {
        let body = r#"{"models":[{"name":"qwen3:8b"},{"name":"llama3:latest"}]}"#;
        assert_eq!(parse_tags(body), vec!["qwen3:8b", "llama3:latest"]);
    }

    #[test]
    fn tags_without_models_is_empty() {
        assert!(parse_tags(r#"{"models":[]}"#).is_empty());
        assert!(parse_tags("{}").is_empty());
        assert!(parse_tags("broken").is_empty());
    }

    // ── URL 与常量 ───────────────────────────────────────────────

    #[test]
    fn urls_use_native_endpoints() {
        let p = OllamaProvider::new("http://127.0.0.1:11434", "qwen3:8b");
        assert_eq!(p.chat_url(), "http://127.0.0.1:11434/api/chat");
        assert_eq!(p.tags_url(), "http://127.0.0.1:11434/api/tags");
        assert_eq!(p.model(), "qwen3:8b");
    }

    #[test]
    fn trailing_slash_is_normalized() {
        let p = OllamaProvider::new("http://127.0.0.1:11434/", "m");
        assert_eq!(p.chat_url(), "http://127.0.0.1:11434/api/chat");
    }

    #[test]
    fn provider_constants_are_stable() {
        assert_eq!(PROVIDER_NAME, "ollama");
        assert_eq!(DEFAULT_PORT, 11434);
        let p = OllamaProvider::new("http://x", "m");
        assert_eq!(p.name(), PROVIDER_NAME);
    }

    // ── 传输错误（无网络依赖）────────────────────────────────────

    /// Ollama 未启动是本地最高频故障，提示必须直接给出启动命令。
    #[tokio::test]
    async fn unreachable_ollama_suggests_serve() {
        let p = OllamaProvider::new("http://127.0.0.1:1", "qwen3:8b");
        let err = p.complete(&req()).await.unwrap_err();
        assert!(matches!(err, AiError::Connection(_)), "实际 {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("ollama serve"), "应提示启动命令: {msg}");
        assert!(err.is_degradable());
    }

    #[tokio::test]
    async fn health_check_fails_when_ollama_down() {
        let p = OllamaProvider::new("http://127.0.0.1:1", "m");
        let err = p.health_check().await.unwrap_err();
        assert!(matches!(err, AiError::Connection(_)));
        assert!(err.to_string().contains("ollama serve"));
    }

    /// 校验失败应在发请求前返回，不等网络超时。
    #[tokio::test]
    async fn invalid_request_short_circuits() {
        let p = OllamaProvider::new("http://127.0.0.1:1", "m");
        let started = Instant::now();
        assert!(matches!(
            p.complete(&CompletionRequest::default()).await.unwrap_err(),
            AiError::MalformedResponse(_)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn wire_message_roundtrips() {
        let m = OllamaMessage {
            role: "user".into(),
            content: "hi".into(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<OllamaMessage>(&json).unwrap(), m);
    }

    // ── 连接自检：模型可用性 ─────────────────────────────────────
    //
    // 与 openai.rs 同一类假绿：旧实现里 `ok` 恒为 `true`，
    // 本地一个模型都没 pull 也报绿灯「已连接」，用户点分析才炸。

    use crate::teststub::StubServer;

    /// 🔴 配置的模型本地不存在 ⇒ 自检必须报**失败**并给出 pull 命令。
    #[tokio::test]
    async fn health_check_fails_when_configured_model_absent() {
        let stub = StubServer::start(vec![(
            "/api/tags",
            200,
            r#"{"models":[{"name":"llama3:latest"}]}"#,
        )]);
        let p = OllamaProvider::new(stub.base_url.clone(), "qwen3:8b");

        let h = p.health_check().await.expect("服务在线，不该是传输层错误");
        assert!(!h.ok, "本地缺这个模型时不能报绿");
        assert!(h.message.contains("qwen3:8b"), "应点名缺哪个模型：{}", h.message);
        assert!(
            h.message.contains("ollama pull qwen3:8b"),
            "应给出可直接复制执行的补救命令：{}",
            h.message
        );
        // 列表仍要带回：前端据此显示"是否想用其中之一"
        assert_eq!(h.models, vec!["llama3:latest".to_string()]);
    }

    /// 本地一个模型都没有 ⇒ 同样报失败，而不是"Ollama 已启动"就算过。
    #[tokio::test]
    async fn health_check_fails_when_no_models_pulled() {
        let stub = StubServer::start(vec![("/api/tags", 200, r#"{"models":[]}"#)]);
        let p = OllamaProvider::new(stub.base_url.clone(), "qwen3:8b");

        let h = p.health_check().await.expect("服务在线，不该是传输层错误");
        assert!(!h.ok, "没有任何模型时不能报绿");
        assert!(
            h.message.contains("ollama pull qwen3:8b"),
            "应直接给出拉取命令：{}",
            h.message
        );
    }

    /// 模型存在（含带 tag 的写法）⇒ 报绿。
    #[tokio::test]
    async fn health_check_passes_when_model_present() {
        let stub = StubServer::start(vec![(
            "/api/tags",
            200,
            r#"{"models":[{"name":"qwen3:8b"},{"name":"llama3:latest"}]}"#,
        )]);
        let p = OllamaProvider::new(stub.base_url.clone(), "qwen3:8b");

        let h = p.health_check().await.expect("应成功");
        assert!(h.ok, "模型齐备就该报绿：{}", h.message);
        assert_eq!(h.models.len(), 2);
    }

    /// 用户填裸模型名（不带 tag）而本地是 `qwen3:8b` ⇒ 也要算命中。
    #[tokio::test]
    async fn health_check_matches_model_without_explicit_tag() {
        let stub = StubServer::start(vec![(
            "/api/tags",
            200,
            r#"{"models":[{"name":"qwen3:8b"}]}"#,
        )]);
        let p = OllamaProvider::new(stub.base_url.clone(), "qwen3");

        let h = p.health_check().await.expect("应成功");
        assert!(h.ok, "裸名应能匹配到带 tag 的本地模型：{}", h.message);
    }
}
