//! 首页概览服务。
//!
//! # 「界面简单，内心强大」如何落地
//! 首页只有四张统计卡 + 一个迷你图谱 + 两条列表，但每张卡背后都是
//! **跨引擎的真实聚合**：项目数来自扫描、可复用资产数来自评分门槛、
//! 能力数来自三层抽取、洞察数来自五个检测器。
//!
//! 关键是：**UI 不做任何计算**。所有数值、文案、空状态引导都在这里算好，
//! 前端只负责渲染。这样两种传输（HTTP / IPC）拿到的东西完全一致，
//! 也不会出现"网页和桌面端首页数字对不上"的问题。
//!
//! # 空状态是一等公民
//! 新用户打开软件时库里什么都没有。此时首页不能显示"0 个项目"了事——
//! 那等于告诉用户"这是个空软件"。[`Overview::onboarding`] 会给出
//! 明确的下一步（添加目录 → 扫描 → 配置模型），把空状态变成引导流程。

use serde::Serialize;

use crate::context::{ServiceContext, ServiceError};

/// 首页概览（一次请求拿全，避免前端发七八个请求）。
#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    /// 四张统计卡
    pub stats: Vec<StatCard>,
    /// 迷你能力图谱（首页只展示 Top N 节点，完整图谱走 graph 服务）
    pub graph_preview: GraphPreview,
    /// 最新洞察（首页只给前几条）
    pub recent_insights: Vec<InsightBrief>,
    /// 可行动的机会
    pub top_opportunities: Vec<OpportunityBrief>,
    /// 最近活动流
    pub activities: Vec<ActivityBrief>,
    /// 当前/最近任务（侧栏进度卡的同源数据）
    pub job: Option<JobBrief>,
    /// 新手引导：库为空或未完成关键步骤时给出
    pub onboarding: Option<Onboarding>,
    /// 数据新鲜度：上次扫描时间（用户判断"数据是不是旧的"）
    pub last_scanned_at: Option<String>,
    /// 未读洞察数（侧栏红点）
    pub unread_insights: usize,
}

/// 统计卡。
#[derive(Debug, Clone, Serialize)]
pub struct StatCard {
    /// 稳定 key（前端据此选图标与配色，不用 label 匹配）
    pub key: &'static str,
    pub label: &'static str,
    /// 真实数值
    pub value: usize,
    /// 补充说明（如"其中 12 个高分"），无则为 `None`
    pub detail: Option<String>,
    /// 点击后跳转的页面
    pub link_page: &'static str,
    /// 数值为 0 时是否算"异常"（决定是否显示引导色）
    pub empty_is_expected: bool,
}

/// 迷你图谱预览。
#[derive(Debug, Clone, Default, Serialize)]
pub struct GraphPreview {
    pub node_count: usize,
    pub edge_count: usize,
    /// 关联项目最多的能力（首页"你的核心能力"）
    pub top_capabilities: Vec<TopItem>,
}

/// 通用"名称 + 计数"条目。
#[derive(Debug, Clone, Serialize)]
pub struct TopItem {
    pub name: String,
    pub count: usize,
}

/// 洞察摘要（首页卡片，不含完整证据链）。
#[derive(Debug, Clone, Serialize)]
pub struct InsightBrief {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub insight_type: String,
    pub type_label: String,
    pub confidence: f64,
    pub badge: String,
}

/// 机会摘要。
#[derive(Debug, Clone, Serialize)]
pub struct OpportunityBrief {
    pub id: String,
    pub title: String,
    pub description: String,
    /// 组合价值 1-5 星
    pub rating: u8,
    /// 能力覆盖度 0.0-1.0（已具备 / (已具备+缺失)）
    pub coverage: f64,
    /// 可直接复用的来源资产数（机会最有说服力的部分：不是空想，是有代码支撑）
    pub reusable_count: usize,
    /// 缺失的能力数（用户还需要新建多少）
    pub missing_count: usize,
}

/// 活动条目。
#[derive(Debug, Clone, Serialize)]
pub struct ActivityBrief {
    pub id: String,
    pub icon: String,
    pub title: String,
    pub detail: String,
    /// 相对时间文案（"2 小时前"），服务端算好避免前端各处格式不一
    pub when: String,
}

/// 任务状态摘要。
#[derive(Debug, Clone, Serialize)]
pub struct JobBrief {
    pub id: String,
    pub job_type: String,
    pub type_label: String,
    pub status: String,
    pub status_label: String,
    pub percent: u8,
    pub stage: Option<String>,
    pub counter: Option<String>,
    pub error: Option<String>,
    /// 是否可取消（终态任务不该显示取消按钮）
    pub cancellable: bool,
}

/// 新手引导。
///
/// 字段用 `&'static str` 而非 `String`：引导文案全部是编译期常量，
/// 零分配，且类型本身就表达了"这些是固定文案，不是运行时拼出来的"。
#[derive(Debug, Clone, Serialize)]
pub struct Onboarding {
    /// 一句话说明当前卡在哪
    pub headline: &'static str,
    /// 有序步骤；`done` 为已完成的
    pub steps: Vec<OnboardingStep>,
}

/// 引导步骤。
#[derive(Debug, Clone, Serialize)]
pub struct OnboardingStep {
    pub title: &'static str,
    pub detail: &'static str,
    pub done: bool,
    /// 跳转页面 key（前端渲染成按钮）
    pub action_page: Option<&'static str>,
    /// 🔴 直达动作 key：比"跳去某个页面让用户自己找"更短的路径。
    /// - `pick_dirs`：直接在当前页打开目录选择弹窗（免去"跳设置页→找输入框→粘贴"）
    /// - `start_scan`：直接触发扫描
    ///
    /// 为 `None` 时前端回退到 `action_page` 跳转。
    pub action_key: Option<&'static str>,
}

/// 首页展示的洞察条数。
pub const RECENT_INSIGHTS_LIMIT: usize = 4;
/// 首页展示的机会条数。
pub const TOP_OPPORTUNITIES_LIMIT: usize = 3;
/// 首页活动流条数。
pub const ACTIVITIES_LIMIT: usize = 6;
/// 迷你图谱的能力节点上限。
pub const GRAPH_PREVIEW_CAPS: usize = 6;

/// 加载首页概览。
pub fn load(ctx: &ServiceContext) -> Result<Overview, ServiceError> {
    let db = &ctx.db;

    // ── 统计卡 ──────────────────────────────────────────────────
    // 🔴 用 count()/count_scanned()，不是 totals()——
    // 后者返回的是 (代码总行数, 文件总数)，误用会让首页显示"项目总数 845 万"。
    let project_total = db.projects().count()?;
    let project_scanned = db.projects().count_scanned()?;
    let asset_total = db.assets().count_all()?;
    let asset_reusable = db.assets().count_reusable()?;
    // 🔴 用 count_capabilities 而非 count：后者含 Domain（固定 5 个）与
    // Implementation（具体技术名，数量很多），会让"能力数量"虚高失真。
    // 用户想看到的是"我掌握了哪些能力"，那正是 Capability 层的口径。
    let capability_total = db.capabilities().count_capabilities()?;
    let insight_total = db.insights().count()?;
    let unread = db.insights().count_unread()?;

    let stats = vec![
        StatCard {
            key: "projects",
            label: "项目总数",
            value: project_total,
            // 🔴 只有真的扫描过才给"已扫描"说明，否则显示 None 而不是 "0 个已扫描"
            detail: (project_scanned > 0).then(|| format!("其中 {project_scanned} 个已完成索引")),
            link_page: "projects",
            empty_is_expected: false, // 没有项目 = 还没扫描，属需要引导的异常
        },
        StatCard {
            key: "assets",
            label: "可复用资产",
            value: asset_total,
            detail: (asset_reusable > 0).then(|| format!("其中 {asset_reusable} 个高复用分")),
            link_page: "assets",
            empty_is_expected: false,
        },
        StatCard {
            key: "capabilities",
            label: "能力节点",
            value: capability_total,
            detail: None,
            link_page: "graph",
            empty_is_expected: false,
        },
        StatCard {
            key: "insights",
            label: "洞察",
            value: insight_total,
            detail: (unread > 0).then(|| format!("{unread} 条待查看")),
            link_page: "insights",
            // 洞察为空是正常的：数据量不够时检测器本就该沉默（不凑数）
            empty_is_expected: true,
        },
    ];

    // ── 迷你图谱 ────────────────────────────────────────────────
    // 🔴 node_count 用**全部节点**（含 Domain 与 Implementation），
    // 因为图谱确实渲染三层；而统计卡用 Capability 层口径。
    // 两者刻意不同：卡片回答"我掌握多少能力"，图谱回答"图里有多少节点"。
    // 混用会让用户发现"卡片说 12，图里有 47"而怀疑数据错了。
    let graph_preview = GraphPreview {
        node_count: db.capabilities().count()?,
        edge_count: db.relations().count()?,
        top_capabilities: db
            .capabilities()
            .top_by_project_count(GRAPH_PREVIEW_CAPS as u32)?
            .into_iter()
            // u32 → usize：在所有支持平台（32/64 位）上都是无损拓宽。
            // 不用 `usize::from`，因为该 impl 是条件编译的（排除 16 位目标），
            // 反而不如 `as` 直观。
            .map(|(name, count)| TopItem {
                name,
                count: count as usize,
            })
            .collect(),
    };

    // ── 洞察 / 机会 / 活动 ──────────────────────────────────────
    let recent_insights = db
        .insights()
        .list(&spolia_storage::InsightFilter {
            limit: Some(RECENT_INSIGHTS_LIMIT as u32),
            ..Default::default()
        })?
        .into_iter()
        .map(|i| {
            // 🔴 先算 badge 再移动字段：`badge()` 需要 `&self`，
            // 而下面几行会把 id/title/description 逐个 move 出去，
            // 顺序反了就是"部分移动后再借用"的编译错误。
            let badge = i.badge().label_zh().to_string();
            let type_label = i.insight_type.label_zh().to_string();
            let insight_type = i.insight_type.as_str().to_string();
            InsightBrief {
                id: i.id,
                title: i.title,
                // Insight 的字段名是 description（不是 summary）
                summary: i.description,
                type_label,
                insight_type,
                badge,
                confidence: i.confidence,
            }
        })
        .collect();

    let top_opportunities = db
        .opportunities()
        .list(&spolia_storage::OpportunityFilter::actionable())?
        .into_iter()
        .take(TOP_OPPORTUNITIES_LIMIT)
        .map(|o| OpportunityBrief {
            id: o.id,
            title: o.title,
            description: o.description,
            rating: o.rating,
            coverage: o.coverage,
            // 🔴 复用数来自 source_asset_ids（真实抽取出的资产），
            // 缺失数来自 missing_capabilities——两者一起才能说明
            // "这个机会不是空想，有多少现成代码可用、还差多少"。
            reusable_count: o.source_asset_ids.len(),
            missing_count: o.missing_capabilities.len(),
        })
        .collect();

    // 相对时间由存储层在读取时算好（Activity.relative），此处直接透传。
    // 🔴 不在 service 层重算：两处计算必然漂移（例如一处按分钟一处按小时），
    // 且"谁负责时间格式化"会变得不明确。
    let activities = db
        .activities()
        .recent(ACTIVITIES_LIMIT as u32)?
        .into_iter()
        .map(|a| ActivityBrief {
            id: a.id,
            icon: a.icon.as_str().to_string(),
            title: a.title,
            detail: a.detail,
            when: a.relative,
        })
        .collect();

    // ── 任务 ────────────────────────────────────────────────────
    // 优先显示进行中的；没有就显示最近一条（让用户知道上次扫描的结果）。
    // 用 match 而非 `.or_else(..).transpose()?` 链：后者类型能对上但极难读，
    // 且 transpose 语义（Result<Option> ↔ Option<Result>）在这里毫无必要。
    let job = match running_job(ctx)? {
        Some(brief) => Some(brief),
        None => recent_job(ctx)?,
    };

    // ── 数据新鲜度 ──────────────────────────────────────────────
    let last_scanned_at = db.projects().latest_scan_time()?;

    Ok(Overview {
        stats,
        graph_preview,
        recent_insights,
        top_opportunities,
        activities,
        job,
        onboarding: build_onboarding(project_total, asset_total, last_scanned_at.as_deref(), ctx),
        last_scanned_at,
        unread_insights: unread,
    })
}

/// 进行中的任务（queued / running）。
fn running_job(ctx: &ServiceContext) -> Result<Option<JobBrief>, ServiceError> {
    Ok(ctx
        .db
        .jobs()
        .running()?
        .into_iter()
        .next()
        .map(job_brief))
}

/// 最近一条任务（终态也要显示，用户需要知道上次扫描成功还是失败）。
fn recent_job(ctx: &ServiceContext) -> Result<Option<JobBrief>, ServiceError> {
    Ok(ctx
        .db
        .jobs()
        .recent(1)?
        .into_iter()
        .next()
        .map(job_brief))
}

fn job_brief(j: spolia_domain::Job) -> JobBrief {
    JobBrief {
        id: j.id.clone(),
        type_label: j.job_type.label_zh().to_string(),
        job_type: j.job_type.as_str().to_string(),
        status_label: j.status.label_zh().to_string(),
        status: j.status.as_str().to_string(),
        percent: j.percent(),
        stage: j.stage.clone(),
        counter: j.counter_text(),
        error: j.error.clone(),
        // 🔴 终态任务不该显示取消按钮：点了必然失败，是明显的交互缺陷
        cancellable: !j.status.is_terminal(),
    }
}

/// 构建新手引导。
///
/// 已完成全部步骤时返回 `None`（首页不该永久挂着一个引导条）。
fn build_onboarding(
    project_count: usize,
    asset_count: usize,
    last_scanned_at: Option<&str>,
    ctx: &ServiceContext,
) -> Option<Onboarding> {
    // 读设置判断是否已添加目录（不报错：读失败时按"未添加"处理，
    // 引导多显示一次比漏显示好）
    let has_dirs = ctx
        .db
        .settings()
        .get_or_default()
        .map(|s| !s.scan.enabled_dirs().is_empty())
        .unwrap_or(false);
    // 🔴 不能用 `llm.local_configured()` 判定：它只校验 URL 格式合法，
    // 而默认设置里就带着 Ollama 的标准地址（http://127.0.0.1:11434），
    // 于是对**全新用户恒为 true**——引导第 4 步会谎报"已完成"，
    // 用户便不会去配置，点"分析"时才发现根本没装 Ollama。
    //
    // 诚实的信号是"模型真的可用过"：
    // - 云端：用户填过 API Key（明确的主动行为）
    // - 本地：审计日志里有**成功的**模型调用记录
    //
    // 🔴 必须是"成功的"，不能是"有任何记录"：
    // schema v3 之后失败的调用也写审计（prompt 已出网，必须留痕）。
    // 若仍按"非空"判定，一次 400 未开通的调用就会把这一步标成已完成——
    // 而它恰恰证明模型**不可用**，正是本段注释开头警告的那种谎报。
    // 判定逻辑在 `has_successful_llm_call`（SQL `WHERE ok = 1`）。
    //
    // 代价：刚装好 Ollama 但还没用过的用户会看到该步未完成。
    // 这是可接受的——该步本就标注"（可选）"，且不阻塞引导完成判定；
    // 相比"谎报已完成导致用户不去配置"，宁可保守。
    let llm_ready = ctx
        .db
        .settings()
        .get_or_default()
        .map(|s| s.llm.cloud_configured())
        .unwrap_or(false)
        || ctx
            .db
            .settings()
            .has_successful_llm_call()
            .unwrap_or(false);

    let steps = vec![
        OnboardingStep {
            title: "添加扫描目录",
            detail: "把你的代码根目录加进来，例如 `F:/CodeProject`。只读不写，凭证文件自动跳过。",
            done: has_dirs,
            action_page: Some("settings"),
            // 🔴 直达：就地打开目录选择弹窗，不再"跳设置页→找输入框→粘贴路径"
            action_key: Some("pick_dirs"),
        },
        OnboardingStep {
            title: "扫描项目",
            detail: "识别项目、统计代码规模、读取 Git 历史。百个项目量级通常在几十秒内完成。",
            done: project_count > 0 && last_scanned_at.is_some(),
            action_page: Some("settings"),
            // 没目录时点了也只会报错：此时引导用户先选目录
            action_key: Some(if has_dirs { "start_scan" } else { "pick_dirs" }),
        },
        OnboardingStep {
            title: "索引代码资产",
            detail: "抽取函数、组件、API 端点，计算复用评分，构建跨项目能力图谱。",
            done: asset_count > 0,
            action_page: Some("assets"),
            // 扫描完成但资产为空 = 索引没跑（或被清理过）：直接补跑索引
            action_key: Some("start_index"),
        },
        OnboardingStep {
            title: "配置大模型（可选）",
            detail: "接入 Ollama 或云端 API 后，可获得项目画像、对话式分析与跨项目推理。未配置时仍可检索真实数据。",
            done: llm_ready,
            action_page: Some("settings"),
            // 配置需要填表单，没有更短的直达路径
            action_key: None,
        },
    ];

    // 前三步是必要的；第四步（模型）可选，不阻塞"完成"判定
    let core_done = steps[0].done && steps[1].done && steps[2].done;
    if core_done {
        return None;
    }

    let headline = if !has_dirs {
        "从一个目录开始：Spolia 会把你写过的东西变成可检索、可复用的资产"
    } else if project_count == 0 {
        "目录已添加，还没扫描过"
    } else {
        "项目已扫描，正在等待代码索引"
    };

    Some(Onboarding { headline, steps })
}

/// 统计各类型洞察数（首页不用，但设置页与诊断需要）。
pub fn insight_type_breakdown(ctx: &ServiceContext) -> Result<Vec<TopItem>, ServiceError> {
    Ok(ctx
        .db
        .insights()
        .count_by_type()?
        .into_iter()
        .map(|(t, n)| TopItem {
            name: t.label_zh().to_string(),
            count: n,
        })
        .collect())
}

/// 各状态机会数。
pub fn opportunity_status_breakdown(ctx: &ServiceContext) -> Result<Vec<TopItem>, ServiceError> {
    Ok(ctx
        .db
        .opportunities()
        .count_by_status()?
        .into_iter()
        .map(|(s, n)| TopItem {
            name: s.label_zh().to_string(),
            count: n,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::{
        Asset, AssetType, Capability, CapabilityLayer, CodeStats, Evidence, Insight, InsightType,
        Job, JobStatus, JobType, OpportunityStatus, Project, ProjectStatus, ScanFacts,
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
            language: "Python".into(),
            framework: "-".into(),
            created_at: None,
            updated_at: Some("2026-09-20".into()),
            last_commit_at: Some("2026-09-20".into()),
            status: ProjectStatus::Active,
            health_score: 80,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats::default(),
            scan: ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn asset(id: &str, reuse: f64) -> Asset {
        Asset {
            id: id.into(),
            project_id: "p1".into(),
            asset_type: AssetType::Component,
            name: format!("Asset{id}"),
            description: "d".into(),
            content: None,
            source_path: format!("src/{id}.py"),
            confidence: 0.9,
            reuse_score: reuse,
            generality: 0.7,
            stability: 0.6,
            tags: vec![],
            created_at: "2026-09-01".into(),
            evidence: Evidence {
                files: vec![format!("src/{id}.py")],
                ..Evidence::default()
            },
            user_feedback: None,
        }
    }

    // ── 空库：首页必须是引导而非一片 0 ───────────────────────────

    /// 🔴 核心原则「界面简单内心强大」的反面是"空软件感"。
    /// 新用户打开首页必须看到明确的下一步。
    #[test]
    fn empty_database_shows_onboarding_not_zeros() {
        let o = load(&ctx()).unwrap();
        let onboarding = o.onboarding.expect("空库必须给出引导");
        assert!(!onboarding.headline.is_empty());
        assert_eq!(onboarding.steps.len(), 4);
        assert!(!onboarding.steps[0].done, "还没添加目录");
        assert!(onboarding.steps.iter().all(|s| !s.done));
        // 第一步就该指向设置页
        assert_eq!(onboarding.steps[0].action_page, Some("settings"));
        // 🔴 且必须带直达动作：空库时第一步直接开目录选择弹窗，
        // 第二步（还没目录）同样引导选目录而非"开始扫描"（点了只会报错）
        assert_eq!(onboarding.steps[0].action_key, Some("pick_dirs"));
        assert_eq!(onboarding.steps[1].action_key, Some("pick_dirs"));
        assert_eq!(onboarding.steps[2].action_key, Some("start_index"));
    }

    #[test]
    fn empty_database_stats_are_zero_but_well_formed() {
        let o = load(&ctx()).unwrap();
        assert_eq!(o.stats.len(), 4);
        for s in &o.stats {
            assert_eq!(s.value, 0);
            assert!(!s.label.is_empty());
            assert!(!s.link_page.is_empty(), "每张卡都要可点击跳转");
            assert!(s.detail.is_none(), "数值为 0 时不该编造补充说明");
        }
        assert!(o.recent_insights.is_empty());
        assert!(o.top_opportunities.is_empty());
        assert!(o.activities.is_empty());
        assert!(o.job.is_none());
        assert!(o.last_scanned_at.is_none());
        assert_eq!(o.unread_insights, 0);
    }

    /// 四张卡的 key 必须稳定：前端靠它选图标配色，改了会静默错位。
    #[test]
    fn stat_card_keys_are_stable() {
        let o = load(&ctx()).unwrap();
        let keys: Vec<&str> = o.stats.iter().map(|s| s.key).collect();
        assert_eq!(keys, vec!["projects", "assets", "capabilities", "insights"]);
    }

    // ── 有数据：统计必须是真实聚合 ───────────────────────────────

    #[test]
    fn stats_reflect_real_database_counts() {
        let c = ctx();
        c.db
            .projects()
            .upsert_batch(&[project("p1", "a"), project("p2", "b")])
            .unwrap();
        c.db
            .assets()
            .upsert_batch(&[asset("a1", 0.9), asset("a2", 0.3)])
            .unwrap();

        let o = load(&c).unwrap();
        assert_eq!(o.stats[0].value, 2, "项目数");
        assert_eq!(o.stats[1].value, 2, "资产数");
        // 高复用分只有 a1（0.9）
        assert_eq!(
            o.stats[1].detail.as_deref(),
            Some("其中 1 个高复用分"),
            "补充说明必须基于真实门槛计数"
        );
    }

    /// 洞察为空是**正常**的（数据不足时检测器本该沉默），不该标记为异常。
    #[test]
    fn insight_card_treats_zero_as_expected() {
        let o = load(&ctx()).unwrap();
        let insight_card = o.stats.iter().find(|s| s.key == "insights").unwrap();
        assert!(
            insight_card.empty_is_expected,
            "洞察为 0 不该被当成需要引导的异常"
        );
        // 项目为 0 则是异常（说明还没扫描）
        let project_card = o.stats.iter().find(|s| s.key == "projects").unwrap();
        assert!(!project_card.empty_is_expected);
    }

    // ── 引导流程的推进 ───────────────────────────────────────────

    /// 添加目录后，第一步应标记完成，引导仍显示（因为还没扫描）。
    #[test]
    fn onboarding_advances_after_adding_dirs() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.scan.add_dir("F:/CodeProject", "2026-09-29T00:00:00Z");
        c.db.settings().save_scan(&s.scan).unwrap();

        let o = load(&c).unwrap();
        let ob = o.onboarding.expect("还没扫描，引导应继续显示");
        assert!(ob.steps[0].done, "已添加目录");
        assert!(!ob.steps[1].done, "还没扫描");
        assert!(ob.headline.contains("还没扫描"), "实际: {}", ob.headline);
    }

    /// 前三步完成后引导消失（第四步配模型是可选的，不该永久挂着引导条）。
    #[test]
    fn onboarding_disappears_after_core_steps_done() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.scan.add_dir("F:/CodeProject", "2026-09-29T00:00:00Z");
        c.db.settings().save_scan(&s.scan).unwrap();

        // 🔴 扫描事实必须经 update_scan_facts 写入：
        // `upsert` 刻意不碰 scan 列（避免"更新描述"把 Git 统计清零），
        // 直接在 Project 上设 `p.scan = ...` 再 upsert 是**不会生效**的。
        c.db.projects().upsert(&project("p1", "a")).unwrap();
        c.db
            .projects()
            .update_scan_facts(
                "p1",
                &ScanFacts {
                    has_git: true,
                    scanned_at: Some("2026-09-29T10:00:00Z".into()),
                    ..ScanFacts::default()
                },
            )
            .unwrap();
        c.db.assets().upsert(&asset("a1", 0.9)).unwrap();

        let o = load(&c).unwrap();
        assert!(
            o.onboarding.is_none(),
            "核心步骤完成后不该再显示引导: {:?}",
            o.onboarding.map(|x| x.headline)
        );
        // 数据新鲜度应可读
        assert!(o.last_scanned_at.is_some());
    }

    /// 🔴 第四步"配置大模型"的完成判定必须诚实。
    ///
    /// 默认设置自带 Ollama 标准地址，所以 `local_configured()` 对新用户恒为 true。
    /// 若用它判定，全新安装的用户一进来就看到"已配置"，于是不会去装模型，
    /// 直到点"分析"才发现根本连不上——这比直接说"未配置"糟糕得多。
    #[test]
    fn llm_step_is_not_done_for_fresh_install() {
        let c = ctx();
        let ob = load(&c).unwrap().onboarding.unwrap();
        // 默认设置里 local_base_url 已经是 http://127.0.0.1:11434
        let s = c.db.settings().get_or_default().unwrap();
        assert!(
            s.llm.local_configured(),
            "默认 URL 格式本就合法（这正是不能用它判定的原因）"
        );
        assert!(
            !ob.steps[3].done,
            "全新安装不得把'配置大模型'标成已完成"
        );
    }

    /// 填了云端 API Key（明确的主动行为）即算完成。
    #[test]
    fn llm_step_done_after_cloud_key_configured() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.llm.cloud_api_key = "sk-real-key".into();
        s.llm.cloud_base_url = "https://api.example.com/v1".into();
        c.db.settings().save_llm(&s.llm).unwrap();
        let ob = load(&c).unwrap().onboarding.unwrap();
        assert!(ob.steps[3].done, "配置了云端 Key 应算完成");
    }

    /// 本地模型跑通过（审计日志有记录）也算完成——这是"真的能用"的证据。
    #[test]
    fn llm_step_done_after_successful_local_call() {
        let c = ctx();
        c.db
            .settings()
            .audit(&spolia_domain::AuditEntry {
                at: "2026-09-29T10:00:00Z".into(),
                model: "ollama:qwen3:8b".into(),
                route: spolia_domain::RouteTarget::Local,
                job_type: "ANALYZE_PROJECT".into(),
                summary: "分析项目 p1".into(),
                project_id: Some("p1".into()),
            })
            .unwrap();
        let ob = load(&c).unwrap().onboarding.unwrap();
        assert!(ob.steps[3].done, "有模型调用记录说明已可用");
    }

    /// 第四步是可选的：即使没配模型，前三步完成就该让引导消失。
    #[test]
    fn llm_step_does_not_block_onboarding_completion() {
        let c = ctx();
        let mut s = c.db.settings().get_or_default().unwrap();
        s.scan.add_dir("F:/CodeProject", "2026-09-29T00:00:00Z");
        c.db.settings().save_scan(&s.scan).unwrap();
        c.db.projects().upsert(&project("p1", "a")).unwrap();
        c.db
            .projects()
            .update_scan_facts(
                "p1",
                &ScanFacts {
                    scanned_at: Some("2026-09-29T10:00:00Z".into()),
                    ..ScanFacts::default()
                },
            )
            .unwrap();
        c.db.assets().upsert(&asset("a1", 0.9)).unwrap();

        // 未配置任何模型，但核心三步已完成
        let s2 = c.db.settings().get_or_default().unwrap();
        assert!(!s2.llm.cloud_configured());
        assert!(
            load(&c).unwrap().onboarding.is_none(),
            "配模型是可选步骤，不该阻塞引导完成"
        );
    }

    // ── 任务卡 ───────────────────────────────────────────────────

    /// 进行中的任务优先展示。
    #[test]
    fn running_job_takes_priority() {
        let c = ctx();
        c.db
            .jobs()
            .create("scan_project-a", JobType::ScanProject, None)
            .unwrap();
        let o = load(&c).unwrap();
        let job = o.job.expect("应展示任务");
        assert_eq!(job.id, "scan_project-a");
        assert_eq!(job.status, JobStatus::Queued.as_str());
        assert!(job.cancellable, "非终态任务应可取消");
        assert_eq!(job.type_label, "扫描项目");
    }

    /// 🔴 终态任务不得显示取消按钮——点了必然失败，是明显的交互缺陷。
    #[test]
    fn finished_job_is_not_cancellable() {
        let c = ctx();
        c.db
            .jobs()
            .create("scan_project-done", JobType::ScanProject, None)
            .unwrap();
        c.db
            .jobs()
            .set_terminal("scan_project-done", JobStatus::Completed, None)
            .unwrap();
        let job = load(&c).unwrap().job.expect("应展示最近任务");
        assert_eq!(job.status, JobStatus::Completed.as_str());
        assert!(!job.cancellable);
        assert_eq!(job.percent, 100);
    }

    /// 失败任务必须把错误文案带出来，否则用户只看到"失败"两个字。
    #[test]
    fn failed_job_exposes_error_message() {
        let c = ctx();
        c.db
            .jobs()
            .create("analyze_project-x", JobType::AnalyzeProject, None)
            .unwrap();
        c.db
            .jobs()
            .set_terminal("analyze_project-x", JobStatus::Failed, Some("模型未配置"))
            .unwrap();
        let job = load(&c).unwrap().job.unwrap();
        assert_eq!(job.error.as_deref(), Some("模型未配置"));
        assert_eq!(job.status_label, "失败");
    }

    // ── 活动与相对时间 ───────────────────────────────────────────

    #[test]
    fn activities_carry_relative_time() {
        let c = ctx();
        c.db
            .activities()
            .push(spolia_storage::ActivityIcon::Scan, "扫描完成", "发现 12 个项目")
            .unwrap();
        let o = load(&c).unwrap();
        assert_eq!(o.activities.len(), 1);
        let a = &o.activities[0];
        assert_eq!(a.title, "扫描完成");
        assert!(!a.when.is_empty(), "相对时间由服务端算好");
        assert_eq!(a.icon, "scan");
    }

    #[test]
    fn activities_are_capped() {
        let c = ctx();
        for i in 0..20 {
            c.db
                .activities()
                .push(spolia_storage::ActivityIcon::Check, format!("t{i}"), "d")
                .unwrap();
        }
        assert!(load(&c).unwrap().activities.len() <= ACTIVITIES_LIMIT);
        assert_eq!(ACTIVITIES_LIMIT, 6);
    }

    // ── 洞察与机会摘要 ───────────────────────────────────────────

    fn insight(id: &str, title: &str, t: InsightType) -> Insight {
        Insight {
            id: id.into(),
            insight_type: t,
            title: title.into(),
            // 真实字段名是 description
            description: format!("{title} 的说明"),
            confidence: 0.85,
            // 产品红线：evidence 为空的洞察不允许入库
            evidence: vec![spolia_domain::EvidenceItem {
                kind: spolia_domain::EvidenceKind::Project,
                label: "项目A".into(),
                target: Some("p1".into()),
            }],
            tags: vec!["python".into()],
            related_project_ids: vec!["p1".into()],
            related_asset_ids: vec![],
            created_at: "2026-09-29".into(),
            user_feedback: None,
        }
    }

    #[test]
    fn recent_insights_are_brief_and_labeled() {
        let c = ctx();
        c.db
            .insights()
            .upsert_batch(&[
                insight("i1", "重复实现", InsightType::DuplicateCapability),
                insight("i2", "高复用组件", InsightType::ReusableComponent),
            ])
            .unwrap();
        let o = load(&c).unwrap();
        assert_eq!(o.recent_insights.len(), 2);
        let i = &o.recent_insights[0];
        assert!(!i.type_label.is_empty(), "类型要有中文标签");
        assert!(!i.badge.is_empty(), "价值分档徽章");
        assert!(i.confidence > 0.0);
        assert_eq!(o.unread_insights, 2, "新洞察应计入未读");
    }

    #[test]
    fn recent_insights_are_capped() {
        let c = ctx();
        let batch: Vec<Insight> = (0..12)
            .map(|i| insight(&format!("i{i}"), &format!("洞察{i}"), InsightType::ReusableComponent))
            .collect();
        c.db.insights().upsert_batch(&batch).unwrap();
        assert_eq!(
            load(&c).unwrap().recent_insights.len(),
            RECENT_INSIGHTS_LIMIT
        );
        assert_eq!(RECENT_INSIGHTS_LIMIT, 4);
    }

    #[test]
    fn opportunities_report_reusable_count() {
        let c = ctx();
        c.db
            .opportunities()
            .upsert(&spolia_domain::Opportunity {
                id: "o1".into(),
                title: "视频工具组合".into(),
                description: "把三个项目的视频能力拼起来".into(),
                source_project_ids: vec!["p1".into(), "p2".into()],
                source_asset_ids: vec!["a1".into(), "a2".into()],
                required_capabilities: vec!["视频生成".into()],
                missing_capabilities: vec!["字幕合成".into()],
                coverage: 0.66,
                rating: 4,
                why: "2 个历史项目存在能力重合".into(),
                evidence: vec!["p1/pipeline.py".into()],
                status: OpportunityStatus::New,
                created_at: "2026-09-29".into(),
            })
            .unwrap();
        let o = load(&c).unwrap();
        assert_eq!(o.top_opportunities.len(), 1);
        let brief = &o.top_opportunities[0];
        // 🔴 可复用资产数是机会最有说服力的部分，必须真实反映
        assert_eq!(brief.reusable_count, 2);
        assert_eq!(brief.missing_count, 1, "缺失能力数也要如实反映");
        assert_eq!(brief.rating, 4);
        assert!((brief.coverage - 0.66).abs() < 1e-9);
        assert_eq!(o.top_opportunities[0].rating, 4);
    }

    // ── 图谱预览 ─────────────────────────────────────────────────

    #[test]
    fn graph_preview_counts_are_real() {
        let c = ctx();
        // 图谱节点数含全部三层，但"核心能力"只取 Capability 层
        let domain = Capability::new("cap_domain_ai", "AI", CapabilityLayer::Domain, None, 0.9).unwrap();
        let mut cap = Capability::new(
            "cap-video-generation",
            "视频生成",
            CapabilityLayer::Capability,
            Some("cap_domain_ai".into()),
            0.9,
        )
        .unwrap();
        cap.project_count = 3;
        c.db.capabilities().upsert_batch(&[domain, cap]).unwrap();
        c.db.projects().upsert(&project("p1", "a")).unwrap();

        let o = load(&c).unwrap();
        // node_count 是图谱真实节点数（两层都在）
        assert_eq!(o.graph_preview.node_count, 2);
        // top_capabilities 只列 Capability 层：Domain（固定 5 个）列出来没有信息量
        assert_eq!(o.graph_preview.top_capabilities.len(), 1);
        assert_eq!(o.graph_preview.top_capabilities[0].name, "视频生成");
        assert_eq!(o.graph_preview.top_capabilities[0].count, 3);
        // 统计卡口径不同：也是 Capability 层
        assert_eq!(o.stats[2].value, 1, "能力卡应为 Capability 层计数");
    }

    /// 只有 Domain 层时，"核心能力"应为空——不能把 AI/Web 这类固定分类当成用户能力。
    #[test]
    fn graph_preview_excludes_domain_only() {
        let c = ctx();
        c.db
            .capabilities()
            .upsert(&Capability::new("cap_domain_ai", "AI", CapabilityLayer::Domain, None, 0.9).unwrap())
            .unwrap();
        let o = load(&c).unwrap();
        assert_eq!(o.graph_preview.node_count, 1);
        assert!(
            o.graph_preview.top_capabilities.is_empty(),
            "Domain 不该出现在核心能力里"
        );
        assert_eq!(o.stats[2].value, 0);
    }

    // ── 健壮性 ───────────────────────────────────────────────────

    /// 首页是打开软件的第一屏，任何情况下都不该返回错误。
    #[test]
    fn load_never_fails_on_empty_or_partial_data() {
        let c = ctx();
        assert!(load(&c).is_ok());
        // 只有一部分数据也不该失败
        c.db.projects().upsert(&project("p1", "a")).unwrap();
        assert!(load(&c).is_ok());
        c.db.assets().upsert(&asset("a1", 0.9)).unwrap();
        assert!(load(&c).is_ok());
    }

    #[test]
    fn breakdown_helpers_work_on_empty_db() {
        let c = ctx();
        assert!(insight_type_breakdown(&c).unwrap().is_empty());
        assert!(opportunity_status_breakdown(&c).unwrap().is_empty());
    }

    #[test]
    fn breakdown_helpers_label_types_in_chinese() {
        let c = ctx();
        c.db
            .insights()
            .upsert(&insight("i1", "x", InsightType::ForgottenAsset))
            .unwrap();
        let b = insight_type_breakdown(&c).unwrap();
        assert_eq!(b.len(), 1);
        assert!(!b[0].name.is_empty());
        assert_eq!(b[0].count, 1);
    }

    #[test]
    fn job_brief_counter_hidden_without_total() {
        let j = Job {
            id: "j".into(),
            job_type: JobType::IndexCode,
            status: JobStatus::Running,
            progress: 0.5,
            stage: Some("索引中".into()),
            processed: None,
            total: None,
            error: None,
            payload: None,
            created_at: "2026-09-29".into(),
            updated_at: "2026-09-29".into(),
        };
        let b = job_brief(j);
        assert!(b.counter.is_none(), "无总数时不该显示 '0 / 0'");
        assert_eq!(b.percent, 50);
        assert_eq!(b.type_label, "索引代码");
    }
}
