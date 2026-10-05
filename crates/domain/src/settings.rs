//! 设置与 LLM 配置（《技术设计书》§16 LLM 抽象层 + 三级分析路由）。
//!
//! 🔴 Local-First 原则的技术兑现：**默认不出网**，云端模型必须用户显式配置并授权。
//! 敏感项目强制走本地；Embedding 强制本地。

use serde::{Deserialize, Serialize};

/// LLM 任务路由目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RouteTarget {
    /// 本地模型（Ollama / llama.cpp / LM Studio）
    #[default]
    Local,
    /// 云端模型（需显式授权）
    Cloud,
}

impl RouteTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "local" => Self::Local,
            "cloud" => Self::Cloud,
            _ => return None,
        })
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Local => "本地模型",
            Self::Cloud => "云端模型",
        }
    }
}

/// 云端提供商（OpenAI-Compatible 抽象）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudProvider {
    OpenAiCompatible,
    Anthropic,
    Qwen,
    Gemini,
}

impl CloudProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai",
            Self::Anthropic => "anthropic",
            Self::Qwen => "qwen",
            Self::Gemini => "gemini",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "openai" => Self::OpenAiCompatible,
            "anthropic" => Self::Anthropic,
            "qwen" => Self::Qwen,
            "gemini" => Self::Gemini,
            _ => return None,
        })
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAI Compatible",
            Self::Anthropic => "Anthropic",
            Self::Qwen => "Qwen (DashScope)",
            Self::Gemini => "Gemini",
        }
    }

    /// 默认 Base URL。切换提供商时前端自动填充，用户可改为代理或自建网关。
    pub fn default_base_url(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com",
            Self::Qwen => "https://dashscope.aliyuncs.com/compatible-mode/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        }
    }

    /// 预置模型列表（下拉候选）。用户可自行输入其他模型名。
    pub fn preset_models(&self) -> &'static [&'static str] {
        match self {
            Self::OpenAiCompatible => &["gpt-5-mini", "gpt-5", "o4-mini"],
            Self::Anthropic => &["claude-sonnet-4-5", "claude-haiku-4"],
            Self::Qwen => &["qwen3-max", "qwen3-coder-plus", "qwen3-8b"],
            Self::Gemini => &["gemini-2.5-pro", "gemini-2.5-flash"],
        }
    }

    /// 该提供商是否需要以 Anthropic 原生协议对话（其余走 OpenAI-compatible）。
    pub fn uses_anthropic_protocol(&self) -> bool {
        matches!(self, Self::Anthropic)
    }

    /// 该提供商是否需要以 Gemini 原生协议对话。
    pub fn uses_gemini_protocol(&self) -> bool {
        matches!(self, Self::Gemini)
    }

    pub fn all() -> &'static [CloudProvider] {
        &[
            Self::OpenAiCompatible,
            Self::Anthropic,
            Self::Qwen,
            Self::Gemini,
        ]
    }
}

/// 本地推理后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalBackend {
    Ollama,
    LlamaCpp,
    LmStudio,
}

impl LocalBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::LlamaCpp => "llamacpp",
            Self::LmStudio => "lmstudio",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ollama" => Self::Ollama,
            "llamacpp" => Self::LlamaCpp,
            "lmstudio" => Self::LmStudio,
            _ => return None,
        })
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Ollama => "Ollama",
            Self::LlamaCpp => "llama.cpp",
            Self::LmStudio => "LM Studio",
        }
    }

    pub fn default_base_url(&self) -> &'static str {
        match self {
            Self::Ollama => "http://127.0.0.1:11434",
            Self::LlamaCpp => "http://127.0.0.1:8080",
            Self::LmStudio => "http://127.0.0.1:1234/v1",
        }
    }

    pub fn preset_models(&self) -> &'static [&'static str] {
        match self {
            Self::Ollama => &["qwen3:8b", "qwen3:14b", "llama3.1:8b", "gemma3:4b"],
            Self::LlamaCpp => &["qwen3-8b-q4.gguf"],
            Self::LmStudio => &["本地已加载模型"],
        }
    }

    /// Ollama 的原生 API 路径与 OpenAI-compatible 不同（/v1 为兼容层），
    /// 连接测试与健康检查需要知道用哪个端点。
    pub fn health_path(&self) -> &'static str {
        match self {
            Self::Ollama => "/api/tags",
            Self::LlamaCpp => "/v1/models",
            Self::LmStudio => "/v1/models",
        }
    }

    pub fn all() -> &'static [LocalBackend] {
        &[Self::Ollama, Self::LlamaCpp, Self::LmStudio]
    }
}

/// LLM 配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSettings {
    pub cloud_provider: CloudProvider,
    pub cloud_base_url: String,
    /// API Key。**仅存于本机 SQLite**；序列化到前端时由 API 层做掩码处理，
    /// 完整值只在 `PUT` 时回写（见 `mask_api_key`）。
    pub cloud_api_key: String,
    pub cloud_model: String,
    pub local_backend: LocalBackend,
    pub local_base_url: String,
    pub local_model: String,
    /// 快速分析（画像 / 分类 / 摘要）路由
    pub route_fast: RouteTarget,
    /// 深度分析（洞察 / 机会 / 跨项目推理）路由
    pub route_deep: RouteTarget,
    /// 敏感项目仅本地：标记为敏感的项目任何数据都不进入云端模型上下文
    pub sensitive_local_only: bool,
    /// Embedding 强制本地：代码永不出网
    pub embedding_local_only: bool,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            cloud_provider: CloudProvider::OpenAiCompatible,
            cloud_base_url: CloudProvider::OpenAiCompatible.default_base_url().to_string(),
            cloud_api_key: String::new(),
            cloud_model: "gpt-5-mini".to_string(),
            local_backend: LocalBackend::Ollama,
            local_base_url: LocalBackend::Ollama.default_base_url().to_string(),
            local_model: "qwen3:8b".to_string(),
            // 默认全本地：符合 Local-First「默认不出网」
            route_fast: RouteTarget::Local,
            route_deep: RouteTarget::Local,
            sensitive_local_only: true,
            embedding_local_only: true,
        }
    }
}

impl LlmSettings {
    /// API Key 掩码：`sk-abc123...xyz` → `sk-ab…xyz`。
    /// 用于 GET 返回给前端，避免明文 key 出现在网络响应与前端内存中。
    pub fn mask_api_key(key: &str) -> String {
        let k = key.trim();
        if k.is_empty() {
            return String::new();
        }
        let chars: Vec<char> = k.chars().collect();
        if chars.len() <= 8 {
            // 短 key 全掩码，不泄露任何字符
            return "•".repeat(chars.len());
        }
        let head: String = chars.iter().take(4).collect();
        let tail: String = chars.iter().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
        format!("{head}…{tail}")
    }

    /// 掩码占位符：前端未修改 key 时回传该值，服务端据此保留原 key（不覆盖为掩码串）。
    pub const MASKED_PLACEHOLDER: &'static str = "__unchanged__";

    /// 云端是否已配置（有 key 且 base_url 合法）。未配置时 UI 应禁用"测试连接"并给出引导。
    pub fn cloud_configured(&self) -> bool {
        !self.cloud_api_key.trim().is_empty() && is_http_url(&self.cloud_base_url)
    }

    pub fn local_configured(&self) -> bool {
        is_http_url(&self.local_base_url)
    }

    /// 敏感项目实际可用的路由目标：强制降级为本地（Local-First 硬约束）。
    pub fn route_for(&self, job: crate::job::JobType, project_sensitive: bool) -> RouteTarget {
        if project_sensitive && self.sensitive_local_only {
            return RouteTarget::Local;
        }
        if job.is_deep_analysis() {
            self.route_deep
        } else {
            self.route_fast
        }
    }

    /// 应用前端提交的 key：若为掩码占位符则保留原值，否则用新值。
    ///
    /// 这解决了原型期的一个真实缺陷——保存配置时会把掩码串 `sk-****-demo-key`
    /// 当成真 key 写回，导致后续连接测试永远失败。
    pub fn apply_api_key(&mut self, incoming: &str) {
        if incoming == Self::MASKED_PLACEHOLDER {
            return;
        }
        self.cloud_api_key = incoming.trim().to_string();
    }
}

/// 校验是否为合法 http(s) URL。
pub fn is_http_url(s: &str) -> bool {
    let t = s.trim();
    (t.starts_with("http://") || t.starts_with("https://")) && t.len() > "https://".len()
}

/// 扫描目录授权项（目录级授权：未添加的目录不读取）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanDir {
    pub path: String,
    /// 是否启用（可临时停用而不删除）
    pub enabled: bool,
    pub added_at: String,
    /// 上次扫描时间；从未扫描为 `None`（前端显示"尚未扫描"而非假时间）
    pub last_scanned_at: Option<String>,
    /// 上次扫描发现的项目数
    pub project_count: Option<u32>,
}

/// 扫描行为配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSettings {
    pub dirs: Vec<ScanDir>,
    /// 文件监听（变更实时增量索引）
    pub watch_enabled: bool,
    /// 额外排除模式（glob，每行一条）。内置排除项（node_modules/.git/dist/凭证文件）
    /// 始终生效，不在此列表中也不可关闭——这是隐私底线。
    pub exclude_patterns: Vec<String>,
    /// 是否允许调用大模型（项目画像 + AI 分析师）。
    ///
    /// 🔴 字段名与所在结构体（`ScanSettings`）都有误导性：它**不控制扫描**，
    /// 也不控制资产/能力抽取——那些在 Level 1 的 `IndexCodeHandler` 里完成，
    /// 跨项目洞察同样是纯规则计算（`InsightHandler` 不调 LLM）。
    /// 真正需要模型的只有两处：`service::profile::generate` 与
    /// `service::analyst::attempt_llm`，它们各自读这个开关。
    ///
    /// 存在的意义是 Local-First 的**全局总闸**：`sensitive` 是逐项目标记
    /// （默认 false，用户往往一个都没标），而这个开关让用户能一键确保
    /// "此刻任何代码都不会因为点了某个按钮而离开本机"。
    ///
    /// 字段名不改：迁移要动 schema、三处端点与前端镜像，收益不抵成本。
    /// 前端 UI 文案已按实际行为写成「AI 分析」。
    pub level2_enabled: bool,
    /// 单目录最大深度（防止扫描整盘失控）
    pub max_depth: u32,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            dirs: Vec::new(),
            watch_enabled: true,
            // 🔴 写法必须是 `**/X` 或裸名 `X`，**不能写 `**/X/**`**。
            //
            // `discover()` 拿"目录名"和"目录路径"两种输入去匹配 glob，
            // 而 `**/X/**` 要求 X **后面还有内容**，对这两种输入都不命中——
            // 实测 `**/node_modules/**` 的命中数是 0。
            // （scanner 侧 `expand_pattern` 会把常见写法归一，但默认值本身应写对，
            //   否则用户照抄默认值去改就会踩同一个坑。）
            //
            // 收录原则：**只放对任意用户都成立的通用目录名**。
            // 机器特有的路径（某人的 boost 源码树、某个 SDK 安装位置）属于用户配置，
            // 通过设置页/API 追加，不硬编码在这里。
            exclude_patterns: vec![
                // 构建产物与虚拟环境（与 ALWAYS_EXCLUDED 有重叠，此处显式列出便于用户看见并修改）
                "**/node_modules".into(),
                "**/.venv".into(),
                "**/venv".into(),
                "**/target".into(),
                "**/dist".into(),
                "**/build".into(),
                "**/out".into(),
                // 包管理器本地缓存/仓库：体积巨大且全是第三方
                "**/.pub-cache".into(),
                "**/pub-cache".into(),
                "**/.cargo".into(),
                "**/.m2".into(),
                "**/.gradle".into(),
                "**/.nuget".into(),
                "**/.yarn".into(),
                "**/.pnpm-store".into(),
                "**/Pods".into(),
                "**/Carthage".into(),
                // 项目内自带的第三方源码树
                "**/ThirdParty".into(),
                "**/third_party".into(),
                "**/thirdparty".into(),
                "**/3rdparty".into(),
                "**/external".into(),
                "**/vendor".into(),
                // Android SDK：路径形态跨机器稳定（ANDROID_HOME 恒为 .../Android/Sdk），
                // 用两段式而非裸 `**/SDK`，避免误伤用户自己名为 SDK 的目录
                "**/Android/SDK".into(),
                "**/Android/Sdk".into(),
            ],
            level2_enabled: true,
            max_depth: 6,
        }
    }
}

impl ScanSettings {
    pub fn enabled_dirs(&self) -> Vec<&ScanDir> {
        self.dirs.iter().filter(|d| d.enabled).collect()
    }

    /// 添加目录：去重（按规范化路径）、忽略空串。
    /// 返回 `false` 表示已存在（前端据此提示"该目录已在列表中"）。
    pub fn add_dir(&mut self, path: &str, now: &str) -> bool {
        let p = path.trim();
        if p.is_empty() {
            return false;
        }
        let norm = normalize_path(p);
        if self.dirs.iter().any(|d| normalize_path(&d.path) == norm) {
            return false;
        }
        self.dirs.push(ScanDir {
            path: p.to_string(),
            enabled: true,
            added_at: now.to_string(),
            last_scanned_at: None,
            project_count: None,
        });
        true
    }

    /// 按路径移除（比按下标移除更安全：前端列表重排不会误删）。
    /// 返回被移除的路径，`None` 表示未找到。
    pub fn remove_dir(&mut self, path: &str) -> Option<String> {
        let norm = normalize_path(path);
        let idx = self.dirs.iter().position(|d| normalize_path(&d.path) == norm)?;
        Some(self.dirs.remove(idx).path)
    }

    /// 启用/停用某目录（临时排除而不删除授权记录）。
    ///
    /// 返回是否找到该目录。与 `add_dir`/`remove_dir` 一样按**规范化路径**匹配：
    /// 前端回传的路径可能与入库时分隔符或大小写不同（Windows 上尤其常见），
    /// 按原始字符串比对会找不到目标而静默失败。
    pub fn set_dir_enabled(&mut self, path: &str, enabled: bool) -> bool {
        let norm = normalize_path(path);
        match self.dirs.iter_mut().find(|d| normalize_path(&d.path) == norm) {
            Some(d) => {
                d.enabled = enabled;
                true
            }
            None => false,
        }
    }
}

/// 路径规范化：统一分隔符、去尾部斜杠、转小写盘符（Windows 大小写不敏感）。
/// 仅用于**比较去重**，不改变存储值。
pub fn normalize_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    while s.len() > 1 && (s.ends_with('/') || s.ends_with('\\')) {
        s.pop();
    }
    // Windows 盘符大小写不敏感；其他平台保持原样
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// 外观设置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => return None,
        })
    }
}

/// 外观设置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppearanceSettings {
    pub theme: Theme,
    /// 减弱动效（尊重系统 prefers-reduced-motion）
    pub reduce_motion: bool,
}

/// 全部设置（单机版：无账号体系，配置只存本机）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    pub llm: LlmSettings,
    pub scan: ScanSettings,
    pub appearance: AppearanceSettings,
}

/// 网络访问审计条目（《技术设计书》§23「可审计」）。
///
/// # 🔴 这张表承担**两种**语义，`ok` 的三态设计正是为此
/// 1. **模型调用留痕**：数据发给了哪个模型、成没成功（`ok = Some(_)`）。
/// 2. **本地安全事件留痕**：用户取消敏感标记、关闭「敏感项目仅本地」等
///    安全约束降级（`model = "-"`，`ok = None`）。
///
/// 第 2 类**不是**一次调用，没有成败概念。若把 `ok` 设计成 `bool`，
/// 就只能给安全事件填 `true`——于是 UI 会在「用户关闭了敏感项目仅本地约束」
/// 旁边显示一个绿色对勾，把一次**安全降级**渲染成"操作成功"。
/// 那不叫审计，叫误导。故用 `Option<bool>`：`None` 表示"不适用"，
/// 前端据此**不渲染**任何成败标记。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at: String,
    /// 目标模型标识（provider:model）；非模型调用事件为 `"-"`
    pub model: String,
    /// 本地/云端
    pub route: RouteTarget,
    /// 任务类型
    pub job_type: String,
    /// 发送了什么（摘要，不含代码原文）
    pub summary: String,
    /// 涉及的项目
    pub project_id: Option<String>,
    /// 模型调用是否成功。`None` = 本条不是模型调用（见类型文档）。
    ///
    /// 🔴 失败也**必须**写一条：请求被网关拒绝（400/未开通/超时）时
    /// prompt 已经出网了——网关是**收到之后**才拒的。
    /// "调用失败"≠"数据没发出去"，只记成功会让审计链出现缺口，
    /// 用户查"我的数据什么时候上过云"会得到假答案。
    pub ok: Option<bool>,
    /// 失败原因（仅 `ok = Some(false)` 时有值）。
    ///
    /// 来自 provider 的错误消息：不含代码原文，也不含 API Key
    /// （reqwest 不会把 `Authorization` 头写进错误消息），且已被
    /// `brief_message` 截断到 180 字符。审计日志是本地文件，不出网。
    pub error: Option<String>,
}

impl AuditEntry {
    /// 构造一条**成功的模型调用**审计。
    ///
    /// 用构造器而非裸字面量，是为了让 `ok`/`error` 的一致性不变式
    /// 只在这一处成立：成功必然 `error = None`。
    /// 若放开字面量，"成功但带错误原因"这种自相矛盾的行随时可能被写出来。
    pub fn llm_ok(
        at: impl Into<String>,
        model: impl Into<String>,
        route: RouteTarget,
        job_type: impl Into<String>,
        summary: impl Into<String>,
        project_id: Option<String>,
    ) -> Self {
        Self {
            at: at.into(),
            model: model.into(),
            route,
            job_type: job_type.into(),
            summary: summary.into(),
            project_id,
            ok: Some(true),
            error: None,
        }
    }

    /// 构造一条**失败的模型调用**审计。
    ///
    /// 🔴 失败也必须留痕：prompt 已经出网，网关是收到之后才拒的。
    /// `error` 会被自动规整——空串归一成 `None`，避免"失败但没原因"的半截记录。
    pub fn llm_failed(
        at: impl Into<String>,
        model: impl Into<String>,
        route: RouteTarget,
        job_type: impl Into<String>,
        summary: impl Into<String>,
        project_id: Option<String>,
        error: impl Into<String>,
    ) -> Self {
        let err = error.into();
        let err = if err.trim().is_empty() { None } else { Some(err) };
        Self {
            at: at.into(),
            model: model.into(),
            route,
            job_type: job_type.into(),
            summary: summary.into(),
            project_id,
            ok: Some(false),
            error: err,
        }
    }

    /// 构造一条**本地安全事件**审计（取消敏感标记、关闭 Local-First 约束等）。
    ///
    /// 🔴 `ok = None` 是刻意的：这类事件不是一次模型调用，没有成败可言。
    /// 前端据此不渲染任何成败标记——绝不能给"用户关闭了安全约束"打上绿色对勾。
    pub fn event(
        at: impl Into<String>,
        job_type: impl Into<String>,
        summary: impl Into<String>,
        project_id: Option<String>,
    ) -> Self {
        Self {
            at: at.into(),
            // 非模型调用：无目标模型、无出网路由
            model: "-".into(),
            route: RouteTarget::Local,
            job_type: job_type.into(),
            summary: summary.into(),
            project_id,
            ok: None,
            error: None,
        }
    }

    /// 是否为一次模型调用（区别于本地安全事件）。
    pub fn is_llm_call(&self) -> bool {
        self.ok.is_some()
    }
}

/// MCP Server 运行状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStatus {
    pub running: bool,
    /// 监听地址（如 127.0.0.1:38018）；未运行时为 `None`
    pub endpoint: Option<String>,
    /// 可接入的客户端
    pub available_to: Vec<String>,
    /// 暴露的工具（固定 8 个，《技术设计书》§18）
    pub tools: Vec<String>,
    /// 内置 Skills
    pub skills: Vec<String>,
    /// 启动失败原因
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_local_first() {
        let s = Settings::default();
        assert_eq!(s.llm.route_fast, RouteTarget::Local);
        assert_eq!(s.llm.route_deep, RouteTarget::Local);
        assert!(s.llm.sensitive_local_only);
        assert!(s.llm.embedding_local_only);
        assert!(!s.llm.cloud_configured(), "默认不应配置云端 key");
    }

    #[test]
    fn api_key_masking_hides_middle() {
        assert_eq!(LlmSettings::mask_api_key("sk-abc123456789xyz"), "sk-a…xyz");
        assert_eq!(LlmSettings::mask_api_key(""), "");
    }

    #[test]
    fn short_key_fully_masked() {
        let m = LlmSettings::mask_api_key("sk12345");
        assert!(!m.contains('s'), "短 key 不应泄露任何字符: {m}");
        assert_eq!(m.chars().count(), 7);
    }

    /// 原型期缺陷：保存配置会把掩码串写真 key。
    #[test]
    fn masked_placeholder_preserves_original_key() {
        let mut s = LlmSettings { cloud_api_key: "sk-real-key-value".into(), ..Default::default() };
        s.apply_api_key(LlmSettings::MASKED_PLACEHOLDER);
        assert_eq!(s.cloud_api_key, "sk-real-key-value");
        s.apply_api_key("sk-new-key-value");
        assert_eq!(s.cloud_api_key, "sk-new-key-value");
    }

    #[test]
    fn sensitive_project_forces_local_route() {
        let s = LlmSettings { route_deep: RouteTarget::Cloud, ..Default::default() };
        let r = s.route_for(crate::job::JobType::DiscoverOpportunity, true);
        assert_eq!(r, RouteTarget::Local, "敏感项目必须走本地");
        let r2 = s.route_for(crate::job::JobType::DiscoverOpportunity, false);
        assert_eq!(r2, RouteTarget::Cloud);
    }

    #[test]
    fn route_by_analysis_depth() {
        let s = LlmSettings {
            route_fast: RouteTarget::Local,
            route_deep: RouteTarget::Cloud,
            ..Default::default()
        };
        assert_eq!(s.route_for(crate::job::JobType::AnalyzeProject, false), RouteTarget::Local);
        assert_eq!(s.route_for(crate::job::JobType::GenerateInsight, false), RouteTarget::Cloud);
    }

    #[test]
    fn url_validation() {
        assert!(is_http_url("https://api.openai.com/v1"));
        assert!(is_http_url("http://127.0.0.1:11434"));
        assert!(!is_http_url("ftp://x"));
        assert!(!is_http_url("https://"));
        assert!(!is_http_url(""));
    }

    #[test]
    fn providers_have_urls_and_models() {
        for p in CloudProvider::all() {
            assert!(is_http_url(p.default_base_url()), "{:?}", p);
            assert!(!p.preset_models().is_empty());
            assert_eq!(CloudProvider::parse(p.as_str()), Some(*p));
            assert!(!p.display_name().is_empty());
        }
        assert!(CloudProvider::Anthropic.uses_anthropic_protocol());
        assert!(CloudProvider::Gemini.uses_gemini_protocol());
        assert!(!CloudProvider::Qwen.uses_anthropic_protocol());
    }

    #[test]
    fn local_backends_have_health_paths() {
        for b in LocalBackend::all() {
            assert!(is_http_url(b.default_base_url()));
            assert!(b.health_path().starts_with('/'));
            assert_eq!(LocalBackend::parse(b.as_str()), Some(*b));
        }
        assert_eq!(LocalBackend::Ollama.health_path(), "/api/tags");
    }

    #[test]
    fn add_dir_dedupes_case_insensitively_on_windows() {
        let mut s = ScanSettings::default();
        assert!(s.add_dir("D:/Projects", "now"));
        // 重复添加（含尾部斜杠与反斜杠变体）应被拒绝
        assert!(!s.add_dir("D:/Projects/", "now"));
        assert!(!s.add_dir("D:\\Projects", "now"));
        assert_eq!(s.dirs.len(), 1);
        assert!(!s.add_dir("", "now"), "空路径不应添加");
    }

    #[test]
    fn remove_dir_by_path_not_index() {
        let mut s = ScanSettings::default();
        s.add_dir("D:/A", "t");
        s.add_dir("D:/B", "t");
        assert_eq!(s.remove_dir("D:/A").as_deref(), Some("D:/A"));
        assert_eq!(s.dirs.len(), 1);
        assert_eq!(s.dirs[0].path, "D:/B");
        assert_eq!(s.remove_dir("D:/nope"), None);
    }

    /// 启停必须按规范化路径匹配：前端回传的分隔符/大小写可能与入库时不同，
    /// 按原始串比对会找不到目标而**静默失败**（用户点了开关却没生效）。
    #[test]
    fn set_dir_enabled_matches_normalized_path() {
        let mut s = ScanSettings::default();
        s.add_dir("D:/Projects", "t");

        // 反斜杠 + 尾部斜杠变体也应命中
        assert!(s.set_dir_enabled("D:\\Projects\\", false));
        assert!(!s.dirs[0].enabled);
        assert!(s.enabled_dirs().is_empty());

        assert!(s.set_dir_enabled("D:/Projects", true));
        assert!(s.dirs[0].enabled);
        assert_eq!(s.enabled_dirs().len(), 1);
    }

    /// 停用只改标记，不得丢失授权记录与扫描历史。
    #[test]
    fn disabling_dir_preserves_metadata() {
        let mut s = ScanSettings::default();
        s.add_dir("D:/A", "t");
        s.dirs[0].last_scanned_at = Some("2026-09-29T10:00:00Z".into());
        s.dirs[0].project_count = Some(12);

        assert!(s.set_dir_enabled("D:/A", false));
        assert_eq!(s.dirs.len(), 1, "停用不等于删除");
        assert_eq!(
            s.dirs[0].last_scanned_at.as_deref(),
            Some("2026-09-29T10:00:00Z"),
            "扫描历史必须保留，重新启用后要能看到上次结果"
        );
        assert_eq!(s.dirs[0].project_count, Some(12));
    }

    #[test]
    fn set_dir_enabled_reports_unknown_path() {
        let mut s = ScanSettings::default();
        s.add_dir("D:/A", "t");
        assert!(
            !s.set_dir_enabled("D:/nope", false),
            "不存在的目录应返回 false，让上层能提示用户"
        );
        assert!(s.dirs[0].enabled, "未命中时不得改动其它目录");
    }

    #[test]
    fn enabled_dirs_filters_disabled() {
        let mut s = ScanSettings::default();
        s.add_dir("D:/A", "t");
        s.add_dir("D:/B", "t");
        s.dirs[1].enabled = false;
        let e = s.enabled_dirs();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].path, "D:/A");
    }

    #[test]
    fn new_dir_has_no_fake_scan_time() {
        let mut s = ScanSettings::default();
        s.add_dir("D:/A", "t");
        assert_eq!(s.dirs[0].last_scanned_at, None, "未扫描不应有假时间");
        assert_eq!(s.dirs[0].project_count, None);
    }

    #[test]
    fn path_normalization_strips_trailing_separators() {
        assert_eq!(normalize_path("D:/A/B/"), if cfg!(windows) { "d:/a/b" } else { "D:/A/B" });
        assert_eq!(normalize_path("D:\\A\\B\\"), if cfg!(windows) { "d:/a/b" } else { "D:/A/B" });
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn theme_roundtrips() {
        assert_eq!(Theme::parse("dark"), Some(Theme::Dark));
        assert_eq!(Theme::parse("light"), Some(Theme::Light));
        assert_eq!(Theme::parse("x"), None);
        assert_eq!(Theme::default(), Theme::Dark);
    }

    #[test]
    fn route_target_roundtrips() {
        for r in [RouteTarget::Local, RouteTarget::Cloud] {
            assert_eq!(RouteTarget::parse(r.as_str()), Some(r));
            assert!(!r.label_zh().is_empty());
        }
    }
}
