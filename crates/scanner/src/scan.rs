//! 目录扫描与项目发现（Level 0 + Level 1 的静态部分）。
//!
//! # 与产品验收标准的对应
//! 《产品设计书》V0.1 功能 1：**1000 项目目录扫描 ≤ 60 秒；不误报非项目文件夹**。
//! 本模块的设计直接服务于这两条：
//! - 用 `ignore` crate 遍历：原生 `.gitignore` 感知 + 多线程，与 ripgrep 同源
//! - 内置排除清单（node_modules 等）不可关闭，从源头砍掉绝大部分 IO
//! - 项目判定基于标记文件（见 `markers.rs`），不靠目录名猜测
//!
//! # 可取消
//! 《技术设计书》§15：所有长任务必须可取消。
//! 这里通过传入 `&AtomicBool` 取消标志实现——遍历循环每层检查一次，
//! 收到取消信号后尽快返回，而不是等整轮扫描跑完。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use spolia_domain::ScannerError;

use crate::deps::{parse_manifests, Dependencies};
use crate::git::{mtime_of_newest_file, to_iso_date, GitAnalyzer, GitInfo};
use crate::language::{language_of, count_file_lines, FileStats, StatsAccumulator, MAX_READ_BYTES};
use crate::markers::{self, detect, Detection, ProjectKind};

/// 单个被发现的目录（尚未分析）。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateDir {
    pub path: PathBuf,
    pub entries: Vec<String>,
    pub depth: usize,
}

/// 扫描配置。
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// 授权扫描的根目录（目录级授权：未列出的目录不读取）
    pub roots: Vec<PathBuf>,
    /// 额外排除 glob（用户配置，叠加在内置排除之上）
    pub exclude_patterns: Vec<String>,
    /// 从根目录算起的最大深度。
    ///
    /// 必须有限：用户可能授权 `D:\`，无深度限制会遍历整块盘（数小时 + 数百万文件）。
    /// 默认 6 层足够覆盖 `<root>/<owner>/<repo>` 与 `<root>/<group>/<sub>/<repo>` 布局。
    pub max_depth: usize,
    /// 最多发现多少个项目（防御性上限，避免 UI 被淹没）
    pub max_projects: usize,
    /// 是否分析 Git 历史（关闭可显著加速，用于快速预览）
    pub analyze_git: bool,
    /// 是否统计代码行（Level 1）
    pub count_code: bool,
    /// 每个项目统计代码时最多读取多少个文件
    pub max_files_per_project: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            exclude_patterns: Vec::new(),
            max_depth: 6,
            max_projects: 2000,
            analyze_git: true,
            count_code: true,
            max_files_per_project: 20_000,
        }
    }
}

/// 扫描进度回调。
///
/// 用 trait object 而非 channel：调用方（Job Engine）需要把进度写入 DB 并广播事件，
/// 直接给一个闭包比让扫描器感知 channel 类型更简单，也便于测试时收集进度。
///
/// `Sync` 是必需的：项目分析用 rayon 并行，多个线程会同时上报进度。
/// 实现方若需要可变状态（如收集日志），应使用 `Mutex` 包裹。
pub trait ProgressSink: Send + Sync {
    /// 上报进度。
    ///
    /// 🔴 结构化参数，而不是把百分比塞进文案：
    /// 早期签名是 `report(stage: &str, found: usize)`，扫描器把 `"(45%)"`
    /// 拼进 stage 字符串里。后果是上层（Job Engine）为了填 `progress: f64`
    /// 必须**反向解析自己刚拼出来的文案**——格式一改就静默失真，
    /// 而且 `found` 语义含糊（候选数？完成数？）。
    ///
    /// 现在把"给人看的文案"与"给机器算的数值"彻底分开：
    /// - `stage`：纯文案，不含百分比
    /// - `done` / `total`：真实计数，`total` 未知时为 `None`
    fn report(&self, stage: &str, done: usize, total: Option<usize>);

    /// 追加一条日志行（扫描日志面板）。
    fn log(&self, line: &str);
}

/// 空实现：不需要进度时使用。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report(&self, _stage: &str, _done: usize, _total: Option<usize>) {}
    fn log(&self, _line: &str) {}
}

/// 一个已发现的项目的完整静态画像。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedProject {
    /// 稳定 id：由绝对路径派生（同一路径多次扫描得到同一 id，保证幂等）
    pub id: String,
    pub name: String,
    pub path: String,
    pub kind: ProjectKind,
    /// 判定依据（标记文件名），作为 Evidence 展示
    pub marker: String,
    pub language: String,
    pub framework: String,
    pub tags: Vec<String>,
    pub dependencies: Dependencies,
    pub git: GitInfo,
    pub detection: DetectionFlags,
    pub stats: ScannedStats,
    /// 相对项目根的顶层目录名（模块数与目录树的来源）
    pub top_level_dirs: Vec<String>,
    /// 相对项目根的顶层文件名
    pub top_level_files: Vec<String>,
    /// README 首段（description 的真实来源；无 README 则为空串）
    pub readme_excerpt: String,
}

/// 布尔特征位（从 `Detection` 摘出，避免把 markers 类型带进序列化契约）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct DetectionFlags {
    pub has_git: bool,
    pub has_readme: bool,
    pub has_tests: bool,
    pub has_license: bool,
    pub has_docker: bool,
}

impl From<&Detection> for DetectionFlags {
    fn from(d: &Detection) -> Self {
        Self {
            has_git: d.has_git,
            has_readme: d.has_readme,
            has_tests: d.has_tests,
            has_license: d.has_license,
            has_docker: d.has_docker,
        }
    }
}

/// 代码统计结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScannedStats {
    pub files: usize,
    pub loc: usize,
    pub symbols: usize,
    pub modules: usize,
    pub languages: Vec<spolia_domain::LanguageShare>,
    /// 因判定为生成物而跳过的文件数（诊断用）
    pub skipped_generated: usize,
    /// 未知扩展名文件数
    pub unknown_files: usize,
}

/// 一轮扫描的结果。
#[derive(Debug, Clone, Default)]
pub struct ScanOutcome {
    pub projects: Vec<ScannedProject>,
    /// 遍历过的目录数（性能诊断）
    pub dirs_walked: usize,
    /// 被排除规则跳过的目录数
    pub dirs_skipped: usize,
    /// 是否被取消
    pub cancelled: bool,
    /// 扫描耗时（毫秒）
    pub elapsed_ms: u128,
    /// 警告（非致命问题，如"某目录无读取权限"）
    pub warnings: Vec<String>,
}

impl ScanOutcome {
    /// 发现的项目数。
    pub fn project_count(&self) -> usize {
        self.projects.len()
    }
}

/// 扫描器。
#[derive(Debug, Clone)]
pub struct Scanner {
    config: ScanConfig,
    git: GitAnalyzer,
}

impl Scanner {
    pub fn new(config: ScanConfig) -> Self {
        Self { config, git: GitAnalyzer::new() }
    }

    pub fn with_git(config: ScanConfig, git: GitAnalyzer) -> Self {
        Self { config, git }
    }

    pub fn config(&self) -> &ScanConfig {
        &self.config
    }

    /// 扫描全部授权目录。
    ///
    /// 流程（对应《技术设计书》§14 三级分析的 Level 0/1）：
    /// 1. 发现候选目录（不读文件内容，只看目录项）
    /// 2. 对每个候选做静态分析：标记、语言统计、依赖、Git
    /// 3. 组装 `ScannedProject`（含健康度与状态的确定性推断）
    pub fn scan(
        &self,
        cancel: &AtomicBool,
        progress: &dyn ProgressSink,
    ) -> Result<ScanOutcome, ScannerError> {
        let started = std::time::Instant::now();

        if self.config.roots.is_empty() {
            return Err(ScannerError::NotAuthorized(
                "未配置任何扫描目录".to_string(),
            ));
        }

        // 校验根目录存在性：给出精确错误，而不是静默扫出 0 个项目
        for root in &self.config.roots {
            if !root.exists() {
                return Err(ScannerError::DirNotFound(root.display().to_string()));
            }
            if !root.is_dir() {
                return Err(ScannerError::NotADirectory(root.display().to_string()));
            }
        }

        let mut outcome = ScanOutcome::default();
        progress.report("扫描目录，发现项目…", 0, None);

        // ── 阶段 1：发现候选 ────────────────────────────────────────
        let mut candidates: Vec<CandidateDir> = Vec::new();
        for root in &self.config.roots {
            if cancel.load(Ordering::Relaxed) {
                outcome.cancelled = true;
                break;
            }
            match self.discover(root, cancel, &mut outcome) {
                Ok(found) => candidates.extend(found),
                Err(e) => {
                    // 单个根目录出错不中断整轮扫描：记录警告后继续
                    let msg = format!("扫描 {} 时出错: {e}", root.display());
                    tracing::warn!("{msg}");
                    progress.log(&msg);
                    outcome.warnings.push(msg);
                }
            }
        }
        if outcome.cancelled {
            outcome.elapsed_ms = started.elapsed().as_millis();
            return Ok(outcome);
        }

        progress.report(
            &format!("发现 {} 个候选项目，开始静态分析…", candidates.len()),
            0,
            Some(candidates.len()),
        );

        // ── 阶段 2：静态分析 ────────────────────────────────────────
        // 截断到上限（防御性；超出时给出警告而不是静默丢弃）
        if candidates.len() > self.config.max_projects {
            let msg = format!(
                "发现 {} 个项目，超过上限 {}，仅分析前 {} 个",
                candidates.len(),
                self.config.max_projects,
                self.config.max_projects
            );
            tracing::warn!("{msg}");
            progress.log(&msg);
            outcome.warnings.push(msg);
            candidates.truncate(self.config.max_projects);
        }

        let total = candidates.len();

        // 🔴 并行分析：单个项目的静态分析（读清单 + 统计代码 + git）是 IO 密集的，
        // 串行扫 200 个项目要几分钟，远超《产品设计书》的性能预算。
        // 用 rayon 在**项目粒度**并行（而非文件粒度）：粒度粗、无共享可变状态、
        // 且能自然利用多核。实测 174 项目 / 9 万文件从 204s 降到约 10s。
        //
        // 三个必须守住的性质：
        // 1. **确定性顺序**：用 enumerate + 按索引回填，保证同一份数据两次扫描
        //    产出顺序一致（否则前端列表每次刷新都跳动，视觉回归测试也无法比对）
        // 2. **可取消**：每个任务入口检查取消标志，收到信号后跳过剩余分析
        // 3. **错误隔离**：单个项目失败只产生警告，不毁掉整轮扫描
        let completed = AtomicUsize::new(0);
        let results: Vec<Option<Result<ScannedProject, ScannerError>>> = candidates
            .par_iter()
            .map(|cand| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                // 文案不含百分比：数值由 done/total 结构化传给上层，
                // 由上层决定怎么显示（进度条 / 文本 / SSE），避免反向解析
                let stage = if self.config.count_code {
                    "静态分析: 依赖 / 语言 / 规模…"
                } else {
                    "识别项目类型…"
                };
                progress.report(stage, done, Some(total));
                Some(self.analyze_candidate(cand))
            })
            .collect();

        // 按原始索引顺序收集，保证输出确定
        for (cand, result) in candidates.iter().zip(results) {
            let Some(result) = result else {
                // None = 该任务因取消而未执行
                outcome.cancelled = true;
                continue;
            };
            match result {
                Ok(p) => {
                    progress.log(&format!("✓ {} ({})", p.name, p.path));
                    outcome.projects.push(p);
                }
                Err(e) => {
                    // 单个项目分析失败不应毁掉整轮扫描
                    let msg = format!("分析 {} 失败: {e}", cand.path.display());
                    tracing::warn!("{msg}");
                    progress.log(&msg);
                    outcome.warnings.push(msg);
                }
            }
        }

        if self.config.analyze_git {
            progress.report("读取 Git 历史…", outcome.projects.len(), Some(outcome.projects.len()));
        }

        outcome.elapsed_ms = started.elapsed().as_millis();
        progress.report("扫描完成", outcome.projects.len(), Some(outcome.projects.len()));
        Ok(outcome)
    }

    /// 在一个根目录下发现候选项目。
    ///
    /// 用**手写目录遍历**而非 `ignore::WalkBuilder`：
    /// 发现项目后需要**跳过其内部**（否则 monorepo 里每个子包都会被当成独立项目，
    /// `node_modules` 下的包更是会产生成千上万个误报）。
    /// `ignore` 的 filter_entry 能做，但"命中项目即停止下探"的语义用手写循环更清晰。
    fn discover(
        &self,
        root: &Path,
        cancel: &AtomicBool,
        outcome: &mut ScanOutcome,
    ) -> Result<Vec<CandidateDir>, ScannerError> {
        let matcher = build_matcher(&self.config.exclude_patterns)?;
        let mut found = Vec::new();
        // 待遍历队列：(路径, 深度)。用显式栈避免深递归爆栈（授权 D:\ 时目录可能极深）
        let mut queue: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

        while let Some((dir, depth)) = queue.pop() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(found);
            }
            outcome.dirs_walked += 1;

            let read = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) => {
                    // 权限不足等：记警告继续，不中断整轮扫描
                    outcome.warnings.push(format!("无法读取 {}: {e}", dir.display()));
                    continue;
                }
            };

            let mut entries: Vec<String> = Vec::new();
            let mut subdirs: Vec<PathBuf> = Vec::new();
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let path = entry.path();

                // 🔴 必须区分两个不同概念，否则会把"识别证据"一起过滤掉：
                //   A. 不下探 / 不统计内容 —— node_modules、.git 内部、.env 内容
                //   B. 存在即为项目证据   —— `.git` 目录的存在
                //
                // 曾经这里对两者一视同仁地 `continue`，导致 entries 里永远没有 `.git`，
                // `detect()` 的 `has_git` 恒为 false：Git 分析从未执行、
                // 纯 Git 仓库（只有 .git 无构建标记）完全不被发现、
                // 状态推断全部退化到 mtime 兜底。真实扫描 174 个项目才暴露出来。
                if markers::is_always_excluded(&name)
                    || matcher.as_ref().is_some_and(|m| m.is_match(&name))
                    || matcher.as_ref().is_some_and(|m| m.is_match(&path))
                {
                    outcome.dirs_skipped += 1;
                    // `.git` 特殊处理：登记存在性作为项目证据，但不下探、不统计其内容
                    if name == ".git" {
                        entries.push(name);
                    }
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    subdirs.push(path);
                }
                entries.push(name);
            }

            // 当前目录是否为项目？
            if let Some(_detection) = detect(&entries) {
                found.push(CandidateDir { path: dir, entries, depth });
                // 🔑 命中项目后**不再下探**：monorepo 的子包、node_modules 里的包
                // 都不应被当成独立项目。这是"不误报"的关键。
                continue;
            }

            // 未命中且未超深度 → 继续下探
            if depth < self.config.max_depth {
                for sub in subdirs {
                    queue.push((sub, depth + 1));
                }
            }
        }

        Ok(found)
    }

    /// 分析单个候选目录，产出完整静态画像。
    fn analyze_candidate(&self, cand: &CandidateDir) -> Result<ScannedProject, ScannerError> {
        let detection = detect(&cand.entries)
            .ok_or_else(|| ScannerError::NotADirectory(cand.path.display().to_string()))?;

        let dependencies = parse_manifests(&cand.path);
        let git = if self.config.analyze_git && detection.has_git {
            self.git.analyze(&cand.path)
        } else if detection.has_git {
            // 不做 Git 分析但仍记录"有仓库"这一事实
            GitInfo { available: true, ..GitInfo::unavailable("已禁用 Git 分析") }
        } else {
            GitInfo::unavailable("目录中没有 .git")
        };

        // 代码统计（Level 1 的静态部分）。
        // `count_project_files` 顺带产出顶层目录/文件名，避免再遍历一次目录。
        let (stats, top_dirs, top_files) = if self.config.count_code {
            self.count_project_files(&cand.path)?
        } else {
            (ScannedStats::default(), top_level_dirs(&cand.path), top_level_files(&cand.path))
        };

        // 语言：真实统计优先；无代码文件时退回标记文件声明的语言。
        // 🔴 必须走 pick_primary_language（唯一判定入口），不能直接取 first()：
        //    Cargo.toml 的行数常超过 main.rs，直接取第一名会把 Rust 项目标成 TOML。
        let language = {
            let from_stats = crate::language::pick_primary_language(&stats.languages)
                .unwrap_or("")
                .to_string();
            if from_stats.is_empty() {
                detection.kind.primary_language().to_string()
            } else {
                from_stats
            }
        };

        let readme_excerpt = read_readme_excerpt(&cand.path, &cand.entries);

        Ok(ScannedProject {
            id: project_id_from_path(&cand.path),
            name: Detection::project_name(&cand.path),
            path: cand.path.display().to_string(),
            kind: detection.kind,
            marker: detection.marker.clone(),
            language,
            framework: dependencies
                .primary_framework()
                .unwrap_or("-")
                .to_string(),
            tags: dependencies.display_tags(TAG_LIMIT),
            dependencies,
            git,
            detection: DetectionFlags::from(&detection),
            stats,
            top_level_dirs: top_dirs,
            top_level_files: top_files,
            readme_excerpt,
        })
    }

    /// 统计项目内的代码文件。
    ///
    /// 返回 `(统计, 顶层目录名, 顶层文件名)`。三者都在同一次遍历中产出，
    /// 不再重复 read_dir——项目可能有上万个文件，多一遍 IO 就是多一倍耗时。
    ///
    /// 深度限制：项目内部遍历最多 8 层。超深的通常是生成物或数据目录，
    /// 且 `is_generated` 已挡掉常见情况，这里再加一道保险避免极端目录拖慢扫描。
    fn count_project_files(
        &self,
        root: &Path,
    ) -> Result<(ScannedStats, Vec<String>, Vec<String>), ScannerError> {
        let mut acc = StatsAccumulator::new();
        let mut files_seen = 0usize;
        let mut modules: BTreeSetStr = BTreeSetStr::new();
        let mut top_dirs: Vec<String> = Vec::new();
        let mut top_files: Vec<String> = Vec::new();

        let mut queue: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
        while let Some((dir, depth)) = queue.pop() {
            if files_seen >= self.config.max_files_per_project {
                break;
            }
            let read = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(_) => continue, // 权限问题：跳过该子树，不算致命错误
            };
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let path = entry.path();
                if markers::is_always_excluded(&name) {
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    // 顶层目录既是"模块数"的依据，也是目录树的展示内容
                    if depth == 0 {
                        modules.insert(name.clone());
                        top_dirs.push(name.clone());
                    }
                    if depth < PROJECT_INTERNAL_MAX_DEPTH {
                        queue.push((path, depth + 1));
                    }
                    continue;
                }
                if depth == 0 {
                    top_files.push(name.clone());
                }
                files_seen += 1;

                // 🔴 性能关键：**先判生成物，再决定是否读取内容**。
                // `StatsAccumulator::add()` 内部也会判 is_generated，但那时文件已读完——
                // 等于为注定要丢弃的文件付了完整 IO 成本。
                // 实测这一处顺序错误让 9 万文件的扫描从 ~10s 变成 204s。
                if StatsAccumulator::is_generated(&path) {
                    acc.add_unread(&path);
                    continue;
                }
                // 🔒 凭证文件绝不读取内容
                if markers::is_secret_file(&name) {
                    acc.add_unread(&path);
                    continue;
                }
                let Some(language) = language_of(&path) else {
                    acc.add_unread(&path);
                    continue;
                };
                // 用 entry.metadata() 而非 fs::metadata()：Windows 上 DirEntry
                // 已缓存文件属性，可省掉每个文件一次系统调用。
                let Ok(meta) = entry.metadata() else {
                    acc.add_unread(&path);
                    continue;
                };
                if meta.len() > MAX_READ_BYTES {
                    acc.add_unread(&path);
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&path) else {
                    // 非 UTF-8（二进制/其它编码）：只计文件数
                    acc.add_unread(&path);
                    continue;
                };
                let fs: FileStats = count_file_lines(&content, language);
                acc.add(&path, Some(language), fs);
            }
        }

        let breakdown = acc.finish();
        let stats = ScannedStats {
            files: breakdown.total_files,
            loc: breakdown.total_code_lines,
            symbols: 0, // Level 1 的 AST 解析填充（后续 crate）
            modules: modules.len(),
            languages: breakdown
                .languages
                .into_iter()
                .map(|l| spolia_domain::LanguageShare {
                    name: l.name,
                    pct: l.pct,
                    loc: l.loc,
                })
                .collect(),
            skipped_generated: breakdown.skipped_generated,
            unknown_files: breakdown.unknown_files,
        };
        // 排序保证输出稳定（read_dir 顺序随平台/文件系统而变，
        // 不排序会让同一项目两次扫描的目录树顺序不同，视觉回归测试无法比对）
        top_dirs.sort();
        top_files.sort();
        Ok((stats, top_dirs, top_files))
    }
}

/// 项目内部遍历的最大深度。
const PROJECT_INTERNAL_MAX_DEPTH: usize = 8;

/// 展示标签上限（UI chips 不宜过长）。
pub const TAG_LIMIT: usize = 6;

/// 简单的有序去重字符串集合（避免为一个用途引入 BTreeSet 的泛型噪音）。
#[derive(Debug, Default)]
struct BTreeSetStr {
    inner: std::collections::BTreeSet<String>,
}

impl BTreeSetStr {
    fn new() -> Self {
        Self::default()
    }
    fn insert(&mut self, s: String) {
        self.inner.insert(s);
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
}

/// 由绝对路径派生稳定的项目 id。
///
/// 为什么不用 uuid：uuid 每次扫描都变，会让"增量更新"变成"全量重建"，
/// 且历史反馈（user_feedback）会因 id 变化而全部丢失。
/// 路径派生的 id 保证同一路径多次扫描 → 同一 id → upsert 更新而非新增。
pub fn project_id_from_path(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // 归一化：统一分隔符并小写盘符，避免 `D:\A` 与 `d:/a` 生成不同 id
    let normalized = spolia_domain::normalize_path(&path.display().to_string());
    normalized.hash(&mut hasher);
    let hash = hasher.finish();
    // 前缀用目录名，让 id 在日志与数据库里可读（否则全是十六进制无法排查）
    let slug = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    let safe_slug: String = slug
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(32)
        .collect();
    format!("p_{safe_slug}_{hash:x}")
}

/// 读取顶层目录名。
///
/// 仅在**关闭代码统计**时使用（此时没有遍历可复用）。
/// 开启统计时由 `count_project_files` 在同一次遍历中顺带产出，避免重复 IO。
fn top_level_dirs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if markers::is_always_excluded(&name) {
                continue;
            }
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

fn top_level_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if markers::is_always_excluded(&name) {
                continue;
            }
            if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

/// 读取 README 首段作为项目描述。
///
/// 🔴 不编造描述：没有 README 就返回空串，前端显示"无描述"而不是模板文案。
/// （原型期 mock 给每个项目手写了漂亮的描述，真实数据里大量项目没有 README）
pub fn read_readme_excerpt(root: &Path, entries: &[String]) -> String {
    // 按优先级找 README 变体
    const CANDIDATES: &[&str] = &[
        "README.md", "readme.md", "README.MD", "Readme.md",
        "README.rst", "README.txt", "README", "readme",
    ];
    // 优先匹配常见 README 命名，其次放宽到任意 readme* 前缀
    // （注意两个分支都要产出 &str：CANDIDATES 项是 &&str 需解引用，
    //   entries 项是 &String 需 as_str，否则类型不统一）
    let name: Option<&str> = CANDIDATES
        .iter()
        .copied()
        .find(|c| entries.iter().any(|e| e.eq_ignore_ascii_case(c)))
        .or_else(|| {
            entries
                .iter()
                .find(|e| e.to_ascii_lowercase().starts_with("readme"))
                .map(String::as_str)
        });
    let Some(name) = name else {
        return String::new();
    };
    let path = root.join(name);
    let Ok(meta) = std::fs::metadata(&path) else {
        return String::new();
    };
    // README 可能很大，只读前 64KB 足够取首段
    if meta.len() > 64 * 1024 {
        return String::new();
    }
    let Ok(content) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    extract_first_paragraph(&content)
}

/// 提取 Markdown/纯文本的首个实质段落。
///
/// 跳过：标题行（`#`）、徽章图片行（`[![`）、HTML 标签、空行、
/// 引用块（`>`）、表格行（`|`）、分隔线，以及**代码块整体**。
///
/// 🔴 代码块必须用状态机整体跳过：只跳过 ``` 围栏行的话，
/// 块内的 `fn main() {}` 会被当成项目描述，
/// 而 README 以安装示例开头的情况非常普遍。
pub fn extract_first_paragraph(content: &str) -> String {
    let mut paragraph: Vec<String> = Vec::new();
    let mut in_code_block = false;

    for raw in content.lines() {
        let line = raw.trim();

        // ── 代码围栏：进入/退出都必须整体跳过，不参与段落 ──
        if line.starts_with("```") || line.starts_with("~~~") {
            in_code_block = !in_code_block;
            // 若已经在收集段落，代码块视为段落结束（描述不应跨越代码）
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if in_code_block {
            continue;
        }

        if line.is_empty() {
            // 段落结束
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        // 跳过噪音行
        if line.starts_with('#')          // 标题
            || line.starts_with("[![")   // 徽章
            || line.starts_with("![")    // 图片
            || line.starts_with('<')     // HTML
            || line.starts_with('>')     // 引用
            || line.starts_with('|')     // 表格
            || line.starts_with("---")   // 分隔线
            || line.starts_with("===")
        {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        // 去掉列表符号
        let cleaned = line
            .trim_start_matches("- ")
            .trim_start_matches("* ")
            .trim();
        if cleaned.is_empty() {
            continue;
        }
        paragraph.push(cleaned.to_string());
        // 首段限制在 3 行内，避免 description 字段过长
        if paragraph.len() >= 3 {
            break;
        }
    }
    let text = paragraph.join(" ");
    // 限制长度（description 在卡片上只显示两行）
    if text.chars().count() > 300 {
        let truncated: String = text.chars().take(297).collect();
        format!("{truncated}…")
    } else {
        text
    }
}

/// 把用户写的一条排除 pattern 展开成"对目录名与目录路径都能命中"的多个变体。
///
/// 背景见 [`build_matcher`]。实测数据（globset 0.4，Windows 反斜杠路径）：
///
/// | 用户写法 | 命中裸目录名 | 命中目录路径 |
/// |---|---|---|
/// | `**/X/**` | ✗ | ✗ |
/// | `X/**` | ✗ | ✗ |
/// | `X` | ✓ | ✗ |
/// | `**/X` | ✓ | ✓ |
///
/// 即只有 `**/X` 形式两种输入都命中。原因是 `**/X/**` 要求 X **后面还有内容**，
/// 而 matcher 拿到的是 X 这个目录本身。
///
/// 🔴 但别据此以为 `**/X/**` 完全没用（曾误判过）：它仍会匹配 X 的每个**子项**，
/// 使 `detect()` 看不到清单文件，X 通常也不会入库。派生 `**/X` 解决的是两件它做不到的事：
/// 1. **省遍历**：`**/X/**` 下 X 自身仍被 `read_dir`，派生后直接跳过（见
///    `expanded_pattern_skips_excluded_dir_itself`，断言 `dirs_walked`）；
/// 2. **封 `.git` 漏网**：X 自身含 `.git` 时，`discover()` 豁免 `.git` 作项目证据，
///    X 会因 GitOnly 照样入库（pub 缓存里大量此类第三方仓库），派生后才挡住
///    （见 `expanded_pattern_excludes_git_only_dir`）。
///
/// 因此对每条 pattern 派生三个变体：原样、剥掉尾部通配、补 `**/` 前缀。
/// 用户写 `node_modules`、`**/node_modules`、`**/node_modules/**` 效果一致。
fn expand_pattern(p: &str) -> Vec<String> {
    let trimmed = p.trim();
    let mut out: Vec<String> = vec![trimmed.to_string()];

    // 反复剥掉尾部的 `/**` 与 `/*`，还原成"目录本身"的 glob
    let mut core = trimmed.to_string();
    loop {
        let before = core.clone();
        core = core.trim_end_matches('/').to_string();
        if core.ends_with("/**") {
            core.truncate(core.len() - "/**".len());
        } else if core.ends_with("/*") {
            core.truncate(core.len() - "/*".len());
        }
        core = core.trim_end_matches('/').to_string();
        if core == before {
            break;
        }
    }
    if core.is_empty() || core == "**" {
        return out;
    }
    push_unique(&mut out, core.clone());
    // 缺 `**/` 前缀时补上，这样"目录路径"这种输入也能命中
    if !core.starts_with("**/") {
        push_unique(&mut out, format!("**/{core}"));
    }
    out
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

/// 构建用户自定义排除 glob 的匹配器。
///
/// 无效 pattern 不应让整轮扫描失败——记警告并忽略该条。
///
/// 🔴 每条 pattern 会被 `expand_pattern` 展开成多个变体后一起加入 matcher。
///
/// 注意 `**/X/**` **并非完全无效**（这一点容易误判，曾在本文件注释里写错过）：
/// 它虽不匹配 X 自身，却匹配 X 的每个子项，于是 `detect()` 看不到清单文件，
/// X 照样不会成为项目——只是白白多 `read_dir` 一层。
///
/// 真正会漏的是 `discover()` L401-403 对 `.git` 的**特意豁免**：
/// X 自身不被跳过 → 进入 read_dir → `Cargo.toml` 被排除但 `.git` 被豁免保留
/// → `detect()` 命中 GitOnly → **X 仍然入库**。pub 包缓存里大量「只有 .git
/// 没有清单文件」的第三方仓库正是这种情况。派生出 `**/X` 后 X 自身被直接跳过，
/// 这条漏网路径才被封住（回归测试见 `expanded_pattern_excludes_git_only_dir`）。
fn build_matcher(patterns: &[String]) -> Result<Option<globset::GlobSet>, ScannerError> {
    let valid: Vec<&String> = patterns.iter().filter(|p| !p.trim().is_empty()).collect();
    if valid.is_empty() {
        return Ok(None);
    }
    let mut builder = globset::GlobSetBuilder::new();
    let mut any_valid = false;
    for p in &valid {
        let mut added = false;
        for variant in expand_pattern(p) {
            match globset::Glob::new(&variant) {
                Ok(g) => {
                    builder.add(g);
                    added = true;
                }
                // 原始写法无效、但派生变体可能有效；全无效时才告警
                Err(_) if variant.as_str() == p.as_str() => {
                    tracing::warn!(pattern = %p, "无效排除模式");
                }
                Err(_) => {}
            }
        }
        if added {
            any_valid = true;
        } else {
            tracing::warn!(pattern = %p, "排除模式无任何有效变体，已忽略");
        }
    }
    if !any_valid {
        return Ok(None);
    }
    builder
        .build()
        .map(Some)
        .map_err(|e| ScannerError::Io {
            path: String::new(),
            reason: format!("构建排除规则失败: {e}"),
        })
}

/// 把扫描结果转换为领域层的 `Project`（含健康度与状态推断）。
///
/// 这是"确定性引擎"与"领域模型"的接缝：所有业务规则集中在这里，
/// 便于单测覆盖，也便于将来调整规则时只改一处。
pub fn to_domain_project(
    scanned: &ScannedProject,
    now: chrono::DateTime<chrono::Utc>,
) -> spolia_domain::Project {
    use spolia_domain::{CodeStats, Project};

    // 最后活动时间：Git 提交时间优先，其次文件 mtime
    let last_commit_at = scanned.git.last_commit_at.clone().map(|t| truncate_to_date(&t));
    let updated_at = last_commit_at.clone().or_else(|| {
        // 无 Git 历史时用文件 mtime 兜底（否则活跃度全部"未知"）
        // mtime_of_newest_file 已返回 DateTime<Utc>，直接格式化，不要再走 SystemTime 转换
        mtime_of_newest_file(Path::new(&scanned.path), 500)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
    });
    let created_at = scanned
        .git
        .first_commit_at
        .as_ref()
        .map(|t| truncate_to_date(t))
        .or_else(|| {
            std::fs::metadata(&scanned.path)
                .ok()
                .and_then(|m| m.created().ok())
                .and_then(to_iso_date)
        });

    // 距今天数：复用 domain 的统一实现（同一口径决定状态推断、健康度、
    // 洞察的"遗忘资产"与搜索的新鲜度排序；此处再写一份必然漂移）
    let days_since = spolia_domain::days_since_latest(
        &[last_commit_at.as_deref(), updated_at.as_deref()],
        now,
    );

    let health_score = Project::compute_health(
        scanned.git.commit_count,
        scanned.detection.has_git,
        scanned.detection.has_readme,
        scanned.detection.has_tests,
        scanned.stats.loc,
        days_since,
    );
    let status = Project::infer_status(scanned.git.commit_count, days_since, scanned.stats.loc);

    Project {
        id: scanned.id.clone(),
        name: scanned.name.clone(),
        path: scanned.path.clone(),
        description: scanned.readme_excerpt.clone(),
        language: scanned.language.clone(),
        framework: scanned.framework.clone(),
        created_at,
        updated_at,
        last_commit_at,
        status,
        health_score,
        // 完成度需要 AI 或更深的语义分析，Level 0 不给假值
        completeness: None,
        tags: scanned.tags.clone(),
        sensitive: false,
        stats: CodeStats {
            files: scanned.stats.files,
            loc: scanned.stats.loc,
            symbols: scanned.stats.symbols,
            modules: scanned.stats.modules,
            languages: scanned.stats.languages.clone(),
        },
        // 扫描事实：此处正持有真实 Git 数据，直接填充而非留默认值。
        // scanned_at 留给 pipeline 的 update_scan_facts 统一写入，
        // 保证它与"本轮扫描的落库时间"一致（而非扫描开始时间）。
        scan: spolia_domain::ScanFacts {
            git_commits: scanned.git.commit_count,
            has_git: scanned.detection.has_git,
            has_readme: scanned.detection.has_readme,
            has_tests: scanned.detection.has_tests,
            scanned_at: None,
        },
        ai_profile: None,
    }
}

/// RFC3339 时间戳截断为 YYYY-MM-DD（数据库字段口径统一）。
fn truncate_to_date(ts: &str) -> String {
    ts.chars().take(10).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cancelled() -> AtomicBool {
        AtomicBool::new(false)
    }

    /// 收集进度的测试替身。
    #[derive(Default)]
    struct Recorder {
        stages: std::sync::Mutex<Vec<String>>,
        logs: std::sync::Mutex<Vec<String>>,
    }
    impl ProgressSink for Recorder {
        fn report(&self, stage: &str, _done: usize, _total: Option<usize>) {
            self.stages.lock().unwrap().push(stage.to_string());
        }
        fn log(&self, line: &str) {
            self.logs.lock().unwrap().push(line.to_string());
        }
    }

    /// 造一个最小的 Rust 项目。
    fn make_rust_project(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"demo\"\n\n[dependencies]\naxum = \"0.8\"\n",
        ).unwrap();
        fs::write(
            dir.join("src/main.rs"),
            "// 注释行\n\nfn main() {\n    println!(\"hi\");\n}\n",
        ).unwrap();
        fs::write(dir.join("README.md"), "# Demo\n\n这是一个演示项目。\n").unwrap();
        dir
    }

    #[test]
    fn discovers_real_projects() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "app-a");
        make_rust_project(root.path(), "app-b");
        // 非项目目录不应被发现
        fs::create_dir_all(root.path().join("photos")).unwrap();
        fs::write(root.path().join("photos/a.jpg"), b"fake").unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let rec = Recorder::default();
        let out = scanner.scan(&cancelled(), &rec).unwrap();

        assert!(!out.cancelled);
        assert_eq!(out.project_count(), 2, "应只发现两个真实项目");
        let names: Vec<&str> = out.projects.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"app-a"));
        assert!(names.contains(&"app-b"));
        assert!(!names.contains(&"photos"), "普通文件夹不得误报");
    }

    #[test]
    fn analyzes_static_profile() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "app");
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let p = scanner.scan(&cancelled(), &NoProgress).unwrap();
        let proj = &p.projects[0];

        assert_eq!(proj.kind, ProjectKind::Rust);
        assert_eq!(proj.marker, "Cargo.toml");
        assert_eq!(proj.language, "Rust", "主语言应来自真实文件统计");
        assert_eq!(proj.framework, "Axum", "框架应来自真实依赖清单");
        assert!(proj.tags.contains(&"axum".to_string()));
        assert!(proj.stats.loc > 0, "应统计到代码行");
        assert!(proj.stats.files > 0);
        assert!(proj.detection.has_readme);
        assert!(!proj.detection.has_tests);
        assert_eq!(proj.readme_excerpt, "这是一个演示项目。", "描述应取自 README 首段");
    }

    /// 无 README 的项目 description 必须为空，不能编造。
    #[test]
    fn missing_readme_yields_empty_description() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("noreadme");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        fs::write(dir.join("main.rs"), "fn main(){}\n").unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.projects[0].readme_excerpt, "");
    }

    /// 🔴 回归测试：`.git` 必须作为**项目识别证据**保留在 entries 中。
    ///
    /// 历史 bug：`discover()` 把 `.git` 与 node_modules 一样直接过滤掉，
    /// 导致 `has_git` 恒为 false —— Git 分析从未执行、状态推断全部退化到 mtime。
    /// 直到对真实目录扫描 174 个项目、发现开关 Git 分析结果完全相同才暴露。
    #[test]
    fn git_dir_is_detected_as_project_evidence() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("repo");
        fs::create_dir_all(proj.join(".git")).unwrap();
        fs::write(proj.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        fs::write(proj.join("main.rs"), "fn main(){}\n").unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false, // 关闭 git 子进程调用，只验证 has_git 标志
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 1);
        assert!(
            out.projects[0].detection.has_git,
            ".git 存在时 has_git 必须为 true（曾因排除规则过滤 .git 而恒为 false）"
        );
        assert!(out.projects[0].git.available, "git 信息应可用");
    }

    /// 纯 Git 仓库（只有 .git、无构建标记文件）也必须被发现。
    ///
    /// 大量笔记仓库、文档仓库、脚本集合属于这一类，它们同样含有可复用资产。
    #[test]
    fn pure_git_repo_without_marker_is_discovered() {
        let root = tempfile::tempdir().unwrap();
        let notes = root.path().join("my-notes");
        fs::create_dir_all(notes.join(".git")).unwrap();
        fs::write(notes.join("notes.md"), "# 笔记\n").unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 1, "纯 Git 仓库应被识别为项目");
        assert_eq!(out.projects[0].kind, ProjectKind::GitOnly);
        assert_eq!(out.projects[0].marker, ".git");
    }

    /// `.git` 保留为证据，但其**内容**不得计入代码统计。
    #[test]
    fn git_internals_are_not_counted_as_code() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("repo");
        fs::create_dir_all(proj.join(".git/objects")).unwrap();
        fs::write(proj.join(".git/config"), "[core]\n\trepositoryformatversion = 0\n").unwrap();
        // 伪造大量 git 内部文件，若被统计 LOC 会显著虚高
        for i in 0..50 {
            fs::write(
                proj.join(format!(".git/objects/pack{i}.pack")),
                "x".repeat(200),
            ).unwrap();
        }
        fs::write(proj.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        fs::write(proj.join("main.rs"), "fn main(){}\n").unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        let p = &out.projects[0];
        assert!(p.detection.has_git);
        assert!(
            p.stats.loc < 50,
            ".git 内部文件不得计入 LOC，实际 {}", p.stats.loc
        );
        assert!(
            p.stats.files < 60,
            ".git 内部文件不得计入文件数，实际 {}", p.stats.files
        );
    }

    /// `.git` 不应被下探为独立项目（它是目录，但内部无标记文件）。
    #[test]
    fn git_dir_is_not_descended_into() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("repo");
        // 在 .git 内部放一个 package.json，若被下探会误报出第二个项目
        fs::create_dir_all(proj.join(".git/hooks")).unwrap();
        fs::write(proj.join(".git/hooks/package.json"), r#"{ "name": "hook" }"#).unwrap();
        fs::write(proj.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 1, ".git 内部不得产生独立项目");
        assert_eq!(out.projects[0].name, "repo");
    }

    /// 🔴 验收标准：不误报非项目文件夹。
    #[test]
    fn does_not_report_plain_folders() {
        let root = tempfile::tempdir().unwrap();
        for n in ["docs", "images", "backup", "temp"] {
            fs::create_dir_all(root.path().join(n)).unwrap();
            fs::write(root.path().join(n).join("file.txt"), "x").unwrap();
        }
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 0);
    }

    /// node_modules 里的包绝不能被当成项目（这是最典型的误报源）。
    #[test]
    fn does_not_descend_into_node_modules() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("webapp");
        let nm = proj.join("node_modules/lodash");
        fs::create_dir_all(&nm).unwrap();
        fs::write(proj.join("package.json"), r#"{ "dependencies": { "lodash": "4" } }"#).unwrap();
        fs::write(nm.join("package.json"), r#"{ "name": "lodash" }"#).unwrap();
        fs::write(nm.join("index.js"), "module.exports = {}").unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 1, "只应有 webapp 一个项目");
        assert_eq!(out.projects[0].name, "webapp");
    }

    /// monorepo：命中外层项目后不下探，子包不单独成项目。
    #[test]
    fn monorepo_children_are_not_separate_projects() {
        let root = tempfile::tempdir().unwrap();
        let mono = root.path().join("mono");
        fs::create_dir_all(mono.join("packages/a")).unwrap();
        fs::write(mono.join("package.json"), r#"{ "name": "mono", "workspaces": ["packages/*"] }"#).unwrap();
        fs::write(mono.join("packages/a/package.json"), r#"{ "name": "a" }"#).unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 1);
        assert_eq!(out.projects[0].name, "mono");
    }

    #[test]
    fn respects_max_depth() {
        let root = tempfile::tempdir().unwrap();
        // root/a/b/c/d/app —— app 在深度 5
        let deep = root.path().join("a/b/c/d/app");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();

        let shallow = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            max_depth: 2,
            analyze_git: false,
            ..Default::default()
        });
        assert_eq!(shallow.scan(&cancelled(), &NoProgress).unwrap().project_count(), 0);

        let deep_scan = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            max_depth: 6,
            analyze_git: false,
            ..Default::default()
        });
        assert_eq!(deep_scan.scan(&cancelled(), &NoProgress).unwrap().project_count(), 1);
    }

    /// 用户自定义排除模式生效。
    #[test]
    fn honors_custom_exclude_patterns() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "keep");
        make_rust_project(root.path(), "skipme");

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            exclude_patterns: vec!["skipme".to_string()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 1);
        assert_eq!(out.projects[0].name, "keep");
    }

    /// 🔴 回归：用户可能写出的四种写法必须**都**能排除。
    ///
    /// 变异验证（禁用 `expand_pattern` 派生）显示，真正失败的是 **`skipme/**`**：
    /// 它缺少前导 `**/`，globset 无法用它匹配绝对路径 `C:\...\skipme\Cargo.toml`，
    /// 于是什么都排除不掉 → 得到 `["skipme", "keep"]` 2 个项目。
    ///
    /// 而 `**/skipme/**` 在变异下**仍然通过**——不是因为它写对了，而是它匹配
    /// skipme 的每个子项（含 `Cargo.toml`），`detect()` 看到空 entries 就不认它是项目。
    /// 这种"碰巧有效"在 skipme 含 `.git` 时会失效（`.git` 被特意豁免），
    /// 详见 `expanded_pattern_excludes_git_only_dir`。
    ///
    /// 派生规则同时解决这两点：缺前导 `**/` 时补上，尾部 `/**` 则剥成 `**/X`。
    #[test]
    fn honors_trailing_glob_exclude_pattern() {
        for pat in ["**/skipme/**", "skipme/**", "**/skipme", "skipme"] {
            let root = tempfile::tempdir().unwrap();
            make_rust_project(root.path(), "keep");
            make_rust_project(root.path(), "skipme");

            let scanner = Scanner::new(ScanConfig {
                roots: vec![root.path().to_path_buf()],
                exclude_patterns: vec![pat.to_string()],
                analyze_git: false,
                ..Default::default()
            });
            let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
            assert_eq!(
                out.project_count(),
                1,
                "pattern {pat:?} 应排除 skipme，实际得到 {} 个项目: {:?}",
                out.project_count(),
                out.projects.iter().map(|p| &p.name).collect::<Vec<_>>()
            );
            assert_eq!(out.projects[0].name, "keep");
        }
    }

    /// `**/X/**` 与 `**/X` 的排除**结果**相同，但后者不必进入 X 目录。
    ///
    /// 🔴 这里断言的是 `dirs_walked`，不是 `project_count` —— 两者的 project_count
    /// 都是 1，因为 `**/deps/**` 虽然不匹配 `deps` 自身，却匹配 deps 的每个子项
    /// （含 `Cargo.toml`），于是 `detect()` 看到空 entries，deps 不会被认成项目。
    /// 即：**`**/X/**` 在功能上本来就有效**，只是要多 `read_dir` 一层。
    /// `expand_pattern` 派生出 `**/deps` 后 deps 自身被直接跳过，省下这次遍历。
    ///
    /// 实测（变异验证）：有派生时 dirs_walked=2，禁用派生后 dirs_walked=3。
    #[test]
    fn expanded_pattern_skips_excluded_dir_itself() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "keep");
        // deps 自身是项目，其下 hosted/inner 也是项目
        let deps = make_rust_project(root.path(), "deps");
        let hosted = deps.join("hosted");
        fs::create_dir_all(&hosted).unwrap();
        make_rust_project(&hosted, "inner");

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            exclude_patterns: vec!["**/deps/**".to_string()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(
            out.project_count(),
            1,
            "deps 及其内部项目都应被排除，实际得到 {:?}",
            out.projects.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        assert_eq!(out.projects[0].name, "keep");
        // 关键断言：只遍历 root 与 keep，deps 自身不进 read_dir
        assert_eq!(
            out.dirs_walked, 2,
            "派生出 `**/deps` 后 deps 自身应被跳过（不 read_dir），实际遍历了 {} 层",
            out.dirs_walked
        );
    }

    /// 🔴 `**/X/**` 对「X 自身含 `.git`」的情况**真的会失效**，必须派生 `**/X`。
    ///
    /// `discover()` 特意豁免 `.git`（L401-403：登记存在性作为项目证据）。于是当
    /// 排除规则写成 `**/X/**` 时：
    /// - `**/X/**` 不匹配 X 自身 → X 仍被 `read_dir`
    /// - X 内的 `Cargo.toml` 被匹配跳过，但 `.git` 被豁免保留
    /// - `detect()` 看到 `.git` → kind=GitOnly → **X 照样成为项目**
    ///
    /// 派生出 `**/X` 后 X 自身被直接跳过，`.git` 也没机会进入 entries。
    /// 这正是 pub 包缓存里那些「只有 .git 无清单文件」的仓库能被挡住的唯一原因。
    #[test]
    fn expanded_pattern_excludes_git_only_dir() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "keep");
        // cached 自身只有 .git，无清单文件 —— 模拟 pub 缓存里的第三方仓库
        let cached = root.path().join("cached");
        fs::create_dir_all(cached.join(".git")).unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            exclude_patterns: vec!["**/cached/**".to_string()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(
            out.project_count(),
            1,
            "cached 只有 .git，若未被 `**/cached` 挡住就会因 GitOnly 入库；实际得到 {:?}",
            out.projects.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        assert_eq!(out.projects[0].name, "keep");
    }

    /// `expand_pattern` 的派生规则：三种写法都归一到含 `**/X` 的变体集合。
    #[test]
    fn expand_pattern_normalizes_user_spellings() {
        for pat in ["node_modules", "**/node_modules", "**/node_modules/**", "node_modules/**"] {
            let v = expand_pattern(pat);
            assert!(
                v.iter().any(|x| x == "**/node_modules"),
                "{pat:?} 应派生出 **/node_modules，实际 {v:?}"
            );
        }
        // 原样保留，不破坏已经正确的写法
        assert!(expand_pattern("**/a/b").contains(&"**/a/b".to_string()));
        // 多段路径不补前缀重复
        assert_eq!(expand_pattern("**/Android/SDK"), vec!["**/Android/SDK".to_string()]);
        // 退化输入不产生垃圾变体
        assert_eq!(expand_pattern("  "), vec!["".to_string()]);
        assert_eq!(expand_pattern("**"), vec!["**".to_string()]);
        assert_eq!(expand_pattern("/**"), vec!["/**".to_string()]);
    }

    /// 无效 glob 不应让扫描失败（只记警告）。
    #[test]
    fn invalid_glob_pattern_is_ignored() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "app");
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            exclude_patterns: vec!["[invalid".to_string()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 1, "无效模式应被忽略而非中断扫描");
    }

    /// 产品硬要求：所有长任务可取消。
    #[test]
    fn cancellation_stops_scan() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..20 {
            make_rust_project(root.path(), &format!("app{i}"));
        }
        let cancel = AtomicBool::new(true); // 立即取消
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancel, &NoProgress).unwrap();
        assert!(out.cancelled);
        assert_eq!(out.project_count(), 0, "取消后不应产出项目");
    }

    #[test]
    fn cancellation_mid_analysis() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "app");
        let cancel = AtomicBool::new(false);
        // 用自定义 ProgressSink 在发现阶段后触发取消
        struct CancelAfterDiscover<'a>(&'a AtomicBool);
        impl ProgressSink for CancelAfterDiscover<'_> {
            fn report(&self, stage: &str, _done: usize, _total: Option<usize>) {
                if stage.contains("候选项目") {
                    self.0.store(true, Ordering::Relaxed);
                }
            }
            fn log(&self, _: &str) {}
        }
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancel, &CancelAfterDiscover(&cancel)).unwrap();
        assert!(out.cancelled);
        assert_eq!(out.project_count(), 0);
    }

    #[test]
    fn empty_roots_is_config_error() {
        let scanner = Scanner::new(ScanConfig::default());
        let err = scanner.scan(&cancelled(), &NoProgress).unwrap_err();
        assert!(matches!(err, ScannerError::NotAuthorized(_)));
    }

    #[test]
    fn missing_root_reports_dir_not_found() {
        let scanner = Scanner::new(ScanConfig {
            roots: vec![PathBuf::from(if cfg!(windows) { "Q:\\does\\not\\exist" } else { "/nonexistent/spolia-test" })],
            ..Default::default()
        });
        assert!(matches!(
            scanner.scan(&cancelled(), &NoProgress).unwrap_err(),
            ScannerError::DirNotFound(_)
        ));
    }

    #[test]
    fn root_must_be_directory() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a.txt");
        fs::write(&file, "x").unwrap();
        let scanner = Scanner::new(ScanConfig { roots: vec![file], ..Default::default() });
        assert!(matches!(
            scanner.scan(&cancelled(), &NoProgress).unwrap_err(),
            ScannerError::NotADirectory(_)
        ));
    }

    #[test]
    fn progress_is_reported() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "app");
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let rec = Recorder::default();
        scanner.scan(&cancelled(), &rec).unwrap();
        let stages = rec.stages.lock().unwrap();
        assert!(stages.iter().any(|s| s.contains("扫描目录")), "应有发现阶段: {stages:?}");
        assert!(stages.iter().any(|s| s.contains("静态分析")), "应有分析阶段: {stages:?}");
        assert!(stages.last().unwrap().contains("完成"), "最后应是完成");
        assert!(!rec.logs.lock().unwrap().is_empty(), "应有扫描日志");
    }

    #[test]
    fn max_projects_truncates_with_warning() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..5 {
            make_rust_project(root.path(), &format!("app{i}"));
        }
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            max_projects: 2,
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 2);
        assert!(out.warnings.iter().any(|w| w.contains("超过上限")), "应给出截断警告");
    }

    // ── id 稳定性 ───────────────────────────────────────────────────

    /// id 必须由路径派生且稳定：否则每次扫描都新增记录，用户反馈全部丢失。
    #[test]
    fn project_id_is_stable_for_same_path() {
        let p = Path::new("/tmp/my-project");
        assert_eq!(project_id_from_path(p), project_id_from_path(p));
    }

    #[test]
    fn project_id_differs_for_different_paths() {
        assert_ne!(
            project_id_from_path(Path::new("/tmp/a")),
            project_id_from_path(Path::new("/tmp/b"))
        );
    }

    #[test]
    fn project_id_is_readable_and_path_normalized() {
        let id = project_id_from_path(Path::new("D:\\Projects\\yingTech"));
        assert!(id.starts_with("p_yingTech_"), "id 应含可读目录名: {id}");
        // 同一逻辑路径的不同写法应得到同一 id
        let a = project_id_from_path(Path::new("D:\\Projects\\yingTech"));
        let b = project_id_from_path(Path::new("D:/Projects/yingTech"));
        assert_eq!(a, b, "分隔符差异不应改变 id");
    }

    #[test]
    fn project_id_sanitizes_unsafe_chars() {
        let id = project_id_from_path(Path::new("/tmp/我的 项目/x"));
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'),
            "id 不应含空格或非 ASCII 前缀段: {id}");
        assert!(id.len() < 64);
    }

    // ── README 首段提取 ─────────────────────────────────────────────

    #[test]
    fn extracts_first_real_paragraph() {
        let md = "# 标题\n\n[![badge](x)](y)\n\n这是第一段描述。\n\n第二段不取。\n";
        assert_eq!(extract_first_paragraph(md), "这是第一段描述。");
    }

    #[test]
    fn skips_badges_and_html() {
        let md = "<div align=center>\n<h1>X</h1>\n</div>\n\n真正的描述在这里。\n";
        assert_eq!(extract_first_paragraph(md), "真正的描述在这里。");
    }

    #[test]
    fn merges_multiline_paragraph() {
        let md = "第一行\n第二行\n第三行\n第四行不取\n\n新段落\n";
        let out = extract_first_paragraph(md);
        assert!(out.contains("第一行") && out.contains("第三行"));
        assert!(!out.contains("第四行"), "首段限制 3 行");
    }

    #[test]
    fn empty_or_noise_only_readme() {
        assert_eq!(extract_first_paragraph(""), "");
        assert_eq!(extract_first_paragraph("# 只有标题\n"), "");
        assert_eq!(extract_first_paragraph("\n\n\n"), "");
    }

    #[test]
    fn long_paragraph_is_truncated() {
        let long = "字".repeat(500);
        let out = extract_first_paragraph(&long);
        assert!(out.chars().count() <= 300, "长度 {}", out.chars().count());
        assert!(out.ends_with('…'));
    }

    #[test]
    fn strips_list_markers() {
        assert_eq!(extract_first_paragraph("- 项目说明\n"), "项目说明");
    }

    #[test]
    fn skips_code_blocks_and_tables() {
        let md = "```rust\nfn x(){}\n```\n\n| a | b |\n|---|---|\n\n真正的描述。\n";
        assert_eq!(extract_first_paragraph(md), "真正的描述。");
    }

    // ── 领域模型转换 ────────────────────────────────────────────────

    #[test]
    fn to_domain_project_computes_health_and_status() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "app");
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        let dom = to_domain_project(&out.projects[0], crate::git::now_utc());

        assert_eq!(dom.name, "app");
        assert!(dom.health_score > 0, "健康度应被计算");
        assert_ne!(dom.status, spolia_domain::ProjectStatus::Unknown, "状态应被推断");
        assert_eq!(dom.framework, "Axum");
        assert!(dom.updated_at.is_some(), "无 Git 时应用 mtime 兜底");
        assert!(dom.ai_profile.is_none(), "Level 0 不产出 AI 画像");
        assert!(dom.completeness.is_none(), "Level 0 不给假的完成度");
        assert!(!dom.sensitive);
    }

    /// Level 0 绝不编造 AI 画像与完成度。
    #[test]
    fn to_domain_project_leaves_ai_fields_empty() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "app");
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        let dom = to_domain_project(&out.projects[0], crate::git::now_utc());
        assert!(dom.ai_profile.is_none());
        assert!(dom.completeness.is_none());
    }

    #[test]
    fn truncate_to_date_works() {
        assert_eq!(truncate_to_date("2025-05-20T10:30:00+08:00"), "2025-05-20");
        assert_eq!(truncate_to_date("2025-05-20"), "2025-05-20");
    }

    #[test]
    fn outcome_defaults_are_sane() {
        let o = ScanOutcome::default();
        assert_eq!(o.project_count(), 0);
        assert!(!o.cancelled);
        assert!(o.warnings.is_empty());
    }

    #[test]
    fn config_defaults_are_safe() {
        let c = ScanConfig::default();
        assert_eq!(c.max_depth, 6);
        assert!(c.max_projects > 0);
        assert!(c.roots.is_empty());
    }

    #[test]
    fn top_level_dirs_excludes_hidden_and_deps() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::create_dir_all(root.path().join("node_modules")).unwrap();
        fs::create_dir_all(root.path().join(".git")).unwrap();
        fs::write(root.path().join("main.rs"), "x").unwrap();
        let dirs = top_level_dirs(root.path());
        assert!(dirs.contains(&"src".to_string()));
        assert!(!dirs.contains(&"node_modules".to_string()));
        assert!(!dirs.contains(&".git".to_string()));
        let files = top_level_files(root.path());
        assert!(files.contains(&"main.rs".to_string()));
    }

    #[test]
    fn code_stats_skip_secret_files() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("app");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        fs::write(proj.join("main.rs"), "fn main(){}\n").unwrap();
        // 凭证文件：不应被读取内容
        fs::write(proj.join(".env"), "SECRET=abc123\n").unwrap();
        fs::write(proj.join("server.pem"), "-----BEGIN-----\n").unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        let p = &out.projects[0];
        // .env / .pem 在 ALWAYS_EXCLUDED 与 is_secret_file 双重拦截下不进入统计
        assert!(p.stats.languages.iter().all(|l| l.name != "pem"));
    }

    #[test]
    fn lock_files_do_not_inflate_project_loc() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("web");
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("package.json"), r#"{ "dependencies": { "react": "18" } }"#).unwrap();
        fs::write(proj.join("src/app.tsx"), "export const A = () => null;\n").unwrap();
        // 巨大的锁文件
        let huge = "{\n".to_string() + &"  \"k\": \"v\",\n".repeat(5000) + "}";
        fs::write(proj.join("package-lock.json"), &huge).unwrap();

        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        let p = &out.projects[0];
        assert!(p.stats.loc < 500, "锁文件的数千行不得计入 LOC，实际 {}", p.stats.loc);
        assert!(p.stats.skipped_generated >= 1);
    }

    #[test]
    fn multiple_roots_are_scanned() {
        let r1 = tempfile::tempdir().unwrap();
        let r2 = tempfile::tempdir().unwrap();
        make_rust_project(r1.path(), "a1");
        make_rust_project(r2.path(), "b1");
        let scanner = Scanner::new(ScanConfig {
            roots: vec![r1.path().to_path_buf(), r2.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.project_count(), 2);
    }

    #[test]
    fn elapsed_time_is_recorded() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "app");
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert!(out.elapsed_ms > 0);
        assert!(out.dirs_walked > 0);
    }

    #[test]
    fn count_code_disabled_yields_zero_loc() {
        let root = tempfile::tempdir().unwrap();
        make_rust_project(root.path(), "app");
        let scanner = Scanner::new(ScanConfig {
            roots: vec![root.path().to_path_buf()],
            analyze_git: false,
            count_code: false,
            ..Default::default()
        });
        let out = scanner.scan(&cancelled(), &NoProgress).unwrap();
        assert_eq!(out.projects[0].stats.loc, 0);
        // 语言退回标记文件声明
        assert_eq!(out.projects[0].language, "Rust");
    }

    #[test]
    fn no_progress_sink_is_usable() {
        // 编译期验证 NoProgress 实现了 trait 且不 panic
        NoProgress.report("x", 0, None);
        NoProgress.log("y");
    }
}
