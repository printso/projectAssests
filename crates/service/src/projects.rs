//! 项目服务：列表、详情、状态切换、AI 画像入口。
//!
//! # 项目详情页的四块内容，各自的真实性来源
//! | 区块 | 来源 | 无数据时 |
//! |---|---|---|
//! | 代码结构（文件/行数/语言构成） | 扫描器 Level 0 真实统计 | 显示 0，不编造 |
//! | 项目考古（提交数/时间跨度/叙述） | Git 历史 + LLM 叙述 | 无 Git 则整块隐藏 |
//! | 能力覆盖 | 三层能力图谱真实节点 | 显示空态 |
//! | 亮点 | LLM 画像（每条带真实文件证据） | 显示"未分析"+ 分析按钮 |
//!
//! 🔴 原型的 `archaeology.narrative` 是手写死的（"共 37 次 AI Coding Session"），
//! `highlights` 是四条固定文案。两者都改成：**没有真实来源就不显示**，
//! 并给出触发分析的入口。假数据比空数据危害大——用户会当真。

use serde::{Deserialize, Serialize};
// `Archaeology` / `RelationType` 只在测试里具名出现（主代码靠类型推断），
// 故不放进这份 use 列表，避免未使用导入警告
use spolia_domain::{LanguageShare, Project, ProjectAiProfile, ProjectHighlight, ProjectStatus, Relation};
use spolia_storage::{ProjectFilter, ProjectSort};

use crate::context::{ServiceContext, ServiceError};

// ══════════════════════════════════════════════════════════════════
// 列表
// ══════════════════════════════════════════════════════════════════

/// 项目列表查询。
/// 项目列表查询。
///
/// 🔴 `Default` 是**手写**的而非 derive：derive 会给 `limit = 0`，
/// 而 `#[serde(default = "default_page_size")]` 只在反序列化时生效。
/// 两者不一致的后果是——HTTP 请求省略 limit 得到 24 条，
/// 而 Rust 调用方用 `ProjectListQuery::default()` 只得到 1 条
/// （0 被 `clamp(1, 200)` 钳成 1）。这类 bug 只在非 HTTP 路径出现，极难发现。
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectListQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub keyword: Option<String>,
    /// 只看敏感 / 只看非敏感；`None` = 全部
    #[serde(default)]
    pub sensitive: Option<bool>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default = "default_page_size")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

impl Default for ProjectListQuery {
    fn default() -> Self {
        Self {
            status: None,
            language: None,
            keyword: None,
            sensitive: None,
            sort: None,
            limit: default_page_size(),
            offset: 0,
        }
    }
}

impl ProjectListQuery {
    /// 实际使用的页大小。
    ///
    /// `0` 一律视为"未指定"而回落到默认值，**不是**钳到 1。
    /// 前端清空分页控件时可能传 0，此时返回 1 条会让用户以为数据丢了。
    pub fn effective_limit(&self) -> u32 {
        if self.limit == 0 {
            default_page_size()
        } else {
            self.limit.clamp(1, MAX_PAGE_SIZE)
        }
    }
}

/// 默认页大小。
fn default_page_size() -> u32 {
    24
}

/// 页大小上限。
///
/// 必须有上限：本地库虽小，但一次返回上万条会让前端渲染卡死，
/// 而"加载更多"本就是列表页该有的交互。
pub const MAX_PAGE_SIZE: u32 = 200;

/// 列表项（比详情轻量：不含语言构成明细与画像）。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectListItem {
    pub id: String,
    pub name: String,
    pub path: String,
    pub description: String,
    pub language: String,
    pub framework: String,
    pub status: String,
    pub status_label: String,
    pub health_score: u8,
    pub files: usize,
    pub loc: usize,
    pub tags: Vec<String>,
    pub sensitive: bool,
    /// 距最后活动天数；`None` = 时间未知（前端显示"未知"而非"今天"）
    pub days_idle: Option<i64>,
    /// 相对时间文案（"3 天前"），服务端算好保证各处格式一致
    pub updated_display: String,
    pub has_git: bool,
    pub git_commits: u32,
    pub has_readme: bool,
    pub has_tests: bool,
    /// 是否已有 AI 画像（前端据此显示"查看画像"或"生成画像"）
    pub has_profile: bool,
    /// 主语言占比（列表页的语言条）
    pub primary_language_pct: u8,
}

/// 分页结果。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectListPage {
    pub items: Vec<ProjectListItem>,
    pub total: usize,
    pub limit: u32,
    pub offset: u32,
    /// 筛选面板的选项（来自真实数据分布，不是写死的枚举）
    pub facets: ProjectFacets,
}

/// 筛选面：各维度的可选值与计数。
///
/// 🔴 必须来自真实数据分布：写死的语言列表会出现"Rust (0)"这种选项，
/// 用户点进去发现是空的，会怀疑筛选功能坏了。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectFacets {
    pub languages: Vec<FacetItem>,
    pub statuses: Vec<FacetItem>,
}

/// 单个筛选项。
#[derive(Debug, Clone, Serialize)]
pub struct FacetItem {
    pub value: String,
    pub label: String,
    pub count: usize,
}

/// 列表页。
pub fn list(ctx: &ServiceContext, q: &ProjectListQuery) -> Result<ProjectListPage, ServiceError> {
    // 🔴 用 effective_limit() 而非 `q.limit.clamp(1, 200)`：
    // clamp 会把 limit=0 钳成 1（只返回一条），而 0 的真实语义是"未指定，用默认页大小"。
    // 前端清空分页控件时就会传 0，钳成 1 会让用户以为数据丢了。
    let limit = q.effective_limit();
    let filter = ProjectFilter {
        status: parse_status(&q.status)?,
        language: non_empty(&q.language),
        keyword: non_empty(&q.keyword),
        sensitive: q.sensitive,
        limit: Some(limit),
        offset: q.offset,
    };
    let sort = parse_sort(&q.sort)?;

    let items = ctx
        .db
        .projects()
        .list(&filter, sort)?
        .into_iter()
        .map(|p| list_item(&p, ctx.now()))
        .collect();

    // total 必须是"过滤后"的总数，否则分页器页数算错
    let total = ctx.db.projects().count_filtered(&filter)?;

    Ok(ProjectListPage {
        items,
        total,
        // 回显实际生效的页大小，前端据此计算总页数；
        // 不能回显 q.limit（可能是 0），否则前端算出 0 页
        limit,
        offset: q.offset,
        facets: facets(ctx)?,
    })
}

/// 构建筛选面（语言与状态的真实分布）。
fn facets(ctx: &ServiceContext) -> Result<ProjectFacets, ServiceError> {
    let languages = ctx
        .db
        .projects()
        .language_distribution()?
        .into_iter()
        .map(|(name, count)| FacetItem {
            value: name.clone(),
            label: name,
            count,
        })
        .collect();

    // 状态：即使某状态计数为 0 也要列出——用户需要知道有哪些状态可选，
    // 且"活跃 (0)"本身是有用信息（说明该重扫了）
    let counts: std::collections::HashMap<String, usize> = ctx
        .db
        .projects()
        .status_distribution()?
        .into_iter()
        .collect();
    let statuses = [
        ProjectStatus::Active,
        ProjectStatus::Paused,
        ProjectStatus::Abandoned,
        ProjectStatus::Experimental,
    ]
    .into_iter()
    .map(|s| FacetItem {
        value: s.as_str().to_string(),
        label: s.label_zh().to_string(),
        count: counts.get(s.as_str()).copied().unwrap_or(0),
    })
    .collect();

    Ok(ProjectFacets { languages, statuses })
}

fn list_item(p: &Project, now: chrono::DateTime<chrono::Utc>) -> ProjectListItem {
    let days_idle = p.days_since_update(now);
    ProjectListItem {
        id: p.id.clone(),
        name: p.name.clone(),
        path: p.path.clone(),
        description: p.description.clone(),
        language: p.language.clone(),
        framework: p.framework.clone(),
        status: p.status.as_str().to_string(),
        status_label: p.status.label_zh().to_string(),
        health_score: p.health_score,
        files: p.stats.files,
        loc: p.stats.loc,
        tags: p.tags.clone(),
        sensitive: p.sensitive,
        days_idle,
        updated_display: days_idle
            .map(|d| spolia_storage::relative_time(&days_ago_iso(d, now), now))
            .unwrap_or_else(|| "时间未知".to_string()),
        has_git: p.scan.has_git,
        git_commits: p.scan.git_commits,
        has_readme: p.scan.has_readme,
        has_tests: p.scan.has_tests,
        has_profile: p.ai_profile.is_some(),
        primary_language_pct: p.stats.languages.first().map(|l| l.pct).unwrap_or(0),
    }
}

/// 把"距今 N 天"还原成 ISO 日期串，交给 `relative_time` 统一格式化。
///
/// 不自己拼"3 天前"：`relative_time` 已在存储层实现了分档（分钟/小时/天/月），
/// 这里再造一套必然与首页活动流的时间文案不一致。
fn days_ago_iso(days: i64, now: chrono::DateTime<chrono::Utc>) -> String {
    (now - chrono::Duration::days(days))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn parse_status(s: &Option<String>) -> Result<Option<ProjectStatus>, ServiceError> {
    match non_empty(s) {
        Some(v) => Ok(Some(ProjectStatus::parse(&v))),
        None => Ok(None),
    }
}

fn parse_sort(s: &Option<String>) -> Result<ProjectSort, ServiceError> {
    Ok(match non_empty(s).as_deref() {
        None | Some("recent") => ProjectSort::RecentlyUpdated,
        Some("health") => ProjectSort::Health,
        Some("name") => ProjectSort::Name,
        Some("size") => ProjectSort::Size,
        Some(other) => {
            return Err(ServiceError::Invalid(format!(
                "未知的排序方式：{other}（可选 recent / health / name / size）"
            )))
        }
    })
}

/// 空串与纯空白视为"未提供"。
///
/// 🔴 前端清空筛选框时常发 `?language=`（空串），
/// 若当成真实过滤条件会生成 `WHERE language = ''`，返回空列表——
/// 用户以为筛选坏了，实际只是空串没被当作"无筛选"。
fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

// ══════════════════════════════════════════════════════════════════
// 详情
// ══════════════════════════════════════════════════════════════════

/// 项目详情。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectDetail {
    /// 基础信息（与列表项同源，前端可直接复用渲染逻辑）
    pub summary: ProjectListItem,
    /// 代码结构
    pub structure: StructureView,
    /// 项目考古；无 Git 历史时为 `None`（整块隐藏而非显示空壳）
    pub archaeology: Option<ArchaeologyView>,
    /// 能力覆盖（真实图谱节点）
    pub capabilities: Vec<CapabilityCoverage>,
    /// AI 画像；未分析时为 `None`（前端显示"生成画像"按钮）
    pub profile: Option<ProfileView>,
    /// 相关项目（基于真实的 similar_to 关系，不是硬编码相似度）
    pub similar: Vec<SimilarProject>,
    /// 该项目的高价值资产
    pub assets: Vec<AssetBrief>,
    /// 该项目的洞察
    pub insights: Vec<InsightBrief>,
}

/// 代码结构视图。
#[derive(Debug, Clone, Serialize)]
pub struct StructureView {
    pub files: usize,
    pub loc: usize,
    /// 人类可读的代码量（"86.2k 行"）
    pub loc_display: String,
    pub symbols: usize,
    pub modules: usize,
    pub languages: Vec<LanguageView>,
    /// 检测标志（工程质量信号）
    pub has_git: bool,
    pub has_readme: bool,
    pub has_tests: bool,
    pub has_license: bool,
    pub has_docker: bool,
}

/// 语言构成条目。
#[derive(Debug, Clone, Serialize)]
pub struct LanguageView {
    pub name: String,
    pub pct: u8,
    pub loc: usize,
    /// 官方色板色值（服务端算好，前端零判断）
    pub color: String,
}

/// 考古视图。
#[derive(Debug, Clone, Serialize)]
pub struct ArchaeologyView {
    pub commits: u32,
    /// AI Coding Session 数；未接入会话历史时为 `None`（不显示 0，避免误解为"没有"）
    pub sessions: Option<u32>,
    /// 完成度；无法可靠估算时为 `None`
    pub completeness: Option<f64>,
    pub phase: Option<String>,
    pub salvage: Vec<String>,
    pub narrative: String,
    /// 叙述来源：模型生成 / 真实数据拼装。
    /// 🔴 必须标注，否则用户会把模板拼装的话当成 AI 分析结论。
    pub narrative_source: NarrativeSource,
    /// 时间跨度
    pub first_commit_at: Option<String>,
    pub last_commit_at: Option<String>,
    pub days_idle: Option<i64>,
    pub branch: Option<String>,
}

/// 叙述来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NarrativeSource {
    /// 由 LLM 基于真实 Git 数据生成
    Model,
    /// 由确定性模板拼装（模型未给叙述时的兜底）
    Deterministic,
}

impl NarrativeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Deterministic => "deterministic",
        }
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Model => "模型生成",
            Self::Deterministic => "数据拼装",
        }
    }
}

/// 能力覆盖条目。
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityCoverage {
    pub id: String,
    pub name: String,
    /// 所属 Domain（图谱分组用）
    pub domain: Option<String>,
    /// 置信度 0.0-1.0（前端渲染成百分比条）
    pub confidence: f64,
    /// 命中该能力的真实信号（依赖名/文件），作为证据展示
    pub evidence: Vec<String>,
}

/// AI 画像视图。
#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub summary: String,
    pub purpose: Option<String>,
    pub phase: Option<String>,
    pub highlights: Vec<HighlightView>,
    pub generated_by: String,
    pub generated_at: String,
}

/// 亮点视图（每条都带可跳转的真实文件）。
#[derive(Debug, Clone, Serialize)]
pub struct HighlightView {
    pub title: String,
    pub desc: String,
    /// 支撑该亮点的真实文件（相对路径 + 可跳转链接）
    pub evidence_files: Vec<EvidenceFile>,
}

/// 证据文件。
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceFile {
    /// 相对项目根的路径（展示用）
    pub path: String,
    /// 绝对路径（前端"在编辑器中打开"用）
    pub absolute: String,
    /// 文件当前是否还存在（被删/被移动的要标出来，否则点击报错）
    pub exists: bool,
}

/// 相关项目。
#[derive(Debug, Clone, Serialize)]
pub struct SimilarProject {
    pub id: String,
    pub name: String,
    /// 相似度 = relation.confidence（真实计算值，不是原型里硬编码的 87%）
    pub similarity: f64,
    /// 相似依据（共有的能力/资产名）
    pub basis: Vec<String>,
}

/// 资产摘要。
#[derive(Debug, Clone, Serialize)]
pub struct AssetBrief {
    pub id: String,
    pub name: String,
    pub asset_type: String,
    pub type_label: String,
    pub source_path: String,
    pub reuse_score: f64,
    pub tier: String,
    pub description: String,
}

/// 洞察摘要。
#[derive(Debug, Clone, Serialize)]
pub struct InsightBrief {
    pub id: String,
    pub title: String,
    pub description: String,
    pub insight_type: String,
    pub type_label: String,
    pub badge: String,
    pub confidence: f64,
}

/// 相似项目的最低置信度。
///
/// 低于此值的关系不展示：勉强算出的"相似"会误导用户去复用不相关的代码。
pub const MIN_SIMILARITY: f64 = 0.5;

/// 详情页最多展示多少资产/洞察（其余引导去专页）。
pub const DETAIL_ASSETS_LIMIT: usize = 8;
pub const DETAIL_INSIGHTS_LIMIT: usize = 5;

/// 项目详情。
pub fn detail(ctx: &ServiceContext, id: &str) -> Result<ProjectDetail, ServiceError> {
    let project = ctx
        .db
        .projects()
        .get(id)?
        .ok_or_else(|| ServiceError::NotFound(format!("项目 {id}")))?;
    let now = ctx.now();

    Ok(ProjectDetail {
        summary: list_item(&project, now),
        structure: structure_view(&project),
        archaeology: archaeology_view(ctx, &project, now),
        capabilities: capabilities_of(ctx, id)?,
        profile: project.ai_profile.as_ref().map(|p| profile_view(p, &project)),
        similar: similar_projects(ctx, id)?,
        assets: project_assets(ctx, id)?,
        insights: project_insights(ctx, id)?,
    })
}

fn structure_view(p: &Project) -> StructureView {
    StructureView {
        files: p.stats.files,
        loc: p.stats.loc,
        loc_display: format_loc(p.stats.loc),
        symbols: p.stats.symbols,
        modules: p.stats.modules,
        languages: p.stats.languages.iter().map(language_view).collect(),
        has_git: p.scan.has_git,
        has_readme: p.scan.has_readme,
        has_tests: p.scan.has_tests,
        // 🔴 许可证/Docker 检测当前扫描器未采集，如实返回 false 而非编造。
        // 宁可不显示这个信号，也不能显示一个假的"有许可证"。
        has_license: false,
        has_docker: false,
    }
}

fn language_view(l: &LanguageShare) -> LanguageView {
    LanguageView {
        name: l.name.clone(),
        pct: l.pct,
        loc: l.loc,
        color: language_color(&l.name).to_string(),
    }
}

/// 代码量的人类可读格式。
///
/// 与首页/资产页共用同一套分档，避免出现"86k 行"与"86,000 行"并存。
pub fn format_loc(loc: usize) -> String {
    if loc >= 1_000_000 {
        format!("{:.1}M 行", loc as f64 / 1_000_000.0)
    } else if loc >= 1_000 {
        format!("{:.1}k 行", loc as f64 / 1_000.0)
    } else {
        format!("{loc} 行")
    }
}

/// 语言 → 色值（官方色板，与图谱同源）。
fn language_color(language: &str) -> &'static str {
    use spolia_domain::colors;
    // 先按语言名找专用色，找不到就按"代码"类回落
    match language.to_ascii_lowercase().as_str() {
        "python" => "#3572A5",
        "javascript" => "#f1e05a",
        "typescript" => "#3178c6",
        "rust" => "#dea584",
        "go" => "#00ADD8",
        "java" => "#b07219",
        "c" => "#555555",
        "c++" | "cpp" => "#f34b7d",
        "c#" | "csharp" => "#178600",
        "vue" => "#41b883",
        "html" => "#e34c26",
        "css" | "scss" => "#563d7c",
        "shell" | "bash" => "#89e051",
        _ => colors::for_key("code"),
    }
}

/// 考古视图：无 Git 历史则返回 `None`（整块隐藏）。
fn archaeology_view(
    ctx: &ServiceContext,
    p: &Project,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<ArchaeologyView> {
    // 🔴 没有 Git 历史就不显示考古区块。
    // 原型里无论有没有 Git 都渲染一段"共 37 次 Session"的文案，那是纯编造。
    if !p.scan.has_git || p.scan.git_commits == 0 {
        return None;
    }

    let root = std::path::Path::new(&p.path);

    // 叙述优先用 AI 画像里的（已经过证据校验），否则用真实数据拼装
    let (narrative, source) = match &p.ai_profile {
        Some(profile) => match &profile.archaeology {
            Some(a) if !a.narrative.trim().is_empty() => {
                (a.narrative.clone(), NarrativeSource::Model)
            }
            _ => (deterministic_narrative(p, ctx), NarrativeSource::Deterministic),
        },
        None => (deterministic_narrative(p, ctx), NarrativeSource::Deterministic),
    };

    let from_profile = p
        .ai_profile
        .as_ref()
        .and_then(|pr| pr.archaeology.as_ref());

    Some(ArchaeologyView {
        commits: p.scan.git_commits,
        // sessions 需要接入会话历史（阶段三）；未接入时返回 None 而非 0，
        // 因为 0 会被读成"你从未用 AI 写过这个项目"，那是错误信息
        sessions: from_profile.map(|a| a.sessions).filter(|s| *s > 0),
        // 完成度无法可靠估算：不给假值（原型里的 0.86 是编的）
        completeness: p.completeness,
        phase: from_profile
            .and_then(|a| a.phase.clone())
            .or_else(|| p.ai_profile.as_ref().and_then(|pr| pr.phase.clone())),
        salvage: from_profile
            .map(|a| a.salvage.clone())
            .unwrap_or_else(|| top_salvage_names(ctx, &p.id)),
        narrative,
        narrative_source: source,
        first_commit_at: p.created_at.clone(),
        last_commit_at: p.last_commit_at.clone(),
        days_idle: p.days_since_update(now),
        branch: spolia_scanner::read_branch_from_head(root),
    })
}

/// 确定性考古叙述：只用真实数据。
fn deterministic_narrative(p: &Project, ctx: &ServiceContext) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("共 {} 次提交", p.scan.git_commits));
    if let (Some(first), Some(last)) = (&p.created_at, &p.last_commit_at) {
        parts.push(format!("时间跨度 {first} → {last}"));
    }
    if let Some(days) = p.days_since_update(ctx.now()) {
        parts.push(format!("距今 {days} 天未活动"));
    }
    parts.push(format!(
        "{}（{} 行代码）",
        or_unknown(&p.language),
        format_loc(p.stats.loc)
    ));
    if p.scan.has_tests {
        parts.push("有测试目录".to_string());
    } else {
        parts.push("未检测到测试目录".to_string());
    }
    let salvage = top_salvage_names(ctx, &p.id);
    if !salvage.is_empty() {
        parts.push(format!("可打捞资产：{}", salvage.join("、")));
    }
    format!("{}。（本段由真实 Git 与扫描数据拼装）", parts.join("，"))
}

/// 可打捞资产名（取该项目复用分最高的几个）。
fn top_salvage_names(ctx: &ServiceContext, project_id: &str) -> Vec<String> {
    ctx.db
        .assets()
        .list(
            &spolia_storage::AssetFilter {
                project_id: Some(project_id.to_string()),
                limit: Some(3),
                ..Default::default()
            },
            spolia_storage::AssetSort::ReuseScore,
        )
        .map(|list| list.into_iter().map(|a| a.name).collect())
        .unwrap_or_default()
}

fn or_unknown(s: &str) -> &str {
    if s.trim().is_empty() || s == "-" {
        "语言未知"
    } else {
        s
    }
}

/// 画像视图：把证据文件补成可跳转 + 存在性校验。
fn profile_view(profile: &ProjectAiProfile, p: &Project) -> ProfileView {
    ProfileView {
        summary: profile.summary.clone(),
        purpose: profile.purpose.clone(),
        phase: profile.phase.clone(),
        highlights: profile
            .highlights
            .iter()
            .map(|h| highlight_view(h, p))
            .collect(),
        generated_by: profile.generated_by.clone(),
        generated_at: profile.generated_at.clone(),
    }
}

/// 亮点视图。
///
/// 🔴 逐条校验文件是否仍存在：画像可能是几周前生成的，
/// 文件已被重命名/删除。不校验的话用户点击就报错，
/// 比不显示更糟。
fn highlight_view(h: &ProjectHighlight, p: &Project) -> HighlightView {
    HighlightView {
        title: h.title.clone(),
        desc: h.desc.clone(),
        evidence_files: h
            .evidence_files
            .iter()
            .map(|rel| {
                let absolute = std::path::Path::new(&p.path).join(rel);
                EvidenceFile {
                    exists: absolute.exists(),
                    path: rel.clone(),
                    absolute: absolute.to_string_lossy().to_string(),
                }
            })
            .collect(),
    }
}

/// 项目的能力覆盖。
fn capabilities_of(ctx: &ServiceContext, project_id: &str) -> Result<Vec<CapabilityCoverage>, ServiceError> {
    let relations: Vec<Relation> = ctx.db.relations().capabilities_of_project(project_id)?;
    let mut out = Vec::with_capacity(relations.len());

    for rel in relations {
        let Some(cap) = ctx.db.capabilities().get(&rel.target_id)? else {
            continue; // 能力节点已被清理，跳过（图谱可能正在重建）
        };
        // Domain 名：用于分组展示
        let domain = cap
            .parent_id
            .as_ref()
            .and_then(|pid| ctx.db.capabilities().get(pid).ok().flatten())
            .map(|d| d.name);

        out.push(CapabilityCoverage {
            id: cap.id,
            name: cap.name,
            domain,
            confidence: rel.confidence,
            // 关系的证据 = 命中该能力的真实信号（依赖名/框架名/符号名）
            evidence: rel.evidence.clone(),
        });
    }
    // 按置信度降序：最强的能力排最前
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// 相关项目。
///
/// 🔴 相似度取 `relation.confidence`（由真实的共同能力/资产算出），
/// 不是原型里硬编码的"相似度 87%"。
fn similar_projects(ctx: &ServiceContext, project_id: &str) -> Result<Vec<SimilarProject>, ServiceError> {
    let relations = ctx
        .db
        .relations()
        .similar_projects(project_id, MIN_SIMILARITY)?;

    let mut out = Vec::with_capacity(relations.len());
    for rel in relations {
        // 关系是双向的：用领域层的 other_end 取"另一端"，
        // 不自己比对 source/target（那是重复实现，且容易漏掉自环情况）
        let Some(other_id) = rel.other_end(project_id) else {
            continue; // 自环或无关关系
        };
        let Some(other) = ctx.db.projects().get(other_id)? else {
            continue; // 对端项目已删除
        };
        out.push(SimilarProject {
            id: other.id,
            name: other.name,
            similarity: rel.confidence,
            basis: rel.evidence.clone(),
        });
    }
    out.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// 该项目的高价值资产。
fn project_assets(ctx: &ServiceContext, project_id: &str) -> Result<Vec<AssetBrief>, ServiceError> {
    Ok(ctx
        .db
        .assets()
        .list(
            &spolia_storage::AssetFilter {
                project_id: Some(project_id.to_string()),
                // 无证据的资产不展示（产品红线）
                evidence_required: true,
                limit: Some(DETAIL_ASSETS_LIMIT as u32),
                ..Default::default()
            },
            spolia_storage::AssetSort::ReuseScore,
        )?
        .into_iter()
        .map(|a| {
            // 🔴 用 ReuseTier::from_score 而非 a.tier()：
            // tier() 是 DuplicateGroup 的方法（基于组内平均分），Asset 上没有。
            // 分档阈值集中在 domain 的 from_score 里，避免各处散落判断。
            let tier = spolia_domain::ReuseTier::from_score(a.reuse_score);
            AssetBrief {
                id: a.id,
                name: a.name,
                type_label: a.asset_type.label_zh().to_string(),
                asset_type: a.asset_type.as_str().to_string(),
                source_path: a.source_path,
                reuse_score: a.reuse_score,
                tier: tier.label_zh().to_string(),
                description: a.description,
            }
        })
        .collect())
}

/// 与该项目相关的洞察。
fn project_insights(ctx: &ServiceContext, project_id: &str) -> Result<Vec<InsightBrief>, ServiceError> {
    let all = ctx.db.insights().list(&spolia_storage::InsightFilter {
        limit: Some(200),
        ..Default::default()
    })?;

    let mut out: Vec<InsightBrief> = all
        .into_iter()
        .filter(|i| i.related_project_ids.iter().any(|pid| pid == project_id))
        .take(DETAIL_INSIGHTS_LIMIT)
        .map(|i| {
            let badge = i.badge();
            InsightBrief {
                id: i.id,
                title: i.title,
                description: i.description,
                type_label: i.insight_type.label_zh().to_string(),
                insight_type: i.insight_type.as_str().to_string(),
                badge: badge.label_zh().to_string(),
                confidence: i.confidence,
            }
        })
        .collect();
    // 按置信度降序（与洞察页一致）
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

// ══════════════════════════════════════════════════════════════════
// 操作
// ══════════════════════════════════════════════════════════════════

/// 敏感标记更新请求。
#[derive(Debug, Clone, Deserialize)]
pub struct SensitiveRequest {
    pub sensitive: bool,
}

/// 设置项目的敏感标记。
///
/// 🔴 标记为敏感后，该项目数据不再进入云端模型上下文（Local-First 硬约束）。
/// 取消标记是安全降级，必须记审计。
pub fn set_sensitive(
    ctx: &ServiceContext,
    project_id: &str,
    req: &SensitiveRequest,
) -> Result<ProjectListItem, ServiceError> {
    let before = ctx
        .db
        .projects()
        .get(project_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("项目 {project_id}")))?;

    ctx.db.projects().set_sensitive(project_id, req.sensitive)?;

    // 从敏感变为不敏感 = 安全约束降级，留痕
    if before.sensitive && !req.sensitive {
        // 🔴 用 `event()` 而非 `llm_ok()`：这不是一次模型调用，没有成败概念。
        // `ok = None` ⇒ 前端不渲染成败标记。若误用 `llm_ok` 填 `Some(true)`，
        // UI 会在「取消敏感标记」旁显示绿色对勾——把一次安全降级渲染成"操作成功"。
        let _ = ctx.db.settings().audit(&spolia_domain::AuditEntry::event(
            spolia_storage::now_utc(),
            "SETTINGS",
            format!("取消项目「{}」的敏感标记", before.name),
            Some(project_id.to_string()),
        ));
        tracing::warn!(project = %project_id, "用户取消了项目的敏感标记");
    }

    let after = ctx
        .db
        .projects()
        .get(project_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("项目 {project_id}")))?;
    Ok(list_item(&after, ctx.now()))
}

/// 更新项目描述（用户可修正 README 摘要提取不准的情况）。
#[derive(Debug, Clone, Deserialize)]
pub struct DescriptionRequest {
    pub description: String,
}

pub fn set_description(
    ctx: &ServiceContext,
    project_id: &str,
    req: &DescriptionRequest,
) -> Result<ProjectListItem, ServiceError> {
    let mut p = ctx
        .db
        .projects()
        .get(project_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("项目 {project_id}")))?;

    let desc = req.description.trim();
    if desc.chars().count() > 500 {
        return Err(ServiceError::Invalid(
            "描述过长（上限 500 字），过长的描述在列表里会被截断而失去意义".to_string(),
        ));
    }
    p.description = desc.to_string();
    ctx.db.projects().upsert(&p)?;

    let after = ctx
        .db
        .projects()
        .get(project_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("项目 {project_id}")))?;
    Ok(list_item(&after, ctx.now()))
}

/// 删除项目（连同其资产与关系）。
///
/// 🔴 只删数据库记录，**绝不删用户磁盘上的文件**。
/// 这个区别必须在 API 文档与前端确认弹窗里都写清楚。
pub fn remove(ctx: &ServiceContext, project_id: &str) -> Result<(), ServiceError> {
    let exists = ctx
        .db
        .projects()
        .get(project_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("项目 {project_id}")))?;

    let deleted = ctx.db.projects().delete(project_id)?;
    if !deleted {
        return Err(ServiceError::NotFound(format!("项目 {project_id}")));
    }

    let _ = ctx.db.activities().push(
        spolia_storage::ActivityIcon::Alert,
        "已移除项目记录",
        format!("{}（磁盘文件未删除）", exists.name),
    );
    Ok(())
}

/// 触发单个项目的重新索引。
///
/// 🔴 走任务队列，不在请求线程里同步跑（可能几十秒）。
pub async fn reindex(ctx: &ServiceContext, project_id: &str) -> Result<String, ServiceError> {
    let p = ctx
        .db
        .projects()
        .get(project_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("项目 {project_id}")))?;

    let payload = serde_json::json!({ "project_id": p.id });
    Ok(ctx
        .jobs
        .submit(spolia_domain::JobType::IndexCode, Some(payload))
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::{
        Archaeology, Asset, AssetType, Capability, CapabilityLayer, CodeStats, Evidence, Insight,
        InsightType, Relation, RelationType, ScanFacts,
    };

    fn ctx() -> ServiceContext {
        ServiceContext::in_memory().unwrap()
    }

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.into(),
            // description 派生自 name：若硬编码成含"视频"的固定串，
            // 关键词搜索测试就无法区分项目（每个都命中），失去意义
            description: format!("{name} 的描述"),
            name: name.into(),
            path: format!("/tmp/{id}"),
            language: "Python".into(),
            framework: "FastAPI".into(),
            created_at: Some("2024-03-01".into()),
            updated_at: Some("2026-09-20".into()),
            last_commit_at: Some("2026-09-20".into()),
            status: ProjectStatus::Active,
            health_score: 85,
            completeness: None,
            tags: vec!["pytorch".into()],
            sensitive: false,
            stats: CodeStats {
                files: 120,
                loc: 8600,
                symbols: 72,
                modules: 13,
                languages: vec![
                    LanguageShare {
                        name: "Python".into(),
                        pct: 92,
                        loc: 7900,
                    },
                    LanguageShare {
                        name: "Shell".into(),
                        pct: 8,
                        loc: 700,
                    },
                ],
            },
            scan: ScanFacts {
                git_commits: 87,
                has_git: true,
                has_readme: true,
                has_tests: true,
                scanned_at: Some("2026-09-29T10:00:00Z".into()),
            },
            ai_profile: None,
        }
    }

    fn asset(id: &str, pid: &str, name: &str, reuse: f64) -> Asset {
        Asset {
            id: id.into(),
            project_id: pid.into(),
            asset_type: AssetType::Component,
            name: name.into(),
            description: format!("{name} 的说明"),
            content: None,
            source_path: format!("src/{id}.py"),
            confidence: 0.9,
            reuse_score: reuse,
            generality: 0.7,
            stability: 0.6,
            tags: vec![],
            created_at: "2026-09-29".into(),
            evidence: Evidence {
                files: vec![format!("src/{id}.py")],
                ..Evidence::default()
            },
            user_feedback: None,
        }
    }

    fn seed(c: &ServiceContext) {
        for (id, name) in [("p1", "视频管线"), ("p2", "图片工具")] {
            seed_project(c, &project(id, name));
        }
        c.db
            .assets()
            .upsert_batch(&[
                asset("a1", "p1", "VideoPipeline", 0.92),
                asset("a2", "p1", "TaskQueue", 0.6),
            ])
            .unwrap();
    }

    /// 写入单个项目，并按真实流水线的顺序补齐 Level 0 / Level 1 数据。
    ///
    /// 🔴 `upsert` 刻意**不写** scan 列（git 事实）与 symbol/module 列（Level 1 统计）——
    /// 那是 `update_scan_facts` 与 `update_symbol_stats` 的职责，
    /// 目的是防止"更新描述"这类操作把 Git 统计或符号数清零。
    ///
    /// 因此只在 `Project` 结构体里设这些字段是**不会入库**的。
    /// 这个助手完整模拟流水线的三阶段写入，避免每个测试都踩同一个坑。
    fn seed_project(c: &ServiceContext, p: &Project) {
        let scan = p.scan.clone();
        let symbols = spolia_domain::SymbolStats {
            symbol_count: p.stats.symbols,
            module_count: p.stats.modules,
        };
        c.db.projects().upsert(p).unwrap();
        c.db.projects().update_scan_facts(&p.id, &scan).unwrap();
        c.db.projects().update_symbol_stats(&p.id, &symbols).unwrap();
    }

    // ── 列表 ─────────────────────────────────────────────────────

    #[test]
    fn list_returns_items_with_total() {
        let c = ctx();
        seed(&c);
        let page = list(&c, &ProjectListQuery::default()).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.total, 2);
        assert_eq!(page.limit, 24, "默认页大小");
    }

    /// 🔴 空串筛选必须当作"无筛选"，否则会生成 `WHERE language=''` 返回空列表。
    #[test]
    fn empty_string_filters_are_ignored() {
        let c = ctx();
        seed(&c);
        let page = list(
            &c,
            &ProjectListQuery {
                language: Some(String::new()),
                keyword: Some("   ".into()),
                status: Some("".into()),
                sort: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 2, "空串不该过滤掉任何结果");
    }

    #[test]
    fn list_filters_by_language_and_status() {
        let c = ctx();
        seed(&c);
        let mut rust = project("p3", "Rust 项目");
        rust.language = "Rust".into();
        rust.status = ProjectStatus::Abandoned;
        c.db.projects().upsert(&rust).unwrap();

        let by_lang = list(
            &c,
            &ProjectListQuery {
                language: Some("Rust".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(by_lang.items.len(), 1);
        assert_eq!(by_lang.items[0].id, "p3");
        assert_eq!(by_lang.total, 1, "total 应是过滤后的数量");

        let by_status = list(
            &c,
            &ProjectListQuery {
                status: Some("abandoned".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(by_status.items.len(), 1);
        assert_eq!(by_status.items[0].id, "p3");
    }

    #[test]
    fn list_supports_keyword_search() {
        let c = ctx();
        seed(&c);
        let page = list(
            &c,
            &ProjectListQuery {
                keyword: Some("视频".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].name, "视频管线");
    }

    #[test]
    fn list_sorts_by_requested_field() {
        let c = ctx();
        seed(&c);
        let mut big = project("p3", "大项目");
        big.stats.loc = 999_999;
        c.db.projects().upsert(&big).unwrap();

        let by_size = list(
            &c,
            &ProjectListQuery {
                sort: Some("size".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(by_size.items[0].id, "p3", "按代码量排序时最大的在前");

        let by_name = list(
            &c,
            &ProjectListQuery {
                sort: Some("name".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let names: Vec<&str> = by_name.items.iter().map(|i| i.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "按名称排序应升序");
    }

    #[test]
    fn unknown_sort_is_rejected_with_options() {
        let c = ctx();
        seed(&c);
        let err = list(
            &c,
            &ProjectListQuery {
                sort: Some("bogus".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, ServiceError::Invalid(_)));
        assert!(err.to_string().contains("recent"), "应列出可选项: {err}");
    }

    #[test]
    fn pagination_splits_results() {
        let c = ctx();
        // 逐个走 seed_project：upsert_batch 不写 scan/symbol 列，
        // 分页本身虽不依赖它们，但保持与其它测试一致的写入路径，
        // 避免"某个测试恰好靠 upsert 就能过"的假象
        for i in 0..10 {
            seed_project(&c, &project(&format!("p{i}"), &format!("项目{i}")));
        }

        let page1 = list(&c, &ProjectListQuery { limit: 3, offset: 0, ..Default::default() }).unwrap();
        let page2 = list(&c, &ProjectListQuery { limit: 3, offset: 3, ..Default::default() }).unwrap();
        assert_eq!(page1.items.len(), 3);
        assert_eq!(page2.items.len(), 3);
        assert_eq!(page1.total, 10, "total 不随分页变化");
        assert_eq!(page2.total, 10);
        let ids1: Vec<&str> = page1.items.iter().map(|i| i.id.as_str()).collect();
        let ids2: Vec<&str> = page2.items.iter().map(|i| i.id.as_str()).collect();
        assert!(ids1.iter().all(|id| !ids2.contains(id)), "分页不得重叠");
    }

    #[test]
    fn oversized_limit_is_clamped() {
        let c = ctx();
        seed(&c);
        let page = list(
            &c,
            &ProjectListQuery {
                limit: 99999,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.limit, MAX_PAGE_SIZE);
        assert_eq!(MAX_PAGE_SIZE, 200);
    }

    /// 🔴 回归守护：`limit = 0` 必须当作"未指定"，回落到默认页大小。
    ///
    /// 早期实现是 `q.limit.clamp(1, 200)`，把 0 钳成 1——
    /// 前端清空分页控件时传 0，结果只返回 1 条，用户以为数据丢了。
    /// 而 HTTP 路径因为 serde 的 `default = "default_page_size"` 不会触发，
    /// 所以这个 bug 只在 Rust 调用方（Tauri IPC）出现，极难发现。
    #[test]
    fn zero_limit_means_default_not_one() {
        let c = ctx();
        let batch: Vec<Project> = (0..10)
            .map(|i| project(&format!("p{i}"), &format!("项目{i}")))
            .collect();
        c.db.projects().upsert_batch(&batch).unwrap();

        let page = list(
            &c,
            &ProjectListQuery {
                limit: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.limit, 24, "0 应回落到默认页大小");
        assert_eq!(
            page.items.len(),
            10,
            "必须返回全部项目，而不是 1 条（clamp 的老行为）"
        );
    }

    /// `Default::default()` 与 serde 反序列化必须给出**相同**的 limit。
    /// 两条构造路径不一致正是这个 bug 的根源，必须显式守护。
    #[test]
    fn default_and_deserialized_query_agree() {
        let from_default = ProjectListQuery::default();
        let from_json: ProjectListQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(
            from_default.limit, from_json.limit,
            "Default 与 serde 的 limit 必须一致，否则 HTTP 与 IPC 行为不同"
        );
        assert_eq!(from_default.effective_limit(), from_json.effective_limit());
        assert_eq!(from_default.offset, from_json.offset);
    }

    #[test]
    fn effective_limit_handles_edge_cases() {
        assert_eq!(
            ProjectListQuery {
                limit: 0,
                ..Default::default()
            }
            .effective_limit(),
            24
        );
        assert_eq!(
            ProjectListQuery {
                limit: 1,
                ..Default::default()
            }
            .effective_limit(),
            1
        );
        assert_eq!(
            ProjectListQuery {
                limit: 500,
                ..Default::default()
            }
            .effective_limit(),
            MAX_PAGE_SIZE
        );
    }

    /// 筛选面必须来自真实分布：不该出现"Rust (0)"这种点了是空的选项。
    #[test]
    fn facets_reflect_real_distribution() {
        let c = ctx();
        seed(&c);
        let page = list(&c, &ProjectListQuery::default()).unwrap();
        let py = page
            .facets
            .languages
            .iter()
            .find(|f| f.value == "Python")
            .expect("应有 Python");
        assert_eq!(py.count, 2);
        assert!(
            !page.facets.languages.iter().any(|f| f.value == "Rust"),
            "没有 Rust 项目就不该出现该选项"
        );
        // 状态则应列全（含计数 0），让用户知道有哪些状态可选
        assert_eq!(page.facets.statuses.len(), 4);
        let active = page.facets.statuses.iter().find(|s| s.value == "active").unwrap();
        assert_eq!(active.count, 2);
        assert_eq!(active.label, "活跃");
    }

    // ── 列表项字段真实性 ─────────────────────────────────────────

    #[test]
    fn list_item_carries_real_scan_facts() {
        let c = ctx();
        seed(&c);
        let item = &list(&c, &ProjectListQuery::default()).unwrap().items[0];
        assert!(item.has_git);
        assert_eq!(item.git_commits, 87);
        assert!(item.has_readme);
        assert!(item.has_tests);
        assert_eq!(item.health_score, 85);
        assert_eq!(item.loc, 8600);
        assert_eq!(item.primary_language_pct, 92);
        assert!(!item.has_profile, "尚未生成画像");
    }

    /// 时间未知时必须显示"未知"，不能显示"今天"。
    #[test]
    fn unknown_time_shows_unknown_not_today() {
        let c = ctx();
        let mut p = project("p1", "无时间项目");
        p.updated_at = None;
        p.last_commit_at = None;
        c.db.projects().upsert(&p).unwrap();

        let item = &list(&c, &ProjectListQuery::default()).unwrap().items[0];
        assert!(item.days_idle.is_none());
        assert_eq!(item.updated_display, "时间未知");
    }

    #[test]
    fn relative_time_is_human_readable() {
        let c = ctx();
        seed(&c);
        let item = &list(&c, &ProjectListQuery::default()).unwrap().items[0];
        assert!(!item.updated_display.is_empty());
        assert_ne!(item.updated_display, "时间未知");
    }

    // ── 详情：代码结构 ───────────────────────────────────────────

    #[test]
    fn detail_structure_uses_real_stats() {
        let c = ctx();
        seed(&c);
        let d = detail(&c, "p1").unwrap();
        assert_eq!(d.structure.files, 120);
        assert_eq!(d.structure.loc, 8600);
        assert_eq!(d.structure.loc_display, "8.6k 行");
        assert_eq!(d.structure.symbols, 72);
        assert_eq!(d.structure.modules, 13);
        assert_eq!(d.structure.languages.len(), 2);
        assert_eq!(d.structure.languages[0].name, "Python");
        assert_eq!(d.structure.languages[0].pct, 92);
        assert!(!d.structure.languages[0].color.is_empty(), "语言应有配色");
    }

    /// 🔴 未采集的信号必须如实为 false，不能编造"有许可证"。
    #[test]
    fn uncollected_signals_are_honestly_false() {
        let c = ctx();
        seed(&c);
        let d = detail(&c, "p1").unwrap();
        assert!(!d.structure.has_license, "许可证检测未实现，不得报 true");
        assert!(!d.structure.has_docker, "Docker 检测未实现，不得报 true");
    }

    #[test]
    fn format_loc_buckets() {
        assert_eq!(format_loc(0), "0 行");
        assert_eq!(format_loc(999), "999 行");
        assert_eq!(format_loc(1_000), "1.0k 行");
        assert_eq!(format_loc(8_600), "8.6k 行");
        assert_eq!(format_loc(1_500_000), "1.5M 行");
    }

    #[test]
    fn language_colors_are_stable_and_have_fallback() {
        assert_eq!(language_color("Python"), language_color("python"), "大小写不敏感");
        assert!(!language_color("Python").is_empty());
        // 未知语言回落到通用代码色，不 panic 也不返回空串
        assert!(!language_color("Brainfuck").is_empty());
        assert_eq!(language_color("Brainfuck"), language_color("Malbolge"));
    }

    // ── 详情：考古 ───────────────────────────────────────────────

    /// 🔴 无 Git 历史时整块隐藏，不显示编造的"共 37 次 Session"。
    #[test]
    fn archaeology_hidden_without_git() {
        let c = ctx();
        let mut p = project("p1", "无 git 项目");
        p.scan = ScanFacts {
            has_git: false,
            git_commits: 0,
            ..ScanFacts::default()
        };
        c.db.projects().upsert(&p).unwrap();

        let d = detail(&c, "p1").unwrap();
        assert!(d.archaeology.is_none(), "无 Git 时不该有考古区块");
    }

    /// 提交数为 0 也算没有可用历史（空仓库）。
    #[test]
    fn archaeology_hidden_for_empty_repo() {
        let c = ctx();
        let mut p = project("p1", "空仓库");
        p.scan = ScanFacts {
            has_git: true,
            git_commits: 0,
            ..ScanFacts::default()
        };
        c.db.projects().upsert(&p).unwrap();
        assert!(detail(&c, "p1").unwrap().archaeology.is_none());
    }

    #[test]
    fn archaeology_uses_real_git_data() {
        let c = ctx();
        seed(&c);
        let arch = detail(&c, "p1").unwrap().archaeology.expect("有 Git 应有考古");
        assert_eq!(arch.commits, 87);
        assert_eq!(arch.first_commit_at.as_deref(), Some("2024-03-01"));
        assert_eq!(arch.last_commit_at.as_deref(), Some("2026-09-20"));
        assert!(arch.narrative.contains("87"), "叙述应引用真实提交数");
        // 🔴 未接入会话历史时必须是 None，不能显示 0（会被读成"你从没用 AI 写过"）
        assert!(arch.sessions.is_none(), "sessions 未接入应为 None");
        // 完成度无法可靠估算 → 不给假值（原型里的 0.86 是编的）
        assert!(arch.completeness.is_none());
        // 拼装叙述必须标注来源
        assert_eq!(arch.narrative_source, NarrativeSource::Deterministic);
        assert!(arch.narrative.contains("拼装"), "应标注是数据拼装: {}", arch.narrative);
    }

    /// 有 AI 画像时叙述用模型的（已过证据校验），并标注来源为模型。
    #[test]
    fn archaeology_prefers_model_narrative_when_available() {
        let c = ctx();
        let mut p = project("p1", "视频管线");
        p.ai_profile = Some(ProjectAiProfile {
            summary: "s".into(),
            purpose: None,
            phase: Some("多镜头优化".into()),
            highlights: vec![],
            archaeology: Some(Archaeology {
                sessions: 0,
                commits: 87,
                completeness: None,
                phase: Some("优化阶段".into()),
                salvage: vec!["VideoPipeline".into()],
                narrative: "该项目从 2024-03 起持续迭代，核心是多镜头生成。".into(),
            }),
            generated_by: "local:qwen3:8b".into(),
            generated_at: "2026-09-29T10:00:00Z".into(),
        });
        // ai_profile 由 upsert 写入；scan 事实必须另外写（upsert 不碰这些列）
        seed_project(&c, &p);
        c.db.assets().upsert(&asset("a1", "p1", "VideoPipeline", 0.92)).unwrap();

        let arch = detail(&c, "p1").unwrap().archaeology.unwrap();
        assert_eq!(arch.narrative_source, NarrativeSource::Model);
        assert!(arch.narrative.contains("多镜头生成"));
        assert_eq!(arch.phase.as_deref(), Some("优化阶段"));
        assert_eq!(arch.salvage, vec!["VideoPipeline"]);
        // sessions 为 0 时应转为 None（0 会被误读）
        assert!(arch.sessions.is_none());
    }

    /// 无画像时 salvage 应回落到真实的高复用资产名。
    #[test]
    fn salvage_falls_back_to_real_assets() {
        let c = ctx();
        seed(&c);
        let arch = detail(&c, "p1").unwrap().archaeology.unwrap();
        assert!(
            arch.salvage.contains(&"VideoPipeline".to_string()),
            "应含真实资产名: {:?}",
            arch.salvage
        );
        // 按复用分降序，最多 3 个
        assert!(arch.salvage.len() <= 3);
        assert_eq!(arch.salvage[0], "VideoPipeline", "复用分最高的在前");
    }

    // ── 详情：能力覆盖 ───────────────────────────────────────────

    #[test]
    fn capabilities_come_from_real_relations() {
        let c = ctx();
        seed(&c);
        let domain = Capability::new("cap_domain_media", "多媒体", CapabilityLayer::Domain, None, 0.9).unwrap();
        let mut cap = Capability::new(
            "cap-video",
            "视频生成",
            CapabilityLayer::Capability,
            Some("cap_domain_media".into()),
            0.88,
        )
        .unwrap();
        cap.project_count = 1;
        c.db.capabilities().upsert_batch(&[domain, cap]).unwrap();
        c.db
            .relations()
            .upsert(&Relation::new(
                "r1",
                "p1",
                spolia_domain::EntityKind::Project,
                RelationType::Implements,
                "cap-video",
                spolia_domain::EntityKind::Capability,
                0.88,
            )
            .with_evidence(["pytorch".to_string(), "ffmpeg".to_string()]))
            .unwrap();

        let d = detail(&c, "p1").unwrap();
        assert_eq!(d.capabilities.len(), 1);
        let cap = &d.capabilities[0];
        assert_eq!(cap.name, "视频生成");
        assert_eq!(cap.domain.as_deref(), Some("多媒体"), "应带 Domain 名用于分组");
        assert!((cap.confidence - 0.88).abs() < 1e-9);
        // 🔴 证据必须是真实信号（依赖名），不是编的
        assert_eq!(cap.evidence, vec!["pytorch", "ffmpeg"]);
    }

    #[test]
    fn capabilities_sorted_by_confidence() {
        let c = ctx();
        seed(&c);
        let domain = Capability::new("cap_domain_ai", "AI", CapabilityLayer::Domain, None, 0.9).unwrap();
        let low = Capability::new("cap-low", "低置信", CapabilityLayer::Capability, Some("cap_domain_ai".into()), 0.6).unwrap();
        let high = Capability::new("cap-high", "高置信", CapabilityLayer::Capability, Some("cap_domain_ai".into()), 0.95).unwrap();
        c.db.capabilities().upsert_batch(&[domain, low, high]).unwrap();
        for (id, target, conf) in [("r1", "cap-low", 0.6), ("r2", "cap-high", 0.95)] {
            c.db
                .relations()
                .upsert(
                    &Relation::new(
                        id,
                        "p1",
                        spolia_domain::EntityKind::Project,
                        RelationType::Implements,
                        target,
                        spolia_domain::EntityKind::Capability,
                        conf,
                    ),
                )
                .unwrap();
        }
        let caps = detail(&c, "p1").unwrap().capabilities;
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].name, "高置信", "置信度高的应排前");
    }

    // ── 详情：相关项目 ───────────────────────────────────────────

    /// 🔴 相似度必须来自真实关系，不是原型里硬编码的 87%。
    #[test]
    fn similar_projects_use_real_confidence() {
        let c = ctx();
        seed(&c);
        c.db
            .relations()
            .upsert(&Relation::new(
                "r1",
                "p1",
                spolia_domain::EntityKind::Project,
                RelationType::SimilarTo,
                "p2",
                spolia_domain::EntityKind::Project,
                0.73,
            )
            .with_evidence(["共同能力: 视频生成".to_string()]))
            .unwrap();

        let d = detail(&c, "p1").unwrap();
        assert_eq!(d.similar.len(), 1);
        assert_eq!(d.similar[0].id, "p2");
        assert!((d.similar[0].similarity - 0.73).abs() < 1e-9);
        assert_eq!(d.similar[0].basis, vec!["共同能力: 视频生成"]);

        // 反向查询也应命中（关系是双向的）
        let d2 = detail(&c, "p2").unwrap();
        assert_eq!(d2.similar.len(), 1);
        assert_eq!(d2.similar[0].id, "p1");
    }

    /// 低于门槛的"相似"不展示——勉强的相似度会误导用户去复用不相关代码。
    #[test]
    fn low_similarity_relations_are_filtered() {
        let c = ctx();
        seed(&c);
        c.db
            .relations()
            .upsert(&Relation::new(
                "r1",
                "p1",
                spolia_domain::EntityKind::Project,
                RelationType::SimilarTo,
                "p2",
                spolia_domain::EntityKind::Project,
                0.2,
            )
            .with_evidence([]))
            .unwrap();
        assert!(detail(&c, "p1").unwrap().similar.is_empty());
        assert_eq!(MIN_SIMILARITY, 0.5);
    }

    // ── 详情：资产与洞察 ─────────────────────────────────────────

    #[test]
    fn detail_lists_project_assets_sorted_by_reuse() {
        let c = ctx();
        seed(&c);
        let d = detail(&c, "p1").unwrap();
        assert_eq!(d.assets.len(), 2);
        assert_eq!(d.assets[0].name, "VideoPipeline", "复用分高的在前");
        assert_eq!(d.assets[0].tier, "高价值");
        assert_eq!(d.assets[0].type_label, "组件");
        assert!(!d.assets[0].source_path.is_empty());
    }

    /// 无证据的资产不得出现在详情里（产品红线）。
    #[test]
    fn detail_excludes_assets_without_evidence() {
        let c = ctx();
        seed(&c);
        let mut no_ev = asset("a3", "p1", "NoEvidence", 0.99);
        no_ev.evidence = Evidence::default();
        // 门禁会拒绝写入，所以这里验证的是"即使分数很高也不会出现"
        let written = c.db.assets().upsert(&no_ev).unwrap();
        assert!(!written);
        let d = detail(&c, "p1").unwrap();
        assert!(d.assets.iter().all(|a| a.id != "a3"));
    }

    #[test]
    fn detail_lists_related_insights() {
        let c = ctx();
        seed(&c);
        c.db
            .insights()
            .upsert(&Insight {
                id: "i1".into(),
                insight_type: InsightType::ReusableComponent,
                title: "VideoPipeline 可复用".into(),
                description: "在 2 个项目中重复出现".into(),
                confidence: 0.9,
                evidence: vec![spolia_domain::EvidenceItem {
                    kind: spolia_domain::EvidenceKind::File,
                    label: "src/a1.py".into(),
                    target: Some("p1:src/a1.py".into()),
                }],
                tags: vec![],
                related_project_ids: vec!["p1".into()],
                related_asset_ids: vec!["a1".into()],
                created_at: "2026-09-29".into(),
                user_feedback: None,
            })
            .unwrap();

        let d = detail(&c, "p1").unwrap();
        assert_eq!(d.insights.len(), 1);
        assert_eq!(d.insights[0].title, "VideoPipeline 可复用");
        assert_eq!(d.insights[0].badge, "高价值");

        // p2 与该洞察无关，不该看到
        assert!(detail(&c, "p2").unwrap().insights.is_empty());
    }

    // ── 详情：画像 ───────────────────────────────────────────────

    #[test]
    fn detail_without_profile_reports_none() {
        let c = ctx();
        seed(&c);
        let d = detail(&c, "p1").unwrap();
        assert!(d.profile.is_none(), "未分析时不该有画像");
        assert!(!d.summary.has_profile);
    }

    /// 🔴 亮点的证据文件必须校验存在性：画像可能是几周前生成的，
    /// 文件已被重命名，不校验的话用户点击就报错。
    #[test]
    fn highlight_evidence_reports_file_existence() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("services");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("video_service.py"), "x=1").unwrap();

        let c = ctx();
        let mut p = project("p1", "视频管线");
        p.path = dir.path().to_string_lossy().to_string();
        p.ai_profile = Some(ProjectAiProfile {
            summary: "视频生成服务".into(),
            purpose: Some("批量生成".into()),
            phase: None,
            highlights: vec![
                ProjectHighlight {
                    title: "真实亮点".into(),
                    desc: "d".into(),
                    evidence_files: vec!["services/video_service.py".into()],
                },
                ProjectHighlight {
                    title: "过期亮点".into(),
                    desc: "d".into(),
                    evidence_files: vec!["services/deleted_module.py".into()],
                },
            ],
            archaeology: None,
            generated_by: "local:qwen3:8b".into(),
            generated_at: "2026-09-29T10:00:00Z".into(),
        });
        c.db.projects().upsert(&p).unwrap();

        let d = detail(&c, "p1").unwrap();
        let profile = d.profile.expect("应有画像");
        assert_eq!(profile.highlights.len(), 2);
        assert!(profile.highlights[0].evidence_files[0].exists, "真实文件应标记存在");
        assert!(
            !profile.highlights[1].evidence_files[0].exists,
            "已删除的文件必须标记为不存在，否则用户点击就报错"
        );
        assert!(profile.highlights[0].evidence_files[0]
            .absolute
            .contains("video_service.py"));
        assert_eq!(profile.generated_by, "local:qwen3:8b");
    }

    // ── 操作 ─────────────────────────────────────────────────────

    #[test]
    fn set_sensitive_updates_and_audits_downgrade() {
        let c = ctx();
        seed(&c);
        // 先标记敏感
        let item = set_sensitive(&c, "p1", &SensitiveRequest { sensitive: true }).unwrap();
        assert!(item.sensitive);
        assert!(c.db.settings().recent_audit(10).unwrap().is_empty(), "升级为敏感不需审计");

        // 取消敏感 = 安全降级，必须留痕
        let item = set_sensitive(&c, "p1", &SensitiveRequest { sensitive: false }).unwrap();
        assert!(!item.sensitive);
        let audit = c.db.settings().recent_audit(10).unwrap();
        assert_eq!(audit.len(), 1);
        assert!(audit[0].summary.contains("敏感标记"), "实际: {}", audit[0].summary);
    }

    #[test]
    fn set_sensitive_reports_unknown_project() {
        let c = ctx();
        let err = set_sensitive(&c, "ghost", &SensitiveRequest { sensitive: true }).unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn set_description_persists() {
        let c = ctx();
        seed(&c);
        let item = set_description(
            &c,
            "p1",
            &DescriptionRequest {
                description: "  用户修正后的描述  ".into(),
            },
        )
        .unwrap();
        assert_eq!(item.description, "用户修正后的描述", "应去除首尾空白");
        assert_eq!(detail(&c, "p1").unwrap().summary.description, "用户修正后的描述");
    }

    #[test]
    fn description_length_is_validated() {
        let c = ctx();
        seed(&c);
        let err = set_description(
            &c,
            "p1",
            &DescriptionRequest {
                description: "x".repeat(600),
            },
        )
        .unwrap_err();
        assert!(matches!(err, ServiceError::Invalid(_)));
        assert!(err.to_string().contains("500"), "应说明上限: {err}");
    }

    /// 🔴 删除项目只删数据库记录，绝不动用户磁盘文件。
    #[test]
    fn remove_deletes_record_but_keeps_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.py");
        std::fs::write(&file, "print(1)").unwrap();

        let c = ctx();
        let mut p = project("p1", "真实项目");
        p.path = dir.path().to_string_lossy().to_string();
        c.db.projects().upsert(&p).unwrap();
        c.db.assets().upsert(&asset("a1", "p1", "Comp", 0.9)).unwrap();

        remove(&c, "p1").unwrap();
        assert!(c.db.projects().get("p1").unwrap().is_none());
        assert!(file.exists(), "磁盘文件必须保留");
        assert!(dir.path().exists(), "项目目录必须保留");
        // 资产由外键级联删除
        assert_eq!(c.db.assets().count_all().unwrap(), 0);
        // 活动流要说明"文件未删除"，避免用户恐慌
        let acts = c.db.activities().recent(5).unwrap();
        assert!(
            acts.iter().any(|a| a.detail.contains("磁盘文件未删除")),
            "应明确告知文件未删: {acts:?}"
        );
    }

    #[test]
    fn remove_unknown_project_reports_not_found() {
        let c = ctx();
        let err = remove(&c, "ghost").unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
        assert_eq!(err.status_code(), 404);
    }

    /// 重新索引必须走任务队列（不同步阻塞请求）。
    #[tokio::test]
    async fn reindex_submits_job() {
        let c = ctx();
        seed(&c);
        let job_id = reindex(&c, "p1").await.unwrap();
        assert!(job_id.starts_with("index_code-"), "实际 {job_id}");
        let job = c.db.jobs().get(&job_id).unwrap().unwrap();
        assert_eq!(job.job_type, spolia_domain::JobType::IndexCode);
        // 载荷必须带上项目 id，否则任务不知道索引哪个项目
        assert_eq!(
            job.payload.as_ref().and_then(|p| p.get("project_id")).and_then(|v| v.as_str()),
            Some("p1")
        );
    }

    #[tokio::test]
    async fn reindex_unknown_project_reports_not_found() {
        let c = ctx();
        let err = reindex(&c, "ghost").await.unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn detail_unknown_project_reports_not_found() {
        let c = ctx();
        let err = detail(&c, "ghost").unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
        assert!(err.to_string().contains("ghost"));
    }

    // ── 健壮性 ───────────────────────────────────────────────────

    #[test]
    fn empty_database_yields_empty_pages() {
        let c = ctx();
        let page = list(&c, &ProjectListQuery::default()).unwrap();
        assert!(page.items.is_empty());
        assert_eq!(page.total, 0);
        assert!(page.facets.languages.is_empty());
        // 状态面仍列出全部选项（计数为 0）
        assert_eq!(page.facets.statuses.len(), 4);
    }

    #[test]
    fn narrative_source_labels_are_stable() {
        assert_eq!(NarrativeSource::Model.as_str(), "model");
        assert_eq!(NarrativeSource::Model.label_zh(), "模型生成");
        assert_eq!(NarrativeSource::Deterministic.as_str(), "deterministic");
        assert_eq!(NarrativeSource::Deterministic.label_zh(), "数据拼装");
    }

    #[test]
    fn detail_limits_are_sane() {
        // 确切值断言已隐含"大于 0"，再写关系式对字面量常量恒真（clippy 会报 constant value）
        assert_eq!(DETAIL_ASSETS_LIMIT, 8);
        assert_eq!(DETAIL_INSIGHTS_LIMIT, 5);
        assert!((MIN_SIMILARITY - 0.5).abs() < f64::EPSILON);
    }
}

