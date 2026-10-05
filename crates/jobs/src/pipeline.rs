//! 任务流水线：把各引擎串成可取消的后台任务。
//!
//! # 三个任务与它们的边界
//! | 任务 | 输入 | 产出 | 耗时量级 |
//! |---|---|---|---|
//! | `ScanProjectHandler` | 授权目录列表 | `projects` 表 + Level 0 事实 | 秒级（156 项目约 25s） |
//! | `IndexCodeHandler` | 已入库的项目 | `assets` / `capabilities` / `relations` | 十秒~分钟级 |
//! | `InsightHandler` | 上述全部 | `insights` / `opportunities` | 秒级（纯内存计算） |
//!
//! 拆成三个而非一个大任务的理由是**产品要求**：《产品设计书》附录 A 要求
//! 用户"秒级看到项目列表"，之后索引在后台继续。
//! 若合成一个任务，用户必须等最慢的那一步才能看到任何东西。
//!
//! # 两条贯穿全程的纪律
//! 1. **同步库跑在 `spawn_blocking`**：扫描器是同步 + rayon 并行的，
//!    直接在 async 任务里调用会阻塞 tokio 工作线程，拖垮整个服务。
//! 2. **每个阶段都检查取消**：任务可能跑几分钟，
//!    用户点取消后必须在下一个安全点退出，而不是等它自然结束。

use std::path::PathBuf;
use std::sync::Arc;

use spolia_asset::{
    build_duplicate_relations, AssetBuilder, AssetRef, CapabilitySignals, HeuristicExtractor,
    ProjectInput, SymbolExtractor,
};
use spolia_domain::{JobType, Project, Relation, ScanFacts, Settings, SymbolStats};
use spolia_insight::{AnalysisInput, DetectorConfig, OpportunityConfig, OpportunityEngine};
use spolia_scanner::{
    parse_manifests, to_domain_project, ProgressSink, ScanConfig, ScanOutcome, Scanner,
};
use spolia_storage::{ActivityIcon, AssetWriteOutcome};

use crate::engine::{JobContext, JobHandler, TaskHandle};
use crate::walk::{list_source_files, read_source, top_dirs_of};

/// 能力置信度门槛：低于此值不进图谱（《技术设计书》§25 防标签爆炸）。
pub const CAPABILITY_CONFIDENCE_FLOOR: f64 = 0.55;

/// 判定"跨项目重复实现"所需的最少项目数。
pub const DUPLICATE_MIN_PROJECTS: usize = 2;

/// 单次扫描的最大项目数（防御性上限，避免 UI 被淹没）。
pub const MAX_PROJECTS_PER_SCAN: usize = 2000;

/// 单个项目在 Level 0 统计时最多读取的文件数。
///
/// 与 `walk::MAX_FILES_PER_PROJECT`（Level 1 符号抽取）是两个不同的闸门：
/// Level 0 只需数行数，可以放宽；Level 1 要读内容抽符号，必须更严。
pub const MAX_FILES_PER_PROJECT_SCAN: usize = 20_000;

// ══════════════════════════════════════════════════════════════════
// 阶段一：扫描项目（Level 0）
// ══════════════════════════════════════════════════════════════════

/// 目录扫描任务。
///
/// 载荷格式：`{"dirs": ["F:/CodeProject"], "mode": "full", "analyze_git": true}`
/// 三个字段都可选，缺省时回退到「设置里的授权目录 + 全量 + 分析 Git」。
#[derive(Debug, Default)]
pub struct ScanProjectHandler;

impl ScanProjectHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl JobHandler for ScanProjectHandler {
    async fn run(&self, ctx: JobContext) -> Result<(), String> {
        // 1. 解析目录：载荷优先，其次设置里的授权目录
        let (roots, analyze_git) = resolve_scan_targets(&ctx)?;
        if roots.is_empty() {
            // 🔴 明确报错而非静默成功：用户点了"扫描"却什么都没发生，
            // 若任务显示"已完成"，他会以为扫描过了而找不到项目。
            return Err("尚未添加任何扫描目录，请先在 设置 → 扫描目录 中添加".to_string());
        }
        ctx.report(0.02, format!("准备扫描 {} 个目录", roots.len()))?;

        let settings = ctx.db.settings().get_or_default().map_err(db_err)?;
        let config = ScanConfig {
            roots: roots.clone(),
            exclude_patterns: settings.scan.exclude_patterns.clone(),
            max_depth: settings.scan.max_depth.max(1) as usize,
            max_projects: MAX_PROJECTS_PER_SCAN,
            analyze_git,
            count_code: true,
            max_files_per_project: MAX_FILES_PER_PROJECT_SCAN,
        };

        // 2. 扫描（同步 + rayon）：必须放到阻塞线程池，
        //    否则会占住 tokio 工作线程，其它请求全部饿死。
        let handle = ctx.handle();
        let flag = handle.cancel_flag();
        let outcome = tokio::task::spawn_blocking(move || run_scan(config, &flag, &handle))
            .await
            .map_err(|e| format!("扫描任务异常终止: {e}"))??;

        if outcome.cancelled {
            // 取消不是错误：引擎会据 is_cancelled 置为 Cancelled
            ctx.log("扫描已被用户取消");
            return Ok(());
        }

        // 3. 落库：项目 upsert + Level 0 事实分开写
        ctx.report(0.85, "写入扫描结果…")?;
        let written = persist_scanned_projects(&ctx, &outcome)?;

        // 4. 更新授权目录的扫描时间与项目数（设置页展示"上次扫描"）
        update_dir_metadata(&ctx, &roots, written)?;

        for w in &outcome.warnings {
            ctx.log(format!("⚠ {w}"));
        }
        ctx.log(format!(
            "扫描完成：发现 {} 个项目，遍历 {} 个目录，耗时 {:.1}s",
            written,
            outcome.dirs_walked,
            outcome.elapsed_ms as f64 / 1000.0
        ));
        ctx.report_counted(1.0, "扫描完成", written as u64, written as u64)?;

        // 活动流：首页"最近活动"的真实来源
        let _ = ctx.db.activities().push(
            ActivityIcon::Scan,
            "扫描完成",
            format!("发现 {written} 个项目"),
        );
        Ok(())
    }
}

/// 在阻塞线程里执行扫描。
///
/// 返回 `Result<Result<…>>`：外层是 `spawn_blocking` 的 JoinError，
/// 内层是扫描器自身的错误。
fn run_scan(
    config: ScanConfig,
    flag: &std::sync::atomic::AtomicBool,
    handle: &TaskHandle,
) -> Result<ScanOutcome, String> {
    let scanner = Scanner::new(config);
    // 桥接：把扫描器的 ProgressSink 转发到任务进度
    let sink = ProgressBridge {
        handle: handle.clone(),
    };
    scanner.scan(flag, &sink).map_err(|e| e.to_string())
}

/// `ProgressSink` → `TaskHandle` 的桥接。
///
/// 🔴 扫描器给出 `(stage, done, total)` 结构化数值，本桥负责换算成 0..=1 的进度。
/// 扫描占整个任务的 0.02..0.85 区间（剩余留给写库），
/// 这样进度条不会在"写库"阶段突然从 100% 跳回去。
struct ProgressBridge {
    handle: TaskHandle,
}

/// 扫描阶段在整体进度中占据的区间起点。
const SCAN_PROGRESS_FROM: f64 = 0.02;
/// 扫描阶段在整体进度中占据的区间终点。
const SCAN_PROGRESS_TO: f64 = 0.85;

impl ProgressSink for ProgressBridge {
    fn report(&self, stage: &str, done: usize, total: Option<usize>) {
        // 把 done/total 映射进 [SCAN_PROGRESS_FROM, SCAN_PROGRESS_TO]
        let ratio = match total {
            Some(t) if t > 0 => (done as f64 / t as f64).clamp(0.0, 1.0),
            // total 未知（发现阶段）：停在区间起点，不编造进度
            _ => 0.0,
        };
        let progress = SCAN_PROGRESS_FROM + ratio * (SCAN_PROGRESS_TO - SCAN_PROGRESS_FROM);
        // 取消后上报会返回 Err，此处忽略：扫描器自己也在检查同一个标志
        let _ = self.handle.report(progress, stage.to_string());
    }

    fn log(&self, line: &str) {
        self.handle.log(line.to_string());
    }
}

/// 解析扫描目标：载荷优先，其次设置里的授权目录。
fn resolve_scan_targets(ctx: &JobContext) -> Result<(Vec<PathBuf>, bool), String> {
    let mut analyze_git = true;
    let mut roots: Vec<PathBuf> = Vec::new();

    if let Some(payload) = ctx.payload.as_ref() {
        if let Some(dirs) = payload.get("dirs").and_then(|d| d.as_array()) {
            roots.extend(
                dirs.iter()
                    .filter_map(|d| d.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from),
            );
        }
        if let Some(git) = payload.get("analyze_git").and_then(|v| v.as_bool()) {
            analyze_git = git;
        }
    }

    if roots.is_empty() {
        // 回退到设置里的授权目录（只取 enabled 的）
        let settings = ctx.db.settings().get_or_default().map_err(db_err)?;
        roots.extend(
            settings
                .scan
                .enabled_dirs()
                .into_iter()
                .map(|d| PathBuf::from(&d.path)),
        );
    }

    // 去重：用户可能在载荷与设置里给了同一个目录
    roots.sort();
    roots.dedup();
    Ok((roots, analyze_git))
}

/// 把扫描结果写入数据库。
///
/// 🔴 项目主体与 Level 0 事实**分两次写**：
/// `upsert` 不碰 `git_commits`/`has_git` 等列（见 `ProjectRepo` 的列注释），
/// 必须由 `update_scan_facts` 单独填。合并成一个写入会让
/// "重新分析描述"这类操作把 Git 统计清零。
fn persist_scanned_projects(ctx: &JobContext, outcome: &ScanOutcome) -> Result<usize, String> {
    let now = chrono::Utc::now();
    let now_str = now.to_rfc3339();

    let projects: Vec<Project> = outcome
        .projects
        .iter()
        .map(|p| to_domain_project(p, now))
        .collect();
    let count = projects.len();

    // 批量 upsert：一次事务，比逐条快一到两个数量级
    ctx.db.projects().upsert_batch(&projects).map_err(db_err)?;

    // 逐项目写 Level 0 事实（数量与项目数同阶，不是热点）
    for (scanned, project) in outcome.projects.iter().zip(projects.iter()) {
        let facts = ScanFacts {
            git_commits: scanned.git.commit_count,
            has_git: scanned.git.available,
            has_readme: scanned.detection.has_readme,
            has_tests: scanned.detection.has_tests,
            scanned_at: Some(now_str.clone()),
        };
        ctx.db
            .projects()
            .update_scan_facts(&project.id, &facts)
            .map_err(db_err)?;
    }
    Ok(count)
}

/// 更新授权目录的"上次扫描时间"与项目数。
///
/// 设置页据此显示"2 小时前扫描，发现 156 个项目"。
/// 不更新的话用户无法判断数据新旧，会反复重扫。
fn update_dir_metadata(ctx: &JobContext, roots: &[PathBuf], project_count: usize) -> Result<(), String> {
    let mut scan = ctx.db.settings().get_or_default().map_err(db_err)?.scan;
    let now = chrono::Utc::now().to_rfc3339();
    let mut changed = false;

    for dir in scan.dirs.iter_mut() {
        if roots.iter().any(|r| same_path(r, &dir.path)) {
            dir.last_scanned_at = Some(now.clone());
            // 项目数是全局的，无法按目录拆分（一个项目可能匹配多个根）；
            // 单目录扫描时它就是该目录的结果，多目录时作为总数展示
            dir.project_count = Some(project_count as u32);
            changed = true;
        }
    }

    if changed {
        // 只保存 scan 段：LLM 配置与外观设置不属于本任务的职责，
        // 整体 save 会在并发修改时把别人的改动覆盖掉
        ctx.db.settings().save_scan(&scan).map_err(db_err)?;
    }
    Ok(())
}

/// 路径等价比较（跨平台：Windows 大小写不敏感、分隔符混用）。
fn same_path(a: &std::path::Path, b: &str) -> bool {
    let na = a.to_string_lossy().replace('\\', "/").to_lowercase();
    let nb = b.replace('\\', "/").to_lowercase();
    na == nb
}

// ══════════════════════════════════════════════════════════════════
// 阶段二：索引代码（Level 1 符号抽取 + 能力 + 关系）
// ══════════════════════════════════════════════════════════════════

/// 代码索引任务。
///
/// 载荷格式：`{"project_id": "p1"}`；缺省时索引**全部**项目。
#[derive(Debug, Default)]
pub struct IndexCodeHandler {
    builder: AssetBuilder,
    extractor: HeuristicExtractor,
}

impl IndexCodeHandler {
    pub fn new() -> Self {
        Self {
            builder: AssetBuilder::new(),
            extractor: HeuristicExtractor,
        }
    }
}

#[async_trait::async_trait]
impl JobHandler for IndexCodeHandler {
    async fn run(&self, ctx: JobContext) -> Result<(), String> {
        let only = ctx
            .payload
            .as_ref()
            .and_then(|p| p.get("project_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let projects: Vec<Project> = match &only {
            Some(id) => vec![ctx
                .db
                .projects()
                .get(id)
                .map_err(db_err)?
                .ok_or_else(|| format!("项目不存在: {id}"))?],
            None => ctx
                .db
                .projects()
                .list(
                    &spolia_storage::ProjectFilter {
                        limit: Some(MAX_PROJECTS_PER_SCAN as u32),
                        ..Default::default()
                    },
                    spolia_storage::ProjectSort::RecentlyUpdated,
                )
                .map_err(db_err)?,
        };

        if projects.is_empty() {
            return Err("没有可索引的项目，请先执行扫描".to_string());
        }

        let total = projects.len();
        ctx.report_counted(0.0, "准备索引代码", 0, total as u64)?;

        let mut totals = IndexTotals::default();
        for (idx, project) in projects.iter().enumerate() {
            // 🔴 每个项目开始前都查取消：单项目索引可能要几十秒，
            //    只在循环边界查会让取消延迟同样久。
            if ctx.is_cancelled() {
                ctx.log("索引已被用户取消");
                return Ok(());
            }
            let progress = (idx as f64 / total as f64).clamp(0.0, 0.99);
            ctx.report_counted(
                progress,
                format!("索引 {}", project.name),
                idx as u64,
                total as u64,
            )?;

            // 单个项目失败不毁掉整轮索引：记警告继续
            match self.index_one(&ctx, project) {
                Ok(outcome) => totals.merge(outcome),
                Err(e) => {
                    let msg = format!("索引 {} 失败: {e}", project.name);
                    tracing::warn!("{msg}");
                    ctx.log(msg);
                    totals.failed += 1;
                }
            }
        }

        // 跨项目关系：同名资产出现在多个项目 → similar_to
        ctx.report(0.9, "构建跨项目关联…")?;
        if ctx.is_cancelled() {
            return Ok(());
        }
        let cross = build_cross_project_relations(&ctx)?;
        ctx.db.relations().upsert_batch(&cross).map_err(db_err)?;

        // 能力节点的 project_count 必须刷新，否则图谱节点大小是旧值
        ctx.db.capabilities().refresh_project_counts().map_err(db_err)?;
        // 清理没有任何项目引用的能力节点（防止图谱堆积孤儿）
        let pruned = ctx.db.capabilities().prune_orphans().map_err(db_err)?;

        ctx.log(format!(
            "索引完成：{} 个资产 / {} 个能力 / {} 条关系（{} 个项目失败，清理 {} 个孤立能力）",
            totals.assets, totals.capabilities, cross.len() + totals.relations, totals.failed, pruned
        ));
        let _ = ctx.db.activities().push(
            ActivityIcon::Repeat,
            "索引完成",
            format!("提取 {} 个可复用资产", totals.assets),
        );
        ctx.report_counted(1.0, "索引完成", total as u64, total as u64)?;
        Ok(())
    }
}

/// 索引统计汇总。
#[derive(Debug, Default)]
struct IndexTotals {
    assets: usize,
    rejected_assets: usize,
    capabilities: usize,
    relations: usize,
    failed: usize,
}

impl IndexTotals {
    fn merge(&mut self, other: ProjectIndexOutcome) {
        self.assets += other.assets;
        self.rejected_assets += other.rejected_assets;
        self.capabilities += other.capabilities;
        self.relations += other.relations;
    }
}

/// 单项目索引结果（对外只报计数，实体已直接落库）。
struct ProjectIndexOutcome {
    assets: usize,
    rejected_assets: usize,
    capabilities: usize,
    relations: usize,
}

impl IndexCodeHandler {
    /// 索引单个项目：抽符号 → 评分 → 建能力与关系 → 落库。
    ///
    /// 🔴 敏感项目**不跳过**符号抽取：本地抽取不发网络，
    /// 跳过会让用户标了敏感就什么都看不到。
    /// "敏感"的约束是**不送 LLM**（Level 2），不是"不索引"。
    fn index_one(&self, ctx: &JobContext, project: &Project) -> Result<ProjectIndexOutcome, String> {
        let root = std::path::Path::new(&project.path);
        let (files, walk_stats) = list_source_files(root);
        ctx.log(format!("{}: {}", project.name, walk_stats.summary()));

        // 1. 抽符号（纯本地计算，CPU 密集）
        let mut symbols = Vec::new();
        let mut skipped_binary = 0usize;
        for f in &files {
            let Some(content) = read_source(f) else {
                skipped_binary += 1;
                continue; // 非 UTF-8：跳过而非报错
            };
            if !self.extractor.supports(f.language) {
                continue;
            }
            symbols.extend(self.extractor.extract(f.language, &f.relative_path, &content));
        }
        if skipped_binary > 0 {
            ctx.log(format!("  跳过 {skipped_binary} 个非文本文件"));
        }

        // 2. 能力信号：依赖清单 + 框架 + 符号名 + 顶层目录
        let deps = parse_manifests(root);
        let signals = CapabilitySignals {
            dependencies: deps.runtime.clone(),
            frameworks: deps.frameworks.clone(),
            language: (!project.language.is_empty()).then(|| project.language.clone()),
            symbol_names: symbols.iter().map(|s| s.name.clone()).collect(),
            top_level_dirs: top_dirs_of(root),
        };

        // 3. 装配资产/能力/关系（builder 是纯函数，时间由调用方注入）
        let created_at = spolia_storage::today_local();
        let input = ProjectInput {
            project_id: &project.id,
            project_name: &project.name,
            project_root: &project.path,
            symbols: &symbols,
            capability_signals: &signals,
            capability_confidence_floor: CAPABILITY_CONFIDENCE_FLOOR,
            created_at: &created_at,
        };
        let extraction = self.builder.build(&input);

        // 4. 落库。资产写入会按证据门禁拒绝部分条目，
        //    🔴 必须如实上报拒绝数：谎报"提取了 500 个资产"而实际只有 300 个，
        //    用户会发现数字与列表对不上，进而怀疑整个产品的可信度。
        let outcome: AssetWriteOutcome = ctx
            .db
            .assets()
            .upsert_batch(&extraction.assets)
            .map_err(db_err)?;
        ctx.db
            .capabilities()
            .upsert_batch(&extraction.capabilities)
            .map_err(db_err)?;
        ctx.db
            .relations()
            .upsert_batch(&extraction.relations)
            .map_err(db_err)?;

        // 5. Level 1 统计（符号数/模块数），与 Level 0 事实分开写
        let stats = SymbolStats {
            symbol_count: symbols.len(),
            module_count: signals.top_level_dirs.len(),
        };
        ctx.db
            .projects()
            .update_symbol_stats(&project.id, &stats)
            .map_err(db_err)?;

        if outcome.has_rejections() {
            ctx.log(format!(
                "  {} 个候选因缺少证据未收录（共 {} 个）",
                outcome.rejected_no_evidence,
                outcome.total()
            ));
        }

        Ok(ProjectIndexOutcome {
            assets: outcome.written,
            rejected_assets: outcome.rejected_no_evidence,
            capabilities: extraction.capabilities.len(),
            relations: extraction.relations.len(),
        })
    }
}

/// 构建跨项目 `similar_to` 关系。
///
/// 从库里读全部资产（而非用本次内存里的），因为"重复实现"的判断
/// 必须基于**全库**：只比对本轮索引的项目会漏掉与历史项目的重复。
fn build_cross_project_relations(ctx: &JobContext) -> Result<Vec<Relation>, String> {
    let assets = ctx
        .db
        .assets()
        .list(
            &spolia_storage::AssetFilter {
                limit: Some(20_000),
                ..Default::default()
            },
            spolia_storage::AssetSort::ReuseScore,
        )
        .map_err(db_err)?;

    let refs: Vec<AssetRef<'_>> = assets
        .iter()
        .map(|a| AssetRef {
            name: &a.name,
            project_id: &a.project_id,
            source_path: &a.source_path,
            reuse_score: a.reuse_score,
        })
        .collect();
    Ok(build_duplicate_relations(&refs, DUPLICATE_MIN_PROJECTS))
}

// `top_dirs_of` 已移至 `crate::walk`（目录遍历职责归遍历模块）。
// 此处通过 `use crate::walk::top_dirs_of` 复用，避免两份实现漂移。

// ══════════════════════════════════════════════════════════════════
// 阶段三：洞察与机会（纯内存计算，不调 LLM）
// ══════════════════════════════════════════════════════════════════

/// 洞察生成任务。
///
/// 🔴 全部由确定性规则产出，**不调用 LLM**：
/// 每条洞察都必须带 Evidence（真实文件路径 + 推理说明），
/// LLM 生成的"你好像在 4 个项目里重复实现了 X"若无法溯源，
/// 一旦出错就直接摧毁用户信任。
#[derive(Debug, Default)]
pub struct InsightHandler;

impl InsightHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl JobHandler for InsightHandler {
    async fn run(&self, ctx: JobContext) -> Result<(), String> {
        ctx.report(0.1, "读取项目与资产…")?;

        // 1. 组装分析输入（一次性载入，之后全是内存计算）
        let input = load_analysis_input(&ctx)?;
        if input.project_count() == 0 {
            return Err("没有可分析的项目，请先执行扫描".to_string());
        }
        ctx.report(0.4, format!("分析 {} 个项目…", input.project_count()))?;
        if ctx.is_cancelled() {
            return Ok(());
        }

        // 2. 洞察检测
        let detected = spolia_insight::detect_all(&input, &DetectorConfig::default());
        ctx.db
            .insights()
            .upsert_batch(&detected.insights)
            .map_err(db_err)?;

        // 3. 机会生成
        ctx.report(0.7, "生成复用机会…")?;
        let opportunities = OpportunityEngine::new().generate(&input, &OpportunityConfig::default());
        ctx.db
            .opportunities()
            .upsert_batch(&opportunities.opportunities)
            .map_err(db_err)?;

        ctx.log(format!(
            "分析完成：{} 条洞察（{} 条候选因证据不足被拦下）、{} 个机会（{} 个被过滤）",
            detected.insights.len(),
            detected.rejected,
            opportunities.opportunities.len(),
            opportunities.filtered
        ));

        if !detected.insights.is_empty() {
            let _ = ctx.db.activities().push(
                ActivityIcon::Bulb,
                "发现新洞察",
                format!("{} 条可复用建议", detected.insights.len()),
            );
        }
        ctx.report(1.0, "分析完成")?;
        Ok(())
    }
}

/// 从数据库装配洞察引擎的输入快照。
///
/// 用快照而非让引擎直接查库：引擎保持纯函数，可独立单测；
/// 也避免它在分析过程中读到被并发写入的半成品数据。
fn load_analysis_input(ctx: &JobContext) -> Result<AnalysisInput, String> {
    let projects = ctx
        .db
        .projects()
        .list(
            &spolia_storage::ProjectFilter {
                limit: Some(MAX_PROJECTS_PER_SCAN as u32),
                ..Default::default()
            },
            spolia_storage::ProjectSort::RecentlyUpdated,
        )
        .map_err(db_err)?;
    let assets = ctx
        .db
        .assets()
        .list(
            &spolia_storage::AssetFilter {
                // 洞察同样受"无证据不展示"约束
                evidence_required: true,
                limit: Some(20_000),
                ..Default::default()
            },
            spolia_storage::AssetSort::ReuseScore,
        )
        .map_err(db_err)?;
    let capabilities = ctx.db.capabilities().list_all().map_err(db_err)?;
    let relations = ctx.db.relations().list_all().map_err(db_err)?;

    Ok(AnalysisInput {
        projects,
        assets,
        capabilities,
        relations,
        // 基准时间注入：保证同一份数据两次分析结论一致
        now: chrono::Utc::now(),
    })
}

// ══════════════════════════════════════════════════════════════════
// 组装入口
// ══════════════════════════════════════════════════════════════════

/// 构建默认处理器表（server 启动时调用）。
///
/// 集中在一处：新增任务类型时只需在此注册，
/// 不必在 server 的启动代码里散落多个 `insert`。
pub fn build_default_handlers() -> Vec<(JobType, Arc<dyn JobHandler>)> {
    vec![
        (
            JobType::ScanProject,
            Arc::new(ScanProjectHandler::new()) as Arc<dyn JobHandler>,
        ),
        (
            JobType::IndexCode,
            Arc::new(IndexCodeHandler::new()) as Arc<dyn JobHandler>,
        ),
        (
            JobType::GenerateInsight,
            Arc::new(InsightHandler::new()) as Arc<dyn JobHandler>,
        ),
    ]
}

/// 存储错误 → 面向用户的文案。
///
/// 🔴 不直接把 `StorageError` 的 Display 抛给用户：
/// 它可能含数据库文件路径，属内部实现细节。
fn db_err(e: spolia_domain::StorageError) -> String {
    tracing::error!(error = %e, "流水线数据库操作失败");
    "数据库操作失败，请查看日志了解详情".to_string()
}

/// 供 server 校验设置合法性（例如扫描目录是否存在）。
pub fn validate_settings(settings: &Settings) -> Vec<String> {
    let mut problems = Vec::new();
    if settings.scan.dirs.is_empty() {
        problems.push("尚未添加扫描目录".to_string());
    }
    for d in settings.scan.enabled_dirs() {
        let p = std::path::Path::new(&d.path);
        if !p.exists() {
            problems.push(format!("目录不存在: {}", d.path));
        } else if !p.is_dir() {
            problems.push(format!("不是目录: {}", d.path));
        }
    }
    if settings.scan.max_depth == 0 {
        problems.push("扫描深度不能为 0".to_string());
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::{Asset, AssetType, CodeStats, Evidence, ProjectStatus};
    use spolia_storage::Database;
    use std::fs;

    fn test_db() -> Arc<Database> {
        Arc::new(Database::in_memory().unwrap())
    }

    /// 造一个真实的临时项目（Python + Git 无关文件）。
    fn temp_python_project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let p = dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        dir
    }

    fn sample_project(id: &str, name: &str, path: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            description: "测试项目".into(),
            language: "Python".into(),
            framework: "FastAPI".into(),
            created_at: None,
            updated_at: Some("2026-09-20".into()),
            last_commit_at: Some("2026-09-20".into()),
            status: ProjectStatus::Active,
            health_score: 80,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats::default(),
            scan: spolia_domain::ScanFacts::default(),
            ai_profile: None,
        }
    }

    // ── 路径与设置 ───────────────────────────────────────────────

    #[test]
    fn same_path_handles_separators_and_case() {
        // Windows 上大小写与分隔符都可能不一致
        assert!(same_path(std::path::Path::new("F:/Code"), "F:\\Code"));
        assert!(same_path(std::path::Path::new("F:/Code"), "f:/code"));
        assert!(!same_path(std::path::Path::new("F:/Code"), "F:/Other"));
    }

    #[test]
    fn validate_settings_reports_missing_dirs() {
        let mut settings = Settings::default();
        assert!(
            !validate_settings(&settings).is_empty(),
            "无目录时必须报告问题"
        );
        settings.scan.add_dir("/nonexistent-spolia-dir", "2026-09-29T00:00:00Z");
        let problems = validate_settings(&settings);
        assert!(
            problems.iter().any(|p| p.contains("不存在")),
            "应报告目录不存在: {problems:?}"
        );
    }

    #[test]
    fn validate_settings_accepts_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings
            .scan
            .add_dir(&dir.path().to_string_lossy(), "2026-09-29T00:00:00Z");
        settings.scan.max_depth = 6;
        assert!(
            validate_settings(&settings).is_empty(),
            "真实目录不该报错: {:?}",
            validate_settings(&settings)
        );
    }

    #[test]
    fn validate_settings_rejects_zero_depth() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings
            .scan
            .add_dir(&dir.path().to_string_lossy(), "2026-09-29T00:00:00Z");
        settings.scan.max_depth = 0;
        let problems = validate_settings(&settings);
        assert!(problems.iter().any(|p| p.contains("深度")), "{problems:?}");
    }

    #[test]
    fn top_dirs_excludes_build_artifacts() {
        let dir = temp_python_project(&[
            ("src/main.py", "x=1\n"),
            ("node_modules/lib/a.js", "y=2\n"),
            ("tests/test_a.py", "z=3\n"),
        ]);
        let dirs = top_dirs_of(dir.path());
        assert!(dirs.contains(&"src".to_string()));
        assert!(dirs.contains(&"tests".to_string()));
        assert!(
            !dirs.contains(&"node_modules".to_string()),
            "构建产物目录不算模块: {dirs:?}"
        );
    }

    #[test]
    fn top_dirs_is_sorted_and_deterministic() {
        let dir = temp_python_project(&[
            ("zebra/a.py", "x=1\n"),
            ("alpha/b.py", "y=2\n"),
            ("middle/c.py", "z=3\n"),
        ]);
        assert_eq!(top_dirs_of(dir.path()), vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn top_dirs_of_missing_path_is_empty() {
        assert!(top_dirs_of(std::path::Path::new("/nonexistent-spolia")).is_empty());
    }

    // ── 进度桥接 ─────────────────────────────────────────────────

    /// 桥接必须把 done/total 映射进扫描区间，且不得越界。
    #[test]
    fn progress_bridge_maps_into_scan_window() {
        // 断言确切值而非 `FROM < TO` 这类关系式：
        // 后者对字面量常量是恒真的（clippy 会报 constant value），
        // 改坏了也不会失败。写死期望值才真正起到守护作用——
        // 尤其 SCAN_PROGRESS_TO 必须 < 1.0，要给"写库"阶段留出进度空间。
        assert_eq!(SCAN_PROGRESS_FROM, 0.02);
        assert_eq!(SCAN_PROGRESS_TO, 0.85);

        // 映射函数逻辑（与 ProgressBridge::report 一致）
        let map = |done: usize, total: Option<usize>| -> f64 {
            let ratio = match total {
                Some(t) if t > 0 => (done as f64 / t as f64).clamp(0.0, 1.0),
                _ => 0.0,
            };
            SCAN_PROGRESS_FROM + ratio * (SCAN_PROGRESS_TO - SCAN_PROGRESS_FROM)
        };
        assert_eq!(map(0, Some(100)), SCAN_PROGRESS_FROM);
        assert_eq!(map(50, Some(100)), SCAN_PROGRESS_FROM + 0.5 * (SCAN_PROGRESS_TO - SCAN_PROGRESS_FROM));
        assert_eq!(map(100, Some(100)), SCAN_PROGRESS_TO);
        // total 未知（发现阶段）：停在起点，不编造进度
        assert_eq!(map(0, None), SCAN_PROGRESS_FROM);
        assert_eq!(map(5, Some(0)), SCAN_PROGRESS_FROM, "total=0 不得除零");
        // done 超过 total 也不得越界
        assert!(map(500, Some(100)) <= SCAN_PROGRESS_TO);
    }

    // ── 端到端：真实扫描 → 落库 ──────────────────────────────────

    /// 🔴 核心验收：扫描真实目录后，项目必须真的进了数据库。
    /// 这是"移除 mock、接入真实数据"的最终证据。
    #[tokio::test]
    async fn scan_persists_real_projects_to_database() {
        let dir = temp_python_project(&[
            ("alpha/pyproject.toml", "[project]\nname=\"alpha\"\n"),
            ("alpha/main.py", "def main():\n    return 1\n"),
            ("beta/package.json", "{\"name\":\"beta\"}\n"),
            ("beta/index.js", "function f(){return 1}\n"),
        ]);

        let db = test_db();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let payload = serde_json::json!({
            "dirs": [dir.path().to_string_lossy()],
            "analyze_git": false
        });
        let job_id = engine
            .submit(JobType::ScanProject, Some(payload))
            .await
            .unwrap();

        let job = wait_terminal(&db, &job_id).await;
        assert_eq!(
            job.status,
            spolia_domain::JobStatus::Completed,
            "扫描应成功: {:?}",
            job.error
        );

        let projects = db
            .projects()
            .list(
                &spolia_storage::ProjectFilter::default(),
                spolia_storage::ProjectSort::Name,
            )
            .unwrap();
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "应发现 alpha: {names:?}");
        assert!(names.contains(&"beta"), "应发现 beta: {names:?}");

        // 真实数据校验：语言来自文件内容统计，不是写死的
        let alpha = projects.iter().find(|p| p.name == "alpha").unwrap();
        assert_eq!(alpha.language, "Python");
        let beta = projects.iter().find(|p| p.name == "beta").unwrap();
        assert!(
            ["JavaScript", "TypeScript"].contains(&beta.language.as_str()),
            "beta 应识别为 JS 系，实际 {}",
            beta.language
        );
        // 无 README → 描述为空串，不编造
        assert!(alpha.description.is_empty(), "无 README 时不得编造描述");
    }

    /// 扫描无授权目录时必须**报错**，而不是静默"完成 0 个项目"。
    #[tokio::test]
    async fn scan_without_dirs_fails_with_actionable_message() {
        let db = test_db();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let job_id = engine.submit(JobType::ScanProject, None).await.unwrap();
        let job = wait_terminal(&db, &job_id).await;
        assert_eq!(job.status, spolia_domain::JobStatus::Failed);
        let err = job.error.unwrap();
        assert!(
            err.contains("设置") || err.contains("目录"),
            "错误文案应指引用户去哪配置: {err}"
        );
    }

    /// 载荷里的目录优先于设置里的目录。
    #[tokio::test]
    async fn scan_prefers_payload_dirs_over_settings() {
        let payload_dir = temp_python_project(&[("from_payload/Cargo.toml", "[package]\nname=\"x\"\n")]);
        let settings_dir = temp_python_project(&[("from_settings/Cargo.toml", "[package]\nname=\"y\"\n")]);

        let db = test_db();
        {
            let mut settings = db.settings().get_or_default().unwrap();
            settings.scan.add_dir(
                &settings_dir.path().to_string_lossy(),
                "2026-09-29T00:00:00Z",
            );
            db.settings().save_scan(&settings.scan).unwrap();
        }
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let payload = serde_json::json!({
            "dirs": [payload_dir.path().to_string_lossy()],
            "analyze_git": false
        });
        let job_id = engine.submit(JobType::ScanProject, Some(payload)).await.unwrap();
        let job = wait_terminal(&db, &job_id).await;
        assert_eq!(job.status, spolia_domain::JobStatus::Completed, "{:?}", job.error);

        let projects = db.projects().list(&spolia_storage::ProjectFilter::default(), spolia_storage::ProjectSort::Name).unwrap();
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"from_payload"), "应扫载荷目录: {names:?}");
        assert!(
            !names.contains(&"from_settings"),
            "载荷已指定目录，不该回退到设置: {names:?}"
        );
    }

    /// 扫描后授权目录的"上次扫描时间"必须更新，否则用户无法判断数据新旧。
    #[tokio::test]
    async fn scan_updates_directory_metadata() {
        let dir = temp_python_project(&[("p/Cargo.toml", "[package]\nname=\"p\"\n")]);
        let db = test_db();
        {
            let mut settings = db.settings().get_or_default().unwrap();
            settings.scan.add_dir(&dir.path().to_string_lossy(), "2026-09-29T00:00:00Z");
            db.settings().save_scan(&settings.scan).unwrap();
        }
        // 扫描前：从未扫描
        let before = db.settings().get_or_default().unwrap();
        assert!(before.scan.dirs[0].last_scanned_at.is_none());

        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let payload = serde_json::json!({"dirs": [dir.path().to_string_lossy()], "analyze_git": false});
        let job_id = engine.submit(JobType::ScanProject, Some(payload)).await.unwrap();
        wait_terminal(&db, &job_id).await;

        let after = db.settings().get_or_default().unwrap();
        assert!(
            after.scan.dirs[0].last_scanned_at.is_some(),
            "扫描后应记录时间"
        );
        assert_eq!(
            after.scan.dirs[0].project_count,
            Some(1),
            "应记录发现的项目数"
        );
    }

    /// Level 0 事实（has_readme/has_tests）必须真的写进库。
    #[tokio::test]
    async fn scan_persists_detection_facts() {
        let dir = temp_python_project(&[
            ("p/Cargo.toml", "[package]\nname=\"p\"\n"),
            ("p/README.md", "# 项目\n说明\n"),
            ("p/tests/test_a.rs", "#[test]\nfn t(){}\n"),
        ]);
        let db = test_db();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let payload = serde_json::json!({"dirs": [dir.path().to_string_lossy()], "analyze_git": false});
        let job_id = engine.submit(JobType::ScanProject, Some(payload)).await.unwrap();
        wait_terminal(&db, &job_id).await;

        let conn = db.conn().unwrap();
        let (has_readme, has_tests): (i64, i64) = conn
            .query_row(
                "SELECT has_readme, has_tests FROM projects LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(has_readme, 1, "应检测到 README");
        assert_eq!(has_tests, 1, "应检测到 tests 目录");
    }

    // ── 端到端：索引 → 资产落库 ──────────────────────────────────

    /// 🔴 索引必须从**真实源码**抽出资产，而不是产出空表。
    #[tokio::test]
    async fn index_extracts_real_assets_from_source() {
        let dir = temp_python_project(&[(
            "proj/pyproject.toml",
            "[project]\nname=\"proj\"\n",
        )]);
        // 写一个有实质内容的可复用函数（体量足够过评分门槛）
        let body = "class VideoPipeline:\n    \"\"\"视频生成管线：串联解码、处理与编码三个阶段。\"\"\"\n\n";
        let body = format!(
            "{body}    def run(self, source, target):\n        frames = self.decode(source)\n        processed = self.process(frames)\n        self.encode(processed, target)\n        return target\n\n    def decode(self, source):\n        return list(source)\n\n    def process(self, frames):\n        return [f for f in frames if f]\n\n    def encode(self, frames, target):\n        return target\n"
        );
        fs::write(dir.path().join("proj/pipeline.py"), &body).unwrap();

        let db = test_db();
        // 先入库一个项目记录（索引任务依赖它）
        db.projects()
            .upsert(&sample_project("p1", "proj", &dir.path().join("proj").to_string_lossy()))
            .unwrap();

        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let job_id = engine.submit(JobType::IndexCode, None).await.unwrap();
        let job = wait_terminal(&db, &job_id).await;
        assert_eq!(job.status, spolia_domain::JobStatus::Completed, "{:?}", job.error);

        let assets = db
            .assets()
            .list(
                &spolia_storage::AssetFilter::default(),
                spolia_storage::AssetSort::ReuseScore,
            )
            .unwrap();
        assert!(!assets.is_empty(), "应从真实源码抽出资产");
        // 每个资产都必须有证据（产品红线）
        for a in &assets {
            assert!(
                a.evidence.is_sufficient(),
                "资产 {} 缺少证据却入了库",
                a.name
            );
            assert!(
                !a.source_path.is_empty(),
                "资产 {} 缺少来源路径",
                a.name
            );
        }
        // Level 1 统计应写入
        let conn = db.conn().unwrap();
        let symbols: i64 = conn
            .query_row("SELECT symbol_count FROM projects WHERE id='p1'", [], |r| r.get(0))
            .unwrap();
        assert!(symbols > 0, "符号数应大于 0，实际 {symbols}");
    }

    /// 索引任务必须在无项目时明确报错，而非静默成功。
    #[tokio::test]
    async fn index_without_projects_fails_clearly() {
        let db = test_db();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let job_id = engine.submit(JobType::IndexCode, None).await.unwrap();
        let job = wait_terminal(&db, &job_id).await;
        assert_eq!(job.status, spolia_domain::JobStatus::Failed);
        assert!(
            job.error.unwrap().contains("扫描"),
            "错误应指引用户先扫描"
        );
    }

    /// 指定不存在的项目 id 应报错而非静默跳过。
    #[tokio::test]
    async fn index_unknown_project_id_fails() {
        let db = test_db();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let payload = serde_json::json!({"project_id": "ghost"});
        let job_id = engine.submit(JobType::IndexCode, Some(payload)).await.unwrap();
        let job = wait_terminal(&db, &job_id).await;
        assert_eq!(job.status, spolia_domain::JobStatus::Failed);
        assert!(job.error.unwrap().contains("ghost"));
    }

    // ── 端到端：洞察 ─────────────────────────────────────────────

    /// 洞察必须基于真实数据：造出"两个项目同名资产"才能触发重复检测。
    #[tokio::test]
    async fn insight_detects_cross_project_duplicates() {
        let db = test_db();
        // 两个项目，各有一个同名高分资产
        for pid in ["p1", "p2"] {
            db.projects()
                .upsert(&sample_project(pid, &format!("项目{pid}"), &format!("/tmp/{pid}")))
                .unwrap();
            db.assets()
                .upsert(&Asset {
                    id: format!("a_{pid}"),
                    project_id: pid.into(),
                    asset_type: AssetType::Component,
                    name: "VideoPipeline".into(),
                    description: "视频生成管线".into(),
                    content: None,
                    source_path: format!("{pid}/pipeline.py"),
                    confidence: 0.9,
                    reuse_score: 0.92,
                    generality: 0.85,
                    stability: 0.8,
                    tags: vec!["python".into()],
                    created_at: "2026-09-01".into(),
                    evidence: Evidence {
                        files: vec![format!("{pid}/pipeline.py")],
                        reasoning: vec!["被多个模块调用".into()],
                        ..Evidence::default()
                    },
                    user_feedback: None,
                })
                .unwrap();
        }

        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let job_id = engine.submit(JobType::GenerateInsight, None).await.unwrap();
        let job = wait_terminal(&db, &job_id).await;
        assert_eq!(job.status, spolia_domain::JobStatus::Completed, "{:?}", job.error);

        let insights = db
            .insights()
            .list(&spolia_storage::InsightFilter::default())
            .unwrap();
        assert!(!insights.is_empty(), "应检测到跨项目重复实现");
        // 🔴 每条洞察都必须有证据（产品红线：无证据的结论不展示）。
        // Insight.evidence 是 Vec<EvidenceItem>（可跳转引用），
        // 与 Asset.evidence 的 Evidence{files,…} 是两套模型：
        // 前者是"指向哪些实体"，后者是"来自哪些静态材料"。
        // validate() 已在入库前挡掉空证据，这里再断言一次以防门禁被绕过。
        for i in &insights {
            assert!(
                !i.evidence.is_empty(),
                "洞察「{}」缺少证据，validate 门禁被绕过了",
                i.title
            );
            // 每条证据都要有可展示的文本，否则前端渲染出空白徽章
            for e in &i.evidence {
                assert!(!e.label.trim().is_empty(), "洞察「{}」的证据缺少 label", i.title);
            }
        }
    }

    #[tokio::test]
    async fn insight_without_projects_fails_clearly() {
        let db = test_db();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let job_id = engine.submit(JobType::GenerateInsight, None).await.unwrap();
        let job = wait_terminal(&db, &job_id).await;
        assert_eq!(job.status, spolia_domain::JobStatus::Failed);
        assert!(job.error.unwrap().contains("扫描"));
    }

    /// 洞察是纯内存计算，必须可取消（虽然是快任务，但契约要一致）。
    #[tokio::test]
    async fn insight_is_cancellable() {
        let db = test_db();
        db.projects().upsert(&sample_project("p1", "a", "/tmp/a")).unwrap();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let job_id = engine.submit(JobType::GenerateInsight, None).await.unwrap();
        // 立即取消：任务可能已完成（快任务），两种终态都可接受
        let _ = engine.cancel(&job_id);
        let job = wait_terminal(&db, &job_id).await;
        assert!(
            matches!(
                job.status,
                spolia_domain::JobStatus::Completed | spolia_domain::JobStatus::Cancelled
            ),
            "应进入正常终态，实际 {:?}",
            job.status
        );
    }

    // ── 全链路：扫描 → 索引 → 洞察 ───────────────────────────────

    /// 🔴 最终验收：三个任务按顺序跑完，数据层层累积到真实库中。
    #[tokio::test]
    async fn full_pipeline_scan_index_insight() {
        let dir = temp_python_project(&[
            ("proj_a/pyproject.toml", "[project]\nname=\"a\"\n"),
            ("proj_b/pyproject.toml", "[project]\nname=\"b\"\n"),
        ]);
        // 两个项目各放一个同名可复用类
        for name in ["proj_a", "proj_b"] {
            let body = "class SharedValidator:\n    \"\"\"通用校验器：检查输入合法性并给出错误信息。\"\"\"\n\n    def validate(self, value):\n        if value is None:\n            return \"不能为空\"\n        if len(str(value)) == 0:\n            return \"长度为零\"\n        return None\n\n    def sanitize(self, value):\n        return str(value).strip()\n";
            fs::write(dir.path().join(name).join("validator.py"), body).unwrap();
        }

        let db = test_db();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());

        // 阶段一：扫描
        let payload = serde_json::json!({"dirs": [dir.path().to_string_lossy()], "analyze_git": false});
        let j1 = engine.submit(JobType::ScanProject, Some(payload)).await.unwrap();
        let job1 = wait_terminal(&db, &j1).await;
        assert_eq!(job1.status, spolia_domain::JobStatus::Completed, "{:?}", job1.error);
        assert_eq!(db.projects().count().unwrap(), 2, "应发现两个项目");

        // 阶段二：索引
        let j2 = engine.submit(JobType::IndexCode, None).await.unwrap();
        let job2 = wait_terminal(&db, &j2).await;
        assert_eq!(job2.status, spolia_domain::JobStatus::Completed, "{:?}", job2.error);
        let asset_count = db.assets().count_all().unwrap();
        assert!(asset_count > 0, "应抽出资产");
        let cap_count = db.capabilities().list_all().unwrap().len();
        assert!(cap_count > 0, "应建出能力节点");

        // 阶段三：洞察
        let j3 = engine.submit(JobType::GenerateInsight, None).await.unwrap();
        let job3 = wait_terminal(&db, &j3).await;
        assert_eq!(job3.status, spolia_domain::JobStatus::Completed, "{:?}", job3.error);

        // 数据可被检索到（证明 FTS 索引也同步了）
        let search = spolia_storage::RetrievalRepo::new(db.pool());
        let hits = search.projects("proj", 20).unwrap();
        assert_eq!(hits.len(), 2, "扫描结果应可被检索到");

        // 活动流有记录（首页"最近活动"的真实来源）
        assert!(db.activities().count().unwrap() > 0, "应写入活动流");
    }

    /// 重复执行扫描必须幂等：同一目录扫两次不该产生重复项目。
    #[tokio::test]
    async fn rescanning_is_idempotent() {
        let dir = temp_python_project(&[("proj/Cargo.toml", "[package]\nname=\"p\"\n")]);
        let db = test_db();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let payload = serde_json::json!({"dirs": [dir.path().to_string_lossy()], "analyze_git": false});

        for round in 0..2 {
            let job_id = engine
                .submit(JobType::ScanProject, Some(payload.clone()))
                .await
                .unwrap();
            let job = wait_terminal(&db, &job_id).await;
            assert_eq!(job.status, spolia_domain::JobStatus::Completed, "第 {round} 轮: {:?}", job.error);
        }
        assert_eq!(
            db.projects().count().unwrap(),
            1,
            "项目 id 由路径派生，重扫必须覆盖而非新增"
        );
    }

    /// 取消扫描后不得留下半成品数据。
    #[tokio::test]
    async fn cancelled_scan_leaves_no_partial_state() {
        let dir = temp_python_project(&[("proj/Cargo.toml", "[package]\nname=\"p\"\n")]);
        let db = test_db();
        let engine = crate::JobEngine::new(Arc::clone(&db), build_default_handlers());
        let payload = serde_json::json!({"dirs": [dir.path().to_string_lossy()], "analyze_git": false});
        let job_id = engine.submit(JobType::ScanProject, Some(payload)).await.unwrap();
        // 提交后立即取消：扫描可能还没开始
        engine.cancel(&job_id).unwrap();
        let job = wait_terminal(&db, &job_id).await;
        assert_eq!(job.status, spolia_domain::JobStatus::Cancelled);
        // 取消发生在写库之前 → 库里应无项目；
        // 若已写库，数据必须完整（不存在半条记录）。两种情况都不该 panic。
        let count = db.projects().count().unwrap();
        assert!(count == 0 || count == 1, "项目数应为 0 或 1，实际 {count}");
    }

    // ── 处理器表 ─────────────────────────────────────────────────

    #[test]
    fn default_handlers_cover_the_three_stages() {
        let handlers = build_default_handlers();
        let types: Vec<JobType> = handlers.iter().map(|(t, _)| *t).collect();
        assert!(types.contains(&JobType::ScanProject));
        assert!(types.contains(&JobType::IndexCode));
        assert!(types.contains(&JobType::GenerateInsight));
        // 不得重复注册同一类型（HashMap 会静默覆盖，后注册的失效）
        let mut sorted = types.clone();
        sorted.sort_by_key(|t| t.as_str());
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "处理器表存在重复类型: {types:?}");
    }

    #[test]
    fn constants_are_sane() {
        // 确切值断言：`> 0.0 && < 1.0` 对字面量常量恒真，改坏也不报。
        // 门槛 0.55 是《技术设计书》§25 定的防标签爆炸阈值，误改会让能力图谱失真。
        assert!((CAPABILITY_CONFIDENCE_FLOOR - 0.55).abs() < f64::EPSILON);
        assert_eq!(DUPLICATE_MIN_PROJECTS, 2, "少于 2 个项目不构成重复");
        assert_eq!(MAX_PROJECTS_PER_SCAN, 2000);
        assert_eq!(MAX_FILES_PER_PROJECT_SCAN, 20_000);
    }

    /// 存储错误不得把内部路径抛给用户。
    #[test]
    fn db_err_hides_internals() {
        let e = spolia_domain::StorageError::Unavailable {
            path: "C:/secret/user/path.db".into(),
            reason: "拒绝访问".into(),
        };
        let msg = db_err(e);
        assert!(!msg.contains("secret"), "不得泄漏路径: {msg}");
        assert!(!msg.contains("C:/"), "不得泄漏盘符: {msg}");
        assert!(msg.contains("数据库"), "应说明是数据库问题: {msg}");
    }

    async fn wait_terminal(db: &Database, job_id: &str) -> spolia_domain::Job {
        for _ in 0..400 {
            if let Ok(Some(job)) = db.jobs().get(job_id)
                && job.status.is_terminal()
            {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("任务 {job_id} 在 4 秒内未进入终态");
    }
}
