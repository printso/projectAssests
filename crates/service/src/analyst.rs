//! 对话式分析师（检索增强 + 证据链）。
//!
//! # 工作流
//! ```text
//! 提问 → 检索真实数据 → 组装上下文 → LLM 回答 → 校验引用 → 返回带出处的答案
//!                    ↘ (无 LLM) → 确定性回答（同样带出处）
//! ```
//!
//! # 🔴 为什么必须先检索再问模型
//! 直接问"我做过哪些视频项目"，模型只能靠项目名瞎猜。
//! 先用检索引擎召回真实记录，把它们作为上下文喂进去，
//! 模型的任务就从"凭空回答"变成"根据给定材料组织回答"——
//! 这是把幻觉率降下来的最有效手段，远比在提示词里写"不要编造"管用。
//!
//! # 引用校验
//! 模型返回的引用必须是**我们给它的候选之一**。
//! 让它自己编 id 是幻觉的主要来源，因此这里做白名单校验：
//! 不在候选集里的引用直接剔除。

use serde::{Deserialize, Serialize};
use projectassests_ai::{ChatMessage, CompletionRequest};
use projectassests_domain::{
    AnalystAnswer, AnalystQuery, AnalystTurn, AnswerSource, AuditEntry, Citation, CitationKind,
    HitKind, HitLink, JobType, RouteTarget, SearchHit, SearchQuery, SearchScope,
};
use projectassests_search::SearchEngine;

use crate::context::{ServiceContext, ServiceError};

/// 单次提问最多召回多少条上下文。
///
/// 本地 7B 模型上下文常见 8k token，每条约 150 字（含路径与理由），
/// 12 条约 2k token，留足空间给系统提示与回答。
/// 塞太多会挤掉指令遵循能力，反而答得更差。
pub const MAX_CONTEXT_HITS: usize = 12;

/// 每条上下文摘要的最大字符数。
pub const MAX_SNIPPET_CHARS: usize = 160;

/// 对话历史最大轮数（与 domain 的 `MAX_HISTORY_TURNS` 一致）。
pub const MAX_HISTORY_TURNS: usize = projectassests_domain::MAX_HISTORY_TURNS;

/// 系统提示词。
///
/// 关键约束是"只能用给定的材料"，以及引用格式必须严格
/// （否则解析不出来，等于没有证据链）。
pub const SYSTEM_PROMPT: &str = "\
你是 projectAssests 的项目资产分析师。用户是熟悉编程的开发者，正在盘点自己的历史项目与可复用代码。

你会收到「检索到的真实数据」，这是从用户本地数据库检索出来的记录，每条带有 id、标题、来源路径与匹配理由。

硬性要求：
1. **只能依据给定的材料回答**。材料里没有的信息，直接说\"本地数据中没有找到相关记录\"，不要猜测、不要用通用知识补全。
2. 每个涉及具体项目/资产的论断，都要用 [id] 标注出处，id 必须逐字来自材料的 id 字段。
   例如：\"你的视频管线项目 [p_a1b2] 已实现了多镜头生成，其中 VideoPipeline 组件 [a_x9] 复用分 0.91。\"
3. 不要编造 id、不要编造文件路径、不要编造数字（行数、提交数、评分都必须来自材料）。
4. 材料不足时明确说出来，并建议用户调整提问或先扫描对应目录。
5. 用中文回答，可以直接使用技术术语，不需要解释基础概念。
6. 严格输出 JSON，不要加 markdown 围栏、不要加解释文字。

JSON 结构：
{\"answer\":\"正文，含 [id] 引用标记\",\"followups\":[\"建议的后续问题\"]}";

/// 分析师请求（对外 API）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalystRequest {
    pub question: String,
    /// 对话历史（前端传入；服务端会裁剪到窗口内）
    #[serde(default)]
    pub history: Vec<AnalystTurn>,
    /// 限定在某项目上下文内提问
    #[serde(default)]
    pub project_id: Option<String>,
    /// 检索范围（默认全部）
    #[serde(default)]
    pub scope: SearchScope,
}

impl From<AnalystRequest> for AnalystQuery {
    fn from(r: AnalystRequest) -> Self {
        Self {
            question: r.question,
            history: r.history,
            project_id: r.project_id,
            // 敏感标记由服务端查库决定，不接受前端传值：
            // 让前端自报"这个项目不敏感"等于没有这条安全约束
            project_sensitive: false,
        }
    }
}

/// 分析师响应。
#[derive(Debug, Clone, Serialize)]
pub struct AnalystResponse {
    pub answer: AnalystAnswer,
    /// 本次检索命中的记录数（让用户知道答案基于多少材料）
    pub context_hits: usize,
    /// 检索命中总数（可能大于 context_hits，因为截断了）
    pub context_total: usize,
    /// 模型剔除了多少条编造的引用（如实告知，不静默丢弃）
    pub rejected_citations: usize,
    /// 检索是否走了子串回退（短中文查询）
    pub used_substring_fallback: bool,
    /// 检索耗时（毫秒）
    pub search_took_ms: u64,
}

/// 模型返回的 JSON 结构。
#[derive(Debug, Deserialize)]
struct RawAnswer {
    #[serde(default)]
    answer: Option<String>,
    #[serde(default, alias = "follow_ups", alias = "suggestions")]
    followups: Vec<String>,
}

/// 提问。
pub async fn ask(ctx: &ServiceContext, req: &AnalystRequest) -> Result<AnalystResponse, ServiceError> {
    let query: AnalystQuery = req.clone().into();
    if query.is_blank() {
        return Err(ServiceError::Invalid("问题不能为空".to_string()));
    }

    // 1. 查项目敏感性（决定能否走云端）——必须查库，不信前端
    let project_sensitive = match &query.project_id {
        Some(pid) => ctx
            .db
            .projects()
            .get(pid)?
            .ok_or_else(|| ServiceError::NotFound(format!("项目 {pid}")))?
            .sensitive,
        None => false,
    };

    // 2. 检索真实数据（这是回答的事实基础）
    let search = run_context_search(ctx, &query, req.scope, project_sensitive)?;

    // 3. 组装候选引用白名单
    let candidates = build_candidates(&search.hits);
    if candidates.is_empty() {
        // 没有任何材料：不调模型（它只能瞎编），直接给确定性回答
        return Ok(no_material_response(ctx, &query, &search));
    }

    // 4. 尝试 LLM 回答；不可用则降级到确定性回答。
    // 🔴 降级**不是错误**：AI 分析师页白屏是最糟的体验，
    // 给出带出处的检索式回答并如实标注来源，用户依然能得到有用信息。
    // 未配置模型是新用户的默认路径，更不该报错。
    let llm = attempt_llm(ctx, &query, &candidates, project_sensitive).await;

    let response = match llm {
        LlmOutcome::Answered { answer, rejected } => AnalystResponse {
            answer,
            context_hits: candidates.len(),
            context_total: search.total,
            rejected_citations: rejected,
            used_substring_fallback: search.used_substring_fallback,
            search_took_ms: search.took_ms,
        },
        // reason 为 None = 未配置模型；Some = 配置了但调用失败（提示里说明）
        LlmOutcome::Unavailable { reason } => {
            fallback_response(ctx, &query, &search, reason.as_deref())
        }
    };
    Ok(response)
}

/// LLM 尝试的结果。
///
/// 刻意不用 `Result`：这里没有"失败"——模型不可用是一条**正常的业务分支**，
/// 用 `Result` 会让调用方以为要 `?` 上抛错误，而正确行为是降级。
enum LlmOutcome {
    /// 模型给出了可用回答（`rejected` 是被剔除的编造引用数）
    Answered {
        answer: AnalystAnswer,
        rejected: usize,
    },
    /// 模型不可用：未配置、路由解析失败、或调用出错
    Unavailable { reason: Option<String> },
}

/// 尝试用 LLM 回答。任何环节不可用都返回 `Unavailable`，**不上抛错误**。
async fn attempt_llm(
    ctx: &ServiceContext,
    query: &AnalystQuery,
    candidates: &[Candidate],
    project_sensitive: bool,
) -> LlmOutcome {
    // 设置只读一次：resolve 与 provider_for 需要同一份配置，
    // 读两次既是多余的 IO，也可能在两次读取之间被用户改动而产生不一致
    let settings = match settings_of(ctx) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "读取设置失败，分析师降级到确定性回答");
            return LlmOutcome::Unavailable { reason: None };
        }
    };

    // 🔴 AI 总闸：用户关掉「AI 分析」后，分析师必须退回确定性回答且**不发任何请求**。
    //
    // 为什么放在这里而不是 `ask()` 入口：`attempt_llm` 是唯一会构造 provider 的地方，
    // 闸门贴着它放，才能保证没有任何路径能绕过。
    //
    // 为什么是降级而非报错：关掉 AI 是用户的主动选择，分析师页靠确定性检索
    // 仍能给出有用答案 —— 直接报错反而是惩罚用户自己的设置。
    // reason 会被 `deterministic_answer` 展示出来，所以必须说清"是被关掉的"，
    // 否则用户会以为模型坏了、跑去检查 API Key（那正是走错方向）。
    if !settings.scan.level2_enabled {
        return LlmOutcome::Unavailable {
            reason: Some("AI 分析已在 设置 → 扫描设置 中关闭".to_string()),
        };
    }

    let resolved = match ctx.router.resolve(
        &settings.llm,
        JobType::AnalyzeProject,
        project_sensitive,
    ) {
        Ok(r) => r,
        // 未配置模型：这是新用户的正常状态，不是故障
        Err(_) => return LlmOutcome::Unavailable { reason: None },
    };

    let Some(provider) = ctx.router.provider_for(
        &settings.llm,
        JobType::AnalyzeProject,
        project_sensitive,
    ) else {
        return LlmOutcome::Unavailable { reason: None };
    };

    let prompt = build_user_prompt(query, candidates);
    let request = CompletionRequest::default()
        .with_messages(build_messages(query, prompt))
        .temperature(0.3)
        .json_mode(true);

    match provider.complete(&request).await {
        Ok(resp) => {
            let (answer, rejected) =
                parse_and_verify(&resp.text, candidates, &resolved.audit_label());
            audit(ctx, &resolved, query, candidates.len(), project_sensitive, Ok(()));
            LlmOutcome::Answered { answer, rejected }
        }
        Err(e) => {
            // 🔴 失败也必须写审计：prompt 已经发出去了，网关是收到之后才拒的。
            // 分析师这条路径尤其容易漏——它会**静默降级**成确定性回答，
            // 用户在界面上只看到"离线回答"，若审计里也没有这一条，
            // 那次云端出网就彻底无痕了。
            audit(ctx, &resolved, query, candidates.len(), project_sensitive, Err(&e));
            tracing::warn!(error = %e, "分析师模型调用失败，降级到确定性回答");
            // 把原因带给前端：用户看到"无法连接到 Ollama"才知道该去启动服务，
            // 而不是困惑于"为什么这次是离线回答"
            LlmOutcome::Unavailable {
                reason: Some(e.to_string()),
            }
        }
    }
}

/// 执行上下文检索。
fn run_context_search(
    ctx: &ServiceContext,
    query: &AnalystQuery,
    scope: SearchScope,
    _sensitive: bool,
) -> Result<projectassests_domain::SearchResult, ServiceError> {
    let q = SearchQuery {
        q: query.normalized_question(),
        scope,
        filter: projectassests_domain::SearchFilter {
            project_id: query.project_id.clone(),
            ..Default::default()
        },
        sort: projectassests_domain::SortBy::Relevance,
        limit: MAX_CONTEXT_HITS as u32,
        offset: 0,
    };
    Ok(SearchEngine::new().search(&ctx.db, &q, ctx.now())?)
}

/// 上下文候选（喂给模型 + 用于校验引用）。
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub kind: HitKind,
    pub title: String,
    pub subtitle: String,
    pub snippet: String,
    pub reasons: Vec<String>,
    pub link: HitLink,
}

/// 把检索结果转成候选（去掉分数等模型不需要的字段，控制 prompt 体积）。
pub fn build_candidates(hits: &[SearchHit]) -> Vec<Candidate> {
    hits.iter()
        .take(MAX_CONTEXT_HITS)
        .map(|h| Candidate {
            id: h.id.clone(),
            kind: h.kind,
            title: h.title.clone(),
            subtitle: h.subtitle.clone(),
            // 摘要截断：完整摘要会迅速撑爆上下文
            snippet: truncate(&h.snippet, MAX_SNIPPET_CHARS),
            // 只给第一条理由：多条会让 prompt 冗长而信息增量很小
            reasons: h.reasons.iter().take(1).cloned().collect(),
            link: h.link.clone(),
        })
        .collect()
}

/// 构造用户提示词（纯函数，可单测）。
pub fn build_user_prompt(query: &AnalystQuery, candidates: &[Candidate]) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(&format!("# 用户问题\n{}\n", query.normalized_question()));

    if let Some(pid) = &query.project_id {
        out.push_str(&format!("\n（提问限定在项目上下文：{pid}）\n"));
    }

    out.push_str("\n# 检索到的真实数据\n");
    out.push_str(&format!(
        "共 {} 条。引用时只能用下面列出的 id。\n\n",
        candidates.len()
    ));
    for c in candidates {
        out.push_str(&format!(
            "- id: {}\n  类型: {}\n  标题: {}\n",
            c.id,
            c.kind.label_zh(),
            c.title
        ));
        if !c.subtitle.is_empty() {
            out.push_str(&format!("  来源: {}\n", c.subtitle));
        }
        if !c.snippet.is_empty() {
            out.push_str(&format!("  摘要: {}\n", c.snippet));
        }
        if let Some(r) = c.reasons.first() {
            out.push_str(&format!("  匹配理由: {r}\n"));
        }
        out.push('\n');
    }
    out
}

/// 构造消息列表（系统提示 + 裁剪后的历史 + 当前问题）。
fn build_messages(query: &AnalystQuery, user_prompt: String) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage::system(SYSTEM_PROMPT)];

    // 历史对话：裁剪到窗口内，保留最近 N 轮
    for turn in query.trimmed_history() {
        let msg = if turn.role == "assistant" {
            ChatMessage::assistant(&turn.content)
        } else {
            ChatMessage::user(&turn.content)
        };
        // 空消息会让部分 provider 直接 400
        if msg.is_valid() {
            messages.push(msg);
        }
    }

    messages.push(ChatMessage::user(user_prompt));
    messages
}

/// 🔴 解析模型输出并校验引用。
///
/// 返回 `(回答, 被剔除的编造引用数)`。
/// 白名单校验：模型给的 `[id]` 必须在候选集里，否则**不建立引用**。
fn parse_and_verify(
    text: &str,
    candidates: &[Candidate],
    generated_by: &str,
) -> (AnalystAnswer, usize) {
    let started = std::time::Instant::now();

    let raw = parse_raw(text);
    let raw_text = raw
        .as_ref()
        .and_then(|r| r.answer.clone())
        .filter(|a| !a.trim().is_empty());

    let Some(body) = raw_text else {
        // 模型没给出可用回答：如实说明，不硬凑
        return (
            AnalystAnswer {
                content: "模型没有返回可用内容。可以重试，或改用更具体的提问。".to_string(),
                generated_by: AnswerSource::Model(generated_by.to_string()),
                citations: Vec::new(),
                followups: Vec::new(),
                took_ms: started.elapsed().as_millis() as u64,
            },
            0,
        );
    };

    // 提取模型引用的 id，并做白名单校验
    let mentioned = extract_citation_ids(&body);
    let mut citations: Vec<Citation> = Vec::new();
    let mut rejected = 0usize;

    for id in &mentioned {
        match candidates.iter().find(|c| &c.id == id) {
            Some(c) => {
                // 去重：同一实体被多次引用只留一条
                if citations.iter().any(|x| x.link.param.as_deref() == Some(id.as_str())) {
                    continue;
                }
                citations.push(Citation {
                    kind: citation_kind(c.kind),
                    label: c.title.clone(),
                    link: c.link.clone(),
                    // 用检索层的真实理由，不自己编
                    supports: c.reasons.first().cloned(),
                });
            }
            None => {
                // 🔴 模型编造了不存在的 id：不建立引用，并计数上报。
                // 计数会回给前端，让用户知道模型这次不太靠谱。
                rejected += 1;
                tracing::warn!(fabricated_id = %id, "模型引用了不存在的记录，已忽略");
            }
        }
    }

    let followups = raw
        .map(|r| r.followups)
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !f.trim().is_empty())
        .take(3)
        .collect();

    (
        AnalystAnswer {
            // 🔴 正文**原样保留**，不做任何删改。
            //
            // 早期实现在这里剥离"未知引用标记"，理由是"用户看到 [ghost_id] 会困惑"。
            // 但它同时删掉了合法的代码语法：`list[idx]`、`arr[0]`、`foo[bar]`
            // 在语法上与引用标记**无法区分**（都是方括号包 ASCII 标识符），
            // 于是技术回答被静默篡改——对"会编码的用户"这是不可接受的。
            //
            // 正确分工：
            // - 服务端只负责**不给伪造 id 建立引用**（citations 里没有它）
            // - 前端只把 citations 里存在的 id 渲染成可点击链接，其余 `[xxx]` 保持字面文本
            // - 静默编辑模型输出本身也是一种失真：用户看到的应该是模型真正说的话
            content: body,
            generated_by: AnswerSource::Model(generated_by.to_string()),
            citations,
            followups,
            took_ms: started.elapsed().as_millis() as u64,
        },
        rejected,
    )
}

/// 解析模型 JSON（容错围栏与前后文字）。
fn parse_raw(text: &str) -> Option<RawAnswer> {
    let cleaned = crate::profile::strip_code_fence(text);
    if let Ok(raw) = serde_json::from_str::<RawAnswer>(&cleaned) {
        return Some(raw);
    }
    // 退一步：截取第一个 { 到最后一个 }（模型常在 JSON 前后加解释文字）
    if let (Some(start), Some(end)) = (cleaned.find('{'), cleaned.rfind('}'))
        && end > start
        && let Ok(raw) = serde_json::from_str::<RawAnswer>(&cleaned[start..=end])
    {
        return Some(raw);
    }
    None
}

/// 从 `bytes[start]`（必须是 `[`）处解析一个合法的引用标记。
///
/// 返回 `(标记内容, 右括号之后的位置)`；不是合法标记则返回 `None`
/// （此时调用方应按普通字符处理，并从 `start + 1` 继续）。
///
/// # 为什么只认这个字符集
/// 项目 id 由 uuid / slug 派生，必然是 ASCII 字母数字加 `_-`。
/// 收紧字符集是为了**不误伤代码语法**：`list[idx]`、`foo[bar]` 里的内容
/// 同样全是 ASCII 标识符，无法靠字面区分，因此本函数只负责"提取候选标记"，
/// 是否算引用由白名单校验（候选集里有没有这个 id）决定，
/// 而不是靠删除文本——删除会篡改技术回答（见 `parse_and_verify` 的注释）。
fn parse_marker_at(bytes: &[u8], start: usize) -> Option<(&str, usize)> {
    debug_assert_eq!(bytes.get(start), Some(&b'['));
    let content_start = start + 1;
    let mut j = content_start;
    while j < bytes.len() && bytes[j] != b']' {
        j += 1;
    }
    // 没有闭合括号，或内容为空
    if j >= bytes.len() || j == content_start {
        return None;
    }
    let inner = std::str::from_utf8(&bytes[content_start..j]).ok()?;
    // 🔴 只认 ASCII 字母数字与 `_-`：项目 id 由 uuid/slug 派生，必然是这个字符集。
    // 放宽会让 `[1, 2, 3]`、`[带 空格]`、`【注释】` 被当成引用，
    // 校验必然失败并污染 rejected 计数——用户会看到"模型编造了 3 条引用"的假警告。
    let valid = inner
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    valid.then_some((inner, j + 1))
}

/// 从正文提取 `[id]` 形式的引用标记。
pub fn extract_citation_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            match parse_marker_at(bytes, i) {
                Some((inner, next)) => {
                    // 去重：同一实体被多次引用只需一条
                    if !out.iter().any(|x| x == inner) {
                        out.push(inner.to_string());
                    }
                    i = next;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    out
}

/// 🔴 **不再在本 crate 维护映射**：唯一真相源是 `HitKind::citation_kind()`（domain）。
///
/// 早先这里是 `_ => CitationKind::File` 兜底，与 `projectassests-ai` 各抄一份。
/// domain 加了 `Insight`/`Opportunity` 后，两处都把结论性实体静默吞成「文件」引用
/// （标签 File、链接却跳洞察页），而通配符让编译器一声不吭。
/// 转发壳保留本地调用点与测试写法不变。
fn citation_kind(kind: HitKind) -> CitationKind {
    kind.citation_kind()
}

/// 无材料时的回答：不调模型，直接说明并引导。
fn no_material_response(
    ctx: &ServiceContext,
    query: &AnalystQuery,
    search: &projectassests_domain::SearchResult,
) -> AnalystResponse {
    let project_count = ctx.db.projects().count().unwrap_or(0);
    let content = if project_count == 0 {
        "本地知识库还是空的——需要先扫描项目目录，我才能回答关于它们的问题。\n\n\
         **下一步**：设置 → 扫描目录，添加代码根目录（例如 `F:/CodeProject`），然后开始扫描。"
            .to_string()
    } else {
        format!(
            "在已索引的 {} 个项目里没有找到与「{}」相关的记录。\n\n\
             可以试试：\n\
             - 换更短的关键词（中文 2 字以上即可，例如把「基于扩散模型的视频生成」换成「视频」）\n\
             - 确认相关目录已添加并扫描过\n\
             - 到「资产」或「图谱」页浏览已有内容",
            project_count,
            query.normalized_question()
        )
    };

    AnalystResponse {
        answer: AnalystAnswer {
            content,
            generated_by: AnswerSource::Deterministic,
            citations: Vec::new(),
            followups: vec![
                "我复用价值最高的资产有哪些？".to_string(),
                "哪些项目已经很久没更新了？".to_string(),
            ],
            took_ms: search.took_ms,
        },
        context_hits: 0,
        context_total: search.total,
        rejected_citations: 0,
        used_substring_fallback: search.used_substring_fallback,
        search_took_ms: search.took_ms,
    }
}

/// 确定性降级回答（无模型 / 模型失败）。
fn fallback_response(
    ctx: &ServiceContext,
    query: &AnalystQuery,
    search: &projectassests_domain::SearchResult,
    model_error: Option<&str>,
) -> AnalystResponse {
    let project_count = ctx.db.projects().count().unwrap_or(0);
    let fb_ctx = projectassests_ai::FallbackContext {
        question: &query.normalized_question(),
        hits: &search.hits,
        total: search.total,
        project_count,
        // 🔴 model_error 为 None 表示"未配置"，Some 表示"配置了但失败"。
        // 两种情况的提示不同：前者引导去配置，后者提示重试。
        model_available: false,
        model_unavailable_reason: model_error,
    };
    let answer = projectassests_ai::deterministic_answer(&fb_ctx);

    AnalystResponse {
        answer,
        context_hits: search.hits.len(),
        context_total: search.total,
        rejected_citations: 0,
        used_substring_fallback: search.used_substring_fallback,
        search_took_ms: search.took_ms,
    }
}

/// 审计记录。
///
/// 🔴 只记问题摘要与规模，**不记问题原文全文、不记代码原文**：
/// 审计日志长期留存，把用户输入与代码写进去等于存了一份明文副本。
fn audit(
    ctx: &ServiceContext,
    resolved: &projectassests_ai::ResolvedModel,
    query: &AnalystQuery,
    context_size: usize,
    sensitive: bool,
    outcome: Result<(), &projectassests_domain::AiError>,
) {
    let q = query.normalized_question();
    let base = format!(
        "分析师提问：{}…（{} 字，检索 {} 条上下文）",
        truncate(&q, 40),
        q.chars().count(),
        context_size
    );
    let at = projectassests_storage::now_utc();
    let job = JobType::AnalyzeProject.as_str().to_string();
    let project_id = query.project_id.clone();

    // 🔴 与 profile.rs 同一条规则：失败的摘要自带「（失败）」标记。
    // 只靠 `ok` 列区分的话，导出成纯文本后就看不出这条是失败的了。
    let entry = match outcome {
        Ok(()) => AuditEntry::llm_ok(at, resolved.audit_label(), resolved.route, job, base, project_id),
        Err(e) => AuditEntry::llm_failed(
            at,
            resolved.audit_label(),
            resolved.route,
            job,
            format!("{base}（失败）"),
            project_id,
            e.to_string(),
        ),
    };

    if let Err(e) = ctx.db.settings().audit(&entry) {
        tracing::error!(error = %e, ok = ?entry.ok, "写入 AI 审计日志失败");
    }
    // 🔴 敏感项目走云端是不该发生的（router 会拦），这里再记一次警告便于排查。
    // 刻意放在审计写入**之后**：这条警告针对的是"路由决策"，
    // 与调用成败无关——失败了也一样违反了 Local-First，必须记。
    if sensitive && resolved.route == RouteTarget::Cloud {
        tracing::error!(project = ?query.project_id, "敏感项目使用了云端模型，违反 Local-First 约束");
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max_chars {
        t.to_string()
    } else {
        let head: String = t.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

fn settings_of(ctx: &ServiceContext) -> Result<projectassests_domain::Settings, ServiceError> {
    Ok(ctx.db.settings().get_or_default()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{
        Asset, AssetType, CodeStats, Evidence, Project, ProjectStatus, ScanFacts,
    };

    fn ctx() -> ServiceContext {
        ServiceContext::in_memory().unwrap()
    }

    fn project(id: &str, name: &str, desc: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("/tmp/{id}"),
            description: desc.into(),
            language: "Python".into(),
            framework: "FastAPI".into(),
            created_at: None,
            updated_at: Some("2026-09-20".into()),
            last_commit_at: Some("2026-09-20".into()),
            status: ProjectStatus::Active,
            health_score: 80,
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
            scan: ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn asset(id: &str, pid: &str, name: &str) -> Asset {
        Asset {
            id: id.into(),
            project_id: pid.into(),
            asset_type: AssetType::Component,
            name: name.into(),
            description: "可复用的视频处理组件".into(),
            content: None,
            source_path: format!("src/{id}.py"),
            confidence: 0.9,
            reuse_score: 0.91,
            generality: 0.7,
            stability: 0.6,
            tags: vec!["python".into()],
            created_at: "2026-09-01".into(),
            evidence: Evidence {
                files: vec![format!("src/{id}.py")],
                ..Evidence::default()
            },
            user_feedback: None,
        }
    }

    /// 造一个有真实数据的库。
    fn seeded() -> ServiceContext {
        let c = ctx();
        c.db
            .projects()
            .upsert_batch(&[
                project("p1", "视频管线", "生成视频的完整流程"),
                project("p2", "图片工具", "批处理图片"),
            ])
            .unwrap();
        c.db
            .assets()
            .upsert(&asset("a1", "p1", "VideoPipeline"))
            .unwrap();
        c
    }

    fn hit(id: &str, kind: HitKind, title: &str) -> SearchHit {
        SearchHit {
            kind,
            id: id.into(),
            title: title.into(),
            subtitle: "Python · FastAPI".into(),
            snippet: format!("{title} 的摘要，包含视频生成的关键实现细节与调用方式说明"),
            score: 0.8,
            sources: vec![projectassests_domain::MatchSource::Keyword],
            reasons: vec!["关键词命中".into()],
            link: HitLink {
                page: "project".into(),
                param: Some(id.into()),
            },
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    // ── 提示词构造 ───────────────────────────────────────────────

    #[test]
    fn prompt_includes_question_and_candidates() {
        let q = AnalystQuery {
            question: "我做过哪些视频项目".into(),
            history: vec![],
            project_id: None,
            project_sensitive: false,
        };
        let cands = vec![Candidate {
            id: "p1".into(),
            kind: HitKind::Project,
            title: "视频管线".into(),
            subtitle: "Python · FastAPI".into(),
            snippet: "生成视频的完整流程".into(),
            reasons: vec!["关键词命中".into()],
            link: HitLink {
                page: "project".into(),
                param: Some("p1".into()),
            },
        }];
        let p = build_user_prompt(&q, &cands);
        assert!(p.contains("我做过哪些视频项目"));
        assert!(p.contains("id: p1"), "必须给出可引用的 id: {p}");
        assert!(p.contains("视频管线"));
        assert!(p.contains("关键词命中"));
        assert!(p.contains("只能用下面列出的 id"));
    }

    #[test]
    fn prompt_notes_project_scope() {
        let q = AnalystQuery {
            question: "这个项目做什么".into(),
            history: vec![],
            project_id: Some("p1".into()),
            project_sensitive: false,
        };
        let p = build_user_prompt(&q, &[]);
        assert!(p.contains("p1"), "应说明限定在哪个项目: {p}");
    }

    /// 摘要必须截断，否则十几条完整摘要就能撑爆本地模型的上下文。
    #[test]
    fn candidates_truncate_snippets() {
        let mut h = hit("p1", HitKind::Project, "长摘要项目");
        h.snippet = "x".repeat(2000);
        let cands = build_candidates(&[h]);
        assert!(
            cands[0].snippet.chars().count() <= MAX_SNIPPET_CHARS + 1,
            "摘要应被截断，实际 {} 字",
            cands[0].snippet.chars().count()
        );
    }

    #[test]
    fn candidates_are_capped() {
        let hits: Vec<SearchHit> = (0..40)
            .map(|i| hit(&format!("p{i}"), HitKind::Project, &format!("项目{i}")))
            .collect();
        assert_eq!(build_candidates(&hits).len(), MAX_CONTEXT_HITS);
        assert_eq!(MAX_CONTEXT_HITS, 12);
    }

    #[test]
    fn system_prompt_forbids_fabrication() {
        assert!(SYSTEM_PROMPT.contains("只能依据给定的材料"));
        assert!(SYSTEM_PROMPT.contains("[id]"));
        assert!(SYSTEM_PROMPT.contains("不要编造"));
        assert!(SYSTEM_PROMPT.contains("JSON"));
    }

    // ── 🔴 引用标记提取与白名单校验 ──────────────────────────────

    #[test]
    fn extracts_citation_markers() {
        let ids = extract_citation_ids("你的项目 [p1] 和组件 [a_x9] 都实现了视频生成");
        assert_eq!(ids, vec!["p1", "a_x9"]);
    }

    #[test]
    fn extract_dedupes_repeated_ids() {
        let ids = extract_citation_ids("[p1] 开头，中间 [p1]，结尾 [p1]");
        assert_eq!(ids, vec!["p1"], "同一 id 只应出现一次");
    }

    /// 非 id 形态的方括号内容不得被当成引用（否则 rejected 计数会被污染）。
    #[test]
    fn ignores_non_id_brackets() {
        let ids = extract_citation_ids("数组 [1, 2, 3] 和中文【注释】以及 [带 空格]");
        assert!(ids.is_empty(), "实际提取到: {ids:?}");
    }

    #[test]
    fn ignores_empty_brackets() {
        assert!(extract_citation_ids("空 [] 标记").is_empty());
        assert!(extract_citation_ids("没有标记").is_empty());
        assert!(extract_citation_ids("").is_empty());
    }

    /// id 允许连字符与下划线（uuid 与 slug 都常见）。
    #[test]
    fn accepts_ids_with_dash_and_underscore() {
        let ids = extract_citation_ids("[scan_project-a1b2] 与 [cap_video-gen]");
        assert_eq!(ids, vec!["scan_project-a1b2", "cap_video-gen"]);
    }

    /// 🔴 模型编造的 id 必须**不建立引用**并计数上报。
    ///
    /// 注意：正文里的 `[ghost_9]` 会被原样保留（见 parse_and_verify 的注释）——
    /// 删除它会连带篡改 `list[idx]` 这类合法代码语法。
    /// 防线是"不给它建引用" + "把剔除数告诉前端"，而不是静默改文本。
    #[test]
    fn fabricated_citations_are_rejected_and_counted() {
        let cands = vec![Candidate {
            id: "p1".into(),
            kind: HitKind::Project,
            title: "视频管线".into(),
            subtitle: String::new(),
            snippet: String::new(),
            reasons: vec!["关键词命中".into()],
            link: HitLink {
                page: "project".into(),
                param: Some("p1".into()),
            },
        }];
        let text = r#"{"answer":"你的视频管线 [p1] 很完整，另外还有个 [ghost_9] 也不错","followups":[]}"#;
        let (answer, rejected) = parse_and_verify(text, &cands, "local:qwen3:8b");

        assert_eq!(rejected, 1, "编造的 id 应被计数（前端据此提示模型不太靠谱）");
        assert_eq!(answer.citations.len(), 1, "只应为真实存在的 id 建立引用");
        assert_eq!(answer.citations[0].label, "视频管线");
        // 真实引用可跳转；编造的不在 citations 里，前端不会把它渲染成链接
        assert!(
            !answer
                .citations
                .iter()
                .any(|c| c.link.param.as_deref() == Some("ghost_9")),
            "编造的 id 不得出现在引用列表里"
        );
        // 正文原样保留（模型说的就是这些话，篡改它同样是失真）
        assert!(answer.content.contains("[p1]"), "实际: {}", answer.content);
        assert!(
            answer.content.contains("ghost_9"),
            "正文不应被编辑: {}",
            answer.content
        );
    }

    #[test]
    fn all_citations_verified_when_model_is_honest() {
        let cands: Vec<Candidate> = vec![
            Candidate {
                id: "p1".into(),
                kind: HitKind::Project,
                title: "视频管线".into(),
                subtitle: "Python".into(),
                snippet: String::new(),
                reasons: vec!["名称匹配".into()],
                link: HitLink {
                    page: "project".into(),
                    param: Some("p1".into()),
                },
            },
            Candidate {
                id: "a1".into(),
                kind: HitKind::Asset,
                title: "VideoPipeline".into(),
                subtitle: "视频管线".into(),
                snippet: String::new(),
                reasons: vec!["复用分 0.91".into()],
                link: HitLink {
                    page: "assets".into(),
                    param: Some("a1".into()),
                },
            },
        ];
        let text = r#"{"answer":"视频管线 [p1] 里的 VideoPipeline [a1] 可直接复用","followups":["怎么用？"]}"#;
        let (answer, rejected) = parse_and_verify(text, &cands, "local:m");
        assert_eq!(rejected, 0);
        assert_eq!(answer.citations.len(), 2);
        assert_eq!(answer.citations[0].kind, CitationKind::Project);
        assert_eq!(answer.citations[1].kind, CitationKind::Asset);
        // supports 必须来自检索层的真实理由，不是这里编的
        assert_eq!(answer.citations[1].supports.as_deref(), Some("复用分 0.91"));
        assert_eq!(answer.followups, vec!["怎么用？"]);
        assert!(answer.generated_by.is_model());
    }

    /// 模型没给正文时不得硬凑一个回答。
    #[test]
    fn empty_model_answer_is_reported_honestly() {
        let cands = vec![Candidate {
            id: "p1".into(),
            kind: HitKind::Project,
            title: "x".into(),
            subtitle: String::new(),
            snippet: String::new(),
            reasons: vec![],
            link: HitLink {
                page: "project".into(),
                param: Some("p1".into()),
            },
        }];
        let (answer, rejected) = parse_and_verify(r#"{"answer":"   "}"#, &cands, "local:m");
        assert!(answer.content.contains("没有返回可用内容"));
        assert!(answer.citations.is_empty());
        assert_eq!(rejected, 0);
    }

    /// 模型输出不是 JSON 时也不该 panic。
    #[test]
    fn unparsable_model_output_degrades_gracefully() {
        let (answer, rejected) = parse_and_verify("我不是 JSON", &[], "local:m");
        assert!(answer.content.contains("没有返回可用内容"));
        assert_eq!(rejected, 0);
    }

    #[test]
    fn parses_json_with_fence_and_prose() {
        let cands = vec![Candidate {
            id: "p1".into(),
            kind: HitKind::Project,
            title: "x".into(),
            subtitle: String::new(),
            snippet: String::new(),
            reasons: vec![],
            link: HitLink {
                page: "project".into(),
                param: Some("p1".into()),
            },
        }];
        for wrapped in [
            r#"```json
{"answer":"项目 [p1]","followups":[]}
```"#,
            r#"好的：{"answer":"项目 [p1]","followups":[]} 希望有用"#,
        ] {
            let (answer, _) = parse_and_verify(wrapped, &cands, "local:m");
            assert!(
                answer.content.contains("[p1]"),
                "应能解析: {wrapped} → {}",
                answer.content
            );
        }
    }

    /// followups 的常见别名都要接受（模型经常自创字段名）。
    #[test]
    fn accepts_followup_aliases() {
        let raw = parse_raw(r#"{"answer":"a","follow_ups":["x"]}"#).unwrap();
        assert_eq!(raw.followups, vec!["x"]);
        let raw2 = parse_raw(r#"{"answer":"a","suggestions":["y"]}"#).unwrap();
        assert_eq!(raw2.followups, vec!["y"]);
    }

    #[test]
    fn followups_are_capped_and_filtered() {
        let text = r#"{"answer":"正文","followups":["a","","  ","b","c","d","e"]}"#;
        let (answer, _) = parse_and_verify(text, &[], "local:m");
        assert!(answer.followups.len() <= 3, "实际 {:?}", answer.followups);
        assert!(answer.followups.iter().all(|f| !f.trim().is_empty()));
    }

    // ── 🔴 正文完整性：技术回答不得被静默篡改 ────────────────────
    //
    // 早期实现会剥离"未知引用标记"，但 `list[idx]`、`arr[0]`、`foo[bar]`
    // 与引用标记在语法上无法区分，于是代码片段被删掉、回答失真。
    // 面向会编码的用户，这是不可接受的。现在的契约是：正文原样保留，
    // 只对"候选集里存在的 id"建立可点击引用。

    /// 代码语法必须原样保留——这是本组测试存在的理由。
    #[test]
    fn code_subscript_syntax_is_preserved_verbatim() {
        let cands = vec![Candidate {
            id: "p1".into(),
            kind: HitKind::Project,
            title: "视频管线".into(),
            subtitle: String::new(),
            snippet: String::new(),
            reasons: vec![],
            link: HitLink {
                page: "project".into(),
                param: Some("p1".into()),
            },
        }];
        // 混合真实引用与代码下标语法
        let text = r#"{"answer":"在 [p1] 里用 list[idx] 取值，arr[0] 是首项，Optional[T] 表示可空","followups":[]}"#;
        let (answer, rejected) = parse_and_verify(text, &cands, "local:m");

        assert!(answer.content.contains("list[idx]"), "代码语法被篡改: {}", answer.content);
        assert!(answer.content.contains("arr[0]"), "代码语法被篡改: {}", answer.content);
        assert!(answer.content.contains("Optional[T]"), "代码语法被篡改: {}", answer.content);
        // 真实引用仍然建立
        assert_eq!(answer.citations.len(), 1);
        assert_eq!(answer.citations[0].label, "视频管线");
        // list/arr/Optional 不在候选集 → 计为 rejected，但正文不受影响
        assert_eq!(rejected, 3, "非候选 id 应被计数（但不删正文）");
    }

    /// 未知 id 不建立引用，但正文里的标记原样保留（由前端决定如何渲染）。
    #[test]
    fn unknown_id_yields_no_citation_but_keeps_text() {
        let cands = vec![Candidate {
            id: "p1".into(),
            kind: HitKind::Project,
            title: "x".into(),
            subtitle: String::new(),
            snippet: String::new(),
            reasons: vec![],
            link: HitLink {
                page: "project".into(),
                param: Some("p1".into()),
            },
        }];
        let text = r#"{"answer":"项目 [p1] 和 [ghost] 都在这","followups":[]}"#;
        let (answer, rejected) = parse_and_verify(text, &cands, "local:m");

        assert_eq!(answer.citations.len(), 1, "只应为已知 id 建立引用");
        assert_eq!(rejected, 1);
        // 🔴 正文完整保留：前端只把 citations 里的 id 渲染成链接，
        // `[ghost]` 作为普通文本显示——这是模型真正说的话
        assert_eq!(answer.content, "项目 [p1] 和 [ghost] 都在这");
    }

    /// 中文与 emoji 不得被破坏。
    #[test]
    fn multibyte_text_is_preserved() {
        let text = r#"{"answer":"视频生成管线 🎬 与 [ghost] 标记","followups":[]}"#;
        let (answer, _) = parse_and_verify(text, &[], "local:m");
        assert!(answer.content.contains("视频生成管线"));
        assert!(answer.content.contains('🎬'), "emoji 必须保留: {}", answer.content);
    }

    /// 孤立方括号、未闭合标记都不得导致 panic 或内容丢失。
    #[test]
    fn malformed_brackets_are_harmless() {
        for body in ["数组 a[0] 与 b[", "未闭合 [abc", "空 [] 标记", "嵌套 [[p1]]"] {
            let text = format!(r#"{{"answer":"{}","followups":[]}}"#, body);
            let (answer, _) = parse_and_verify(&text, &[], "local:m");
            assert_eq!(answer.content, body, "正文应原样保留: {body}");
        }
    }

    // ── 历史裁剪 ─────────────────────────────────────────────────

    #[test]
    fn history_is_trimmed_and_blanks_dropped() {
        let q = AnalystQuery {
            question: "现在呢".into(),
            history: (0..20)
                .map(|i| AnalystTurn {
                    role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                    content: if i == 19 { "   ".into() } else { format!("第{i}轮") },
                })
                .collect(),
            project_id: None,
            project_sensitive: false,
        };
        let msgs = build_messages(&q, "当前问题".into());
        // 1 条系统 + 最多 6 条历史 + 1 条当前
        assert!(msgs.len() <= 1 + MAX_HISTORY_TURNS + 1, "实际 {}", msgs.len());
        assert!(msgs.iter().all(|m| m.is_valid()), "空消息必须被丢弃");
        assert_eq!(msgs[0].role, projectassests_ai::Role::System);
        assert_eq!(msgs.last().unwrap().content, "当前问题");
    }

    #[test]
    fn messages_start_with_system_prompt() {
        let q = AnalystQuery {
            question: "hi".into(),
            history: vec![],
            project_id: None,
            project_sensitive: false,
        };
        let msgs = build_messages(&q, "问题".into());
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, SYSTEM_PROMPT);
    }

    // ── 端到端：无模型时降级 ─────────────────────────────────────

    /// 🔴 新用户（未配置模型）提问必须得到可用的回答，而不是报错白屏。
    #[test]
    fn ask_without_llm_returns_deterministic_answer() {
        let c = seeded();
        let resp = block_on(ask(
            &c,
            &AnalystRequest {
                question: "视频".into(),
                history: vec![],
                project_id: None,
                scope: SearchScope::All,
            },
        ))
        .unwrap();

        assert!(resp.answer.is_deterministic(), "未配置模型应走确定性回答");
        assert!(resp.context_hits > 0, "应检索到真实数据");
        assert!(
            !resp.answer.content.is_empty(),
            "不得返回空回答（那是白屏）"
        );
        assert!(
            resp.answer.content.contains("离线检索式回答"),
            "必须如实标注是离线回答: {}",
            resp.answer.content
        );
        // 引用必须来自真实检索结果
        assert!(!resp.answer.citations.is_empty());
        for cite in &resp.answer.citations {
            assert!(cite.link.param.is_some());
            assert!(!cite.label.is_empty());
        }
    }

    /// 没有任何材料时不调模型（它只能瞎编），直接说明并引导。
    #[test]
    fn ask_without_material_does_not_call_model() {
        let c = seeded();
        let resp = block_on(ask(
            &c,
            &AnalystRequest {
                question: "完全不存在的词汇xyzqqq".into(),
                history: vec![],
                project_id: None,
                scope: SearchScope::All,
            },
        ))
        .unwrap();
        assert_eq!(resp.context_hits, 0);
        assert!(resp.answer.is_deterministic());
        assert!(
            resp.answer.content.contains("没有找到"),
            "应明确说没找到: {}",
            resp.answer.content
        );
        assert!(resp.answer.citations.is_empty());
    }

    /// 空库时引导去扫描，而不是说"没找到"。
    #[test]
    fn ask_on_empty_library_prompts_scan() {
        let c = ctx();
        let resp = block_on(ask(
            &c,
            &AnalystRequest {
                question: "视频".into(),
                history: vec![],
                project_id: None,
                scope: SearchScope::All,
            },
        ))
        .unwrap();
        assert!(resp.answer.content.contains("扫描"), "实际: {}", resp.answer.content);
        assert!(resp.answer.content.contains("设置"));
    }

    // ── AI 总闸（level2_enabled）────────────────────────────────

    /// 云端已配置、库里有数据，只按参数切换总闸。
    ///
    /// 🔴 必须配上**可用的云端配置**：否则 `attempt_llm` 会在更早的
    /// `resolve` 分支就因"未配置"返回 `reason: None`，
    /// 闸门那条路径根本走不到，测试会被"别的原因"满足而形同虚设。
    fn seeded_with_cloud(level2: bool) -> ServiceContext {
        let c = seeded();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.cloud_base_url = "https://api.example.com/v1".into();
        s.llm.cloud_api_key = "sk-x".into();
        s.llm.cloud_model = "gpt-5-mini".into();
        s.llm.route_fast = projectassests_domain::RouteTarget::Cloud;
        s.scan.level2_enabled = level2;
        c.db.settings().save_llm(&s.llm).unwrap();
        c.db.settings().save_scan(&s.scan).unwrap();
        c
    }

    /// 🔴 关闭 AI 后：不发任何模型调用，且必须**说清是被关掉的**。
    ///
    /// 判别点在 reason 文本。若闸门失效，流程会走到 provider 调用，
    /// reason 变成"无法连接到 https://api.example.com/v1…"——
    /// 那是完全不同的字符串，所以断言 `contains("关闭")` 有真实区分度。
    ///
    /// 为什么措辞重要：用户主动关掉 AI 后看到"离线回答（原因：无法连接…）"，
    /// 会以为模型坏了、跑去检查 API Key —— 那正是走错方向。
    #[test]
    fn level2_disabled_analyst_degrades_and_says_why() {
        let c = seeded_with_cloud(false);
        let resp = block_on(ask(
            &c,
            &AnalystRequest {
                question: "视频".into(),
                history: vec![],
                project_id: None,
                scope: SearchScope::All,
            },
        ))
        .unwrap();

        assert!(
            resp.answer.is_deterministic(),
            "关闭 AI 后必须是确定性回答"
        );
        assert!(
            resp.answer.content.contains("关闭"),
            "应如实说明是被关掉的，而非模型故障: {}",
            resp.answer.content
        );
        assert!(
            resp.context_hits > 0,
            "关掉 AI 不该让检索失效——本地数据仍可查: {}",
            resp.answer.content
        );
    }

    /// 🔴 反向断言：开关**打开**时不得出现"已关闭"的说辞。
    ///
    /// 只测"关闭时有提示"的话，一个把闸门写成恒真的变异照样能过——
    /// 那会让分析师永久降级，比死开关更糟（用户打开开关也没用）。
    /// 打开后流程走到 provider，测试环境里 example.com 不可达，
    /// 于是 reason 是连接错误：同样降级，但措辞完全不同。
    #[test]
    fn level2_enabled_analyst_does_not_claim_disabled() {
        let c = seeded_with_cloud(true);
        let resp = block_on(ask(
            &c,
            &AnalystRequest {
                question: "视频".into(),
                history: vec![],
                project_id: None,
                scope: SearchScope::All,
            },
        ))
        .unwrap();

        assert!(
            !resp.answer.content.contains("AI 分析已在"),
            "开关打开时不得声称 AI 已关闭: {}",
            resp.answer.content
        );
    }

    /// 🔴🔴 端到端守门员：`ask()` 走到 LLM 调用失败后，审计里必须有一条 `ok=false`。
    ///
    /// 与 profile.rs 的 `generate_writes_failed_audit_when_llm_call_fails` 对称。
    /// # 为什么单元测试 `audit_failure_entry_marks_failed_and_leaks_nothing` 不够
    /// 那条只证明"audit 函数被喂 Err 时写对了记录"。但本次缺陷的本质是
    /// **`attempt_llm` 的 `Err` 分支压根没调用 audit**（旧代码只 `tracing::warn!`
    /// 就降级了）。分析师这条路径尤其隐蔽：失败会**静默降级**成确定性回答，
    /// 用户在界面上只看到"离线回答"，若审计也没这一条，那次云端出网就彻底无痕。
    /// 必须从 `ask()` 入口走一遍才能证明失败分支接上了 audit。
    #[test]
    fn ask_writes_failed_audit_when_llm_call_fails() {
        let c = seeded_with_cloud(true);
        assert!(
            c.db.settings().recent_audit(10).unwrap().is_empty(),
            "前提：调用前审计应为空"
        );

        // api.example.com 不可达 → complete() 失败 → 降级成确定性回答，
        // 但失败必须先写进审计
        let resp = block_on(ask(
            &c,
            &AnalystRequest {
                question: "视频".into(),
                history: vec![],
                project_id: None,
                scope: SearchScope::All,
            },
        ))
        .unwrap();
        assert!(
            resp.answer.is_deterministic(),
            "调用失败应降级为确定性回答，实际 {:?}", resp.answer.generated_by
        );

        let logs = c.db.settings().recent_audit(10).unwrap();
        assert_eq!(logs.len(), 1, "🔴 失败的模型调用必须在审计里留下恰好一条记录");
        assert_eq!(logs[0].ok, Some(false), "必须标为失败");
        assert!(
            logs[0].error.as_ref().is_some_and(|e| !e.is_empty()),
            "必须带失败原因，实际 {:?}", logs[0].error
        );
        assert!(
            logs[0].summary.contains("失败"),
            "摘要必须自带失败标记：{}", logs[0].summary
        );
    }

    #[test]
    fn blank_question_is_rejected() {
        let c = seeded();
        let err = block_on(ask(
            &c,
            &AnalystRequest {
                question: "   ".into(),
                history: vec![],
                project_id: None,
                scope: SearchScope::All,
            },
        ))
        .unwrap_err();
        assert!(matches!(err, ServiceError::Invalid(_)));
        assert_eq!(err.status_code(), 400);
    }

    /// 限定不存在的项目 → NotFound，而不是静默返回空结果。
    #[test]
    fn unknown_project_scope_reports_not_found() {
        let c = seeded();
        let err = block_on(ask(
            &c,
            &AnalystRequest {
                question: "视频".into(),
                history: vec![],
                project_id: Some("ghost".into()),
                scope: SearchScope::All,
            },
        ))
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)), "实际 {err:?}");
        assert!(err.to_string().contains("ghost"));
    }

    /// 限定项目上下文时，检索应只返回该项目的记录。
    #[test]
    fn project_scope_limits_context() {
        let c = seeded();
        let resp = block_on(ask(
            &c,
            &AnalystRequest {
                question: "视频".into(),
                history: vec![],
                project_id: Some("p1".into()),
                scope: SearchScope::All,
            },
        ))
        .unwrap();
        // p2 是图片工具，不该出现在 p1 的上下文里
        assert!(
            !resp
                .answer
                .citations
                .iter()
                .any(|x| x.link.param.as_deref() == Some("p2")),
            "限定 p1 时不该混入 p2 的记录"
        );
    }

    /// 短中文查询必须走 LIKE 回退并如实标注。
    #[test]
    fn short_chinese_query_marks_substring_fallback() {
        let c = seeded();
        let resp = block_on(ask(
            &c,
            &AnalystRequest {
                question: "视频".into(),
                history: vec![],
                project_id: None,
                scope: SearchScope::All,
            },
        ))
        .unwrap();
        assert!(resp.used_substring_fallback, "2 字中文应标记子串回退");
        assert!(resp.search_took_ms < 1000, "检索应在 1 秒内");
    }

    /// 敌意输入不得让分析师崩溃或返回错误。
    #[test]
    fn hostile_questions_degrade_gracefully() {
        let c = seeded();
        for q in [
            "\"", "'; DROP TABLE projects; --", "%", "[]", "[p1]", "<script>alert(1)</script>",
            "视频\"[ghost]\"", &"长".repeat(500),
        ] {
            let r = block_on(ask(
                &c,
                &AnalystRequest {
                    question: q.to_string(),
                    history: vec![],
                    project_id: None,
                    scope: SearchScope::All,
                },
            ));
            assert!(r.is_ok(), "问题 {q:?} 不该报错: {:?}", r.err().map(|e| e.to_string()));
        }
        // SQL 注入尝试不得真的删表
        assert_eq!(c.db.projects().count().unwrap(), 2);
    }

    /// 🔴 用户自报的 project_sensitive 必须被忽略——以库里查到的为准。
    #[test]
    fn client_cannot_claim_project_is_not_sensitive() {
        let c = seeded();
        // 库里把 p1 标记为敏感
        c.db.projects().set_sensitive("p1", true).unwrap();
        // 前端传 project_sensitive: false 也没用（From 转换里写死 false，实际查库）
        let req = AnalystRequest {
            question: "视频".into(),
            history: vec![],
            project_id: Some("p1".into()),
            scope: SearchScope::All,
        };
        let q: AnalystQuery = req.into();
        assert!(
            !q.project_sensitive,
            "前端传值不得直接采信（真实值由服务端查库）"
        );
        // 服务端查库得到 true
        let from_db = c.db.projects().get("p1").unwrap().unwrap().sensitive;
        assert!(from_db, "库里的敏感标记才是权威");
    }

    // ── 审计 ─────────────────────────────────────────────────────

    /// 🔴 审计不得记录问题原文全文与代码原文。
    #[test]
    fn audit_summary_is_truncated_and_has_no_code() {
        let c = ctx();
        let resolved = projectassests_ai::ResolvedModel {
            route: RouteTarget::Local,
            base_url: "http://127.0.0.1:11434".into(),
            model: "qwen3:8b".into(),
            api_key: None,
            forced_local: false,
        };
        let long_question = "为什么我的视频生成管线总是失败".repeat(20);
        let q = AnalystQuery {
            question: long_question.clone(),
            history: vec![],
            project_id: Some("p1".into()),
            project_sensitive: false,
        };
        audit(&c, &resolved, &q, 5, false, Ok(()));

        let entries = c.db.settings().recent_audit(10).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.model, "local:qwen3:8b");
        assert_eq!(e.route, RouteTarget::Local);
        assert_eq!(e.project_id.as_deref(), Some("p1"));
        assert_eq!(e.ok, Some(true), "成功路径应记为 Some(true)");
        assert_eq!(e.error, None, "成功时不该有错误原因");
        // 摘要必须被截断
        assert!(
            e.summary.chars().count() < 120,
            "摘要过长（{} 字），可能含全文: {}",
            e.summary.chars().count(),
            e.summary
        );
        assert!(
            !e.summary.contains(&long_question),
            "审计不该存问题全文"
        );
        assert!(!e.summary.contains("sk-"), "不得含密钥");
    }

    /// 🔴 失败路径同样不得泄漏，且摘要必须自带失败标记。
    ///
    /// 失败条目新增了 `error` 字段——这是本改动引入的**新泄漏面**：
    /// 它的内容来自 provider 的错误消息。必须确认
    /// 用户的问题全文与密钥都不会顺着它进审计日志。
    #[test]
    fn audit_failure_entry_marks_failed_and_leaks_nothing() {
        let c = ctx();
        let resolved = projectassests_ai::ResolvedModel {
            route: RouteTarget::Cloud,
            base_url: "https://example.com/v1".into(),
            model: "some-model".into(),
            api_key: Some("sk-super-secret-key-12345".into()),
            forced_local: false,
        };
        let long_question = "为什么我的视频生成管线总是失败".repeat(20);
        let q = AnalystQuery {
            question: long_question.clone(),
            history: vec![],
            project_id: Some("p1".into()),
            project_sensitive: false,
        };
        let err = projectassests_domain::AiError::Provider(
            "请求被拒绝 (400): The product is not activated".into(),
        );
        audit(&c, &resolved, &q, 5, false, Err(&err));

        let entries = c.db.settings().recent_audit(10).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.ok, Some(false), "失败路径必须记为 Some(false)");
        assert!(
            e.error.as_deref().unwrap_or("").contains("not activated"),
            "失败原因应写入，实际 {:?}", e.error
        );
        // 🔴 摘要必须自带失败标记：设置页列表以 summary 为主体，
        // 导出成纯文本后更是只剩 summary——两种呈现下都要能看出这条是失败的。
        assert!(
            e.summary.contains("失败"),
            "失败的摘要必须自带标记，实际 {:?}", e.summary
        );
        assert!(
            !e.summary.contains(&long_question),
            "审计不该存问题全文"
        );
        // 🔴 新泄漏面：error 字段不得带出密钥（reqwest 不会把 Authorization 头
        // 写进错误消息，但这里显式守住，防止日后有人改成拼接头信息）
        assert!(
            !e.error.as_deref().unwrap_or("").contains("sk-"),
            "🔴 错误原因里出现了密钥: {:?}", e.error
        );
        assert!(!e.summary.contains("sk-"), "摘要不得含密钥");
        assert_eq!(e.model, "cloud:some-model", "审计只记 provider:model，不记密钥");
    }

    // ── 类型映射与工具函数 ───────────────────────────────────────

    #[test]
    fn citation_kind_covers_all_hit_kinds() {
        assert_eq!(citation_kind(HitKind::Project), CitationKind::Project);
        assert_eq!(citation_kind(HitKind::Asset), CitationKind::Asset);
        assert_eq!(citation_kind(HitKind::Capability), CitationKind::Capability);
        // 其余类型落到 File（知识/经验/决策/创意都对应具体文件）
        assert_eq!(citation_kind(HitKind::Knowledge), CitationKind::File);
        assert_eq!(citation_kind(HitKind::Idea), CitationKind::File);
    }

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate("短", 10), "短");
        assert_eq!(truncate("  带空格  ", 100), "带空格");
        let long = "漢".repeat(100);
        let t = truncate(&long, 10);
        assert_eq!(t.chars().count(), 11); // 10 + 省略号
        assert!(t.ends_with('…'));
    }

    /// `parse_marker_at` 是 extract 与 strip 的共享语法，必须单独守护。
    #[test]
    fn parse_marker_at_validates_syntax() {
        let text = "项目 [p1] 与 [ghost_id]";
        let bytes = text.as_bytes();
        let start = text.find('[').unwrap();
        let (inner, next) = parse_marker_at(bytes, start).unwrap();
        assert_eq!(inner, "p1");
        // next 指向 ']' 之后，可直接用作扫描位置
        assert_eq!(&text[next..], " 与 [ghost_id]");
    }

    /// 非法标记必须返回 None，让调用方按普通字符处理。
    #[test]
    fn parse_marker_at_rejects_invalid() {
        for text in ["数组 [1, 2]", "空 [] 标记", "未闭合 [abc", "中文 【注释】"] {
            let bytes = text.as_bytes();
            if let Some(start) = bytes.iter().position(|&b| b == b'[') {
                assert!(
                    parse_marker_at(bytes, start).is_none(),
                    "{text:?} 不该被当成引用标记"
                );
            }
        }
    }

    // 注：此处原有三个 strip_* 测试，守护的是已废弃的"剥离未知标记"行为
    // （其中还断言"未知标记应整体删除"，与现行契约直接冲突）。
    // 现行契约的等价覆盖见前文：
    // - code_subscript_syntax_is_preserved_verbatim（代码语法不被篡改）
    // - unknown_id_yields_no_citation_but_keeps_text（正文保留、不建引用）
    // - multibyte_text_is_preserved / malformed_brackets_are_harmless

    #[test]
    fn request_converts_to_query() {
        let req = AnalystRequest {
            question: "  视频  生成 ".into(),
            history: vec![AnalystTurn {
                role: "user".into(),
                content: "之前的问题".into(),
            }],
            project_id: Some("p1".into()),
            scope: SearchScope::Assets,
        };
        let q: AnalystQuery = req.into();
        assert_eq!(q.normalized_question(), "视频 生成");
        assert_eq!(q.history.len(), 1);
        assert_eq!(q.project_id.as_deref(), Some("p1"));
        // 🔴 敏感标记不接受前端传值
        assert!(!q.project_sensitive);
    }
}
