//! 统一错误类型。
//!
//! 设计原则（面向开源协作与可维护性）：
//! 1. **每个引擎定义自己的错误枚举**，通过 `From` 转换汇入 [`SpoliaError`]，
//!    避免"一个巨型 Error 枚举"变成所有人都要改的耦合点。
//! 2. 错误必须**可展示给用户**：`Display` 输出人类可读中文，不含内部路径泄漏。
//! 3. 错误必须**可映射为 HTTP 状态码**（见 `status_code`），前端据此决定提示方式。

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 顶层错误。
#[derive(Debug, Error)]
pub enum SpoliaError {
    #[error("存储层错误: {0}")]
    Storage(#[from] StorageError),

    #[error("扫描错误: {0}")]
    Scanner(#[from] ScannerError),

    #[error("资产抽取错误: {0}")]
    Asset(#[from] AssetError),

    #[error("检索错误: {0}")]
    Search(#[from] SearchError),

    #[error("LLM 调用错误: {0}")]
    Ai(#[from] AiError),

    #[error("任务错误: {0}")]
    Job(#[from] JobError),

    #[error("配置错误: {0}")]
    Config(#[from] ConfigError),

    #[error("请求参数无效: {0}")]
    BadRequest(String),

    #[error("未找到: {0}")]
    NotFound(String),
}

/// 存储层错误。
///
/// 注意：这里**不**包含 `rusqlite::Error` 变体。领域层不应感知具体存储技术，
/// 否则换存储实现（《技术设计书》§25 提到向量层可平滑切到 LanceDB）会波及全部上层。
/// 由 `spolia-storage` 在自己的边界把驱动错误转成带上下文的字符串。
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("数据库操作失败: {context} ({reason})")]
    Sqlite { context: String, reason: String },

    /// 数据库正被另一个写入者占用（SQLITE_BUSY / SQLITE_LOCKED）。
    ///
    /// 🔴 为什么单列一个变体，而不是塞进 `Sqlite { reason: "database is locked" }`
    /// 再靠 `reason.contains("locked")` 判断：
    /// 那是**子串匹配决定行为**的反模式——文案随 SQLite 版本/语言变化就会失效，
    /// 且没有任何编译期保护（本项目已明令禁止这种写法，见 `service/context.rs`
    /// 中 `Job(AlreadyRunning)` 的注释）。
    ///
    /// 这里由 `spolia-storage` 在**类型层面**根据 `rusqlite::ErrorCode`
    /// （`DatabaseBusy` / `DatabaseLocked`）分类，domain 只认这个枚举，
    /// 不依赖 rusqlite，也不需要看字符串。
    ///
    /// 语义：这是**可重试的瞬时状态**，不是故障。典型场景是后台索引大项目时，
    /// 用户同时改设置——两个写入者撞在 SQLite 的单写锁上。
    /// 上层应映射成 409（而非 500），并提示"稍后重试"。
    #[error("数据库正忙: {context}")]
    Busy { context: String },

    #[error("数据库迁移失败: {0}")]
    Migration(String),

    #[error("序列化失败: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("数据库文件无法访问: {path} ({reason})")]
    Unavailable { path: String, reason: String },
}

impl StorageError {
    /// 在存储层边界把驱动错误转为领域错误，附带操作上下文便于排查。
    ///
    /// ⚠️ 这个方法**不识别 busy**：它收 `impl Display`，拿不到 SQLite 错误码。
    /// 写路径上需要区分锁竞争的调用点，应改用 `spolia-storage` 里的
    /// `sqlite_err(context, rusqlite::Error)`——那个会按错误码分类出 [`Self::Busy`]。
    pub fn sqlite(context: impl Into<String>, err: impl std::fmt::Display) -> Self {
        Self::Sqlite {
            context: context.into(),
            reason: err.to_string(),
        }
    }

    /// 是否为"数据库正忙"（可重试的瞬时状态）。
    ///
    /// 供 service 层决定映射成 409 而非 5xx。类型层面的判断，不看文案。
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }
}

/// 扫描器错误。
#[derive(Debug, Error)]
pub enum ScannerError {
    #[error("目录不存在: {0}")]
    DirNotFound(String),

    #[error("目录未授权: {0}")]
    NotAuthorized(String),

    #[error("路径不是目录: {0}")]
    NotADirectory(String),

    #[error("读取失败: {path} ({reason})")]
    Io { path: String, reason: String },

    #[error("Git 命令不可用，已降级为无 Git 历史模式")]
    GitUnavailable,

    #[error("扫描被取消")]
    Cancelled,
}

/// 资产抽取错误。
#[derive(Debug, Error)]
pub enum AssetError {
    #[error("不支持的语言: {0}")]
    UnsupportedLanguage(String),

    #[error("解析失败: {path} ({reason})")]
    Parse { path: String, reason: String },

    #[error("抽取结果未通过校验: {0}")]
    Validation(String),
}

/// 检索错误。
#[derive(Debug, Error)]
pub enum SearchError {
    #[error("查询语法无效: {0}")]
    InvalidQuery(String),

    #[error("索引尚未建立，请先扫描项目")]
    IndexNotReady,

    #[error("检索失败: {0}")]
    Backend(String),
}

/// AI 层错误。
#[derive(Debug, Error)]
pub enum AiError {
    #[error("未配置模型：请在 设置 → 大模型配置 中完成配置")]
    NotConfigured,

    #[error("连接失败: {0}")]
    Connection(String),

    #[error("认证失败: API Key 无效或已过期")]
    Unauthorized,

    #[error("请求被限流，请稍后重试")]
    RateLimited,

    #[error("模型响应无法解析为约定结构: {0}")]
    MalformedResponse(String),

    #[error("模型调用超时（{0}s）")]
    Timeout(u64),

    #[error("敏感项目禁止使用云端模型（Local-First 约束）")]
    SensitiveBlocked,

    /// AI 分析被用户在设置里关掉了（`scan.level2_enabled == false`）。
    ///
    /// 🔴 与 `NotConfigured` 严格区分：后者是"还没配"，这个是"配好了但被主动关闭"。
    /// 混用会给出"去启动 Ollama / 填 API Key"的提示，而用户真正要做的是打开一个开关——
    /// 照着错误提示操作只会让人怀疑配置丢了。
    ///
    /// 🔴 也与 `SensitiveBlocked` 严格区分：那个是**安全拦截**，绝不能被降级绕过；
    /// 这个是用户的**主动选择**，降级到确定性回答正是用户想要的效果。
    #[error("AI 分析已关闭：请在 设置 → 扫描设置 打开「AI 分析」开关")]
    Disabled,

    #[error("用户取消")]
    Cancelled,

    #[error("模型服务错误: {0}")]
    Provider(String),
}

impl AiError {
    /// 是否为"可降级"错误：LLM 不可用时系统应回退到确定性检索式回答，
    /// 而不是让整个 AI 分析师页面报错白屏。
    ///
    /// 🔴 `Disabled` 在此列表内是刻意的：用户关掉 AI 就是要"别调模型"，
    /// 退回确定性回答（并说明原因）完全符合其意图。
    /// `SensitiveBlocked` / `Cancelled` 不在列表内——安全拦截与用户取消
    /// 都不该被"降级"悄悄绕过。
    pub fn is_degradable(&self) -> bool {
        matches!(
            self,
            Self::NotConfigured
                | Self::Disabled
                | Self::Connection(_)
                | Self::Timeout(_)
                | Self::Provider(_)
                | Self::Unauthorized
        )
    }
}

/// 任务错误。
#[derive(Debug, Error)]
pub enum JobError {
    #[error("任务不存在: {0}")]
    NotFound(String),

    #[error("任务已结束，无法取消: {0}")]
    AlreadyFinished(String),

    #[error("已有同类任务在运行: {0}")]
    AlreadyRunning(String),

    #[error("任务执行失败: {0}")]
    Execution(String),
}

/// 配置错误。
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Base URL 无效: {0}")]
    InvalidUrl(String),

    #[error("缺少 API Key")]
    MissingApiKey,

    #[error("扫描目录列表为空，请先在 设置 → 扫描目录 中添加")]
    NoScanDirs,

    #[error("配置写入失败: {0}")]
    Persist(String),
}

impl SpoliaError {
    /// 映射为 HTTP 状态码。集中在此处，避免各 handler 自行判断导致不一致。
    pub fn status_code(&self) -> u16 {
        match self {
            Self::BadRequest(_) => 400,
            Self::NotFound(_) => 404,
            Self::Storage(StorageError::Unavailable { .. }) => 503,
            Self::Scanner(ScannerError::DirNotFound(_)) => 404,
            Self::Scanner(ScannerError::NotAuthorized(_)) => 403,
            Self::Scanner(ScannerError::NotADirectory(_)) => 400,
            Self::Scanner(ScannerError::Cancelled) => 499, // 客户端关闭请求
            Self::Ai(AiError::NotConfigured) => 424,       // Failed Dependency：需先配置
            Self::Ai(AiError::Unauthorized) => 401,
            Self::Ai(AiError::RateLimited) => 429,
            Self::Ai(AiError::SensitiveBlocked) => 403,
            Self::Ai(AiError::Cancelled) => 499,
            Self::Ai(AiError::Timeout(_)) => 504,
            Self::Job(JobError::NotFound(_)) => 404,
            Self::Job(JobError::AlreadyRunning(_)) => 409,
            Self::Job(JobError::AlreadyFinished(_)) => 409,
            Self::Config(ConfigError::InvalidUrl(_)) => 400,
            Self::Config(ConfigError::MissingApiKey) => 400,
            Self::Config(ConfigError::NoScanDirs) => 424,
            Self::Search(SearchError::IndexNotReady) => 424,
            _ => 500,
        }
    }

    /// 稳定的机器可读错误码，前端据此做差异化提示（不依赖中文文案匹配）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::NotFound(_) => "not_found",
            Self::Storage(_) => "storage_error",
            Self::Scanner(ScannerError::DirNotFound(_)) => "dir_not_found",
            Self::Scanner(ScannerError::NotAuthorized(_)) => "not_authorized",
            Self::Scanner(ScannerError::NotADirectory(_)) => "not_a_directory",
            Self::Scanner(ScannerError::GitUnavailable) => "git_unavailable",
            Self::Scanner(ScannerError::Cancelled) => "cancelled",
            Self::Scanner(_) => "scan_error",
            Self::Asset(_) => "asset_error",
            Self::Search(SearchError::IndexNotReady) => "index_not_ready",
            Self::Search(_) => "search_error",
            Self::Ai(AiError::NotConfigured) => "llm_not_configured",
            Self::Ai(AiError::SensitiveBlocked) => "sensitive_blocked",
            Self::Ai(AiError::Cancelled) => "cancelled",
            Self::Ai(_) => "llm_error",
            Self::Job(JobError::AlreadyRunning(_)) => "job_already_running",
            Self::Job(JobError::AlreadyFinished(_)) => "job_already_finished",
            Self::Job(_) => "job_error",
            Self::Config(ConfigError::NoScanDirs) => "no_scan_dirs",
            Self::Config(_) => "config_error",
        }
    }

    /// 是否为"用户可自行修复"的错误：前端应展示引导而非仅报错。
    pub fn is_recoverable_by_user(&self) -> bool {
        matches!(
            self,
            Self::Ai(AiError::NotConfigured)
                | Self::Config(ConfigError::NoScanDirs)
                | Self::Config(ConfigError::MissingApiKey)
                | Self::Config(ConfigError::InvalidUrl(_))
                | Self::Scanner(ScannerError::NotAuthorized(_))
                | Self::Search(SearchError::IndexNotReady)
        )
    }
}

/// API 统一错误响应体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// 机器可读错误码
    pub code: String,
    /// 人类可读中文提示（可直接展示给用户）
    pub message: String,
    /// 用户是否可自行修复（前端据此显示"去设置"等引导按钮）
    pub recoverable: bool,
    /// 可选的修复引导文案
    pub hint: Option<String>,
}

impl ErrorBody {
    pub fn from_error(e: &SpoliaError) -> Self {
        Self {
            code: e.code().to_string(),
            message: e.to_string(),
            recoverable: e.is_recoverable_by_user(),
            hint: hint_for(e),
        }
    }
}

/// 为可修复错误生成引导文案。集中于此，避免各页面各写一套提示。
fn hint_for(e: &SpoliaError) -> Option<String> {
    match e {
        SpoliaError::Ai(AiError::NotConfigured) => Some("前往「设置 → 大模型配置」填写 Base URL 与 API Key，或配置本地 Ollama。".into()),
        SpoliaError::Config(ConfigError::NoScanDirs) => Some("前往「设置 → 扫描目录」添加至少一个项目目录。".into()),
        SpoliaError::Config(ConfigError::MissingApiKey) => Some("该提供商需要 API Key 才能调用。".into()),
        SpoliaError::Config(ConfigError::InvalidUrl(u)) => Some(format!("「{u}」不是合法的 http(s) 地址。")),
        SpoliaError::Search(SearchError::IndexNotReady) => Some("先在首页执行一次扫描，索引建立后即可搜索。".into()),
        SpoliaError::Scanner(ScannerError::NotAuthorized(p)) => Some(format!("「{p}」未获得授权，请在「设置 → 扫描目录」中添加。")),
        SpoliaError::Scanner(ScannerError::GitUnavailable) => Some("安装 Git 后可获得提交历史、活跃度与项目考古能力。".into()),
        SpoliaError::Ai(AiError::RateLimited) => Some("模型服务限流，稍后重试或切换到本地模型。".into()),
        SpoliaError::Ai(AiError::SensitiveBlocked) => Some("该项目被标记为敏感，仅允许本地模型处理。".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_are_sensible() {
        assert_eq!(SpoliaError::NotFound("x".into()).status_code(), 404);
        assert_eq!(SpoliaError::BadRequest("x".into()).status_code(), 400);
        assert_eq!(SpoliaError::Ai(AiError::Unauthorized).status_code(), 401);
        assert_eq!(SpoliaError::Ai(AiError::RateLimited).status_code(), 429);
        assert_eq!(SpoliaError::Ai(AiError::Timeout(30)).status_code(), 504);
        assert_eq!(SpoliaError::Job(JobError::AlreadyRunning("j".into())).status_code(), 409);
        assert_eq!(SpoliaError::Scanner(ScannerError::NotAuthorized("d".into())).status_code(), 403);
    }

    #[test]
    fn unknown_errors_default_to_500() {
        let e = SpoliaError::Asset(AssetError::UnsupportedLanguage("brainfuck".into()));
        assert_eq!(e.status_code(), 500);
    }

    #[test]
    fn error_body_carries_code_and_message() {
        let e = SpoliaError::Ai(AiError::NotConfigured);
        let body = ErrorBody::from_error(&e);
        assert_eq!(body.code, "llm_not_configured");
        assert!(body.recoverable);
        assert!(body.hint.unwrap().contains("大模型配置"));
        assert!(body.message.contains("设置"), "提示应指向设置页: {}", body.message);
    }

    #[test]
    fn non_recoverable_error_has_no_hint() {
        let e = SpoliaError::Storage(StorageError::Migration("boom".into()));
        let body = ErrorBody::from_error(&e);
        assert!(!body.recoverable);
        assert!(body.hint.is_none());
    }

    #[test]
    fn degradable_ai_errors_allow_fallback() {
        assert!(AiError::NotConfigured.is_degradable());
        assert!(AiError::Connection("refused".into()).is_degradable());
        assert!(AiError::Timeout(30).is_degradable());
        // 敏感阻断不是"服务不可用"，不应静默降级到云端
        assert!(!AiError::SensitiveBlocked.is_degradable());
        assert!(!AiError::Cancelled.is_degradable());
    }

    #[test]
    fn sensitive_block_and_no_dirs_are_recoverable() {
        assert!(SpoliaError::Config(ConfigError::NoScanDirs).is_recoverable_by_user());
        assert!(!SpoliaError::Ai(AiError::SensitiveBlocked).is_recoverable_by_user());
    }

    #[test]
    fn messages_are_user_facing_chinese() {
        let e = SpoliaError::Scanner(ScannerError::DirNotFound("D:/gone".into()));
        let m = e.to_string();
        assert!(m.contains("目录不存在"), "{m}");
        assert!(!m.contains("panicked"));
    }

    #[test]
    fn error_codes_are_stable_strings() {
        assert_eq!(SpoliaError::NotFound("x".into()).code(), "not_found");
        assert_eq!(SpoliaError::Search(SearchError::IndexNotReady).code(), "index_not_ready");
        assert_eq!(SpoliaError::Scanner(ScannerError::Cancelled).code(), "cancelled");
    }
}
