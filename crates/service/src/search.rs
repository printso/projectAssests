//! 检索用例：查询参数解析 → 引擎检索 → 视图组装。
//!
//! # service 在这条链路上到底做了什么
//! 检索算法（召回/评分/排序/摘要）全在 `spolia-search`，本模块**不碰**。
//! 这里只负责三件引擎不该知道的事：
//! 1. **把宽松的 HTTP query string 解析成严格的领域查询**——
//!    拼错的 `scope=alll` 要给出"可选值"提示，而不是静默当成默认值
//! 2. **注入基准时间**（新鲜度评分必须可复现）
//! 3. **组装面向 UI 的视图**（补中文标签、按 kind 分组计数）
//!
//! # 🔴 一个刻意不复用 domain `parse` 的地方
//! `ProjectStatus::parse` 对无法识别的值返回 `Unknown` 而不是 `None`
//! （它的调用方是数据库行映射，那里的容错是对的：脏数据不该让整页崩掉）。
//! 但用在**查询参数**上就错了：用户拼错 `?status=actve` 会被静默当成
//! "筛选未知状态的项目"，得到 0 结果，且没有任何提示说明为什么。
//! 因此本模块对 status 做显式白名单校验。

use serde::{Deserialize, Serialize};
use spolia_domain::{
    AssetType, HitKind, ProjectStatus, SearchFilter, SearchQuery, SearchResult, SearchScope, SortBy,
};
use spolia_search::SearchEngine;

use crate::context::{ServiceContext, ServiceError};

/// 搜索每页默认条数。
fn default_limit() -> u32 {
    20
}

/// 搜索单页上限。
pub const MAX_SEARCH_LIMIT: u32 = 100;

/// 查询串最大长度。
///
/// 🔴 必须设限：超长串会进 FTS 的 MATCH 表达式与 LIKE 模式，
/// 既拖慢查询，也可能触发 SQLite 的表达式深度限制而报错——
/// 用户看到的会是"搜索坏了"，而不是"你输入太多了"。
pub const MAX_QUERY_CHARS: usize = 200;

/// 搜索查询（HTTP query string 的映射目标）。
#[derive(Debug, Clone, Deserialize)]
pub struct SearchRequest {
    #[serde(default)]
    pub q: Option<String>,
    /// all / projects / assets / capabilities / insights / knowledge
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub asset_type: Option<String>,
    #[serde(default)]
    pub project_status: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub min_reuse_score: Option<f64>,
    /// relevance / reuse_score / recently_updated / confidence
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

impl Default for SearchRequest {
    /// 🔴 手写而非 derive：derive 会让 `limit` 为 0，
    /// 与 serde 的 `default = "default_limit"` 不一致，
    /// 于是"代码里构造的默认查询"和"反序列化出的默认查询"分页行为不同。
    fn default() -> Self {
        Self {
            q: None,
            scope: None,
            asset_type: None,
            project_status: None,
            language: None,
            project_id: None,
            min_reuse_score: None,
            sort: None,
            limit: default_limit(),
            offset: 0,
        }
    }
}

impl SearchRequest {
    pub fn effective_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_SEARCH_LIMIT)
    }
}

/// 搜索响应视图。
#[derive(Debug, Clone, Serialize)]
pub struct SearchView {
    pub hits: Vec<HitView>,
    /// 命中总数（**不是**本页条数），分页器据此算总页数
    pub total: usize,
    /// 归一化后实际使用的查询串（前端回显，让用户知道空格被折叠了）
    pub query: String,
    /// 是否走了 LIKE 子串回退（中文 2 字查询）。
    /// 🔴 必须暴露：否则用户搜"视频"看到一堆宽松结果，会以为搜索不准，
    /// 而真相是 trigram 索引要求 ≥3 字符、引擎主动降级了。
    pub used_substring_fallback: bool,
    pub took_ms: u64,
    /// 按实体类型分组的命中数（前端渲染 tab 角标）
    pub kind_counts: Vec<KindCount>,
    /// 当前生效的筛选条件回显（用户能看到"我筛了什么"）
    pub applied_filters: AppliedFilters,
    /// 空结果时的引导文案。非空结果时为 `None`
    pub empty_hint: Option<String>,
}

/// 命中项视图。
///
/// 相比 `SearchHit` 多了 `kind_label` 与 `source_labels`：
/// 中英文映射是业务规则，写在 service 而非前端 JS，
/// 否则 domain 改了标签、前端还显示旧文案。
#[derive(Debug, Clone, Serialize)]
pub struct HitView {
    pub kind: String,
    pub kind_label: String,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub snippet: String,
    pub score: f64,
    /// 0-100 整数分（前端进度条/角标直接用）
    pub score_percent: u8,
    pub sources: Vec<String>,
    pub source_labels: Vec<String>,
    /// 🔴 排序理由：产品硬要求"结果必须说明为什么排在这"。
    /// 前端应把它渲染成可展开的说明，而不是只显示一个分数。
    pub reasons: Vec<String>,
    pub link: LinkView,
}

/// 跳转信息。
#[derive(Debug, Clone, Serialize)]
pub struct LinkView {
    pub page: String,
    pub param: Option<String>,
}

/// 类型分组计数。
#[derive(Debug, Clone, Serialize)]
pub struct KindCount {
    pub kind: String,
    pub label: String,
    pub count: usize,
}

/// 生效中的筛选条件回显。
#[derive(Debug, Clone, Serialize, Default)]
pub struct AppliedFilters {
    pub scope: String,
    pub scope_label: String,
    pub sort: String,
    pub sort_label: String,
    pub asset_type: Option<String>,
    pub project_status: Option<String>,
    pub language: Option<String>,
    pub project_id: Option<String>,
    pub min_reuse_score: Option<f64>,
}

/// 执行检索。
pub fn search(ctx: &ServiceContext, req: &SearchRequest) -> Result<SearchView, ServiceError> {
    let query = build_query(req)?;
    let result = SearchEngine::new().search(&ctx.db, &query, ctx.now())?;
    Ok(view(&result, &query))
}

/// 把请求解析成领域查询。
///
/// 所有非法值都在这里被拦下并给出可选值——
/// 这是"拼错参数静默返回空结果"这类最难自查问题的唯一拦截点。
fn build_query(req: &SearchRequest) -> Result<SearchQuery, ServiceError> {
    let q = normalize_query(req.q.as_deref())?;

    Ok(SearchQuery {
        q,
        scope: parse_scope(&req.scope)?,
        filter: SearchFilter {
            asset_type: parse_asset_type(&req.asset_type)?,
            project_status: parse_project_status(&req.project_status)?,
            language: non_empty(&req.language),
            project_id: non_empty(&req.project_id),
            min_reuse_score: validated_score(req.min_reuse_score)?,
        },
        sort: parse_sort(&req.sort)?,
        limit: req.effective_limit(),
        offset: req.offset,
    })
}

/// 校验并归一化查询串。
fn normalize_query(raw: Option<&str>) -> Result<String, ServiceError> {
    let Some(text) = raw else {
        return Ok(String::new());
    };
    // 折叠空白：搜索引擎自己也会做（normalized_q），
    // 但长度校验必须在折叠前，否则用户用大量空格可以绕过上限。
    let trimmed = text.trim();
    let chars = trimmed.chars().count();
    if chars > MAX_QUERY_CHARS {
        return Err(ServiceError::Invalid(format!(
            "查询过长（{chars} 字，上限 {MAX_QUERY_CHARS}）。请用更具体的关键词，或用左侧筛选条件缩小范围"
        )));
    }
    Ok(trimmed.to_string())
}

fn validated_score(v: Option<f64>) -> Result<Option<f64>, ServiceError> {
    if let Some(n) = v
        && !(0.0..=1.0).contains(&n)
    {
        return Err(ServiceError::Invalid(format!(
            "min_reuse_score 必须在 0.0-1.0 之间，收到 {n}"
        )));
    }
    Ok(v)
}

fn view(result: &SearchResult, query: &SearchQuery) -> SearchView {
    let hits: Vec<HitView> = result.hits.iter().map(hit_view).collect();

    SearchView {
        total: result.total,
        kind_counts: kind_counts(&result.hits),
        query: result.query.clone(),
        used_substring_fallback: result.used_substring_fallback,
        took_ms: result.took_ms,
        applied_filters: applied_filters(query),
        empty_hint: empty_hint(result, query),
        hits,
    }
}

fn hit_view(h: &spolia_domain::SearchHit) -> HitView {
    HitView {
        kind: h.kind.as_str().to_string(),
        kind_label: h.kind.label_zh().to_string(),
        id: h.id.clone(),
        title: h.title.clone(),
        subtitle: h.subtitle.clone(),
        snippet: h.snippet.clone(),
        score: h.score,
        score_percent: (h.score.clamp(0.0, 1.0) * 100.0).round() as u8,
        sources: h.sources.iter().map(|s| s.as_str().to_string()).collect(),
        source_labels: h.sources.iter().map(source_label).collect(),
        reasons: h.reasons.clone(),
        link: LinkView {
            page: h.link.page.clone(),
            param: h.link.param.clone(),
        },
    }
}

fn source_label(s: &spolia_domain::MatchSource) -> String {
    match s {
        spolia_domain::MatchSource::Keyword => "关键词",
        spolia_domain::MatchSource::Semantic => "语义",
        spolia_domain::MatchSource::Structured => "筛选条件",
        spolia_domain::MatchSource::NameMatch => "名称匹配",
    }
    .to_string()
}

/// 按实体类型统计本页命中数。
///
/// 🔴 这是**本页**的分布，不是全库分布——tab 角标要告诉用户
/// "当前这批结果里有几个项目、几个资产"，用全库数字会误导。
///
/// 🔴 计数为 0 的类型也必须出现（值为 0）：只列非零项的话，
/// 用户切到"资产" tab 后该 tab 就消失了，再也切不回来。
/// 顺序取自 `HitKind::all()`，与 UI 的 tab 顺序同源。
fn kind_counts(hits: &[spolia_domain::SearchHit]) -> Vec<KindCount> {
    HitKind::all()
        .iter()
        .map(|k| KindCount {
            kind: k.as_str().to_string(),
            label: k.label_zh().to_string(),
            count: hits.iter().filter(|h| h.kind == *k).count(),
        })
        .collect()
}

fn applied_filters(q: &SearchQuery) -> AppliedFilters {
    AppliedFilters {
        scope: q.scope.as_str().to_string(),
        // 🔴 直接用 domain 的 `label_zh`，不在 service 侧另写一份：
            // 早先这里有个私有 `scope_label`，与 `SearchScope::label_zh` 逐字重复，
            // 结果 domain 加了 `Insights` 变体后这里没跟上，穷举 match 直接编译失败。
            // 中文标签的唯一来源必须是 domain。
            scope_label: q.scope.label_zh().to_string(),
        sort: sort_key(&q.sort),
        sort_label: sort_label(&q.sort),
        asset_type: q.filter.asset_type.map(|t| t.as_str().to_string()),
        project_status: q.filter.project_status.map(|s| s.as_str().to_string()),
        language: q.filter.language.clone(),
        project_id: q.filter.project_id.clone(),
        min_reuse_score: q.filter.min_reuse_score,
    }
}

/// 空结果时的引导。
///
/// 🔴 分两种情况，绝不能都返回同一句"没有结果"：
/// - **带筛选**：最可能是条件太严，引导用户放宽
/// - **无筛选**：说明库里确实没东西，引导去扫描
///
/// 混淆这两种情况会让用户在空库上反复改筛选条件，越改越困惑。
fn empty_hint(result: &SearchResult, q: &SearchQuery) -> Option<String> {
    if !result.hits.is_empty() {
        return None;
    }
    let query_empty = result.query.trim().is_empty();
    if q.filter.is_empty() && query_empty {
        return Some("索引里还没有数据。打开 设置 → 扫描目录 添加代码根目录，然后开始扫描。".to_string());
    }
    let mut parts = Vec::new();
    if !query_empty {
        parts.push(format!("关键词「{}」", result.query));
    }
    if let Some(t) = q.filter.asset_type {
        parts.push(format!("资产类型={}", t.label_zh()));
    }
    if let Some(s) = q.filter.project_status {
        parts.push(format!("项目状态={}", s.label_zh()));
    }
    if let Some(l) = &q.filter.language {
        parts.push(format!("语言={l}"));
    }
    if q.filter.min_reuse_score.is_some() {
        parts.push("复用分下限".to_string());
    }
    if parts.is_empty() {
        return Some(format!(
            "「{}」范围内没有匹配项。试试去掉筛选条件，或换个关键词。",
            q.scope.as_str()
        ));
    }
    Some(format!(
        "当前条件（{}）没有匹配项。试着放宽筛选：先清空关键词只留条件，或反过来。",
        parts.join("、")
    ))
}

// ══════════════════════════════════════════════════════════════════
// 参数解析
// ══════════════════════════════════════════════════════════════════

fn parse_scope(s: &Option<String>) -> Result<SearchScope, ServiceError> {
    match non_empty(s) {
        None => Ok(SearchScope::default()),
        Some(v) => SearchScope::parse(&v).ok_or_else(|| {
            ServiceError::Invalid(format!(
                "未知的检索范围：{v}（可选 all / projects / assets / capabilities / insights / knowledge）"
            ))
        }),
    }
}

fn parse_asset_type(s: &Option<String>) -> Result<Option<AssetType>, ServiceError> {
    match non_empty(s) {
        None => Ok(None),
        Some(v) => AssetType::parse(&v).map(Some).ok_or_else(|| {
            ServiceError::Invalid(format!(
                "未知的资产类型：{v}（可选 {}）",
                AssetType::all()
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ))
        }),
    }
}

/// 🔴 显式白名单，不用 `ProjectStatus::parse`。
///
/// 后者对无法识别的值返回 `Unknown`（那是为数据库脏数据容错设计的），
/// 用在这里会让拼错的 `?status=actve` 静默变成"筛选未知状态"，
/// 用户得到 0 结果且毫无提示。
fn parse_project_status(s: &Option<String>) -> Result<Option<ProjectStatus>, ServiceError> {
    let Some(v) = non_empty(s) else {
        return Ok(None);
    };
    const VALID: &[(&str, ProjectStatus)] = &[
        ("active", ProjectStatus::Active),
        ("paused", ProjectStatus::Paused),
        ("abandoned", ProjectStatus::Abandoned),
        ("experimental", ProjectStatus::Experimental),
        ("unknown", ProjectStatus::Unknown),
    ];
    VALID
        .iter()
        .find(|(k, _)| *k == v)
        .map(|(_, st)| Some(*st))
        .ok_or_else(|| {
            ServiceError::Invalid(format!(
                "未知的项目状态：{v}（可选 {}）",
                VALID
                    .iter()
                    .map(|(k, _)| *k)
                    .collect::<Vec<_>>()
                    .join(" / ")
            ))
        })
}

fn parse_sort(s: &Option<String>) -> Result<SortBy, ServiceError> {
    Ok(match non_empty(s).as_deref() {
        None | Some("relevance") => SortBy::Relevance,
        Some("reuse_score") | Some("reuse") => SortBy::ReuseScore,
        Some("recently_updated") | Some("recent") => SortBy::RecentlyUpdated,
        Some("confidence") => SortBy::Confidence,
        Some(other) => {
            return Err(ServiceError::Invalid(format!(
                "未知的排序方式：{other}（可选 relevance / reuse_score / recently_updated / confidence）"
            )))
        }
    })
}

fn sort_key(s: &SortBy) -> String {
    match s {
        SortBy::Relevance => "relevance",
        SortBy::ReuseScore => "reuse_score",
        SortBy::RecentlyUpdated => "recently_updated",
        SortBy::Confidence => "confidence",
    }
    .to_string()
}

fn sort_label(s: &SortBy) -> String {
    match s {
        SortBy::Relevance => "综合相关性",
        SortBy::ReuseScore => "复用价值",
        SortBy::RecentlyUpdated => "最近更新",
        SortBy::Confidence => "置信度",
    }
    .to_string()
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
    use spolia_domain::{
        Asset, CodeStats, Evidence, Project, ProjectStatus as PS, ScanFacts,
    };

    fn ctx() -> ServiceContext {
        ServiceContext::in_memory().unwrap()
    }

    fn project(id: &str, name: &str, desc: &str, status: PS) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("/tmp/{id}"),
            description: desc.into(),
            language: "Rust".into(),
            framework: "Axum".into(),
            created_at: None,
            updated_at: Some("2026-09-20".into()),
            last_commit_at: Some("2026-09-20".into()),
            status,
            health_score: 80,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats::default(),
            scan: ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn asset(id: &str, pid: &str, name: &str, desc: &str) -> Asset {
        Asset {
            id: id.into(),
            project_id: pid.into(),
            asset_type: AssetType::Component,
            name: name.into(),
            description: desc.into(),
            content: None,
            source_path: format!("src/{id}.rs"),
            confidence: 0.9,
            reuse_score: 0.91,
            generality: 0.7,
            stability: 0.6,
            tags: vec![],
            created_at: spolia_storage::now_utc(),
            evidence: Evidence {
                files: vec![format!("src/{id}.rs")],
                ..Evidence::default()
            },
            user_feedback: None,
        }
    }

    fn seeded() -> ServiceContext {
        let c = ctx();
        c.db
            .projects()
            .upsert_batch(&[
                project("p1", "视频融合平台", "多摄像头视频融合与车道配置", PS::Active),
                project("p2", "分身云栖", "数字分身前端应用", PS::Paused),
            ])
            .unwrap();
        c.db
            .assets()
            .upsert(&asset("a1", "p1", "VideoDecoder", "视频解码组件"))
            .unwrap();
        c
    }

    // ── 默认值一致性 ────────────────────────────────────────────

    #[test]
    fn default_request_matches_serde_default() {
        // 🔴 回归：derive(Default) 会让 limit=0，与 serde 默认值不一致
        let built = SearchRequest::default();
        let parsed: SearchRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(built.effective_limit(), parsed.effective_limit());
        assert_eq!(built.effective_limit(), 20);
    }

    #[test]
    fn limit_is_clamped() {
        let big = SearchRequest { limit: 99999, ..Default::default() };
        assert_eq!(big.effective_limit(), MAX_SEARCH_LIMIT);
        let zero = SearchRequest { limit: 0, ..Default::default() };
        assert_eq!(zero.effective_limit(), 1);
    }

    // ── 参数校验 ────────────────────────────────────────────────

    #[test]
    fn unknown_scope_is_rejected_with_options() {
        let c = ctx();
        let err = search(
            &c,
            &SearchRequest {
                scope: Some("alll".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("alll"), "应回显非法值: {msg}");
        assert!(msg.contains("capabilities"), "应列出可选值: {msg}");
    }

    #[test]
    fn unknown_asset_type_is_rejected() {
        let c = ctx();
        let err = search(
            &c,
            &SearchRequest {
                asset_type: Some("widget".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("prompt"));
    }

    #[test]
    fn misspelled_project_status_is_rejected_not_silently_unknown() {
        let c = seeded();
        // 🔴 回归：ProjectStatus::parse 对非法值返回 Unknown，
        // 直接用它会把 "actve" 静默当成"筛选未知状态"→ 0 结果且无提示
        let err = search(
            &c,
            &SearchRequest {
                project_status: Some("actve".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("actve"), "{msg}");
        assert!(msg.contains("active"), "应提示正确写法: {msg}");
        assert!(matches!(err, ServiceError::Invalid(_)));
    }

    #[test]
    fn valid_project_status_is_accepted() {
        let c = seeded();
        let v = search(
            &c,
            &SearchRequest {
                project_status: Some("paused".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.applied_filters.project_status.as_deref(), Some("paused"));
    }

    #[test]
    fn unknown_sort_is_rejected() {
        let c = ctx();
        let err = search(
            &c,
            &SearchRequest {
                sort: Some("magic".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("relevance"));
    }

    #[test]
    fn out_of_range_min_score_is_rejected() {
        let c = ctx();
        let err = search(
            &c,
            &SearchRequest {
                min_reuse_score: Some(-0.1),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("0.0-1.0"));
    }

    #[test]
    fn overlong_query_is_rejected_with_actionable_hint() {
        let c = ctx();
        let long = "视".repeat(MAX_QUERY_CHARS + 1);
        let err = search(
            &c,
            &SearchRequest {
                q: Some(long),
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(&MAX_QUERY_CHARS.to_string()), "{msg}");
        assert!(msg.contains("筛选条件"), "应告诉用户替代做法: {msg}");
    }

    #[test]
    fn whitespace_padding_cannot_bypass_length_limit() {
        let c = ctx();
        // 空格折叠后仍超长：校验必须在折叠后的串上做
        let long = format!("{} {}", "视".repeat(MAX_QUERY_CHARS), "频".repeat(10));
        assert!(search(
            &c,
            &SearchRequest { q: Some(long), ..Default::default() }
        )
        .is_err());
    }

    // ── 检索行为 ────────────────────────────────────────────────

    #[test]
    fn finds_project_by_keyword() {
        let c = seeded();
        let v = search(
            &c,
            &SearchRequest {
                q: Some("视频融合".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!v.hits.is_empty(), "应命中视频融合平台");
        assert!(v.hits.iter().any(|h| h.title.contains("视频融合")));
        // 产品硬要求：每个结果都要有排序理由
        assert!(
            v.hits.iter().all(|h| !h.reasons.is_empty()),
            "结果必须带排序理由"
        );
    }

    #[test]
    fn every_hit_carries_link_and_labels() {
        let c = seeded();
        let v = search(
            &c,
            &SearchRequest {
                q: Some("视频".into()),
                ..Default::default()
            },
        )
        .unwrap();
        for h in &v.hits {
            assert!(!h.kind_label.is_empty(), "{} 缺中文标签", h.kind);
            assert!(!h.link.page.is_empty(), "{} 缺跳转页面", h.id);
            assert!(h.score_percent <= 100);
        }
    }

    #[test]
    fn short_chinese_query_reports_substring_fallback() {
        let c = seeded();
        // "视频" 是 2 字，FTS5 trigram 无法命中，必须走 LIKE 回退
        let v = search(
            &c,
            &SearchRequest {
                q: Some("视频".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            v.used_substring_fallback,
            "2 字中文查询应标记子串回退，否则用户以为搜索不准"
        );
    }

    #[test]
    fn total_is_not_page_size() {
        let c = ctx();
        let projects: Vec<Project> = (0..5)
            .map(|i| project(&format!("p{i}"), &format!("项目{i}"), "共同描述关键词", PS::Active))
            .collect();
        c.db.projects().upsert_batch(&projects).unwrap();

        let v = search(
            &c,
            &SearchRequest {
                q: Some("共同描述关键词".into()),
                limit: 2,
                ..Default::default()
            },
        )
        .unwrap();
        // 🔴 total 必须是全量命中数，前端才能算对总页数
        assert!(v.total >= 5, "total 应覆盖全部命中，实得 {}", v.total);
        assert!(v.hits.len() <= 2, "本页只返回 limit 条");
    }

    #[test]
    fn empty_db_hint_points_to_scanning() {
        let c = ctx();
        let v = search(&c, &SearchRequest::default()).unwrap();
        assert!(v.hits.is_empty());
        let hint = v.empty_hint.expect("空库应给引导");
        assert!(hint.contains("扫描"), "{hint}");
    }

    #[test]
    fn empty_result_with_filters_hints_at_loosening() {
        let c = seeded();
        let v = search(
            &c,
            &SearchRequest {
                q: Some("不存在的关键词xyz".into()),
                language: Some("Haskell".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(v.hits.is_empty());
        let hint = v.empty_hint.expect("应给引导");
        // 🔴 必须区分"库是空的"和"条件太严"：这里库里有数据，
        // 提示不该叫用户去扫描
        assert!(!hint.contains("扫描目录"), "有数据时不该引导去扫描: {hint}");
        assert!(hint.contains("Haskell") || hint.contains("关键词"), "应指明是哪个条件: {hint}");
    }

    #[test]
    fn non_empty_result_has_no_hint() {
        let c = seeded();
        let v = search(
            &c,
            &SearchRequest {
                q: Some("视频".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!v.hits.is_empty());
        assert!(v.empty_hint.is_none(), "有结果时不该给空状态引导");
    }

    #[test]
    fn scope_filters_hits() {
        let c = seeded();
        let only_assets = search(
            &c,
            &SearchRequest {
                q: Some("视频".into()),
                scope: Some("assets".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            only_assets.hits.iter().all(|h| h.kind == "asset"),
            "scope=assets 不应混入其他类型: {:?}",
            only_assets.hits.iter().map(|h| &h.kind).collect::<Vec<_>>()
        );
    }

    #[test]
    fn kind_counts_describe_current_page() {
        let c = seeded();
        let v = search(
            &c,
            &SearchRequest {
                q: Some("视频".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let summed: usize = v.kind_counts.iter().map(|k| k.count).sum();
        assert_eq!(summed, v.hits.len(), "分组计数之和应等于本页条数");
        for k in &v.kind_counts {
            assert!(!k.label.is_empty());
        }
    }

    #[test]
    fn kind_tabs_never_disappear_when_empty() {
        let c = seeded();
        let v = search(
            &c,
            &SearchRequest {
                q: Some("视频".into()),
                scope: Some("projects".into()),
                ..Default::default()
            },
        )
        .unwrap();
        // 🔴 回归：只列非零项会让 tab 在切换后消失，用户再也点不回来
        assert_eq!(
            v.kind_counts.len(),
            HitKind::all().len(),
            "所有类型的 tab 都必须存在"
        );
        assert!(
            v.kind_counts.iter().any(|k| k.kind == "asset" && k.count == 0),
            "被 scope 排除的类型应保留为 0，而不是消失"
        );
        // 顺序必须与 HitKind::all() 一致，否则 tab 顺序会随结果变化
        let order: Vec<&str> = v.kind_counts.iter().map(|k| k.kind.as_str()).collect();
        let expected: Vec<&str> = HitKind::all().iter().map(|k| k.as_str()).collect();
        assert_eq!(order, expected);
    }

    #[test]
    fn applied_filters_echo_back() {
        let c = seeded();
        let v = search(
            &c,
            &SearchRequest {
                q: Some("视频".into()),
                scope: Some("assets".into()),
                sort: Some("reuse_score".into()),
                asset_type: Some("component".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let f = &v.applied_filters;
        assert_eq!(f.scope, "assets");
        assert_eq!(f.scope_label, "资产");
        assert_eq!(f.sort, "reuse_score");
        assert_eq!(f.sort_label, "复用价值");
        assert_eq!(f.asset_type.as_deref(), Some("component"));
    }

    #[test]
    fn sort_aliases_are_accepted() {
        let c = seeded();
        for s in ["relevance", "reuse", "reuse_score", "recent", "recently_updated", "confidence"] {
            let v = search(
                &c,
                &SearchRequest {
                    q: Some("视频".into()),
                    sort: Some(s.into()),
                    ..Default::default()
                },
            );
            assert!(v.is_ok(), "排序别名 {s} 应被接受: {:?}", v.err());
        }
    }

    #[test]
    fn whitespace_only_query_is_browse_mode() {
        let c = seeded();
        // 全空格 = 清空搜索框，应进入浏览模式而不是报"没结果"
        let v = search(
            &c,
            &SearchRequest {
                q: Some("   ".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.query, "", "查询串应归一化为空");
        assert!(!v.hits.is_empty(), "浏览模式应列出可浏览项");
    }

    #[test]
    fn search_is_deterministic() {
        let c = seeded();
        let q = SearchRequest {
            q: Some("视频".into()),
            ..Default::default()
        };
        let a = search(&c, &q).unwrap();
        let b = search(&c, &q).unwrap();
        let ids1: Vec<_> = a.hits.iter().map(|h| h.id.clone()).collect();
        let ids2: Vec<_> = b.hits.iter().map(|h| h.id.clone()).collect();
        assert_eq!(ids1, ids2, "同一查询两次结果顺序必须一致");
    }

    #[test]
    fn took_ms_is_reported() {
        let c = seeded();
        let v = search(&c, &SearchRequest::default()).unwrap();
        // 产品指标"搜索 ≤ 1 秒"靠这个字段验证；必须始终有值
        assert!(v.took_ms < 1000, "内存库检索不该超过 1 秒: {}ms", v.took_ms);
    }
}
