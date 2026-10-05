//! projectAssests 存储层：SQLite + FTS5 持久化。
//!
//! # 分层职责
//! 本 crate 只做**持久化**：SQL、行映射、事务、迁移。
//! 不含业务规则（健康度计算在 `projectassests-scanner`，评分在 `projectassests-asset`），
//! 也不含 HTTP 概念（DTO 转换在 `projectassests-server`）。
//!
//! # Repository 约定
//! - 每个聚合一个 Repository，构造函数接收 `&Pool`，自身无状态
//! - 方法返回 `Result<T, StorageError>`，**不泄漏 rusqlite 类型**
//!   （否则换存储实现时全部调用方都要改）
//! - 命名：写用 `upsert_/insert_/delete_/update_`，读用 `get_/list_/count_`
//! - 多语句写操作内部包事务，保证不出现"写了一半"
//!
//! # FTS5 同步（最易漏掉的一致性点）
//! `projects` / `assets` / `capabilities` 各有一张 `*_fts` 虚表。
//! 写入主表后**必须**同步 FTS，否则新数据搜不到。
//! 该责任放在 Repository 内部而非交给调用方——调用方不可能每次都记得。
//!
//! ⚠️ 删除顺序：**先删 FTS 再删主表**。
//! FTS 清理依赖 `SELECT id FROM 主表 WHERE …` 定位行，
//! 若主表先被清空，子查询返回空集，FTS 就会残留孤儿行（搜索命中已删数据）。

mod activities;
mod assets;
mod capabilities;
mod err;
mod fts;
mod insights;
mod jobs;
mod opportunities;
mod pool;
mod projects;
mod relations;
mod row;
mod schema;
mod settings_repo;

// 🔴 Activity / ActivityIcon 必须导出：它们是 `ActivityRepo::push` 的参数类型
// 与 `recent` 的返回类型。漏掉会让这两个方法对外**完全不可调用**
// （外部拿不到类型名），而编译期不报错——这类"公开方法配私有类型"的缺陷
// 只有在真正写调用方代码时才会暴露。
pub use activities::{Activity, ActivityIcon, ActivityRepo};
pub use assets::{AssetFilter, AssetRepo, AssetSort, AssetWriteOutcome};
pub use capabilities::CapabilityRepo;
pub use err::sqlite_err;
pub use fts::{
    compile_match_query, escape_like, like_pattern, needs_substring_fallback, Candidate,
    RetrievalRepo, FTS_MIN_TOKEN_LEN,
};
pub use insights::{InsightFilter, InsightRepo};
pub use jobs::JobRepo;
pub use opportunities::{OpportunityFilter, OpportunityRepo};
pub use pool::{Pool, PooledConnection};
pub use projects::{ProjectFilter, ProjectRepo, ProjectSort};
// `ScanFacts` / `SymbolStats` 定义在 domain（描述项目属性而非存储细节）。
// 这里重导出，让 pipeline 与测试可以从 `projectassests_storage::` 一并拿到写入所需类型，
// 而不必额外依赖 domain——调用方只跟存储层打交道即可。
pub use projectassests_domain::{ScanFacts, SymbolStats};
pub use relations::RelationRepo;
pub use row::{format_bytes, now_utc, parse_ts, relative_time, today_local};
pub use schema::{
    collect_stats, current_version, migrate, verify_fts5, DbStats, MigrationOutcome, MIGRATIONS,
};
pub use settings_repo::SettingsRepo;

use std::path::Path;

use projectassests_domain::{Settings, StorageError};

/// 数据库门面：持有连接池，提供全部 Repository 的构造入口。
///
/// 调用方只需持有一个 `Database`。Repository 无状态（只借 `&Pool`），
/// 按需临时构造即可，因此这里**不**预建字段——避免生命周期参数传染到上层。
#[derive(Debug)]
pub struct Database {
    pool: Pool,
}

impl Database {
    /// 打开文件数据库并应用迁移。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let pool = Pool::open(path)?;
        // conn 借用 pool，必须在移动 pool 前显式 drop
        let conn = pool.get()?;
        migrate(&conn)?;
        drop(conn);
        Ok(Self { pool })
    }

    /// 打开内存数据库（单元测试用）。
    pub fn in_memory() -> Result<Self, StorageError> {
        let pool = Pool::in_memory()?;
        let conn = pool.get()?;
        migrate(&conn)?;
        drop(conn);
        Ok(Self { pool })
    }

    /// 打开并在首次运行时写入默认设置。
    ///
    /// 与 `open` 分开：测试通常不需要默认设置，避免污染断言。
    pub fn open_with_defaults(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let db = Self::open(path)?;
        // 仅在无设置时写入默认值，避免覆盖用户已有配置
        if db.settings().get_all()?.is_none() {
            db.settings().save(&Settings::default())?;
        }
        Ok(db)
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// 借用一条连接（需要跨 repo 事务时使用）。
    pub fn conn(&self) -> Result<PooledConnection<'_>, StorageError> {
        self.pool.get()
    }

    pub fn projects(&self) -> ProjectRepo<'_> {
        ProjectRepo::new(&self.pool)
    }

    pub fn assets(&self) -> AssetRepo<'_> {
        AssetRepo::new(&self.pool)
    }

    pub fn capabilities(&self) -> CapabilityRepo<'_> {
        CapabilityRepo::new(&self.pool)
    }

    pub fn relations(&self) -> RelationRepo<'_> {
        RelationRepo::new(&self.pool)
    }

    pub fn insights(&self) -> InsightRepo<'_> {
        InsightRepo::new(&self.pool)
    }

    pub fn opportunities(&self) -> OpportunityRepo<'_> {
        OpportunityRepo::new(&self.pool)
    }

    pub fn jobs(&self) -> JobRepo<'_> {
        JobRepo::new(&self.pool)
    }

    pub fn activities(&self) -> ActivityRepo<'_> {
        ActivityRepo::new(&self.pool)
    }

    pub fn settings(&self) -> SettingsRepo<'_> {
        SettingsRepo::new(&self.pool)
    }

    /// 检索原语（FTS5 召回 + LIKE 回退 + 按 id 批量取回）。
    ///
    /// 与 `projects().list()` 的区别：list 面向"带筛选的浏览"，
    /// retrieval 面向"相关性排序的搜索"。两者共用同一批表，
    /// 但评分口径不同，故分开暴露而非塞进各 Repository。
    pub fn retrieval(&self) -> RetrievalRepo<'_> {
        RetrievalRepo::new(&self.pool)
    }

    /// 数据库统计（设置页「数据与隐私」展示真实占用）。
    pub fn stats(&self) -> Result<DbStats, StorageError> {
        let conn = self.pool.get()?;
        collect_stats(&conn)
    }

    /// FTS5 是否可用。不可用时检索层降级为 LIKE 子串匹配。
    pub fn fts_available(&self) -> bool {
        self.pool
            .get()
            .and_then(|c| verify_fts5(&c))
            .unwrap_or(false)
    }

    /// 当前 schema 版本。
    pub fn version(&self) -> Result<i32, StorageError> {
        let conn = self.pool.get()?;
        current_version(&conn)
    }

    /// 清空所有派生数据（设置页「清理派生数据」）。
    ///
    /// 🔴 三类数据刻意**不**删除，每一类都有具体理由：
    /// - `settings`：模型配置与扫描目录授权不是派生数据，误删会导致下次启动无法扫描。
    /// - `audit_log`：《技术设计书》§23「可审计」的落地。审计的价值恰恰在于
    ///   **不能被普通操作抹掉**——否则用户可以随手销毁"哪个模型在什么时候
    ///   处理过哪个敏感项目"的痕迹，Local-First 承诺就失去了可信凭证。
    ///   它记录的也不是派生数据，而是已发生的事实。
    /// - `projects` 行本身：`sensitive` 与 `description` 两列由**用户手写**
    ///   （见 `projects.rs` 的列所有权表）。删掉整行意味着用户标记的
    ///   "敏感项目"保护静默消失，重扫后 `sensitive=0`，
    ///   该项目代码就可能被送去云端模型——这是安全事故，不是数据丢失。
    ///   因此这里只把**派生列**重置回 schema 默认值，与 `clear_project_data`
    ///   （单项目版本）保持同一语义。
    pub fn clear_derived_data(&self) -> Result<ClearReport, StorageError> {
        let conn = self.pool.get()?;
        let mut report = ClearReport::default();

        conn.execute_batch("BEGIN")
            .map_err(|e| StorageError::sqlite("开始清理事务", e))?;

        let result = (|| -> Result<(), StorageError> {
            // 🔴 顺序不变式：**每张 FTS 虚表都必须在其主表之前清理**。
            //
            // 全量 DELETE 时顺序不影响结果（两边都是清空），
            // 但"按条件清理"（如 `clear_project_data` 只删某项目的派生数据）
            // 必须靠主表定位 FTS 行：
            // `DELETE FROM assets_fts WHERE asset_id IN (SELECT id FROM assets WHERE …)`。
            // 主表先删的话子查询就查不到任何行，FTS 会留下**永久孤儿索引行**——
            // 用户搜到一个早已删除的资产，点进去 404，且无法自愈。
            // 因此这里统一按"FTS 先、主表后"书写，让顺序成为可依赖的约定。
            let del = |table: &str| -> Result<usize, StorageError> {
                conn.execute(&format!("DELETE FROM {table}"), [])
                    .map_err(|e| StorageError::sqlite(format!("清空 {table}"), e))
            };
            // 🔴 `audit_log` 不在清理范围内（见方法文档）。
            report.activities = del("activities")?;
            // 机会：索引 → 主表
            report.fts_rows = del("opportunities_fts")?;
            report.opportunities = del("opportunities")?;
            // 洞察：索引 → 主表
            report.fts_rows += del("insights_fts")?;
            report.insights = del("insights")?;
            report.jobs = del("jobs")?;
            report.relations = del("relations")?;
            // 能力：索引 → 主表
            report.fts_rows += del("capabilities_fts")?;
            report.capabilities = del("capabilities")?;
            // 资产：索引 → 主表
            report.fts_rows += del("assets_fts")?;
            report.assets = del("assets")?;

            // 项目：重置派生列，保留用户手写列（sensitive / description）。
            // 未列出的列一律不动——与 upsert 的"列所有权"约定同构，
            // 新增派生列时必须显式加进来，否则它会带着旧值假装是新的。
            report.projects_reset = conn
                .execute(
                    "UPDATE projects SET
                        language='', framework='-', status='unknown',
                        health_score=0, completeness=NULL,
                        file_count=0, loc=0, symbol_count=0, module_count=0,
                        languages_json='[]', tags_json='[]',
                        git_commits=0, has_git=0, has_readme=0, has_tests=0,
                        ai_profile_json=NULL, scanned_at=NULL,
                        last_commit_at=NULL, updated_at=NULL",
                    [],
                )
                .map_err(|e| StorageError::sqlite("重置项目派生列", e))?;

            // 项目索引按重置后的行重建：tags/language 已清空，
            // 不重建的话搜索会命中一个"看起来有内容其实已被清掉"的项目。
            del("projects_fts")?;
            conn.execute_batch(
                "INSERT INTO projects_fts(project_id, name, description, tags, language, framework)
                 SELECT id, name, description, '', language, framework FROM projects",
            )
            .map_err(|e| StorageError::sqlite("重建项目索引", e))?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| StorageError::sqlite("提交清理事务", e))?;
                // 回收空间：DELETE 不会让文件自动缩小
                let _ = conn.execute_batch("VACUUM");
                Ok(report)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// 删除单个项目的全部派生数据（《技术设计书》§23「数据可清除」）。
    ///
    /// 保留 `projects` 行本身（用户可能想重新扫描），只清派生内容。
    pub fn clear_project_data(&self, project_id: &str) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        conn.execute_batch("BEGIN")
            .map_err(|e| StorageError::sqlite("开始项目清理事务", e))?;

        let result = (|| -> Result<(), StorageError> {
            // ⚠️ FTS 必须在 assets 之前删：子查询依赖 assets 表仍有数据
            conn.execute(
                "DELETE FROM assets_fts WHERE asset_id IN (SELECT id FROM assets WHERE project_id = ?1)",
                [project_id],
            )
            .map_err(|e| StorageError::sqlite("清理资产索引", e))?;
            conn.execute("DELETE FROM assets WHERE project_id = ?1", [project_id])
                .map_err(|e| StorageError::sqlite("删除项目资产", e))?;
            // 关系：两端任一涉及该项目
            conn.execute(
                "DELETE FROM relations WHERE source_id = ?1 OR target_id = ?1",
                [project_id],
            )
            .map_err(|e| StorageError::sqlite("删除项目关系", e))?;
            // 清空 AI 画像与扫描标记（均属派生数据）
            conn.execute(
                "UPDATE projects SET ai_profile_json = NULL, scanned_at = NULL WHERE id = ?1",
                [project_id],
            )
            .map_err(|e| StorageError::sqlite("清空项目画像", e))?;
            Ok(())
        })();

        match result {
            Ok(()) => conn
                .execute_batch("COMMIT")
                .map_err(|e| StorageError::sqlite("提交项目清理事务", e)),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}

/// 清空派生数据的报告（用于操作反馈：明确告诉用户动了多少）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClearReport {
    /// 被**重置派生列**的项目行数。
    ///
    /// 🔴 不叫 `projects`：项目行并未被删除（`sensitive`/`description` 是
    /// 用户手写数据，必须保留）。命名如实反映行为，避免上层把它
    /// 显示成"删除了 N 个项目"——那是假的，会让用户以为项目清单没了。
    pub projects_reset: usize,
    pub assets: usize,
    pub capabilities: usize,
    pub relations: usize,
    pub insights: usize,
    pub opportunities: usize,
    pub jobs: usize,
    pub activities: usize,
    /// FTS 索引行数。不计入 `total()`：它是主表的副本，重复计数会误导用户。
    pub fts_rows: usize,
}

// 🔴 这里刻意**没有** `audit_log` 字段：审计日志不在清理范围内
// （理由见 `clear_derived_data` 的文档）。字段一旦存在就等于邀请上层把它
// 显示成"audit_log: 0 行"，用户会读成"审计日志被清了 0 条"，
// 从而以为这个操作本可以删审计——那是错误的暗示。

impl ClearReport {
    /// 总删除行数（用户提示用）。
    pub fn total(&self) -> usize {
        self.assets
            + self.capabilities
            + self.relations
            + self.insights
            + self.opportunities
            + self.jobs
            + self.activities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{
        Asset, AssetType, AuditEntry, Evidence, Project, ProjectAiProfile, ProjectHighlight,
    };

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    pub(crate) fn sample_project(id: &str, name: &str, path: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            description: "演示项目".into(),
            language: "Python".into(),
            framework: "-".into(),
            created_at: Some("2024-01-01".into()),
            updated_at: Some("2025-01-01".into()),
            last_commit_at: Some("2025-01-01".into()),
            status: projectassests_domain::ProjectStatus::Active,
            health_score: 80,
            completeness: Some(0.7),
            tags: vec!["Python".into(), "Demo".into()],
            sensitive: false,
            stats: projectassests_domain::CodeStats {
                files: 42,
                loc: 3000,
                symbols: 12,
                modules: 4,
                languages: vec![projectassests_domain::LanguageShare {
                    name: "Python".into(),
                    pct: 100,
                    loc: 3000,
                }],
            },
            scan: projectassests_domain::ScanFacts::default(),
            ai_profile: None,
        }
    }

    pub(crate) fn sample_asset(id: &str, project_id: &str, name: &str) -> Asset {
        Asset {
            id: id.into(),
            project_id: project_id.into(),
            asset_type: AssetType::Component,
            name: name.into(),
            description: "演示资产".into(),
            content: None,
            source_path: format!("src/{name}.py"),
            confidence: 0.9,
            reuse_score: 0.88,
            generality: 0.8,
            stability: 0.75,
            tags: vec!["demo".into()],
            created_at: "2025-01-01".into(),
            evidence: Evidence {
                files: vec![format!("src/{name}.py")],
                commits: vec!["abc1234".into()],
                used_by: vec!["main()".into()],
                reasoning: vec!["被多个模块调用".into()],
            },
            user_feedback: None,
        }
    }

    #[test]
    fn in_memory_db_is_migrated() {
        assert_eq!(db().version().unwrap(), projectassests_domain::SCHEMA_VERSION);
    }

    #[test]
    fn file_db_is_created_and_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projectassests.db");
        {
            let d = Database::open(&path).unwrap();
            assert_eq!(d.version().unwrap(), projectassests_domain::SCHEMA_VERSION);
            assert!(path.exists());
        }
        assert!(path.exists(), "关闭连接后库文件应保留");
    }

    #[test]
    fn fts_is_available() {
        assert!(db().fts_available());
    }

    #[test]
    fn open_with_defaults_seeds_settings() {
        let dir = tempfile::tempdir().unwrap();
        let d = Database::open_with_defaults(dir.path().join("s.db")).unwrap();
        let s = d.settings().get_all().unwrap().expect("应写入默认设置");
        assert!(s.llm.embedding_local_only, "默认必须 Local-First");
    }

    /// 重复打开不得覆盖已有设置（否则用户配置每次重启都丢）。
    #[test]
    fn open_with_defaults_preserves_existing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        {
            let d = Database::open_with_defaults(&path).unwrap();
            let mut s = d.settings().get_all().unwrap().unwrap();
            s.llm.cloud_model = "my-custom-model".into();
            d.settings().save(&s).unwrap();
        }
        let d2 = Database::open_with_defaults(&path).unwrap();
        assert_eq!(
            d2.settings().get_all().unwrap().unwrap().llm.cloud_model,
            "my-custom-model",
            "不得覆盖用户配置"
        );
    }

    /// 🔴 这条测试守住三条真实约束，每一条都曾有具体缺陷：
    /// 1. 配置不被清（否则下次启动无法扫描）
    /// 2. **项目行不被删**（`sensitive` 是用户手写的安全策略，删行会让
    ///    Local-First 保护静默失效——重扫后 `sensitive=0`，代码可能被送去云端模型）
    /// 3. **审计日志不被清**（可审计的前提是审计凭证不能被普通操作抹掉）
    #[test]
    fn clear_derived_data_keeps_settings_projects_and_audit() {
        let d = Database::in_memory().unwrap();
        d.settings().save(&Settings::default()).unwrap();
        let mut p = sample_project("p1", "demo", "/tmp/demo");
        p.sensitive = true; // 用户手写的敏感标记
        p.ai_profile = Some(ProjectAiProfile {
            summary: "一个演示项目".into(),
            purpose: None,
            phase: None,
            highlights: vec![ProjectHighlight {
                title: "t".into(),
                desc: "d".into(),
                evidence_files: vec!["a.py".into()],
            }],
            archaeology: None,
            generated_by: "test".into(),
            generated_at: now_utc(),
        });
        d.projects().upsert(&p).unwrap();
        // 🔴 三类条目都要写：成功调用、失败调用、本地安全事件。
        // 「审计不可被普通操作抹掉」这条契约必须对**所有**类型成立——
        // 尤其是失败记录（数据已出网的唯一凭证）与安全事件（安全约束降级的留痕）。
        d.settings()
            .audit(&AuditEntry::llm_ok(
                now_utc(),
                "local:qwen3:8b",
                projectassests_domain::RouteTarget::Local,
                "ANALYZE_PROJECT",
                "生成项目画像",
                Some("p1".into()),
            ))
            .unwrap();
        d.settings()
            .audit(&AuditEntry::llm_failed(
                now_utc(),
                "cloud:some-model",
                projectassests_domain::RouteTarget::Cloud,
                "ANALYZE_PROJECT",
                "生成项目画像失败",
                Some("p1".into()),
                "请求被拒绝 (400)",
            ))
            .unwrap();
        d.settings()
            .audit(&AuditEntry::event(
                now_utc(),
                "SETTINGS",
                "用户取消项目「p1」的敏感标记",
                Some("p1".into()),
            ))
            .unwrap();

        let report = d.clear_derived_data().unwrap();

        assert_eq!(report.projects_reset, 1, "项目派生列应被重置");
        assert!(
            d.settings().get_all().unwrap().is_some(),
            "配置不是派生数据，不得被清除"
        );

        // 项目行必须还在，且用户手写列原样保留
        let kept = d.projects().get("p1").unwrap().expect("项目行不得被删除");
        assert!(kept.sensitive, "敏感标记是用户手写数据，清理不得抹掉");
        assert_eq!(kept.description, "演示项目", "描述是用户手写数据，应保留");
        assert!(kept.ai_profile.is_none(), "AI 画像是派生数据，应被清空");
        assert_eq!(kept.language, "", "语言来自扫描，应被重置");
        assert_eq!(kept.status, projectassests_domain::ProjectStatus::Unknown);

        // 审计日志必须还在——三类条目一条都不能少，且字段完好
        let logs = d.settings().recent_audit(10).unwrap();
        assert_eq!(logs.len(), 3, "审计日志不得被清除（成功/失败/安全事件三类都应在）");
        assert!(
            logs.iter().any(|e| e.ok == Some(true)),
            "成功调用记录应存活"
        );
        assert!(
            logs.iter().any(|e| e.ok == Some(false) && e.error.is_some()),
            "🔴 失败调用记录（数据已出网的唯一凭证）必须存活，且原因不丢"
        );
        assert!(
            logs.iter().any(|e| e.ok.is_none()),
            "本地安全事件记录应存活"
        );
    }

    /// 重置后项目索引必须与项目表一致，否则搜索会命中一个"内容已被清掉"的项目。
    #[test]
    fn clear_derived_data_rebuilds_project_fts_from_reset_rows() {
        let d = Database::in_memory().unwrap();
        let mut p = sample_project("p1", "video-tool", "/tmp/a");
        // 🔴 探针词必须只出现在 tags 里，不能是 name/description 的子串：
        // trigram 分词器下 "Video" 会同时命中 name "video-tool"，
        // 那样即使 tags 没被清掉断言也会通过——测试就成了摆设。
        p.tags = vec!["Orchestration".into()];
        d.projects().upsert(&p).unwrap();
        assert_eq!(d.projects().fts_count().unwrap(), 1);

        d.clear_derived_data().unwrap();

        assert_eq!(
            d.projects().fts_count().unwrap(),
            1,
            "项目行还在，索引行也应还在"
        );
        let conn = d.conn().unwrap();
        let hits = |q: &str| -> i64 {
            conn.query_row(
                "SELECT count(*) FROM projects_fts WHERE projects_fts MATCH ?1",
                [q],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(hits("Orchestration"), 0, "已重置的 tags 不得仍被索引命中");
        assert_eq!(hits("video"), 1, "name 仍应可检索");
        assert_eq!(hits("演示项目"), 1, "用户手写的 description 仍应可检索");
    }

    #[test]
    fn clear_derived_report_sums_without_fts() {
        let d = Database::in_memory().unwrap();
        d.projects().upsert(&sample_project("p1", "a", "/tmp/a")).unwrap();
        d.assets().upsert(&sample_asset("a1", "p1", "Foo")).unwrap();
        let r = d.clear_derived_data().unwrap();
        assert_eq!(r.projects_reset, 1);
        assert_eq!(r.assets, 1);
        assert!(r.fts_rows > 0, "FTS 行应被统计");
        // total 只算真正被删掉的行：项目是"重置"不是"删除"，
        // 计进去会让提示说"删除了 2 行"而项目其实还在。
        assert_eq!(r.total(), 1, "total 不应计入 FTS 副本行与被重置的项目行");
    }

    /// 清空后 FTS 不得残留孤儿行（否则搜索会命中已删数据）。
    #[test]
    fn clear_derived_data_purges_derived_fts() {
        let d = Database::in_memory().unwrap();
        d.projects().upsert(&sample_project("p1", "视频生成器", "/tmp/a")).unwrap();
        d.assets().upsert(&sample_asset("a1", "p1", "视频拼接工具")).unwrap();
        assert_eq!(d.assets().fts_count().unwrap(), 1);
        d.clear_derived_data().unwrap();
        assert_eq!(d.assets().fts_count().unwrap(), 0, "已删资产的索引应被清空");
    }

    #[test]
    fn clear_project_data_removes_only_that_project() {
        let d = Database::in_memory().unwrap();
        d.projects().upsert(&sample_project("p1", "a", "/tmp/a")).unwrap();
        d.projects().upsert(&sample_project("p2", "b", "/tmp/b")).unwrap();
        d.assets().upsert(&sample_asset("a1", "p1", "Foo")).unwrap();
        d.assets().upsert(&sample_asset("a2", "p2", "Bar")).unwrap();

        d.clear_project_data("p1").unwrap();

        assert_eq!(d.assets().count_all().unwrap(), 1, "只应删 p1 的资产");
        assert!(d.projects().get("p1").unwrap().is_some(), "项目行本身保留");
        assert!(d.assets().get("a1").unwrap().is_none());
        assert!(d.assets().get("a2").unwrap().is_some());
    }

    /// 这是本次修复的真实 bug：先删主表会让 FTS 子查询返回空集，残留孤儿行。
    #[test]
    fn clear_project_data_purges_asset_fts() {
        let d = Database::in_memory().unwrap();
        d.projects().upsert(&sample_project("p1", "a", "/tmp/a")).unwrap();
        d.projects().upsert(&sample_project("p2", "b", "/tmp/b")).unwrap();
        d.assets().upsert(&sample_asset("a1", "p1", "Foo")).unwrap();
        d.assets().upsert(&sample_asset("a2", "p2", "Bar")).unwrap();
        assert_eq!(d.assets().fts_count().unwrap(), 2);

        d.clear_project_data("p1").unwrap();

        assert_eq!(
            d.assets().fts_count().unwrap(),
            1,
            "p1 的资产索引必须被清理，不得残留孤儿行"
        );
        // 剩余索引必须仍属于 p2 且可被搜到
        let hits = d.assets().search_fts("Bar", 10).unwrap();
        assert_eq!(hits.len(), 1);
        let ghost = d.assets().search_fts("Foo", 10).unwrap();
        assert!(ghost.is_empty(), "已删资产不应出现在检索结果中");
    }

    #[test]
    fn clear_project_data_resets_ai_profile() {
        let d = Database::in_memory().unwrap();
        let mut p = sample_project("p1", "a", "/tmp/a");
        p.ai_profile = Some(ProjectAiProfile {
            summary: "一个演示项目".into(),
            purpose: None,
            phase: None,
            highlights: vec![ProjectHighlight {
                title: "t".into(),
                desc: "d".into(),
                evidence_files: vec!["a.py".into()],
            }],
            archaeology: None,
            generated_by: "test".into(),
            generated_at: now_utc(),
        });
        d.projects().upsert(&p).unwrap();
        assert!(d.projects().get("p1").unwrap().unwrap().ai_profile.is_some());

        d.clear_project_data("p1").unwrap();
        assert!(
            d.projects().get("p1").unwrap().unwrap().ai_profile.is_none(),
            "AI 画像属派生数据，应被清除"
        );
    }

    #[test]
    fn stats_report_real_counts() {
        let d = Database::in_memory().unwrap();
        d.projects().upsert(&sample_project("p1", "a", "/tmp/a")).unwrap();
        d.assets().upsert(&sample_asset("a1", "p1", "Foo")).unwrap();
        let s = d.stats().unwrap();
        assert_eq!(s.projects, 1);
        assert_eq!(s.assets, 1);
        assert_eq!(s.capabilities, 0);
    }
}
