//! 分析输入快照。
//!
//! # 为什么不直接依赖 `spolia-storage`
//! 洞察引擎需要读项目、资产、能力、关系四类数据。若直接依赖 storage，
//! 每个检测器都得开数据库连接，单元测试要先建库造数据，
//! 且"检测逻辑"与"SQL 查询"耦死——想换一种加载策略就得改检测器。
//!
//! 快照结构体让引擎成为**纯函数**：输入一个内存结构，输出一批洞察。
//! 加载数据是上层（server / job）的职责。这样检测器可以被穷举单测，
//! 也便于将来做"离线重算全部洞察"这类批处理。

use spolia_domain::{Asset, AssetType, Capability, CapabilityLayer, Project, Relation};

/// 一轮洞察分析所需的全部输入。
#[derive(Debug, Clone, Default)]
pub struct AnalysisInput {
    pub projects: Vec<Project>,
    pub assets: Vec<Asset>,
    /// 全部能力节点（含三层）
    pub capabilities: Vec<Capability>,
    /// 全部关系边（用于 project→capability 的 implements 查询）
    pub relations: Vec<Relation>,
    /// 分析基准时间（注入而非读时钟，保证可复现）
    pub now: chrono::DateTime<chrono::Utc>,
}

impl AnalysisInput {
    /// 按 id 查项目。
    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// 某项目的资产。
    pub fn assets_of(&self, project_id: &str) -> Vec<&Asset> {
        self.assets.iter().filter(|a| a.project_id == project_id).collect()
    }

    /// 某项目实现的能力 id 集合。
    ///
    /// 从 relations 查而非 capabilities 表，因为能力是**跨项目共享**的节点，
    /// "哪个项目实现了它"只存在于关系边里。
    pub fn capabilities_of(&self, project_id: &str) -> Vec<String> {
        self.relations
            .iter()
            .filter(|r| {
                r.source_id == project_id
                    && r.relation_type == spolia_domain::RelationType::Implements
                    && r.target_type == spolia_domain::EntityKind::Capability
            })
            .map(|r| r.target_id.clone())
            .collect()
    }

    /// 某能力被哪些项目实现。
    pub fn projects_of_capability(&self, capability_id: &str) -> Vec<String> {
        self.relations
            .iter()
            .filter(|r| {
                r.target_id == capability_id
                    && r.relation_type == spolia_domain::RelationType::Implements
                    && r.source_type == spolia_domain::EntityKind::Project
            })
            .map(|r| r.source_id.clone())
            .collect()
    }

    /// 按 id 查能力。
    pub fn capability(&self, id: &str) -> Option<&Capability> {
        self.capabilities.iter().find(|c| c.id == id)
    }

    /// Capability 层节点（排除 Domain 骨架与 Implementation 细节）。
    pub fn capability_layer_nodes(&self) -> Vec<&Capability> {
        self.capabilities
            .iter()
            .filter(|c| c.layer == CapabilityLayer::Capability)
            .collect()
    }

    /// 指定类型的资产。
    pub fn assets_of_type(&self, t: AssetType) -> Vec<&Asset> {
        self.assets.iter().filter(|a| a.asset_type == t).collect()
    }

    /// 项目总数（首页统计口径）。
    pub fn project_count(&self) -> usize {
        self.projects.len()
    }
}

// 日期解析与"距今天数"统一由 domain 层提供（`spolia_domain::{parse_date, days_since}`）。
//
// 🔴 本模块刻意**不转发**这些函数：早期这里有一份独立实现，与 domain 重复。
// 两份实现意味着两处可能漂移的口径（例如一处把解析失败当 0 天、另一处当 None），
// 而"距今多少天"同时决定项目状态、健康度、遗忘资产检测与搜索排序——
// 口径不一致会让同一份数据在不同页面自相矛盾。
// 同样不 `pub use` 转发：那会留下两条导入路径，漂移只是被推迟而非消除。
// 需要日期能力时直接用 `spolia_domain::days_since`。

/// 项目的"闲置天数"：Git 提交时间与文件 mtime 取较近者。
///
/// 🔴 不能只看 `updated_at`（mtime）：一个持续提交但文件时间戳未变的项目
/// （例如刚 clone、或用 Git 操作而未改写工作区文件）会被误判为"久未更新"，
/// 进而被"遗忘资产"检测器错误地当成待挖掘对象推荐给用户。
///
/// 无任何时间信息时返回 `i64::MAX`（视为极久未更新），
/// 这样"闲置 ≥ N 天"的判定成立，而"最近 N 天内"的判定不成立——
/// 与检测器的两个用途都自洽。
pub fn idle_days(p: &spolia_domain::Project, now: chrono::DateTime<chrono::Utc>) -> i64 {
    p.days_since_update(now).unwrap_or(i64::MAX)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use spolia_domain::{
        CodeStats, EntityKind, Evidence, ProjectStatus, RelationType,
    };

    pub fn project(id: &str, status: ProjectStatus, updated: Option<&str>) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            path: format!("/tmp/{id}"),
            description: String::new(),
            language: "Python".into(),
            framework: "-".into(),
            created_at: Some("2023-01-01".into()),
            updated_at: updated.map(str::to_string),
            last_commit_at: updated.map(str::to_string),
            status,
            health_score: 70,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats::default(),
            scan: spolia_domain::ScanFacts::default(),
            ai_profile: None,
        }
    }

    pub fn asset(
        id: &str,
        project_id: &str,
        name: &str,
        ty: AssetType,
        score: f64,
    ) -> Asset {
        Asset {
            id: id.into(),
            project_id: project_id.into(),
            asset_type: ty,
            name: name.into(),
            description: format!("{name} 描述"),
            content: None,
            source_path: format!("{project_id}/src/{name}.py"),
            confidence: 0.9,
            reuse_score: score,
            generality: 0.8,
            stability: 0.7,
            tags: vec![],
            created_at: "2025-01-01".into(),
            evidence: Evidence {
                files: vec![format!("{project_id}/src/{name}.py")],
                ..Default::default()
            },
            user_feedback: None,
        }
    }

    pub fn implements(project_id: &str, cap_id: &str) -> Relation {
        Relation::new(
            format!("r_{project_id}_{cap_id}"),
            project_id,
            EntityKind::Project,
            RelationType::Implements,
            cap_id,
            EntityKind::Capability,
            0.9,
        )
    }

    pub fn cap(id: &str, name: &str, layer: CapabilityLayer, parent: Option<&str>) -> Capability {
        let mut c = Capability::new(
            id,
            name,
            layer,
            parent.map(str::to_string),
            0.9,
        )
        .unwrap();
        c.description = format!("{name} 能力");
        c
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-29T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn lookups_work() {
        let input = AnalysisInput {
            projects: vec![project("p1", ProjectStatus::Active, Some("2026-09-01"))],
            assets: vec![asset("a1", "p1", "Foo", AssetType::Code, 0.9)],
            capabilities: vec![
                cap("d_ai", "AI", CapabilityLayer::Domain, None),
                cap("c_rag", "RAG", CapabilityLayer::Capability, Some("d_ai")),
            ],
            relations: vec![implements("p1", "c_rag")],
            now: now(),
        };

        assert!(input.project("p1").is_some());
        assert!(input.project("nope").is_none());
        assert_eq!(input.assets_of("p1").len(), 1);
        assert_eq!(input.capabilities_of("p1"), vec!["c_rag".to_string()]);
        assert_eq!(input.projects_of_capability("c_rag"), vec!["p1".to_string()]);
        assert!(input.capability("c_rag").is_some());
        assert_eq!(input.capability_layer_nodes().len(), 1, "只返回 Capability 层");
        assert_eq!(input.assets_of_type(AssetType::Code).len(), 1);
        assert_eq!(input.project_count(), 1);
    }

    #[test]
    fn empty_input_is_safe() {
        let input = AnalysisInput::default();
        assert!(input.project("x").is_none());
        assert!(input.assets_of("x").is_empty());
        assert!(input.capabilities_of("x").is_empty());
        assert_eq!(input.project_count(), 0);
    }

    // 日期解析与 days_since 的行为由 domain 层测试覆盖
    // （crates/domain/src/time.rs），此处不重复——重复测试会让
    // 口径变更时出现"一处改了两处红"的无效维护成本。
    // 下面只测 insight 自己的概念：idle_days。

    /// 🔴 只看 mtime 会把"持续提交但文件时间未变"的项目误判为久未更新，
    /// 进而被"遗忘资产"检测器错误推荐。必须取 Git 与 mtime 中较近者。
    #[test]
    fn idle_days_prefers_the_more_recent_signal() {
        let mut p = project("p1", ProjectStatus::Active, Some("2026-09-01"));
        p.updated_at = Some("2026-09-01".into()); // mtime 很久以前
        p.last_commit_at = Some("2026-09-28".into()); // 但昨天刚提交
        assert_eq!(idle_days(&p, now()), 1, "应采用较近的 Git 提交时间");
    }

    #[test]
    fn idle_days_falls_back_to_mtime_without_git() {
        let mut p = project("p2", ProjectStatus::Active, None);
        p.updated_at = Some("2026-09-19".into());
        p.last_commit_at = None; // 无 Git 历史
        assert_eq!(idle_days(&p, now()), 10);
    }

    /// 无任何时间信息 → i64::MAX（视为极久未更新），
    /// 让"闲置 ≥ N 天"成立而"最近 N 天内"不成立。
    #[test]
    fn idle_days_without_any_timestamp_is_max() {
        let mut p = project("p3", ProjectStatus::Active, None);
        p.updated_at = None;
        p.last_commit_at = None;
        assert_eq!(idle_days(&p, now()), i64::MAX);
    }

    /// 无法解析的脏数据同样视为"未知"，不得当成 0 天（"今天刚更新"）。
    #[test]
    fn idle_days_treats_garbage_as_unknown() {
        let mut p = project("p4", ProjectStatus::Active, None);
        p.updated_at = Some("not-a-date".into());
        p.last_commit_at = Some("也不对".into());
        assert_eq!(idle_days(&p, now()), i64::MAX);
    }

    #[test]
    fn idle_days_is_never_negative() {
        let mut p = project("p5", ProjectStatus::Active, None);
        p.updated_at = Some("2027-01-01".into()); // 未来时间
        assert_eq!(idle_days(&p, now()), 0);
    }
}
