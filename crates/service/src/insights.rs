//! 洞察与机会用例：列表 / 详情 / 反馈回流 / 机会处置 / 采纳率统计。
//!
//! # 为什么洞察和机会放在同一个模块
//! 二者是同一条价值链的上下游：洞察是**线索**（"你在 4 个项目里重复实现了任务队列"），
//! 机会是**成型建议**（"把它们抽成一个 crate，覆盖度 70%，4 星"）。
//! 用户的操作路径也是连续的：看到洞察 → 展开成机会 → Dismiss 或 Adopt。
//! 拆成两个模块会让"机会的来源洞察是哪条"这类关联查询跨模块跳转。
//!
//! # 反馈回流是产品指标，不是 UI 装饰
//! 《产品设计书》阶段二的关键指标是"标记有用 / 总展示 ≥ 40%"。
//! 这个比率只能从 `user_feedback` 列算出来，因此反馈写入必须可靠，
//! 且 `adoption_rate` 必须在**分母为 0 时返回 None 而非 0%**——
//! 显示"采纳率 0%"会让用户以为功能没用，而真相是还没人投过票。

use serde::{Deserialize, Serialize};
use projectassests_domain::{
    Insight, InsightType, Opportunity, OpportunityStatus, UserFeedback,
};
use projectassests_storage::{InsightFilter, OpportunityFilter};

use crate::context::{ServiceContext, ServiceError};

/// 洞察列表默认条数。
fn default_limit() -> u32 {
    20
}

/// 洞察列表上限。
pub const MAX_LIST_LIMIT: u32 = 200;

/// 洞察列表查询。
#[derive(Debug, Clone, Deserialize)]
pub struct InsightListQuery {
    /// 多个类型，逗号分隔（UI 的 chips）
    #[serde(default)]
    pub types: Option<String>,
    /// `true` = 只看未读（首页"新发现"）；`false` = 只看已读；缺省 = 全部
    #[serde(default)]
    pub unread_only: Option<bool>,
    /// 最低置信度（0.0-1.0）
    #[serde(default)]
    pub min_confidence: Option<f64>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

impl Default for InsightListQuery {
    /// 🔴 手写而非 derive：derive 的 `limit` 是 0，与 serde 默认值不一致，
    /// 于是代码里构造的默认查询和反序列化出的默认查询分页行为不同。
    fn default() -> Self {
        Self {
            types: None,
            unread_only: None,
            min_confidence: None,
            limit: default_limit(),
            offset: 0,
        }
    }
}

impl InsightListQuery {
    pub fn effective_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_LIST_LIMIT)
    }
}

/// 洞察列表项。
#[derive(Debug, Clone, Serialize)]
pub struct InsightItem {
    pub id: String,
    #[serde(rename = "type")]
    pub insight_type: String,
    pub type_label: String,
    pub title: String,
    pub description: String,
    pub confidence: f64,
    /// 0-100 整数（前端置信度条直接用）
    pub confidence_percent: u8,
    pub tags: Vec<String>,
    pub created_at: String,
    pub created_relative: String,
    /// 价值分档徽章（"高价值 / 高潜力 / 建议查看"）。
    ///
    /// 🔴 必须来自 domain 的 `Insight::badge()`，与首页、项目详情页**同源**。
    /// 早期版本在这里另写了一套"证据充分 / 待评估"的逻辑，
    /// 结果同一条洞察在首页显示"高价值"、在洞察页显示"证据充分"——
    /// 用户会认为是两条不同的洞察。徽章是价值判断，全站只能有一套口径。
    pub badge: String,
    pub badge_key: String,
    /// 处置状态（"已标记有用 / 已标记无用 / 已忽略 / 待处理"）。
    ///
    /// 与 `badge` 刻意分开：badge 是**系统对价值的判断**（由置信度算出，不随点击变化），
    /// state 是**用户对它的处置**（点一次就变）。压进同一个字段的话，
    /// 用户标记"无用"后价值信息就丢了，也无法再区分"未处理"和"处理过但价值低"。
    pub state: String,
    pub state_key: String,
    pub evidence_count: usize,
    pub related_project_ids: Vec<String>,
    pub related_asset_ids: Vec<String>,
    pub user_feedback: Option<String>,
}

/// 洞察列表分页响应。
#[derive(Debug, Clone, Serialize)]
pub struct InsightListPage {
    pub items: Vec<InsightItem>,
    pub total: usize,
    pub limit: u32,
    pub offset: u32,
    /// 未读数（首页角标）
    pub unread: usize,
    /// 各类型的 chip（含计数为 0 的，否则勾掉后点不回来）
    pub facets: Vec<TypeFacet>,
    pub adoption: AdoptionView,
}

/// 类型 chip。
#[derive(Debug, Clone, Serialize)]
pub struct TypeFacet {
    pub value: String,
    pub label: String,
    pub count: usize,
}

/// 采纳率视图（北极星指标之一）。
#[derive(Debug, Clone, Serialize)]
pub struct AdoptionView {
    pub useful: usize,
    pub rated: usize,
    /// 🔴 分母为 0 时必须是 `None`：显示 "0%" 会让用户误以为结论都不准，
    /// 而真相是还没人投过票。前端应据此显示"暂无反馈"。
    pub rate: Option<f64>,
    /// 面向用户的说明文案（前端可直接展示）
    pub label: String,
}

/// 洞察详情。
#[derive(Debug, Clone, Serialize)]
pub struct InsightDetail {
    #[serde(flatten)]
    pub item: InsightItem,
    /// 结构化证据。空数组就是空数组，前端据此显示"该结论缺少证据"。
    pub evidence: Vec<EvidenceView>,
    /// 关联项目的名称（批量取回，避免 N+1）
    pub related_projects: Vec<RelatedProject>,
    /// 关联资产的名称与路径
    pub related_assets: Vec<RelatedAsset>,
}

/// 证据条目视图。
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceView {
    pub kind: String,
    pub kind_label: String,
    pub label: String,
    /// 可跳转目标：项目 id 或 "project_id:相对路径"
    pub target: Option<String>,
}

/// 关联项目。
#[derive(Debug, Clone, Serialize)]
pub struct RelatedProject {
    pub id: String,
    pub name: String,
    pub language: String,
    pub status: String,
    pub status_label: String,
}

/// 关联资产。
#[derive(Debug, Clone, Serialize)]
pub struct RelatedAsset {
    pub id: String,
    pub name: String,
    pub asset_type: String,
    pub type_label: String,
    pub source_path: String,
    pub reuse_score: f64,
}

/// 反馈请求。`feedback: None` = 撤销。
#[derive(Debug, Clone, Deserialize)]
pub struct FeedbackRequest {
    #[serde(default)]
    pub feedback: Option<String>,
}

/// 机会列表查询。
#[derive(Debug, Clone, Deserialize)]
pub struct OpportunityListQuery {
    /// 多个状态，逗号分隔。缺省 = 只看可操作（new + explored）
    #[serde(default)]
    pub statuses: Option<String>,
    /// `true` = 含已忽略/已采纳（审计视图）
    #[serde(default)]
    pub include_closed: Option<bool>,
    /// 最低星级（1-5）
    #[serde(default)]
    pub min_rating: Option<u8>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

impl Default for OpportunityListQuery {
    fn default() -> Self {
        Self {
            statuses: None,
            include_closed: None,
            min_rating: None,
            limit: default_limit(),
            offset: 0,
        }
    }
}

impl OpportunityListQuery {
    pub fn effective_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_LIST_LIMIT)
    }
}

/// 机会卡片视图。
#[derive(Debug, Clone, Serialize)]
pub struct OpportunityItem {
    pub id: String,
    pub title: String,
    pub description: String,
    /// 为什么值得关注（真实依据，不是模板文案）
    pub why: String,
    pub rating: u8,
    /// 星级展示串（"★★★★☆"），前端不必自己拼
    pub rating_stars: String,
    /// 覆盖度 0-100 整数
    pub coverage_percent: u8,
    pub coverage: f64,
    pub required_capabilities: Vec<String>,
    pub missing_capabilities: Vec<String>,
    pub evidence: Vec<String>,
    pub status: String,
    pub status_label: String,
    pub created_at: String,
    pub created_relative: String,
    /// 来源项目（带名称，卡片直接展示）
    pub source_projects: Vec<RelatedProject>,
    /// 是否还能改状态（已采纳的机会不该再显示 Dismiss 按钮）
    pub actionable: bool,
    /// 是否已有深入分析
    pub has_analysis: bool,
}

/// 机会列表响应。
#[derive(Debug, Clone, Serialize)]
pub struct OpportunityListPage {
    pub items: Vec<OpportunityItem>,
    pub total: usize,
    pub limit: u32,
    pub offset: u32,
    /// 各状态计数（含 0，理由同 chips）
    pub facets: Vec<TypeFacet>,
    /// 可操作机会数（侧栏角标）
    pub actionable_count: usize,
    /// 空状态引导：Dismiss 全部后产品要求进入明确的空状态
    pub empty_hint: Option<String>,
}

/// 机会状态变更请求。
#[derive(Debug, Clone, Deserialize)]
pub struct StatusRequest {
    /// new / explored / dismissed / adopted
    pub status: String,
}

/// 机会详情（含深入分析）。
#[derive(Debug, Clone, Serialize)]
pub struct OpportunityDetail {
    #[serde(flatten)]
    pub item: OpportunityItem,
    /// 深入分析；未生成时为 `None`（前端显示"展开分析"按钮而非空白）
    pub analysis: Option<projectassests_domain::OpportunityAnalysis>,
}

/// 洞察与机会的汇总统计（洞察页头部）。
#[derive(Debug, Clone, Serialize)]
pub struct InsightsSummary {
    pub insight_total: usize,
    pub insight_unread: usize,
    pub opportunity_total: usize,
    pub opportunity_actionable: usize,
    pub adoption: AdoptionView,
    pub by_type: Vec<TypeFacet>,
}

// ══════════════════════════════════════════════════════════════════
// 洞察
// ══════════════════════════════════════════════════════════════════

/// 洞察列表。
pub fn list(ctx: &ServiceContext, q: &InsightListQuery) -> Result<InsightListPage, ServiceError> {
    let filter = build_filter(q)?;
    let total = ctx.db.insights().count_filtered(&filter)?;
    let insights = ctx.db.insights().list(&filter)?;
    let now = ctx.now();

    Ok(InsightListPage {
        items: insights.iter().map(|i| item(i, now)).collect(),
        total,
        limit: q.effective_limit(),
        offset: q.offset,
        unread: ctx.db.insights().count_unread()?,
        facets: type_facets(ctx, &filter)?,
        adoption: adoption(ctx)?,
    })
}

/// 洞察详情。
pub fn detail(ctx: &ServiceContext, id: &str) -> Result<InsightDetail, ServiceError> {
    let i = ctx
        .db
        .insights()
        .get(id)?
        .ok_or_else(|| ServiceError::NotFound(format!("洞察 {id}")))?;

    Ok(InsightDetail {
        item: item(&i, ctx.now()),
        evidence: i.evidence.iter().map(evidence_view).collect(),
        related_projects: related_projects(ctx, &i.related_project_ids)?,
        related_assets: related_assets(ctx, &i.related_asset_ids)?,
    })
}

/// 记录洞察反馈（反馈回流）。
pub fn set_feedback(
    ctx: &ServiceContext,
    id: &str,
    req: &FeedbackRequest,
) -> Result<InsightItem, ServiceError> {
    if ctx.db.insights().get(id)?.is_none() {
        return Err(ServiceError::NotFound(format!("洞察 {id}")));
    }
    let fb = match non_empty(&req.feedback) {
        Some(v) => Some(parse_feedback(&v)?),
        None => None,
    };
    ctx.db.insights().set_feedback(id, fb)?;

    let after = ctx
        .db
        .insights()
        .get(id)?
        .ok_or_else(|| ServiceError::NotFound(format!("洞察 {id}")))?;
    Ok(item(&after, ctx.now()))
}

/// 采纳率（阶段二关键指标）。
pub fn adoption(ctx: &ServiceContext) -> Result<AdoptionView, ServiceError> {
    let (useful, rated) = ctx.db.insights().adoption_rate()?;
    let rate = if rated == 0 { None } else { Some(useful as f64 / rated as f64) };
    Ok(AdoptionView {
        useful,
        rated,
        rate,
        // 🔴 分母为 0 时必须说"暂无反馈"，显示 0% 会让用户以为结论都不准
        label: match rate {
            None => "暂无反馈：给洞察标记「有用 / 无用」后这里会显示采纳率".to_string(),
            Some(r) => format!(
                "采纳率 {:.0}%（{} / {} 条被标记）",
                r * 100.0,
                useful,
                rated
            ),
        },
    })
}

/// 洞察与机会汇总（洞察页头部）。
pub fn summary(ctx: &ServiceContext) -> Result<InsightsSummary, ServiceError> {
    Ok(InsightsSummary {
        insight_total: ctx.db.insights().count()?,
        insight_unread: ctx.db.insights().count_unread()?,
        opportunity_total: ctx.db.opportunities().count()?,
        opportunity_actionable: ctx.db.opportunities().count_actionable()?,
        adoption: adoption(ctx)?,
        by_type: ctx
            .db
            .insights()
            .count_by_type()?
            .iter()
            .map(|(t, n)| TypeFacet {
                value: t.as_str().to_string(),
                label: t.label_zh().to_string(),
                count: *n,
            })
            .collect(),
    })
}

// ══════════════════════════════════════════════════════════════════
// 机会
// ══════════════════════════════════════════════════════════════════

/// 机会列表。
pub fn list_opportunities(
    ctx: &ServiceContext,
    q: &OpportunityListQuery,
) -> Result<OpportunityListPage, ServiceError> {
    let filter = build_opp_filter(q)?;
    let total = ctx.db.opportunities().count_filtered(&filter)?;
    let opps = ctx.db.opportunities().list(&filter)?;

    let items: Vec<OpportunityItem> = {
        // 🔴 一次性取回"已分析"的 id 集合，避免逐条 get_analysis 的 N+1
        let analyzed: std::collections::HashSet<String> =
            ctx.db.opportunities().ids_with_analysis()?.into_iter().collect();
        let now = ctx.now();
        opps.iter()
            .map(|o| opp_item(ctx, o, now, analyzed.contains(&o.id)))
            .collect::<Result<_, _>>()?
    };

    Ok(OpportunityListPage {
        items,
        total,
        limit: q.effective_limit(),
        offset: q.offset,
        facets: status_facets(ctx)?,
        actionable_count: ctx.db.opportunities().count_actionable()?,
        empty_hint: opp_empty_hint(ctx, total)?,
    })
}

/// 机会详情。
pub fn opportunity_detail(
    ctx: &ServiceContext,
    id: &str,
) -> Result<OpportunityDetail, ServiceError> {
    let o = ctx
        .db
        .opportunities()
        .get(id)?
        .ok_or_else(|| ServiceError::NotFound(format!("机会 {id}")))?;
    // 分析存在单独的列，不在 Opportunity 结构里：逐条读取
    let analysis = ctx.db.opportunities().get_analysis(id)?;

    Ok(OpportunityDetail {
        item: opp_item(ctx, &o, ctx.now(), analysis.is_some())?,
        analysis,
    })
}

/// 变更机会状态（Explore / Dismiss / Adopt）。
///
/// 🔴 Dismiss 是**状态变更而非删除**：产品要求"Dismiss 全部后进入空状态"，
/// 但机会本身要留存以便审计与恢复。真删除只在"清理派生数据"时发生。
pub fn set_status(
    ctx: &ServiceContext,
    id: &str,
    req: &StatusRequest,
) -> Result<OpportunityItem, ServiceError> {
    let status = parse_status(&req.status)?;

    let before = ctx
        .db
        .opportunities()
        .get(id)?
        .ok_or_else(|| ServiceError::NotFound(format!("机会 {id}")))?;

    // 已采纳的机会再改成 dismissed 是数据回退，明确拒绝而不是静默执行：
    // Adopt 意味着用户已据此建了项目，把它标成"已忽略"会让审计记录自相矛盾。
    if before.status == OpportunityStatus::Adopted && status != OpportunityStatus::Adopted {
        return Err(ServiceError::Conflict(format!(
            "机会已标记为「已采纳」，不能再改为「{}」",
            status.label_zh()
        )));
    }

    ctx.db.opportunities().set_status(id, status)?;
    let after = ctx
        .db
        .opportunities()
        .get(id)?
        .ok_or_else(|| ServiceError::NotFound(format!("机会 {id}")))?;
    // 状态变更不影响分析，但 opp_item 需要 has_analysis 标志
    let has_analysis = ctx.db.opportunities().get_analysis(id)?.is_some();
    opp_item(ctx, &after, ctx.now(), has_analysis)
}

/// 批量 Dismiss 全部可操作机会（产品要求：清空后进入明确空状态）。
///
/// 返回受影响条数，前端据此显示"已忽略 N 条"而不是干巴巴一个成功。
pub fn dismiss_all(ctx: &ServiceContext) -> Result<usize, ServiceError> {
    let n = ctx.db.opportunities().dismiss_all()?;
    if n > 0 {
        // 批量处置会影响"采纳率"这类指标的解释，留痕
        let _ = ctx.db.activities().push(
            projectassests_storage::ActivityIcon::Check,
            "批量忽略机会",
            format!("已忽略 {n} 条组合机会"),
        );
    }
    Ok(n)
}

// ══════════════════════════════════════════════════════════════════
// 内部辅助
// ══════════════════════════════════════════════════════════════════

fn build_filter(q: &InsightListQuery) -> Result<InsightFilter, ServiceError> {
    if let Some(v) = q.min_confidence
        && !(0.0..=1.0).contains(&v)
    {
        return Err(ServiceError::Invalid(format!(
            "min_confidence 必须在 0.0-1.0 之间，收到 {v}"
        )));
    }
    Ok(InsightFilter {
        insight_type: None,
        types: parse_types(&q.types)?,
        unread_only: q.unread_only,
        min_confidence: q.min_confidence,
        limit: Some(q.effective_limit()),
        offset: q.offset,
    })
}

fn build_opp_filter(q: &OpportunityListQuery) -> Result<OpportunityFilter, ServiceError> {
    if let Some(r) = q.min_rating
        && !(1..=5).contains(&r)
    {
        return Err(ServiceError::Invalid(format!(
            "min_rating 必须在 1-5 之间，收到 {r}"
        )));
    }

    // 显式给了 statuses 就按它来；否则 include_closed=true 看全部、
    // 默认只看可操作。三条路径必须互斥且可预测。
    let statuses = match non_empty(&q.statuses) {
        Some(_) => Some(parse_statuses(&q.statuses)?),
        None => {
            if q.include_closed.unwrap_or(false) {
                None
            } else {
                Some(OpportunityStatus::actionable())
            }
        }
    };

    Ok(OpportunityFilter {
        statuses,
        min_rating: q.min_rating,
        limit: Some(q.effective_limit()),
        offset: q.offset,
    })
}

fn item(i: &Insight, now: chrono::DateTime<chrono::Utc>) -> InsightItem {
    let badge = i.badge();
    let (state_key, state) = state_of(i);
    InsightItem {
        id: i.id.clone(),
        insight_type: i.insight_type.as_str().to_string(),
        type_label: i.insight_type.label_zh().to_string(),
        title: i.title.clone(),
        description: i.description.clone(),
        confidence: i.confidence,
        confidence_percent: (i.confidence.clamp(0.0, 1.0) * 100.0).round() as u8,
        tags: i.tags.clone(),
        created_at: i.created_at.clone(),
        created_relative: projectassests_storage::relative_time(&i.created_at, now),
        // 🔴 与首页、项目详情页同源：都走 `Insight::badge()`
        badge: badge.label_zh().to_string(),
        badge_key: badge.as_str().to_string(),
        state,
        state_key: state_key.to_string(),
        evidence_count: i.evidence.len(),
        related_project_ids: i.related_project_ids.clone(),
        related_asset_ids: i.related_asset_ids.clone(),
        user_feedback: i.user_feedback.map(|f| f.as_str().to_string()),
    }
}

/// 用户对这条洞察的处置状态。
///
/// # 为什么不并进 `badge`
/// badge 是系统对**价值**的判断（置信度算出，点击不变），
/// state 是用户的**处置**（点一次就变）。
/// 早期版本把两者压进一个 `badge` 字段，标记"无用"后价值信息就被覆盖丢了，
/// 而且无法区分"从未处理"与"处理过但价值低"。
///
/// # 三种反馈都必须有各自的文案
/// 🔴 `Useless` 曾经没有分支，会落到"待处理"：
/// 用户明确标了"无用"，界面却仍显示"待处理"，
/// 等于反馈没有任何视觉确认——用户会以为没点上而反复点击。
fn state_of(i: &Insight) -> (&'static str, String) {
    match i.user_feedback {
        Some(UserFeedback::Useful) => ("useful", "已标记有用".to_string()),
        Some(UserFeedback::Useless) => ("useless", "已标记无用".to_string()),
        Some(UserFeedback::Ignored) => ("ignored", "已忽略".to_string()),
        None => ("pending", "待处理".to_string()),
    }
}

fn evidence_view(e: &projectassests_domain::EvidenceItem) -> EvidenceView {
    EvidenceView {
        kind: e.kind.as_str().to_string(),
        kind_label: evidence_kind_label(&e.kind),
        label: e.label.clone(),
        target: e.target.clone(),
    }
}

fn evidence_kind_label(k: &projectassests_domain::EvidenceKind) -> String {
    match k {
        projectassests_domain::EvidenceKind::File => "文件",
        projectassests_domain::EvidenceKind::Project => "项目",
        projectassests_domain::EvidenceKind::Commit => "提交",
        projectassests_domain::EvidenceKind::Symbol => "符号",
    }
    .to_string()
}

fn related_projects(
    ctx: &ServiceContext,
    ids: &[String],
) -> Result<Vec<RelatedProject>, ServiceError> {
    let mut out = Vec::new();
    for id in ids {
        // 洞察可能引用已被删除的项目：跳过而不是报错，
        // 否则一条陈旧洞察会让整个详情页 404。
        let Some(p) = ctx.db.projects().get(id)? else {
            continue;
        };
        out.push(RelatedProject {
            id: p.id.clone(),
            name: p.name.clone(),
            language: p.language.clone(),
            status: p.status.as_str().to_string(),
            status_label: p.status.label_zh().to_string(),
        });
    }
    Ok(out)
}

fn related_assets(
    ctx: &ServiceContext,
    ids: &[String],
) -> Result<Vec<RelatedAsset>, ServiceError> {
    let mut out = Vec::new();
    for id in ids {
        let Some(a) = ctx.db.assets().get(id)? else {
            continue;
        };
        out.push(RelatedAsset {
            id: a.id.clone(),
            name: a.name.clone(),
            asset_type: a.asset_type.as_str().to_string(),
            type_label: a.asset_type.label_zh().to_string(),
            source_path: a.source_path.clone(),
            reuse_score: a.reuse_score,
        });
    }
    Ok(out)
}

/// 类型 chips：计数受当前筛选影响（**排除类型维度自身**）。
///
/// 🔴 两个坑：
/// 1. 用全表 `count_by_type()` 会让 chip 数字与列表条数对不上
///    （用户筛了"未读"，chip 却显示全库计数）。
/// 2. 不排除类型维度自身的话，勾了"重复能力"后其他 chip 全变 0，
///    看起来像"其他类型都没有数据"。
/// 3. 计数为 0 的类型也必须出现，否则 chip 消失后点不回来。
fn type_facets(
    ctx: &ServiceContext,
    base: &InsightFilter,
) -> Result<Vec<TypeFacet>, ServiceError> {
    let mut out = Vec::new();
    for t in InsightType::all() {
        let f = InsightFilter {
            insight_type: Some(*t),
            types: Vec::new(),
            ..base.clone()
        };
        out.push(TypeFacet {
            value: t.as_str().to_string(),
            label: t.label_zh().to_string(),
            count: ctx.db.insights().count_filtered(&f)?,
        });
    }
    Ok(out)
}

fn status_facets(ctx: &ServiceContext) -> Result<Vec<TypeFacet>, ServiceError> {
    let mut out = Vec::new();
    for s in OpportunityStatus::all() {
        let f = OpportunityFilter {
            statuses: Some(vec![*s]),
            ..Default::default()
        };
        out.push(TypeFacet {
            value: s.as_str().to_string(),
            label: s.label_zh().to_string(),
            count: ctx.db.opportunities().count_filtered(&f)?,
        });
    }
    Ok(out)
}

fn opp_item(
    ctx: &ServiceContext,
    o: &Opportunity,
    now: chrono::DateTime<chrono::Utc>,
    has_analysis: bool,
) -> Result<OpportunityItem, ServiceError> {
    Ok(OpportunityItem {
        id: o.id.clone(),
        title: o.title.clone(),
        description: o.description.clone(),
        why: o.why.clone(),
        rating: o.rating,
        rating_stars: stars(o.rating),
        coverage: o.coverage,
        coverage_percent: (o.coverage.clamp(0.0, 1.0) * 100.0).round() as u8,
        required_capabilities: o.required_capabilities.clone(),
        missing_capabilities: o.missing_capabilities.clone(),
        evidence: o.evidence.clone(),
        status: o.status.as_str().to_string(),
        status_label: o.status.label_zh().to_string(),
        created_at: o.created_at.clone(),
        created_relative: projectassests_storage::relative_time(&o.created_at, now),
        source_projects: related_projects(ctx, &o.source_project_ids)?,
        actionable: o.status.is_actionable(),
        // 🔴 由调用方传入：`Opportunity` 结构不含 analysis 列（单独存 analysis_json），
        // 在这里现查会退化成 N+1。列表用 ids_with_analysis 批量判断，详情用 get_analysis。
        has_analysis,
    })
}

/// 星级展示串。clamp 到 1-5：脏数据不该让 UI 渲染出 7 颗星。
fn stars(rating: u8) -> String {
    let n = rating.clamp(1, 5) as usize;
    format!("{}{}", "★".repeat(n), "☆".repeat(5 - n))
}

/// 机会页的空状态引导。
///
/// 🔴 必须区分"从来没生成过"和"用户全 Dismiss 了"：
/// 前者该引导去扫描，后者该说明"已全部忽略，可在审计视图找回"。
/// 混淆的话用户会在已经清空的列表上反复点"重新分析"。
fn opp_empty_hint(ctx: &ServiceContext, total: usize) -> Result<Option<String>, ServiceError> {
    if total > 0 {
        return Ok(None);
    }
    let any_at_all = ctx.db.opportunities().count()?;
    if any_at_all > 0 {
        return Ok(Some(
            "当前筛选下没有机会。已忽略和已采纳的仍然保留——勾选「含已关闭」可以找回。".to_string(),
        ));
    }
    Ok(Some(
        "还没有发现组合机会。机会由跨项目分析产生，请先完成一次扫描并生成洞察。".to_string(),
    ))
}

fn parse_types(s: &Option<String>) -> Result<Vec<InsightType>, ServiceError> {
    let Some(raw) = non_empty(s) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for part in raw.split(',') {
        let v = part.trim();
        if v.is_empty() {
            continue;
        }
        let t = InsightType::parse(v).ok_or_else(|| {
            ServiceError::Invalid(format!(
                "未知的洞察类型：{v}（可选 {}）",
                InsightType::all()
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ))
        })?;
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

fn parse_statuses(s: &Option<String>) -> Result<Vec<OpportunityStatus>, ServiceError> {
    let Some(raw) = non_empty(s) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for part in raw.split(',') {
        let v = part.trim();
        if v.is_empty() {
            continue;
        }
        let st = OpportunityStatus::parse(v).ok_or_else(|| {
            ServiceError::Invalid(format!(
                "未知的机会状态：{v}（可选 {}）",
                OpportunityStatus::all()
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ))
        })?;
        if !out.contains(&st) {
            out.push(st);
        }
    }
    Ok(out)
}

fn parse_status(s: &str) -> Result<OpportunityStatus, ServiceError> {
    let v = s.trim();
    OpportunityStatus::parse(v).ok_or_else(|| {
        ServiceError::Invalid(format!(
            "未知的机会状态：{v}（可选 {}）",
            OpportunityStatus::all()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        ))
    })
}

fn parse_feedback(v: &str) -> Result<UserFeedback, ServiceError> {
    UserFeedback::parse(v).ok_or_else(|| {
        ServiceError::Invalid(format!(
            "未知的反馈值：{v}（可选 {}）",
            UserFeedback::all()
                .iter()
                .map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        ))
    })
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{
        Asset, AssetType, CodeStats, Evidence, EvidenceItem, EvidenceKind, OpportunityAnalysis,
        Project, ProjectStatus, ScanFacts,
    };

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
            updated_at: Some("2026-09-20".into()),
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

    fn insight(id: &str, t: InsightType, confidence: f64, evidence_n: usize) -> Insight {
        Insight {
            id: id.into(),
            insight_type: t,
            title: format!("{id} 的标题"),
            description: "你在多个项目里重复实现了同一个能力".into(),
            evidence: (0..evidence_n)
                .map(|i| EvidenceItem {
                    kind: EvidenceKind::File,
                    label: format!("src/queue{i}.rs"),
                    target: Some(format!("p1:src/queue{i}.rs")),
                })
                .collect(),
            confidence,
            created_at: projectassests_storage::now_utc(),
            user_feedback: None,
            tags: vec!["queue".into()],
            related_project_ids: vec!["p1".into()],
            related_asset_ids: vec!["a1".into()],
        }
    }

    fn opportunity(id: &str, status: OpportunityStatus, rating: u8) -> Opportunity {
        Opportunity {
            id: id.into(),
            title: format!("{id} 的组合机会"),
            description: "把三处重复的队列实现抽成一个 crate".into(),
            source_project_ids: vec!["p1".into()],
            source_asset_ids: vec!["a1".into()],
            required_capabilities: vec!["任务队列".into()],
            missing_capabilities: vec!["统一配置".into()],
            coverage: 0.5,
            rating,
            why: "3 个历史项目存在能力重合".into(),
            evidence: vec!["src/queue.rs".into()],
            status,
            created_at: projectassests_storage::now_utc(),
        }
    }

    fn asset(id: &str) -> Asset {
        Asset {
            id: id.into(),
            project_id: "p1".into(),
            asset_type: AssetType::Code,
            name: "TaskQueue".into(),
            description: "任务队列".into(),
            content: None,
            source_path: "src/queue.rs".into(),
            confidence: 0.9,
            reuse_score: 0.91,
            generality: 0.7,
            stability: 0.6,
            tags: vec![],
            created_at: projectassests_storage::now_utc(),
            evidence: Evidence {
                files: vec!["src/queue.rs".into()],
                ..Evidence::default()
            },
            user_feedback: None,
        }
    }

    fn seeded() -> ServiceContext {
        let c = ctx();
        c.db.projects().upsert(&project("p1", "项目一")).unwrap();
        c.db.assets().upsert(&asset("a1")).unwrap();
        c
    }

    // ── 默认值一致性 ────────────────────────────────────────────

    #[test]
    fn default_queries_match_serde_defaults() {
        // 🔴 回归：derive(Default) 会让 limit=0
        assert_eq!(
            InsightListQuery::default().effective_limit(),
            serde_json::from_str::<InsightListQuery>("{}")
                .unwrap()
                .effective_limit()
        );
        assert_eq!(
            OpportunityListQuery::default().effective_limit(),
            serde_json::from_str::<OpportunityListQuery>("{}")
                .unwrap()
                .effective_limit()
        );
        assert_eq!(InsightListQuery::default().effective_limit(), 20);
    }

    // ── 洞察列表 ────────────────────────────────────────────────

    #[test]
    fn insight_total_is_stable_across_pages() {
        let c = seeded();
        for i in 0..5 {
            c.db
                .insights()
                .upsert(&insight(&format!("i{i}"), InsightType::DuplicateCapability, 0.9, 2))
                .unwrap();
        }
        let p1 = list(
            &c,
            &InsightListQuery { limit: 2, offset: 0, ..Default::default() },
        )
        .unwrap();
        let p2 = list(
            &c,
            &InsightListQuery { limit: 2, offset: 2, ..Default::default() },
        )
        .unwrap();
        // 🔴 回归：total 曾随翻页缩短
        assert_eq!(p1.total, 5);
        assert_eq!(p2.total, 5);
        assert_eq!(p1.items.len(), 2);
        assert_eq!(p1.unread, 5, "全部未反馈即全部未读");
    }

    #[test]
    fn insight_facets_survive_type_selection() {
        let c = seeded();
        c.db
            .insights()
            .upsert(&insight("i1", InsightType::DuplicateCapability, 0.9, 2))
            .unwrap();
        c.db
            .insights()
            .upsert(&insight("i2", InsightType::ForgottenAsset, 0.9, 2))
            .unwrap();

        let page = list(
            &c,
            &InsightListQuery {
                types: Some("duplicate_capability".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.total, 1);
        // 🔴 勾了某类型后其他 chip 不能全变 0（类型维度要排除在自身统计外）
        let forgotten = page
            .facets
            .iter()
            .find(|f| f.value == "forgotten_asset")
            .unwrap();
        assert_eq!(forgotten.count, 1, "其他类型的计数应不受当前类型筛选影响");
        assert_eq!(
            page.facets.len(),
            InsightType::all().len(),
            "所有 chip 都必须存在，否则勾掉后点不回来"
        );
    }

    #[test]
    fn unread_only_filter_works() {
        let c = seeded();
        c.db
            .insights()
            .upsert(&insight("i1", InsightType::DuplicateCapability, 0.9, 2))
            .unwrap();
        c.db
            .insights()
            .upsert(&insight("i2", InsightType::ForgottenAsset, 0.9, 2))
            .unwrap();
        c.db
            .insights()
            .set_feedback("i1", Some(UserFeedback::Useful))
            .unwrap();

        let unread = list(
            &c,
            &InsightListQuery {
                unread_only: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(unread.total, 1);
        assert_eq!(unread.items[0].id, "i2");
        assert_eq!(unread.unread, 1);
    }

    #[test]
    fn unknown_insight_type_is_rejected_with_options() {
        let c = seeded();
        let err = list(
            &c,
            &InsightListQuery {
                types: Some("duplicate_capability,bogus".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bogus"), "{msg}");
        assert!(msg.contains("forgotten_asset"), "应列出可选值: {msg}");
    }

    #[test]
    fn invalid_min_confidence_is_rejected() {
        let c = seeded();
        let err = list(
            &c,
            &InsightListQuery {
                min_confidence: Some(1.2),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("0.0-1.0"));
    }

    // ── 徽章与处置状态 ──────────────────────────────────────────

    #[test]
    fn badge_comes_from_confidence_tiers() {
        let c = seeded();
        // badge 是价值分档（置信度算出），三档必须可区分：
        // ≥0.85 高价值 / ≥0.7 高潜力 / 其余 建议查看
        c.db.insights().upsert(&insight("i1", InsightType::DuplicateCapability, 0.9, 4)).unwrap();
        c.db.insights().upsert(&insight("i2", InsightType::ReusableComponent, 0.75, 1)).unwrap();
        c.db.insights().upsert(&insight("i3", InsightType::TechDirection, 0.6, 1)).unwrap();

        let page = list(&c, &InsightListQuery::default()).unwrap();
        let by_id = |id: &str| page.items.iter().find(|i| i.id == id).unwrap();
        assert_eq!(by_id("i1").badge, "高价值");
        assert_eq!(by_id("i1").badge_key, "high");
        assert_eq!(by_id("i2").badge, "高潜力");
        assert_eq!(by_id("i2").badge_key, "potent");
        assert_eq!(by_id("i3").badge, "建议查看");
        assert_eq!(by_id("i3").badge_key, "info");
    }

    #[test]
    fn badge_matches_homepage_source_of_truth() {
        let c = seeded();
        c.db.insights().upsert(&insight("i1", InsightType::DuplicateCapability, 0.9, 2)).unwrap();

        // 🔴 回归：曾在洞察页另写一套徽章逻辑（"证据充分/待评估"），
        // 导致同一条洞察在首页显示"高价值"、在洞察页显示别的词，
        // 用户以为是两条不同的洞察。徽章全站必须同源 = `Insight::badge()`。
        let insight_page = list(&c, &InsightListQuery::default()).unwrap();
        let overview_badge = crate::overview::load(&c)
            .unwrap()
            .recent_insights
            .iter()
            .find(|i| i.id == "i1")
            .map(|i| i.badge.clone())
            .expect("首页应包含该洞察");
        assert_eq!(
            insight_page.items[0].badge, overview_badge,
            "同一条洞察在首页与洞察页的徽章必须一致"
        );
    }

    #[test]
    fn state_reflects_every_feedback_kind() {
        let c = seeded();
        for id in ["i1", "i2", "i3", "i4"] {
            c.db
                .insights()
                .upsert(&insight(id, InsightType::DuplicateCapability, 0.9, 2))
                .unwrap();
        }
        c.db.insights().set_feedback("i1", Some(UserFeedback::Useful)).unwrap();
        c.db.insights().set_feedback("i2", Some(UserFeedback::Useless)).unwrap();
        c.db.insights().set_feedback("i3", Some(UserFeedback::Ignored)).unwrap();

        let page = list(&c, &InsightListQuery::default()).unwrap();
        let by_id = |id: &str| page.items.iter().find(|i| i.id == id).unwrap();
        assert_eq!(by_id("i1").state, "已标记有用");
        assert_eq!(by_id("i1").state_key, "useful");
        // 🔴 回归：Useless 曾落到"待处理"，用户标了"无用"却看不到确认
        assert_eq!(by_id("i2").state, "已标记无用");
        assert_eq!(by_id("i2").state_key, "useless");
        assert_eq!(by_id("i3").state, "已忽略");
        assert_eq!(by_id("i3").state_key, "ignored");
        assert_eq!(by_id("i4").state, "待处理");
        assert_eq!(by_id("i4").state_key, "pending");

        // 🔴 处置状态变化**不得**影响价值徽章：两者是独立维度
        assert_eq!(by_id("i1").badge, by_id("i4").badge);
        assert_eq!(by_id("i2").badge_key, "high");
    }

    #[test]
    fn low_confidence_insight_is_rejected_at_the_gate() {
        let c = seeded();
        // 门禁在 upsert：置信度 < 0.55 的洞察进不了库
        let weak = insight("i1", InsightType::DuplicateCapability, 0.3, 2);
        let written = c.db.insights().upsert(&weak).unwrap();
        assert!(!written, "低置信洞察必须被门禁拒写");
        assert_eq!(list(&c, &InsightListQuery::default()).unwrap().total, 0);
    }

    #[test]
    fn evidence_free_insight_is_rejected_at_the_gate() {
        let c = seeded();
        let bare = insight("i1", InsightType::DuplicateCapability, 0.9, 0);
        let written = c.db.insights().upsert(&bare).unwrap();
        assert!(!written, "无证据洞察必须被门禁拒写");
    }

    #[test]
    fn adoption_is_none_when_nobody_voted() {
        let c = seeded();
        let a = adoption(&c).unwrap();
        assert_eq!(a.rated, 0);
        // 🔴 显示 0% 会让用户以为结论都不准，真相是还没人投票
        assert!(a.rate.is_none());
        assert!(a.label.contains("暂无反馈"), "{:?}", a.label);
    }

    #[test]
    fn adoption_computes_rate_after_votes() {
        let c = seeded();
        for i in 0..4 {
            c.db
                .insights()
                .upsert(&insight(&format!("i{i}"), InsightType::DuplicateCapability, 0.9, 2))
                .unwrap();
        }
        c.db.insights().set_feedback("i0", Some(UserFeedback::Useful)).unwrap();
        c.db.insights().set_feedback("i1", Some(UserFeedback::Useful)).unwrap();
        c.db.insights().set_feedback("i2", Some(UserFeedback::Useless)).unwrap();

        let a = adoption(&c).unwrap();
        assert_eq!(a.useful, 2);
        assert_eq!(a.rated, 3);
        let rate = a.rate.expect("有人投票后必须有比率");
        assert!((rate - 2.0 / 3.0).abs() < 1e-9, "{rate}");
        assert!(a.label.contains("67%"), "{:?}", a.label);
    }

    // ── 反馈 ────────────────────────────────────────────────────

    #[test]
    fn feedback_roundtrip_and_revoke() {
        let c = seeded();
        c.db
            .insights()
            .upsert(&insight("i1", InsightType::DuplicateCapability, 0.9, 2))
            .unwrap();

        let item = set_feedback(&c, "i1", &FeedbackRequest { feedback: Some("useful".into()) }).unwrap();
        assert_eq!(item.user_feedback.as_deref(), Some("useful"));
        assert_eq!(item.state, "已标记有用");
        assert_eq!(item.state_key, "useful");

        // 🔴 徽章是价值判断，不因用户反馈而改变
        assert_eq!(item.badge, "高价值");

        let revoked = set_feedback(&c, "i1", &FeedbackRequest { feedback: None }).unwrap();
        assert!(revoked.user_feedback.is_none());
        assert_eq!(revoked.state_key, "pending", "撤销反馈应回到待处理");
        assert_eq!(revoked.badge, "高价值", "撤销反馈不得影响价值徽章");
    }

    #[test]
    fn feedback_on_missing_insight_is_404() {
        let c = seeded();
        let err = set_feedback(&c, "ghost", &FeedbackRequest { feedback: Some("useful".into()) })
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
        assert_eq!(err.status_code(), 404);
    }

    #[test]
    fn invalid_feedback_value_lists_options() {
        let c = seeded();
        c.db
            .insights()
            .upsert(&insight("i1", InsightType::DuplicateCapability, 0.9, 2))
            .unwrap();
        let err = set_feedback(&c, "i1", &FeedbackRequest { feedback: Some("meh".into()) })
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("meh"));
        assert!(msg.contains("useful") && msg.contains("ignored"), "{msg}");
    }

    // ── 详情 ────────────────────────────────────────────────────

    #[test]
    fn detail_resolves_related_entities() {
        let c = seeded();
        c.db
            .insights()
            .upsert(&insight("i1", InsightType::DuplicateCapability, 0.9, 2))
            .unwrap();

        let d = detail(&c, "i1").unwrap();
        assert_eq!(d.evidence.len(), 2);
        assert_eq!(d.evidence[0].kind_label, "文件");
        assert_eq!(d.related_projects.len(), 1);
        assert_eq!(d.related_projects[0].name, "项目一");
        assert_eq!(d.related_assets.len(), 1);
        assert_eq!(d.related_assets[0].name, "TaskQueue");
    }

    #[test]
    fn detail_skips_deleted_related_entities() {
        let c = ctx();
        // 洞察引用了不存在的项目与资产：不该让整个详情页 404
        let mut i = insight("i1", InsightType::DuplicateCapability, 0.9, 1);
        i.related_project_ids = vec!["ghost_p".into()];
        i.related_asset_ids = vec!["ghost_a".into()];
        c.db.insights().upsert(&i).unwrap();

        let d = detail(&c, "i1").unwrap();
        assert!(d.related_projects.is_empty());
        assert!(d.related_assets.is_empty());
        assert_eq!(d.item.id, "i1");
    }

    #[test]
    fn detail_of_missing_insight_is_404() {
        let c = seeded();
        assert!(matches!(detail(&c, "ghost").unwrap_err(), ServiceError::NotFound(_)));
    }

    // ── 机会 ────────────────────────────────────────────────────

    #[test]
    fn opportunity_list_hides_closed_by_default() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        c.db.opportunities().upsert(&opportunity("o2", OpportunityStatus::Dismissed, 3)).unwrap();

        let page = list_opportunities(&c, &OpportunityListQuery::default()).unwrap();
        assert_eq!(page.total, 1, "默认不该列出已忽略的");
        assert_eq!(page.items[0].id, "o1");
        assert!(page.items[0].actionable);

        let all = list_opportunities(
            &c,
            &OpportunityListQuery {
                include_closed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(all.total, 2);
        assert_eq!(all.actionable_count, 1);
    }

    #[test]
    fn opportunity_status_facets_include_zero_counts() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        let page = list_opportunities(&c, &OpportunityListQuery::default()).unwrap();
        assert_eq!(page.facets.len(), OpportunityStatus::all().len());
        assert!(page.facets.iter().any(|f| f.value == "adopted" && f.count == 0));
    }

    #[test]
    fn empty_hint_distinguishes_never_generated_from_all_dismissed() {
        // 从未生成过：引导去扫描
        let c1 = seeded();
        let h1 = list_opportunities(&c1, &OpportunityListQuery::default())
            .unwrap()
            .empty_hint
            .expect("空库应有引导");
        assert!(h1.contains("扫描"), "{h1}");

        // 全部忽略：引导去审计视图找回
        let c2 = seeded();
        c2.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        dismiss_all(&c2).unwrap();
        let h2 = list_opportunities(&c2, &OpportunityListQuery::default())
            .unwrap()
            .empty_hint
            .expect("全忽略后应有引导");
        // 🔴 这两种空状态混用同一句话，用户会在已清空的列表上反复点"重新分析"
        assert!(h2.contains("已忽略"), "{h2}");
        assert!(!h2.contains("扫描"), "{h2}");
    }

    #[test]
    fn non_empty_list_has_no_hint() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        let page = list_opportunities(&c, &OpportunityListQuery::default()).unwrap();
        assert!(page.empty_hint.is_none());
    }

    #[test]
    fn stars_and_coverage_are_clamped() {
        let c = seeded();
        // rating 超范围（脏数据）不该渲染出 7 颗星
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 9)).unwrap();
        let mut o = opportunity("o2", OpportunityStatus::New, 0);
        o.coverage = 1.7;
        c.db.opportunities().upsert(&o).unwrap();

        let page = list_opportunities(&c, &OpportunityListQuery::default()).unwrap();
        let by_id = |id: &str| page.items.iter().find(|i| i.id == id).unwrap();
        assert_eq!(by_id("o1").rating_stars.chars().count(), 5);
        assert_eq!(by_id("o1").rating_stars, "★★★★★");
        assert_eq!(by_id("o2").coverage_percent, 100);
    }

    #[test]
    fn status_transition_roundtrip() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();

        let explored = set_status(
            &c,
            "o1",
            &StatusRequest { status: "explored".into() },
        )
        .unwrap();
        assert_eq!(explored.status, "explored");
        assert_eq!(explored.status_label, "已分析");
        assert!(explored.actionable, "explored 仍属可操作");

        let dismissed = set_status(
            &c,
            "o1",
            &StatusRequest { status: "dismissed".into() },
        )
        .unwrap();
        assert!(!dismissed.actionable, "dismissed 不该再显示处置按钮");
    }

    #[test]
    fn adopted_opportunity_cannot_be_downgraded() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        set_status(&c, "o1", &StatusRequest { status: "adopted".into() }).unwrap();

        // 🔴 Adopt 意味着用户已据此建了项目，再标"已忽略"会让审计自相矛盾
        let err = set_status(
            &c,
            "o1",
            &StatusRequest { status: "dismissed".into() },
        )
        .unwrap_err();
        assert!(matches!(err, ServiceError::Conflict(_)), "{err:?}");
        assert_eq!(err.status_code(), 409);
        assert!(err.to_string().contains("已采纳"), "{err}");

        // 状态未被改动
        let d = opportunity_detail(&c, "o1").unwrap();
        assert_eq!(d.item.status, "adopted");
    }

    #[test]
    fn adopted_to_adopted_is_idempotent() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        set_status(&c, "o1", &StatusRequest { status: "adopted".into() }).unwrap();
        // 重复提交同一状态不该报错（前端重试是常态）
        let again = set_status(&c, "o1", &StatusRequest { status: "adopted".into() });
        assert!(again.is_ok(), "{again:?}");
    }

    #[test]
    fn unknown_status_is_rejected_with_options() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        let err = set_status(&c, "o1", &StatusRequest { status: "done".into() }).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("done"), "{msg}");
        assert!(msg.contains("dismissed"), "应列出可选值: {msg}");
    }

    #[test]
    fn status_of_missing_opportunity_is_404() {
        let c = seeded();
        let err = set_status(
            &c,
            "ghost",
            &StatusRequest { status: "explored".into() },
        )
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn dismiss_all_returns_count_and_records_activity() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        c.db.opportunities().upsert(&opportunity("o2", OpportunityStatus::Explored, 3)).unwrap();
        c.db.opportunities().upsert(&opportunity("o3", OpportunityStatus::Adopted, 5)).unwrap();

        let n = dismiss_all(&c).unwrap();
        assert_eq!(n, 2, "只处理可操作的，已采纳的不动");
        assert_eq!(c.db.opportunities().count_actionable().unwrap(), 0);
        // 已采纳的必须留存
        assert_eq!(c.db.opportunities().count().unwrap(), 3);

        let acts = activities(&c).unwrap();
        assert!(acts.iter().any(|a| a.detail.contains("2 条")), "{acts:?}");
    }

    #[test]
    fn dismiss_all_on_empty_db_is_zero_not_error() {
        let c = seeded();
        assert_eq!(dismiss_all(&c).unwrap(), 0);
        // 没有实际变更就不该往活动流里塞噪音
        assert!(activities(&c).unwrap().is_empty());
    }

    #[test]
    fn invalid_min_rating_is_rejected() {
        let c = seeded();
        for r in [0u8, 6] {
            let err = list_opportunities(
                &c,
                &OpportunityListQuery {
                    min_rating: Some(r),
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(err.to_string().contains("1-5"), "{err}");
        }
    }

    #[test]
    fn min_rating_filters() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 2)).unwrap();
        c.db.opportunities().upsert(&opportunity("o2", OpportunityStatus::New, 5)).unwrap();
        let page = list_opportunities(
            &c,
            &OpportunityListQuery {
                min_rating: Some(4),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].id, "o2");
    }

    #[test]
    fn explicit_statuses_override_include_closed() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        c.db.opportunities().upsert(&opportunity("o2", OpportunityStatus::Adopted, 5)).unwrap();

        let page = list_opportunities(
            &c,
            &OpportunityListQuery {
                statuses: Some("adopted".into()),
                include_closed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.total, 1, "显式 statuses 优先");
        assert_eq!(page.items[0].id, "o2");
    }

    #[test]
    fn detail_carries_analysis_when_present() {
        let c = seeded();
        let o = opportunity("o1", OpportunityStatus::Explored, 4);
        c.db.opportunities().upsert(&o).unwrap();
        // 🔴 分析存在单独的 analysis_json 列，用 set_analysis 写，不在 Opportunity 结构里
        c.db
            .opportunities()
            .set_analysis(
                "o1",
                &OpportunityAnalysis {
                    opportunity_id: "o1".into(),
                    rationale: "三处实现高度重合".into(),
                    reusable: vec![],
                    to_build: vec!["统一配置".into()],
                    mvp_suggestion: "先抽 crate".into(),
                    scaffold: vec!["src/lib.rs".into()],
                },
            )
            .unwrap();

        let bare = opportunity("o2", OpportunityStatus::New, 4);
        c.db.opportunities().upsert(&bare).unwrap();

        let d1 = opportunity_detail(&c, "o1").unwrap();
        assert!(d1.item.has_analysis);
        let a = d1.analysis.expect("应带回分析");
        assert_eq!(a.mvp_suggestion, "先抽 crate");

        // 未分析时是 None 而非空对象：前端据此显示"展开分析"按钮
        let d2 = opportunity_detail(&c, "o2").unwrap();
        assert!(!d2.item.has_analysis);
        assert!(d2.analysis.is_none());
    }

    #[test]
    fn list_marks_analyzed_without_n_plus_one_reads() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        c.db.opportunities().upsert(&opportunity("o2", OpportunityStatus::New, 3)).unwrap();
        c.db
            .opportunities()
            .set_analysis(
                "o1",
                &OpportunityAnalysis {
                    opportunity_id: "o1".into(),
                    rationale: "x".into(),
                    reusable: vec![],
                    to_build: vec![],
                    mvp_suggestion: "y".into(),
                    scaffold: vec![],
                },
            )
            .unwrap();

        let page = list_opportunities(&c, &OpportunityListQuery::default()).unwrap();
        let by_id = |id: &str| page.items.iter().find(|i| i.id == id).unwrap();
        // 列表侧的 has_analysis 必须与详情一致（走的是 ids_with_analysis 批量查询）
        assert!(by_id("o1").has_analysis);
        assert!(!by_id("o2").has_analysis);
    }

    #[test]
    fn set_analysis_promotes_new_to_explored() {
        let c = seeded();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        c.db
            .opportunities()
            .set_analysis(
                "o1",
                &OpportunityAnalysis {
                    opportunity_id: "o1".into(),
                    rationale: "x".into(),
                    reusable: vec![],
                    to_build: vec![],
                    mvp_suggestion: "y".into(),
                    scaffold: vec![],
                },
            )
            .unwrap();
        // 生成分析 = 用户已展开查看，状态应自动从 new 前进到 explored
        let d = opportunity_detail(&c, "o1").unwrap();
        assert_eq!(d.item.status, "explored");
        assert!(d.item.actionable);
    }

    #[test]
    fn opportunity_detail_of_missing_is_404() {
        let c = seeded();
        assert!(matches!(
            opportunity_detail(&c, "ghost").unwrap_err(),
            ServiceError::NotFound(_)
        ));
    }

    // ── 汇总 ────────────────────────────────────────────────────

    #[test]
    fn summary_counts_are_real() {
        let c = seeded();
        c.db
            .insights()
            .upsert(&insight("i1", InsightType::DuplicateCapability, 0.9, 2))
            .unwrap();
        c.db.opportunities().upsert(&opportunity("o1", OpportunityStatus::New, 4)).unwrap();
        c.db.opportunities().upsert(&opportunity("o2", OpportunityStatus::Dismissed, 3)).unwrap();

        let s = summary(&c).unwrap();
        assert_eq!(s.insight_total, 1);
        assert_eq!(s.insight_unread, 1);
        assert_eq!(s.opportunity_total, 2);
        assert_eq!(s.opportunity_actionable, 1);
        assert!(s.adoption.rate.is_none());
        assert!(s.by_type.iter().any(|t| t.value == "duplicate_capability" && t.count == 1));
    }

    #[test]
    fn summary_on_empty_db_is_all_zero() {
        let c = ctx();
        let s = summary(&c).unwrap();
        assert_eq!(s.insight_total, 0);
        assert_eq!(s.opportunity_total, 0);
        assert!(s.adoption.rate.is_none());
        // 空状态返回 0 而非报错，前端才能渲染引导页
    }

    #[test]
    fn empty_db_list_is_ok() {
        let c = ctx();
        let page = list(&c, &InsightListQuery::default()).unwrap();
        assert_eq!(page.total, 0);
        assert!(page.items.is_empty());
        assert_eq!(page.facets.len(), InsightType::all().len());
    }

    /// 活动流（jobs 模块导出的同名函数的本地版本，避免测试跨模块耦合）。
    fn activities(c: &ServiceContext) -> Result<Vec<crate::jobs::ActivityView>, ServiceError> {
        crate::jobs::activities(c, None)
    }
}
