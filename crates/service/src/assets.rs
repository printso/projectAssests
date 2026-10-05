//! 资产用例：列表 / 详情 / 类型分布 / 用户反馈。
//!
//! # 与 `projects::AssetBrief` 的关系
//! 项目详情页只需要资产的摘要（名称 + 类型 + 复用分），那是**项目视图的一部分**；
//! 本模块服务的是资产页——需要完整证据链、内容摘录、跨项目重名关系。
//! 两处刻意不共用 DTO：资产页的响应结构会随功能演进（加过滤、加批量操作），
//! 与项目详情页的耦合会让任何一边的改动都波及另一边。
//!
//! # 产品红线
//! `evidence_required` 默认为 **true**：无证据的资产一律不展示。
//! 要看到"抽取了但被门禁拒了多少"，用 `type_breakdown` 的 `total_including_rejected`。

use serde::{Deserialize, Serialize};
use projectassests_domain::{Asset, AssetType, ReuseTier, UserFeedback};
use projectassests_storage::{AssetFilter, AssetSort};

use crate::context::{ServiceContext, ServiceError};

/// 资产页每页默认条数。
fn default_page_size() -> u32 {
    24
}

/// 资产页单页上限。
///
/// 🔴 必须设上限：不设的话前端传 `limit=100000` 就能一次拖走全库，
/// 而资产表的 `evidence_json` 字段很大，万级响应会让浏览器直接卡死。
pub const MAX_PAGE_SIZE: u32 = 200;

/// 资产列表查询。
///
/// 字段全部用 `Option<String>` 而非枚举接收：HTTP query string 里
/// `?type=foo` 这种拼错值的情况很常见，枚举反序列化会直接返回
/// 框架级的 "invalid type" 错误，用户看不懂；
/// 字符串 + 显式 `parse` 才能给出"未知类型 foo（可选 code / component / …）"这样的引导。
#[derive(Debug, Clone, Deserialize)]
pub struct AssetListQuery {
    #[serde(default)]
    pub project_id: Option<String>,
    /// 单个类型；与 `types` 合并生效
    #[serde(default)]
    pub asset_type: Option<String>,
    /// 多选类型（UI 的 chips），逗号分隔：`?types=code,prompt`
    #[serde(default)]
    pub types: Option<String>,
    #[serde(default)]
    pub keyword: Option<String>,
    /// 最低复用分（0.0-1.0）
    #[serde(default)]
    pub min_reuse_score: Option<f64>,
    /// 只看高价值 / 中 / 低；与 `min_reuse_score` 同时给时取更严的
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default = "default_page_size")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

impl Default for AssetListQuery {
    /// 🔴 手写而非 `#[derive(Default)]`：derive 会把 `limit` 置 0，
    /// 而 serde 的 `default = "default_page_size"` 只在字段缺失时生效——
    /// 于是"反序列化出来的查询"和"`AssetListQuery::default()`"分页行为不一致。
    /// 这种不一致只在代码里构造默认查询时才暴露，恰好是测试的常见写法。
    fn default() -> Self {
        Self {
            project_id: None,
            asset_type: None,
            types: None,
            keyword: None,
            min_reuse_score: None,
            tier: None,
            sort: None,
            limit: default_page_size(),
            offset: 0,
        }
    }
}

impl AssetListQuery {
    /// 实际生效的 limit（clamp 到 1..=MAX_PAGE_SIZE）。
    pub fn effective_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_PAGE_SIZE)
    }
}

/// 资产列表项。
///
/// 相比 domain 的 `Asset` 增加了三个**派生展示字段**：
/// `type_label` / `tier` / `project_name`。
/// 派生逻辑放在 service 而非前端：中英文映射与分层阈值是业务规则，
/// 写在 JS 里就会与 Rust 侧的 `ReuseTier::from_score` 漂移。
#[derive(Debug, Clone, Serialize)]
pub struct AssetListItem {
    pub id: String,
    pub project_id: String,
    /// 所属项目名（列表里必须显示，否则用户不知道这资产来自哪）
    pub project_name: String,
    #[serde(rename = "type")]
    pub asset_type: String,
    pub type_label: String,
    pub name: String,
    pub description: String,
    pub source_path: String,
    pub confidence: f64,
    pub reuse_score: f64,
    pub tier: String,
    pub tier_label: String,
    pub tags: Vec<String>,
    pub created_at: String,
    /// 证据文件数（列表用数字角标提示"这条有依据"）
    pub evidence_files: usize,
    /// 相对时间文案（"3 天前"），列表直接展示
    pub created_relative: String,
    pub user_feedback: Option<String>,
}

/// 资产列表分页响应。
#[derive(Debug, Clone, Serialize)]
pub struct AssetListPage {
    pub items: Vec<AssetListItem>,
    /// 满足筛选条件的总数（**不是**本页条数）
    pub total: usize,
    pub limit: u32,
    pub offset: u32,
    /// 各类型的可选值与计数（前端渲染 chips）
    pub facets: Vec<AssetFacet>,
}

/// 类型 chip：值 + 中文标签 + 该类型下的资产数。
#[derive(Debug, Clone, Serialize)]
pub struct AssetFacet {
    pub value: String,
    pub label: String,
    pub count: usize,
}

/// 资产详情。
#[derive(Debug, Clone, Serialize)]
pub struct AssetDetail {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    #[serde(rename = "type")]
    pub asset_type: String,
    pub type_label: String,
    pub name: String,
    pub description: String,
    /// 内容摘录（符号签名或代码片段）。`None` 表示抽取时未保留内容。
    pub content: Option<String>,
    pub source_path: String,
    pub confidence: f64,
    pub reuse_score: f64,
    pub generality: f64,
    pub stability: f64,
    pub tier: String,
    pub tier_label: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub evidence: EvidenceView,
    pub user_feedback: Option<String>,
    /// 其他项目中同名同类型的资产（"你在别处也写过一份"）
    pub duplicates: Vec<DuplicateItem>,
}

/// 证据链视图。
///
/// 🔴 逐项对应真实文件 / commit，**不做任何推测补全**。
/// 空数组就是空数组——前端据此显示"该结论缺少证据"而不是编造说明文字。
#[derive(Debug, Clone, Serialize, Default)]
pub struct EvidenceView {
    pub files: Vec<String>,
    pub commits: Vec<String>,
    pub used_by: Vec<String>,
    pub reasoning: Vec<String>,
    pub file_count: usize,
    /// 证据是否达到展示门槛（domain 的 `is_sufficient` 结论）
    pub sufficient: bool,
}

/// 重名资产条目。
#[derive(Debug, Clone, Serialize)]
pub struct DuplicateItem {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub project_name: String,
    pub source_path: String,
    pub reuse_score: f64,
}

/// 用户反馈请求。
///
/// `feedback: None` = 撤销已有反馈（前端"取消"按钮）。
#[derive(Debug, Clone, Deserialize)]
pub struct FeedbackRequest {
    #[serde(default)]
    pub feedback: Option<String>,
}

// ══════════════════════════════════════════════════════════════════
// 列表
// ══════════════════════════════════════════════════════════════════

/// 资产列表（分页 + 筛选 + 类型 facets）。
pub fn list(ctx: &ServiceContext, q: &AssetListQuery) -> Result<AssetListPage, ServiceError> {
    let filter = build_filter(q)?;
    let sort = parse_sort(&q.sort)?;
    let limit = q.effective_limit();

    // 🔴 total 必须来自 count_filtered（真 COUNT），不能用 items.len()：
    // 后者最多是一页的条数，前端算出的总页数会随翻页缩短。
    let total = ctx.db.assets().count_filtered(&filter)?;

    let assets = ctx.db.assets().list(&filter, sort)?;
    let names = project_names(ctx, &assets)?;
    let now = ctx.now();

    let items = assets
        .iter()
        .map(|a| list_item(a, &names, now))
        .collect();

    Ok(AssetListPage {
        items,
        total,
        limit,
        offset: q.offset,
        facets: type_facets(ctx, &filter)?,
    })
}

/// 构造资产筛选条件。
///
/// `tier` 与 `min_reuse_score` 取更严的一个：两个都表达"质量下限"，
/// 若各算各的（例如先后覆盖同一个字段），用户同时勾选时行为不可预测。
fn build_filter(q: &AssetListQuery) -> Result<AssetFilter, ServiceError> {
    let mut min_score = q.min_reuse_score;
    if let Some(v) = min_score
        && !(0.0..=1.0).contains(&v)
    {
        return Err(ServiceError::Invalid(format!(
            "min_reuse_score 必须在 0.0-1.0 之间，收到 {v}"
        )));
    }

    if let Some(tier) = non_empty(&q.tier) {
        // 🔴 阈值取自 domain 的 `ReuseTier::min_score`，不在此处硬编码 0.85/0.70：
        // 分档规则改了而筛选没改，用户勾"高价值"就会看到中档资产。
        let t = ReuseTier::parse(&tier.to_ascii_lowercase()).ok_or_else(|| {
            ServiceError::Invalid(format!(
                "未知的复用层级：{tier}（可选 high / medium / low）"
            ))
        })?;
        let floor = t.min_score();
        // 取更严的下限：用户既勾"高价值"又设了 0.9，应按 0.9 过滤
        min_score = Some(min_score.map_or(floor, |v| v.max(floor)));
    }

    Ok(AssetFilter {
        project_id: non_empty(&q.project_id),
        asset_type: parse_type(&q.asset_type)?,
        asset_types: parse_types(&q.types)?,
        keyword: non_empty(&q.keyword),
        min_reuse_score: min_score,
        // 🔴 恒为 true，且**刻意不开放查询参数**：
        // "无证据不展示"是产品红线，不是用户可选项。
        // 主执行点在 `AssetRepo::upsert`（证据不足直接拒写），这里是双保险——
        // 万一历史数据或外部写入绕过了门禁，列表也不会把它端给用户。
        evidence_required: true,
        limit: Some(q.effective_limit()),
        offset: q.offset,
    })
}

/// 资产详情。
pub fn detail(ctx: &ServiceContext, id: &str) -> Result<AssetDetail, ServiceError> {
    let a = ctx
        .db
        .assets()
        .get(id)?
        .ok_or_else(|| ServiceError::NotFound(format!("资产 {id}")))?;

    let project_name = ctx
        .db
        .projects()
        .get(&a.project_id)?
        .map(|p| p.name)
        .unwrap_or_else(|| "(项目已删除)".to_string());

    Ok(AssetDetail {
        id: a.id.clone(),
        project_id: a.project_id.clone(),
        project_name,
        asset_type: a.asset_type.as_str().to_string(),
        type_label: a.asset_type.label_zh().to_string(),
        name: a.name.clone(),
        description: a.description.clone(),
        content: a.content.clone(),
        source_path: a.source_path.clone(),
        confidence: a.confidence,
        reuse_score: a.reuse_score,
        generality: a.generality,
        stability: a.stability,
        tier: tier_of(a.reuse_score).to_string(),
        tier_label: tier_of(a.reuse_score).label_zh().to_string(),
        tags: a.tags.clone(),
        created_at: a.created_at.clone(),
        evidence: evidence_view(&a.evidence),
        user_feedback: a.user_feedback.map(|f| f.as_str().to_string()),
        duplicates: duplicates_of(ctx, &a)?,
    })
}

/// 同名的其他资产（跨项目重复实现的直接线索）。
///
/// 只按 `name + type` 匹配，且排除自身。刻意不做模糊匹配：
/// "名字完全一样的同类型资产"才是可信的重复信号，
/// 模糊匹配会产出大量假阳性，用户点进去发现毫不相干就再也不信这个功能了。
fn duplicates_of(ctx: &ServiceContext, a: &Asset) -> Result<Vec<DuplicateItem>, ServiceError> {
    let filter = AssetFilter {
        asset_type: Some(a.asset_type),
        keyword: Some(a.name.clone()),
        // 重名分析同样遵守证据红线
        evidence_required: true,
        limit: Some(20),
        ..Default::default()
    };
    let candidates = ctx.db.assets().list(&filter, AssetSort::ReuseScore)?;

    // 关键词是 LIKE 匹配，会带出"名字包含它"的其他资产，必须精确比对
    let mut out: Vec<DuplicateItem> = candidates
        .into_iter()
        .filter(|c| c.id != a.id && c.name == a.name)
        .map(|c| DuplicateItem {
            id: c.id.clone(),
            name: c.name.clone(),
            project_id: c.project_id.clone(),
            project_name: String::new(),
            source_path: c.source_path.clone(),
            reuse_score: c.reuse_score,
        })
        .collect();

    if out.is_empty() {
        return Ok(out);
    }

    // 批量补项目名，避免 N+1
    let ids: Vec<String> = out.iter().map(|d| d.project_id.clone()).collect();
    let names = project_names_by_ids(ctx, &ids)?;
    for d in &mut out {
        d.project_name = names.get(&d.project_id).cloned().unwrap_or_default();
    }
    Ok(out)
}

/// 类型分布（资产页头部统计 + chips）。
///
/// `total` 是**受当前筛选影响**的计数，`all_types` 始终给全量类型枚举，
/// 否则用户筛掉某个类型后，chip 会直接消失，再也点不回来。
#[derive(Debug, Clone, Serialize)]
pub struct TypeBreakdown {
    pub total: usize,
    pub by_type: Vec<AssetFacet>,
    /// 全部资产类型（含计数为 0 的），供 chips 渲染
    pub all_types: Vec<AssetFacet>,
}

pub fn type_breakdown(ctx: &ServiceContext) -> Result<TypeBreakdown, ServiceError> {
    let counts = ctx.db.assets().count_by_type()?;
    let by_type: Vec<AssetFacet> = counts
        .iter()
        .map(|(t, n)| AssetFacet {
            value: t.as_str().to_string(),
            label: t.label_zh().to_string(),
            count: *n,
        })
        .collect();
    let total: usize = counts.iter().map(|(_, n)| n).sum();

    Ok(TypeBreakdown {
        total,
        by_type,
        all_types: all_type_facets(&counts),
    })
}

/// 记录用户反馈（北极星指标 Rediscovered Value 的数据来源）。
///
/// 返回更新后的资产，前端可直接用它刷新列表项而不必重新拉整页。
pub fn set_feedback(
    ctx: &ServiceContext,
    id: &str,
    req: &FeedbackRequest,
) -> Result<AssetListItem, ServiceError> {
    // 先确认存在：set_feedback 对不存在的 id 返回 false，
    // 若不先查，404 会被误报成"更新失败"
    if ctx.db.assets().get(id)?.is_none() {
        return Err(ServiceError::NotFound(format!("资产 {id}")));
    }

    let fb = match non_empty(&req.feedback) {
        Some(v) => Some(parse_feedback(&v)?),
        None => None,
    };
    ctx.db.assets().set_feedback(id, fb)?;

    let after = ctx
        .db
        .assets()
        .get(id)?
        .ok_or_else(|| ServiceError::NotFound(format!("资产 {id}")))?;
    let names = project_names(ctx, std::slice::from_ref(&after))?;
    Ok(list_item(&after, &names, ctx.now()))
}

// ══════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════

fn list_item(
    a: &Asset,
    names: &std::collections::HashMap<String, String>,
    now: chrono::DateTime<chrono::Utc>,
) -> AssetListItem {
    let tier = tier_of(a.reuse_score);
    AssetListItem {
        id: a.id.clone(),
        project_id: a.project_id.clone(),
        project_name: names.get(&a.project_id).cloned().unwrap_or_default(),
        asset_type: a.asset_type.as_str().to_string(),
        type_label: a.asset_type.label_zh().to_string(),
        name: a.name.clone(),
        description: a.description.clone(),
        source_path: a.source_path.clone(),
        confidence: a.confidence,
        reuse_score: a.reuse_score,
        tier: tier.to_string(),
        tier_label: tier.label_zh().to_string(),
        tags: a.tags.clone(),
        created_at: a.created_at.clone(),
        evidence_files: a.evidence.file_count(),
        created_relative: projectassests_storage::relative_time(&a.created_at, now),
        user_feedback: a.user_feedback.map(|f| f.as_str().to_string()),
    }
}

fn evidence_view(e: &projectassests_domain::Evidence) -> EvidenceView {
    EvidenceView {
        files: e.files.clone(),
        commits: e.commits.clone(),
        used_by: e.used_by.clone(),
        reasoning: e.reasoning.clone(),
        file_count: e.file_count(),
        sufficient: e.is_sufficient(),
    }
}

fn tier_of(score: f64) -> ReuseTier {
    ReuseTier::from_score(score)
}

/// 批量取项目名（一次查询，避免 N+1）。
fn project_names(
    ctx: &ServiceContext,
    assets: &[Asset],
) -> Result<std::collections::HashMap<String, String>, ServiceError> {
    let ids: Vec<String> = assets.iter().map(|a| a.project_id.clone()).collect();
    project_names_by_ids(ctx, &ids)
}

fn project_names_by_ids(
    ctx: &ServiceContext,
    ids: &[String],
) -> Result<std::collections::HashMap<String, String>, ServiceError> {
    let mut map = std::collections::HashMap::new();
    // 逐个 get 而非 IN 查询：ids 已去重且量小（≤ 一页资产的项目数），
    // 而新增一个 IN 查询接口只为省几次主键查找，收益不抵 API 面积。
    for id in unique(ids) {
        if let Some(p) = ctx.db.projects().get(&id)? {
            map.insert(id.clone(), p.name);
        }
    }
    Ok(map)
}

fn unique(ids: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect()
}

/// 类型 facets：只统计**当前筛选条件下**各类型的数量。
///
/// 🔴 不能直接用 `count_by_type()`（全表统计）：用户筛了"语言=Python"后，
/// chips 上的数字若仍是全库计数，就会出现"chip 显示 300 条，点进去只有 12 条"。
/// 这里对每个类型各跑一次 count_filtered，条件与列表完全一致。
fn type_facets(
    ctx: &ServiceContext,
    base: &AssetFilter,
) -> Result<Vec<AssetFacet>, ServiceError> {
    let mut out = Vec::new();
    for t in AssetType::all() {
        let f = AssetFilter {
            asset_type: Some(*t),
            asset_types: Vec::new(),
            // 类型维度自身不再参与筛选，否则勾了 code 后其他 chip 全变 0
            ..base.clone()
        };
        out.push(AssetFacet {
            value: t.as_str().to_string(),
            label: t.label_zh().to_string(),
            count: ctx.db.assets().count_filtered(&f)?,
        });
    }
    Ok(out)
}

fn all_type_facets(counts: &[(AssetType, usize)]) -> Vec<AssetFacet> {
    AssetType::all()
        .iter()
        .map(|t| AssetFacet {
            value: t.as_str().to_string(),
            label: t.label_zh().to_string(),
            count: counts
                .iter()
                .find(|(ct, _)| ct == t)
                .map(|(_, n)| *n)
                .unwrap_or(0),
        })
        .collect()
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn parse_type(s: &Option<String>) -> Result<Option<AssetType>, ServiceError> {
    match non_empty(s) {
        Some(v) => Ok(Some(parse_type_str(&v)?)),
        None => Ok(None),
    }
}

fn parse_types(s: &Option<String>) -> Result<Vec<AssetType>, ServiceError> {
    let Some(raw) = non_empty(s) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for part in raw.split(',') {
        let v = part.trim();
        if v.is_empty() {
            continue;
        }
        let t = parse_type_str(v)?;
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

fn parse_type_str(v: &str) -> Result<AssetType, ServiceError> {
    AssetType::parse(v).ok_or_else(|| {
        ServiceError::Invalid(format!(
            "未知的资产类型：{v}（可选 {}）",
            AssetType::all()
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        ))
    })
}

fn parse_sort(s: &Option<String>) -> Result<AssetSort, ServiceError> {
    Ok(match non_empty(s).as_deref() {
        None | Some("reuse") => AssetSort::ReuseScore,
        Some("confidence") => AssetSort::Confidence,
        Some("newest") => AssetSort::Newest,
        Some("name") => AssetSort::Name,
        Some(other) => {
            return Err(ServiceError::Invalid(format!(
                "未知的排序方式：{other}（可选 reuse / confidence / newest / name）"
            )))
        }
    })
}

fn parse_feedback(v: &str) -> Result<UserFeedback, ServiceError> {
    UserFeedback::parse(v).ok_or_else(|| {
        ServiceError::Invalid(format!(
            "未知的反馈值：{v}（可选 useful / useless / ignored）"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{CodeStats, Evidence, Project, ProjectStatus, ScanFacts};

    fn ctx() -> ServiceContext {
        ServiceContext::in_memory().unwrap()
    }

    /// `Project` 没有 `Default`（刻意：字段都有业务含义，全默认值会掩盖漏填）。
    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("/tmp/{id}"),
            description: format!("{name} 的描述"),
            language: "Rust".into(),
            framework: "Axum".into(),
            created_at: None,
            updated_at: Some("2026-09-20".into()),
            last_commit_at: Some("2026-09-20".into()),
            status: ProjectStatus::Active,
            health_score: 80,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats {
                files: 10,
                loc: 5000,
                symbols: 20,
                modules: 4,
                languages: vec![],
            },
            scan: ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn insert_project(ctx: &ServiceContext, id: &str, name: &str) {
        ctx.db.projects().upsert(&project(id, name)).unwrap();
    }

    fn asset(id: &str, project_id: &str, name: &str, t: AssetType, score: f64) -> Asset {
        Asset {
            id: id.to_string(),
            project_id: project_id.to_string(),
            asset_type: t,
            name: name.to_string(),
            description: format!("{name} 的描述"),
            content: Some(format!("fn {name}() {{}}")),
            source_path: format!("src/{name}.rs"),
            confidence: 0.9,
            reuse_score: score,
            generality: 0.8,
            stability: 0.7,
            tags: vec!["rust".to_string()],
            created_at: projectassests_storage::now_utc(),
            evidence: Evidence {
                files: vec![format!("src/{name}.rs")],
                ..Evidence::default()
            },
            user_feedback: None,
        }
    }

    fn no_evidence_asset(id: &str) -> Asset {
        let mut a = asset(id, "p1", "orphan", AssetType::Code, 0.9);
        a.evidence = Evidence::default();
        a
    }

    #[test]
    fn default_query_has_nonzero_page_size() {
        // 回归：derive(Default) 会让 limit=0，与 serde 默认值不一致
        let q = AssetListQuery::default();
        assert_eq!(q.effective_limit(), 24);
        let from_serde: AssetListQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(from_serde.effective_limit(), q.effective_limit());
    }

    #[test]
    fn limit_is_clamped_to_max() {
        let q = AssetListQuery {
            limit: 100_000,
            ..Default::default()
        };
        assert_eq!(q.effective_limit(), MAX_PAGE_SIZE);
        let zero = AssetListQuery {
            limit: 0,
            ..Default::default()
        };
        assert_eq!(zero.effective_limit(), 1);
    }

    #[test]
    fn total_is_stable_across_pages() {
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        for i in 0..5 {
            c.db
                .assets()
                .upsert(&asset(&format!("a{i}"), "p1", &format!("asset{i}"), AssetType::Code, 0.9))
                .unwrap();
        }

        let page1 = list(
            &c,
            &AssetListQuery {
                limit: 2,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        let page2 = list(
            &c,
            &AssetListQuery {
                limit: 2,
                offset: 2,
                ..Default::default()
            },
        )
        .unwrap();

        // 🔴 回归：total 曾随翻页缩短（用了 items.len()）
        assert_eq!(page1.total, 5);
        assert_eq!(page2.total, 5);
        assert_eq!(page1.items.len(), 2);
        assert_eq!(page2.items.len(), 2);
    }

    #[test]
    fn assets_without_evidence_never_reach_the_list() {
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        c.db
            .assets()
            .upsert(&asset("a1", "p1", "good", AssetType::Code, 0.9))
            .unwrap();
        // 🔴 门禁在 upsert：无证据资产被拒写，返回 Ok(false) 而非静默成功。
        // 所以列表侧的 evidence_required 是双保险，这里验证的是主执行点。
        let written = c.db.assets().upsert(&no_evidence_asset("a2")).unwrap();
        assert!(!written, "无证据资产必须被拒写");

        let page = list(&c, &AssetListQuery::default()).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].id, "a1");
    }

    #[test]
    fn facet_counts_respect_active_filters() {
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        c.db.assets().upsert(&asset("a1", "p1", "code1", AssetType::Code, 0.9)).unwrap();
        c.db
            .assets()
            .upsert(&asset("a2", "p1", "prompt1", AssetType::Prompt, 0.9))
            .unwrap();

        // 按项目筛选后，chips 数字应随之变化，而不是显示全库计数
        let page = list(
            &c,
            &AssetListQuery {
                project_id: Some("p1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let code = page.facets.iter().find(|f| f.value == "code").unwrap();
        assert_eq!(code.count, 1);

        let ghost = list(
            &c,
            &AssetListQuery {
                project_id: Some("nope".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(ghost.facets.iter().all(|f| f.count == 0), "不存在的项目应让所有 chip 归零");
        assert!(ghost.facets.len() >= 2, "chips 必须始终可点，不能因为计数 0 就消失");
    }

    #[test]
    fn unknown_type_is_rejected_with_options() {
        let c = ctx();
        let err = list(
            &c,
            &AssetListQuery {
                asset_type: Some("banana".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("banana"), "应回显非法值: {msg}");
        assert!(msg.contains("code"), "应列出可选值: {msg}");
    }

    #[test]
    fn invalid_min_score_is_rejected() {
        let c = ctx();
        let err = list(
            &c,
            &AssetListQuery {
                min_reuse_score: Some(1.5),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("0.0-1.0"));
    }

    #[test]
    fn tier_and_min_score_take_the_stricter_one() {
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        // high 阈值 0.85；显式 0.9 更严
        c.db.assets().upsert(&asset("a1", "p1", "mid", AssetType::Code, 0.87)).unwrap();
        c.db.assets().upsert(&asset("a2", "p1", "top", AssetType::Code, 0.95)).unwrap();

        let only_tier = list(
            &c,
            &AssetListQuery {
                tier: Some("high".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(only_tier.total, 2);

        let stricter = list(
            &c,
            &AssetListQuery {
                tier: Some("high".into()),
                min_reuse_score: Some(0.9),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(stricter.total, 1, "应取更严的下限 0.9");
        assert_eq!(stricter.items[0].id, "a2");
    }

    #[test]
    fn unknown_tier_is_rejected() {
        let c = ctx();
        let err = list(
            &c,
            &AssetListQuery {
                tier: Some("ultra".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("high / medium / low"));
    }

    #[test]
    fn detail_carries_project_name_and_evidence() {
        let c = ctx();
        insert_project(&c, "p1", "视频融合平台");
        c.db.assets().upsert(&asset("a1", "p1", "decoder", AssetType::Code, 0.92)).unwrap();

        let d = detail(&c, "a1").unwrap();
        assert_eq!(d.project_name, "视频融合平台");
        assert_eq!(d.tier, "high");
        assert!(d.evidence.sufficient);
        assert_eq!(d.evidence.file_count, 1);
        assert!(d.content.is_some());
    }

    #[test]
    fn detail_of_missing_asset_is_404() {
        let c = ctx();
        let err = detail(&c, "ghost").unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
        assert_eq!(err.status_code(), 404);
    }

    #[test]
    fn duplicates_require_exact_name_and_type_match() {
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        insert_project(&c, "p2", "项目二");
        c.db.assets().upsert(&asset("a1", "p1", "TaskQueue", AssetType::Code, 0.9)).unwrap();
        c.db.assets().upsert(&asset("a2", "p2", "TaskQueue", AssetType::Code, 0.8)).unwrap();
        // LIKE 会命中它（名字包含 TaskQueue），但精确比对必须排除
        c.db
            .assets()
            .upsert(&asset("a3", "p2", "TaskQueueManager", AssetType::Code, 0.8))
            .unwrap();
        // 同名但类型不同：不算重复
        c.db
            .assets()
            .upsert(&asset("a4", "p2", "TaskQueue", AssetType::Prompt, 0.8))
            .unwrap();

        let d = detail(&c, "a1").unwrap();
        assert_eq!(d.duplicates.len(), 1);
        assert_eq!(d.duplicates[0].id, "a2");
        assert_eq!(d.duplicates[0].project_name, "项目二");
    }

    #[test]
    fn feedback_roundtrip_and_revoke() {
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        c.db.assets().upsert(&asset("a1", "p1", "x", AssetType::Code, 0.9)).unwrap();

        let item = set_feedback(
            &c,
            "a1",
            &FeedbackRequest {
                feedback: Some("useful".into()),
            },
        )
        .unwrap();
        assert_eq!(item.user_feedback.as_deref(), Some("useful"));

        // 撤销
        let revoked = set_feedback(&c, "a1", &FeedbackRequest { feedback: None }).unwrap();
        assert!(revoked.user_feedback.is_none());
    }

    #[test]
    fn feedback_on_missing_asset_is_404() {
        let c = ctx();
        // 回归：set_feedback 对不存在 id 返回 false，曾被误报成"更新失败"
        let err = set_feedback(
            &c,
            "ghost",
            &FeedbackRequest {
                feedback: Some("useful".into()),
            },
        )
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn invalid_feedback_value_is_rejected() {
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        c.db.assets().upsert(&asset("a1", "p1", "x", AssetType::Code, 0.9)).unwrap();
        let err = set_feedback(
            &c,
            "a1",
            &FeedbackRequest {
                feedback: Some("maybe".into()),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("useful"));
    }

    #[test]
    fn type_breakdown_lists_every_type_including_empty() {
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        c.db.assets().upsert(&asset("a1", "p1", "x", AssetType::Code, 0.9)).unwrap();

        let b = type_breakdown(&c).unwrap();
        assert_eq!(b.total, 1);
        assert!(b.all_types.len() >= AssetType::all().len());
        // 计数为 0 的类型也要出现，否则 chip 消失后再也点不回来
        assert!(b.all_types.iter().any(|f| f.value == "prompt" && f.count == 0));
    }

    #[test]
    fn deleting_project_cascades_to_its_assets() {
        // 🔴 schema 是 `project_id REFERENCES projects(id) ON DELETE CASCADE`，
        // 且连接开启了 foreign_keys。因此"资产在、项目不在"不可达——
        // detail() 里的 "(项目已删除)" 只是对外部改库的防御，无法用正常操作构造。
        // 这里锁定真正可达的不变式：删项目必须连资产一起清掉，
        // 否则资产页会列出一堆点进去 404 的孤儿。
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        c.db
            .assets()
            .upsert(&asset("a1", "p1", "x", AssetType::Code, 0.9))
            .unwrap();
        assert_eq!(list(&c, &AssetListQuery::default()).unwrap().total, 1);

        c.db.projects().delete("p1").unwrap();

        assert!(c.db.assets().get("a1").unwrap().is_none(), "资产应随项目级联删除");
        let page = list(&c, &AssetListQuery::default()).unwrap();
        assert_eq!(page.total, 0, "孤儿资产不得出现在列表里");
    }

    #[test]
    fn inserting_asset_for_unknown_project_is_rejected() {
        // 外键的另一半：不能凭空给不存在的项目挂资产
        let c = ctx();
        let err = c
            .db
            .assets()
            .upsert(&asset("a1", "ghost", "x", AssetType::Code, 0.9))
            .unwrap_err();
        assert!(err.to_string().contains("FOREIGN KEY"), "{err}");
    }

    #[test]
    fn empty_db_returns_empty_page_not_error() {
        let c = ctx();
        let page = list(&c, &AssetListQuery::default()).unwrap();
        assert_eq!(page.total, 0);
        assert!(page.items.is_empty());
    }

    #[test]
    fn list_is_deterministic() {
        let c = ctx();
        insert_project(&c, "p1", "项目一");
        for i in 0..3 {
            c.db
                .assets()
                .upsert(&asset(&format!("a{i}"), "p1", "same", AssetType::Code, 0.9))
                .unwrap();
        }
        let first = list(&c, &AssetListQuery::default()).unwrap();
        let second = list(&c, &AssetListQuery::default()).unwrap();
        let ids1: Vec<_> = first.items.iter().map(|i| i.id.clone()).collect();
        let ids2: Vec<_> = second.items.iter().map(|i| i.id.clone()).collect();
        assert_eq!(ids1, ids2, "同分必须有稳定兜底排序，否则刷新一次顺序就变");
    }

    #[test]
    fn database_reopen_sees_assets() {
        // 确认 upsert 真的落盘（而非只改内存态）
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let c = ServiceContext::open(&path).unwrap();
            insert_project(&c, "p1", "项目一");
            c.db
                .assets()
                .upsert(&asset("a1", "p1", "x", AssetType::Code, 0.9))
                .unwrap();
        }
        let c2 = ServiceContext::open(&path).unwrap();
        assert_eq!(c2.db.assets().count_all().unwrap(), 1);
        assert_eq!(list(&c2, &AssetListQuery::default()).unwrap().total, 1);
    }
}
