//! 无 LLM 时的确定性回答。
//!
//! # 为什么必须存在
//! 用户第一次打开软件时几乎不可能已经装好 Ollama 或填好 API Key。
//! 如果 AI 分析师页此时白屏或报错，产品的核心体验就断了。
//! 本模块从**数据库里真实存在的记录**组装回答，让"问一句就能看到相关项目/资产"
//! 在零配置状态下也能工作。
//!
//! # 三条纪律
//! 1. **绝不编造**：没检索到相关内容就直说"没找到"，并给出可操作的下一步
//!    （去添加扫描目录 / 去配置模型）。编一个看似合理的答案比承认不知道危害大得多。
//! 2. **必须标注来源**：`generated_by = Deterministic`，前端据此显示
//!    "离线检索式回答"，不冒充 AI 生成。
//! 3. **每条论断都要有出处**：citations 全部来自真实检索结果，可点击跳转。
//!
//! # 为什么是纯函数
//! 输入是已经检索好的 `SearchHit` 列表（由上层用 `spolia-search` 取得），
//! 本模块不碰数据库、不发网络。这样可以穷举单测各种边界
//! （空结果、只有项目、混合类型、超长列表），而 AI crate 也不必依赖 storage。

use spolia_domain::{
    AnalystAnswer, AnswerSource, Citation, CitationKind, HitKind, SearchHit,
};

/// 确定性回答的输入。
#[derive(Debug, Clone, Default)]
pub struct FallbackContext<'a> {
    /// 用户的问题（已归一化）
    pub question: &'a str,
    /// 检索到的相关记录（按相关性降序）
    pub hits: &'a [SearchHit],
    /// 检索命中的总数（可能大于 `hits.len()`，因为分页）
    pub total: usize,
    /// 库里项目总数：用于区分"没找到"与"库是空的"，两者该给不同的建议
    pub project_count: usize,
    /// 是否配置了模型（影响提示文案：未配置 → 引导配置；已配置但失败 → 引导重试）
    pub model_available: bool,
    /// 模型不可用的原因（已配置但连接失败时给出，帮助用户自查）
    pub model_unavailable_reason: Option<&'a str>,
}

/// 回答里最多列出多少条引用。
///
/// 再多就成了搜索结果列表，失去"回答"的意义。
/// 剩余的通过 `total` 告知用户，引导他去资产/项目页看全部。
pub const MAX_CITATIONS: usize = 6;

/// 后续建议问题的最大条数。
pub const MAX_FOLLOWUPS: usize = 3;

/// 生成确定性回答。
pub fn deterministic_answer(ctx: &FallbackContext<'_>) -> AnalystAnswer {
    let question = ctx.question.trim();
    let hits = ctx.hits;

    // 库里什么都没有：这不是"没搜到"，而是"还没扫描"。
    // 两种情况给一样的提示会让用户困惑（明明扫过了却说没数据）。
    if ctx.project_count == 0 {
        return AnalystAnswer {
            content: empty_library_answer(ctx),
            generated_by: AnswerSource::Deterministic,
            citations: Vec::new(),
            followups: vec![
                "如何添加扫描目录？".to_string(),
                "如何配置本地模型？".to_string(),
            ],
            took_ms: 0,
        };
    }

    if hits.is_empty() {
        return AnalystAnswer {
            content: no_match_answer(question),
            generated_by: AnswerSource::Deterministic,
            citations: Vec::new(),
            followups: browse_suggestions(),
            took_ms: 0,
        };
    }

    let citations = build_citations(hits);
    let content = compose_answer(question, hits, ctx.total, &citations, ctx);

    AnalystAnswer {
        content,
        generated_by: AnswerSource::Deterministic,
        citations,
        followups: followups_for(hits),
        took_ms: 0,
    }
}

/// 库为空时的回答：引导用户先扫描。
fn empty_library_answer(ctx: &FallbackContext<'_>) -> String {
    let mut out = String::from(
        "本地知识库还是空的——需要先扫描你的项目目录，我才能回答关于它们的问题。\n\n\
         **下一步**：打开 设置 → 扫描目录，添加你的代码目录（例如 `F:/CodeProject`），然后点「开始扫描」。\n\n",
    );
    if ctx.model_available {
        out.push_str("扫描完成后即可提问，例如「我做过哪些视频生成相关的项目？」。");
    } else {
        out.push_str("扫描完成后，未配置模型时也能得到基于真实数据的检索式回答；\n若想要更深入的分析，可在 设置 → 大模型配置 里接入 Ollama 或云端模型。");
    }
    out
}

/// 有数据但没检索到时的回答。
///
/// 🔴 不编造：明确说"没找到"，并给出**具体**的调整建议
/// （换关键词、放宽筛选），而不是笼统的"请重试"。
fn no_match_answer(question: &str) -> String {
    if question.is_empty() {
        return "请输入你想了解的内容，例如「视频生成」「有哪些可复用组件」。".to_string();
    }
    format!(
        "在已索引的项目里没有找到与「{question}」相关的记录。\n\n\
         可能的原因与调整方向：\n\
         - **关键词太长或太具体**：试试更短的词（中文 2 字以上即可，例如把「基于扩散模型的视频生成管线」换成「视频」）\n\
         - **相关目录还没被扫描**：在 设置 → 扫描目录 里确认它已添加并启用\n\
         - **项目里确实没有这部分内容**：可以到「机会」页看看能否用现有资产组合出来"
    )
}

/// 组装正文：先给结论，再列证据。
///
/// 结构刻意模仿"人类回答"而非"搜索结果列表"：
/// 一句话结论 + 分类清单 + 数据来源说明。
fn compose_answer(
    question: &str,
    hits: &[SearchHit],
    total: usize,
    citations: &[Citation],
    ctx: &FallbackContext<'_>,
) -> String {
    let counts = count_by_kind(hits);
    let mut out = String::new();

    // ── 结论行 ────────────────────────────────────────────────
    out.push_str(&conclusion_line(question, hits.len(), total, &counts));
    out.push_str("\n\n");

    // ── 分类清单 ──────────────────────────────────────────────
    for (kind, n) in &counts {
        out.push_str(&format!("**{}**（{} 条）\n", kind.label_zh(), n));
        let mut shown = 0;
        for h in hits {
            if h.kind != *kind || shown >= MAX_CITATIONS {
                continue;
            }
            // 副标题给出上下文（所属项目 / 语言 · 框架），让用户不必点开就能判断
            let subtitle = if h.subtitle.is_empty() {
                String::new()
            } else {
                format!(" · {}", h.subtitle)
            };
            // 🔴 理由必须来自检索层给出的真实 reasons，不自己编
            let reason = h
                .reasons
                .first()
                .map(|r| format!("（{r}）"))
                .unwrap_or_default();
            out.push_str(&format!("- `{}`{}{}\n", h.title, subtitle, reason));
            shown += 1;
        }
        out.push('\n');
    }

    // ── 数据来源与局限说明 ────────────────────────────────────
    out.push_str(&provenance_note(ctx, citations.len(), total));
    out
}

/// 结论行：直接回答"找到了什么"。
fn conclusion_line(question: &str, shown: usize, total: usize, counts: &[(HitKind, usize)]) -> String {
    let parts: Vec<String> = counts
        .iter()
        .map(|(k, n)| format!("{n} 个{}", k.label_zh()))
        .collect();
    let summary = parts.join("、");

    let scope = if question.is_empty() {
        "已索引的内容".to_string()
    } else {
        format!("「{question}」")
    };

    if total > shown {
        format!(
            "与 {scope} 相关的共有 {total} 条记录（{summary}），下面列出最相关的前 {shown} 条："
        )
    } else {
        format!("找到 {summary} 与 {scope} 相关：")
    }
}

/// 数据来源说明：如实告知这是离线检索结果，并给出深入分析的路径。
fn provenance_note(ctx: &FallbackContext<'_>, cited: usize, total: usize) -> String {
    let mut out = format!(
        "---\n以上 {} 条引用全部来自本地已索引的真实数据，点击可跳转到出处。\n",
        cited.min(total)
    );
    if ctx.model_available {
        out.push_str("若需要跨项目的深入分析（例如「这些能力如何组合成新项目」），模型当前可用，可重试提问。");
    } else if let Some(reason) = ctx.model_unavailable_reason {
        out.push_str(&format!(
            "本次为**离线检索式回答**，未调用大模型（原因：{reason}）。\n\
             配置模型后可得到综合分析：设置 → 大模型配置。"
        ));
    } else {
        out.push_str(
            "本次为**离线检索式回答**，未调用大模型。\n\
             配置模型后可得到综合分析（跨项目推理、复用建议）：设置 → 大模型配置。",
        );
    }
    out
}

/// 按实体类型统计命中数（保持稳定顺序：项目 → 资产 → 能力 → 洞察 → 机会 → 其它）。
///
/// # 🔴 顺序来源必须是 `HitKind::all()`，不能在这里再抄一份数组
/// 早先这里是 `const ORDER: [HitKind; 7]` 手抄了七个变体。
/// 后来 domain 加了 `Insight`/`Opportunity` 两类实体，这个数组没跟上——
/// 后果是**降级回答的分类清单永远不显示洞察与机会**，
/// 而这恰恰是分析师降级路径答不出「有哪些重复实现」的第二个原因
/// （第一个是检索层根本不召回它们）。
///
/// 数组长度写死成 7 时，漏掉变体连编译警告都没有（`[HitKind; 7]` 只要凑够 7 个就合法）。
/// 用 `HitKind::all()` 之后，domain 加任何新变体都会自动出现在这里，
/// 且顺序与全局唯一真相源一致。
fn count_by_kind(hits: &[SearchHit]) -> Vec<(HitKind, usize)> {
    // 固定顺序而非出现顺序：同样的数据两次提问，回答结构必须一致
    HitKind::all()
        .iter()
        .filter_map(|k| {
            let n = hits.iter().filter(|h| h.kind == *k).count();
            (n > 0).then_some((*k, n))
        })
        .collect()
}

/// 从检索结果构造引用（去重、限数）。
fn build_citations(hits: &[SearchHit]) -> Vec<Citation> {
    let mut out: Vec<Citation> = Vec::with_capacity(hits.len().min(MAX_CITATIONS));
    for h in hits {
        if out.len() >= MAX_CITATIONS {
            break;
        }
        // 去重：同一实体可能因多字段命中而出现多次
        if out.iter().any(|c| c.link.param == h.link.param && c.kind == citation_kind(h.kind)) {
            continue;
        }
        out.push(Citation {
            kind: citation_kind(h.kind),
            label: h.title.clone(),
            link: h.link.clone(),
            // 支撑的论断直接取检索层给出的第一条理由：
            // 那是基于真实分数算出来的，比在这里编一句话诚实
            supports: h.reasons.first().cloned(),
        });
    }
    out
}

/// 检索命中类型 → 引用类型。
///
/// 🔴 **不再在本 crate 维护映射**：唯一真相源是 `HitKind::citation_kind()`（domain）。
/// 早先这里与 `spolia-service` 各抄了一份 `_ => File` 的 match，
/// domain 加 `Insight`/`Opportunity` 后两处都把结论性实体静默吞成「文件」引用。
/// 现在只是一个转发壳，保留本地调用点与测试的写法不变。
fn citation_kind(kind: HitKind) -> CitationKind {
    kind.citation_kind()
}

/// 后续建议问题：由命中类型推导，而不是写死一套通用问题。
///
/// 写死的建议（"你还想了解什么？"）没有信息量；
/// 基于实际结果的建议才能真正引导下一步。
fn followups_for(hits: &[SearchHit]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(MAX_FOLLOWUPS);
    let has = |k: HitKind| hits.iter().any(|h| h.kind == k);

    // `find` 已覆盖"无项目命中"的情况，无需先 `has` 再 `find`（那是重复遍历）
    if let Some(p) = hits.iter().find(|h| h.kind == HitKind::Project) {
        out.push(format!("「{}」这个项目具体做了什么？", p.title));
    }
    if has(HitKind::Asset) {
        out.push("这些资产里哪些可以直接复用到新项目？".to_string());
    }
    if has(HitKind::Capability) {
        out.push("我具备的能力可以组合出什么新项目？".to_string());
    }
    if out.is_empty() {
        out.extend(browse_suggestions());
    }
    out.truncate(MAX_FOLLOWUPS);
    out
}

/// 无结果时的浏览式建议（引导用户从"提问"切到"逛"）。
fn browse_suggestions() -> Vec<String> {
    vec![
        "我复用价值最高的资产有哪些？".to_string(),
        "哪些项目已经很久没更新了？".to_string(),
        "我掌握的技术栈集中在哪些方向？".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::{HitLink, MatchSource};

    fn hit(kind: HitKind, id: &str, title: &str) -> SearchHit {
        SearchHit {
            kind,
            id: id.into(),
            title: title.into(),
            subtitle: match kind {
                HitKind::Project => "Python · FastAPI".into(),
                HitKind::Asset => "视频管线".into(),
                _ => String::new(),
            },
            snippet: format!("{title} 的摘要"),
            score: 0.8,
            sources: vec![MatchSource::Keyword],
            reasons: vec!["关键词命中".into()],
            link: HitLink {
                page: match kind {
                    HitKind::Project => "project".into(),
                    HitKind::Asset => "assets".into(),
                    _ => "graph".into(),
                },
                param: Some(id.into()),
            },
        }
    }

    fn mixed_hits() -> Vec<SearchHit> {
        vec![
            hit(HitKind::Project, "p1", "视频管线"),
            hit(HitKind::Asset, "a1", "VideoPipeline"),
            hit(HitKind::Capability, "c1", "视频生成"),
        ]
    }

    fn ctx<'a>(hits: &'a [SearchHit], projects: usize) -> FallbackContext<'a> {
        FallbackContext {
            question: "视频生成",
            hits,
            total: hits.len(),
            project_count: projects,
            model_available: false,
            model_unavailable_reason: None,
        }
    }

    // ── 来源标注（产品红线）──────────────────────────────────────

    /// 🔴 确定性回答必须标注为 Deterministic，前端才不会误标成"AI 生成"。
    #[test]
    fn answer_is_marked_deterministic() {
        let hits = mixed_hits();
        let a = deterministic_answer(&ctx(&hits, 10));
        assert!(a.is_deterministic());
        assert!(!a.generated_by.is_model());
        assert_eq!(a.generated_by.label(), "deterministic");
    }

    /// 每种分支（有结果/无结果/空库）都必须标注来源，不能漏。
    #[test]
    fn all_branches_mark_source() {
        let hits = mixed_hits();
        let answers = vec![
            deterministic_answer(&ctx(&hits, 10)),
            deterministic_answer(&ctx(&[], 10)),
            deterministic_answer(&ctx(&[], 0)),
        ];
        for a in &answers {
            assert!(a.is_deterministic(), "存在未标注来源的回答: {}", a.content);
        }
    }

    // ── 绝不编造 ─────────────────────────────────────────────────

    /// 没检索到就直说没找到，并给出可操作建议。
    #[test]
    fn no_match_says_so_and_suggests_adjustments() {
        let a = deterministic_answer(&ctx(&[], 10));
        assert!(a.content.contains("没有找到"), "实际: {}", a.content);
        assert!(a.content.contains("视频生成"), "应回显用户的查询词");
        assert!(a.citations.is_empty(), "无结果不该有引用");
        // 给出的建议必须具体可操作，而非"请重试"
        assert!(
            a.content.contains("关键词") || a.content.contains("扫描目录"),
            "应给出具体调整方向: {}",
            a.content
        );
        assert!(!a.followups.is_empty(), "应提供后续建议降低使用门槛");
    }

    /// 🔴 空库与"没搜到"必须区分：前者该引导扫描，后者该引导换关键词。
    #[test]
    fn empty_library_prompts_scan_not_rephrase() {
        let a = deterministic_answer(&ctx(&[], 0));
        assert!(
            a.content.contains("扫描"),
            "空库应引导去扫描: {}",
            a.content
        );
        assert!(a.content.contains("设置"), "应指明去哪个设置项");
        assert!(
            !a.content.contains("没有找到与"),
            "空库不该说'没搜到'，那会让刚扫过的用户困惑"
        );
    }

    /// 空问题不该编造回答。
    #[test]
    fn blank_question_asks_for_input() {
        let mut c = ctx(&[], 10);
        c.question = "   ";
        let a = deterministic_answer(&c);
        assert!(a.content.contains("请输入"), "实际: {}", a.content);
        assert!(a.citations.is_empty());
    }

    // ── 引用必须真实可跳转 ───────────────────────────────────────

    #[test]
    fn citations_point_to_real_entities() {
        let hits = mixed_hits();
        let a = deterministic_answer(&ctx(&hits, 10));
        assert_eq!(a.citations.len(), 3);
        assert!(a.has_citations());
        for c in &a.citations {
            assert!(!c.label.is_empty());
            assert!(!c.link.page.is_empty(), "引用必须可跳转");
            assert!(c.link.param.is_some());
        }
        // 类型映射正确
        assert_eq!(a.citations[0].kind, CitationKind::Project);
        assert_eq!(a.citations[1].kind, CitationKind::Asset);
        assert_eq!(a.citations[2].kind, CitationKind::Capability);
    }

    /// 引用的"支撑论断"必须来自检索层的真实理由，不能在这里另编一句。
    #[test]
    fn citation_supports_come_from_search_reasons() {
        let mut hits = mixed_hits();
        hits[0].reasons = vec!["名称匹配".into(), "健康度 85 分".into()];
        let a = deterministic_answer(&ctx(&hits, 10));
        assert_eq!(a.citations[0].supports.as_deref(), Some("名称匹配"));
    }

    /// 引用数量必须有上限，否则回答退化成搜索结果列表。
    #[test]
    fn citations_are_capped() {
        let hits: Vec<SearchHit> = (0..50)
            .map(|i| hit(HitKind::Asset, &format!("a{i}"), &format!("资产{i}")))
            .collect();
        let c = FallbackContext {
            question: "资产",
            hits: &hits,
            total: 50,
            project_count: 10,
            model_available: false,
            model_unavailable_reason: None,
        };
        let a = deterministic_answer(&c);
        assert_eq!(a.citations.len(), MAX_CITATIONS);
        assert_eq!(MAX_CITATIONS, 6);
        // 总数仍如实告知，用户知道还有更多
        assert!(a.content.contains("50"), "应告知总命中数: {}", a.content);
    }

    /// 重复实体不该产生重复引用。
    #[test]
    fn duplicate_entities_are_deduped() {
        let hits = vec![
            hit(HitKind::Project, "p1", "视频管线"),
            hit(HitKind::Project, "p1", "视频管线"),
        ];
        let a = deterministic_answer(&ctx(&hits, 10));
        assert_eq!(a.citations.len(), 1, "同一实体只应引用一次");
    }

    // ── 正文结构 ─────────────────────────────────────────────────

    #[test]
    fn content_lists_each_kind_with_counts() {
        let hits = mixed_hits();
        let a = deterministic_answer(&ctx(&hits, 10));
        assert!(a.content.contains("**项目**（1 条）"), "实际: {}", a.content);
        assert!(a.content.contains("**资产**（1 条）"));
        assert!(a.content.contains("**能力**（1 条）"));
        // 实体名以代码格式列出，便于扫读
        assert!(a.content.contains("`视频管线`"));
        assert!(a.content.contains("`VideoPipeline`"));
    }

    /// 副标题提供上下文，用户不必点开就能判断相关性。
    #[test]
    fn content_includes_subtitle_context() {
        let hits = mixed_hits();
        let a = deterministic_answer(&ctx(&hits, 10));
        assert!(a.content.contains("Python · FastAPI"), "实际: {}", a.content);
    }

    /// 顺序必须稳定：同样的数据两次提问，回答结构一致（可快照比对）。
    #[test]
    fn kind_order_is_stable_regardless_of_hit_order() {
        let forward = mixed_hits();
        let mut reversed = mixed_hits();
        reversed.reverse();
        let a = deterministic_answer(&ctx(&forward, 10));
        let b = deterministic_answer(&ctx(&reversed, 10));
        // 两者的分类标题顺序应一致（项目 → 资产 → 能力）
        let order = |s: &str| {
            let p = s.find("**项目**").unwrap_or(usize::MAX);
            let a = s.find("**资产**").unwrap_or(usize::MAX);
            let c = s.find("**能力**").unwrap_or(usize::MAX);
            (p, a, c)
        };
        let (p1, a1, c1) = order(&a.content);
        let (p2, a2, c2) = order(&b.content);
        assert!(p1 < a1 && a1 < c1, "分类顺序应为 项目→资产→能力");
        assert_eq!((p1 < a1, a1 < c1), (p2 < a2, a2 < c2));
    }

    /// 只有一类命中时不该出现空的分类标题。
    #[test]
    fn only_present_kinds_are_listed() {
        let hits = vec![hit(HitKind::Asset, "a1", "组件A")];
        let a = deterministic_answer(&ctx(&hits, 10));
        assert!(a.content.contains("**资产**"));
        assert!(!a.content.contains("**项目**"), "无项目命中不该出现该标题");
        assert!(!a.content.contains("**能力**"));
    }

    // ── 模型状态提示 ─────────────────────────────────────────────

    /// 未配置模型时必须如实说明，不能让用户以为是 AI 生成的。
    #[test]
    fn discloses_when_model_not_used() {
        let hits = mixed_hits();
        let a = deterministic_answer(&ctx(&hits, 10));
        assert!(
            a.content.contains("离线检索式回答"),
            "应声明这是离线回答: {}",
            a.content
        );
        assert!(a.content.contains("设置"), "应指引去哪配置模型");
    }

    /// 配置了但连接失败时，应带上具体原因帮用户自查。
    #[test]
    fn discloses_model_failure_reason() {
        let hits = mixed_hits();
        let mut c = ctx(&hits, 10);
        c.model_unavailable_reason = Some("无法连接到 Ollama（http://127.0.0.1:11434）");
        let a = deterministic_answer(&c);
        assert!(a.content.contains("无法连接到 Ollama"), "实际: {}", a.content);
        assert!(a.content.contains("离线检索式回答"));
    }

    /// 模型可用时不该说"未调用模型"（那是误导）。
    #[test]
    fn model_available_changes_the_note() {
        let hits = mixed_hits();
        let mut c = ctx(&hits, 10);
        c.model_available = true;
        let a = deterministic_answer(&c);
        assert!(
            !a.content.contains("未调用大模型"),
            "模型可用时不该声称未调用: {}",
            a.content
        );
    }

    // ── 后续建议 ─────────────────────────────────────────────────

    /// 建议问题应由实际命中类型推导，而非写死通用问题。
    #[test]
    fn followups_reflect_actual_hits() {
        let hits = mixed_hits();
        let a = deterministic_answer(&ctx(&hits, 10));
        assert!(a.followups.len() <= MAX_FOLLOWUPS);
        assert!(
            a.followups.iter().any(|f| f.contains("视频管线")),
            "应引用实际命中的项目名: {:?}",
            a.followups
        );
        assert!(
            a.followups.iter().any(|f| f.contains("复用")),
            "有资产命中时应建议复用相关问题: {:?}",
            a.followups
        );
    }

    #[test]
    fn followups_fall_back_to_browse_when_no_hits() {
        let a = deterministic_answer(&ctx(&[], 10));
        assert!(!a.followups.is_empty());
        assert!(a.followups.len() <= MAX_FOLLOWUPS);
        assert!(browse_suggestions().iter().any(|s| a.followups.contains(s)));
    }

    // ── 类型映射与统计 ───────────────────────────────────────────

    #[test]
    fn citation_kind_mapping_covers_all_hit_kinds() {
        for k in [
            HitKind::Project,
            HitKind::Asset,
            HitKind::Capability,
            HitKind::Knowledge,
            HitKind::Experience,
            HitKind::Decision,
            HitKind::Idea,
        ] {
            // 所有命中类型都必须能映射，否则该类型的结果无法生成引用
            let ck = citation_kind(k);
            assert!(!ck.label_zh().is_empty(), "{k:?} 映射失败");
        }
        assert_eq!(citation_kind(HitKind::Knowledge), CitationKind::File);
    }

    #[test]
    fn counting_ignores_zero_groups() {
        let hits = vec![
            hit(HitKind::Project, "p1", "a"),
            hit(HitKind::Project, "p2", "b"),
            hit(HitKind::Asset, "a1", "c"),
        ];
        let counts = count_by_kind(&hits);
        assert_eq!(counts, vec![(HitKind::Project, 2), (HitKind::Asset, 1)]);
    }

    #[test]
    fn counting_empty_input_is_empty() {
        assert!(count_by_kind(&[]).is_empty());
    }

    // ── 边界 ─────────────────────────────────────────────────────

    /// total 大于展示数时，措辞应说明"只列了前 N 条"。
    #[test]
    fn distinguishes_partial_from_complete_results() {
        let hits: Vec<SearchHit> = (0..3)
            .map(|i| hit(HitKind::Asset, &format!("a{i}"), &format!("资产{i}")))
            .collect();
        let c = FallbackContext {
            question: "资产",
            hits: &hits,
            total: 40, // 实际命中远多于本页
            project_count: 10,
            model_available: false,
            model_unavailable_reason: None,
        };
        let a = deterministic_answer(&c);
        assert!(a.content.contains("40"), "应告知真实总数");
        assert!(a.content.contains("前 3 条"), "应说明只列了部分: {}", a.content);
    }

    #[test]
    fn long_titles_do_not_break_formatting() {
        let long = "x".repeat(500);
        let hits = vec![hit(HitKind::Project, "p1", &long)];
        let a = deterministic_answer(&ctx(&hits, 10));
        assert!(a.content.contains(&long));
        assert!(!a.content.is_empty());
    }

    /// 中文与 emoji 混排不得产生乱码（按 char 处理，不按字节）。
    #[test]
    fn unicode_content_is_preserved() {
        let hits = vec![hit(HitKind::Project, "p1", "视频生成🎬管线")];
        let a = deterministic_answer(&ctx(&hits, 10));
        assert!(a.content.contains("视频生成🎬管线"));
        assert_eq!(a.citations[0].label, "视频生成🎬管线");
    }

    #[test]
    fn constants_are_sane() {
        // 精确值断言已隐含"大于 0"，再写 `> 0` 是恒真式（clippy 会报 constant value）
        assert_eq!(MAX_CITATIONS, 6);
        assert_eq!(MAX_FOLLOWUPS, 3);
    }
}
