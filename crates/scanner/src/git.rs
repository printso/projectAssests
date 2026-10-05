//! Git 历史分析（Level 0，**零 LLM 成本**）。
//!
//! 《技术设计书》§3：Git 用 `git CLI / git2`。这里选 **CLI**，理由：
//! - `git2`(libgit2) 需要额外编译 C 库，增加开源贡献者的构建门槛
//! - 本项目只需 4 个只读命令（log/rev-list/first-commit），CLI 足够
//! - CLI 与用户机器上的 git 行为一致（包括 `core.autocrlf`、hooks 等配置）
//!
//! 🔴 降级纪律：Git 不可用**不是错误**。
//! 大量有价值的项目没有 Git 历史（压缩包解压、AI 生成的 MVP）。
//! 此时 `GitInfo::unavailable()`，扫描继续，只是不产出 commit 统计与考古报告。

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

// 已知限制：Windows 的 std::process 不支持原生命令超时，因此本模块的 git 调用
// 没有硬超时。风险缓解措施：
// - 全部命令都是本地只读操作（log / rev-list / status），不触网络
// - GIT_TERMINAL_PROMPT=0 禁用凭证交互提示，避免永久挂起
// - `status --porcelain` 已限定在当前仓库范围
// 若后续需要真正的超时，应改为「线程 + 通道 + 超时」或用 tokio::process 包装。
// 《技术设计书》§25 已登记"文件监听在 Windows/网络盘上的稳定性"为低风险项。

/// Git 分析结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitInfo {
    /// 是否为 Git 仓库
    pub available: bool,
    /// 提交总数
    pub commit_count: u32,
    /// 首次提交时间（ISO 日期）
    pub first_commit_at: Option<String>,
    /// 最后提交时间（ISO 日期）
    pub last_commit_at: Option<String>,
    /// 近 90 天的提交数（活跃度的直接证据）
    pub recent_commits: u32,
    /// 贡献者数
    pub contributor_count: u32,
    /// 当前分支名
    pub branch: Option<String>,
    /// 是否有未提交改动（"进行中"的信号）
    pub dirty: bool,
    /// 最近一次提交标题（项目考古用）
    pub last_commit_subject: Option<String>,
    /// 降级原因（Git 不可用时说明为什么，便于用户自查）
    pub unavailable_reason: Option<String>,
}

impl GitInfo {
    /// Git 不可用时的降级值。
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            commit_count: 0,
            first_commit_at: None,
            last_commit_at: None,
            recent_commits: 0,
            contributor_count: 0,
            branch: None,
            dirty: false,
            last_commit_subject: None,
            unavailable_reason: Some(reason.into()),
        }
    }

    /// 空仓库（有 .git 但零提交）。
    pub fn empty_repo() -> Self {
        Self {
            available: true,
            commit_count: 0,
            first_commit_at: None,
            last_commit_at: None,
            recent_commits: 0,
            contributor_count: 0,
            branch: None,
            dirty: false,
            last_commit_subject: None,
            unavailable_reason: None,
        }
    }

    /// 距今最后一次提交的天数。无提交历史时返回 `None`
    /// （调用方据此走"无 Git 历史"分支，而不是当成 0 天前）。
    pub fn days_since_last_commit(&self, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
        let ts = self.last_commit_at.as_deref()?;
        let dt = chrono::DateTime::parse_from_rfc3339(ts)
            .map(|d| d.with_timezone(&chrono::Utc))
            .ok()?;
        Some((now - dt).num_days().max(0))
    }
}

impl Default for GitInfo {
    fn default() -> Self {
        Self::unavailable("未检测")
    }
}

/// Git 分析器。持有 git 可执行文件路径，便于测试时注入。
#[derive(Debug, Clone)]
pub struct GitAnalyzer {
    git_bin: String,
    /// 是否检测未提交改动。
    ///
    /// 关闭可把每仓库的 git 进程调用从 2 次降到 1 次（Windows 上每次约 1.3 秒）。
    /// 对"历史项目盘点"这一主场景，dirty 信息价值有限，默认关闭；
    /// 项目详情页需要提示"有未提交改动"时再单独开启。
    check_dirty: bool,
}

impl Default for GitAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl GitAnalyzer {
    pub fn new() -> Self {
        Self { git_bin: "git".into(), check_dirty: false }
    }

    /// 用指定可执行文件（测试或自定义 git 路径）。
    pub fn with_binary(bin: impl Into<String>) -> Self {
        Self { git_bin: bin.into(), check_dirty: false }
    }

    /// 开启未提交改动检测（多一次 git 进程调用）。
    pub fn with_dirty_check(mut self) -> Self {
        self.check_dirty = true;
        self
    }

    /// 是否检测未提交改动。
    pub fn dirty_check_enabled(&self) -> bool {
        self.check_dirty
    }

    /// git 是否可用（一次性探测，结果应缓存）。
    pub fn is_available(&self) -> bool {
        run_git(&self.git_bin, Path::new("."), &["--version"])
            .map(|_| ())
            .is_ok()
    }

    /// 目录是否为 Git 仓库。
    pub fn is_repo(&self, dir: &Path) -> bool {
        dir.join(".git").exists()
    }

    /// 分析一个仓库。任何失败都降级为 `unavailable`，**不向上抛错**。
    ///
    /// # 🔴 性能纪律：git 调用次数必须最少化
    /// Windows 上 **git 进程启动本身就要 ~1.3 秒**（`git --version` 实测 0.73s，
    /// 与仓库大小无关）。曾经本函数发起 8 次调用，48 个真实仓库就是 384 次
    /// 进程创建 —— 实测整轮扫描从 9 秒恶化到 126 秒。
    ///
    /// 现在压缩到 **2 次**：
    /// 1. `git log --format=…` 一次拿齐：提交数、首末提交时间、贡献者、近 90 天提交、最后提交标题
    /// 2. `git status --porcelain --branch` 一次拿齐：分支名 + 是否有未提交改动
    ///
    /// 新增字段时必须优先合并进这两个调用，**不要**再开新的子进程。
    pub fn analyze(&self, dir: &Path) -> GitInfo {
        if !self.is_repo(dir) {
            return GitInfo::unavailable("目录中没有 .git");
        }

        // ── 调用 1：一次 log 取全部历史统计 ──────────────────────────
        // %x1f 是单元分隔符（US），不会出现在提交信息里，比逗号/制表符更安全。
        // --max-count 上限保护：百万提交的仓库不需要全量拉取。
        // 注意 `git log` 在空仓库上会以非零退出，据此区分"空仓库"与"git 故障"。
        const LOG_LIMIT: usize = 20_000;
        let log_out = run_git(
            &self.git_bin,
            dir,
            &[
                "log",
                &format!("--max-count={LOG_LIMIT}"),
                "--format=%cI%x1f%aN%x1f%s",
            ],
        );

        let raw_log = match log_out {
            Ok(s) if !s.trim().is_empty() => s,
            Ok(_) => return GitInfo::empty_repo(), // 有 .git 但零提交
            Err(_) => {
                // log 失败可能是空仓库，也可能是真故障；用 rev-parse 精确区分。
                // 这是唯一保留的额外调用，且只在异常路径上发生。
                return if self.has_any_commit(dir) {
                    GitInfo::unavailable("git log 执行失败")
                } else {
                    GitInfo::empty_repo()
                };
            }
        };

        let mut commit_count: u32 = 0;
        let mut recent_commits: u32 = 0;
        let mut authors: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut last_commit_at: Option<String> = None;
        let mut last_commit_subject: Option<String> = None;
        let mut first_commit_at: Option<String> = None;

        // git log 默认按时间倒序：第一行 = 最新提交，最后一行 = 最早提交
        let cutoff = now_utc() - chrono::Duration::days(90);
        for line in raw_log.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            commit_count += 1;
            let mut fields = line.split('\u{1f}');
            let date_raw = fields.next().unwrap_or("");
            let author = fields.next().unwrap_or("");
            let subject = fields.next().unwrap_or("");

            if !author.is_empty() {
                authors.insert(author.to_string());
            }
            // 首行：最后提交时间与标题
            if last_commit_at.is_none() {
                last_commit_at = Some(date_raw.to_string());
                last_commit_subject = Some(subject.to_string());
            }
            // 近 90 天提交数：解析失败则不计入（保守，不虚报活跃度）
            if let Some(dt) = parse_commit_date(date_raw)
                && dt >= cutoff
            {
                recent_commits += 1;
            }
            // 末行：最早提交时间（每行都覆盖，循环结束即最早）
            first_commit_at = Some(date_raw.to_string());
        }

        // 超过 LOG_LIMIT 时，"最早提交"其实是被截断的那条，不是真实首次提交。
        // 诚实处理：置为 None，让上层显示"未知"而不是给一个错误的日期。
        if commit_count as usize >= LOG_LIMIT {
            first_commit_at = None;
            tracing::debug!(
                path = %dir.display(),
                commits = commit_count,
                "提交数超过 log 上限，首次提交时间标记为未知"
            );
        }

        // ── 分支名：直接读 `.git/HEAD` 文件，零进程开销 ──────────────
        // 🔑 为什么不用 `git rev-parse --abbrev-ref HEAD`：
        // Windows 上起一个 git 进程要 ~1.3 秒，而 .git/HEAD 只是个几十字节的文本文件，
        // 读它是微秒级。48 个仓库就能省下一分钟。
        let branch = read_branch_from_head(dir);

        // 脏标记（未提交改动）：需要 git status，是本次分析的第 2 次也是最后一次进程调用。
        // 仅当调用方需要时才做（对"历史项目盘点"这个主场景，dirty 价值不高，
        // 但它决定项目详情页能否提示"有未提交改动"，故保留但可关闭）。
        //
        // 🔴 `--no-optional-locks` 是 git 的**全局选项**，必须放在子命令之前。
        // 曾误写成 `status --porcelain --no-optional-locks`，git 报 unknown option，
        // 于是 dirty 静默回退成 false（错误被吞掉，很难发现）。
        // 该选项的作用：status 默认会刷新 index 并可能加写锁，
        // 扫描是批量的，不能干扰用户正在进行的 git 操作。
        let dirty = if self.check_dirty {
            match run_git(
                &self.git_bin,
                dir,
                &["--no-optional-locks", "status", "--porcelain"],
            ) {
                Ok(s) => s.lines().any(|l| !l.trim().is_empty()),
                Err(e) => {
                    // 不静默吞掉：曾因把全局选项写错位置导致 git 报错，
                    // 而这里直接返回 false，使 bug 潜伏到真实数据验证才暴露。
                    tracing::debug!(path = %dir.display(), error = %e, "git status 失败，dirty 按 false 处理");
                    false
                }
            }
        } else {
            false
        };

        GitInfo {
            available: true,
            commit_count,
            first_commit_at,
            last_commit_at,
            recent_commits,
            contributor_count: authors.len() as u32,
            branch,
            dirty,
            last_commit_subject,
            unavailable_reason: None,
        }
    }

    /// 是否至少有一个提交（区分空仓库与 git 故障）。
    fn has_any_commit(&self, dir: &Path) -> bool {
        run_git(&self.git_bin, dir, &["rev-parse", "--verify", "HEAD"]).is_ok()
    }

    /// 按时间倒序的提交主题（项目考古"最后停在哪"的输入）。
    ///
    /// 限制条数：一个项目可能有上万次提交，全量拉取既慢又没必要。
    pub fn recent_commit_subjects(&self, dir: &Path, limit: usize) -> Vec<String> {
        let limit = limit.clamp(1, 200);
        run_git(
            &self.git_bin,
            dir,
            &["log", &format!("--max-count={limit}"), "--format=%s"],
        )
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
    }
}

/// 执行 git 命令并返回 stdout。
///
/// 刻意用同步 `Command`：扫描是 IO 密集型的批处理，
/// 上层（`spolia-jobs`）已经用 tokio 的 `spawn_blocking` 隔离，
/// 在这里再引入 async 只会增加复杂度而无收益。
fn run_git(bin: &str, dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(dir)
        // 关键：禁用交互式提示。git 在某些配置下会弹凭证输入，
        // 那会让批处理扫描永久挂住。
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        // 统一输出编码与语言，避免中文 locale 下解析 "分支" 之类的本地化文本
        .env("LC_ALL", "C")
        .env("LANG", "C")
        // 禁止分页器（git log 在非 tty 下默认不分页，但显式设置更稳妥）
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|e| format!("无法执行 git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {args:?} 失败: {}", stderr.trim()));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("git 输出非 UTF-8: {e}"))
}

/// 取输出的第一个非空行（git 常带尾部换行）。
fn first_non_empty_line(s: String) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// 解析 `%cI` 输出的提交时间。
///
/// `git log --format=%cI` 给出严格 ISO 8601（含时区偏移），如
/// `2024-04-01T13:44:32-07:00`。解析失败返回 `None`，调用方按"不计入"处理——
/// 宁可少算一个近期提交，也不能因为解析错误虚报活跃度。
fn parse_commit_date(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(t)
        .map(|d| d.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            // 兜底：某些 git 配置下 %cI 可能退化为纯日期
            chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|dt| dt.and_utc())
        })
}

/// 直接读取 `.git/HEAD` 得到当前分支名，**不起 git 进程**。
///
/// 🔑 这是本模块最重要的性能优化之一：Windows 上启动一个 git 进程约 1.3 秒，
/// 而 `.git/HEAD` 只是个几十字节的文本文件，读取是微秒级。
/// 48 个真实仓库仅"取分支名"这一项就能省下约一分钟。
///
/// HEAD 文件格式：
/// - 正常状态：`ref: refs/heads/main`  → 分支名 `main`
/// - 游离 HEAD：`<40 位 commit sha>`   → 返回 `None`（对用户无意义）
///
/// 读取失败（无权限、worktree 的 `.git` 是文件而非目录等）一律返回 `None`，
/// 绝不 panic——分支名只是展示信息，拿不到不影响任何判定。
pub fn read_branch_from_head(dir: &Path) -> Option<String> {
    let head_path = dir.join(".git").join("HEAD");
    let content = std::fs::read_to_string(&head_path).ok()?;
    let line = content.lines().next()?.trim();

    // `ref: refs/heads/<branch>` —— 分支名可能含斜杠（feature/login-ui），
    // 因此只剥掉固定前缀，不能按 '/' 取最后一段
    let branch = line.strip_prefix("ref: refs/heads/")?.trim();
    if branch.is_empty() {
        return None;
    }
    Some(branch.to_string())
}

/// 从目录的文件修改时间推断"最后更新"（无 Git 历史时的兜底）。
///
/// 这是**确定性**兜底：没有 Git 的项目也要能判断活跃度，
/// 否则全部显示"未知"，首页统计失去意义。
pub fn mtime_of_newest_file(dir: &Path, max_files: usize) -> Option<chrono::DateTime<chrono::Utc>> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut seen = 0;
    // 用 walkdir 但不深入依赖目录，避免在 node_modules 里空转
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !spolia_domain_scanner_excluded(&name)
        })
    {
        if seen >= max_files {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        seen += 1;
        if let Ok(meta) = entry.metadata()
            && let Ok(mtime) = meta.modified()
        {
            newest = Some(newest.map_or(mtime, |n| n.max(mtime)));
        }
    }
    newest
        .and_then(|t| chrono::DateTime::<chrono::Utc>::from_timestamp(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64, 0))
}

/// 排除判定（复用 markers 模块的规则，避免两处维护同一份清单）。
fn spolia_domain_scanner_excluded(name: &str) -> bool {
    crate::markers::is_always_excluded(name)
}

/// 把 SystemTime 转为 ISO 日期字符串（YYYY-MM-DD）。
pub fn to_iso_date(t: std::time::SystemTime) -> Option<String> {
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(
        t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64,
        0,
    )?;
    Some(dt.format("%Y-%m-%d").to_string())
}

/// 当前 UTC 时间。
pub fn now_utc() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

/// 探测到的 git 可执行文件版本（诊断信息，展示在设置页）。
pub fn git_version(analyzer: &GitAnalyzer) -> Option<String> {
    run_git(&analyzer.git_bin, Path::new("."), &["--version"])
        .ok()
        .and_then(first_non_empty_line)
}

/// 供上层判断"是否需要提示用户安装 Git"。
pub fn suggest_git_install(info: &GitInfo) -> Option<String> {
    if info.available {
        return None;
    }
    let reason = info.unavailable_reason.as_deref()?;
    if reason.contains("无法执行 git") {
        Some("安装 Git 后可获得提交历史、活跃度与项目考古能力。".to_string())
    } else {
        None // 只是该项目没有 .git，不需要全局提示
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── 降级行为（不依赖真实 git，纯逻辑测试）────────────────────────

    #[test]
    fn unavailable_carries_reason() {
        let g = GitInfo::unavailable("目录中没有 .git");
        assert!(!g.available);
        assert_eq!(g.commit_count, 0);
        assert!(g.last_commit_at.is_none());
        assert_eq!(g.unavailable_reason.as_deref(), Some("目录中没有 .git"));
    }

    #[test]
    fn empty_repo_is_available_but_has_no_commits() {
        let g = GitInfo::empty_repo();
        assert!(g.available);
        assert_eq!(g.commit_count, 0);
        assert!(g.unavailable_reason.is_none());
    }

    #[test]
    fn default_is_unavailable() {
        assert!(!GitInfo::default().available);
    }

    /// 无提交历史时必须返回 None，不能当成"0 天前"（那会显示成"活跃中"）。
    #[test]
    fn days_since_last_commit_none_without_history() {
        let g = GitInfo::unavailable("no git");
        assert_eq!(g.days_since_last_commit(now_utc()), None);
        assert_eq!(GitInfo::empty_repo().days_since_last_commit(now_utc()), None);
    }

    #[test]
    fn days_since_last_commit_computes() {
        let mut g = GitInfo::empty_repo();
        let three_days_ago = (now_utc() - chrono::Duration::days(3)).to_rfc3339();
        g.last_commit_at = Some(three_days_ago);
        assert_eq!(g.days_since_last_commit(now_utc()), Some(3));
    }

    #[test]
    fn days_since_never_negative() {
        let mut g = GitInfo::empty_repo();
        g.last_commit_at = Some((now_utc() + chrono::Duration::days(5)).to_rfc3339());
        assert_eq!(g.days_since_last_commit(now_utc()), Some(0));
    }

    #[test]
    fn days_since_invalid_timestamp_is_none() {
        let mut g = GitInfo::empty_repo();
        g.last_commit_at = Some("not-a-date".into());
        assert_eq!(g.days_since_last_commit(now_utc()), None);
    }

    // ── 输出解析 ────────────────────────────────────────────────────

    #[test]
    fn first_non_empty_line_skips_blanks() {
        assert_eq!(first_non_empty_line("\n\n  abc \n".into()).as_deref(), Some("abc"));
        assert_eq!(first_non_empty_line("\n\n".into()), None);
        assert_eq!(first_non_empty_line("".into()), None);
    }

    // ── 直接读 .git/HEAD 取分支（零进程开销）──────────────────────
    //
    // 这些测试刻意**不依赖真实 git**：只造一个含 .git/HEAD 的临时目录，
    // 因此能覆盖各种 HEAD 形态（游离/损坏/缺失），而真实仓库很难造出全部情况。

    /// 写一个假的 .git/HEAD 文件用于测试。
    fn fake_head(dir: &Path, content: &str) {
        let git = dir.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), content).unwrap();
    }

    #[test]
    fn reads_branch_from_head_file() {
        let dir = tempfile::tempdir().unwrap();
        fake_head(dir.path(), "ref: refs/heads/main\n");
        assert_eq!(read_branch_from_head(dir.path()).as_deref(), Some("main"));
    }

    /// 分支名含斜杠时必须完整保留：只剥固定前缀，不能按 '/' 取末段，
    /// 否则 `feature/login-ui` 会被截成 `ui`。
    #[test]
    fn branch_with_slashes_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        fake_head(dir.path(), "ref: refs/heads/feature/login-ui\n");
        assert_eq!(
            read_branch_from_head(dir.path()).as_deref(),
            Some("feature/login-ui")
        );
    }

    #[test]
    fn branch_with_dots_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        fake_head(dir.path(), "ref: refs/heads/release-1.2.3\n");
        assert_eq!(read_branch_from_head(dir.path()).as_deref(), Some("release-1.2.3"));
    }

    /// 游离 HEAD 存的是 40 位 commit sha，不是分支名，应返回 None。
    #[test]
    fn detached_head_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        fake_head(dir.path(), "32237147ab3529be6d182c885edcc8d753a176d9\n");
        assert_eq!(read_branch_from_head(dir.path()), None);
    }

    #[test]
    fn missing_head_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        // 没有 .git/HEAD
        assert_eq!(read_branch_from_head(dir.path()), None);
        // 有 .git 目录但没 HEAD 文件
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        assert_eq!(read_branch_from_head(dir.path()), None);
    }

    #[test]
    fn empty_head_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        fake_head(dir.path(), "");
        assert_eq!(read_branch_from_head(dir.path()), None);
        fake_head(dir.path(), "   \n");
        assert_eq!(read_branch_from_head(dir.path()), None);
    }

    /// ref 指向非 heads（如 detached 到 tag）时不应被当成分支。
    #[test]
    fn non_heads_ref_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        fake_head(dir.path(), "ref: refs/tags/v1.0\n");
        assert_eq!(read_branch_from_head(dir.path()), None);
    }

    /// 内容无换行也应能解析（防御非标准写入）。
    #[test]
    fn head_without_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        fake_head(dir.path(), "ref: refs/heads/dev");
        assert_eq!(read_branch_from_head(dir.path()).as_deref(), Some("dev"));
    }

    /// `dirty` 检测默认关闭（省一次 git 进程），开启时才检测。
    #[test]
    fn dirty_check_is_opt_in() {
        assert!(!GitAnalyzer::new().dirty_check_enabled(), "默认应关闭以节省进程开销");
        assert!(GitAnalyzer::new().with_dirty_check().dirty_check_enabled());
    }

    #[test]
    fn dirty_detection_works_when_enabled() {
        let analyzer = GitAnalyzer::new();
        if !analyzer.is_available() {
            eprintln!("跳过：环境无 git");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .env("LC_ALL", "C")
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.t"]);
        git(&["config", "user.name", "Tester"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "init"]);

        // 关闭 dirty 检测：即使有改动也不报告（且省一次进程）
        let off = GitAnalyzer::new().analyze(root);
        assert!(!off.dirty, "未开启检测时 dirty 恒为 false");
        assert_eq!(off.branch.as_deref(), git_branch_of(root).as_deref(), "分支名应一致");

        // 开启后应能检测到未提交改动
        std::fs::write(root.join("b.txt"), "uncommitted").unwrap();
        let on = GitAnalyzer::new().with_dirty_check().analyze(root);
        assert!(on.dirty, "开启检测后应发现未跟踪文件");
    }

    /// 测试辅助：用真实 git 取分支名，验证 read_branch_from_head 的结果正确。
    fn git_branch_of(dir: &Path) -> Option<String> {
        std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(dir)
            .env("LC_ALL", "C")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.lines().next().map(str::trim).filter(|b| *b != "HEAD").map(str::to_string))
    }

    /// 真实仓库中 read_branch_from_head 必须与 git 自报的分支一致。
    #[test]
    fn head_read_matches_real_git_branch() {
        let analyzer = GitAnalyzer::new();
        if !analyzer.is_available() {
            eprintln!("跳过：环境无 git");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .env("LC_ALL", "C")
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "custom-branch"]);
        git(&["config", "user.email", "t@t.t"]);
        git(&["config", "user.name", "Tester"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("a.txt"), "x").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);

        let from_file = read_branch_from_head(root);
        let from_git = git_branch_of(root);
        assert_eq!(from_file.as_deref(), Some("custom-branch"));
        assert_eq!(from_file, from_git, "读文件结果必须与 git 自报一致");
    }

    // ── 提交时间解析 ────────────────────────────────────────────────

    #[test]
    fn parses_rfc3339_commit_dates() {
        let d = parse_commit_date("2024-04-01T13:44:32-07:00");
        assert!(d.is_some());
        // 带正偏移的时区
        assert!(parse_commit_date("2025-05-20T10:30:00+08:00").is_some());
        // UTC Z 结尾
        assert!(parse_commit_date("2025-05-20T02:30:00Z").is_some());
    }

    #[test]
    fn commit_date_falls_back_to_date_only() {
        assert!(parse_commit_date("2025-05-20").is_some());
    }

    /// 解析失败返回 None（调用方按"不计入"处理），绝不 panic。
    #[test]
    fn invalid_commit_date_returns_none() {
        assert_eq!(parse_commit_date(""), None);
        assert_eq!(parse_commit_date("   "), None);
        assert_eq!(parse_commit_date("not-a-date"), None);
        assert_eq!(parse_commit_date("2024-13-45T99:99:99Z"), None);
    }

    /// 时区偏移必须被正确归一到 UTC，否则"近 90 天"判断会出错。
    #[test]
    fn commit_date_normalizes_timezone() {
        let plus8 = parse_commit_date("2025-05-20T10:30:00+08:00").unwrap();
        let utc = parse_commit_date("2025-05-20T02:30:00Z").unwrap();
        assert_eq!(plus8, utc, "+08:00 的 10:30 等于 UTC 02:30");
    }

    // ── Git 安装建议 ────────────────────────────────────────────────

    /// 只有"git 命令本身不可用"才提示安装；单个项目没 .git 不该骚扰用户。
    #[test]
    fn suggests_install_only_when_git_missing() {
        let no_git = GitInfo::unavailable("无法执行 git: 系统找不到指定的文件");
        assert!(suggest_git_install(&no_git).is_some());

        let no_repo = GitInfo::unavailable("目录中没有 .git");
        assert_eq!(suggest_git_install(&no_repo), None);

        let ok = GitInfo::empty_repo();
        assert_eq!(suggest_git_install(&ok), None);
    }

    // ── 时间转换 ────────────────────────────────────────────────────

    #[test]
    fn to_iso_date_formats_correctly() {
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(to_iso_date(t).as_deref(), Some("2023-11-14"));
    }

    // ── 真实 Git 集成测试（依赖环境有 git，否则自动跳过）──────────────

    /// 在临时目录里造一个真实仓库，验证解析正确。
    /// 若环境无 git，测试自行跳过（返回而非失败）——CI 与本地环境差异不应导致红。
    fn with_real_repo(f: impl FnOnce(&Path)) {
        let analyzer = GitAnalyzer::new();
        if !analyzer.is_available() {
            eprintln!("跳过真实 git 测试：环境中没有 git");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(root)
                .env("LC_ALL", "C")
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.t"]);
        git(&["config", "user.name", "Tester"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "初始提交"]);
        std::fs::write(root.join("b.txt"), "world").unwrap();
        git(&["add", "b.txt"]);
        git(&["commit", "-q", "-m", "second commit"]);
        f(root);
    }

    #[test]
    fn real_repo_commit_count_and_dates() {
        with_real_repo(|root| {
            let info = GitAnalyzer::new().analyze(root);
            assert!(info.available);
            assert_eq!(info.commit_count, 2);
            assert!(info.first_commit_at.is_some());
            assert!(info.last_commit_at.is_some());
            assert_eq!(info.contributor_count, 1);
            assert_eq!(info.last_commit_subject.as_deref(), Some("second commit"));
            assert!(info.branch.is_some(), "应有分支名");
            assert_eq!(info.days_since_last_commit(now_utc()), Some(0));
            // 刚提交的仓库，近 90 天提交数应等于总提交数。
            // 这是新逻辑：不再用 `git rev-list --since`（多一次进程），
            // 而是解析 log 输出的日期自行统计，故必须单独验证。
            assert_eq!(info.recent_commits, 2, "两条提交都在 90 天内");
        });
    }

    /// 首次提交时间必须取**最早**那条（log 是倒序，末行才是最早）。
    #[test]
    fn real_repo_first_commit_is_earliest() {
        with_real_repo(|root| {
            let info = GitAnalyzer::new().analyze(root);
            let first = info.first_commit_at.clone().unwrap();
            let last = info.last_commit_at.clone().unwrap();
            // 两次提交都在测试运行的同一时刻附近，但至少 first <= last 必须成立
            assert!(
                parse_commit_date(&first).unwrap() <= parse_commit_date(&last).unwrap(),
                "首次提交时间({first})不应晚于最后提交({last})"
            );
            assert_ne!(info.last_commit_subject.as_deref(), Some("初始提交"),
                "最后提交标题应是较新的那条");
        });
    }

    #[test]
    fn real_repo_recent_commit_subjects() {
        with_real_repo(|root| {
            let subjects = GitAnalyzer::new().recent_commit_subjects(root, 10);
            assert_eq!(subjects.len(), 2);
            assert_eq!(subjects[0], "second commit", "应按时间倒序");
            assert_eq!(subjects[1], "初始提交");
            // limit 生效
            assert_eq!(GitAnalyzer::new().recent_commit_subjects(root, 1).len(), 1);
        });
    }

    #[test]
    fn non_repo_dir_degrades_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let info = GitAnalyzer::new().analyze(dir.path());
        assert!(!info.available);
        assert!(info.unavailable_reason.is_some());
    }

    #[test]
    fn is_repo_detects_git_dir() {
        with_real_repo(|root| {
            assert!(GitAnalyzer::new().is_repo(root));
            let other = tempfile::tempdir().unwrap();
            assert!(!GitAnalyzer::new().is_repo(other.path()));
        });
    }

    #[test]
    fn mtime_fallback_works_without_git() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1").unwrap();
        let mtime = mtime_of_newest_file(dir.path(), 100);
        assert!(mtime.is_some(), "应能从 mtime 推断更新时间");
    }

    #[test]
    fn mtime_skips_excluded_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let nm = dir.path().join("node_modules/pkg");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(nm.join("index.js"), "x").unwrap();
        // 目录里只有 node_modules，mtime 应返回 None（全被排除）
        assert!(
            mtime_of_newest_file(dir.path(), 100).is_none(),
            "依赖目录内的文件不应参与推断"
        );
    }

    #[test]
    fn git_version_returns_string_when_available() {
        let a = GitAnalyzer::new();
        if a.is_available() {
            let v = git_version(&a).unwrap();
            assert!(v.contains("git version"), "实际输出: {v}");
        }
    }

    #[test]
    fn custom_binary_path_is_used() {
        // 不存在的 git 路径 → is_available 为 false，但不 panic
        let a = GitAnalyzer::with_binary("definitely-not-a-real-git-binary");
        assert!(!a.is_available());
        assert_eq!(git_version(&a), None);
    }
}
