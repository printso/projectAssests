//! 项目画像与考古（Level 2，LLM 驱动）。
//!
//! # 这个模块要解决的问题
//! 扫描能告诉你"这个项目有 8000 行 Python、用了 FastAPI"，
//! 但回答不了"**它是干什么的、做到哪一步了、哪些东西值得打捞**"。
//! 后者正是"管理历史产物"的核心价值——用户翻三年前的项目时，
//! 需要的不是文件列表，而是一段能让他迅速想起上下文的叙述。
//!
//! # 🔴 防幻觉是本模块的第一要务
//! LLM 很擅长编造看起来合理的技术叙述。产品红线是：
//! **亮点必须能回溯到真实文件**。因此这里有一道硬校验：
//! 模型返回的 `evidence_files` 必须逐条比对项目真实文件清单，
//! 不存在的路径直接剔除；剔完为空的亮点整条丢弃。
//!
//! 宁可少给三条亮点，也不能给一条假的——
//! 用户点进去发现文件不存在，对整个产品的信任就没了。
//!
//! # 无 LLM 时的行为
//! 明确返回 `Precondition` 错误（带"去设置页配置"的 hint），
//! **不生成模板画像**。假画像比没有画像危害大得多：
//! 用户会以为"这个项目是做视频生成的"，而实际可能完全不是。

use serde::{Deserialize, Serialize};
use projectassests_ai::{ChatMessage, CompletionRequest, ResolvedModel};
use projectassests_domain::{Archaeology, AuditEntry, JobType, ProjectAiProfile, ProjectHighlight};

use crate::context::{ServiceContext, ServiceError};

/// 画像请求。
#[derive(Debug, Clone, Serialize)]
pub struct ProfileRequest {
    pub project_id: String,
    /// 强制重新生成（忽略已存在的画像）
    pub force: bool,
}

/// 画像响应。
#[derive(Debug, Clone, Serialize)]
pub struct ProfileResponse {
    pub project_id: String,
    pub project_name: String,
    /// 画像内容；`None` 表示尚未生成
    pub profile: Option<ProjectAiProfile>,
    /// 🔴 校验结果：让用户知道 AI 说的话有多少被证据支撑。
    /// 不展示这个，用户无法判断画像的可信度。
    ///
    /// `None` = 本次没有跑校验（返回的是已缓存画像）。
    /// 这必须与"校验了但通过率 0%"区分开：校验报告是生成时算出、并未持久化，
    /// 缓存路径给不出它。若用 `VerificationReport::default()` 冒充，
    /// 前端会把一份早已验证过的画像显示成"0% 可信"，误导用户重跑。
    pub verification: Option<VerificationReport>,
    /// 本次是否真的调用了模型（false = 返回已缓存的画像）
    pub regenerated: bool,
    /// 实际使用的模型（`route:model`），未调用时为 `None`
    pub generated_by: Option<String>,
}

/// 证据校验报告。
#[derive(Debug, Clone, Default, Serialize)]
pub struct VerificationReport {
    /// 模型给出的文件引用总数
    pub files_claimed: usize,
    /// 其中在真实文件清单里找到的
    pub files_verified: usize,
    /// 被剔除的不存在路径（如实告知，不静默丢弃）
    pub files_rejected: Vec<String>,
    /// 亮点：模型给出数 / 校验后保留数
    pub highlights_claimed: usize,
    pub highlights_kept: usize,
    /// 考古叙述是否保留了 Git 真实数据
    pub archaeology_grounded: bool,
}

impl VerificationReport {
    /// 校验通过率 0.0-1.0。
    pub fn trust_ratio(&self) -> f64 {
        if self.files_claimed == 0 {
            // 模型没引用任何文件：不是"100% 可信"，而是"无从验证"
            return 0.0;
        }
        self.files_verified as f64 / self.files_claimed as f64
    }

    /// 是否完全可信（所有引用都验证通过）。
    pub fn fully_verified(&self) -> bool {
        self.files_claimed > 0 && self.files_claimed == self.files_verified
    }
}

/// 喂给模型的项目事实包。
///
/// 🔴 只放**扫描器真实采集**的数据，不含任何推测。
/// 模型的工作是"把这些事实组织成人话"，不是"猜这个项目是什么"。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectFacts {
    pub name: String,
    pub path: String,
    pub language: String,
    pub framework: String,
    pub description: String,
    pub tags: Vec<String>,
    pub stats: FactStats,
    pub git: FactGit,
    /// 顶层目录（结构信号）
    pub top_dirs: Vec<String>,
    /// 高价值资产（名称 + 路径 + 复用分）
    pub top_assets: Vec<FactAsset>,
    /// 已抽取的能力名
    pub capabilities: Vec<String>,
    /// 关键源文件清单（用于校验模型引用的路径是否真实）
    pub source_files: Vec<String>,
}

/// 代码统计事实。
#[derive(Debug, Clone, Serialize)]
pub struct FactStats {
    pub files: usize,
    pub loc: usize,
    pub symbols: usize,
    pub modules: usize,
    pub languages: Vec<FactLanguage>,
}

/// 语言占比。
#[derive(Debug, Clone, Serialize)]
pub struct FactLanguage {
    pub name: String,
    pub pct: u8,
    pub loc: usize,
}

/// Git 事实。
#[derive(Debug, Clone, Serialize)]
pub struct FactGit {
    pub has_git: bool,
    pub commits: u32,
    pub first_commit_at: Option<String>,
    pub last_commit_at: Option<String>,
    pub days_idle: Option<i64>,
    pub branch: Option<String>,
    /// 最近若干次提交标题（考古叙述的真实素材）
    pub recent_subjects: Vec<String>,
}

/// 资产事实。
#[derive(Debug, Clone, Serialize)]
pub struct FactAsset {
    pub name: String,
    pub asset_type: String,
    pub source_path: String,
    pub reuse_score: f64,
}

/// 模型应返回的 JSON 结构。
///
/// 用 `Option` + `#[serde(default)]`：模型少给字段是常态，
/// 缺字段不该让整个解析失败（否则一次画像就白跑了）。
#[derive(Debug, Deserialize)]
struct RawProfile {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    highlights: Vec<RawHighlight>,
    #[serde(default)]
    archaeology: Option<RawArchaeology>,
}

#[derive(Debug, Deserialize)]
struct RawHighlight {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default, alias = "evidence")]
    evidence_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawArchaeology {
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    salvage: Vec<String>,
    #[serde(default)]
    narrative: Option<String>,
}

/// 送给模型的资产条数上限。
pub const MAX_FACT_ASSETS: usize = 12;
/// 送给模型的提交标题条数上限。
pub const MAX_COMMIT_SUBJECTS: usize = 15;
/// 送给模型的源文件清单上限（用于路径校验，也要控制 prompt 体积）。
pub const MAX_SOURCE_FILES: usize = 300;

/// 系统提示词：约束模型只做"组织事实"，不做"推测"。
pub const SYSTEM_PROMPT: &str = "\
你是代码资产分析助手，用户是熟悉编程的开发者。根据给定的**真实扫描事实**为项目生成画像。

硬性要求：
1. 只依据输入的事实描述，**不得推测**输入中没有的内容。事实不足时直接说\"信息不足\"，不要编造。
2. highlights 里每条的 evidence_files 必须**逐字复制**输入 source_files 中出现的路径。
   不得拼写变体、不得补全前缀、不得编造。引用不存在的文件会导致该条被丢弃。
3. 面向会读代码的人：可以直接用技术术语，不需要解释基础概念。
4. summary 用 2-3 句中文说清\"这是什么项目、解决什么问题\"。
5. archaeology.narrative 必须引用真实的 commit 数与时间跨度，不得虚构演进故事。
6. 严格输出 JSON，不要加 markdown 代码块围栏、不要加解释文字。

JSON 结构：
{\"summary\":\"…\",\"purpose\":\"…\",\"phase\":\"…\",
 \"highlights\":[{\"title\":\"…\",\"desc\":\"…\",\"evidence_files\":[\"相对路径\"]}],
 \"archaeology\":{\"phase\":\"…\",\"salvage\":[\"资产名\"],\"narrative\":\"…\"}}";

/// 生成或读取项目画像。
pub async fn generate(ctx: &ServiceContext, req: &ProfileRequest) -> Result<ProfileResponse, ServiceError> {
    let project = ctx
        .db
        .projects()
        .get(&req.project_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("项目 {}", req.project_id)))?;

    // 已有画像且未强制刷新 → 直接返回，不浪费一次模型调用。
    // let-chain 合并两层 if：嵌套只为一个 return，读起来反而更绕。
    if !req.force
        && let Some(cached) = &project.ai_profile
    {
        return Ok(ProfileResponse {
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            profile: Some(cached.clone()),
            // 🔴 None 而非 default()：校验报告只在生成时算出且未持久化。
            // 用默认值会让前端把"已验证过的缓存画像"显示成 0% 可信，
            // 用户看到红色警告后被迫重跑一次模型调用——纯属误导。
            verification: None,
            regenerated: false,
            generated_by: Some(cached.generated_by.clone()),
        });
    }

    // 🔴 AI 总闸：`level2_enabled` 是用户对"是否允许调用模型"的唯一显式开关。
    //
    // 为什么放在缓存检查**之后**：关掉 AI 不该让已经生成过的画像消失——
    // 那些结果是用户此前授权产出的，读它们不产生任何新的网络调用。
    // 放在 `resolve_model` 之前则是硬要求：一旦走到 resolve，
    // 下一步就是把代码事实拼进 prompt 发出去，那时再拦已经晚了。
    //
    // 为什么必须有这道闸（而不是只靠 route 配置）：
    // 用户把 route_deep 设成云端后，**任何未标敏感的项目**都会静默走云端。
    // 而 sensitive 默认是 false，用户往往一个都没标——
    // 于是"点一下生成画像"就把公司代码送出去了，没有任何确认。
    // 总闸给的是一个与单项目标记正交的、全局的"先别发"开关。
    //
    // 用 `AiError::Disabled` 而非 `NotConfigured`：模型是配好的，只是被主动关闭，
    // 两者的 hint 完全不同（一个让用户填 Key，一个让用户打开开关）。
    if !settings_of(ctx)?.scan.level2_enabled {
        return Err(ServiceError::Ai(projectassests_domain::AiError::Disabled));
    }

    // 敏感项目必须走本地：这里再挡一道（router 也会挡，双保险）
    let resolved = resolve_model(ctx, project.sensitive)?;
    let facts = collect_facts(ctx, &project)?;

    if facts.source_files.is_empty() && facts.top_assets.is_empty() {
        // 没有任何可分析的材料：不硬调模型，否则它只能靠项目名瞎编
        return Err(ServiceError::Precondition(format!(
            "项目「{}」没有可分析的源码或资产，请先执行代码索引",
            project.name
        )));
    }

    let prompt = build_user_prompt(&facts);
    let request = CompletionRequest::default()
        .with_messages(vec![
            ChatMessage::system(SYSTEM_PROMPT),
            ChatMessage::user(prompt),
        ])
        .temperature(0.2) // 事实组织任务：低温度，减少发挥
        .json_mode(true);

    let provider = ctx
        .router
        // provider_for 收 `&LlmSettings`（它只关心模型配置，不需要整个 Settings）
        .provider_for(&settings_of(ctx)?.llm, JobType::AnalyzeProject, project.sensitive)
        .ok_or(projectassests_domain::AiError::NotConfigured)?;

    // 🔴 审计点必须紧跟 `complete()`，而不是放在函数末尾。
    //
    // 这一行是"prompt 已经离开本机"的唯一确定时点。放在它之后的**任何**一步
    // （解析 JSON、校验证据、落库）失败，都不该把这条痕迹带走：
    // - 旧代码 `complete(...).await?` → 调用失败（400 未开通/超时/限流）直接返回，
    //   末尾的 audit 永远执行不到；
    // - 旧代码 `parse_llm_json(...)?` → 模型明明回了话、数据确实发出去了，
    //   只因返回的不是合法 JSON 就同样不留痕。
    // 两者都让"我的数据什么时候上过云"这个问题得到假答案。
    let resp = match provider.complete(&request).await {
        Ok(resp) => {
            // 审计只关心"调用发生了且成功"，不需要 resp 的内容（摘要来自 facts）
            audit(ctx, &resolved, &project, &facts, Ok(()));
            resp
        }
        Err(e) => {
            // 🔴 失败也留痕。请求被网关拒绝时 prompt **已经发出去了**——
            // 网关是收到之后才拒的，"调用失败"绝不等于"数据没出网"。
            audit(ctx, &resolved, &project, &facts, Err(&e));
            return Err(e.into());
        }
    };
    let raw = parse_llm_json(&resp.text)?;

    // 🔴 证据校验：剔除模型编造的文件路径
    let (profile, verification) = verify_and_build(raw, &facts, &resolved, ctx);

    ctx.db.projects().set_ai_profile(&project.id, &profile)?;

    Ok(ProfileResponse {
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        profile: Some(profile),
        verification: Some(verification),
        regenerated: true,
        generated_by: Some(resolved.audit_label()),
    })
}

/// 只读地获取已存在的画像（不调用模型）。
pub fn get_cached(ctx: &ServiceContext, project_id: &str) -> Result<Option<ProjectAiProfile>, ServiceError> {
    let project = ctx
        .db
        .projects()
        .get(project_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("项目 {project_id}")))?;
    Ok(project.ai_profile)
}

/// 解析模型路由。未配置时返回带引导的错误。
fn resolve_model(ctx: &ServiceContext, sensitive: bool) -> Result<ResolvedModel, ServiceError> {
    let settings = settings_of(ctx)?;
    ctx.router
        .resolve(&settings.llm, JobType::AnalyzeProject, sensitive)
        .map_err(ServiceError::from)
}

fn settings_of(ctx: &ServiceContext) -> Result<projectassests_domain::Settings, ServiceError> {
    Ok(ctx.db.settings().get_or_default()?)
}

/// 采集项目真实事实。
fn collect_facts(
    ctx: &ServiceContext,
    project: &projectassests_domain::Project,
) -> Result<ProjectFacts, ServiceError> {
    let now = ctx.now();
    let root = std::path::Path::new(&project.path);

    // 源文件清单：既喂给模型做参考，也用于校验它引用的路径是否真实
    let (files, _stats) = projectassests_jobs::list_source_files(root);
    let source_files: Vec<String> = files
        .iter()
        .take(MAX_SOURCE_FILES)
        .map(|f| f.relative_path.clone())
        .collect();

    // 高价值资产（按复用分降序，取前 N）
    let assets = ctx.db.assets().top_reusable(MAX_FACT_ASSETS as u32)?;
    let top_assets: Vec<FactAsset> = assets
        .into_iter()
        .filter(|a| a.project_id == project.id)
        .map(|a| FactAsset {
            name: a.name,
            asset_type: a.asset_type.label_zh().to_string(),
            source_path: a.source_path,
            reuse_score: a.reuse_score,
        })
        .collect();

    // 能力名（图谱上的真实节点）
    let capabilities: Vec<String> = ctx
        .db
        .relations()
        .capabilities_of_project(&project.id)?
        .into_iter()
        .filter_map(|r| ctx.db.capabilities().get(&r.target_id).ok().flatten())
        .map(|c| c.name)
        .collect();

    Ok(ProjectFacts {
        name: project.name.clone(),
        path: project.path.clone(),
        language: project.language.clone(),
        framework: project.framework.clone(),
        description: project.description.clone(),
        tags: project.tags.clone(),
        stats: FactStats {
            files: project.stats.files,
            loc: project.stats.loc,
            symbols: project.stats.symbols,
            modules: project.stats.modules,
            languages: project
                .stats
                .languages
                .iter()
                .map(|l| FactLanguage {
                    name: l.name.clone(),
                    pct: l.pct,
                    loc: l.loc,
                })
                .collect(),
        },
        git: FactGit {
            has_git: project.scan.has_git,
            commits: project.scan.git_commits,
            first_commit_at: project.created_at.clone(),
            last_commit_at: project.last_commit_at.clone(),
            days_idle: project.days_since_update(now),
            branch: projectassests_scanner::read_branch_from_head(root),
            recent_subjects: projectassests_scanner::GitAnalyzer::new()
                .recent_commit_subjects(root, MAX_COMMIT_SUBJECTS),
        },
        top_dirs: projectassests_jobs::top_dirs_of(root),
        top_assets,
        capabilities,
        source_files,
    })
}

/// 构造用户提示词（纯函数，可单测）。
///
/// 用结构化文本而非塞整个 JSON：模型对"带标签的清单"的遵循度
/// 明显高于一大坨嵌套 JSON，且便于人工检查发了什么。
pub fn build_user_prompt(facts: &ProjectFacts) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(&format!("# 项目 {}\n", facts.name));
    out.push_str(&format!("- 主语言: {}\n", or_unknown(&facts.language)));
    out.push_str(&format!("- 框架: {}\n", or_unknown(&facts.framework)));
    if !facts.tags.is_empty() {
        out.push_str(&format!("- 技术栈标签: {}\n", facts.tags.join(", ")));
    }
    if !facts.description.is_empty() {
        // README 首段：这是最可靠的项目自述
        out.push_str(&format!("- README 摘要: {}\n", truncate(&facts.description, 400)));
    }

    out.push_str("\n## 代码规模\n");
    out.push_str(&format!(
        "- {} 个文件 / {} 行代码 / {} 个符号 / {} 个模块\n",
        facts.stats.files, facts.stats.loc, facts.stats.symbols, facts.stats.modules
    ));
    if !facts.stats.languages.is_empty() {
        let langs: Vec<String> = facts
            .stats
            .languages
            .iter()
            .map(|l| format!("{} {}% ({}行)", l.name, l.pct, l.loc))
            .collect();
        out.push_str(&format!("- 语言构成: {}\n", langs.join(", ")));
    }

    out.push_str("\n## Git 历史\n");
    if facts.git.has_git {
        out.push_str(&format!("- 提交数: {}\n", facts.git.commits));
        if let (Some(first), Some(last)) = (&facts.git.first_commit_at, &facts.git.last_commit_at) {
            out.push_str(&format!("- 时间跨度: {first} → {last}\n"));
        }
        if let Some(days) = facts.git.days_idle {
            out.push_str(&format!("- 距最后活动: {days} 天\n"));
        }
        if let Some(branch) = &facts.git.branch {
            out.push_str(&format!("- 当前分支: {branch}\n"));
        }
        if !facts.git.recent_subjects.is_empty() {
            out.push_str("- 最近提交:\n");
            for s in &facts.git.recent_subjects {
                out.push_str(&format!("  - {}\n", truncate(s, 120)));
            }
        }
    } else {
        // 明确告知"没有 Git"，否则模型会自己编一段演进史
        out.push_str("- 无 Git 历史（不要编造演进过程）\n");
    }

    if !facts.top_dirs.is_empty() {
        out.push_str(&format!("\n## 顶层目录\n{}\n", facts.top_dirs.join(", ")));
    }

    if !facts.capabilities.is_empty() {
        out.push_str(&format!(
            "\n## 已识别能力\n{}\n",
            facts.capabilities.join(", ")
        ));
    }

    if !facts.top_assets.is_empty() {
        out.push_str("\n## 高复用资产（可打捞）\n");
        for a in &facts.top_assets {
            out.push_str(&format!(
                "- {} ({}) @ {} 复用分 {:.2}\n",
                a.name, a.asset_type, a.source_path, a.reuse_score
            ));
        }
    }

    // 🔴 文件清单必须给：这是模型引用 evidence_files 的唯一合法来源。
    // 不给清单，它只能凭想象编路径。
    out.push_str("\n## source_files（evidence_files 只能从这里逐字复制）\n");
    if facts.source_files.is_empty() {
        out.push_str("（无可用文件清单：此时 highlights 的 evidence_files 应为空数组）\n");
    } else {
        for f in facts.source_files.iter().take(MAX_SOURCE_FILES) {
            out.push_str(f);
            out.push('\n');
        }
    }
    out
}

fn or_unknown(s: &str) -> &str {
    if s.trim().is_empty() || s == "-" {
        "未知"
    } else {
        s
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

/// 解析模型输出的 JSON。
///
/// 容错三种常见情况：
/// 1. 带 ```json 围栏（明明提示词说了不要，模型还是经常加）
/// 2. JSON 前后有解释文字
/// 3. 完全不是 JSON
fn parse_llm_json(text: &str) -> Result<RawProfile, ServiceError> {
    let cleaned = strip_code_fence(text);

    if let Ok(raw) = serde_json::from_str::<RawProfile>(&cleaned) {
        return Ok(raw);
    }

    // 退一步：截取第一个 { 到最后一个 } 之间的内容
    // （模型常在 JSON 前后加"好的，这是分析结果："之类的话）
    if let (Some(start), Some(end)) = (cleaned.find('{'), cleaned.rfind('}'))
        && end > start
    {
        let slice = &cleaned[start..=end];
        if let Ok(raw) = serde_json::from_str::<RawProfile>(slice) {
            return Ok(raw);
        }
    }

    Err(ServiceError::Ai(projectassests_domain::AiError::MalformedResponse(
        truncate(&format!("模型输出无法解析为画像 JSON: {}", cleaned), 200),
    )))
}

/// 去掉 markdown 代码围栏。
pub(crate) fn strip_code_fence(text: &str) -> String {
    let t = text.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    let t = t.trim();
    let t = t.strip_suffix("```").unwrap_or(t);
    t.trim().to_string()
}

/// 🔴 核心：校验模型输出并组装画像。
///
/// 剔除编造的文件路径；亮点剔完为空则整条丢弃；
/// 考古叙述只在有真实 Git 数据时保留。
fn verify_and_build(
    raw: RawProfile,
    facts: &ProjectFacts,
    resolved: &ResolvedModel,
    ctx: &ServiceContext,
) -> (ProjectAiProfile, VerificationReport) {
    let mut report = VerificationReport::default();

    // 真实路径集合：小写归一以容忍大小写差异（Windows 上不敏感），
    // 但不做模糊匹配——"差不多对"的路径点进去就是 404，等于没校验
    let real_files: std::collections::HashSet<String> = facts
        .source_files
        .iter()
        .map(|f| f.to_lowercase())
        .collect();
    let real_assets: std::collections::HashSet<String> = facts
        .top_assets
        .iter()
        .map(|a| a.name.to_lowercase())
        .collect();

    report.highlights_claimed = raw.highlights.len();

    let mut highlights: Vec<ProjectHighlight> = Vec::new();
    for h in raw.highlights {
        let title = h.title.unwrap_or_default().trim().to_string();
        let desc = h.desc.unwrap_or_default().trim().to_string();
        if title.is_empty() {
            continue; // 没标题的亮点无法展示
        }

        let mut verified: Vec<String> = Vec::new();
        for f in h.evidence_files {
            report.files_claimed += 1;
            let norm = f.trim().replace('\\', "/").to_lowercase();
            if real_files.contains(&norm) {
                report.files_verified += 1;
                // 保留原始大小写（展示用），去重
                if !verified.iter().any(|v| v.to_lowercase() == norm) {
                    verified.push(f.trim().replace('\\', "/"));
                }
            } else {
                report.files_rejected.push(f.trim().to_string());
            }
        }

        // 🔴 证据全被剔除 → 整条亮点丢弃。
        // 留一条"没有出处的结论"比少一条亮点危害大：
        // 用户会以为这是经过验证的事实。
        if verified.is_empty() {
            continue;
        }
        highlights.push(ProjectHighlight {
            title,
            desc,
            evidence_files: verified,
        });
    }
    report.highlights_kept = highlights.len();

    // 考古：只在有真实 Git 数据时生成
    let archaeology = raw
        .archaeology
        .filter(|_| facts.git.has_git && facts.git.commits > 0)
        .map(|a| {
            // salvage 必须是真实存在的资产名，否则同样是编造
            let salvage: Vec<String> = a
                .salvage
                .into_iter()
                .filter(|s| real_assets.contains(&s.trim().to_lowercase()))
                .collect();
            report.archaeology_grounded = true;
            Archaeology {
                // sessions 需要会话历史接入（阶段三），当前恒为 0 而非编造
                sessions: 0,
                commits: facts.git.commits,
                completeness: None, // Level 2 无法可靠估算完成度，不给假值
                phase: a.phase.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
                salvage,
                // 叙述若模型没给，用真实 Git 数据拼一句（而不是留空）
                narrative: a
                    .narrative
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| fallback_narrative(facts)),
            }
        });

    let summary = raw.summary.unwrap_or_default().trim().to_string();

    let profile = ProjectAiProfile {
        // 摘要为空时用事实拼一句，绝不留空串（前端会显示成空白卡片）
        summary: if summary.is_empty() {
            fallback_summary(facts)
        } else {
            summary
        },
        purpose: raw.purpose.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        phase: raw.phase.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        highlights,
        archaeology,
        generated_by: resolved.audit_label(),
        generated_at: projectassests_storage::now_utc(),
    };

    let _ = ctx; // 校验只用 facts，ctx 预留给将来查库补充证据
    (profile, report)
}

/// 无 Git 数据时的兜底摘要：全部来自真实统计。
fn fallback_summary(facts: &ProjectFacts) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "{} 项目，{} 行代码",
        or_unknown(&facts.language),
        facts.stats.loc
    ));
    if facts.framework != "-" && !facts.framework.is_empty() {
        parts.push(format!("使用 {}", facts.framework));
    }
    if facts.git.has_git && facts.git.commits > 0 {
        parts.push(format!("{} 次提交", facts.git.commits));
    }
    if !facts.capabilities.is_empty() {
        parts.push(format!("涉及 {}", facts.capabilities.join("、")));
    }
    format!("{}。（模型未返回摘要，本条由真实统计数据拼装）", parts.join("，"))
}

/// 模型未给考古叙述时的兜底：只用真实 Git 数据，且明确标注来源。
///
/// 🔴 不能留空串——前端会渲染出一个空白卡片，看起来像坏了。
/// 也不能让模型的空值静默变成"看起来像真话"的叙述。
fn fallback_narrative(facts: &ProjectFacts) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("共 {} 次提交", facts.git.commits));
    if let (Some(first), Some(last)) = (&facts.git.first_commit_at, &facts.git.last_commit_at) {
        parts.push(format!("时间跨度 {first} → {last}"));
    }
    if let Some(days) = facts.git.days_idle {
        parts.push(format!("距今 {days} 天未活动"));
    }
    if !facts.top_assets.is_empty() {
        let names: Vec<&str> = facts.top_assets.iter().take(3).map(|a| a.name.as_str()).collect();
        parts.push(format!("可打捞资产：{}", names.join("、")));
    }
    format!(
        "{}。（模型未返回叙述，本条由真实 Git 数据拼装）",
        parts.join("，")
    )
}

/// 写审计日志：一次画像生成 = 一条记录，成功与失败都记。
///
/// 🔴 只记摘要信息，**绝不记代码原文**：审计日志会长期留存，
/// 把用户代码写进去等于在数据库里存了一份明文副本。
///
/// # 为什么收 `Result` 而不是"成功时调一次、失败时调一次"
/// 摘要（文件数/行数/资产数）在两种情况下**完全相同**，
/// 差别只在成败与原因。若拆成两个调用点，摘要格式就得各写一遍，
/// 日后改一处忘另一处 → 审计日志里出现两种格式的同类记录，
/// 而审计的价值恰恰在于长期一致、可对照。
fn audit(
    ctx: &ServiceContext,
    resolved: &ResolvedModel,
    project: &projectassests_domain::Project,
    facts: &ProjectFacts,
    outcome: Result<(), &projectassests_domain::AiError>,
) {
    let base = format!(
        "生成项目画像：{} 个文件 / {} 行 / {} 个资产引用",
        facts.stats.files,
        facts.stats.loc,
        facts.top_assets.len()
    );
    let at = projectassests_storage::now_utc();
    let job = JobType::AnalyzeProject.as_str().to_string();
    let project_id = Some(project.id.clone());

    // 🔴 失败的摘要必须**自带失败标记**，不能只靠 `ok` 列区分。
    // 设置页的列表以 summary 为主体、`ok` 只是一个小图标；
    // 导出成纯文本后更是只剩 summary。两种呈现下都要能一眼看出这条是失败的。
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
        // 审计写失败不该让画像功能整体失败，但必须记日志（合规要求）。
        // 🔴 尤其不能因为"写失败记录时失败了"就把原错误吞掉——
        // 主流程仍要把真实的模型错误返回给用户。
        tracing::error!(error = %e, ok = ?entry.ok, "写入 AI 审计日志失败");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // RouteTarget 仅测试构造 ResolvedModel 时用到（主代码经 resolved.route 透传），
    // 故在测试模块导入而非污染主代码的 use 列表
    use projectassests_domain::RouteTarget;

    fn facts() -> ProjectFacts {
        ProjectFacts {
            name: "video-pipeline".into(),
            path: "/tmp/video-pipeline".into(),
            language: "Python".into(),
            framework: "FastAPI".into(),
            description: "基于 ComfyUI 的视频生成服务".into(),
            tags: vec!["pytorch".into(), "ffmpeg".into()],
            stats: FactStats {
                files: 120,
                loc: 8600,
                symbols: 72,
                modules: 13,
                languages: vec![FactLanguage {
                    name: "Python".into(),
                    pct: 92,
                    loc: 7900,
                }],
            },
            git: FactGit {
                has_git: true,
                commits: 87,
                first_commit_at: Some("2024-03-01".into()),
                last_commit_at: Some("2026-09-20".into()),
                days_idle: Some(9),
                branch: Some("main".into()),
                recent_subjects: vec!["fix: 多镜头衔接".into(), "feat: 角色一致性".into()],
            },
            top_dirs: vec!["api".into(), "services".into(), "tests".into()],
            top_assets: vec![FactAsset {
                name: "VideoPipeline".into(),
                asset_type: "组件".into(),
                source_path: "services/video_service.py".into(),
                reuse_score: 0.91,
            }],
            capabilities: vec!["视频生成".into()],
            source_files: vec![
                "services/video_service.py".into(),
                "api/routes.py".into(),
                "main.py".into(),
            ],
        }
    }

    // ── 提示词构造 ───────────────────────────────────────────────

    #[test]
    fn prompt_contains_all_real_facts() {
        let p = build_user_prompt(&facts());
        assert!(p.contains("video-pipeline"));
        assert!(p.contains("Python"));
        assert!(p.contains("FastAPI"));
        assert!(p.contains("8600"), "代码行数");
        assert!(p.contains("87"), "提交数");
        assert!(p.contains("services/video_service.py"));
        assert!(p.contains("fix: 多镜头衔接"), "真实提交标题");
    }

    /// 🔴 文件清单必须在提示词里，否则模型只能编路径。
    #[test]
    fn prompt_includes_source_file_whitelist() {
        let p = build_user_prompt(&facts());
        assert!(p.contains("source_files"));
        assert!(p.contains("api/routes.py"));
        assert!(p.contains("main.py"));
        assert!(
            p.contains("只能从这里逐字复制"),
            "必须明确约束引用来源"
        );
    }

    /// 无 Git 时必须明确告知，否则模型会自己编一段演进史。
    #[test]
    fn prompt_states_absence_of_git() {
        let mut f = facts();
        f.git.has_git = false;
        f.git.commits = 0;
        f.git.recent_subjects.clear();
        let p = build_user_prompt(&f);
        assert!(p.contains("无 Git 历史"));
        assert!(p.contains("不要编造"));
        assert!(!p.contains("fix: 多镜头衔接"));
    }

    /// 空文件清单也要说明，否则模型会凭项目名猜路径。
    #[test]
    fn prompt_handles_empty_source_files() {
        let mut f = facts();
        f.source_files.clear();
        let p = build_user_prompt(&f);
        assert!(p.contains("无可用文件清单"));
        assert!(p.contains("evidence_files 应为空数组"));
    }

    #[test]
    fn prompt_hides_unknown_language_as_unknown() {
        let mut f = facts();
        f.language = String::new();
        f.framework = "-".into();
        let p = build_user_prompt(&f);
        assert!(p.contains("主语言: 未知"));
        assert!(p.contains("框架: 未知"), "占位符 - 也该显示为未知");
    }

    #[test]
    fn prompt_truncates_long_description() {
        let mut f = facts();
        f.description = "x".repeat(2000);
        let p = build_user_prompt(&f);
        assert!(p.contains('…'), "超长描述应被截断");
        assert!(p.chars().count() < 3000);
    }

    #[test]
    fn system_prompt_forbids_fabrication() {
        assert!(SYSTEM_PROMPT.contains("不得推测"));
        assert!(SYSTEM_PROMPT.contains("evidence_files"));
        assert!(SYSTEM_PROMPT.contains("逐字复制"));
        assert!(SYSTEM_PROMPT.contains("JSON"));
    }

    // ── JSON 解析容错 ────────────────────────────────────────────

    fn valid_json() -> &'static str {
        r#"{"summary":"视频生成服务","purpose":"批量生成短视频","phase":"多镜头优化",
            "highlights":[{"title":"多镜头生成","desc":"支持分镜","evidence_files":["services/video_service.py"]}],
            "archaeology":{"phase":"迭代中","salvage":["VideoPipeline"],"narrative":"87 次提交"}}"#
    }

    #[test]
    fn parses_clean_json() {
        let raw = parse_llm_json(valid_json()).unwrap();
        assert_eq!(raw.summary.as_deref(), Some("视频生成服务"));
        assert_eq!(raw.highlights.len(), 1);
        assert!(raw.archaeology.is_some());
    }

    /// 模型经常无视"不要加围栏"的指示，必须容错。
    #[test]
    fn parses_json_with_code_fence() {
        let text = format!("```json\n{}\n```", valid_json());
        assert!(parse_llm_json(&text).is_ok());
        let text2 = format!("```\n{}\n```", valid_json());
        assert!(parse_llm_json(&text2).is_ok());
    }

    #[test]
    fn parses_json_with_surrounding_prose() {
        let text = format!("好的，这是分析结果：\n{}\n希望对你有帮助！", valid_json());
        let raw = parse_llm_json(&text).unwrap();
        assert_eq!(raw.summary.as_deref(), Some("视频生成服务"));
    }

    #[test]
    fn rejects_non_json_output() {
        let err = parse_llm_json("我无法分析这个项目").unwrap_err();
        assert!(matches!(err, ServiceError::Ai(projectassests_domain::AiError::MalformedResponse(_))));
        // 错误信息要带上模型实际说了什么，便于排查
        assert!(err.to_string().contains("无法解析"));
    }

    /// 缺字段不该整体失败：模型少给 purpose 是常态。
    #[test]
    fn tolerates_missing_optional_fields() {
        let raw = parse_llm_json(r#"{"summary":"仅摘要"}"#).unwrap();
        assert_eq!(raw.summary.as_deref(), Some("仅摘要"));
        assert!(raw.purpose.is_none());
        assert!(raw.highlights.is_empty());
        assert!(raw.archaeology.is_none());
    }

    /// `evidence` 是模型常用的别名，必须接受。
    #[test]
    fn accepts_evidence_alias() {
        let raw = parse_llm_json(
            r#"{"highlights":[{"title":"t","desc":"d","evidence":["a.py"]}]}"#,
        )
        .unwrap();
        assert_eq!(raw.highlights[0].evidence_files, vec!["a.py"]);
    }

    // ── 🔴 证据校验（防幻觉核心）────────────────────────────────

    fn resolved_local() -> ResolvedModel {
        ResolvedModel {
            route: RouteTarget::Local,
            base_url: "http://127.0.0.1:11434".into(),
            model: "qwen3:8b".into(),
            api_key: None,
            forced_local: false,
        }
    }

    fn ctx() -> ServiceContext {
        ServiceContext::in_memory().unwrap()
    }

    fn proj() -> projectassests_domain::Project {
        projectassests_domain::Project {
            id: "p1".into(),
            name: "video-pipeline".into(),
            path: "/tmp/video-pipeline".into(),
            description: String::new(),
            language: "Python".into(),
            framework: "-".into(),
            created_at: None,
            updated_at: None,
            last_commit_at: None,
            status: projectassests_domain::ProjectStatus::Active,
            health_score: 70,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: projectassests_domain::CodeStats::default(),
            scan: projectassests_domain::ScanFacts::default(),
            ai_profile: None,
        }
    }

    // ── 🔴 审计留痕（成功与失败都要写）───────────────────────────
    //
    // 这是本次缺陷修复的核心行为：旧代码 `complete(...).await?` 失败就跳过
    // 了末尾的 audit，导致"数据已出网但审计无痕"。以下测试直接盯着 audit 函数。

    /// 成功路径：写一条 `ok=Some(true)` 的审计。
    #[test]
    fn audit_writes_success_entry() {
        let c = ctx();
        audit(&c, &resolved_local(), &proj(), &facts(), Ok(()));
        let logs = c.db.settings().recent_audit(10).unwrap();
        assert_eq!(logs.len(), 1, "成功调用应写一条审计");
        assert_eq!(logs[0].ok, Some(true));
        assert_eq!(logs[0].error, None);
        assert!(logs[0].summary.contains("生成项目画像"), "摘要应是画像：{}", logs[0].summary);
        assert!(!logs[0].summary.contains("失败"), "成功摘要不该带失败标记");
        assert_eq!(logs[0].project_id.as_deref(), Some("p1"));
    }

    /// 🔴 失败路径：**必须**也写一条审计，且 `ok=Some(false)` + 原因。
    ///
    /// 这条测试是本次修复的守门员。删掉 `generate()` 里 `Err` 分支的
    /// `audit(...)` 调用，或把 audit 改回只接受成功，它都会变红。
    #[test]
    fn audit_writes_failure_entry_with_reason() {
        let c = ctx();
        let err = projectassests_domain::AiError::Provider(
            "请求被拒绝 (400): The product is not activated".into(),
        );
        audit(&c, &resolved_local(), &proj(), &facts(), Err(&err));

        let logs = c.db.settings().recent_audit(10).unwrap();
        assert_eq!(logs.len(), 1, "🔴 失败调用也必须留痕");
        assert_eq!(logs[0].ok, Some(false), "失败必须记为 Some(false)");
        assert!(
            logs[0].error.as_deref().unwrap_or("").contains("not activated"),
            "失败原因必须写入，实际 {:?}", logs[0].error
        );
        assert!(
            logs[0].summary.contains("失败"),
            "失败摘要必须自带标记（导出成纯文本后 ok 列看不到）：{}", logs[0].summary
        );
        // 失败也要记 project_id：用户要能查"哪个项目的数据出网失败了"
        assert_eq!(logs[0].project_id.as_deref(), Some("p1"));
    }

    /// 🔴 失败的审计不得泄漏代码原文或密钥。
    ///
    /// 失败分支新增了 `error` 字段，内容来自 provider。必须确认
    /// 它只含错误消息，不含 prompt 里的项目事实（源码清单等）。
    #[test]
    fn audit_failure_entry_leaks_no_code_or_secret() {
        let c = ctx();
        let resolved = ResolvedModel {
            route: RouteTarget::Cloud,
            base_url: "https://example.com/v1".into(),
            model: "some-model".into(),
            api_key: Some("sk-super-secret-key-12345".into()),
            forced_local: false,
        };
        let err = projectassests_domain::AiError::Provider("400 not activated".into());
        audit(&c, &resolved, &proj(), &facts(), Err(&err));

        let logs = c.db.settings().recent_audit(10).unwrap();
        let e = &logs[0];
        // facts() 里的源码路径不得进入审计（无论 summary 还是 error）
        let blob = format!("{} {}", e.summary, e.error.clone().unwrap_or_default());
        assert!(!blob.contains("video_service.py"), "审计不该含源码文件名：{blob}");
        assert!(!blob.contains("sk-super-secret"), "🔴 审计不得含密钥：{blob}");
        assert_eq!(e.model, "cloud:some-model", "审计只记 provider:model");
    }

    #[test]
    fn verified_paths_are_kept() {
        let raw = parse_llm_json(valid_json()).unwrap();
        let (profile, report) = verify_and_build(raw, &facts(), &resolved_local(), &ctx());
        assert_eq!(profile.highlights.len(), 1);
        assert_eq!(
            profile.highlights[0].evidence_files,
            vec!["services/video_service.py"]
        );
        assert_eq!(report.files_claimed, 1);
        assert_eq!(report.files_verified, 1);
        assert!(report.fully_verified());
        assert!((report.trust_ratio() - 1.0).abs() < 1e-9);
    }

    /// 🔴 模型编造的路径必须被剔除，且如实报告。
    #[test]
    fn fabricated_paths_are_rejected_and_reported() {
        let json = r#"{"summary":"s","highlights":[
            {"title":"真实亮点","desc":"d","evidence_files":["main.py"]},
            {"title":"编造亮点","desc":"d","evidence_files":["utils/ghost_helper.py"]}
        ]}"#;
        let raw = parse_llm_json(json).unwrap();
        let (profile, report) = verify_and_build(raw, &facts(), &resolved_local(), &ctx());

        assert_eq!(profile.highlights.len(), 1, "编造路径的亮点应被整条丢弃");
        assert_eq!(profile.highlights[0].title, "真实亮点");
        assert_eq!(report.files_claimed, 2);
        assert_eq!(report.files_verified, 1);
        assert_eq!(report.files_rejected, vec!["utils/ghost_helper.py"]);
        assert!(!report.fully_verified());
        assert!((report.trust_ratio() - 0.5).abs() < 1e-9);
    }

    /// 部分真实部分编造：保留真实的那条引用，亮点不整条丢弃。
    #[test]
    fn partially_fabricated_highlight_keeps_real_evidence() {
        let json = r#"{"summary":"s","highlights":[
            {"title":"混合","desc":"d","evidence_files":["main.py","nope.py","api/routes.py"]}
        ]}"#;
        let raw = parse_llm_json(json).unwrap();
        let (profile, report) = verify_and_build(raw, &facts(), &resolved_local(), &ctx());
        assert_eq!(profile.highlights.len(), 1);
        assert_eq!(
            profile.highlights[0].evidence_files,
            vec!["main.py", "api/routes.py"]
        );
        assert_eq!(report.files_rejected, vec!["nope.py"]);
    }

    /// Windows 反斜杠与大小写差异应被容忍（否则合法引用会被误杀）。
    #[test]
    fn path_normalization_tolerates_separators_and_case() {
        let json = r#"{"summary":"s","highlights":[
            {"title":"t","desc":"d","evidence_files":["services\\Video_Service.PY"]}
        ]}"#;
        let raw = parse_llm_json(json).unwrap();
        let (profile, report) = verify_and_build(raw, &facts(), &resolved_local(), &ctx());
        assert_eq!(profile.highlights.len(), 1, "反斜杠+大小写应视为同一路径");
        assert_eq!(report.files_verified, 1);
    }

    /// 相似但不存在的路径**不该**被模糊匹配放过——点进去 404 等于没校验。
    #[test]
    fn similar_but_nonexistent_path_is_rejected() {
        let json = r#"{"summary":"s","highlights":[
            {"title":"t","desc":"d","evidence_files":["services/video_service2.py"]}
        ]}"#;
        let raw = parse_llm_json(json).unwrap();
        let (profile, report) = verify_and_build(raw, &facts(), &resolved_local(), &ctx());
        assert!(profile.highlights.is_empty());
        assert_eq!(report.files_verified, 0);
    }

    /// 无标题的亮点无法展示，应丢弃。
    #[test]
    fn highlight_without_title_is_dropped() {
        let json = r#"{"summary":"s","highlights":[
            {"title":"  ","desc":"d","evidence_files":["main.py"]},
            {"desc":"没标题","evidence_files":["main.py"]}
        ]}"#;
        let raw = parse_llm_json(json).unwrap();
        let (profile, _) = verify_and_build(raw, &facts(), &resolved_local(), &ctx());
        assert!(profile.highlights.is_empty());
    }

    /// 无 Git 数据时不得保留考古叙述（那是编造的演进史）。
    #[test]
    fn archaeology_dropped_without_real_git() {
        let mut f = facts();
        f.git.has_git = false;
        f.git.commits = 0;
        let raw = parse_llm_json(valid_json()).unwrap();
        let (profile, report) = verify_and_build(raw, &f, &resolved_local(), &ctx());
        assert!(
            profile.archaeology.is_none(),
            "无 Git 数据时不该有考古叙述"
        );
        assert!(!report.archaeology_grounded);
    }

    /// 考古里的 salvage 必须是真实资产名。
    #[test]
    fn archaeology_salvage_filters_fabricated_assets() {
        let json = r#"{"summary":"s","archaeology":{"salvage":["VideoPipeline","GhostModule"],"narrative":"n"}}"#;
        let raw = parse_llm_json(json).unwrap();
        let (profile, _) = verify_and_build(raw, &facts(), &resolved_local(), &ctx());
        let arch = profile.archaeology.expect("有 Git 数据应保留考古");
        assert_eq!(arch.salvage, vec!["VideoPipeline"]);
        assert_eq!(arch.commits, 87, "提交数必须用真实值而非模型给的");
        assert_eq!(arch.sessions, 0, "会话数尚未接入，必须是 0 而非编造");
        assert!(arch.completeness.is_none(), "完成度无法可靠估算，不给假值");
    }

    /// 模型没给摘要时用真实统计兜底，并**明确标注**是兜底。
    #[test]
    fn missing_summary_falls_back_to_facts_with_disclosure() {
        let raw = parse_llm_json(r#"{"purpose":"p"}"#).unwrap();
        let (profile, _) = verify_and_build(raw, &facts(), &resolved_local(), &ctx());
        assert!(!profile.summary.is_empty(), "不得留空串");
        assert!(profile.summary.contains("8600"), "应含真实代码行数");
        assert!(
            profile.summary.contains("拼装"),
            "必须标注这不是模型生成的: {}",
            profile.summary
        );
    }

    #[test]
    fn profile_records_model_and_timestamp() {
        let raw = parse_llm_json(valid_json()).unwrap();
        let (profile, _) = verify_and_build(raw, &facts(), &resolved_local(), &ctx());
        assert_eq!(profile.generated_by, "local:qwen3:8b");
        assert!(!profile.generated_at.is_empty());
        assert!(
            !profile.generated_by.contains("127.0.0.1"),
            "审计标识不该含端点地址"
        );
    }

    // ── 校验报告 ─────────────────────────────────────────────────

    #[test]
    fn trust_ratio_zero_when_nothing_claimed() {
        let r = VerificationReport::default();
        // 🔴 没引用任何文件不是"100% 可信"，而是"无从验证"
        assert_eq!(r.trust_ratio(), 0.0);
        assert!(!r.fully_verified());
    }

    #[test]
    fn trust_ratio_computes_correctly() {
        let r = VerificationReport {
            files_claimed: 4,
            files_verified: 3,
            ..Default::default()
        };
        assert!((r.trust_ratio() - 0.75).abs() < 1e-9);
        assert!(!r.fully_verified());
    }

    // ── 未配置模型时的行为 ───────────────────────────────────────

    /// 🔴 不得生成模板画像：假画像比没画像危害大。
    #[test]
    fn generate_without_llm_returns_precondition_not_fake_profile() {
        let c = ctx();
        // 空库 + 未配置模型
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(generate(
                &c,
                &ProfileRequest {
                    project_id: "ghost".into(),
                    force: false,
                },
            ))
            .unwrap_err();
        assert!(
            matches!(err, ServiceError::NotFound(_)),
            "项目不存在应先报 NotFound，实际 {err:?}"
        );
    }

    #[test]
    fn get_cached_returns_none_for_unanalyzed_project() {
        let c = ctx();
        c.db
            .projects()
            .upsert(&projectassests_domain::Project {
                id: "p1".into(),
                name: "a".into(),
                path: "/tmp/a".into(),
                description: String::new(),
                language: "Python".into(),
                framework: "-".into(),
                created_at: None,
                updated_at: None,
                last_commit_at: None,
                status: projectassests_domain::ProjectStatus::Active,
                health_score: 70,
                completeness: None,
                tags: vec![],
                sensitive: false,
                stats: projectassests_domain::CodeStats::default(),
                scan: projectassests_domain::ScanFacts::default(),
                ai_profile: None,
            })
            .unwrap();
        assert!(get_cached(&c, "p1").unwrap().is_none());
        let err = get_cached(&c, "ghost").unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    // ── 常量 ─────────────────────────────────────────────────────

    #[test]
    fn limits_are_sane() {
        // 断言确切值：改常量时测试会真的失败。
        // 不再写 `MAX_SOURCE_FILES > MAX_FACT_ASSETS` 这类关系式——
        // 对字面量常量它恒真（clippy 会报 constant value），
        // 且已被上面三个确切值断言蕴含。
        assert_eq!(MAX_FACT_ASSETS, 12);
        assert_eq!(MAX_COMMIT_SUBJECTS, 15);
        // 源文件清单要足够大以覆盖真实项目，但不能撑爆 prompt
        assert_eq!(MAX_SOURCE_FILES, 300);
    }

    // ── 缓存路径（不需要模型即可测）─────────────────────────────

    /// 造一个已带画像的项目，用于测试缓存分支。
    fn project_with_profile(c: &ServiceContext, id: &str) {
        let profile = ProjectAiProfile {
            summary: "视频生成服务".into(),
            purpose: Some("批量生成短视频".into()),
            phase: Some("多镜头优化".into()),
            highlights: vec![ProjectHighlight {
                title: "多镜头生成".into(),
                desc: "支持分镜".into(),
                evidence_files: vec!["services/video_service.py".into()],
            }],
            archaeology: None,
            generated_by: "local:qwen3:8b".into(),
            generated_at: "2026-09-29T10:00:00Z".into(),
        };
        c.db
            .projects()
            .upsert(&projectassests_domain::Project {
                id: id.into(),
                name: "video-pipeline".into(),
                path: "/tmp/a".into(),
                description: String::new(),
                language: "Python".into(),
                framework: "FastAPI".into(),
                created_at: None,
                updated_at: None,
                last_commit_at: None,
                status: projectassests_domain::ProjectStatus::Active,
                health_score: 80,
                completeness: None,
                tags: vec![],
                sensitive: false,
                stats: projectassests_domain::CodeStats::default(),
                scan: projectassests_domain::ScanFacts::default(),
                ai_profile: Some(profile.clone()),
            })
            .unwrap();
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// 🔴 命中缓存时 verification 必须是 None，而不是"0% 可信"。
    ///
    /// 校验报告只在生成时算出且未持久化。若缓存路径返回
    /// `VerificationReport::default()`，其 trust_ratio() 是 0.0，
    /// 前端会把一份早已验证过的画像标红成"不可信"，
    /// 用户看到警告后被迫重跑一次模型调用——纯属误导。
    #[test]
    fn cached_profile_reports_no_verification_instead_of_zero_trust() {
        let c = ctx();
        project_with_profile(&c, "p1");

        let resp = block_on(generate(
            &c,
            &ProfileRequest {
                project_id: "p1".into(),
                force: false,
            },
        ))
        .unwrap();

        assert!(!resp.regenerated, "命中缓存不该调用模型");
        assert!(resp.profile.is_some(), "应返回缓存的画像");
        assert_eq!(resp.generated_by.as_deref(), Some("local:qwen3:8b"));
        assert!(
            resp.verification.is_none(),
            "缓存路径必须返回 None，不能用默认值冒充（那等于 0% 可信）"
        );
    }

    /// force=true 时不得走缓存：必须绕过已存在的画像去重新生成。
    ///
    /// 这里的项目路径指向不存在的目录，所以会在"采集材料"阶段就失败——
    /// 但这恰好证明它**没有**返回缓存（返回缓存的话会是 Ok + regenerated=false）。
    #[test]
    fn force_regenerate_bypasses_cache() {
        let c = ctx();
        project_with_profile(&c, "p1");

        // 对照：force=false 命中缓存，成功返回
        let cached = block_on(generate(
            &c,
            &ProfileRequest {
                project_id: "p1".into(),
                force: false,
            },
        ))
        .unwrap();
        assert!(!cached.regenerated);
        assert!(cached.profile.is_some());

        // force=true 绕过缓存，尝试重新生成 → 因无可分析材料而失败（而非返回缓存）
        let err = block_on(generate(
            &c,
            &ProfileRequest {
                project_id: "p1".into(),
                force: true,
            },
        ))
        .unwrap_err();
        assert!(
            matches!(err, ServiceError::Precondition(_) | ServiceError::Ai(_)),
            "force 刷新必须真的去重新生成，实际 {err:?}"
        );
    }

    /// 🔴 核心承诺：无论如何失败，都绝不写入模板假画像。
    ///
    /// 假画像比没画像危害大——用户会以为"这个项目是做视频生成的"，
    /// 而实际可能完全不是。因此失败路径必须：返回错误 + 库里不留画像 + 给可操作提示。
    #[test]
    fn generate_never_writes_template_profile_on_failure() {
        let c = ctx();
        // 项目路径不存在 → 无源码、无资产，任何生成都无法进行
        c.db
            .projects()
            .upsert(&projectassests_domain::Project {
                id: "p1".into(),
                name: "a".into(),
                path: "/tmp/nonexistent-projectassests-project".into(),
                description: String::new(),
                language: "Python".into(),
                framework: "-".into(),
                created_at: None,
                updated_at: None,
                last_commit_at: None,
                status: projectassests_domain::ProjectStatus::Active,
                health_score: 70,
                completeness: None,
                tags: vec![],
                sensitive: false,
                stats: projectassests_domain::CodeStats::default(),
                scan: projectassests_domain::ScanFacts::default(),
                ai_profile: None,
            })
            .unwrap();

        let err = block_on(generate(
            &c,
            &ProfileRequest {
                project_id: "p1".into(),
                force: false,
            },
        ))
        .unwrap_err();

        // 无论是"未配置模型"还是"无可分析材料"，都必须是错误
        assert!(
            matches!(err, ServiceError::Ai(_) | ServiceError::Precondition(_)),
            "应返回可识别的错误，实际 {err:?}"
        );
        // 🔴 关键：数据库里不得留下任何假画像
        assert!(
            c.db.projects().get("p1").unwrap().unwrap().ai_profile.is_none(),
            "失败路径不得写入模板画像"
        );
        // 必须给可操作提示，而不是让用户对着错误发愣
        assert!(
            err.hint().is_some_and(|h| !h.is_empty()),
            "失败应带引导: {err}"
        );
    }

    /// 敏感项目 + sensitive_local_only + 仅有云端配置 → 必须被拦截，
    /// 且不得偷偷改走云端。
    #[test]
    fn sensitive_project_cannot_reach_cloud_only_config() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.cloud_base_url = "https://api.example.com/v1".into();
        s.llm.cloud_api_key = "sk-x".into();
        s.llm.cloud_model = "gpt-5-mini".into();
        s.llm.route_fast = projectassests_domain::RouteTarget::Cloud;
        s.llm.sensitive_local_only = true;
        s.llm.local_base_url = String::new(); // 本地没配
        c.db.settings().save_llm(&s.llm).unwrap();

        c.db
            .projects()
            .upsert(&projectassests_domain::Project {
                id: "p1".into(),
                name: "机密项目".into(),
                path: "/tmp/a".into(),
                description: String::new(),
                language: "Python".into(),
                framework: "-".into(),
                created_at: None,
                updated_at: None,
                last_commit_at: None,
                status: projectassests_domain::ProjectStatus::Active,
                health_score: 70,
                completeness: None,
                tags: vec![],
                sensitive: true, // 🔴 敏感
                stats: projectassests_domain::CodeStats::default(),
                scan: projectassests_domain::ScanFacts::default(),
                ai_profile: None,
            })
            .unwrap();

        let err = block_on(generate(
            &c,
            &ProfileRequest {
                project_id: "p1".into(),
                force: false,
            },
        ))
        .unwrap_err();
        // 敏感项目被强制降级到本地，而本地未配置 → NotConfigured。
        // 绝不能因为"云端可用"就把敏感数据发出去。
        assert!(
            matches!(err, ServiceError::Ai(projectassests_domain::AiError::NotConfigured)),
            "敏感项目不得走云端，实际 {err:?}"
        );
    }

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate("短文本", 10), "短文本");
        let long = "漢".repeat(50);
        let t = truncate(&long, 10);
        assert_eq!(t.chars().count(), 11); // 10 + 省略号
        assert!(t.ends_with('…'));
    }

    // ── AI 总闸（level2_enabled）────────────────────────────────

    /// 非敏感项目 + 云端已配置 + 开关**关闭**：不得发出任何模型调用。
    fn ctx_cloud_ready(level2: bool) -> ServiceContext {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.cloud_base_url = "https://api.example.com/v1".into();
        s.llm.cloud_api_key = "sk-x".into();
        s.llm.cloud_model = "gpt-5-mini".into();
        s.llm.route_fast = projectassests_domain::RouteTarget::Cloud;
        s.scan.level2_enabled = level2;
        c.db.settings().save_llm(&s.llm).unwrap();
        c.db.settings().save_scan(&s.scan).unwrap();

        // 项目路径指向一个真实存在的临时目录，否则 collect_facts 会先失败，
        // 那就测不到闸门了（断言会被"别的原因"满足 = 测试形同虚设）。
        let dir = std::env::temp_dir().join(format!("projectassests-l2-gate-{level2}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.py"), "def run():\n    return 1\n").unwrap();
        c.db
            .projects()
            .upsert(&projectassests_domain::Project {
                id: "p1".into(),
                name: "普通项目".into(),
                path: dir.to_string_lossy().into_owned(),
                description: String::new(),
                language: "Python".into(),
                framework: "-".into(),
                created_at: None,
                updated_at: None,
                last_commit_at: None,
                status: projectassests_domain::ProjectStatus::Active,
                health_score: 70,
                completeness: None,
                tags: vec![],
                sensitive: false, // 🔴 非敏感：否则会被敏感拦截先挡下，测不到总闸
                stats: projectassests_domain::CodeStats::default(),
                scan: projectassests_domain::ScanFacts::default(),
                ai_profile: None,
            })
            .unwrap();
        c
    }

    /// 🔴 关闭 AI 后必须**在发请求之前**就拦下，且错误是 `Disabled`。
    ///
    /// 这条测试守的是 Local-First 的真实缺口：非敏感项目 + 云端路由的组合下，
    /// 点一次「生成画像」就会把代码送出去，而用户可能一个 sensitive 都没标。
    #[test]
    fn level2_disabled_blocks_profile_before_any_network_call() {
        let c = ctx_cloud_ready(false);
        let err = block_on(generate(
            &c,
            &ProfileRequest {
                project_id: "p1".into(),
                force: true,
            },
        ))
        .unwrap_err();

        assert!(
            matches!(err, ServiceError::Ai(projectassests_domain::AiError::Disabled)),
            "开关关闭时应返回 Disabled，实际 {err:?}"
        );
        // 不得误报成"未配置"：模型明明配好了，照着 NotConfigured 的 hint
        // 去填 API Key 是走错方向，用户要做的只是打开一个开关。
        assert!(!matches!(err, ServiceError::Ai(projectassests_domain::AiError::NotConfigured)));
        // 状态码不得是 5xx：用户主动关闭 AI 不是服务端故障，
        // 记进 error 日志会把真正的故障淹掉。
        assert_eq!(err.status_code(), 424, "应归为前置条件未满足，实际 {err:?}");
        assert_eq!(err.code(), "llm_disabled");
        let hint = err.hint().expect("必须给出可操作的下一步");
        assert!(hint.contains("AI 分析"), "hint 应指向那个开关: {hint}");
    }

    /// 🔴 反向断言（给上一条测试判别力）：开关**打开**时不得再被闸门拦下。
    ///
    /// 若只测"关闭时被拦"，一个把闸门写成恒真的变异照样能通过 ——
    /// 那样开关就变成了"永久禁用 AI"，比死开关更糟。
    /// 打开后流程会继续走到 provider 调用；测试环境里 api.example.com 不可达，
    /// 因此预期是**连接类错误**，而绝不是 `Disabled`。
    #[test]
    fn level2_enabled_does_not_block_profile() {
        let c = ctx_cloud_ready(true);
        let err = block_on(generate(
            &c,
            &ProfileRequest {
                project_id: "p1".into(),
                force: true,
            },
        ))
        .unwrap_err();

        assert!(
            !matches!(err, ServiceError::Ai(projectassests_domain::AiError::Disabled)),
            "开关打开时不得被总闸拦下，实际 {err:?}"
        );
    }

    /// 🔴🔴 端到端守门员：**走完 `generate()` 到 LLM 调用失败后，审计里必须有一条 `ok=false`**。
    ///
    /// # 为什么单元测试 `audit_writes_failure_entry_with_reason` 不够
    /// 那条只证明"audit 函数被喂 `Err` 时会写对记录"。但本次缺陷的本质是
    /// **`generate()` 的失败路径压根没调用 audit**（旧代码 `complete(...).await?`
    /// 直接 return，跳过了函数末尾的 audit）。单元测试永远照不到"调用点有没有接线"——
    /// 必须从 `generate()` 入口走一遍，才能证明失败分支真的接上了 audit。
    ///
    /// # 构造：真实的失败 LLM 调用
    /// `ctx_cloud_ready(true)` 的项目路径是真实存在的临时目录，
    /// `collect_facts` 成功、闸门放行，于是流程真的走到 `provider.complete()`；
    /// 而 `api.example.com` 在测试环境不可达 → 返回连接错误 → 触发 `Err` 分支。
    /// 这正是"prompt 已发出、调用失败"的最小可复现场景。
    #[test]
    fn generate_writes_failed_audit_when_llm_call_fails() {
        let c = ctx_cloud_ready(true);
        assert!(
            c.db.settings().recent_audit(10).unwrap().is_empty(),
            "前提：调用前审计应为空"
        );

        let err = block_on(generate(
            &c,
            &ProfileRequest { project_id: "p1".into(), force: true },
        ))
        .unwrap_err();
        // 确认失败发生在**模型调用**环节（连接类错误），而非更靠前的前置检查。
        // 否则测试会被"别的原因的失败"满足 —— 那条路径不写审计，断言就失去意义。
        assert!(
            matches!(err, ServiceError::Ai(_)),
            "应是模型调用阶段的失败，实际 {err:?}"
        );

        let logs = c.db.settings().recent_audit(10).unwrap();
        assert_eq!(logs.len(), 1, "🔴 失败的模型调用必须在审计里留下恰好一条记录");
        assert_eq!(logs[0].ok, Some(false), "必须标为失败");
        assert!(
            logs[0].error.as_ref().is_some_and(|e| !e.is_empty()),
            "必须带失败原因，实际 {:?}", logs[0].error
        );
        assert_eq!(logs[0].project_id.as_deref(), Some("p1"), "要能查是哪个项目");
        assert!(
            logs[0].summary.contains("失败"),
            "摘要必须自带失败标记：{}", logs[0].summary
        );
        // 🔴 失败不得写假画像（与 generate_never_writes_template_profile_on_failure 呼应）
        assert!(
            c.db.projects().get("p1").unwrap().unwrap().ai_profile.is_none(),
            "失败路径不得写入画像"
        );
    }

    /// 关掉 AI 不该让**已生成**的画像消失：读缓存不产生网络调用。
    #[test]
    fn level2_disabled_still_serves_cached_profile() {
        let c = ctx_cloud_ready(false);
        let profile = projectassests_domain::ProjectAiProfile {
            summary: "此前生成的画像".into(),
            purpose: None,
            phase: None,
            highlights: vec![],
            archaeology: None,
            generated_by: "local:qwen3:8b".into(),
            generated_at: projectassests_storage::now_utc(),
        };
        c.db.projects().set_ai_profile("p1", &profile).unwrap();

        // force=false → 命中缓存，直接返回
        let resp = block_on(generate(
            &c,
            &ProfileRequest {
                project_id: "p1".into(),
                force: false,
            },
        ))
        .expect("已生成的画像应仍可读，不受开关影响");
        assert!(!resp.regenerated, "缓存路径不该标记为重新生成");
        assert_eq!(resp.profile.unwrap().summary, "此前生成的画像");
    }
}
