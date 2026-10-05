//! Job Engine 的领域类型（《技术设计书》§15）。
//!
//! 🔴 设计红线：**UI 不允许直接调 `scan()` / `analyze()`**，一切走任务队列。
//! 且**所有 AI 分析都必须可取消**——这是桌面软件体验的硬要求。

use serde::{Deserialize, Serialize};

/// 任务类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobType {
    /// Level 0：目录扫描 + 项目发现
    ScanProject,
    /// Level 1：符号解析与索引
    IndexCode,
    /// Level 1：AST 解析
    ParseAst,
    /// Level 1：符号图构建
    BuildSymbolGraph,
    /// Level 1：本地 Embedding
    GenerateEmbedding,
    /// Level 2：AI 项目画像
    AnalyzeProject,
    /// Level 2：资产抽取
    ExtractAssets,
    /// 能力抽取
    ExtractCapabilities,
    /// 跨项目关系分析
    AnalyzeRelations,
    /// 洞察生成
    GenerateInsight,
    /// 机会发现
    DiscoverOpportunity,
}

impl JobType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ScanProject => "SCAN_PROJECT",
            Self::IndexCode => "INDEX_CODE",
            Self::ParseAst => "PARSE_AST",
            Self::BuildSymbolGraph => "BUILD_SYMBOL_GRAPH",
            Self::GenerateEmbedding => "GENERATE_EMBEDDING",
            Self::AnalyzeProject => "ANALYZE_PROJECT",
            Self::ExtractAssets => "EXTRACT_ASSETS",
            Self::ExtractCapabilities => "EXTRACT_CAPABILITIES",
            Self::AnalyzeRelations => "ANALYZE_RELATIONS",
            Self::GenerateInsight => "GENERATE_INSIGHT",
            Self::DiscoverOpportunity => "DISCOVER_OPPORTUNITY",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "SCAN_PROJECT" => Self::ScanProject,
            "INDEX_CODE" => Self::IndexCode,
            "PARSE_AST" => Self::ParseAst,
            "BUILD_SYMBOL_GRAPH" => Self::BuildSymbolGraph,
            "GENERATE_EMBEDDING" => Self::GenerateEmbedding,
            "ANALYZE_PROJECT" => Self::AnalyzeProject,
            "EXTRACT_ASSETS" => Self::ExtractAssets,
            "EXTRACT_CAPABILITIES" => Self::ExtractCapabilities,
            "ANALYZE_RELATIONS" => Self::AnalyzeRelations,
            "GENERATE_INSIGHT" => Self::GenerateInsight,
            "DISCOVER_OPPORTUNITY" => Self::DiscoverOpportunity,
            _ => return None,
        })
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::ScanProject => "扫描项目",
            Self::IndexCode => "索引代码",
            Self::ParseAst => "解析语法树",
            Self::BuildSymbolGraph => "构建符号图",
            Self::GenerateEmbedding => "生成向量",
            Self::AnalyzeProject => "分析项目",
            Self::ExtractAssets => "提取资产",
            Self::ExtractCapabilities => "提取能力",
            Self::AnalyzeRelations => "分析关联",
            Self::GenerateInsight => "生成洞察",
            Self::DiscoverOpportunity => "发现机会",
        }
    }

    /// 该任务是否属于 Level 2（需要调用 LLM）。
    /// 用于任务路由（本地/云端模型）与"敏感项目仅本地"约束的判定。
    pub fn requires_llm(&self) -> bool {
        matches!(
            self,
            Self::AnalyzeProject
                | Self::ExtractCapabilities
                | Self::AnalyzeRelations
                | Self::GenerateInsight
                | Self::DiscoverOpportunity
        )
    }

    /// 是否属于"深度分析"路由（低频高价值 → 推荐云端模型）。
    pub fn is_deep_analysis(&self) -> bool {
        matches!(
            self,
            Self::AnalyzeRelations | Self::GenerateInsight | Self::DiscoverOpportunity
        )
    }
}

/// 任务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    #[default]
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    /// 是否为终态（不会再变化）。终态任务不再出现在"进行中"列表。
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Queued => "排队中",
            Self::Running => "进行中",
            Self::Completed => "已完成",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }
}

/// 一个任务。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    #[serde(rename = "type")]
    pub job_type: JobType,
    pub status: JobStatus,
    /// 0.0-1.0
    pub progress: f64,
    /// 当前阶段文案（UI 直接展示，如"扫描目录，发现项目…"）
    pub stage: Option<String>,
    /// 已处理 / 总数（如 127/183 projects）
    pub processed: Option<u64>,
    pub total: Option<u64>,
    /// 错误信息（仅 Failed 时有值）
    pub error: Option<String>,
    /// 任务载荷（目录列表、项目 id 等），JSON 字符串
    pub payload: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl Job {
    /// 更新进度。
    ///
    /// 集中在此处而非各处直接改字段，是为了保证不变式：
    /// - progress 必须 clamp 到 0.0-1.0
    /// - 首次有进展时把 Queued 推进到 Running
    /// - 终态任务不再接受进度更新（避免取消后又被覆盖）
    ///
    /// # 🔴 为什么 progress=1.0 **不会**把状态翻成 Completed
    /// 这里曾经有 `if p >= 1.0 { status = Completed }`，
    /// 意图是修原型期"进度 100% 但侧栏一直显示索引中"的问题。
    /// 但它把**数据更新**和**生命周期转移**混成了一件事，
    /// 而这两者的真相源不同：
    /// - 进度 = handler 上报的值，可以提前到 1.0（活干完了）
    /// - 状态 = handler **有没有返回**，只有引擎知道
    ///
    /// 真实后果（由 pipeline 冒烟测试发现，单测从未覆盖）：
    /// `ScanProjectHandler` 调 `report_counted(1.0, "扫描完成")` 之后，
    /// 还要写活动流才返回。这段窗口里 DB 状态已是 completed，
    /// 于是 `has_active_of_type` 返回 false，
    /// 用户能触发**第二个并发扫描**，两个任务同时写同一批表。
    ///
    /// 连锁伤害：状态提前进终态后，`set_terminal` 的
    /// `WHERE status NOT IN ('completed','failed','cancelled')` 守卫
    /// 会让真正的 `finish()` 变成 no-op——失败任务的 error 文案写不进去，
    /// 用户只看到"失败"两个字却不知道原因。
    ///
    /// "进度 100% 但状态 running"不是 bug，而是**准确的中间态**：
    /// 活干完了、收尾还没结束。侧栏文案由 `stage` 决定（"扫描完成"），
    /// 不依赖状态字段，所以原型期那个显示问题本来就不需要靠翻状态来修。
    ///
    /// 终态只能由 `complete()` / `fail()` / `cancel()` 显式设置。
    pub fn set_progress(&mut self, progress: f64, stage: Option<String>) {
        if self.status.is_terminal() {
            return;
        }
        let p = progress.clamp(0.0, 1.0);
        self.progress = p;
        if let Some(s) = stage {
            self.stage = Some(s);
        }
        if self.status == JobStatus::Queued && p > 0.0 {
            self.status = JobStatus::Running;
        }
    }

    /// 标记完成。
    ///
    /// 🔴 这是把任务置为 Completed 的**唯一**入口：
    /// 进度值不能代替它（见 `set_progress`）。
    /// 进度一并置 1.0，避免"已完成但进度条停在 80%"。
    pub fn complete(&mut self) {
        if self.status.is_terminal() {
            return;
        }
        self.status = JobStatus::Completed;
        self.progress = 1.0;
    }

    /// 标记失败。终态后不可再变。
    pub fn fail(&mut self, err: impl Into<String>) {
        if self.status.is_terminal() {
            return;
        }
        self.status = JobStatus::Failed;
        self.error = Some(err.into());
    }

    /// 取消（对应用户点"取消"按钮；产品要求所有 AI 分析可取消）。
    pub fn cancel(&mut self) {
        if self.status.is_terminal() {
            return;
        }
        self.status = JobStatus::Cancelled;
    }

    /// 0-100 整数百分比，UI 直接展示。
    pub fn percent(&self) -> u8 {
        (self.progress.clamp(0.0, 1.0) * 100.0).round() as u8
    }

    /// "127 / 183" 形式的进度文本；无总数时返回 `None`（前端隐藏该段，不显示 "0 / 0"）。
    pub fn counter_text(&self) -> Option<String> {
        match (self.processed, self.total) {
            (Some(p), Some(t)) if t > 0 => Some(format!("{p} / {t}")),
            _ => None,
        }
    }
}

/// 扫描模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScanMode {
    /// 仅重算 File Watcher 报告的变更文件
    Incremental,
    /// 重新遍历全部授权目录
    #[default]
    Full,
}

impl ScanMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Incremental => "incremental",
            Self::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "incremental" => Self::Incremental,
            "full" => Self::Full,
            _ => return None,
        })
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Incremental => "增量扫描",
            Self::Full => "全量扫描",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job {
            id: "j1".into(),
            job_type: JobType::ScanProject,
            status: JobStatus::Queued,
            progress: 0.0,
            stage: None,
            processed: None,
            total: None,
            error: None,
            payload: None,
            created_at: "2026-09-29T10:00:00Z".into(),
            updated_at: "2026-09-29T10:00:00Z".into(),
        }
    }

    #[test]
    fn progress_advances_status_to_running() {
        let mut j = job();
        j.set_progress(0.4, Some("扫描目录".into()));
        assert_eq!(j.status, JobStatus::Running);
        assert_eq!(j.percent(), 40);
        assert_eq!(j.stage.as_deref(), Some("扫描目录"));
    }

    /// 🔴 回归：进度到 100% **不得**自动把状态翻成 Completed。
    ///
    /// 早期实现是 `if p >= 1.0 { status = Completed }`，本测试当时断言的正是那个行为，
    /// 于是错误被固化成契约、再无人质疑。它想修的是原型期
    /// "进度 100% 但侧栏一直显示索引中"的显示问题，但用错了手段：
    /// 状态字段的真相源是"handler 有没有返回"，不是进度值。
    ///
    /// 真实危害见 `set_progress` 的文档：并发扫描 + finish() 变 no-op。
    #[test]
    fn full_progress_does_not_complete_job() {
        let mut j = job();
        j.set_progress(1.0, Some("扫描完成".into()));
        // 进度满 ≠ 任务结束：handler 可能还在写活动流、注销令牌
        assert_eq!(j.progress, 1.0);
        assert_eq!(j.percent(), 100);
        assert_eq!(j.status, JobStatus::Running, "状态不该被进度值改写");
        assert!(!j.status.is_terminal());
        // 原本要解决的显示问题，正确解法是 stage 文案：
        // 侧栏显示"扫描完成"而不是"索引中"，用户看到的不是转圈
        assert_eq!(j.stage.as_deref(), Some("扫描完成"));
    }

    /// 终态只能由 complete() 显式设置。
    #[test]
    fn complete_is_the_only_path_to_completed() {
        let mut j = job();
        j.set_progress(1.0, Some("扫描完成".into()));
        j.complete();
        assert_eq!(j.status, JobStatus::Completed);
        assert!(j.status.is_terminal());
        assert_eq!(j.progress, 1.0);
    }

    /// complete() 要把进度补到 1.0：
    /// 任务可能没上报过 1.0 就正常返回（例如空目录扫描），
    /// 否则会出现"已完成但进度条停在 80%"。
    #[test]
    fn complete_forces_progress_to_full() {
        let mut j = job();
        j.set_progress(0.4, Some("扫了一半".into()));
        j.complete();
        assert_eq!(j.progress, 1.0);
        assert_eq!(j.percent(), 100);
    }

    /// 终态后 complete() 不得覆盖已有结论（取消的任务不该变成完成）。
    #[test]
    fn complete_ignores_terminal_state() {
        let mut j = job();
        j.cancel();
        j.complete();
        assert_eq!(j.status, JobStatus::Cancelled, "取消结论必须保留");

        let mut j2 = job();
        j2.fail("boom");
        j2.complete();
        assert_eq!(j2.status, JobStatus::Failed);
        assert_eq!(j2.error.as_deref(), Some("boom"), "失败原因不得被抹掉");
    }

    #[test]
    fn progress_is_clamped() {
        let mut j = job();
        j.set_progress(5.0, None);
        assert_eq!(j.progress, 1.0);
        let mut j2 = job();
        j2.set_progress(-3.0, None);
        assert_eq!(j2.progress, 0.0);
    }

    /// 取消后不得被后续进度覆盖（对应"取消扫描后进度条又跳回"这类 bug）。
    #[test]
    fn terminal_state_ignores_further_updates() {
        let mut j = job();
        j.cancel();
        assert_eq!(j.status, JobStatus::Cancelled);
        j.set_progress(0.9, Some("不应生效".into()));
        assert_eq!(j.status, JobStatus::Cancelled);
        assert_eq!(j.progress, 0.0);
        assert_ne!(j.stage.as_deref(), Some("不应生效"));

        let mut j2 = job();
        j2.fail("boom");
        assert_eq!(j2.error.as_deref(), Some("boom"));
        j2.fail("second");
        assert_eq!(j2.error.as_deref(), Some("boom"), "首个错误应保留");
    }

    #[test]
    fn counter_text_hidden_when_no_total() {
        let j = job();
        assert_eq!(j.counter_text(), None);
        let mut j2 = job();
        j2.processed = Some(127);
        j2.total = Some(183);
        assert_eq!(j2.counter_text().as_deref(), Some("127 / 183"));
        let mut j3 = job();
        j3.processed = Some(5);
        j3.total = Some(0);
        assert_eq!(j3.counter_text(), None, "总数为 0 不应显示 5 / 0");
    }

    #[test]
    fn job_type_roundtrips() {
        for t in [
            JobType::ScanProject,
            JobType::IndexCode,
            JobType::AnalyzeProject,
            JobType::ExtractAssets,
            JobType::DiscoverOpportunity,
        ] {
            assert_eq!(JobType::parse(t.as_str()), Some(t), "{:?}", t);
            assert!(!t.label_zh().is_empty());
        }
    }

    #[test]
    fn llm_routing_classification() {
        assert!(JobType::AnalyzeProject.requires_llm());
        assert!(!JobType::ScanProject.requires_llm());
        assert!(!JobType::IndexCode.requires_llm());
        assert!(JobType::DiscoverOpportunity.is_deep_analysis());
        assert!(!JobType::AnalyzeProject.is_deep_analysis(), "画像属快速分析");
    }

    #[test]
    fn status_and_scan_mode_roundtrip() {
        for s in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ] {
            assert_eq!(JobStatus::parse(s.as_str()), Some(s));
            assert!(!s.label_zh().is_empty());
        }
        assert!(JobStatus::Completed.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
        assert_eq!(ScanMode::parse("incremental"), Some(ScanMode::Incremental));
        assert_eq!(ScanMode::parse("full"), Some(ScanMode::Full));
        assert_eq!(ScanMode::parse("x"), None);
        assert_eq!(ScanMode::Incremental.label_zh(), "增量扫描");
    }

    #[test]
    fn job_type_serializes_screaming_snake() {
        assert_eq!(serde_json::to_string(&JobType::ScanProject).unwrap(), "\"SCAN_PROJECT\"");
    }
}
