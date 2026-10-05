//! 设置服务：扫描目录管理、模型配置、连接测试、外观、审计与数据管理。
//!
//! # 🔴 API Key 的红线
//! **明文 key 永远不出服务层**：
//! - 读：返回掩码（`sk-a…xyz`），前端显示用
//! - 写：前端未修改时回传哨兵值 `__unchanged__`，服务端保留原值
//!   （原型期的真实缺陷：保存配置把掩码串当真 key 写回，之后连接测试永远失败）
//! - 日志/审计：只记 `provider:model`，不记 key
//!
//! # 目录管理
//! 增删启停都按**规范化路径**匹配（`normalize_path`），因为前端回传的
//! 路径分隔符/大小写可能与入库时不同，按原始串比对会静默失败。

use serde::{Deserialize, Serialize};
use projectassests_domain::{
    AppearanceSettings, CloudProvider, LlmSettings, LocalBackend, RouteTarget, ScanSettings,
    Settings, Theme,
};
use projectassests_storage::DbStats;

use crate::context::{ServiceContext, ServiceError};

// ══════════════════════════════════════════════════════════════════
// 视图 DTO
// ══════════════════════════════════════════════════════════════════

/// 设置页视图（读取用）。
///
/// 🔴 与 `Settings` 的区别：这里的 `cloud_api_key` 是**掩码**，
/// 且附带 `*_configured` 等派生状态——前端不该自己判断"配好了没有"，
/// 判断口径必须在服务端统一（否则网页与桌面端会给出不同结论）。
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub llm: LlmView,
    pub scan: ScanView,
    pub appearance: AppearanceSettings,
    /// 数据库占用（设置页「数据与隐私」）
    pub db: DbView,
}

/// 模型配置视图。
#[derive(Debug, Clone, Serialize)]
pub struct LlmView {
    pub cloud_provider: String,
    pub cloud_provider_label: String,
    pub cloud_base_url: String,
    /// 🔴 掩码后的 key（`sk-a…xyz`）；未设置为空串
    pub cloud_api_key_masked: String,
    /// 前端保存时若不修改 key，应回传该哨兵值
    pub api_key_placeholder: &'static str,
    pub cloud_model: String,
    pub local_backend: String,
    pub local_backend_label: String,
    pub local_base_url: String,
    pub local_model: String,
    pub route_fast: String,
    pub route_deep: String,
    pub sensitive_local_only: bool,
    pub embedding_local_only: bool,
    /// 派生状态：云端是否配置完整（前端据此启用/禁用"测试连接"）
    pub cloud_configured: bool,
    pub local_configured: bool,
    /// 各提供商的默认地址与预置模型（前端下拉选项的数据源）
    pub cloud_providers: Vec<ProviderOption>,
    pub local_backends: Vec<ProviderOption>,
}

/// 提供商/后端选项（含默认值，前端选中后自动填充）。
#[derive(Debug, Clone, Serialize)]
pub struct ProviderOption {
    pub value: String,
    pub label: String,
    pub default_base_url: String,
    pub preset_models: Vec<String>,
}

/// 扫描设置视图。
#[derive(Debug, Clone, Serialize)]
pub struct ScanView {
    pub dirs: Vec<DirView>,
    pub watch_enabled: bool,
    pub exclude_patterns: Vec<String>,
    pub level2_enabled: bool,
    pub max_depth: u32,
    /// 目录路径校验结果（不存在的目录前端标红并提示）
    pub problems: Vec<String>,
}

/// 单个授权目录的视图。
#[derive(Debug, Clone, Serialize)]
pub struct DirView {
    pub path: String,
    pub enabled: bool,
    pub added_at: String,
    /// `None` = 从未扫描（前端显示"尚未扫描"而非假时间）
    pub last_scanned_at: Option<String>,
    pub project_count: Option<u32>,
    /// 目录当前是否存在（被移动/删除的目录要能被用户看见并处理）
    pub exists: bool,
}

/// 数据库占用视图。
#[derive(Debug, Clone, Serialize)]
pub struct DbView {
    pub path: String,
    /// 人类可读的大小（"12.4 MB"）
    pub size_display: String,
    pub size_bytes: u64,
    pub schema_version: i32,
    pub fts_available: bool,
    pub tables: Vec<TableCount>,
}

/// 单表计数。
#[derive(Debug, Clone, Serialize)]
pub struct TableCount {
    pub table: String,
    pub rows: usize,
}

// ══════════════════════════════════════════════════════════════════
// 更新请求
// ══════════════════════════════════════════════════════════════════

/// 设置更新请求（部分更新：只带需要改的段）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SettingsUpdate {
    #[serde(default)]
    pub llm: Option<LlmUpdate>,
    #[serde(default)]
    pub appearance: Option<AppearanceUpdate>,
}

/// 模型配置更新。
#[derive(Debug, Clone, Deserialize)]
pub struct LlmUpdate {
    #[serde(default)]
    pub cloud_provider: Option<String>,
    #[serde(default)]
    pub cloud_base_url: Option<String>,
    /// 🔴 `None` 或哨兵值 = 不修改；`Some("")` = 清除已存的 key
    #[serde(default)]
    pub cloud_api_key: Option<String>,
    #[serde(default)]
    pub cloud_model: Option<String>,
    #[serde(default)]
    pub local_backend: Option<String>,
    #[serde(default)]
    pub local_base_url: Option<String>,
    #[serde(default)]
    pub local_model: Option<String>,
    #[serde(default)]
    pub route_fast: Option<String>,
    #[serde(default)]
    pub route_deep: Option<String>,
    #[serde(default)]
    pub sensitive_local_only: Option<bool>,
    #[serde(default)]
    pub embedding_local_only: Option<bool>,
}

/// 外观更新。
#[derive(Debug, Clone, Deserialize)]
pub struct AppearanceUpdate {
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub reduce_motion: Option<bool>,
}

/// 目录操作请求。
#[derive(Debug, Clone, Deserialize)]
pub struct DirRequest {
    pub path: String,
}

/// 目录启停请求。
#[derive(Debug, Clone, Deserialize)]
pub struct DirToggleRequest {
    pub path: String,
    pub enabled: bool,
}

/// 连接测试请求。
#[derive(Debug, Clone, Deserialize)]
pub struct TestConnectionRequest {
    /// `None` = 测当前默认路由（快速分析路由）
    #[serde(default)]
    pub route: Option<String>,
}

/// 连接测试结果视图。
#[derive(Debug, Clone, Serialize)]
pub struct TestConnectionView {
    pub ok: bool,
    pub message: String,
    pub backend: String,
    pub model: String,
    pub route: String,
    pub route_label: String,
    /// 本地后端能列出已拉取的模型（前端可做成下拉选择）
    pub models: Vec<String>,
    pub latency_ms: u64,
}

// ══════════════════════════════════════════════════════════════════
// 读取
// ══════════════════════════════════════════════════════════════════

/// 加载设置页视图。
pub fn load(ctx: &ServiceContext) -> Result<SettingsView, ServiceError> {
    let s = ctx.db.settings().get_or_default()?;
    let stats = ctx.db.stats()?;
    Ok(SettingsView {
        llm: llm_view(&s.llm),
        scan: scan_view(&s.scan),
        appearance: s.appearance,
        db: db_view(ctx, &stats),
    })
}

fn llm_view(llm: &LlmSettings) -> LlmView {
    LlmView {
        cloud_provider: llm.cloud_provider.as_str().to_string(),
        cloud_provider_label: llm.cloud_provider.display_name().to_string(),
        // 地址不掩码：它不含凭据，且用户需要看到自己配的是哪个端点
        // （排查"为什么连不上"时，地址错比 key 错更常见）
        cloud_base_url: llm.cloud_base_url.clone(),
        // 🔴 掩码：明文 key 绝不返回前端
        cloud_api_key_masked: LlmSettings::mask_api_key(&llm.cloud_api_key),
        api_key_placeholder: LlmSettings::MASKED_PLACEHOLDER,
        cloud_model: llm.cloud_model.clone(),
        local_backend: llm.local_backend.as_str().to_string(),
        local_backend_label: llm.local_backend.display_name().to_string(),
        local_base_url: llm.local_base_url.clone(),
        local_model: llm.local_model.clone(),
        route_fast: llm.route_fast.as_str().to_string(),
        route_deep: llm.route_deep.as_str().to_string(),
        sensitive_local_only: llm.sensitive_local_only,
        embedding_local_only: llm.embedding_local_only,
        cloud_configured: llm.cloud_configured(),
        local_configured: llm.local_configured(),
        cloud_providers: CloudProvider::all()
            .iter()
            .map(|p| ProviderOption {
                value: p.as_str().to_string(),
                label: p.display_name().to_string(),
                default_base_url: p.default_base_url().to_string(),
                preset_models: p.preset_models().iter().map(|s| s.to_string()).collect(),
            })
            .collect(),
        local_backends: LocalBackend::all()
            .iter()
            .map(|b| ProviderOption {
                value: b.as_str().to_string(),
                label: b.display_name().to_string(),
                default_base_url: b.default_base_url().to_string(),
                preset_models: b.preset_models().iter().map(|s| s.to_string()).collect(),
            })
            .collect(),
    }
}

fn scan_view(scan: &ScanSettings) -> ScanView {
    let problems = projectassests_jobs::validate_settings(&Settings {
        llm: LlmSettings::default(),
        scan: scan.clone(),
        appearance: AppearanceSettings::default(),
    });
    ScanView {
        dirs: scan
            .dirs
            .iter()
            .map(|d| DirView {
                exists: std::path::Path::new(&d.path).is_dir(),
                path: d.path.clone(),
                enabled: d.enabled,
                added_at: d.added_at.clone(),
                last_scanned_at: d.last_scanned_at.clone(),
                project_count: d.project_count,
            })
            .collect(),
        watch_enabled: scan.watch_enabled,
        exclude_patterns: scan.exclude_patterns.clone(),
        level2_enabled: scan.level2_enabled,
        max_depth: scan.max_depth,
        problems,
    }
}

fn db_view(ctx: &ServiceContext, stats: &DbStats) -> DbView {
    DbView {
        path: ctx.db_path.clone(),
        size_display: projectassests_storage::format_bytes(stats.size_bytes),
        size_bytes: stats.size_bytes,
        // schema 版本不在 DbStats 里，单独查（用于诊断"库结构是否过期"）
        schema_version: ctx.db.version().unwrap_or(0),
        fts_available: ctx.db.fts_available(),
        // DbStats 用具名字段而非 map：这里转成列表供前端渲染，
        // 顺序固定，保证同一份数据两次加载的展示顺序一致
        tables: vec![
            ("projects", stats.projects),
            ("assets", stats.assets),
            ("capabilities", stats.capabilities),
            ("relations", stats.relations),
            ("insights", stats.insights),
            ("opportunities", stats.opportunities),
            ("jobs", stats.jobs),
            ("activities", stats.activities),
            ("audit_log", stats.audit_entries),
        ]
        .into_iter()
        .map(|(table, rows)| TableCount {
            table: table.to_string(),
            rows,
        })
        .collect(),
    }
}

// ══════════════════════════════════════════════════════════════════
// 目录管理
// ══════════════════════════════════════════════════════════════════

/// 添加扫描目录。
///
/// 校验顺序：非空 → 存在 → 是目录 → 未重复。
/// 🔴 每步都给出**具体**的失败原因：笼统的"添加失败"会让用户反复试错。
pub fn add_dir(ctx: &ServiceContext, req: &DirRequest) -> Result<ScanView, ServiceError> {
    let path = req.path.trim();
    if path.is_empty() {
        return Err(ServiceError::Invalid("目录路径不能为空".to_string()));
    }
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Err(ServiceError::Invalid(format!("目录不存在：{path}")));
    }
    if !p.is_dir() {
        return Err(ServiceError::Invalid(format!("不是目录：{path}")));
    }

    let mut scan = ctx.db.settings().get_or_default()?.scan;
    let now = projectassests_storage::now_utc();
    if !scan.add_dir(path, &now) {
        // 已存在不算错误（幂等），但要告诉前端"没变化"
        return Err(ServiceError::Conflict(format!("该目录已在列表中：{path}")));
    }
    ctx.db.settings().save_scan(&scan)?;
    Ok(scan_view(&scan))
}

/// 移除扫描目录。
///
/// 🔴 只移除授权，**不删除已索引的项目数据**：用户可能只是不想再扫这个目录，
/// 历史资产仍有价值。要清数据请走"清除派生数据"（明确的双确认操作）。
pub fn remove_dir(ctx: &ServiceContext, req: &DirRequest) -> Result<ScanView, ServiceError> {
    let mut scan = ctx.db.settings().get_or_default()?.scan;
    let removed = scan.remove_dir(req.path.trim());
    if removed.is_none() {
        return Err(ServiceError::NotFound(format!("目录 {}", req.path)));
    }
    ctx.db.settings().save_scan(&scan)?;
    Ok(scan_view(&scan))
}

/// 启用/停用目录。
pub fn toggle_dir(ctx: &ServiceContext, req: &DirToggleRequest) -> Result<ScanView, ServiceError> {
    let mut scan = ctx.db.settings().get_or_default()?.scan;
    if !scan.set_dir_enabled(req.path.trim(), req.enabled) {
        return Err(ServiceError::NotFound(format!("目录 {}", req.path)));
    }
    ctx.db.settings().save_scan(&scan)?;
    Ok(scan_view(&scan))
}

/// 更新扫描行为配置（排除模式、深度、Level 2 开关）。
///
/// `Default` 是必需的：前端通常只改其中一项，其余字段留空即"不修改"。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScanSettingsUpdate {
    #[serde(default)]
    pub watch_enabled: Option<bool>,
    /// 整段替换：前端传完整列表（含空数组表示清空）
    #[serde(default)]
    pub exclude_patterns: Option<Vec<String>>,
    #[serde(default)]
    pub level2_enabled: Option<bool>,
    #[serde(default)]
    pub max_depth: Option<u32>,
}

pub fn update_scan(ctx: &ServiceContext, req: &ScanSettingsUpdate) -> Result<ScanView, ServiceError> {
    let mut scan = ctx.db.settings().get_or_default()?.scan;

    if let Some(depth) = req.max_depth {
        // 深度为 0 会让扫描什么都发现不了；上限防止用户填个天文数字把整盘扫穿
        if depth == 0 {
            return Err(ServiceError::Invalid("扫描深度不能为 0".to_string()));
        }
        if depth > 20 {
            return Err(ServiceError::Invalid(
                "扫描深度过大（上限 20），过深会显著拖慢扫描".to_string(),
            ));
        }
        scan.max_depth = depth;
    }
    if let Some(w) = req.watch_enabled {
        scan.watch_enabled = w;
    }
    if let Some(l2) = req.level2_enabled {
        scan.level2_enabled = l2;
    }
    if let Some(patterns) = &req.exclude_patterns {
        // 去空白、去空行；内置排除项（node_modules 等）不可关闭，是隐私底线
        let cleaned: Vec<String> = patterns
            .iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        scan.exclude_patterns = cleaned;
    }

    ctx.db.settings().save_scan(&scan)?;
    Ok(scan_view(&scan))
}

// ══════════════════════════════════════════════════════════════════
// 模型配置
// ══════════════════════════════════════════════════════════════════

/// 更新模型配置。
pub fn update_llm(ctx: &ServiceContext, req: &LlmUpdate) -> Result<LlmView, ServiceError> {
    let mut llm = ctx.db.settings().get_or_default()?.llm;

    if let Some(p) = &req.cloud_provider {
        llm.cloud_provider = CloudProvider::parse(p)
            .ok_or_else(|| ServiceError::Invalid(format!("未知的云端提供商：{p}")))?;
    }
    if let Some(url) = &req.cloud_base_url {
        let url = url.trim();
        // 允许留空（表示未配置云端），但填了就必须是合法 http(s)
        if !url.is_empty() && !projectassests_domain::is_http_url(url) {
            return Err(ServiceError::Invalid(format!(
                "云端地址必须以 http:// 或 https:// 开头：{url}"
            )));
        }
        llm.cloud_base_url = url.to_string();
    }
    // 🔴 key 的三态语义：None/哨兵 = 不改；Some("") = 清除；其它 = 新值
    if let Some(key) = &req.cloud_api_key {
        llm.apply_api_key(key);
    }
    if let Some(m) = &req.cloud_model {
        llm.cloud_model = m.trim().to_string();
    }
    if let Some(b) = &req.local_backend {
        llm.local_backend = LocalBackend::parse(b)
            .ok_or_else(|| ServiceError::Invalid(format!("未知的本地后端：{b}")))?;
    }
    if let Some(url) = &req.local_base_url {
        let url = url.trim();
        if !url.is_empty() && !projectassests_domain::is_http_url(url) {
            return Err(ServiceError::Invalid(format!(
                "本地地址必须以 http:// 或 https:// 开头：{url}"
            )));
        }
        llm.local_base_url = url.to_string();
    }
    if let Some(m) = &req.local_model {
        llm.local_model = m.trim().to_string();
    }
    if let Some(r) = &req.route_fast {
        llm.route_fast = RouteTarget::parse(r)
            .ok_or_else(|| ServiceError::Invalid(format!("未知的路由目标：{r}")))?;
    }
    if let Some(r) = &req.route_deep {
        llm.route_deep = RouteTarget::parse(r)
            .ok_or_else(|| ServiceError::Invalid(format!("未知的路由目标：{r}")))?;
    }
    if let Some(v) = req.sensitive_local_only {
        llm.sensitive_local_only = v;
    }
    if let Some(v) = req.embedding_local_only {
        llm.embedding_local_only = v;
    }

    // 🔴 关闭"敏感项目仅本地"是安全约束的降级，必须显式确认。
    // 这里不阻止（用户有权决定），但记审计：日后排查"数据怎么上了云"时有据可查。
    let before = ctx.db.settings().get_or_default()?.llm;
    if before.sensitive_local_only && !llm.sensitive_local_only {
        // 🔴 `event()` 而非 `llm_ok()`：本地安全事件，不是模型调用。
        // 这条记录的存在意义恰恰是"用户主动降低了安全等级"，
        // 给它一个成功对勾会削弱甚至反转这个信号。
        let _ = ctx.db.settings().audit(&projectassests_domain::AuditEntry::event(
            projectassests_storage::now_utc(),
            "SETTINGS",
            "用户关闭了「敏感项目仅本地」约束",
            None,
        ));
        tracing::warn!("用户关闭了 sensitive_local_only 约束");
    }

    ctx.db.settings().save_llm(&llm)?;
    Ok(llm_view(&llm))
}

/// 更新外观。
pub fn update_appearance(
    ctx: &ServiceContext,
    req: &AppearanceUpdate,
) -> Result<AppearanceSettings, ServiceError> {
    let mut app = ctx.db.settings().get_or_default()?.appearance;
    if let Some(t) = &req.theme {
        app.theme = Theme::parse(t)
            .ok_or_else(|| ServiceError::Invalid(format!("未知主题：{t}")))?;
    }
    if let Some(m) = req.reduce_motion {
        app.reduce_motion = m;
    }
    ctx.db.settings().save_appearance(&app)?;
    Ok(app)
}

/// 统一入口：按段更新。
pub fn update(ctx: &ServiceContext, req: &SettingsUpdate) -> Result<SettingsView, ServiceError> {
    if let Some(llm) = &req.llm {
        update_llm(ctx, llm)?;
    }
    if let Some(app) = &req.appearance {
        update_appearance(ctx, app)?;
    }
    load(ctx)
}

// ══════════════════════════════════════════════════════════════════
// 连接测试
// ══════════════════════════════════════════════════════════════════

/// 测试模型连接。
///
/// 🔴 未配置时返回**带引导的失败结果**而非报错：
/// 前端要在同一块 UI 里显示"未配置 → 去填"与"配置了 → 测试中"，
/// 用异常表达前者会让前端多写一套错误处理分支。
pub async fn test_connection(
    ctx: &ServiceContext,
    req: &TestConnectionRequest,
) -> Result<TestConnectionView, ServiceError> {
    let settings = ctx.db.settings().get_or_default()?;
    let route = match &req.route {
        Some(r) => Some(
            RouteTarget::parse(r)
                .ok_or_else(|| ServiceError::Invalid(format!("未知的路由目标：{r}")))?,
        ),
        None => None,
    };

    let outcome = ctx.router.test_connection(&settings.llm, route).await;
    Ok(TestConnectionView {
        ok: outcome.ok,
        message: outcome.message,
        backend: outcome.backend,
        model: outcome.model,
        route_label: outcome.route.label_zh().to_string(),
        route: outcome.route.as_str().to_string(),
        models: outcome.models,
        latency_ms: outcome.latency_ms,
    })
}

// ══════════════════════════════════════════════════════════════════
// 审计与数据管理
// ══════════════════════════════════════════════════════════════════

/// 审计日志视图。
///
/// 🔴 改这里的字段必须同步 `apps/desktop/src/api/types.ts` 的 `AuditView`：
/// 那是本 DTO 的逐字段镜像，漏改会让类型检查全绿但前端运行时读到 `undefined`。
#[derive(Debug, Clone, Serialize)]
pub struct AuditView {
    pub at: String,
    pub model: String,
    pub route: String,
    pub route_label: String,
    pub job_type: String,
    pub summary: String,
    pub project_id: Option<String>,
    /// 模型调用成败。`null` = 本条不是模型调用（本地安全事件）。
    ///
    /// 🔴 前端必须按三态渲染，不能当成布尔：
    /// `null` 不显示任何成败标记——给「用户关闭了敏感项目仅本地约束」
    /// 打一个绿色对勾，会把一次**安全降级**说成"操作成功"。
    pub ok: Option<bool>,
    /// 失败原因（仅 `ok = false` 时有值）。
    /// 不含代码原文与 API Key（见 `AuditEntry::error` 的文档）。
    pub error: Option<String>,
}

/// 读取审计日志（设置页「数据与隐私」）。
pub fn recent_audit(ctx: &ServiceContext, limit: u32) -> Result<Vec<AuditView>, ServiceError> {
    Ok(ctx
        .db
        .settings()
        .recent_audit(limit.clamp(1, 200))?
        .into_iter()
        .map(|e| AuditView {
            at: e.at,
            model: e.model,
            route_label: e.route.label_zh().to_string(),
            route: e.route.as_str().to_string(),
            job_type: e.job_type,
            summary: e.summary,
            project_id: e.project_id,
            ok: e.ok,
            error: e.error,
        })
        .collect())
}

/// 清除派生数据的结果。
#[derive(Debug, Clone, Serialize)]
pub struct ClearResult {
    /// 各表删除的行数
    pub cleared: std::collections::HashMap<String, usize>,
    /// 保留的内容说明（让用户确认"什么没被删"）
    pub preserved: Vec<String>,
}

/// 清除派生数据（资产/能力/关系/洞察/机会/索引）。
///
/// 🔴 **保留**三类用户数据，理由各不相同（详见 `storage::clear_derived_data`）：
/// - `settings`：模型配置与扫描目录授权不是派生数据。
/// - `audit_log`：审计凭证必须不能被普通操作抹掉，否则「可审计」形同虚设。
/// - `projects` 行：`sensitive` 与 `description` 由用户手写，
///   删行会让敏感项目保护静默失效（重扫后可能把代码送去云端模型）。
///   只重置派生列，项目清单与敏感标记原地不动。
///
/// ⚠️ **不**保留的：`user_feedback`（存在 assets/insights 行内）。
/// 反馈标注随派生行一起消失，因此前端确认弹窗**不得**声称保留它——
/// 兑现不了的承诺比不承诺更糟，用户会据此误判操作风险。
/// 要让反馈跨清理存活，需要把它提到独立表（schema 变更），是另一件事。
pub fn clear_derived_data(ctx: &ServiceContext) -> Result<ClearResult, ServiceError> {
    let report = ctx.db.clear_derived_data()?;
    // 🔴 键名如实反映行为：`projects_reset` 不是 `projects`。
    // 前端直接把这些键渲染成 "xxx: N 行"，写成 `projects` 会显示
    // "projects: 2 行" —— 用户读成"删了 2 个项目"，而项目清单其实还在。
    let cleared = std::collections::HashMap::from([
        ("assets".to_string(), report.assets),
        ("capabilities".to_string(), report.capabilities),
        ("relations".to_string(), report.relations),
        ("insights".to_string(), report.insights),
        ("opportunities".to_string(), report.opportunities),
        ("jobs".to_string(), report.jobs),
        ("activities".to_string(), report.activities),
        ("fts_rows".to_string(), report.fts_rows),
        ("projects_reset".to_string(), report.projects_reset),
    ]);

    let _ = ctx.db.activities().push(
        projectassests_storage::ActivityIcon::Alert,
        "已清除派生数据",
        "项目清单、敏感标记、设置与审计日志均已保留；重新扫描即可重建索引",
    );

    Ok(ClearResult {
        cleared,
        preserved: vec![
            "扫描目录授权".to_string(),
            "模型配置".to_string(),
            "外观设置".to_string(),
            "项目清单与敏感标记".to_string(),
            "审计日志".to_string(),
        ],
    })
}

/// 导出非敏感配置（用户备份/迁移用）。
///
/// 🔴 只导出 `export_non_sensitive` 白名单里的键：
/// API Key 与项目路径都不在其中。导出的文件会被用户随意传阅，
/// 一旦含 key 就是安全事故。
pub fn export_config(ctx: &ServiceContext) -> Result<Vec<(String, String)>, ServiceError> {
    Ok(ctx.db.settings().export_non_sensitive()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{Asset, AssetType, CodeStats, Evidence, Project, ProjectStatus, ScanFacts};

    fn ctx() -> ServiceContext {
        ServiceContext::in_memory().unwrap()
    }

    fn real_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn project(id: &str) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            path: format!("/tmp/{id}"),
            description: String::new(),
            language: "Python".into(),
            framework: "-".into(),
            created_at: None,
            updated_at: None,
            last_commit_at: None,
            status: ProjectStatus::Active,
            health_score: 70,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats::default(),
            scan: ScanFacts::default(),
            ai_profile: None,
        }
    }

    // ── 读取：API Key 掩码 ───────────────────────────────────────

    /// 🔴 明文 key 绝不能出现在任何返回给前端的结构里。
    #[test]
    fn api_key_is_never_returned_in_plaintext() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.cloud_api_key = "sk-super-secret-key-1234567890".into();
        s.llm.cloud_base_url = "https://api.example.com/v1".into();
        c.db.settings().save_llm(&s.llm).unwrap();

        let view = load(&c).unwrap();
        assert_eq!(view.llm.cloud_api_key_masked, "sk-s…890");
        assert!(view.llm.cloud_configured);
        assert_eq!(view.llm.api_key_placeholder, "__unchanged__");

        // 🔴 序列化后的 JSON 里也不得出现明文（防止将来有人加字段时泄漏）
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            !json.contains("super-secret"),
            "序列化结果泄漏了明文 key: {json}"
        );
    }

    #[test]
    fn empty_key_masks_to_empty() {
        let view = load(&ctx()).unwrap();
        assert_eq!(view.llm.cloud_api_key_masked, "");
        assert!(!view.llm.cloud_configured);
    }

    /// 默认设置必须是 Local-First：新用户不该在不知情下把代码发上云。
    #[test]
    fn default_settings_are_local_first() {
        let view = load(&ctx()).unwrap();
        assert_eq!(view.llm.route_fast, "local");
        assert_eq!(view.llm.route_deep, "local");
        assert!(view.llm.sensitive_local_only);
        assert!(view.llm.embedding_local_only);
    }

    #[test]
    fn provider_options_are_populated() {
        let view = load(&ctx()).unwrap();
        assert!(!view.llm.cloud_providers.is_empty());
        assert!(!view.llm.local_backends.is_empty());
        // Ollama 应在本地后端选项里，且带默认地址
        let ollama = view
            .llm
            .local_backends
            .iter()
            .find(|b| b.value == "ollama")
            .expect("应包含 Ollama");
        assert!(ollama.default_base_url.contains("11434"));
        assert!(!ollama.preset_models.is_empty());
        assert!(!ollama.label.is_empty());
    }

    // ── 目录管理 ─────────────────────────────────────────────────

    #[test]
    fn add_dir_validates_existence() {
        let c = ctx();
        let err = add_dir(
            &c,
            &DirRequest {
                path: "/nonexistent-projectassests-dir".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, ServiceError::Invalid(_)));
        assert!(err.to_string().contains("不存在"), "应说明原因: {err}");
    }

    #[test]
    fn add_dir_rejects_file_and_blank() {
        let c = ctx();
        let dir = real_dir();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();

        let err = add_dir(&c, &DirRequest { path: file.to_string_lossy().into() }).unwrap_err();
        assert!(err.to_string().contains("不是目录"), "实际: {err}");

        let err2 = add_dir(&c, &DirRequest { path: "   ".into() }).unwrap_err();
        assert!(err2.to_string().contains("不能为空"), "实际: {err2}");
    }

    #[test]
    fn add_dir_succeeds_for_real_directory() {
        let c = ctx();
        let dir = real_dir();
        let view = add_dir(
            &c,
            &DirRequest {
                path: dir.path().to_string_lossy().into(),
            },
        )
        .unwrap();
        assert_eq!(view.dirs.len(), 1);
        assert!(view.dirs[0].enabled);
        assert!(view.dirs[0].exists);
        assert!(
            view.dirs[0].last_scanned_at.is_none(),
            "从未扫描应显示 None 而非假时间"
        );
        assert!(view.problems.is_empty(), "真实目录不该有问题: {:?}", view.problems);
    }

    /// 重复添加必须明确报冲突——静默成功会让用户以为加了两个目录。
    #[test]
    fn add_duplicate_dir_reports_conflict() {
        let c = ctx();
        let dir = real_dir();
        let req = DirRequest {
            path: dir.path().to_string_lossy().into(),
        };
        add_dir(&c, &req).unwrap();
        let err = add_dir(&c, &req).unwrap_err();
        assert!(matches!(err, ServiceError::Conflict(_)), "实际 {err:?}");
        assert_eq!(err.status_code(), 409);
        assert!(err.to_string().contains("已在列表中"));
    }

    #[test]
    fn remove_dir_works_and_reports_unknown() {
        let c = ctx();
        let dir = real_dir();
        let path = dir.path().to_string_lossy().to_string();
        add_dir(&c, &DirRequest { path: path.clone() }).unwrap();

        let view = remove_dir(&c, &DirRequest { path: path.clone() }).unwrap();
        assert!(view.dirs.is_empty());

        let err = remove_dir(&c, &DirRequest { path }).unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    /// 🔴 移除授权不得删掉已索引的项目：用户可能只是不想再扫这个目录。
    #[test]
    fn remove_dir_preserves_indexed_projects() {
        let c = ctx();
        let dir = real_dir();
        let path = dir.path().to_string_lossy().to_string();
        add_dir(&c, &DirRequest { path: path.clone() }).unwrap();
        c.db.projects().upsert(&project("p1")).unwrap();

        remove_dir(&c, &DirRequest { path }).unwrap();
        assert_eq!(
            c.db.projects().count().unwrap(),
            1,
            "移除授权不得连带删除项目数据"
        );
    }

    #[test]
    fn toggle_dir_enables_and_disables() {
        let c = ctx();
        let dir = real_dir();
        let path = dir.path().to_string_lossy().to_string();
        add_dir(&c, &DirRequest { path: path.clone() }).unwrap();

        let view = toggle_dir(&c, &DirToggleRequest { path: path.clone(), enabled: false }).unwrap();
        assert!(!view.dirs[0].enabled);

        let view = toggle_dir(&c, &DirToggleRequest { path, enabled: true }).unwrap();
        assert!(view.dirs[0].enabled);
    }

    #[test]
    fn toggle_unknown_dir_reports_not_found() {
        let c = ctx();
        let err = toggle_dir(
            &c,
            &DirToggleRequest {
                path: "/nope".into(),
                enabled: false,
            },
        )
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    /// 目录被移动/删除后必须在设置页可见，否则用户不知道扫描为什么少了项目。
    #[test]
    fn missing_dir_is_flagged() {
        let c = ctx();
        // 直接写库造一个已不存在的目录（绕过 add_dir 的存在性校验）
        let mut scan = c.db.settings().get_or_default().unwrap().scan;
        scan.add_dir("/vanished-projectassests-dir", "2026-09-29T00:00:00Z");
        c.db.settings().save_scan(&scan).unwrap();

        let view = load(&c).unwrap();
        assert!(!view.scan.dirs[0].exists, "应标记目录已不存在");
        assert!(
            view.scan.problems.iter().any(|p| p.contains("不存在")),
            "应给出问题说明: {:?}",
            view.scan.problems
        );
    }

    // ── 扫描配置校验 ─────────────────────────────────────────────

    #[test]
    fn max_depth_is_validated() {
        let c = ctx();
        let err = update_scan(&c, &ScanSettingsUpdate { max_depth: Some(0), ..Default::default() })
            .unwrap_err();
        assert!(err.to_string().contains("不能为 0"), "实际: {err}");

        let err2 = update_scan(&c, &ScanSettingsUpdate { max_depth: Some(99), ..Default::default() })
            .unwrap_err();
        assert!(err2.to_string().contains("上限"), "实际: {err2}");

        let view = update_scan(&c, &ScanSettingsUpdate { max_depth: Some(8), ..Default::default() })
            .unwrap();
        assert_eq!(view.max_depth, 8);
    }

    /// 排除模式要去空白去空行，但内置排除项不受影响（隐私底线）。
    #[test]
    fn exclude_patterns_are_cleaned() {
        let c = ctx();
        let view = update_scan(
            &c,
            &ScanSettingsUpdate {
                exclude_patterns: Some(vec![
                    "  **/*.log  ".into(),
                    "".into(),
                    "   ".into(),
                    "**/tmp/**".into(),
                ]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(view.exclude_patterns, vec!["**/*.log", "**/tmp/**"]);
    }

    #[test]
    fn scan_flags_are_updated() {
        let c = ctx();
        let view = update_scan(
            &c,
            &ScanSettingsUpdate {
                watch_enabled: Some(false),
                level2_enabled: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!view.watch_enabled);
        assert!(view.level2_enabled);
    }

    // ── 模型配置更新 ─────────────────────────────────────────────

    /// 🔴 原型期真实缺陷：前端回传掩码串被当真 key 写入，之后连接永远失败。
    #[test]
    fn masked_placeholder_preserves_original_key() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.cloud_api_key = "sk-original-key-abcdefgh".into();
        c.db.settings().save_llm(&s.llm).unwrap();

        // 前端未修改 key，回传哨兵值
        update_llm(
            &c,
            &LlmUpdate {
                cloud_api_key: Some(LlmSettings::MASKED_PLACEHOLDER.to_string()),
                cloud_model: Some("gpt-5".into()),
                ..base_llm_update()
            },
        )
        .unwrap();

        let stored = c.db.settings().get_api_key().unwrap();
        assert_eq!(
            stored, "sk-original-key-abcdefgh",
            "哨兵值不得覆盖原 key"
        );
        assert_eq!(c.db.settings().get_or_default().unwrap().llm.cloud_model, "gpt-5");
    }

    /// 传空串应清除 key（用户主动移除配置），与"不修改"区分开。
    #[test]
    fn empty_string_clears_key() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.cloud_api_key = "sk-to-remove".into();
        c.db.settings().save_llm(&s.llm).unwrap();

        update_llm(
            &c,
            &LlmUpdate {
                cloud_api_key: Some(String::new()),
                ..base_llm_update()
            },
        )
        .unwrap();
        assert!(
            c.db.settings().get_api_key().unwrap().is_empty(),
            "空串应清除 key"
        );
    }

    #[test]
    fn new_key_overwrites() {
        let c = ctx();
        update_llm(
            &c,
            &LlmUpdate {
                cloud_api_key: Some("sk-new-key-1234567890".into()),
                ..base_llm_update()
            },
        )
        .unwrap();
        assert_eq!(c.db.settings().get_api_key().unwrap(), "sk-new-key-1234567890");
    }

    #[test]
    fn invalid_urls_are_rejected() {
        let c = ctx();
        for url in ["api.example.com/v1", "ftp://x", "not a url"] {
            let err = update_llm(
                &c,
                &LlmUpdate {
                    cloud_base_url: Some(url.into()),
                    ..base_llm_update()
                },
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("http"),
                "{url} 应被拒且提示格式: {err}"
            );
        }
        // 留空表示未配置，应允许
        update_llm(
            &c,
            &LlmUpdate {
                cloud_base_url: Some(String::new()),
                ..base_llm_update()
            },
        )
        .unwrap();
    }

    #[test]
    fn unknown_enum_values_are_rejected() {
        let c = ctx();
        assert!(matches!(
            update_llm(&c, &LlmUpdate { cloud_provider: Some("bogus".into()), ..base_llm_update() })
                .unwrap_err(),
            ServiceError::Invalid(_)
        ));
        assert!(matches!(
            update_llm(&c, &LlmUpdate { local_backend: Some("bogus".into()), ..base_llm_update() })
                .unwrap_err(),
            ServiceError::Invalid(_)
        ));
        assert!(matches!(
            update_llm(&c, &LlmUpdate { route_fast: Some("bogus".into()), ..base_llm_update() })
                .unwrap_err(),
            ServiceError::Invalid(_)
        ));
    }

    #[test]
    fn valid_enum_values_are_accepted() {
        let c = ctx();
        let view = update_llm(
            &c,
            &LlmUpdate {
                cloud_provider: Some("qwen".into()),
                local_backend: Some("lmstudio".into()),
                route_fast: Some("cloud".into()),
                route_deep: Some("local".into()),
                sensitive_local_only: Some(false),
                ..base_llm_update()
            },
        )
        .unwrap();
        assert_eq!(view.cloud_provider, "qwen");
        assert_eq!(view.local_backend, "lmstudio");
        assert_eq!(view.route_fast, "cloud");
        assert_eq!(view.route_deep, "local");
        assert!(!view.sensitive_local_only);
    }

    /// 🔴 关闭"敏感项目仅本地"必须留审计痕迹（安全约束降级要可追溯）。
    #[test]
    fn disabling_sensitive_local_only_is_audited() {
        let c = ctx();
        update_llm(
            &c,
            &LlmUpdate {
                sensitive_local_only: Some(false),
                ..base_llm_update()
            },
        )
        .unwrap();
        let audit = c.db.settings().recent_audit(10).unwrap();
        assert!(
            audit.iter().any(|e| e.summary.contains("敏感项目仅本地")),
            "应记录安全约束降级: {audit:?}"
        );
    }

    /// 保持开启（默认值）不该产生审计噪音。
    #[test]
    fn keeping_sensitive_local_only_is_not_audited() {
        let c = ctx();
        update_llm(
            &c,
            &LlmUpdate {
                sensitive_local_only: Some(true),
                ..base_llm_update()
            },
        )
        .unwrap();
        assert!(c.db.settings().recent_audit(10).unwrap().is_empty());
    }

    fn base_llm_update() -> LlmUpdate {
        LlmUpdate {
            cloud_provider: None,
            cloud_base_url: None,
            cloud_api_key: None,
            cloud_model: None,
            local_backend: None,
            local_base_url: None,
            local_model: None,
            route_fast: None,
            route_deep: None,
            sensitive_local_only: None,
            embedding_local_only: None,
        }
    }

    // ── 外观与统一更新 ───────────────────────────────────────────

    #[test]
    fn appearance_updates() {
        let c = ctx();
        let app = update_appearance(
            &c,
            &AppearanceUpdate {
                theme: Some("light".into()),
                reduce_motion: Some(true),
            },
        )
        .unwrap();
        assert_eq!(app.theme, Theme::Light);
        assert!(app.reduce_motion);

        assert!(matches!(
            update_appearance(&c, &AppearanceUpdate { theme: Some("neon".into()), reduce_motion: None })
                .unwrap_err(),
            ServiceError::Invalid(_)
        ));
    }

    #[test]
    fn partial_update_only_touches_given_sections() {
        let c = ctx();
        let dir = real_dir();
        add_dir(&c, &DirRequest { path: dir.path().to_string_lossy().into() }).unwrap();

        // 只改外观，不该动模型与扫描配置
        let view = update(
            &c,
            &SettingsUpdate {
                llm: None,
                appearance: Some(AppearanceUpdate {
                    theme: Some("light".into()),
                    reduce_motion: None,
                }),
            },
        )
        .unwrap();
        assert_eq!(view.appearance.theme, Theme::Light);
        assert_eq!(view.scan.dirs.len(), 1, "扫描目录不该被清掉");
        assert_eq!(view.llm.route_fast, "local", "模型配置不该被重置");
    }

    #[test]
    fn empty_update_is_noop() {
        let c = ctx();
        let before = load(&c).unwrap();
        let after = update(&c, &SettingsUpdate::default()).unwrap();
        assert_eq!(before.llm.cloud_model, after.llm.cloud_model);
        assert_eq!(before.scan.max_depth, after.scan.max_depth);
    }

    // ── 连接测试 ─────────────────────────────────────────────────

    /// 未配置时返回结构化失败结果（带引导），而不是抛异常。
    #[tokio::test]
    async fn test_connection_returns_guidance_when_unconfigured() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.local_base_url = String::new();
        c.db.settings().save_llm(&s.llm).unwrap();

        let v = test_connection(
            &c,
            &TestConnectionRequest {
                route: Some("local".into()),
            },
        )
        .await
        .unwrap();
        assert!(!v.ok);
        assert!(!v.message.is_empty(), "应给出说明");
        assert_eq!(v.route, "local");
        assert_eq!(v.route_label, "本地模型");
    }

    #[tokio::test]
    async fn test_connection_rejects_unknown_route() {
        let c = ctx();
        let err = test_connection(&c, &TestConnectionRequest { route: Some("bogus".into()) })
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::Invalid(_)));
    }

    /// 连不上的本地端点：失败结果要指明怎么启动服务。
    #[tokio::test]
    async fn test_connection_reports_unreachable_backend() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.local_backend = LocalBackend::Ollama;
        s.llm.local_base_url = "http://127.0.0.1:1".into();
        s.llm.local_model = "qwen3:8b".into();
        c.db.settings().save_llm(&s.llm).unwrap();

        let v = test_connection(&c, &TestConnectionRequest { route: None }).await.unwrap();
        assert!(!v.ok);
        assert!(v.message.contains("ollama serve"), "实际: {}", v.message);
        assert_eq!(v.model, "qwen3:8b");
        assert_eq!(v.backend, "ollama");
    }

    // ── 审计与数据管理 ───────────────────────────────────────────

    #[test]
    fn audit_view_includes_labels() {
        let c = ctx();
        c.db
            .settings()
            .audit(&projectassests_domain::AuditEntry::llm_ok(
                projectassests_storage::now_utc(),
                "local:qwen3:8b",
                RouteTarget::Local,
                "ANALYZE_PROJECT",
                "生成项目画像",
                Some("p1".into()),
            ))
            .unwrap();
        let v = recent_audit(&c, 10).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].route_label, "本地模型");
        assert_eq!(v[0].model, "local:qwen3:8b");
        assert_eq!(v[0].project_id.as_deref(), Some("p1"));
        assert_eq!(v[0].ok, Some(true));
        assert_eq!(v[0].error, None);
    }

    /// 🔴 三类条目的视图字段必须各自正确，前端要靠它们区分渲染。
    #[test]
    fn audit_view_exposes_tristate_ok() {
        let c = ctx();
        c.db
            .settings()
            .audit(&projectassests_domain::AuditEntry::llm_failed(
                projectassests_storage::now_utc(),
                "cloud:bad-model",
                RouteTarget::Cloud,
                "ANALYZE_PROJECT",
                "生成项目画像（失败）",
                Some("p1".into()),
                "请求被拒绝 (400): The product is not activated",
            ))
            .unwrap();
        c.db
            .settings()
            .audit(&projectassests_domain::AuditEntry::event(
                projectassests_storage::now_utc(),
                "SETTINGS",
                "用户关闭了「敏感项目仅本地」约束",
                None,
            ))
            .unwrap();

        let v = recent_audit(&c, 10).unwrap();
        assert_eq!(v.len(), 2);
        // 倒序：后写入的事件在前
        let event = v.iter().find(|e| e.model == "-").expect("应有安全事件");
        assert_eq!(event.ok, None, "安全事件的 ok 必须是 None，前端据此不渲染成败标记");
        assert_eq!(event.error, None);
        let failed = v.iter().find(|e| e.model == "cloud:bad-model").expect("应有失败调用");
        assert_eq!(failed.ok, Some(false));
        assert_eq!(
            failed.error.as_deref(),
            Some("请求被拒绝 (400): The product is not activated"),
            "失败原因必须透传到视图层"
        );
    }

    #[test]
    fn audit_limit_is_clamped() {
        let c = ctx();
        // 不 panic 即可（clamp 到 1..200）
        assert!(recent_audit(&c, 0).is_ok());
        assert!(recent_audit(&c, 99999).is_ok());
    }

    /// 🔴 清除派生数据的保留契约（service 层口径）。
    ///
    /// 每一项都对应一个真实缺陷或安全后果：
    /// - 设置/授权目录被清 → 用户要重新配一遍，且下次启动无法扫描
    /// - 项目行被删 → 用户手写的 `sensitive` 标记消失，重扫后敏感项目
    ///   可能被送去云端模型（安全事故，不只是数据丢失）
    /// - 审计日志被清 → 「可审计」形同虚设
    #[test]
    fn clear_derived_data_preserves_settings_projects_and_audit() {
        let c = ctx();
        let dir = real_dir();
        add_dir(&c, &DirRequest { path: dir.path().to_string_lossy().into() }).unwrap();
        // 敏感标记：Local-First 红线的用户侧开关
        let mut p = project("p1");
        p.sensitive = true;
        p.description = "内部风控系统".into();
        c.db.projects().upsert(&p).unwrap();
        c.db
            .assets()
            .upsert(&Asset {
                id: "a1".into(),
                project_id: "p1".into(),
                asset_type: AssetType::Component,
                name: "Comp".into(),
                description: "d".into(),
                content: None,
                source_path: "a.py".into(),
                confidence: 0.9,
                reuse_score: 0.9,
                generality: 0.7,
                stability: 0.6,
                tags: vec![],
                created_at: "2026-09-29".into(),
                evidence: Evidence {
                    files: vec!["a.py".into()],
                    ..Evidence::default()
                },
                user_feedback: None,
            })
            .unwrap();
        c.db.settings()
            .audit(&projectassests_domain::AuditEntry::llm_ok(
                projectassests_storage::now_utc(),
                "local:qwen3:8b",
                projectassests_domain::RouteTarget::Local,
                "ANALYZE_PROJECT",
                "生成项目画像",
                Some("p1".into()),
            ))
            .unwrap();
        // 🔴 失败记录也要写一条：它是"数据曾出网"的唯一凭证，
        // 「审计不可被普通操作抹掉」这条契约对它同样必须成立。
        c.db.settings()
            .audit(&projectassests_domain::AuditEntry::llm_failed(
                projectassests_storage::now_utc(),
                "cloud:bad-model",
                projectassests_domain::RouteTarget::Cloud,
                "ANALYZE_PROJECT",
                "生成项目画像（失败）",
                Some("p1".into()),
                "请求被拒绝 (400)",
            ))
            .unwrap();

        let result = clear_derived_data(&c).unwrap();

        // 派生数据确实被清了
        assert_eq!(result.cleared["assets"], 1);
        assert_eq!(c.db.assets().count_all().unwrap(), 0);

        // 键名必须是 projects_reset：前端把它原样渲染成 "xxx: N 行"，
        // 写成 projects 会让用户读成"删除了 1 个项目"——而项目清单其实还在。
        assert_eq!(result.cleared["projects_reset"], 1);
        assert!(
            !result.cleared.contains_key("projects"),
            "不得用 projects 作为键名，会误导成'删了 N 个项目'"
        );
        assert!(
            !result.cleared.contains_key("audit_log"),
            "审计日志不在清理范围内，不该出现在 cleared 里"
        );

        // 设置与授权目录必须还在
        let view = load(&c).unwrap();
        assert_eq!(view.scan.dirs.len(), 1, "授权目录不该被清掉");

        // 项目行必须还在，敏感标记与描述原样保留
        assert_eq!(c.db.projects().count().unwrap(), 1, "项目清单不得被清空");
        let kept = c.db.projects().get("p1").unwrap().expect("项目行应保留");
        assert!(kept.sensitive, "敏感标记丢失会让敏感项目被送去云端模型");
        assert_eq!(kept.description, "内部风控系统", "用户手写描述应保留");

        // 审计日志必须还在——🔴 成功与失败两类都要在。
        // 失败记录是"那次数据确实出过网"的唯一凭证，
        // 清理派生数据把它一起抹掉，等于销毁审计证据。
        let kept_audit = recent_audit(&c, 10).unwrap();
        assert_eq!(kept_audit.len(), 2, "审计日志不得被清除");
        assert!(
            kept_audit.iter().any(|e| e.ok == Some(true)),
            "成功调用记录应存活"
        );
        assert!(
            kept_audit.iter().any(|e| e.ok == Some(false) && e.error.is_some()),
            "🔴 失败调用记录（含原因）必须存活"
        );

        // preserved 必须如实列出保留项（前端确认弹窗直接展示它）
        let joined = result.preserved.join(" ");
        assert!(joined.contains("审计"), "preserved 应提到审计日志: {joined}");
        assert!(joined.contains("项目"), "preserved 应提到项目清单: {joined}");
        assert!(
            !joined.contains("反馈"),
            "user_feedback 随行删除，不得声称保留: {joined}"
        );
    }

    /// 导出配置不得含 API Key 与项目路径。
    #[test]
    fn export_excludes_secrets() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.cloud_api_key = "sk-export-secret-12345".into();
        c.db.settings().save_llm(&s.llm).unwrap();

        let exported = export_config(&c).unwrap();
        let joined: String = exported.iter().map(|(k, v)| format!("{k}={v}")).collect();
        assert!(
            !joined.contains("sk-export-secret"),
            "导出内容泄漏了 key: {joined}"
        );
    }

    // ── 数据库视图 ───────────────────────────────────────────────

    #[test]
    fn db_view_reports_real_stats() {
        let c = ctx();
        c.db.projects().upsert(&project("p1")).unwrap();
        let view = load(&c).unwrap();
        assert!(!view.db.size_display.is_empty());
        assert!(view.db.schema_version >= 1);
        assert!(
            view.db.tables.iter().any(|t| t.table == "projects" && t.rows == 1),
            "应含真实表计数: {:?}",
            view.db.tables
        );
    }
}
