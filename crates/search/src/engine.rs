//! 检索引擎：编排召回 → 过滤 → 评分 → 排序 → 分页。
//!
//! # 依赖方向
//! 本 crate 依赖 `spolia-storage` 取数据，但**只通过 `RetrievalRepo` 与各 Repository**：
//! 引擎自身不出现任何 SQL。换检索后端（例如接向量库）时，
//! 改动限于 storage 的 `fts.rs`，本文件与上层 API 都不受影响。
//!
//! # 三步流水线的职责边界
//! 1. **召回**（storage）：给出候选 id + 相关性分数，负责 FTS/LIKE 的选择
//! 2. **过滤与评分**（本模块）：应用结构化过滤条件、混合质量与新鲜度、生成理由
//! 3. **排序与分页**（本模块）：确定性排序，绝不依赖数据库返回顺序
//!
//! # 产品红线
//! - 搜索结果**必须有排序理由**（见 `ranking::RankInput::reasons`）
//! - 搜索响应 ≤ 1 秒（`took_ms` 用于验证该指标）
//! - 短中文查询不得静默返回空（LIKE 回退 + `used_substring_fallback` 告知前端）
//! - 无证据的资产不展示（浏览模式下由 `evidence_required` 保证）

use std::collections::HashMap;
use std::time::Instant;

use spolia_domain::{
    Asset, Capability, HitKind, HitLink, Insight, MatchSource, Opportunity, Project, ReuseTier,
    SearchHit, SearchQuery, SearchResult, SearchScope, SortBy, SpoliaError, UserFeedback,
};
use spolia_storage::{
    needs_substring_fallback, AssetFilter, AssetSort, Candidate, Database, InsightFilter,
    OpportunityFilter, ProjectFilter, ProjectSort, RetrievalRepo,
};

use crate::ranking::{
    asset_quality, capability_quality, explicit_sort_key, insight_quality, opportunity_quality,
    project_quality, recency_score, RankInput, SortFacts,
};
use crate::snippet;

/// 每类实体的召回上限。
///
/// 三类实体各召回这么多，合并后再排序取 `limit`。
/// 不能只召回 `limit` 条：否则"综合排序"退化成"按实体类型分批"，
/// 高相关的资产可能因为项目先占满名额而完全不出现。
const RECALL_PER_KIND: u32 = 60;

/// 浏览模式（空查询）每类实体的候选池大小。
///
/// 与 `RECALL_PER_KIND` 一样必须固定：池大小随 `offset` 变化会让 `total`
/// 逐页增长，前端分页器永远算不准总页数。
///
/// 取 200 而非更大：浏览模式是"随便看看"，用户极少翻到第 10 页之后；
/// 真要逐条浏览全部资产应走资产页的列表接口（那里有真正的分页查询）。
const BROWSE_LIMIT: usize = 200;

/// 名称命中的加权幅度。
///
/// 用户搜项目名时期望它排第一；但 FTS 的 bm25 只看词频，
/// "名称命中一次"可能不如"描述里命中五次"，与用户直觉相反。
pub const NAME_BOOST: f64 = 0.25;

/// 检索引擎。无状态，按需构造。
#[derive(Debug, Clone, Default)]
pub struct SearchEngine;

/// 内部排序单元：命中项 + 显式排序主键。
///
/// 🔴 `sort_key` 刻意**不放进 `SearchHit`**：它是排序过程的中间量，
/// 属于实现细节。泄漏到 API 响应会让前端误以为可以依赖它，
/// 而它随排序方式变化，是个不稳定契约。
struct Scored {
    hit: SearchHit,
    /// 用户显式排序时的主键；相关性排序下为 `None`
    sort_key: Option<f64>,
}

/// 资产所属项目的摘要信息。
///
/// 资产自身不带语言与时间线（`created_at` 是抽取时间，不代表代码新旧），
/// 这两项过滤与排序都必须借助所属项目。批量取回避免 N+1 查询。
#[derive(Debug, Clone)]
struct ProjectBrief {
    name: String,
    language: String,
    days_since_update: Option<i64>,
}

impl SearchEngine {
    pub fn new() -> Self {
        Self
    }

    /// 执行检索。
    ///
    /// `now` 注入而非读时钟：新鲜度评分必须可复现，
    /// 否则同一查询跨午夜会得到不同排序，快照测试与用户认知都会失效。
    pub fn search(
        &self,
        db: &Database,
        query: &SearchQuery,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<SearchResult, SpoliaError> {
        let started = Instant::now();
        let q = query.normalized_q();
        let limit = query.effective_limit() as usize;
        let offset = query.offset as usize;
        let terms = query_terms(&q);

        let mut scored: Vec<Scored> = if q.is_empty() {
            // 空查询 = 纯浏览模式：不做关键词召回，直接按过滤条件列出高质量项。
            // 这条路径必须存在，否则用户清空搜索框会看到"0 结果"，
            // 而正确行为是展示可浏览的资产/项目列表。
            self.browse(db, query, &terms, now)?
        } else {
            self.recall_all(db, query, &q, &terms, now)?
        };

        // 排序：用户显式选择的维度优先，否则按综合分
        sort(&mut scored, query.sort);

        let total = scored.len();
        // 分页必须在排序之后，否则每页内容随数据库返回顺序漂移
        let hits: Vec<SearchHit> = scored
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|s| s.hit)
            .collect();

        // 必须在构造 SearchResult 之前算好：`query: q` 会移动 q，
        // 之后再借用 &q 就是"使用已移动的值"。
        let used_substring_fallback = !q.is_empty() && needs_substring_fallback(&q);

        Ok(SearchResult {
            hits,
            total,
            query: q,
            // 告知前端"短查询按子串匹配"，避免用户困惑于结果偏多
            used_substring_fallback,
            took_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// 关键词检索：三类实体各自召回后合并。
    fn recall_all(
        &self,
        db: &Database,
        query: &SearchQuery,
        q: &str,
        terms: &[String],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Scored>, SpoliaError> {
        let mut out: Vec<Scored> = Vec::new();
        let retrieval = db.retrieval();

        if scope_includes(query.scope, SearchScope::Projects) {
            let cands = retrieval.projects(q, RECALL_PER_KIND)?;
            let projects = retrieval.projects_by_ids(&ids_of(&cands))?;
            for p in projects {
                if !project_matches(&p, query) {
                    continue;
                }
                let cand = find_cand(&cands, &p.id);
                let input = RankInput {
                    relevance: cand_score(&cand),
                    quality: project_quality(&p),
                    recency: recency_score(p.days_since_update(now)),
                    from_substring: cand.map(|c| c.from_substring).unwrap_or(false),
                };
                out.push(project_scored(&p, input, terms, query.sort));
            }
        }

        if scope_includes(query.scope, SearchScope::Assets) {
            let cands = retrieval.assets(q, RECALL_PER_KIND)?;
            let assets = retrieval.assets_by_ids(&ids_of(&cands))?;
            let briefs = project_briefs(&retrieval, &assets, now)?;
            for a in assets {
                if !asset_matches(&a, query, &briefs) {
                    continue;
                }
                let cand = find_cand(&cands, &a.id);
                let brief = briefs.get(&a.project_id);
                let input = RankInput {
                    relevance: cand_score(&cand),
                    quality: asset_quality(&a),
                    // 资产无独立时间线：用所属项目的新鲜度。
                    // 项目活跃 → 其资产更可能仍然适用；
                    // 找不到项目（已删除）时给 0，不编造中位数。
                    recency: recency_score(brief.and_then(|b| b.days_since_update)),
                    from_substring: cand.map(|c| c.from_substring).unwrap_or(false),
                };
                out.push(asset_scored(&a, input, terms, brief.map(|b| b.name.clone()), query.sort));
            }
        }

        if scope_includes(query.scope, SearchScope::Capabilities) {
            let cands = retrieval.capabilities(q, RECALL_PER_KIND)?;
            let caps = retrieval.capabilities_by_ids(&ids_of(&cands))?;
            for c in caps {
                let cand = find_cand(&cands, &c.id);
                let input = RankInput {
                    relevance: cand_score(&cand),
                    quality: capability_quality(&c),
                    recency: 0.0, // 能力是跨项目聚合概念，没有时间线
                    from_substring: cand.map(|x| x.from_substring).unwrap_or(false),
                };
                out.push(capability_scored(&c, input, terms));
            }
        }

        // 🔴 洞察与机会：这是补齐的核心缺口。
        //
        // 它们此前完全不在召回范围内，直接导致对话式分析师答不出
        // 库里明明有的结论——用户问"我有哪些重复实现的代码？"，
        // 库里存着标题为"你在 2 个项目中重复实现了 Task Queue"的洞察，
        // 却因为搜不到而回答"没有找到相关记录"。
        //
        // 洞察/机会是**结论性**内容，用户提问时最想拿到的正是结论，
        // 而不是让他自己在几十个资产里翻找。
        //
        // 两类实体共用 `SearchScope::Insights` 这一个范围：
        // 它们在 UI 上是同一个页面，用户的心智模型是"系统给我的结论"。
        if scope_includes(query.scope, SearchScope::Insights) {
            // ── 洞察 ──
            let cands = retrieval.insights(q, RECALL_PER_KIND)?;
            let insights = retrieval.insights_by_ids(&ids_of(&cands))?;
            for i in insights {
                let cand = find_cand(&cands, &i.id);
                // 洞察有真实时间线：created_at 是"这条结论何时得出"，
                // 语义上正是它的时效（与资产的 created_at=抽取时间不同）。
                let recency =
                    recency_score(spolia_domain::days_since(Some(&i.created_at), now));
                let input = RankInput {
                    relevance: cand_score(&cand),
                    quality: insight_quality(&i),
                    recency,
                    from_substring: cand.map(|x| x.from_substring).unwrap_or(false),
                };
                out.push(insight_scored(&i, input, terms, query.sort));
            }

            // ── 机会 ──
            let cands = retrieval.opportunities(q, RECALL_PER_KIND)?;
            let opps = retrieval.opportunities_by_ids(&ids_of(&cands))?;
            for o in opps {
                let cand = find_cand(&cands, &o.id);
                let recency =
                    recency_score(spolia_domain::days_since(Some(&o.created_at), now));
                let input = RankInput {
                    relevance: cand_score(&cand),
                    quality: opportunity_quality(&o),
                    recency,
                    from_substring: cand.map(|x| x.from_substring).unwrap_or(false),
                };
                out.push(opportunity_scored(&o, input, terms, query.sort));
            }
        }

        Ok(out)
    }

    /// 浏览模式（空查询）：按过滤条件列出高质量项。
    fn browse(
        &self,
        db: &Database,
        query: &SearchQuery,
        terms: &[String],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Scored>, SpoliaError> {
        let mut out: Vec<Scored> = Vec::new();
        // 🔴 候选池大小必须**固定**，不能是 `limit + offset`。
        // 若随 offset 增长，`total` 就会逐页变大（第 1 页报 3 条、第 2 页报 6 条），
        // 前端分页器的总页数每翻一次都变，用户无法判断"到底有多少条"。
        // 与关键词路径的 RECALL_PER_KIND 同理：召回池固定，分页在池上切片。
        let want = BROWSE_LIMIT;

        if scope_includes(query.scope, SearchScope::Projects) {
            let filter = ProjectFilter {
                status: query.filter.project_status,
                language: query.filter.language.clone(),
                limit: Some(want as u32),
                ..Default::default()
            };
            for p in db.projects().list(&filter, ProjectSort::RecentlyUpdated)? {
                let input = RankInput {
                    relevance: 0.0, // 浏览模式无关键词命中
                    quality: project_quality(&p),
                    recency: recency_score(p.days_since_update(now)),
                    from_substring: false,
                };
                out.push(project_scored(&p, input, terms, query.sort));
            }
        }

        if scope_includes(query.scope, SearchScope::Assets) {
            let filter = AssetFilter {
                asset_type: query.filter.asset_type,
                project_id: query.filter.project_id.clone(),
                min_reuse_score: query.filter.min_reuse_score,
                // 产品红线：无证据的资产不展示
                evidence_required: true,
                limit: Some(want as u32),
                ..Default::default()
            };
            let assets = db.assets().list(&filter, AssetSort::ReuseScore)?;
            let briefs = project_briefs(&db.retrieval(), &assets, now)?;
            for a in assets {
                // list 已按 filter 过滤类型/项目/分数，此处只需补语言过滤
                // （资产不带语言字段，必须借所属项目判定）
                if let Some(lang) = &query.filter.language {
                    let matches = briefs
                        .get(&a.project_id)
                        .is_some_and(|b| b.language.eq_ignore_ascii_case(lang))
                        || a.tags.iter().any(|t| t.eq_ignore_ascii_case(lang));
                    if !matches {
                        continue;
                    }
                }
                let brief = briefs.get(&a.project_id);
                let input = RankInput {
                    relevance: 0.0,
                    quality: asset_quality(&a),
                    recency: recency_score(brief.and_then(|b| b.days_since_update)),
                    from_substring: false,
                };
                out.push(asset_scored(&a, input, terms, brief.map(|b| b.name.clone()), query.sort));
            }
        }

        if scope_includes(query.scope, SearchScope::Capabilities) {
            // 能力总量受三层结构约束（数百量级），全量取回再截断即可，
            // 无需为浏览模式单独加一个分页查询
            let mut caps = db.capabilities().list_all()?;
            caps.truncate(want);
            for c in caps {
                let input = RankInput {
                    relevance: 0.0,
                    quality: capability_quality(&c),
                    recency: 0.0,
                    from_substring: false,
                };
                out.push(capability_scored(&c, input, terms));
            }
        }

        // 🔴 浏览模式同样要覆盖洞察与机会：
        // 用户在搜索框清空后切到"洞察"范围，期望看到结论列表而非空白。
        // 关键词路径（recall_all）和浏览路径（browse）必须覆盖同一组实体，
        // 否则会出现"搜'队列'能搜到洞察，但清空搜索框反而看不到"的矛盾。
        if scope_includes(query.scope, SearchScope::Insights) {
            // 洞察按置信度降序（repo 的 list 默认口径），取前 want 条
            let insight_filter = InsightFilter {
                limit: Some(want as u32),
                ..Default::default()
            };
            for i in db.insights().list(&insight_filter)? {
                let recency = recency_score(spolia_domain::days_since(Some(&i.created_at), now));
                let input = RankInput {
                    relevance: 0.0, // 浏览模式无关键词命中
                    quality: insight_quality(&i),
                    recency,
                    from_substring: false,
                };
                out.push(insight_scored(&i, input, terms, query.sort));
            }

            // 机会：浏览模式列出**可操作**的（new + explored），
            // 已忽略/已采纳的不默认展示——与机会页默认视图口径一致。
            let mut opp_filter = OpportunityFilter::actionable();
            opp_filter.limit = Some(want as u32);
            for o in db.opportunities().list(&opp_filter)? {
                let recency = recency_score(spolia_domain::days_since(Some(&o.created_at), now));
                let input = RankInput {
                    relevance: 0.0,
                    quality: opportunity_quality(&o),
                    recency,
                    from_substring: false,
                };
                out.push(opportunity_scored(&o, input, terms, query.sort));
            }
        }

        Ok(out)
    }
}

/// 排序：显式维度优先，综合分次之，id 兜底。
///
/// 🔴 必须按 id 兜底：否则同分项的相对顺序取决于召回顺序，
/// 而召回顺序来自数据库存储布局，会随增删变化。
/// 用户会看到"刷新一次顺序就变"，无法建立对结果的信任。
fn sort(items: &mut [Scored], sort_by: SortBy) {
    match sort_by {
        SortBy::Relevance => items.sort_by(|a, b| {
            b.hit
                .score
                .partial_cmp(&a.hit.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.hit.id.cmp(&b.hit.id))
        }),
        // 显式排序时主键来自实体维度；综合分退为次键，id 兜底。
        // sort_key 为 None 表示该实体没有这个维度（如项目没有 reuse_score），
        // 用综合分代替而非 0——否则用户按"复用价值"排序时项目会全部沉底消失。
        _ => items.sort_by(|a, b| {
            let ka = (a.sort_key.unwrap_or(a.hit.score), a.hit.score);
            let kb = (b.sort_key.unwrap_or(b.hit.score), b.hit.score);
            kb.partial_cmp(&ka)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.hit.id.cmp(&b.hit.id))
        }),
    }
}

// ── 召回辅助 ─────────────────────────────────────────────────────

fn ids_of(cands: &[Candidate]) -> Vec<String> {
    cands.iter().map(|c| c.id.clone()).collect()
}

fn find_cand<'a>(cands: &'a [Candidate], id: &str) -> Option<&'a Candidate> {
    cands.iter().find(|c| c.id == id)
}

fn cand_score(cand: &Option<&Candidate>) -> f64 {
    cand.map(|c| c.score).unwrap_or(0.0)
}

/// 批量取回资产所属项目的摘要（名称 + 语言 + 活跃度）。
fn project_briefs(
    retrieval: &RetrievalRepo<'_>,
    assets: &[Asset],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<HashMap<String, ProjectBrief>, SpoliaError> {
    let mut ids: Vec<String> = Vec::new();
    for a in assets {
        if !ids.contains(&a.project_id) {
            ids.push(a.project_id.clone());
        }
    }
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let projects = retrieval.projects_by_ids(&ids)?;
    Ok(projects
        .into_iter()
        .map(|p| {
            (
                p.id.clone(),
                ProjectBrief {
                    name: p.name.clone(),
                    language: p.language.clone(),
                    days_since_update: p.days_since_update(now),
                },
            )
        })
        .collect())
}

/// 检索范围是否包含某类实体。
pub fn scope_includes(scope: SearchScope, target: SearchScope) -> bool {
    scope == SearchScope::All || scope == target
}

/// 查询词切分：按空白拆，用于摘要高亮与名称加权。
pub fn query_terms(q: &str) -> Vec<String> {
    q.split_whitespace().map(str::to_string).collect()
}

fn project_matches(p: &Project, q: &SearchQuery) -> bool {
    if let Some(st) = q.filter.project_status
        && p.status != st
    {
        return false;
    }
    if let Some(lang) = &q.filter.language
        && !lang.eq_ignore_ascii_case(&p.language)
    {
        return false;
    }
    true
}

fn asset_matches(a: &Asset, q: &SearchQuery, briefs: &HashMap<String, ProjectBrief>) -> bool {
    if let Some(t) = q.filter.asset_type
        && a.asset_type != t
    {
        return false;
    }
    if let Some(pid) = &q.filter.project_id
        && a.project_id != *pid
    {
        return false;
    }
    if let Some(min) = q.filter.min_reuse_score
        && a.reuse_score < min
    {
        return false;
    }
    // 资产不带语言字段：看所属项目的主语言，其次看资产自身标签
    if let Some(lang) = &q.filter.language {
        let via_project = briefs
            .get(&a.project_id)
            .is_some_and(|b| b.language.eq_ignore_ascii_case(lang));
        let via_tag = a.tags.iter().any(|t| t.eq_ignore_ascii_case(lang));
        if !via_project && !via_tag {
            return false;
        }
    }
    true
}

/// 名称命中加权：名称含任一查询词则加分。
fn name_boost(name: &str, terms: &[String]) -> f64 {
    let lower = name.to_lowercase();
    let hit = terms.iter().any(|t| {
        let t = t.trim().to_lowercase();
        !t.is_empty() && lower.contains(&t)
    });
    if hit {
        NAME_BOOST
    } else {
        0.0
    }
}

/// 综合分 + 名称加权。
///
/// 🔴 加权必须作用在**最终分**上，不能加进 relevance 再乘权重：
/// LIKE 回退路径下"名称命中"的 relevance 已经是 1.0，
/// 再 `+0.25` 会被 `clamp(0,1)` 整个吞掉——加权恰好在最需要它的
/// 中文短查询场景下完全失效，于是"描述里提到关键词但质量高的项目"
/// 会排在"名字就叫这个项目"的前面，与用户直觉相反。
fn boosted_score(input: &RankInput, name: &str, terms: &[String]) -> f64 {
    (input.score() + name_boost(name, terms)).clamp(0.0, 1.0)
}

fn sources_for(input: &RankInput, name: &str, terms: &[String]) -> Vec<MatchSource> {
    let mut sources: Vec<MatchSource> = Vec::with_capacity(3);
    if name_boost(name, terms) > 0.0 {
        sources.push(MatchSource::NameMatch);
    }
    if input.relevance > 0.0 {
        sources.push(MatchSource::Keyword);
    }
    if sources.is_empty() {
        // 浏览模式：没有关键词命中，命中来源就是"符合筛选条件"
        sources.push(MatchSource::Structured);
    }
    sources
}

fn project_scored(p: &Project, input: RankInput, terms: &[String], sort: SortBy) -> Scored {
    let recency = input.recency;
    let hit = SearchHit {
        kind: HitKind::Project,
        id: p.id.clone(),
        title: p.name.clone(),
        subtitle: format!("{} · {}", p.language, p.framework),
        // 描述为空时用路径兜底：不留空片段，否则结果项看起来像渲染坏了
        snippet: snippet::highlight(
            if p.description.is_empty() { &p.path } else { &p.description },
            terms,
        ),
        score: boosted_score(&input, &p.name, terms),
        sources: sources_for(&input, &p.name, terms),
        reasons: input.reasons(&format!("健康度 {} 分", p.health_score)),
        link: HitLink {
            page: "project".into(),
            param: Some(p.id.clone()),
        },
    };
    Scored {
        // 🔴 必须用**用户实际请求**的排序方式算主键，不能猜。
        // 猜测（"ReuseScore 拿不到就退 Confidence"）会让用户选"按最近更新"时
        // 项目却拿健康度当主键——排序结果与所选维度不符，且无法从 UI 察觉。
        //
        // 项目：有真实时间线（updated_at）；没有 reuse_score 概念；
        // "置信度"维度映射到健康度，与资产/洞察在同一维度下可比。
        sort_key: explicit_sort_key(
            sort,
            &SortFacts::with_timeline(
                None,
                Some(f64::from(p.health_score) / 100.0),
                recency,
            ),
        ),
        hit,
    }
}

fn asset_scored(
    a: &Asset,
    input: RankInput,
    terms: &[String],
    project_name: Option<String>,
    sort: SortBy,
) -> Scored {
    let tier = ReuseTier::from_score(a.reuse_score);
    // 🔴 刻意不再取 `input.recency`：资产的 created_at 是**抽取时间**，
    // 重新索引就变成今天，不代表代码新旧。排序主键走
    // `SortFacts::without_timeline`（内部填 NEUTRAL_RECENCY），
    // 传进来的 recency 本就不该被采用——留着这个绑定只会误导读者。
    let hit = SearchHit {
        kind: HitKind::Asset,
        id: a.id.clone(),
        title: a.name.clone(),
        // 副标题给"来自哪个项目"：用户据此判断能否直接拿来用
        subtitle: project_name.unwrap_or_else(|| a.asset_type.label_zh().to_string()),
        snippet: snippet::highlight(
            if a.description.is_empty() { &a.source_path } else { &a.description },
            terms,
        ),
        score: boosted_score(&input, &a.name, terms),
        sources: sources_for(&input, &a.name, terms),
        reasons: input.reasons(&format!(
            "复用评分 {:.2}（{}）",
            a.reuse_score,
            tier.label_zh()
        )),
        link: HitLink {
            page: "assets".into(),
            param: Some(a.id.clone()),
        },
    };
    Scored {
        // 资产**没有真实时间线**：它的 created_at 是抽取时间，
        // 重新索引就变成今天，当新鲜度会让刚索引的十年老代码排到最前。
        // 故用 without_timeline（内部填 NEUTRAL_RECENCY），传入的 recency 不采用。
        sort_key: explicit_sort_key(
            sort,
            &SortFacts::without_timeline(Some(a.reuse_score), Some(a.confidence)),
        ),
        hit,
    }
}

fn capability_scored(c: &Capability, input: RankInput, terms: &[String]) -> Scored {
    let hit = SearchHit {
        kind: HitKind::Capability,
        id: c.id.clone(),
        title: c.name.clone(),
        subtitle: format!("{} · 关联 {} 个项目", c.layer.label_zh(), c.project_count),
        snippet: snippet::highlight(&c.description, terms),
        score: boosted_score(&input, &c.name, terms),
        sources: sources_for(&input, &c.name, terms),
        reasons: input.reasons(&format!("被 {} 个项目使用", c.project_count)),
        link: HitLink {
            page: "graph".into(),
            param: Some(c.id.clone()),
        },
    };
    Scored {
        sort_key: None, // 能力没有 reuse_score / confidence 维度
        hit,
    }
}

/// 洞察命中。
///
/// # 🔴 subtitle 必须带出用户处置状态
/// 洞察可能被用户标记为"已忽略"。召回阶段刻意**不**过滤它
/// （理由见 `RetrievalRepo::insights`：隐藏过滤会让"搜索结果与洞察页对不上"），
/// 但必须把状态显示出来，否则用户会困惑"我明明忽略过这条，怎么又出现了"。
///
/// 这不是装饰文案：`state` 直接回答"这条结论我处理过了吗"。
fn insight_scored(
    i: &Insight,
    input: RankInput,
    terms: &[String],
    sort: SortBy,
) -> Scored {
    let recency = input.recency;
    let hit = SearchHit {
        kind: HitKind::Insight,
        id: i.id.clone(),
        title: i.title.clone(),
        subtitle: format!(
            "{} · {} · {}{}",
            i.insight_type.label_zh(),
            i.badge().label_zh(),
            state_label_zh(i.user_feedback),
            evidence_suffix(i.evidence.len()),
        ),
        snippet: snippet::highlight(&i.description, terms),
        score: boosted_score(&input, &i.title, terms),
        sources: sources_for(&input, &i.title, terms),
        reasons: input.reasons(&format!(
            "置信度 {:.0}% · {} 条证据",
            i.confidence * 100.0,
            i.evidence.len()
        )),
        link: HitLink {
            page: "insights".into(),
            param: Some(i.id.clone()),
        },
    };
    Scored {
        // 洞察有真实时间线（created_at = 这条结论何时得出），
        // 但没有"复用价值"维度；置信度即其可信程度。
        sort_key: explicit_sort_key(
            sort,
            &SortFacts::with_timeline(None, Some(i.confidence), recency),
        ),
        hit,
    }
}

/// 机会命中。
fn opportunity_scored(
    o: &Opportunity,
    input: RankInput,
    terms: &[String],
    sort: SortBy,
) -> Scored {
    let recency = input.recency;
    let hit = SearchHit {
        kind: HitKind::Opportunity,
        id: o.id.clone(),
        title: o.title.clone(),
        subtitle: format!(
            "{} · {} · 覆盖度 {:.0}% · {}",
            stars_label(o.rating),
            o.status.label_zh(),
            o.coverage * 100.0,
            capability_brief(o),
        ),
        // `why` 是"为什么值得关注"的真实依据，比 description 更能回答用户的疑问
        snippet: snippet::highlight(
            if o.why.is_empty() { &o.description } else { &o.why },
            terms,
        ),
        score: boosted_score(&input, &o.title, terms),
        sources: sources_for(&input, &o.title, terms),
        reasons: input.reasons(&format!(
            "{} · 已具备 {} 项能力、缺 {} 项",
            stars_label(o.rating),
            o.required_capabilities.len(),
            o.missing_capabilities.len()
        )),
        link: HitLink {
            // 机会与洞察同属洞察页（service 层也是同一模块）
            page: "insights".into(),
            param: Some(o.id.clone()),
        },
    };
    // 🔴 机会没有 confidence 字段，两个维度都映射到 `opportunity_quality`：
    // 它由星级与覆盖度确定性算出，正是"这个机会值不值得做"的度量。
    // 若强行给 ReuseScore 维度填 None，用户按"复用价值"排序时机会会全部消失。
    let quality = opportunity_quality(o);
    Scored {
        sort_key: explicit_sort_key(
            sort,
            &SortFacts::with_timeline(Some(quality), Some(quality), recency),
        ),
        hit,
    }
}

/// 星级展示串（"★★★☆☆"）。clamp 到 1-5：脏数据不该渲染出 7 颗星。
fn stars_label(rating: u8) -> String {
    let n = usize::from(rating.clamp(1, 5));
    format!("{}{}", "★".repeat(n), "☆".repeat(5 - n))
}

/// 机会副标题里的能力摘要：只列前 2 项。
///
/// 不列全部：机会可能有 5-6 项能力，全列出来副标题会撑成两三行，
/// 搜索结果列表的扫读体验就毁了（用户要的是一眼看清"这是什么"）。
fn capability_brief(o: &Opportunity) -> String {
    let mut names: Vec<&str> = o
        .required_capabilities
        .iter()
        .map(String::as_str)
        .take(2)
        .collect();
    if names.is_empty() {
        names.extend(o.missing_capabilities.iter().map(String::as_str).take(2));
    }
    if names.is_empty() {
        "无能力清单".to_string()
    } else {
        names.join("、")
    }
}

/// 证据条数的后缀文案。0 条时明确说明，不含糊。
fn evidence_suffix(n: usize) -> String {
    if n == 0 {
        " · 无证据".to_string()
    } else {
        format!(" · {n} 条证据")
    }
}

/// 洞察的用户处置状态文案。
///
/// 🔴 与 service 层 `insights::state_of` 的措辞必须一致：
/// 同一条洞察在搜索结果副标题和洞察列表里若显示不同文案，
/// 用户会以为是两条不同的记录。
fn state_label_zh(fb: Option<UserFeedback>) -> &'static str {
    match fb {
        Some(UserFeedback::Useful) => "已标记有用",
        Some(UserFeedback::Useless) => "已标记无用",
        Some(UserFeedback::Ignored) => "已忽略",
        None => "待处理",
    }
}

// StorageError → SpoliaError 的转换由 domain 层的
// `SpoliaError::Storage(#[from] StorageError)` 提供，本 crate 直接用 `?` 即可。
// （不得在此再 impl From：两个类型都属外部 crate，违反孤儿规则且与 domain 冲突。）

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::{AssetType, CapabilityLayer, CodeStats, Evidence, ProjectStatus, SearchFilter};

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-29T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn date_before(days: i64) -> String {
        (now() - chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string()
    }

    fn project(id: &str, name: &str, desc: &str, health: u8, days: Option<i64>) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("/tmp/{id}"),
            description: desc.into(),
            language: "Python".into(),
            framework: "FastAPI".into(),
            created_at: None,
            updated_at: days.map(date_before),
            last_commit_at: days.map(date_before),
            status: ProjectStatus::Active,
            health_score: health,
            completeness: None,
            tags: vec!["video".into()],
            sensitive: false,
            stats: CodeStats {
                files: 10,
                loc: 5000,
                symbols: 20,
                modules: 4,
                languages: vec![],
            },
            scan: spolia_domain::ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn asset(id: &str, pid: &str, name: &str, desc: &str, reuse: f64) -> Asset {
        Asset {
            id: id.into(),
            project_id: pid.into(),
            asset_type: AssetType::Component,
            name: name.into(),
            description: desc.into(),
            content: None,
            source_path: format!("src/{id}.py"),
            confidence: 0.9,
            reuse_score: reuse,
            generality: 0.7,
            stability: 0.6,
            tags: vec!["python".into()],
            created_at: "2026-09-01".into(),
            // 证据门禁要求至少一个来源文件，否则资产不会入库
            evidence: Evidence {
                files: vec![format!("src/{id}.py")],
                ..Evidence::default()
            },
            user_feedback: None,
        }
    }

    fn cap(id: &str, name: &str, desc: &str, count: u32) -> Capability {
        let mut c = Capability::new(id, name, CapabilityLayer::Domain, None, 0.9).unwrap();
        c.description = desc.to_string();
        c.project_count = count;
        c
    }

    /// 造一个有项目 + 资产 + 能力的库。
    fn seeded() -> Database {
        let d = db();
        d.projects()
            .upsert_batch(&[
                project("p1", "视频管线", "生成视频的完整流程", 85, Some(5)),
                project("p2", "图片工具", "批处理图片", 60, Some(200)),
            ])
            .unwrap();
        d.assets()
            .upsert_batch(&[
                asset("a1", "p1", "VideoPipeline", "视频生成管线组件", 0.9),
                asset("a2", "p2", "ImageBatch", "图片批处理", 0.5),
            ])
            .unwrap();
        d.capabilities()
            .upsert_batch(&[
                cap("cap_domain_media", "多媒体", "音视频处理能力", 3),
                cap("c_video", "视频生成", "从脚本生成视频", 2),
            ])
            .unwrap();
        d
    }

    fn q(text: &str) -> SearchQuery {
        SearchQuery {
            q: text.into(),
            ..Default::default()
        }
    }

    // ── 基本检索 ─────────────────────────────────────────────────

    /// 🔴 2 字中文查询是高频场景，必须有结果（FTS trigram 覆盖不到）。
    #[test]
    fn finds_results_for_two_char_chinese_query() {
        let d = seeded();
        let r = SearchEngine::new().search(&d, &q("视频"), now()).unwrap();
        assert!(!r.is_empty(), "2 字中文查询必须有结果（LIKE 回退）");
        assert!(r.used_substring_fallback, "应标记走了子串回退");
        assert!(r.hits.iter().any(|h| h.id == "p1"));
    }

    #[test]
    fn finds_asset_by_english_keyword() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(&d, &q("Pipeline"), now())
            .unwrap();
        assert!(!r.used_substring_fallback, "英文长词走 FTS");
        let hit = r.hits.iter().find(|h| h.id == "a1");
        assert!(hit.is_some(), "应命中 VideoPipeline 资产");
        assert_eq!(hit.unwrap().kind, HitKind::Asset);
        assert_eq!(hit.unwrap().subtitle, "视频管线", "副标题应为所属项目名");
    }

    #[test]
    fn finds_capability_by_keyword() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    scope: SearchScope::Capabilities,
                    ..q("视频生成")
                },
                now(),
            )
            .unwrap();
        assert!(r.hits.iter().any(|h| h.kind == HitKind::Capability));
        assert!(r.hits.iter().any(|h| h.id == "c_video"));
    }

    #[test]
    fn all_scope_returns_mixed_entity_kinds() {
        let d = seeded();
        let r = SearchEngine::new().search(&d, &q("视频"), now()).unwrap();
        let kinds: Vec<HitKind> = r.hits.iter().map(|h| h.kind).collect();
        assert!(kinds.contains(&HitKind::Project), "应含项目: {kinds:?}");
        assert!(kinds.contains(&HitKind::Asset), "应含资产: {kinds:?}");
    }

    #[test]
    fn no_match_returns_empty_not_error() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(&d, &q("完全不存在的词汇xyz"), now())
            .unwrap();
        assert!(r.is_empty());
        assert_eq!(r.total, 0);
    }

    // ── 产品红线：排序理由与证据 ─────────────────────────────────

    /// 每条结果都必须有排序理由——这是产品硬要求，不是可选装饰。
    #[test]
    fn every_hit_has_reasons() {
        let d = seeded();
        for query in ["视频", "Pipeline", "图片", ""] {
            let r = SearchEngine::new()
                .search(&d, &q(query), now())
                .unwrap();
            for h in &r.hits {
                assert!(
                    !h.reasons.is_empty(),
                    "查询 {query:?} 的结果 {} 缺少排序理由",
                    h.id
                );
            }
        }
    }

    #[test]
    fn every_hit_is_fully_populated() {
        let d = seeded();
        let r = SearchEngine::new().search(&d, &q("视频"), now()).unwrap();
        assert!(!r.hits.is_empty());
        for h in &r.hits {
            assert!(!h.title.is_empty(), "{} 缺少标题", h.id);
            assert!(!h.snippet.is_empty(), "{} 缺少摘要", h.id);
            assert!(!h.link.page.is_empty(), "{} 缺少跳转页", h.id);
            assert!(h.link.param.is_some(), "{} 缺少跳转参数", h.id);
            assert!(!h.sources.is_empty(), "{} 缺少命中来源", h.id);
            assert!(
                (0.0..=1.0).contains(&h.score),
                "分数越界: {} ({})",
                h.score,
                h.id
            );
        }
    }

    #[test]
    fn links_point_to_real_pages() {
        let d = seeded();
        let r = SearchEngine::new().search(&d, &q("视频"), now()).unwrap();
        // 页面 key 必须与前端路由表一致，否则点击结果会跳到空白页
        let valid = ["project", "assets", "graph", "overview", "insights"];
        for h in &r.hits {
            assert!(
                valid.contains(&h.link.page.as_str()),
                "未知页面 key: {} ({})",
                h.link.page,
                h.id
            );
        }
    }

    /// 名称命中的结果应排在仅描述命中的之前（符合用户直觉）。
    #[test]
    fn name_match_ranks_first() {
        let d = db();
        d.projects()
            .upsert_batch(&[
                project("p_desc", "其它工具", "包含 视频 处理功能", 90, Some(1)),
                project("p_name", "视频编辑器", "剪辑工具", 50, Some(100)),
            ])
            .unwrap();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    scope: SearchScope::Projects,
                    ..q("视频")
                },
                now(),
            )
            .unwrap();
        assert!(r.hits.len() >= 2, "两个项目都应命中");
        assert_eq!(r.hits[0].id, "p_name", "名称命中应排第一");
        assert!(r.hits[0].sources.contains(&MatchSource::NameMatch));
    }

    // ── 范围与过滤 ───────────────────────────────────────────────

    #[test]
    fn scope_restricts_entity_kinds() {
        let d = seeded();
        for (scope, expected) in [
            (SearchScope::Assets, HitKind::Asset),
            (SearchScope::Projects, HitKind::Project),
            (SearchScope::Capabilities, HitKind::Capability),
        ] {
            let r = SearchEngine::new()
                .search(
                    &d,
                    &SearchQuery {
                        scope,
                        limit: 50,
                        ..q("视频")
                    },
                    now(),
                )
                .unwrap();
            assert!(
                r.hits.iter().all(|h| h.kind == expected),
                "范围 {scope:?} 混入了其它类型: {:?}",
                r.hits.iter().map(|h| h.kind).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn filter_by_asset_type() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    filter: SearchFilter {
                        asset_type: Some(AssetType::Api),
                        ..Default::default()
                    },
                    ..q("Pipeline")
                },
                now(),
            )
            .unwrap();
        assert!(
            !r.hits.iter().any(|h| h.id == "a1"),
            "类型不符的资产应被过滤掉"
        );
    }

    #[test]
    fn filter_by_min_reuse_score() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    filter: SearchFilter {
                        min_reuse_score: Some(0.8),
                        ..Default::default()
                    },
                    scope: SearchScope::Assets,
                    ..q("批处理")
                },
                now(),
            )
            .unwrap();
        assert!(
            !r.hits.iter().any(|h| h.id == "a2"),
            "reuse_score 0.5 的资产应被 min_reuse_score=0.8 过滤"
        );
    }

    #[test]
    fn filter_by_project_status() {
        let d = db();
        let mut abandoned = project("p1", "废弃项目", "视频相关", 30, Some(900));
        abandoned.status = ProjectStatus::Abandoned;
        d.projects()
            .upsert_batch(&[
                abandoned,
                project("p2", "活跃项目", "视频相关", 90, Some(2)),
            ])
            .unwrap();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    filter: SearchFilter {
                        project_status: Some(ProjectStatus::Active),
                        ..Default::default()
                    },
                    scope: SearchScope::Projects,
                    ..q("视频")
                },
                now(),
            )
            .unwrap();
        let ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
        assert!(ids.contains(&"p2"));
        assert!(!ids.contains(&"p1"), "已归档项目应被状态过滤排除");
    }

    #[test]
    fn filter_by_language() {
        let d = db();
        let mut rust = project("p_rs", "Rust 视频工具", "视频处理", 80, Some(3));
        rust.language = "Rust".into();
        d.projects()
            .upsert_batch(&[rust, project("p_py", "Python 视频工具", "视频处理", 80, Some(3))])
            .unwrap();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    filter: SearchFilter {
                        language: Some("Rust".into()),
                        ..Default::default()
                    },
                    scope: SearchScope::Projects,
                    ..q("视频")
                },
                now(),
            )
            .unwrap();
        let ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["p_rs"], "语言过滤应只留 Rust 项目");
    }

    /// 资产不带语言字段：语言过滤必须借所属项目判定。
    #[test]
    fn asset_language_filter_uses_owning_project() {
        let d = db();
        let mut rust = project("p_rs", "Rust 项目", "视频", 80, Some(3));
        rust.language = "Rust".into();
        d.projects()
            .upsert_batch(&[rust, project("p_py", "Python 项目", "视频", 80, Some(3))])
            .unwrap();
        d.assets()
            .upsert_batch(&[
                asset("a_rs", "p_rs", "视频组件A", "视频", 0.9),
                asset("a_py", "p_py", "视频组件B", "视频", 0.9),
            ])
            .unwrap();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    filter: SearchFilter {
                        language: Some("Rust".into()),
                        ..Default::default()
                    },
                    scope: SearchScope::Assets,
                    ..q("视频")
                },
                now(),
            )
            .unwrap();
        let ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["a_rs"], "资产语言过滤应经所属项目判定");
    }

    // ── 浏览模式（空查询）────────────────────────────────────────

    /// 清空搜索框必须展示可浏览列表，而不是"0 结果"。
    #[test]
    fn empty_query_browses_instead_of_returning_nothing() {
        let d = seeded();
        let r = SearchEngine::new().search(&d, &q(""), now()).unwrap();
        assert!(!r.is_empty(), "空查询应进入浏览模式");
        assert!(!r.used_substring_fallback);
        // 浏览模式没有关键词命中，来源应是"符合筛选条件"
        assert!(r
            .hits
            .iter()
            .all(|h| h.sources.contains(&MatchSource::Structured)));
    }

    #[test]
    fn blank_query_is_treated_as_browse() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(&d, &q("   \t  "), now())
            .unwrap();
        assert!(!r.is_empty());
        assert_eq!(r.query, "", "回显的查询应为归一化后的空串");
    }

    #[test]
    fn browse_respects_limit_and_offset() {
        let d = db();
        let batch: Vec<Project> = (0..10)
            .map(|i| project(&format!("p{i}"), &format!("项目{i}"), "视频相关", 70, Some(i)))
            .collect();
        d.projects().upsert_batch(&batch).unwrap();
        let engine = SearchEngine::new();
        let page1 = engine
            .search(
                &d,
                &SearchQuery {
                    scope: SearchScope::Projects,
                    limit: 3,
                    ..q("")
                },
                now(),
            )
            .unwrap();
        let page2 = engine
            .search(
                &d,
                &SearchQuery {
                    scope: SearchScope::Projects,
                    limit: 3,
                    offset: 3,
                    ..q("")
                },
                now(),
            )
            .unwrap();
        assert_eq!(page1.hits.len(), 3);
        assert_eq!(page2.hits.len(), 3);
        let ids1: Vec<&str> = page1.hits.iter().map(|h| h.id.as_str()).collect();
        let ids2: Vec<&str> = page2.hits.iter().map(|h| h.id.as_str()).collect();
        assert!(
            ids1.iter().all(|id| !ids2.contains(id)),
            "分页不得重叠: {ids1:?} vs {ids2:?}"
        );
        assert_eq!(page1.total, page2.total, "total 应为总数而非当页数");
    }

    /// 浏览模式下资产仍须遵守"无证据不展示"的产品红线。
    #[test]
    fn browse_requires_evidence_for_assets() {
        let d = seeded();
        let mut no_evidence = asset("a_bad", "p1", "无证据资产", "视频", 0.99);
        no_evidence.evidence = Evidence::default();
        // 直接写库会被门禁拒；这里验证门禁确实生效
        let outcome = d.assets().upsert(&no_evidence).unwrap();
        assert!(!outcome, "无证据资产不应入库");

        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    scope: SearchScope::Assets,
                    ..q("")
                },
                now(),
            )
            .unwrap();
        assert!(!r.hits.is_empty());
        assert!(
            !r.hits.iter().any(|h| h.id == "a_bad"),
            "无证据资产不得出现在浏览结果中"
        );
    }

    // ── 分页与确定性 ─────────────────────────────────────────────

    #[test]
    fn pagination_splits_keyword_results() {
        let d = db();
        let batch: Vec<Project> = (0..8)
            .map(|i| {
                project(
                    &format!("p{i}"),
                    &format!("视频工具{i}"),
                    "视频处理",
                    70,
                    Some(i),
                )
            })
            .collect();
        d.projects().upsert_batch(&batch).unwrap();
        let engine = SearchEngine::new();
        let all = engine
            .search(
                &d,
                &SearchQuery {
                    scope: SearchScope::Projects,
                    limit: 100,
                    ..q("视频")
                },
                now(),
            )
            .unwrap();
        let page = engine
            .search(
                &d,
                &SearchQuery {
                    scope: SearchScope::Projects,
                    limit: 3,
                    ..q("视频")
                },
                now(),
            )
            .unwrap();
        assert_eq!(page.hits.len(), 3);
        assert_eq!(page.total, all.total, "total 不随分页变化");
        assert_eq!(all.hits.len(), 8);
    }

    /// 同一查询多次执行必须给出完全相同的顺序。
    #[test]
    fn results_are_deterministic_across_runs() {
        let d = seeded();
        let engine = SearchEngine::new();
        let first: Vec<String> = engine
            .search(&d, &q("视频"), now())
            .unwrap()
            .hits
            .iter()
            .map(|h| h.id.clone())
            .collect();
        for _ in 0..5 {
            let again: Vec<String> = engine
                .search(&d, &q("视频"), now())
                .unwrap()
                .hits
                .iter()
                .map(|h| h.id.clone())
                .collect();
            assert_eq!(first, again, "顺序不得随执行次数变化");
        }
    }

    /// 同分结果必须按 id 稳定排序，否则刷新一次顺序就变。
    #[test]
    fn equal_scores_break_tie_by_id() {
        let d = db();
        // 三个完全同质的项目：分数必然相同
        d.projects()
            .upsert_batch(&[
                project("p_c", "视频C", "视频", 70, Some(1)),
                project("p_a", "视频A", "视频", 70, Some(1)),
                project("p_b", "视频B", "视频", 70, Some(1)),
            ])
            .unwrap();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    scope: SearchScope::Projects,
                    ..q("视频")
                },
                now(),
            )
            .unwrap();
        let ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["p_a", "p_b", "p_c"], "同分应按 id 升序");
    }

    // ── 排序方式 ─────────────────────────────────────────────────

    #[test]
    fn explicit_sort_by_reuse_score() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    sort: SortBy::ReuseScore,
                    scope: SearchScope::Assets,
                    ..q("")
                },
                now(),
            )
            .unwrap();
        assert!(r.hits.len() >= 2);
        let pos1 = r.hits.iter().position(|h| h.id == "a1");
        let pos2 = r.hits.iter().position(|h| h.id == "a2");
        assert!(
            pos1 < pos2,
            "按复用分排序时高分应在前: {pos1:?} vs {pos2:?}"
        );
    }

    /// 按"复用价值"排序时，项目没有该维度——应退回综合分而非沉底消失。
    #[test]
    fn explicit_sort_does_not_drop_entities_lacking_the_dimension() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    sort: SortBy::ReuseScore,
                    // All 范围：项目 + 资产 + 能力混排
                    ..q("视频")
                },
                now(),
            )
            .unwrap();
        assert!(
            r.hits.iter().any(|h| h.kind == HitKind::Project),
            "项目不得因缺少 reuse_score 而从结果中消失"
        );
    }

    #[test]
    fn explicit_sort_by_recency() {
        let d = db();
        d.projects()
            .upsert_batch(&[
                project("old", "视频旧项目", "视频", 90, Some(300)),
                project("new", "视频新项目", "视频", 40, Some(1)),
            ])
            .unwrap();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    sort: SortBy::RecentlyUpdated,
                    scope: SearchScope::Projects,
                    ..q("")
                },
                now(),
            )
            .unwrap();
        assert_eq!(r.hits[0].id, "new", "按最近更新排序时新项目应在前");
    }

    // ── 健壮性 ───────────────────────────────────────────────────

    #[test]
    fn hostile_queries_do_not_error() {
        let d = seeded();
        let engine = SearchEngine::new();
        for hostile in [
            "\"", "C++", "a\"b", "foo*", "(video", "%", "_", "\\", "' OR 1=1 --", "<script>",
            "视频%", "%%%", "\u{0}", "视频\"", "*",
        ] {
            let r = engine.search(&d, &q(hostile), now());
            assert!(
                r.is_ok(),
                "查询 {hostile:?} 不应报错: {:?}",
                r.err().map(|e| e.to_string())
            );
        }
    }

    /// SQL 注入尝试必须按字面处理，不得返回全表。
    #[test]
    fn sql_injection_is_treated_as_literal() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(&d, &q("' OR 1=1 --"), now())
            .unwrap();
        assert!(r.is_empty(), "注入串应按字面匹配，命中 0 条");
    }

    /// LIKE 通配符必须按字面处理：搜 "%%" 不得返回全表。
    #[test]
    fn wildcards_are_literal_not_match_all() {
        let d = seeded();
        let r = SearchEngine::new().search(&d, &q("%%%"), now()).unwrap();
        assert!(r.is_empty(), "通配符应按字面匹配");
    }

    #[test]
    fn empty_database_is_safe() {
        let d = db();
        let engine = SearchEngine::new();
        for query in ["视频", "", "pipeline"] {
            let r = engine.search(&d, &q(query), now()).unwrap();
            assert!(r.is_empty(), "空库查询 {query:?} 应返回空集");
            assert_eq!(r.total, 0);
        }
    }

    #[test]
    fn took_ms_is_reported_within_budget() {
        let d = seeded();
        let r = SearchEngine::new().search(&d, &q("视频"), now()).unwrap();
        // 内存库应远快于 1 秒（产品指标：搜索响应 ≤ 1 秒）
        assert!(
            r.took_ms < 1000,
            "检索耗时 {}ms 超出 1 秒指标",
            r.took_ms
        );
    }

    #[test]
    fn query_is_echoed_normalized() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(&d, &q("  视频   生成  "), now())
            .unwrap();
        assert_eq!(r.query, "视频 生成");
    }

    #[test]
    fn oversized_limit_is_clamped() {
        let d = seeded();
        let r = SearchEngine::new()
            .search(
                &d,
                &SearchQuery {
                    limit: 999_999,
                    ..q("视频")
                },
                now(),
            )
            .unwrap();
        assert!(r.hits.len() <= 200, "limit 应被钳到 200");
    }

    /// 描述为空时片段用路径兜底，不留空。
    #[test]
    fn empty_description_falls_back_to_path_in_snippet() {
        let d = db();
        d.projects()
            .upsert(&project("p1", "视频工具", "", 80, Some(1)))
            .unwrap();
        let r = SearchEngine::new()
            .search(&d, &q("视频"), now())
            .unwrap();
        let hit = r.hits.iter().find(|h| h.id == "p1").unwrap();
        assert!(!hit.snippet.is_empty(), "描述为空时应回退到路径");
    }

    // ── 纯函数 ───────────────────────────────────────────────────

    #[test]
    fn scope_inclusion_logic() {
        assert!(scope_includes(SearchScope::All, SearchScope::Assets));
        assert!(scope_includes(SearchScope::All, SearchScope::Projects));
        assert!(scope_includes(SearchScope::Assets, SearchScope::Assets));
        assert!(!scope_includes(SearchScope::Assets, SearchScope::Projects));
        assert!(!scope_includes(SearchScope::Projects, SearchScope::Capabilities));
    }

    #[test]
    fn query_terms_splits_on_whitespace() {
        assert_eq!(query_terms("视频 生成"), vec!["视频", "生成"]);
        assert!(query_terms("").is_empty());
        assert_eq!(query_terms("  a  "), vec!["a"]);
        assert_eq!(query_terms("a\tb\nc"), vec!["a", "b", "c"]);
    }

    #[test]
    fn name_boost_only_for_matching_names() {
        assert_eq!(name_boost("视频管线", &query_terms("视频")), NAME_BOOST);
        assert_eq!(name_boost("图片工具", &query_terms("视频")), 0.0);
        // 大小写不敏感
        assert_eq!(
            name_boost("VideoPipeline", &query_terms("pipeline")),
            NAME_BOOST
        );
        // 空词不得匹配一切
        assert_eq!(name_boost("任何名称", &query_terms("")), 0.0);
        assert_eq!(name_boost("任何名称", &query_terms("   ")), 0.0);
        assert_eq!(NAME_BOOST, 0.25);
    }

    #[test]
    fn boosted_score_is_clamped_into_unit_interval() {
        let input = RankInput {
            relevance: 1.0,
            quality: 1.0,
            recency: 1.0,
            from_substring: false,
        };
        let s = boosted_score(&input, "视频", &query_terms("视频"));
        assert!(s <= 1.0, "加权后不得超过 1.0，实际 {s}");
        assert!((s - 1.0).abs() < 1e-9);
    }

    /// 🔴 回归守护：relevance 已饱和（LIKE 路径下名称命中即为 1.0）时，
    /// 名称加权仍必须生效。
    ///
    /// 早期实现把 boost 加进 relevance 再 clamp，饱和时 `1.0 + 0.25` 被钳回 1.0，
    /// 加权恰好在中文短查询场景下完全失效——结果是"描述里提到关键词但质量高的项目"
    /// 排在"名字就叫这个项目"的前面，与用户直觉相反且难以察觉。
    #[test]
    fn name_boost_survives_saturated_relevance() {
        let terms = query_terms("视频");
        let saturated = RankInput {
            relevance: 1.0, // LIKE 名称命中已经是满分
            quality: 0.5,
            recency: 0.5,
            from_substring: true,
        };
        let name_hit = boosted_score(&saturated, "视频编辑器", &terms);
        let desc_only = boosted_score(&saturated, "其它工具", &terms);
        assert!(
            name_hit > desc_only,
            "相关性饱和时名称命中仍应排前: {name_hit} vs {desc_only}"
        );
        assert!(
            (name_hit - desc_only - NAME_BOOST).abs() < 1e-9 || name_hit >= 1.0,
            "差值应等于加权幅度（除非已触及上限）"
        );
    }

    /// 未命中名称时不得加分。
    #[test]
    fn boosted_score_without_name_match_equals_base() {
        let input = RankInput {
            relevance: 0.4,
            quality: 0.6,
            recency: 0.2,
            from_substring: false,
        };
        let base = input.score();
        assert_eq!(boosted_score(&input, "无关名称", &query_terms("视频")), base);
    }

    #[test]
    fn recall_per_kind_exceeds_default_limit() {
        // 召回上限必须大于默认分页 limit，否则跨类型综合排序无意义
        assert!(RECALL_PER_KIND as usize > SearchQuery::default().limit as usize);
    }

    #[test]
    fn sources_never_empty() {
        // 浏览模式（relevance=0）也必须给出命中来源，否则前端渲染空徽章
        let input = RankInput {
            relevance: 0.0,
            quality: 0.5,
            recency: 0.0,
            from_substring: false,
        };
        assert_eq!(sources_for(&input, "无关名称", &query_terms("视频")), vec![MatchSource::Structured]);
        let matched = RankInput {
            relevance: 0.5,
            ..input
        };
        assert!(sources_for(&matched, "视频工具", &query_terms("视频")).contains(&MatchSource::NameMatch));
    }
}
