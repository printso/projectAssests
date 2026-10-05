//! 服务上下文与统一错误。
//!
//! # 为什么用一个 Context 而不是各模块自己持有依赖
//! service 层的函数签名要保持"纯业务"：`fn list(ctx, query) -> Result<Page>`。
//! 若每个模块各自接收 `db`、`jobs`、`router`，参数会随功能增长不断膨胀，
//! 且新增依赖要改所有函数签名（开源协作里这是最容易冲突的改动）。

use std::sync::Arc;

use projectassests_ai::{AiRouter, RouterConfig};
use projectassests_jobs::{build_default_handlers, JobEngine};
use projectassests_storage::Database;

/// 服务上下文：所有 service 函数共享的依赖容器。
///
/// 廉价克隆（内部全是 `Arc`），每个请求 clone 一次成本可忽略。
#[derive(Clone)]
pub struct ServiceContext {
    pub db: Arc<Database>,
    pub jobs: JobEngine,
    pub router: AiRouter,
    /// 数据库文件路径（设置页展示与诊断用；内存库为 `:memory:`）
    pub db_path: String,
}

impl ServiceContext {
    /// 打开文件数据库并初始化。
    ///
    /// 🔴 必须收割僵尸任务：上次进程被杀会留下 `running` 记录，
    /// 不清理则 `has_active_of_type` 永久为真——用户点"扫描"永远提示
    /// "已有任务在运行"，而界面上根本看不到那个任务。这是最难自查的故障之一。
    pub fn open(db_path: impl AsRef<std::path::Path>) -> Result<Self, ServiceError> {
        let path = db_path.as_ref();
        let db = Arc::new(Database::open(path)?);
        let jobs = JobEngine::new(Arc::clone(&db), build_default_handlers());
        let reaped = jobs.reap_stale_jobs()?;
        if reaped > 0 {
            tracing::warn!(count = reaped, "启动时清理了上次运行遗留的未完成任务");
        }
        Ok(Self {
            db,
            jobs,
            router: AiRouter::new(RouterConfig::default()),
            db_path: path.display().to_string(),
        })
    }

    /// 内存库（测试专用）。
    pub fn in_memory() -> Result<Self, ServiceError> {
        let db = Arc::new(Database::in_memory()?);
        let jobs = JobEngine::new(Arc::clone(&db), build_default_handlers());
        Ok(Self {
            db,
            jobs,
            router: AiRouter::new(RouterConfig::default()),
            db_path: ":memory:".to_string(),
        })
    }

    /// 当前时间。集中在此处便于测试注入（虽然目前直接读时钟）。
    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

/// 服务层统一错误。
///
/// # 为什么不直接复用 `ProjectAssestsError`
/// `ProjectAssestsError` 是领域/引擎层的错误，粒度细但不含"用户该怎么办"。
/// 服务层要面向 UI，因此每个变体都自带 `hint`——
/// 适配器只管把它映射成 HTTP 状态码或 IPC 错误，不需要再判断语义。
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// 请求参数不合法
    #[error("{0}")]
    Invalid(String),

    /// 实体不存在
    #[error("{0}")]
    NotFound(String),

    /// 前置条件未满足（未扫描 / 未配置模型）
    #[error("{0}")]
    Precondition(String),

    /// 与当前状态冲突（任务已在运行、任务已结束）
    #[error("{0}")]
    Conflict(String),

    /// 存储层故障
    #[error("数据库操作失败")]
    Storage(#[from] projectassests_domain::StorageError),

    /// 任务引擎故障
    #[error("{0}")]
    Job(#[from] projectassests_domain::JobError),

    /// AI 层故障
    #[error("{0}")]
    Ai(#[from] projectassests_domain::AiError),

    /// 检索层故障
    #[error("{0}")]
    Search(#[from] projectassests_domain::SearchError),

    /// 配置故障
    #[error("{0}")]
    Config(#[from] projectassests_domain::ConfigError),

    /// 内部错误（细节已记日志，不外泄）
    #[error("内部错误，详情请查看日志")]
    Internal,
}

impl ServiceError {
    /// 建议的 HTTP 状态码。
    ///
    /// 放在 service 而非 adapter：状态码是**语义映射**，
    /// 两种传输（HTTP / IPC）都需要同样的判断，写两遍必然漂移。
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::NotFound(_) => 404,
            Self::Precondition(_) => 424,
            Self::Conflict(_) => 409,
            // StorageError 本身没有 status_code（那是 ProjectAssestsError 的方法）；
            // 这里按变体映射：只有"库文件不可访问"是用户可自救的环境问题（503），
            // 其余（SQL 错误、迁移失败、序列化）都是程序缺陷 → 500。
            Self::Storage(projectassests_domain::StorageError::Unavailable { .. }) => 503,
            // 🔴 写锁竞争 → 409，**必须排在下面的 `Storage(_) => 500` 之前**。
            //
            // 它不是服务端故障：后台索引正持有写锁时用户改设置，两个写入者
            // 撞在 SQLite 单写锁上，等满 busy_timeout(5s) 后失败。
            // 这是单机单写者架构下的**正常瞬时状态**，用户重试通常就成功。
            // 报 500 的三重危害：① 看起来像程序坏了；② 被记进 ERROR 日志，
            // 淹没真正的故障；③ 前端不会重试，用户白等 5 秒还失败。
            //
            // 实测数据（合成 22000 资产项目，索引 21.9s）：
            // 39 次设置写入中 36 次 200（p50 仅 16ms），3 次撞锁等满 5s。
            Self::Storage(e) if e.is_busy() => 409,
            Self::Storage(_) => 500,
            Self::Job(e) => match e {
                projectassests_domain::JobError::NotFound(_) => 404,
                projectassests_domain::JobError::AlreadyRunning(_)
                | projectassests_domain::JobError::AlreadyFinished(_) => 409,
                projectassests_domain::JobError::Execution(_) => 500,
            },
            Self::Ai(e) => match e {
                projectassests_domain::AiError::NotConfigured => 424,
                // AI 被用户主动关闭：与"未配置"同样是前置条件未满足，
                // 不是服务端故障，因此绝不落到 5xx（否则会污染 error 日志）。
                projectassests_domain::AiError::Disabled => 424,
                projectassests_domain::AiError::Unauthorized => 401,
                projectassests_domain::AiError::RateLimited => 429,
                projectassests_domain::AiError::SensitiveBlocked => 403,
                projectassests_domain::AiError::Timeout(_) => 504,
                projectassests_domain::AiError::Cancelled => 499,
                _ => 502,
            },
            // 索引未建立 = 用户还没扫描，是"前置条件未满足"而非服务端故障。
            // 🔴 早前这里统一返回 500：用户第一次打开应用就看到"内部错误"，
            // 而正确提示是"请先扫描"。500 还会被记进 error 日志，
            // 让真正需要关注的故障淹没在新用户的正常空库里。
            Self::Search(projectassests_domain::SearchError::IndexNotReady) => 424,
            Self::Search(_) => 500,
            Self::Config(e) => match e {
                projectassests_domain::ConfigError::NoScanDirs => 424,
                _ => 400,
            },
            Self::Internal => 500,
        }
    }

    /// 机器可读的稳定错误码（前端据此分支，不能靠匹配 message 文本）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "bad_request",
            Self::NotFound(_) => "not_found",
            Self::Precondition(_) => "precondition_failed",
            Self::Conflict(_) => "conflict",
            // 库文件不可访问与"SQL 写错了"是两类完全不同的问题：
            // 前者用户能自救（检查磁盘/权限），后者是我们的缺陷。
            // 共用一个 code 的话，前端无法给出正确的引导文案。
            Self::Storage(projectassests_domain::StorageError::Unavailable { .. }) => {
                "storage_unavailable"
            }
            // 🔴 独立 code：前端据此决定是否自动重试（409 可重试，500 不该重试）。
            // 若让它落到下面的 `storage_error`，前端就无从区分，
            // 只能把两者都当成"服务器坏了"处理。
            Self::Storage(e) if e.is_busy() => "storage_busy",
            Self::Storage(_) => "storage_error",
            Self::Job(projectassests_domain::JobError::NotFound(_)) => "job_not_found",
            Self::Job(projectassests_domain::JobError::AlreadyRunning(_)) => "job_already_running",
            Self::Job(projectassests_domain::JobError::AlreadyFinished(_)) => "job_already_finished",
            Self::Job(_) => "job_error",
            Self::Ai(projectassests_domain::AiError::NotConfigured) => "llm_not_configured",
            // 🔴 必须有独立 code：下面的 `Self::Ai(_)` 通配符会把它吞进 "llm_error"，
            // 前端就无法区分"AI 被用户关了"（该引导去打开开关）与"模型真出错了"（该看日志）。
            // 这与 `citation_kind` 用 `_ => File` 吞掉新变体是同一类缺陷。
            Self::Ai(projectassests_domain::AiError::Disabled) => "llm_disabled",
            Self::Ai(projectassests_domain::AiError::Unauthorized) => "llm_unauthorized",
            Self::Ai(projectassests_domain::AiError::RateLimited) => "llm_rate_limited",
            Self::Ai(projectassests_domain::AiError::SensitiveBlocked) => "sensitive_blocked",
            Self::Ai(projectassests_domain::AiError::Timeout(_)) => "llm_timeout",
            Self::Ai(projectassests_domain::AiError::Cancelled) => "cancelled",
            Self::Ai(_) => "llm_error",
            Self::Search(projectassests_domain::SearchError::IndexNotReady) => "index_not_ready",
            Self::Search(_) => "search_error",
            Self::Config(projectassests_domain::ConfigError::NoScanDirs) => "no_scan_dirs",
            Self::Config(_) => "config_error",
            Self::Internal => "internal_error",
        }
    }

    /// 可操作的下一步建议；`None` 表示无明确引导。
    ///
    /// 🔴 面向"会编码的用户"：提示可以直接说术语与路径，
    /// 不需要过度解释基础概念，但必须指明**去哪个界面**操作。
    pub fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Ai(projectassests_domain::AiError::NotConfigured) => {
                "设置 → 大模型配置：启动 Ollama（`ollama serve` + `ollama pull qwen3:8b`），或填入云端 API Key".to_string()
            }
            Self::Ai(projectassests_domain::AiError::Disabled) => {
                "设置 → 扫描设置 → 打开「AI 分析」开关。\
已生成的画像仍可正常查看，关闭只阻止新的模型调用（Local-First：代码不因误点而外发）"
                    .to_string()
            }
            Self::Ai(projectassests_domain::AiError::Unauthorized) => {
                "API Key 无效或已过期，请在 设置 → 大模型配置 更新".to_string()
            }
            Self::Ai(projectassests_domain::AiError::RateLimited) => {
                "云端限流：稍后重试，或把路由切到本地模型（设置 → 大模型配置 → 路由）".to_string()
            }
            Self::Ai(projectassests_domain::AiError::Timeout(secs)) => {
                format!("模型 {secs}s 未响应。可换更快的模型，或降低 设置 中的输出长度")
            }
            Self::Ai(projectassests_domain::AiError::SensitiveBlocked) => {
                "该项目标记为敏感，按 Local-First 不走云端。如需云端分析，先在项目详情取消敏感标记".to_string()
            }
            Self::Config(projectassests_domain::ConfigError::NoScanDirs) => {
                "设置 → 扫描目录：添加代码根目录（如 `F:/CodeProject`），然后点「开始扫描」".to_string()
            }
            // 索引未就绪与"没有扫描目录"指向同一个操作入口，
            // 但成因不同：前者可能是清过派生数据，所以措辞要覆盖"重新扫描"。
            Self::Search(projectassests_domain::SearchError::IndexNotReady) => {
                "索引尚未建立：设置 → 扫描目录，执行一次扫描后即可检索".to_string()
            }
            // 🔴 任务类 hint 按**变体**给，不看消息文本。
            // 早期实现写成 `if msg.contains("进行中")`，而 domain 的实际文案是
            // "已有同类任务在运行"——匹配不上，hint 静默变成 None，
            // 用户只看到"冲突"却不知道该做什么。靠子串匹配决定是否给建议
            // 本身就是反模式：文案一改就失效，且没有任何编译期保护。
            Self::Job(projectassests_domain::JobError::AlreadyRunning(_)) => {
                "任务正在运行：侧栏可查看进度，或取消后重试".to_string()
            }
            Self::Job(projectassests_domain::JobError::AlreadyFinished(_)) => {
                "任务已结束，无需重复操作；如需重跑请重新触发".to_string()
            }
            // 任务不存在：最常见的原因是前端持有过期数据
            // （任务记录被 purge_finished 清理，或页面开着时进程重启过）。
            // 没有这条 hint 时用户只看到"任务不存在"，会以为数据丢了。
            Self::Job(projectassests_domain::JobError::NotFound(_)) => {
                "任务可能已被清理：刷新任务列表后重试".to_string()
            }
            // Precondition 的三种来源（目录不存在 / 未授权 / 尚未扫描）
            // 都指向同一个操作入口，因此给统一提示即可——具体原因由 message 说明。
            Self::Precondition(_) => {
                "在 设置 → 扫描目录 检查路径是否存在且已启用，然后重新扫描".to_string()
            }
            Self::Conflict(_) => {
                "当前状态不允许该操作：可刷新页面后重试".to_string()
            }
            Self::Storage(projectassests_domain::StorageError::Unavailable { path, .. }) => {
                format!("数据库文件不可访问：{path}。检查磁盘空间与读写权限")
            }
            // 🔴 写锁竞争的 hint 必须说清**为什么**要等，而不只是"稍后重试"。
            // 用户此刻多半正在索引，若只说"稍后重试"，他会反复点、反复等 5 秒、
            // 反复失败，最后认定产品坏了。告诉他"索引跑完就好"才是可操作的。
            Self::Storage(e) if e.is_busy() => {
                "数据库正忙：后台任务（扫描/索引）正在写入。等侧栏进度完成后再试，\
                 通常几秒内即可；反复失败请检查是否同时开着多个 projectAssests 实例".to_string()
            }
            _ => return None,
        })
    }

    /// 是否为服务端故障（5xx）。只有这类才记 error 日志：
    /// 把"用户没配模型"记成服务端错误，会让真正的故障淹没在噪音里。
    pub fn is_internal(&self) -> bool {
        self.status_code() >= 500
    }
}

impl From<projectassests_domain::ProjectAssestsError> for ServiceError {
    fn from(e: projectassests_domain::ProjectAssestsError) -> Self {
        use projectassests_domain::ProjectAssestsError as E;
        match e {
            E::Storage(s) => Self::Storage(s),
            E::Job(j) => Self::Job(j),
            E::Ai(a) => Self::Ai(a),
            E::Search(s) => Self::Search(s),
            E::Config(c) => Self::Config(c),
            E::NotFound(m) => Self::NotFound(m),
            E::BadRequest(m) => Self::Invalid(m),
            // 扫描器错误在服务层统一降级为 Precondition/Internal：
            // 目录不存在、未授权都属于"用户需要先准备好"，不是程序故障
            E::Scanner(projectassests_domain::ScannerError::DirNotFound(p)) => {
                Self::Precondition(format!("目录不存在或已移动：{p}"))
            }
            E::Scanner(projectassests_domain::ScannerError::NotAuthorized(p)) => {
                Self::Precondition(format!("目录未授权：{p}"))
            }
            E::Scanner(projectassests_domain::ScannerError::NotADirectory(p)) => {
                Self::Invalid(format!("不是目录：{p}"))
            }
            E::Scanner(_) => Self::Internal,
            E::Asset(_) => Self::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{AiError, ConfigError, JobError, ScannerError, StorageError};

    #[test]
    fn in_memory_context_is_constructible() {
        let ctx = ServiceContext::in_memory().unwrap();
        assert_eq!(ctx.db_path, ":memory:");
        assert_eq!(ctx.db.projects().count().unwrap(), 0);
        // now() 必须可用（service 层统一从这里取时间，便于将来注入）
        assert!(ctx.now().timestamp() > 0);
    }

    #[test]
    fn file_context_creates_database_and_reaps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projectassests.db");
        // 先造一条僵尸 running 任务
        {
            let ctx = ServiceContext::open(&path).unwrap();
            ctx.db
                .jobs()
                .create("scan_project-zombie", projectassests_domain::JobType::ScanProject, None)
                .unwrap();
            let mut job = ctx.db.jobs().get("scan_project-zombie").unwrap().unwrap();
            job.status = projectassests_domain::JobStatus::Running;
            ctx.db.jobs().update(&job).unwrap();
        }
        // 重开：僵尸必须被收割，否则扫描功能永久不可用
        let ctx2 = ServiceContext::open(&path).unwrap();
        assert!(!ctx2
            .db
            .jobs()
            .has_active_of_type(projectassests_domain::JobType::ScanProject)
            .unwrap());
        assert_eq!(ctx2.db_path, path.display().to_string());
    }

    #[test]
    fn context_clone_shares_database() {
        let ctx = ServiceContext::in_memory().unwrap();
        let c2 = ctx.clone();
        assert_eq!(c2.db_path, ctx.db_path);
        // 一侧写入另一侧可见（共享同一个 Arc<Database>）
        assert_eq!(ctx.db.projects().count().unwrap(), 0);
        assert_eq!(c2.db.projects().count().unwrap(), 0);
    }

    // ── 错误映射：状态码 / code / hint 三者必须一致 ───────────────

    #[test]
    fn status_codes_are_semantically_correct() {
        let cases: Vec<(ServiceError, u16, &str)> = vec![
            (ServiceError::Invalid("x".into()), 400, "bad_request"),
            (ServiceError::NotFound("项目".into()), 404, "not_found"),
            (ServiceError::Precondition("未扫描".into()), 424, "precondition_failed"),
            (ServiceError::Conflict("进行中".into()), 409, "conflict"),
            (
                ServiceError::Ai(AiError::NotConfigured),
                424,
                "llm_not_configured",
            ),
            (ServiceError::Ai(AiError::Unauthorized), 401, "llm_unauthorized"),
            (ServiceError::Ai(AiError::RateLimited), 429, "llm_rate_limited"),
            (ServiceError::Ai(AiError::Timeout(30)), 504, "llm_timeout"),
            (
                ServiceError::Ai(AiError::SensitiveBlocked),
                403,
                "sensitive_blocked",
            ),
            (
                ServiceError::Job(JobError::AlreadyRunning("扫描".into())),
                409,
                "job_already_running",
            ),
            (
                ServiceError::Job(JobError::NotFound("j".into())),
                404,
                "job_not_found",
            ),
            (
                ServiceError::Config(ConfigError::NoScanDirs),
                424,
                "no_scan_dirs",
            ),
            // 🔴 索引未就绪必须是 424 而非 500：新用户第一次打开应用就是空库，
            // 报"内部错误"既误导用户又污染 error 日志
            (
                ServiceError::Search(projectassests_domain::SearchError::IndexNotReady),
                424,
                "index_not_ready",
            ),
            (
                ServiceError::Storage(StorageError::Unavailable {
                    path: "/x.db".into(),
                    reason: "拒绝访问".into(),
                }),
                503,
                "storage_unavailable",
            ),
            (ServiceError::Internal, 500, "internal_error"),
        ];
        for (err, status, code) in cases {
            assert_eq!(err.status_code(), status, "状态码不符: {code}");
            assert_eq!(err.code(), code, "错误码不符: {err}");
        }
    }

    /// 🔴 只有 5xx 算内部错误。用户侧问题（没配模型、参数错）记 error 日志
    /// 会让真正的故障被淹没——这条判断直接决定日志可用性。
    #[test]
    fn only_5xx_is_internal() {
        assert!(!ServiceError::Ai(AiError::NotConfigured).is_internal());
        assert!(!ServiceError::Invalid("x".into()).is_internal());
        assert!(!ServiceError::NotFound("x".into()).is_internal());
        assert!(!ServiceError::Precondition("x".into()).is_internal());
        assert!(ServiceError::Internal.is_internal());
        assert!(ServiceError::Storage(StorageError::Migration("boom".into())).is_internal());
    }

    /// 需要用户行动的错误必须带 hint，且要指明具体去哪操作。
    #[test]
    fn actionable_errors_carry_concrete_hints() {
        let needs_hint = vec![
            ServiceError::Ai(AiError::NotConfigured),
            ServiceError::Ai(AiError::Unauthorized),
            ServiceError::Ai(AiError::RateLimited),
            ServiceError::Ai(AiError::SensitiveBlocked),
            ServiceError::Ai(AiError::Timeout(30)),
            ServiceError::Config(ConfigError::NoScanDirs),
            ServiceError::Job(JobError::AlreadyRunning("扫描".into())),
            ServiceError::Precondition("请先扫描".into()),
        ];
        for err in needs_hint {
            let hint = err.hint().unwrap_or_else(|| panic!("{} 缺少 hint", err.code()));
            assert!(!hint.is_empty());
            // hint 必须可操作：指明界面位置、或给出可执行命令。
            // "项目详情"也算——取消敏感标记确实要在那里操作。
            assert!(
                hint.contains("设置")
                    || hint.contains("侧栏")
                    || hint.contains("项目详情")
                    || hint.contains("ollama"),
                "{} 的 hint 应可操作: {hint}",
                err.code()
            );
        }
    }

    /// 未配置模型的 hint 要直接给出命令——面向会编码的用户，不必绕弯子。
    #[test]
    fn llm_hint_includes_runnable_command() {
        let hint = ServiceError::Ai(AiError::NotConfigured).hint().unwrap();
        assert!(hint.contains("ollama serve"), "实际: {hint}");
        assert!(hint.contains("ollama pull"), "实际: {hint}");
    }

    /// 无关错误不该硬编 hint（否则会给出误导性建议）。
    #[test]
    fn non_actionable_errors_have_no_hint() {
        assert!(ServiceError::Internal.hint().is_none());
        assert!(ServiceError::Invalid("参数错".into()).hint().is_none());
        assert!(ServiceError::NotFound("项目".into()).hint().is_none());
    }

    /// 🔴 写锁竞争（SQLITE_BUSY）必须是 **409 可重试**，不是 500。
    ///
    /// 这是本条映射存在的全部理由。报 500 的三重危害：
    /// ① 用户以为程序坏了；② 被记进 ERROR 日志、淹没真正的故障；
    /// ③ 前端不会重试，用户白等 5 秒（busy_timeout）还失败。
    ///
    /// 实测：索引 22000 资产的大项目时，39 次设置写入有 3 次撞锁。
    /// 分块提交把大多数写入降到 16ms，但竞争无法归零——
    /// 所以"撞上了"必须被如实报成可重试状态，而不是伪装成故障。
    #[test]
    fn storage_busy_is_retryable_conflict_not_server_error() {
        let busy = ServiceError::Storage(StorageError::Busy {
            context: "保存扫描设置".into(),
        });
        assert_eq!(busy.status_code(), 409, "写锁竞争是可重试状态，不是故障");
        assert_eq!(
            busy.code(),
            "storage_busy",
            "前端需要独立 code 才能决定要不要重试"
        );
        assert!(
            !busy.is_internal(),
            "409 不得被当作服务端故障记进 ERROR 日志"
        );
        let hint = busy.hint().expect("必须给出可操作的下一步");
        // 🔴 hint 要说清"为什么忙"，不能只说"稍后重试"：
        // 用户多半正在索引，只说重试会让他反复点、反复等 5 秒、反复失败。
        assert!(hint.contains("索引"), "hint 应指出等待索引完成: {hint}");
    }

    /// 🔴 **反向断言**：其他存储故障必须仍是 500，不得被误判成 busy。
    ///
    /// 只测"busy 是 409"的话，一个把 `Storage(_) => 500` 整条改成 409 的
    /// 变异照样通过——那会让 SQL 写错、库损坏、约束冲突全变成"稍后重试"，
    /// 用户被引导去重试一个永远不会成功的操作。
    #[test]
    fn other_storage_errors_stay_500() {
        for err in [
            StorageError::Sqlite {
                context: "写入项目".into(),
                reason: "UNIQUE constraint failed: projects.path".into(),
            },
            StorageError::Migration("迁移失败".into()),
            StorageError::Sqlite {
                context: "读取".into(),
                reason: "database disk image is malformed".into(),
            },
        ] {
            let e = ServiceError::Storage(err);
            assert_eq!(e.status_code(), 500, "{e} 应是服务端故障");
            assert_eq!(e.code(), "storage_error", "{e} 不得借用 storage_busy");
            assert!(e.is_internal(), "{e} 必须记 ERROR 日志");
        }
    }

    /// 存储不可用属于用户环境问题，告知路径有助于自查（不是敏感信息）。
    #[test]
    fn storage_unavailable_hint_includes_path() {
        let err = ServiceError::Storage(StorageError::Unavailable {
            path: "C:/data/projectassests.db".into(),
            reason: "拒绝访问".into(),
        });
        assert_eq!(err.status_code(), 503);
        assert!(err.hint().unwrap().contains("projectassests.db"));
    }

    /// 内部错误不得泄漏底层细节。
    #[test]
    fn internal_error_hides_details() {
        let err = ServiceError::Internal;
        assert!(!err.to_string().contains("malformed"));
        assert!(err.to_string().contains("日志"));
    }

    // ── ProjectAssestsError → ServiceError 转换 ──────────────────────────

    #[test]
    fn scanner_dir_not_found_becomes_precondition() {
        // 目录不存在是"用户需要准备好"，不是程序故障 → 424 而非 500
        let err = ServiceError::from(projectassests_domain::ProjectAssestsError::Scanner(
            ScannerError::DirNotFound("/gone".into()),
        ));
        assert_eq!(err.status_code(), 424);
        assert!(!err.is_internal());
        assert!(err.to_string().contains("/gone"));
    }

    #[test]
    fn scanner_not_a_directory_becomes_invalid() {
        let err = ServiceError::from(projectassests_domain::ProjectAssestsError::Scanner(
            ScannerError::NotADirectory("/a/file.txt".into()),
        ));
        assert_eq!(err.status_code(), 400);
        assert_eq!(err.code(), "bad_request");
    }

    #[test]
    fn domain_errors_map_through() {
        assert_eq!(
            ServiceError::from(projectassests_domain::ProjectAssestsError::NotFound("x".into())).code(),
            "not_found"
        );
        assert_eq!(
            ServiceError::from(projectassests_domain::ProjectAssestsError::BadRequest("x".into())).code(),
            "bad_request"
        );
        assert_eq!(
            ServiceError::from(projectassests_domain::ProjectAssestsError::Ai(AiError::NotConfigured)).code(),
            "llm_not_configured"
        );
        // 引擎内部错误统一降级为 Internal，不透传细节
        assert_eq!(
            ServiceError::from(projectassests_domain::ProjectAssestsError::Scanner(ScannerError::Cancelled)).code(),
            "internal_error"
        );
    }
}
