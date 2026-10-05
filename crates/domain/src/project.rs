//! 项目实体：Spolia 的**容器**对象。
//!
//! 产品判断 #1（《产品设计书》§1）：核心对象不是 Project 而是 Asset，
//! 项目只是资产的容器。因此这里只保留"确定性可得"的画像字段，
//! 语义性描述（AI 画像）单独放在 [`ProjectAiProfile`]，未生成时为 `None`，
//! 前端据此展示"尚未分析"而非假数据。

use serde::{Deserialize, Serialize};

/// 项目状态。由确定性规则推断（最近 commit 时间 + Git 标记），不依赖 LLM。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    /// 90 天内有提交
    Active,
    /// 90~365 天无提交
    Paused,
    /// 超过 365 天无提交
    Abandoned,
    /// 提交数极少（≤3）或无 Git 历史且无实质代码
    Experimental,
    /// 尚未判定（刚被发现，未完成静态分析）
    #[default]
    Unknown,
}

impl ProjectStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Abandoned => "abandoned",
            Self::Experimental => "experimental",
            Self::Unknown => "unknown",
        }
    }

    /// 从数据库字符串解析；未知值降级为 `Unknown` 而非报错——
    /// 保证旧库在新版本下仍可读（向前兼容）。
    pub fn parse(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "paused" => Self::Paused,
            "abandoned" => Self::Abandoned,
            "experimental" => Self::Experimental,
            _ => Self::Unknown,
        }
    }

    /// 中文标签（UI 状态徽章）。
    ///
    /// 与 `JobStatus` / `OpportunityStatus` / `InsightBadge` 保持同一约定：
    /// 展示文案集中在领域层。项目状态是列表与详情页必显的徽章，
    /// 缺这个方法会迫使各前端页面自己维护一份映射表——口径必然漂移。
    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Active => "活跃",
            Self::Paused => "暂停",
            Self::Abandoned => "已归档",
            Self::Experimental => "实验性",
            Self::Unknown => "待判定",
        }
    }
}

/// 项目的静态画像（Level 0 / Level 1，**零 LLM 成本**）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// 项目根目录绝对路径。UI 中显示，不用于拼接任何 SQL。
    pub path: String,
    /// 一句话描述：优先取 README 首段；无 README 时为空串（**不编造**）。
    pub description: String,
    /// 主语言（按代码行占比最高者）
    pub language: String,
    /// 框架（由依赖清单推断，如 "Next.js"、"FastAPI"）；无法确定时为 "-"
    pub framework: String,
    /// ISO 日期（YYYY-MM-DD）。来源：Git 首次提交 / 文件最早 mtime。
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub last_commit_at: Option<String>,
    pub status: ProjectStatus,
    /// 0-100，由确定性规则计算（见 `spolia-scanner`）
    pub health_score: u8,
    /// 0.0-1.0，完成度：仅有 Git 历史时可估算，否则 `None`
    pub completeness: Option<f64>,
    /// 技术栈标签（来自依赖清单的键名，真实数据）
    pub tags: Vec<String>,
    /// 是否被用户标记为"敏感项目"：任何数据不进入云端模型上下文
    pub sensitive: bool,
    /// 静态统计
    pub stats: CodeStats,
    /// 扫描事实（Git 提交数、README/测试检测标志、扫描时间）。
    ///
    /// 🔴 **只读语义**：这些列由 `ProjectRepo::update_scan_facts` 专写，
    /// `upsert` 刻意不碰它们（避免"更新描述"把 Git 统计清零）。
    /// 因此构造 `Project` 时通常留 `Default`，读回时才有值。
    ///
    /// 项目详情页的"考古"与"工程质量"区块依赖它：
    /// 没有这个字段时，数据库里明明存着提交数，页面却只能显示"未知"，
    /// 或者更糟——用模板文案编一个数字。
    #[serde(default)]
    pub scan: ScanFacts,
    /// AI 生成的语义画像；未分析时为 `None`
    pub ai_profile: Option<ProjectAiProfile>,
}

impl Project {
    /// 健康度：由活跃度、Git 存在性、代码规模三项确定性指标加权，**不调用 LLM**。
    ///
    /// 权重设计（可测试、可解释，写入 Evidence 而非黑盒分数）：
    /// - 活跃度 50%：距最后修改的天数分段
    /// - Git 完整度 30%：有 Git 历史 / 有 README / 有测试目录
    /// - 规模合理性 20%：有一定代码量但未爆炸
    pub fn compute_health(git_commits: u32, has_git: bool, has_readme: bool, has_tests: bool, loc: usize, days_since_update: Option<i64>) -> u8 {
        let activity: f64 = match days_since_update {
            Some(d) if d <= 30 => 1.0,
            Some(d) if d <= 90 => 0.8,
            Some(d) if d <= 180 => 0.6,
            Some(d) if d <= 365 => 0.4,
            Some(_) => 0.2,
            None => 0.3, // 无法确定修改时间：给中低分而非 0
        };

        let mut completeness: f64 = 0.0;
        if has_git {
            completeness += 0.5;
        }
        if has_readme {
            completeness += 0.3;
        }
        if has_tests {
            completeness += 0.2;
        }
        if git_commits > 50 {
            completeness = (completeness + 0.1).min(1.0);
        }

        // 规模：太小（<50 行）几乎无内容，太大（>20 万行）多为生成物/依赖混入
        let scale: f64 = match loc {
            0 => 0.0,
            1..=50 => 0.3,
            51..=2_000 => 0.8,
            2_001..=200_000 => 1.0,
            _ => 0.7,
        };

        let score = activity * 0.5 + completeness * 0.3 + scale * 0.2;
        (score * 100.0).round().clamp(0.0, 100.0) as u8
    }

    /// 状态推断：完全确定性，可被单元测试覆盖。
    pub fn infer_status(git_commits: u32, days_since_update: Option<i64>, loc: usize) -> ProjectStatus {
        // 实验性：几乎没有提交，或几乎没有代码
        if git_commits > 0 && git_commits <= 3 {
            return ProjectStatus::Experimental;
        }
        if loc < 30 {
            return ProjectStatus::Experimental;
        }
        match days_since_update {
            Some(d) if d <= 90 => ProjectStatus::Active,
            Some(d) if d <= 365 => ProjectStatus::Paused,
            Some(_) => ProjectStatus::Abandoned,
            // 没有 Git 历史时无法判断活跃度；只要有实质代码就视为进行中
            None if loc >= 30 => ProjectStatus::Active,
            None => ProjectStatus::Experimental,
        }
    }

    /// 距最后活动的天数；无任何时间信息时返回 `None`。
    ///
    /// 取 `last_commit_at`（Git 权威）与 `updated_at`（文件 mtime 兜底）中**较近**的一个：
    /// 只有 mtime、没有 Git 的项目也要能算活跃度，否则一律"未知"会退化成默认排序。
    ///
    /// `now` 由调用方注入而非读时钟：保证同一份数据多次调用结果一致（可复现、可单测）。
    pub fn days_since_update(&self, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
        crate::days_since_latest(
            &[self.last_commit_at.as_deref(), self.updated_at.as_deref()],
            now,
        )
    }
}

/// Level 0 静态统计（真实扫描结果，无估算）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeStats {
    pub files: usize,
    pub loc: usize,
    /// 识别出的符号数（函数/类/接口），Level 1 后填充
    pub symbols: usize,
    /// 顶层模块（目录）数
    pub modules: usize,
    /// 语言构成：按代码行占比降序
    pub languages: Vec<LanguageShare>,
}

/// 扫描元数据：由扫描器分阶段填充的项目附加信息。
///
/// # 为什么放在 domain 而不是 storage
/// 这些字段描述的是**项目本身的属性**（有没有 Git、有没有测试、提交数多少），
/// 而不是"某张表怎么存"。放在 storage 会造成分层倒置：
/// 扫描器产出的数据要先转成存储层的类型才能写库，
/// 于是 `spolia-jobs` 的 pipeline 被迫依赖 `spolia-storage` 的领域概念。
///
/// # 为什么拆成两个结构体
/// `ScanMeta` 曾把 Level 0 的 Git 事实与 Level 1 的符号统计捆在一起。
/// 两者生产者不同（Level 0 = 目录扫描，Level 1 = 符号抽取），
/// 但只有一个写方法——于是 Level 1 任务用 `..Default::default()` 构造时
/// 会把 `git_commits` / `has_git` 静默清零，项目状态推断随即退化。
///
/// 现在按生产者拆分：[`ScanFacts`] 归 Level 0，[`SymbolStats`] 归 Level 1。
/// 各自只写自己的列，物理上不可能互相覆盖。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanFacts {
    /// Git 提交总数
    pub git_commits: u32,
    pub has_git: bool,
    pub has_readme: bool,
    pub has_tests: bool,
    /// 扫描完成时间；`None` 表示由存储层填当前时间
    pub scanned_at: Option<String>,
}

/// Level 1 符号统计（由符号抽取阶段写入）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SymbolStats {
    /// 抽取到的符号数
    pub symbol_count: usize,
    /// 顶层模块数
    pub module_count: usize,
}

/// 语言占比。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageShare {
    pub name: String,
    /// 0-100 整数百分比，所有项之和可能因四舍五入为 99~101，前端不做强校验
    pub pct: u8,
    pub loc: usize,
}

/// AI 生成的项目画像（Level 2）。缺失时前端显示"未分析"，**绝不用模板文案填充**。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectAiProfile {
    /// 项目是什么
    pub summary: String,
    /// 解决什么问题
    pub purpose: Option<String>,
    /// 当前所处阶段（如"多镜头生成优化"）
    pub phase: Option<String>,
    /// 亮点：每条都必须能回溯到真实文件
    pub highlights: Vec<ProjectHighlight>,
    /// 项目考古报告；仅有 Git 历史时生成
    pub archaeology: Option<Archaeology>,
    /// 生成该画像使用的模型与时间，用于"可审计"要求（《技术设计书》§23）
    pub generated_by: String,
    pub generated_at: String,
}

/// 项目亮点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectHighlight {
    pub title: String,
    pub desc: String,
    /// 支撑该亮点的真实文件相对路径（Evidence 要求）
    pub evidence_files: Vec<String>,
}

/// 项目考古报告（《产品设计书》§7-②）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Archaeology {
    /// AI Coding Session 数（阶段三接入会话历史后才有真实值，否则 0）
    pub sessions: u32,
    pub commits: u32,
    pub completeness: Option<f64>,
    /// 最后活跃阶段描述
    pub phase: Option<String>,
    /// 可打捞资产名（来自真实抽取结果）
    pub salvage: Vec<String>,
    /// 叙述文本：由确定性模板 + 真实数据拼装，或 AI 生成（带 generated_by 标记）
    pub narrative: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_deterministic_by_recency() {
        assert_eq!(Project::infer_status(20, Some(10), 5000), ProjectStatus::Active);
        assert_eq!(Project::infer_status(20, Some(200), 5000), ProjectStatus::Paused);
        assert_eq!(Project::infer_status(20, Some(800), 5000), ProjectStatus::Abandoned);
    }

    #[test]
    fn few_commits_or_tiny_code_means_experimental() {
        assert_eq!(Project::infer_status(2, Some(1), 9000), ProjectStatus::Experimental);
        assert_eq!(Project::infer_status(50, Some(1), 10), ProjectStatus::Experimental);
    }

    #[test]
    fn no_git_history_with_real_code_is_active() {
        assert_eq!(Project::infer_status(0, None, 800), ProjectStatus::Active);
        assert_eq!(Project::infer_status(0, None, 5), ProjectStatus::Experimental);
    }

    #[test]
    fn health_score_stays_in_range_and_rewards_signals() {
        let weak = Project::compute_health(1, false, false, false, 20, Some(900));
        let strong = Project::compute_health(120, true, true, true, 8_000, Some(5));
        assert!(weak <= 100 && strong <= 100);
        assert!(strong > weak, "strong={strong} weak={weak}");
        assert!(strong >= 80);
    }

    #[test]
    fn unknown_status_string_degrades_to_unknown() {
        assert_eq!(ProjectStatus::parse("weird-future-value"), ProjectStatus::Unknown);
        assert_eq!(ProjectStatus::parse("active"), ProjectStatus::Active);
    }

    #[test]
    fn status_roundtrips_through_str() {
        for s in [
            ProjectStatus::Active,
            ProjectStatus::Paused,
            ProjectStatus::Abandoned,
            ProjectStatus::Experimental,
        ] {
            assert_eq!(ProjectStatus::parse(s.as_str()), s);
        }
    }
}
