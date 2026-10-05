//! 任务用例：触发扫描 / 查询任务 / 取消 / 进度订阅。
//!
//! # 为什么触发扫描要经过 service 而不是前端直接调引擎
//! 《技术设计书》§15 的红线是"UI 不允许直接调 `scan()`，一切走任务队列"。
//! 但"提交给队列"这件事本身也有业务规则：
//! - 目录从哪来（请求指定 > 设置里已启用的目录）
//! - 没有可用目录时要给引导而不是报"参数为空"
//! - 提交成功后要立刻返回任务 id，让前端能马上订阅进度
//!
//! 这些规则若写在 axum handler 里，Tauri 那边就得再写一遍。
//!
//! # 进度的两种消费方式
//! 1. **拉取**（`progress` / `list`）：前端定时轮询，简单可靠
//! 2. **订阅**（`subscribe`）：SSE / Tauri event 的载荷来源
//!
//! 两者共用 `ProgressEvent`，适配器决定用哪种传输。

use serde::{Deserialize, Serialize};
use projectassests_domain::{Job, JobType};
use projectassests_jobs::ProgressEvent;

use crate::context::{ServiceContext, ServiceError};

/// 任务列表默认条数。
fn default_limit() -> u32 {
    20
}

/// 任务列表上限。
pub const MAX_LIST_LIMIT: u32 = 100;

/// 触发扫描的请求。
///
/// `dirs` 为空时回落到设置里已启用的扫描目录——
/// 首页那个"开始扫描"按钮不带任何参数，它该扫的就是用户配好的目录。
#[derive(Debug, Clone, Deserialize)]
pub struct ScanRequest {
    /// 要扫描的目录（绝对路径）。空 = 用设置里已启用的目录
    #[serde(default)]
    pub dirs: Vec<String>,
    /// 是否做 Git 历史分析（大仓库很慢，允许用户关掉）
    #[serde(default)]
    pub analyze_git: Option<bool>,
    /// 扫描完成后是否自动续跑「索引 → 洞察」全链。
    ///
    /// 🔴 默认 **true**：《技术设计书》§13 定义的三级流水线要求
    /// Level 0 扫描之后 Level 1 在后台自动索引（用户看到 `Indexing... 127/183`），
    /// 随后资产与洞察陆续出现。若默认 false，用户扫完看到的是
    /// "发现 2 个项目"但资产页与洞察页全是空的——
    /// 产品最核心的价值（发现可复用资产）根本不会自动出现，
    /// 而他没有任何界面可以触发后续阶段。
    ///
    /// 显式传 false 用于"只想知道有哪些项目"的快速探测。
    #[serde(default = "default_chain")]
    pub chain: bool,
}

fn default_chain() -> bool {
    true
}

impl Default for ScanRequest {
    /// 🔴 手写而非 derive：derive 会让 `chain` 为 false，
    /// 与 serde 默认值（true）不一致。于是"代码里构造的扫描请求"
    /// 不续跑、"HTTP 传来的"续跑，两条路径行为不同——
    /// 而这种不一致恰好只在测试里构造请求时暴露，最容易漏。
    fn default() -> Self {
        Self {
            dirs: Vec::new(),
            analyze_git: None,
            chain: default_chain(),
        }
    }
}

/// 触发索引的请求。
///
/// `project_id` 为 `None` 时索引**全部项目**。
///
/// 🔴 这个能力必须暴露：跨项目分析（重复能力检测、组合机会）
/// 只有在多个项目都索引过之后才有素材，只支持单项目索引的话，
/// "你在两个项目里重复实现了 TaskQueue"这类核心洞察永远算不出来。
/// 引擎侧 `IndexCodeHandler` 本来就支持全量（`only = None`），
/// 之前是 service 层把 `project_id` 设成必填、把这个能力挡住了。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IndexRequest {
    #[serde(default)]
    pub project_id: Option<String>,
}

/// 提交结果。
///
/// 🔴 带上 `job_type_label` 与 `subscribed`：前端提交后要在 toast 里
/// 说清"已开始扫描项目"，而不是干巴巴一个 id。
#[derive(Debug, Clone, Serialize)]
pub struct SubmitResponse {
    pub job_id: String,
    pub job_type: String,
    pub job_type_label: String,
    /// 面向用户的确认文案（toast 直接用）
    pub message: String,
}

/// 任务视图。
#[derive(Debug, Clone, Serialize)]
pub struct JobView {
    pub id: String,
    pub job_type: String,
    pub job_type_label: String,
    pub status: String,
    pub status_label: String,
    /// 0-100 整数百分比（前端进度条直接用，不必自己算）
    pub percent: u8,
    pub progress: f64,
    pub stage: Option<String>,
    pub processed: Option<u64>,
    pub total: Option<u64>,
    /// "127 / 183" 形式的计数文本；无总数时为 `None`（前端隐藏该段）
    pub counter_text: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// 是否还能取消（终态任务点取消没意义，前端据此禁用按钮）
    pub cancellable: bool,
}

/// 任务列表响应。
#[derive(Debug, Clone, Serialize)]
pub struct JobListPage {
    pub items: Vec<JobView>,
    /// 进行中的任务（queued + running），侧栏角标用
    pub active: Vec<JobView>,
    /// 全局进度：所有进行中任务的平均进度。无进行中任务时为 `None`
    pub overall_progress: Option<f64>,
    pub total: usize,
}

/// 进度订阅快照（拉取式接口用）。
#[derive(Debug, Clone, Serialize)]
pub struct ProgressSnapshot {
    /// 最近一次进度事件；从未跑过任务时为 `None`
    pub current: Option<ProgressEvent>,
    /// 当前订阅者数量（诊断用：判断 SSE 连接是否泄漏）
    pub subscribers: usize,
}

// ══════════════════════════════════════════════════════════════════
// 触发
// ══════════════════════════════════════════════════════════════════

/// 触发一次扫描。
///
/// 🔴 目录解析顺序：**请求指定 > 设置里已启用的目录**。
/// 两者都没有时返回 `Precondition` 并给出引导，而不是让引擎去扫空列表
/// （那样会"成功完成"却什么都没扫，用户以为功能坏了）。
pub async fn scan(ctx: &ServiceContext, req: &ScanRequest) -> Result<SubmitResponse, ServiceError> {
    let dirs = resolve_scan_dirs(ctx, req)?;
    let analyze_git = req.analyze_git.unwrap_or(true);

    // 🔴 chain 透传给引擎：默认 true，扫描完成后自动跑「索引 → 洞察」，
    // 让产品核心价值（可复用资产、洞察）自动出现，而不是扫完一片空白。
    let payload = serde_json::json!({
        "dirs": dirs,
        "analyze_git": analyze_git,
        "chain": req.chain,
    });
    let job_id = ctx.jobs.submit(JobType::ScanProject, Some(payload)).await?;

    // 消息要如实告知用户接下来会自动发生什么：
    // 只说"已开始扫描"的话，几分钟后洞察突然出现会让用户困惑"哪来的"。
    let message = if req.chain {
        format!("已开始扫描 {} 个目录，完成后将自动索引并生成洞察", dirs.len())
    } else {
        format!("已开始扫描 {} 个目录", dirs.len())
    };

    Ok(SubmitResponse {
        job_id: job_id.clone(),
        job_type: JobType::ScanProject.as_str().to_string(),
        job_type_label: JobType::ScanProject.label_zh().to_string(),
        message,
    })
}

/// 解析待扫描目录。
///
/// 顺带做两件前端做不了的事：
/// 1. 去重与 trim（用户可能粘贴了带空格或重复的路径）
/// 2. 过滤掉**未启用**的目录（设置里可以临时停用而不删除）
fn resolve_scan_dirs(ctx: &ServiceContext, req: &ScanRequest) -> Result<Vec<String>, ServiceError> {
    let mut dirs: Vec<String> = Vec::new();

    for d in &req.dirs {
        let t = d.trim();
        if t.is_empty() {
            continue;
        }
        let s = t.to_string();
        if !dirs.contains(&s) {
            dirs.push(s);
        }
    }

    if dirs.is_empty() {
        let settings = ctx.db.settings().get_or_default()?;
        for d in &settings.scan.dirs {
            // 🔴 只取启用的：被用户停用的目录若仍被扫进来，
            // "停用"这个操作就完全无效，而且用户不会知道为什么。
            if d.enabled {
                let t = d.path.trim();
                if !t.is_empty() && !dirs.contains(&t.to_string()) {
                    dirs.push(t.to_string());
                }
            }
        }
    }

    if dirs.is_empty() {
        return Err(ServiceError::Precondition(
            "没有可扫描的目录".to_string(),
        ));
    }
    Ok(dirs)
}

/// 触发单项目重建索引。
pub async fn index(ctx: &ServiceContext, req: &IndexRequest) -> Result<SubmitResponse, ServiceError> {
    // 🔴 归一化**只做一次**，payload 与 message 都从它派生。
    //
    // 早期写法是 payload 用 `non_empty(&req.project_id)`、
    // message 用 `match &req.project_id { Some(_) => … }`——两个判断来源不同，
    // 于是 `Some("")` 会跑全量索引却回一句"已开始重建该项目索引"。
    // 消息与实际行为不一致比报错更糟：用户等着看单项目结果，
    // 实际引擎正在重索引全库，而他没有任何线索察觉。
    //
    // 规则：只要不是非空字符串，就是全量。
    let target = non_empty(&req.project_id);

    // 🔴 `project_id` 缺省 = 索引全部项目（跨项目分析的素材来源），
    // 给了则只重建那一个（项目详情页的"重建索引"按钮）。
    // 两条路径的前置校验不同，必须分开处理。
    let payload = match &target {
        Some(pid) => {
            // 先确认项目存在：否则任务会"成功完成"但什么都没做，
            // 用户看到的是转圈结束后毫无变化。
            if ctx.db.projects().get(pid)?.is_none() {
                return Err(ServiceError::NotFound(format!("项目 {pid}")));
            }
            serde_json::json!({ "project_id": pid })
        }
        None => {
            let projects = ctx.db.projects().count()?;
            if projects == 0 {
                // 空库上索引会得到"没有可索引的项目"的失败任务，
                // 提前拦住并说明原因，比让用户看一条红色失败记录清楚
                return Err(ServiceError::Precondition(
                    "还没有任何项目，无法索引".to_string(),
                ));
            }
            serde_json::json!({})
        }
    };
    let job_id = ctx.jobs.submit(JobType::IndexCode, Some(payload)).await?;

    Ok(SubmitResponse {
        job_id,
        job_type: JobType::IndexCode.as_str().to_string(),
        job_type_label: JobType::IndexCode.label_zh().to_string(),
        message: match &target {
            Some(_) => "已开始重建该项目索引".to_string(),
            None => "已开始索引全部项目".to_string(),
        },
    })
}

/// 触发洞察生成（跨项目分析）。
pub async fn generate_insights(ctx: &ServiceContext) -> Result<SubmitResponse, ServiceError> {
    // 没有项目就没有可分析的素材，提前拦住比让任务跑完再报"0 条洞察"更清楚
    let projects = ctx.db.projects().count()?;
    if projects == 0 {
        return Err(ServiceError::Precondition(
            "还没有任何项目，无法生成洞察".to_string(),
        ));
    }

    let job_id = ctx.jobs.submit(JobType::GenerateInsight, None).await?;
    Ok(SubmitResponse {
        job_id,
        job_type: JobType::GenerateInsight.as_str().to_string(),
        job_type_label: JobType::GenerateInsight.label_zh().to_string(),
        message: format!("已开始分析 {projects} 个项目"),
    })
}

/// 取消任务。
///
/// 返回更新后的任务视图，前端可直接用它刷新那一行。
pub fn cancel(ctx: &ServiceContext, job_id: &str) -> Result<JobView, ServiceError> {
    let id = job_id.trim();
    if id.is_empty() {
        return Err(ServiceError::Invalid("任务 id 不能为空".to_string()));
    }
    ctx.jobs.cancel(id)?;
    get(ctx, id)
}

// ══════════════════════════════════════════════════════════════════
// 查询
// ══════════════════════════════════════════════════════════════════

/// 单个任务详情。
pub fn get(ctx: &ServiceContext, job_id: &str) -> Result<JobView, ServiceError> {
    let job = ctx
        .db
        .jobs()
        .get(job_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("任务 {job_id}")))?;
    Ok(job_view(&job, ctx.now()))
}

/// 任务列表（含进行中任务与全局进度）。
pub fn list(ctx: &ServiceContext, limit: Option<u32>) -> Result<JobListPage, ServiceError> {
    let lim = limit.unwrap_or(default_limit()).clamp(1, MAX_LIST_LIMIT);

    let recent = ctx.jobs.recent(lim)?;
    let now = ctx.now();
    let items: Vec<JobView> = recent.iter().map(|j| job_view(j, now)).collect();
    let active: Vec<JobView> = ctx
        .jobs
        .running()?
        .iter()
        .map(|j| job_view(j, now))
        .collect();

    Ok(JobListPage {
        items,
        active,
        overall_progress: ctx.jobs.overall_progress()?,
        total: ctx.db.jobs().count()?,
    })
}

/// 进行中的任务（侧栏角标 / 顶栏进度条）。
///
/// 独立于 `list`：侧栏每秒都要刷，不需要拖回整个历史列表。
pub fn active(ctx: &ServiceContext) -> Result<Vec<JobView>, ServiceError> {
    let now = ctx.now();
    Ok(ctx
        .jobs
        .running()?
        .iter()
        .map(|j| job_view(j, now))
        .collect())
}

/// 进度快照（拉取式）。
pub fn progress(ctx: &ServiceContext) -> Result<ProgressSnapshot, ServiceError> {
    Ok(ProgressSnapshot {
        current: ctx.jobs.latest_progress(),
        subscribers: ctx.jobs.subscriber_count(),
    })
}

/// 订阅进度变化（推送式：SSE / Tauri event）。
///
/// 返回订阅句柄而非直接的流：适配器决定怎么把它变成
/// SSE 的 `data:` 帧或 Tauri 的 `emit`，service 不感知传输协议。
pub fn subscribe(ctx: &ServiceContext) -> projectassests_jobs::ProgressSubscription {
    ctx.jobs.subscribe()
}

// ══════════════════════════════════════════════════════════════════
// 活动流
// ══════════════════════════════════════════════════════════════════

/// 活动条目视图。
#[derive(Debug, Clone, Serialize)]
pub struct ActivityView {
    pub id: String,
    pub icon: String,
    pub title: String,
    pub detail: String,
    pub created_at: String,
    pub relative: String,
}

/// 最近活动（首页"最近动态"区块）。
pub fn activities(ctx: &ServiceContext, limit: Option<u32>) -> Result<Vec<ActivityView>, ServiceError> {
    let lim = limit.unwrap_or(10).clamp(1, 50);
    Ok(ctx
        .db
        .activities()
        .recent(lim)?
        .iter()
        .map(|a| ActivityView {
            id: a.id.clone(),
            icon: a.icon.as_str().to_string(),
            title: a.title.clone(),
            detail: a.detail.clone(),
            created_at: a.created_at.clone(),
            relative: a.relative.clone(),
        })
        .collect())
}

// ══════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════

/// trim 后为空则视为"未提供"。
///
/// 🔴 `project_id: Some("")` 与 `Some("  ")` 都必须当成 `None`（全量索引），
/// 否则会拿空串去查项目、报"项目不存在"，
/// 而用户真正想要的是"索引全部"。前端把清空后的输入框传成空串很常见。
fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn job_view(j: &Job, now: chrono::DateTime<chrono::Utc>) -> JobView {
    JobView {
        id: j.id.clone(),
        job_type: j.job_type.as_str().to_string(),
        job_type_label: j.job_type.label_zh().to_string(),
        status: j.status.as_str().to_string(),
        status_label: j.status.label_zh().to_string(),
        percent: (j.progress.clamp(0.0, 1.0) * 100.0).round() as u8,
        progress: j.progress,
        stage: j.stage.clone(),
        processed: j.processed,
        total: j.total,
        counter_text: match (j.processed, j.total) {
            (Some(p), Some(t)) if t > 0 => Some(format!("{p} / {t}")),
            _ => None,
        },
        error: j.error.clone(),
        created_at: j.created_at.clone(),
        updated_at: projectassests_storage::relative_time(&j.updated_at, now),
        cancellable: !j.status.is_terminal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{CodeStats, JobStatus, Project, ProjectStatus, ScanFacts, Settings};

    fn ctx() -> ServiceContext {
        ServiceContext::in_memory().unwrap()
    }

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("/tmp/{id}"),
            description: String::new(),
            language: "Rust".into(),
            framework: "-".into(),
            created_at: None,
            updated_at: None,
            last_commit_at: None,
            status: ProjectStatus::Active,
            health_score: 50,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats::default(),
            scan: ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn save_dirs(c: &ServiceContext, dirs: Vec<(&str, bool)>) {
        let mut s = c.db.settings().get_or_default().unwrap();
        s.scan.dirs = dirs
            .into_iter()
            .map(|(path, enabled)| projectassests_domain::ScanDir {
                path: path.to_string(),
                enabled,
                added_at: projectassests_storage::now_utc(),
                last_scanned_at: None,
                project_count: None,
            })
            .collect();
        c.db.settings().save(&s).unwrap();
    }

    // ── 目录解析 ─────────────────────────────────────────────────

    #[tokio::test]
    async fn scan_without_dirs_and_without_settings_is_precondition() {
        let c = ctx();
        let err = scan(&c, &ScanRequest::default()).await.unwrap_err();
        assert!(matches!(err, ServiceError::Precondition(_)), "{err:?}");
        assert_eq!(err.status_code(), 424);
        // 必须给出引导，否则用户只看到"没有可扫描的目录"却不知道去哪加
        assert!(err.hint().is_some(), "Precondition 应带 hint");
    }

    #[tokio::test]
    async fn scan_falls_back_to_enabled_settings_dirs() {
        let c = ctx();
        save_dirs(&c, vec![("/tmp/a", true), ("/tmp/b", false), ("/tmp/c", true)]);
        // 提交会真的起任务，但目录解析在提交前完成，失败会先返回。
        // 这里只验证"不会因缺目录而报 Precondition"。
        let r = scan(&c, &ScanRequest::default()).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(r.unwrap().job_type, "SCAN_PROJECT");
    }

    #[tokio::test]
    async fn scan_with_only_disabled_dirs_is_precondition() {
        let c = ctx();
        save_dirs(&c, vec![("/tmp/a", false)]);
        let err = scan(&c, &ScanRequest::default()).await.unwrap_err();
        assert!(matches!(err, ServiceError::Precondition(_)), "{err:?}");
    }

    #[tokio::test]
    async fn request_dirs_win_over_settings() {
        let c = ctx();
        save_dirs(&c, vec![("/tmp/from_settings", true)]);
        let r = scan(
            &c,
            &ScanRequest {
                dirs: vec!["/tmp/from_request".into()],
                analyze_git: None,
                chain: false,
            },
        )
        .await
        .unwrap();
        // 校验 payload 里存的是请求目录
        let job = c.db.jobs().get(&r.job_id).unwrap().unwrap();
        let payload = job.payload.unwrap();
        let dirs = payload["dirs"].as_array().unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], "/tmp/from_request");
    }

    #[tokio::test]
    async fn blank_and_duplicate_dirs_are_collapsed() {
        let c = ctx();
        let r = scan(
            &c,
            &ScanRequest {
                dirs: vec!["  ".into(), "/tmp/a".into(), "/tmp/a".into(), "/tmp/b".into()],
                analyze_git: Some(false),
                chain: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(r.message, "已开始扫描 2 个目录");
        let job = c.db.jobs().get(&r.job_id).unwrap().unwrap();
        let p = job.payload.unwrap();
        assert_eq!(p["dirs"].as_array().unwrap().len(), 2);
        assert_eq!(p["analyze_git"], false);
    }

    #[tokio::test]
    async fn analyze_git_defaults_to_true() {
        let c = ctx();
        let r = scan(
            &c,
            &ScanRequest {
                dirs: vec!["/tmp/a".into()],
                analyze_git: None,
                chain: false,
            },
        )
        .await
        .unwrap();
        let job = c.db.jobs().get(&r.job_id).unwrap().unwrap();
        assert_eq!(job.payload.unwrap()["analyze_git"], true);
    }

    // ── 索引 ────────────────────────────────────────────────────

    #[tokio::test]
    async fn index_unknown_project_is_404() {
        let c = ctx();
        // 🔴 回归：曾经直接提交，任务"成功完成"但什么都没做
        let err = index(
            &c,
            &IndexRequest {
                project_id: Some("ghost".into()),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)), "{err:?}");
    }

    /// 🔴 语义变更：空 project_id 现在表示"索引全部项目"，不再是参数错误。
    ///
    /// 跨项目分析（重复能力检测、组合机会）只有在多个项目都索引过后才有素材。
    /// 之前把 project_id 设成必填、挡住全量索引，等于让产品最核心的
    /// "你在两个项目里重复实现了 X"这类洞察永远算不出来。
    #[tokio::test]
    async fn index_blank_id_means_index_all() {
        // 空串与全空白都要当成"未提供" = 全量。
        // 🔴 每个变体用独立 context：三个都会真的提交 IndexCode 任务，
        // 共用一个库的话第二个会撞上第一个还活跃的任务（AlreadyRunning）——
        // 那是引擎的正确行为，不是被测逻辑的问题。
        for blank in [None, Some(String::new()), Some("   ".into())] {
            let c = ctx();
            c.db.projects().upsert(&project("p1", "项目一")).unwrap();

            let r = index(&c, &IndexRequest { project_id: blank })
                .await
                .expect("全量索引不该报错");
            assert_eq!(r.job_type, "INDEX_CODE");
            assert_eq!(r.message, "已开始索引全部项目");
            // payload 不带 project_id：引擎据此走全量分支
            let job = c.db.jobs().get(&r.job_id).unwrap().unwrap();
            let payload = job.payload.unwrap();
            assert!(
                payload.get("project_id").is_none(),
                "全量索引的 payload 不该带 project_id: {payload}"
            );
        }
    }

    /// 🔴 回归：消息必须与实际提交的任务一致。
    ///
    /// 缺陷曾是 payload 用 `non_empty(project_id)` 判断（空串→全量），
    /// message 却用 `match project_id { Some(_) => 单项目 }` 判断——
    /// 于是传 `Some("")` 时任务跑的是全量索引，回给用户的却是
    /// "已开始重建该项目索引"。行为与提示不一致比报错更难发现。
    #[tokio::test]
    async fn index_message_matches_the_submitted_scope() {
        for (input, expected_message, expect_project_id) in [
            (None, "已开始索引全部项目", false),
            (Some(String::new()), "已开始索引全部项目", false),
            (Some("   ".to_string()), "已开始索引全部项目", false),
        ] {
            let c = ctx();
            c.db.projects().upsert(&project("p1", "项目一")).unwrap();
            let r = index(&c, &IndexRequest { project_id: input })
                .await
                .unwrap();
            assert_eq!(r.message, expected_message);
            let job = c.db.jobs().get(&r.job_id).unwrap().unwrap();
            let payload = job.payload.unwrap();
            assert_eq!(
                payload.get("project_id").is_some(),
                expect_project_id,
                "payload 与 message 必须描述同一个范围: {payload}"
            );
        }
    }

    /// 空库上请求全量索引：提前拦成 Precondition，而不是提交一个必然失败的任务。
    #[tokio::test]
    async fn index_all_without_projects_is_precondition() {
        let c = ctx();
        let err = index(&c, &IndexRequest { project_id: None })
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::Precondition(_)), "{err:?}");
        assert!(err.to_string().contains("还没有任何项目"), "{err}");
    }

    #[tokio::test]
    async fn index_known_project_submits() {
        let c = ctx();
        c.db.projects().upsert(&project("p1", "项目一")).unwrap();
        let r = index(
            &c,
            &IndexRequest {
                project_id: Some("p1".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(r.job_type, "INDEX_CODE");
        assert_eq!(r.message, "已开始重建该项目索引");
        // 单项目 payload 必须带 project_id，否则引擎会误索引全库
        let job = c.db.jobs().get(&r.job_id).unwrap().unwrap();
        assert_eq!(job.payload.unwrap()["project_id"], "p1");
    }

    // ── 洞察 ────────────────────────────────────────────────────

    #[tokio::test]
    async fn insights_without_projects_is_precondition() {
        let c = ctx();
        let err = generate_insights(&c).await.unwrap_err();
        assert!(matches!(err, ServiceError::Precondition(_)), "{err:?}");
        assert!(err.to_string().contains("还没有任何项目"));
    }

    #[tokio::test]
    async fn insights_with_projects_submits() {
        let c = ctx();
        c.db
            .projects()
            .upsert_batch(&[project("p1", "项目一"), project("p2", "项目二")])
            .unwrap();
        let r = generate_insights(&c).await.unwrap();
        assert_eq!(r.message, "已开始分析 2 个项目");
    }

    // ── 查询与取消 ──────────────────────────────────────────────

    #[tokio::test]
    async fn get_missing_job_is_404() {
        let c = ctx();
        let err = get(&c, "ghost").unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
        assert_eq!(err.status_code(), 404);
    }

    #[tokio::test]
    async fn cancel_missing_job_is_404() {
        let c = ctx();
        // 注意：这里得到的是 `ServiceError::Job(JobError::NotFound)` 而非
        // `ServiceError::NotFound`——取消走引擎，错误形状由引擎决定。
        // 🔴 因此断言**契约**（404 + 有引导）而非内部变体：
        // 适配器只关心状态码与 hint，变体形状改了不该让这条测试失败。
        let err = cancel(&c, "ghost").unwrap_err();
        assert_eq!(err.status_code(), 404, "{err:?}");
        assert!(err.hint().is_some(), "取消失败应告诉用户下一步做什么");
    }

    #[tokio::test]
    async fn cancel_terminal_job_is_conflict() {
        let c = ctx();
        let r = scan(
            &c,
            &ScanRequest {
                dirs: vec!["/tmp/a".into()],
                analyze_git: Some(false),
                chain: false,
            },
        )
        .await
        .unwrap();

        // 等任务进入终态（目录不存在会很快失败）
        let mut view = get(&c, &r.job_id).unwrap();
        for _ in 0..100 {
            if !view.cancellable {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            view = get(&c, &r.job_id).unwrap();
        }
        assert!(!view.cancellable, "任务应已进入终态: {view:?}");

        // 🔴 再取消必须报 409 而不是 500：这是"你点晚了"，不是程序故障
        let err = cancel(&c, &r.job_id).unwrap_err();
        assert_eq!(err.status_code(), 409, "{err:?}");
        assert!(err.hint().is_some(), "应说明任务已结束");
    }

    #[tokio::test]
    async fn cancel_blank_id_is_invalid() {
        let c = ctx();
        assert!(matches!(cancel(&c, "  ").unwrap_err(), ServiceError::Invalid(_)));
    }

    #[tokio::test]
    async fn list_reports_active_and_overall_progress() {
        let c = ctx();
        let r = scan(
            &c,
            &ScanRequest {
                dirs: vec!["/tmp/a".into()],
                analyze_git: Some(false),
                chain: false,
            },
        )
        .await
        .unwrap();

        let page = list(&c, None).unwrap();
        assert_eq!(page.total, 1);
        assert!(!page.items.is_empty());
        let view = &page.items[0];
        assert_eq!(view.id, r.job_id);
        assert_eq!(view.job_type_label, "扫描项目");
        // 刚提交时是 queued，可取消
        assert!(view.cancellable, "非终态任务应可取消");
        assert_eq!(view.percent, 0);
    }

    #[test]
    fn list_limit_is_clamped() {
        let c = ctx();
        // 不报错即可：clamp 到 1..=100
        assert!(list(&c, Some(0)).is_ok());
        assert!(list(&c, Some(99999)).is_ok());
        assert!(list(&c, None).is_ok());
    }

    #[test]
    fn job_view_derives_counter_text_and_percent() {
        let now = chrono::Utc::now();
        let j = Job {
            id: "j1".into(),
            job_type: JobType::ScanProject,
            status: JobStatus::Running,
            progress: 0.456,
            stage: Some("扫描目录".into()),
            processed: Some(127),
            total: Some(183),
            error: None,
            payload: None,
            created_at: projectassests_storage::now_utc(),
            updated_at: projectassests_storage::now_utc(),
        };
        let v = job_view(&j, now);
        assert_eq!(v.percent, 46, "四舍五入而非截断");
        assert_eq!(v.counter_text.as_deref(), Some("127 / 183"));
        assert_eq!(v.status_label, "进行中");
        assert!(v.cancellable);
    }

    #[test]
    fn job_view_hides_meaningless_counter() {
        let now = chrono::Utc::now();
        let j = Job {
            id: "j1".into(),
            job_type: JobType::GenerateInsight,
            status: JobStatus::Completed,
            progress: 1.0,
            stage: None,
            processed: Some(0),
            total: Some(0),
            error: None,
            payload: None,
            created_at: projectassests_storage::now_utc(),
            updated_at: projectassests_storage::now_utc(),
        };
        let v = job_view(&j, now);
        // 🔴 total=0 时不能显示 "0 / 0"，前端应隐藏该段
        assert!(v.counter_text.is_none());
        assert_eq!(v.percent, 100);
        assert!(!v.cancellable, "终态任务不可取消");
    }

    #[test]
    fn job_view_clamps_out_of_range_progress() {
        let now = chrono::Utc::now();
        let mut j = Job {
            id: "j1".into(),
            job_type: JobType::IndexCode,
            status: JobStatus::Running,
            progress: 1.7,
            stage: None,
            processed: None,
            total: None,
            error: None,
            payload: None,
            created_at: projectassests_storage::now_utc(),
            updated_at: projectassests_storage::now_utc(),
        };
        assert_eq!(job_view(&j, now).percent, 100);
        j.progress = -0.3;
        assert_eq!(job_view(&j, now).percent, 0);
    }

    #[test]
    fn activities_are_empty_on_fresh_db() {
        let c = ctx();
        let a = activities(&c, None).unwrap();
        assert!(a.is_empty());
        // 空状态返回空数组而非报错，前端才能渲染"暂无动态"
        assert!(activities(&c, Some(0)).is_ok());
    }

    #[test]
    fn progress_snapshot_reports_no_event_initially() {
        let c = ctx();
        let s = progress(&c).unwrap();
        assert!(s.current.is_none(), "从未跑过任务时不该有进度事件");
    }

    #[test]
    fn settings_default_is_writable() {
        // resolve_scan_dirs 依赖 get_or_default 在全新库上不报错
        let c = ctx();
        let s: Settings = c.db.settings().get_or_default().unwrap();
        assert!(s.scan.dirs.is_empty());
    }
}
