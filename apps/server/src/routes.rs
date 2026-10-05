//! HTTP 适配器：路由表 + handler。
//!
//! # 🔴 这一层的唯一职责
//! 解析参数 → 调用 `projectassests_service` → 把 `Result` 映射成 HTTP 响应。
//! **一行业务逻辑都不许写在这里。**
//!
//! 判据很简单：如果某个判断在 Tauri IPC 适配器里也得再写一遍，
//! 那它就属于 service 层，不属于这里。
//! 例如"没有扫描目录时要不要报错"是业务规则（service 管），
//! "这个错误返回 424 还是 400"是协议映射（本文件管，且直接取
//! `ServiceError::status_code()`，不自己重新判断）。
//!
//! # 为什么同步 service 调用要包 `spawn_blocking`
//! service 层是同步的（rusqlite），而 axum handler 跑在 tokio worker 上。
//! 直接调用会**阻塞整个 worker 线程**：一次慢查询（大库上的全文检索）
//! 会让同 worker 上的其他请求全部卡住，SSE 推送也会断流。
//!
//! `blocking()` helper 把同步调用挪到专用线程池，
//! `ServiceContext` 是 `Clone`（内部全 `Arc`），move 进去零成本。
//!
//! 异步 service 调用（LLM、任务提交）本身就是非阻塞的，直接 `.await`。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post, put},
    Router,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use projectassests_service as svc;
use projectassests_service::{ServiceContext, ServiceError};

// 🔴 请求体 / 查询参数一律走 `JsonBody` / `QueryOf`，而非 axum 内建的
// `Json` / `Query`：内建提取器拒绝请求时返回**纯文本**，绕过统一信封，
// 前端就得为「业务错误」和「格式错误」各写一套解析逻辑。
// 已实测确认该缺陷存在（见 malformed_query_returns_400_in_envelope）。
use crate::error::{ok, ApiError, ApiResult, Empty, JsonBody, QueryOf};
use crate::state::AppState;

/// 构建路由表。
///
/// 全部挂在 `/api` 下：静态资源（前端构建产物）由 `serve_dir` 挂根路径，
/// 两者不冲突。这样前端用相对路径 `/api/...` 即可，无需关心后端端口。
pub fn router(state: AppState) -> Router {
    Router::new()
        // ── 概览 ────────────────────────────────────────────────
        .route("/api/overview", get(overview))
        .route("/api/health", get(health))
        // ── 项目 ────────────────────────────────────────────────
        .route("/api/projects", get(projects_list))
        .route("/api/projects/{id}", get(project_detail).delete(project_remove))
        .route("/api/projects/{id}/sensitive", put(project_sensitive))
        .route("/api/projects/{id}/description", put(project_description))
        .route("/api/projects/{id}/reindex", post(project_reindex))
        .route("/api/projects/{id}/profile", get(profile_cached).post(profile_generate))
        // ── 资产 ────────────────────────────────────────────────
        .route("/api/assets", get(assets_list))
        .route("/api/assets/types", get(assets_types))
        .route("/api/assets/{id}", get(asset_detail))
        .route("/api/assets/{id}/feedback", post(asset_feedback))
        // ── 检索 ────────────────────────────────────────────────
        .route("/api/search", get(search))
        // ── 图谱 ────────────────────────────────────────────────
        .route("/api/graph", get(graph))
        .route("/api/graph/neighborhood/{id}", get(neighborhood))
        // ── 洞察与机会 ──────────────────────────────────────────
        .route("/api/insights", get(insights_list))
        .route("/api/insights/summary", get(insights_summary))
        .route("/api/insights/adoption", get(insights_adoption))
        .route("/api/insights/{id}", get(insight_detail))
        .route("/api/insights/{id}/feedback", post(insight_feedback))
        .route("/api/opportunities", get(opportunities_list))
        // 🔴 具体动作路由必须声明在 `{id}` 之前：
        // axum 按注册顺序匹配，否则 "dismiss-all" 会被当成机会 id
        .route("/api/opportunities/dismiss-all", post(opportunities_dismiss_all))
        .route("/api/opportunities/{id}", get(opportunity_detail))
        .route("/api/opportunities/{id}/status", post(opportunity_status))
        // ── 任务与进度 ──────────────────────────────────────────
        .route("/api/jobs", get(jobs_list))
        .route("/api/jobs/scan", post(jobs_scan))
        .route("/api/jobs/index", post(jobs_index))
        .route("/api/jobs/insights", post(jobs_insights))
        .route("/api/jobs/active", get(jobs_active))
        .route("/api/jobs/progress", get(jobs_progress))
        .route("/api/jobs/{id}", get(job_detail))
        .route("/api/jobs/{id}/cancel", post(job_cancel))
        .route("/api/events", get(events))
        .route("/api/activities", get(activities))
        // ── 分析师 ──────────────────────────────────────────────
        .route("/api/analyst/ask", post(analyst_ask))
        // ── 设置 ────────────────────────────────────────────────
        .route("/api/settings", get(settings_load).put(settings_update))
        .route("/api/settings/llm", put(settings_llm))
        .route("/api/settings/scan", put(settings_scan))
        .route("/api/settings/appearance", put(settings_appearance))
        .route("/api/settings/dirs", post(settings_add_dir).delete(settings_remove_dir))
        .route("/api/settings/dirs/toggle", put(settings_toggle_dir))
        .route("/api/settings/test-connection", post(settings_test_connection))
        .route("/api/settings/audit", get(settings_audit))
        .route("/api/settings/export", get(settings_export))
        // ── 本机目录浏览（设置页「选择目录」弹窗）─────────────
        // 🔴 只读端点：列目录不写文件；服务只监听回环地址，
        // 局域网内其他机器无法借它浏览磁盘（见 crates/service/src/fs.rs）。
        .route("/api/fs/list", get(fs_list))
        // 🔴 危险操作放最后，且用 POST 而非 DELETE：
        // DELETE 在某些代理/浏览器预取场景下会被自动重试，
        // 而这个端点会清掉全部派生数据（重扫一次要几分钟）。
        .route("/api/settings/clear-derived", post(settings_clear_derived))
        .with_state(state)
}

// ══════════════════════════════════════════════════════════════════
// 同步调用的统一包装
// ══════════════════════════════════════════════════════════════════

/// 在阻塞线程池里执行同步 service 调用。
///
/// 🔴 所有同步 service 函数都必须经此调用，不要在 handler 里直接调：
/// 直接调会占住 tokio worker，慢查询时其他请求与 SSE 全部受影响。
async fn blocking<F, T>(state: &AppState, f: F) -> ApiResult<T>
where
    F: FnOnce(&ServiceContext) -> Result<T, ServiceError> + Send + 'static,
    T: Send + 'static,
{
    // clone 是廉价的：ServiceContext 内部全是 Arc。
    // 必须 clone 而非借用——spawn_blocking 的闭包要求 'static。
    let ctx = state.ctx.clone();
    tokio::task::spawn_blocking(move || f(&ctx))
        .await
        // JoinError = 线程 panic 或被中止。这是服务端故障，
        // 细节（可能含数据）写日志，用户只看到"内部错误"。
        .map_err(|e| ApiError::internal("请求处理", e))?
        .map_err(ApiError::from)
}

/// 审计查询的 limit 上限（避免 `?limit=999999` 拖垮响应）。
const MAX_AUDIT_LIMIT: u32 = 200;

// ══════════════════════════════════════════════════════════════════
// 概览与健康
// ══════════════════════════════════════════════════════════════════

async fn overview(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, svc::overview::load).await?;
    Ok(ok(data))
}

/// 健康检查。
///
/// 🔴 返回**真实**的数据库统计，不是硬编码的 `{status:"ok"}`：
/// 前端/Tauri 壳靠它判断后端是否可用，而"进程活着但库打不开"
/// 是最需要被发现的一种故障。
async fn health(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let stats = state
        .db
        .stats()
        .map_err(|e| ApiError::internal("采集数据库统计", e))?;
    Ok(ok(serde_json::json!({
        "status": "ok",
        "db_path": state.db_path(),
        "fts_available": state.db.fts_available(),
        "stats": stats,
    })))
}

// ══════════════════════════════════════════════════════════════════
// 项目
// ══════════════════════════════════════════════════════════════════

async fn projects_list(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<svc::projects::ProjectListQuery>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::projects::list(ctx, &q)).await?;
    Ok(ok(data))
}

async fn project_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::projects::detail(ctx, &id)).await?;
    Ok(ok(data))
}

/// 删除项目。
///
/// 🔴 响应文案必须说清"只删记录，不删磁盘文件"——
/// 这个区别不写明白，用户会以为源码被删了而恐慌。
async fn project_remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    blocking(&state, move |ctx| svc::projects::remove(ctx, &id)).await?;
    Ok(ok(Empty::new().message("已从数据库移除该项目记录（磁盘文件未改动）")))
}

async fn project_sensitive(
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<svc::projects::SensitiveRequest>,
) -> ApiResult<impl IntoResponse> {
    let data =
        blocking(&state, move |ctx| svc::projects::set_sensitive(ctx, &id, &req)).await?;
    Ok(ok(data))
}

async fn project_description(
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<svc::projects::DescriptionRequest>,
) -> ApiResult<impl IntoResponse> {
    let data =
        blocking(&state, move |ctx| svc::projects::set_description(ctx, &id, &req)).await?;
    Ok(ok(data))
}

/// 重建索引（异步：提交任务后立即返回 job id）。
async fn project_reindex(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let job_id = svc::projects::reindex(&state.ctx, &id)
        .await
        .map_err(ApiError::from)?;
    Ok(ok(serde_json::json!({
        "job_id": job_id,
        "message": "已提交索引重建任务",
    })))
}

// ══════════════════════════════════════════════════════════════════
// 项目画像（LLM）
// ══════════════════════════════════════════════════════════════════

/// 读取已缓存的画像。
///
/// 🔴 未生成时返回 `null` 而不是 404：
/// "还没分析过"是正常状态，前端据此显示「生成画像」按钮；
/// 返回 404 会让前端把它当成错误弹提示。
async fn profile_cached(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::profile::get_cached(ctx, &id)).await?;
    Ok(ok(data))
}

/// 生成画像（异步：调 LLM，可能数秒）。
async fn profile_generate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<ProfileGenerateBody>,
) -> ApiResult<impl IntoResponse> {
    let req = svc::ProfileRequest {
        project_id: id,
        force: body.force.unwrap_or(false),
    };
    let data = svc::profile::generate(&state.ctx, &req)
        .await
        .map_err(ApiError::from)?;
    Ok(ok(data))
}

/// `POST /profile` 的请求体。
///
/// 只有 `force` 一个字段：`project_id` 来自路径，不接受前端另传一份
/// （两份不一致时以谁为准是个没有正确答案的问题，不如根本不接受）。
#[derive(Debug, serde::Deserialize)]
struct ProfileGenerateBody {
    #[serde(default)]
    force: Option<bool>,
}

// ══════════════════════════════════════════════════════════════════
// 资产
// ══════════════════════════════════════════════════════════════════

async fn assets_list(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<svc::assets::AssetListQuery>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::assets::list(ctx, &q)).await?;
    Ok(ok(data))
}

async fn assets_types(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, svc::assets::type_breakdown).await?;
    Ok(ok(data))
}

async fn asset_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::assets::detail(ctx, &id)).await?;
    Ok(ok(data))
}

async fn asset_feedback(
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<svc::assets::FeedbackRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::assets::set_feedback(ctx, &id, &req)).await?;
    Ok(ok(data))
}

// ══════════════════════════════════════════════════════════════════
// 检索
// ══════════════════════════════════════════════════════════════════

async fn search(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<svc::search::SearchRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::search::search(ctx, &q)).await?;
    Ok(ok(data))
}

// ══════════════════════════════════════════════════════════════════
// 图谱
// ══════════════════════════════════════════════════════════════════

async fn graph(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<svc::graph::GraphRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::graph::graph(ctx, &q)).await?;
    Ok(ok(data))
}

async fn neighborhood(
    State(state): State<AppState>,
    Path(id): Path<String>,
    QueryOf(q): QueryOf<svc::graph::GraphRequest>,
) -> ApiResult<impl IntoResponse> {
    let data =
        blocking(&state, move |ctx| svc::graph::neighborhood(ctx, &id, &q)).await?;
    Ok(ok(data))
}

// ══════════════════════════════════════════════════════════════════
// 洞察与机会
// ══════════════════════════════════════════════════════════════════

async fn insights_list(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<svc::insights::InsightListQuery>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::insights::list(ctx, &q)).await?;
    Ok(ok(data))
}

async fn insights_summary(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, svc::insights::summary).await?;
    Ok(ok(data))
}

async fn insights_adoption(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, svc::insights::adoption).await?;
    Ok(ok(data))
}

async fn insight_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::insights::detail(ctx, &id)).await?;
    Ok(ok(data))
}

async fn insight_feedback(
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<svc::insights::FeedbackRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::insights::set_feedback(ctx, &id, &req)).await?;
    Ok(ok(data))
}

async fn opportunities_list(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<svc::insights::OpportunityListQuery>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::insights::list_opportunities(ctx, &q)).await?;
    Ok(ok(data))
}

async fn opportunity_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let data =
        blocking(&state, move |ctx| svc::insights::opportunity_detail(ctx, &id)).await?;
    Ok(ok(data))
}

async fn opportunity_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<svc::insights::StatusRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::insights::set_status(ctx, &id, &req)).await?;
    Ok(ok(data))
}

async fn opportunities_dismiss_all(
    State(state): State<AppState>,
) -> ApiResult<impl IntoResponse> {
    let n = blocking(&state, svc::insights::dismiss_all).await?;
    Ok(ok(Empty::new().affected(n).message(format!("已忽略 {n} 条机会"))))
}

// ══════════════════════════════════════════════════════════════════
// 任务与进度
// ══════════════════════════════════════════════════════════════════

async fn jobs_list(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<LimitQuery>,
) -> ApiResult<impl IntoResponse> {
    let limit = q.limit;
    let data = blocking(&state, move |ctx| svc::jobs::list(ctx, limit)).await?;
    Ok(ok(data))
}

async fn jobs_active(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, svc::jobs::active).await?;
    Ok(ok(data))
}

async fn jobs_progress(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, svc::jobs::progress).await?;
    Ok(ok(data))
}

async fn job_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::jobs::get(ctx, &id)).await?;
    Ok(ok(data))
}

async fn jobs_scan(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::jobs::ScanRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = svc::jobs::scan(&state.ctx, &req)
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::ACCEPTED, ok(data)))
}

async fn jobs_index(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::jobs::IndexRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = svc::jobs::index(&state.ctx, &req)
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::ACCEPTED, ok(data)))
}

async fn jobs_insights(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let data = svc::jobs::generate_insights(&state.ctx)
        .await
        .map_err(ApiError::from)?;
    Ok((StatusCode::ACCEPTED, ok(data)))
}

async fn job_cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::jobs::cancel(ctx, &id)).await?;
    Ok(ok(data))
}

async fn activities(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<LimitQuery>,
) -> ApiResult<impl IntoResponse> {
    let limit = q.limit;
    let data = blocking(&state, move |ctx| svc::jobs::activities(ctx, limit)).await?;
    Ok(ok(data))
}

/// 通用的 `?limit=` 参数。
#[derive(Debug, serde::Deserialize)]
struct LimitQuery {
    #[serde(default)]
    limit: Option<u32>,
}

/// 进度推送（SSE）。
///
/// # 实现方式：watch → mpsc → ReceiverStream
/// 引擎侧的 `ProgressSubscription` 是 `watch` 语义（只保留最新值），
/// 而 axum 的 `Sse` 需要一个 `TryStream<Ok = Event>`。
/// 这里用 mpsc 桥接，而不是引入 `async-stream` 这类 proc-macro 依赖。
///
/// 🔴 元素类型是 `Result<Event, Infallible>`：axum 0.8 要求 TryStream。
/// 用 `Infallible` 而非真实错误类型，因为**这个流本身不会失败**——
/// 序列化失败时我们发一个 `error` 事件（仍是 `Ok`），让客户端知道
/// "连接还活着但数据坏了"，而不是中断整条流。
///
/// 🔴 必须先推一次 `current()`：
/// 客户端可能在任务已经跑到 80% 时才连上来，
/// 只等"下一次变化"的话，进度条会一直显示 0% 直到任务快结束。
///
/// 🔴 `send` 失败必须退出：客户端断开后 `tx` 会被 drop，
/// 继续循环就是泄漏一个永不退出的任务。
async fn events(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, rx) = mpsc::channel(64);
    let mut sub = svc::jobs::subscribe(&state.ctx);

    tokio::spawn(async move {
        // 1. 立即推送当前状态（可能为 None = 从未跑过任务）
        if let Some(cur) = sub.current()
            && tx.send(Ok(progress_event(&cur))).await.is_err()
        {
            return;
        }
        // 2. 持续推送变化，直到广播结束或客户端断开
        while let Some(ev) = sub.next().await {
            if tx.send(Ok(progress_event(&ev))).await.is_err() {
                break;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(
        // 心跳：Nginx/代理默认 60s 无数据就断连，
        // 而扫描任务的两次进度上报间隔可能超过这个值。
        KeepAlive::default().interval(std::time::Duration::from_secs(25)),
    )
}

/// 把进度事件包成 SSE 帧。
///
/// 🔴 序列化失败时发一个错误事件而不是静默跳过：
/// 客户端需要知道"连接还活着但数据坏了"，
/// 静默跳过会让前端一直等一个永远不会来的进度更新。
fn progress_event(ev: &projectassests_jobs::ProgressEvent) -> Event {
    match serde_json::to_string(ev) {
        Ok(json) => Event::default().event("progress").data(json),
        Err(e) => Event::default()
            .event("error")
            .data(format!("进度序列化失败: {e}")),
    }
}

// ══════════════════════════════════════════════════════════════════
// 分析师
// ══════════════════════════════════════════════════════════════════

async fn analyst_ask(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::AnalystRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = svc::analyst::ask(&state.ctx, &req)
        .await
        .map_err(ApiError::from)?;
    Ok(ok(data))
}

// ══════════════════════════════════════════════════════════════════
// 设置
// ══════════════════════════════════════════════════════════════════

async fn settings_load(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, svc::settings::load).await?;
    Ok(ok(data))
}

async fn settings_update(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::settings::SettingsUpdate>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::settings::update(ctx, &req)).await?;
    Ok(ok(data))
}

async fn settings_llm(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::settings::LlmUpdate>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::settings::update_llm(ctx, &req)).await?;
    Ok(ok(data))
}

async fn settings_scan(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::settings::ScanSettingsUpdate>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::settings::update_scan(ctx, &req)).await?;
    Ok(ok(data))
}

async fn settings_appearance(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::settings::AppearanceUpdate>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::settings::update_appearance(ctx, &req)).await?;
    Ok(ok(data))
}

async fn settings_add_dir(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::settings::DirRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::settings::add_dir(ctx, &req)).await?;
    Ok(ok(data))
}

async fn settings_remove_dir(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::settings::DirRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::settings::remove_dir(ctx, &req)).await?;
    Ok(ok(data))
}

async fn settings_toggle_dir(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::settings::DirToggleRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, move |ctx| svc::settings::toggle_dir(ctx, &req)).await?;
    Ok(ok(data))
}

async fn settings_test_connection(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<svc::settings::TestConnectionRequest>,
) -> ApiResult<impl IntoResponse> {
    let data = svc::settings::test_connection(&state.ctx, &req)
        .await
        .map_err(ApiError::from)?;
    Ok(ok(data))
}

async fn settings_audit(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<AuditQuery>,
) -> ApiResult<impl IntoResponse> {
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_AUDIT_LIMIT);
    let data = blocking(&state, move |ctx| svc::settings::recent_audit(ctx, limit)).await?;
    Ok(ok(data))
}

#[derive(Debug, serde::Deserialize)]
struct AuditQuery {
    #[serde(default)]
    limit: Option<u32>,
}

/// 导出配置。
///
/// 🔴 返回 `key: value` 列表而非裸 map：`export_config` 的签名是
/// `Vec<(String, String)>`，保持顺序（用户导出后要能按顺序阅读）。
async fn settings_export(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, svc::settings::export_config).await?;
    Ok(ok(data))
}

// ══════════════════════════════════════════════════════════════════
// 本机目录浏览
// ══════════════════════════════════════════════════════════════════

/// 目录浏览查询参数。`path` 缺省/空串 = 列根（盘符或 `/`）。
#[derive(Debug, serde::Deserialize)]
pub struct FsListQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    show_hidden: Option<bool>,
}

async fn fs_list(
    State(state): State<AppState>,
    QueryOf(q): QueryOf<FsListQuery>,
) -> ApiResult<impl IntoResponse> {
    let path = q.path.unwrap_or_default();
    let show_hidden = q.show_hidden.unwrap_or(false);
    let data = blocking(&state, move |_ctx| {
        // 目录浏览不依赖数据库，但统一走 blocking 线程池：
        // 列大目录（如 C:\Users）是同步 IO，不能占住 tokio worker
        svc::list_fs_dir(&path, show_hidden)
    })
    .await?;
    Ok(ok(data))
}

async fn settings_clear_derived(
    State(state): State<AppState>,
) -> ApiResult<impl IntoResponse> {
    let data = blocking(&state, svc::settings::clear_derived_data).await?;
    Ok(ok(data))
}

// ══════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request};
    use tower::ServiceExt;

    /// 构造一个内存库的 Router 用于集成测试。
    fn app() -> Router {
        router(AppState::in_memory().unwrap())
    }

    async fn get_json(uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    async fn post_json(uri: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    // ── 信封结构 ────────────────────────────────────────────────

    #[tokio::test]
    async fn success_uses_envelope() {
        let (status, body) = get_json("/api/overview").await;
        assert_eq!(status, StatusCode::OK);
        // 🔴 前端只认这个形状：{ success, data }
        assert_eq!(body["success"], true);
        assert!(body["data"].is_object(), "{body}");
    }

    #[tokio::test]
    async fn error_uses_envelope_with_code_and_hint() {
        // 空库上触发扫描：没有目录 → Precondition → 424
        let (status, body) = post_json("/api/jobs/scan", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::from_u16(424).unwrap());
        assert_eq!(body["success"], false);
        let err = &body["error"];
        assert_eq!(err["code"], "precondition_failed");
        assert!(!err["message"].as_str().unwrap().is_empty());
        // hint 必须指向具体操作位置
        assert!(
            err["hint"].as_str().unwrap().contains("扫描目录"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn missing_entity_is_404_with_stable_code() {
        let (status, body) = get_json("/api/projects/ghost").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }

    // ── 健康检查 ────────────────────────────────────────────────

    #[tokio::test]
    async fn health_reports_real_db_state() {
        let (status, body) = get_json("/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["status"], "ok");
        // 🔴 必须是真实统计而非硬编码
        assert_eq!(body["data"]["db_path"], ":memory:");
        assert!(body["data"]["stats"].is_object());
        assert_eq!(body["data"]["stats"]["projects"], 0);
    }

    // ── 空库不报错（前端要能渲染引导页）────────────────────────

    #[tokio::test]
    async fn empty_db_returns_ok_with_empty_collections() {
        for uri in [
            "/api/projects",
            "/api/assets",
            "/api/search",
            "/api/graph",
            "/api/insights",
            "/api/opportunities",
            "/api/jobs",
            "/api/jobs/active",
            "/api/activities",
            "/api/settings",
            "/api/insights/summary",
            "/api/assets/types",
            "/api/settings/audit",
        ] {
            let (status, body) = get_json(uri).await;
            assert_eq!(status, StatusCode::OK, "{uri} 应返回 200，实得 {body}");
            assert_eq!(body["success"], true, "{uri}: {body}");
        }
    }

    // ── 参数校验（错误必须可理解）──────────────────────────────

    #[tokio::test]
    async fn bad_query_param_returns_400_from_service_not_framework() {
        // service 层显式校验 → 中文提示 + 可选值
        let (status, body) = get_json("/api/assets?asset_type=banana").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("banana"), "{msg}");
        assert!(msg.contains("code"), "应列出可选值: {msg}");
    }

    #[tokio::test]
    async fn malformed_query_returns_400_in_envelope() {
        // 🔴 类型不匹配（limit 期望数字）由 axum 的 Query 提取器拦截。
        // 关键不只是 400，而是**必须走统一信封**：
        // axum 内建提取器默认返回纯文本，前端就得写两套错误解析逻辑。
        let (status, body) = get_json("/api/projects?limit=abc").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["success"], false, "必须是信封形状，实得: {body}");
        assert_eq!(body["error"]["code"], "bad_request");
        // 纯文本响应会被 from_slice 解析成 null
        assert!(!body.is_null(), "响应体必须是 JSON 信封，不是纯文本");
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let (status, _) = get_json("/api/does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ── 路由顺序 ────────────────────────────────────────────────

    #[tokio::test]
    async fn dismiss_all_is_not_captured_by_id_route() {
        // 🔴 若 `/api/opportunities/{id}` 注册在前，"dismiss-all" 会被当成机会 id
        // 而返回 404；正确行为是执行批量忽略并返回受影响条数。
        let (status, body) =
            post_json("/api/opportunities/dismiss-all", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["affected"], 0, "空库上应忽略 0 条");
    }

    // ── 任务提交 ────────────────────────────────────────────────

    #[tokio::test]
    async fn scan_returns_202_with_job_id() {
        // 🔴 必须用真实存在的目录：`add_dir` 会校验路径存在且是目录，
        // 硬编码 "/tmp" 在 Windows 上不存在，测试会以 400 失败。
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_string_lossy().to_string();

        let st = AppState::in_memory().unwrap();
        let app = router(st.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/settings/dirs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"path": dir_path})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = router(st)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/jobs/scan")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"analyze_git": false})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 🔴 202 Accepted：任务已受理但尚未完成
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(!body["data"]["job_id"].as_str().unwrap().is_empty());
        assert_eq!(body["data"]["job_type"], "SCAN_PROJECT");
        assert!(!body["data"]["message"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn index_unknown_project_is_404() {
        let (status, body) = post_json(
            "/api/jobs/index",
            serde_json::json!({"project_id": "ghost"}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn insights_job_without_projects_is_424() {
        let (status, body) =
            post_json("/api/jobs/insights", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::from_u16(424).unwrap());
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("还没有任何项目"));
    }

    // ── 分析师（LLM 未配置时降级而非报错）──────────────────────

    #[tokio::test]
    async fn analyst_without_material_gives_guidance_not_500() {
        // 🔴 空库 + 未配置模型：AI 分析师页白屏是最糟的体验，
        // 必须给出带引导的确定性回答（200），而不是 5xx。
        let (status, body) = post_json(
            "/api/analyst/ask",
            serde_json::json!({"question": "我做过哪些视频相关的项目？"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["success"], true);

        let answer = &body["data"]["answer"];
        // 正文必须有可操作的引导，而不是"暂无数据"
        let content = answer["content"].as_str().unwrap();
        assert!(content.contains("扫描"), "{content}");
        assert!(content.contains("设置"), "应指明去哪操作: {content}");

        // 🔴 必须如实标注来源：确定性回答绝不能被包装成"AI 生成"，那是欺骗用户
        assert_eq!(answer["generated_by"], "deterministic");
        assert!(!answer["followups"].as_array().unwrap().is_empty(), "应给后续问题引导");
        // 空库上没有材料可引用，引用为空是诚实的结果
        assert!(answer["citations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn analyst_blank_question_is_400() {
        let (status, body) =
            post_json("/api/analyst/ask", serde_json::json!({"question": "   "})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"]["message"].as_str().unwrap().contains("不能为空"));
    }

    // ── 画像 ────────────────────────────────────────────────────

    #[tokio::test]
    async fn cached_profile_of_unknown_project_is_404() {
        let (status, _) = get_json("/api/projects/ghost/profile").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ── 设置 ────────────────────────────────────────────────────

    #[tokio::test]
    async fn settings_roundtrip_via_http() {
        // 🔴 用真实目录：add_dir 校验路径存在，硬编码 "/tmp/code" 在 Windows 上会 400
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_string_lossy().to_string();

        let st = AppState::in_memory().unwrap();

        // 添加目录
        let resp = router(st.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/settings/dirs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"path": dir_path})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 读回来必须能看到刚加的目录
        let resp = router(st.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let dirs = body["data"]["scan"]["dirs"].as_array().unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0]["path"], dir_path);
        assert_eq!(dirs[0]["enabled"], true);
        // 🔴 新增目录默认必须是启用状态：
        // 用户主动添加一个目录却得到"已停用"，接着点扫描什么都不发生，
        // 而界面上没有任何地方提示原因。
    }

    #[tokio::test]
    async fn audit_limit_is_clamped() {
        // 不该因为 limit 过大而报错或拖垮响应
        let (status, body) = get_json("/api/settings/audit?limit=999999").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
    }

    // ── SSE ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn events_endpoint_streams_sse() {
        let st = AppState::in_memory().unwrap();
        let resp = router(st)
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // 🔴 必须是 text/event-stream，否则浏览器 EventSource 不认
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap();
        assert!(ct.starts_with("text/event-stream"), "{ct}");
    }

    #[test]
    fn progress_event_serializes_to_sse_frame() {
        let ev = projectassests_jobs::ProgressEvent {
            job_id: "j1".into(),
            job_type: "SCAN_PROJECT".into(),
            status: projectassests_domain::JobStatus::Running,
            progress: 0.5,
            stage: Some("扫描目录".into()),
            processed: Some(10),
            total: Some(20),
            error: None,
        };
        let frame = progress_event(&ev);
        // Event 没有公开的读取接口，这里只验证不 panic 且能 Debug
        assert!(format!("{frame:?}").contains("progress"));
    }
}
