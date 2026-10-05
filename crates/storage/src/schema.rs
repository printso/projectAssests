//! 数据库 schema 与迁移。
//!
//! # 迁移策略
//! 顺序版本化迁移：`user_version` 存当前版本，启动时依次应用高于当前版本的迁移。
//! 每条迁移**必须幂等可重放**（用 `IF NOT EXISTS`），因为中断后可能重复执行。
//!
//! 🔴 纪律（开源多人维护必备）：
//! - 已发布的迁移**永不修改**，需要变更就追加新版本
//! - 新增迁移必须同时在 `MIGRATIONS` 与 `projectassests_domain::SCHEMA_VERSION` 递增
//! - `lib.rs` 中有一致性测试强制检查这两者同步
//!
//! # FTS5 分词器选择
//! 使用 `trigram` 而非默认 `unicode61`：`unicode61` 按空白/标点分词，
//! 对中文整句只能匹配完整 token，`MATCH '视频'` 会返回 0 条。
//! trigram 按 3 字符滑窗切分，中文子串可命中。
//!
//! ⚠️ trigram 的已知限制：**查询串少于 3 字符时无法命中**（如"视频"2 字）。
//! `projectassests-search` 对此实现了 LIKE 子串回退，并把 `used_substring_fallback`
//! 暴露给前端提示用户，避免"为什么搜不到/搜太多"的困惑。

use rusqlite::Connection;

use projectassests_domain::StorageError;

/// 迁移脚本：`(版本号, 说明, SQL)`。
///
/// SQL 以字符串数组存放而非单个大字符串：便于逐条执行与定位失败语句。
pub static MIGRATIONS: &[(i32, &str, &[&str])] = &[
    (1, "初始 schema（11 张表 + FTS5）", V1),
    (2, "洞察与机会的 FTS5 索引", V2),
    (3, "audit_log 记录模型调用成败（失败也留痕）", V3),
];

/// V1：初始 schema，对应《技术设计书》§11 的 11 张表。
///
/// 额外增加了 `settings` / `activities` / `audit_log` 三张运维表：
/// - `settings`：单机配置持久化（原型期存 localStorage，真实版必须落库）
/// - `activities`：首页"最近活动"流的真实来源（替代 mock）
/// - `audit_log`：《技术设计书》§23「可审计」要求的落地
const V1: &[&str] = &[
    // ── 元数据 ─────────────────────────────────────────────────────────
    // 键值设置表：settings 与 secrets 分离，便于审计与导出时排除敏感项
    "CREATE TABLE IF NOT EXISTS settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        -- 标记为敏感的键不会出现在诊断导出中
        sensitive INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL
    )",

    // ── 1. projects ────────────────────────────────────────────────────
    "CREATE TABLE IF NOT EXISTS projects (
        id             TEXT PRIMARY KEY,
        name           TEXT NOT NULL,
        -- 绝对路径，带唯一约束：同一目录不会被重复登记
        path           TEXT NOT NULL UNIQUE,
        description    TEXT NOT NULL DEFAULT '',
        language       TEXT NOT NULL DEFAULT '',
        framework      TEXT NOT NULL DEFAULT '-',
        created_at     TEXT,
        updated_at     TEXT,
        last_commit_at TEXT,
        status         TEXT NOT NULL DEFAULT 'unknown',
        health_score   INTEGER NOT NULL DEFAULT 0,
        completeness   REAL,
        -- 静态统计（Level 0）
        file_count     INTEGER NOT NULL DEFAULT 0,
        loc            INTEGER NOT NULL DEFAULT 0,
        symbol_count   INTEGER NOT NULL DEFAULT 0,
        module_count   INTEGER NOT NULL DEFAULT 0,
        -- JSON: [{name, pct, loc}]
        languages_json TEXT NOT NULL DEFAULT '[]',
        -- JSON: [tag]
        tags_json      TEXT NOT NULL DEFAULT '[]',
        git_commits    INTEGER NOT NULL DEFAULT 0,
        has_git        INTEGER NOT NULL DEFAULT 0,
        has_readme     INTEGER NOT NULL DEFAULT 0,
        has_tests      INTEGER NOT NULL DEFAULT 0,
        sensitive      INTEGER NOT NULL DEFAULT 0,
        -- JSON: ProjectAiProfile，未分析时为 NULL（前端显示「未分析」而非假数据）
        ai_profile_json TEXT,
        scanned_at     TEXT
    )",
    "CREATE INDEX IF NOT EXISTS idx_projects_status ON projects(status)",
    "CREATE INDEX IF NOT EXISTS idx_projects_language ON projects(language)",
    "CREATE INDEX IF NOT EXISTS idx_projects_updated ON projects(updated_at DESC)",
    "CREATE INDEX IF NOT EXISTS idx_projects_health ON projects(health_score DESC)",

    // ── 2. assets（统一资产表 + type 判别）──────────────────────────────
    "CREATE TABLE IF NOT EXISTS assets (
        id          TEXT PRIMARY KEY,
        project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        type        TEXT NOT NULL,
        name        TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        content     TEXT,
        -- 相对项目根的路径：库可跨机器复制，且不把用户目录结构写入派生数据
        source_path TEXT NOT NULL DEFAULT '',
        confidence  REAL NOT NULL DEFAULT 0,
        reuse_score REAL NOT NULL DEFAULT 0,
        generality  REAL NOT NULL DEFAULT 0,
        stability   REAL NOT NULL DEFAULT 0,
        tags_json   TEXT NOT NULL DEFAULT '[]',
        -- JSON: Evidence{files, commits, used_by, reasoning}
        evidence_json TEXT NOT NULL DEFAULT '{}',
        created_at  TEXT NOT NULL,
        -- 用户反馈：useful / useless / ignored（Rediscovered Value 指标来源）
        user_feedback TEXT
    )",
    "CREATE INDEX IF NOT EXISTS idx_assets_project ON assets(project_id)",
    "CREATE INDEX IF NOT EXISTS idx_assets_type ON assets(type)",
    "CREATE INDEX IF NOT EXISTS idx_assets_reuse ON assets(reuse_score DESC)",
    "CREATE INDEX IF NOT EXISTS idx_assets_feedback ON assets(user_feedback)",

    // ── 3. capabilities（三层，parent_id 自引用）───────────────────────
    "CREATE TABLE IF NOT EXISTS capabilities (
        id           TEXT PRIMARY KEY,
        name         TEXT NOT NULL,
        description  TEXT NOT NULL DEFAULT '',
        -- domain / capability / implementation
        layer        TEXT NOT NULL,
        parent_id    TEXT REFERENCES capabilities(id) ON DELETE CASCADE,
        confidence   REAL NOT NULL DEFAULT 0,
        project_count INTEGER NOT NULL DEFAULT 0,
        -- 同一父节点下能力名唯一：防止重复抽取导致标签爆炸
        UNIQUE(name, parent_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_capabilities_parent ON capabilities(parent_id)",
    "CREATE INDEX IF NOT EXISTS idx_capabilities_layer ON capabilities(layer)",

    // ── 4. relations（泛化边表）────────────────────────────────────────
    "CREATE TABLE IF NOT EXISTS relations (
        id            TEXT PRIMARY KEY,
        source_id     TEXT NOT NULL,
        source_type   TEXT NOT NULL,
        relation_type TEXT NOT NULL,
        target_id     TEXT NOT NULL,
        target_type   TEXT NOT NULL,
        confidence    REAL NOT NULL DEFAULT 0,
        -- JSON: [evidence string]
        evidence_json TEXT NOT NULL DEFAULT '[]',
        UNIQUE(source_id, relation_type, target_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_relations_source ON relations(source_id)",
    "CREATE INDEX IF NOT EXISTS idx_relations_target ON relations(target_id)",
    "CREATE INDEX IF NOT EXISTS idx_relations_type ON relations(relation_type)",

    // ── 5. jobs（任务队列，UI 不得直接调 scan/analyze）─────────────────
    "CREATE TABLE IF NOT EXISTS jobs (
        id          TEXT PRIMARY KEY,
        type        TEXT NOT NULL,
        status      TEXT NOT NULL DEFAULT 'queued',
        progress    REAL NOT NULL DEFAULT 0,
        stage       TEXT,
        processed   INTEGER,
        total       INTEGER,
        error       TEXT,
        payload_json TEXT,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status)",
    "CREATE INDEX IF NOT EXISTS idx_jobs_created ON jobs(created_at DESC)",

    // ── 6. insights ────────────────────────────────────────────────────
    "CREATE TABLE IF NOT EXISTS insights (
        id          TEXT PRIMARY KEY,
        type        TEXT NOT NULL,
        title       TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        -- JSON: [EvidenceItem]
        evidence_json TEXT NOT NULL DEFAULT '[]',
        confidence  REAL NOT NULL DEFAULT 0,
        tags_json   TEXT NOT NULL DEFAULT '[]',
        project_ids_json TEXT NOT NULL DEFAULT '[]',
        asset_ids_json   TEXT NOT NULL DEFAULT '[]',
        created_at  TEXT NOT NULL,
        user_feedback TEXT
    )",
    "CREATE INDEX IF NOT EXISTS idx_insights_type ON insights(type)",
    "CREATE INDEX IF NOT EXISTS idx_insights_created ON insights(created_at DESC)",
    "CREATE INDEX IF NOT EXISTS idx_insights_feedback ON insights(user_feedback)",

    // ── 7. opportunities（历史 → 新项目的桥梁）──────────────────────────
    "CREATE TABLE IF NOT EXISTS opportunities (
        id           TEXT PRIMARY KEY,
        title        TEXT NOT NULL,
        description  TEXT NOT NULL DEFAULT '',
        source_project_ids_json TEXT NOT NULL DEFAULT '[]',
        source_asset_ids_json   TEXT NOT NULL DEFAULT '[]',
        required_capabilities_json TEXT NOT NULL DEFAULT '[]',
        missing_capabilities_json  TEXT NOT NULL DEFAULT '[]',
        coverage     REAL NOT NULL DEFAULT 0,
        rating       INTEGER NOT NULL DEFAULT 1,
        why          TEXT NOT NULL DEFAULT '',
        evidence_json TEXT NOT NULL DEFAULT '[]',
        status       TEXT NOT NULL DEFAULT 'new',
        created_at   TEXT NOT NULL,
        -- JSON: OpportunityAnalysis，「深入分析」后填充
        analysis_json TEXT
    )",
    "CREATE INDEX IF NOT EXISTS idx_opportunities_status ON opportunities(status)",
    "CREATE INDEX IF NOT EXISTS idx_opportunities_rating ON opportunities(rating DESC)",

    // ── 8. activities（首页「最近活动」真实来源）───────────────────────
    "CREATE TABLE IF NOT EXISTS activities (
        id         TEXT PRIMARY KEY,
        -- repeat / check / link / bulb / scan / alert …（前端映射图标）
        icon       TEXT NOT NULL DEFAULT 'check',
        title      TEXT NOT NULL,
        detail     TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_activities_created ON activities(created_at DESC)",

    // ── 9. audit_log（§23「可审计」：哪些数据、发给哪个模型、什么时候）──
    // 注意：`ok` / `error` 两列由 **V3** 追加，不在这里定义。
    // 已发布的迁移永不修改（见文件头纪律），全新库也是先建这张表、再由 V3 补列，
    // 与老库升级走完全相同的路径——避免"新库和老库结构不一致"。
    "CREATE TABLE IF NOT EXISTS audit_log (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        at          TEXT NOT NULL,
        model       TEXT NOT NULL,
        route       TEXT NOT NULL,
        job_type    TEXT NOT NULL,
        summary     TEXT NOT NULL DEFAULT '',
        project_id  TEXT
    )",
    "CREATE INDEX IF NOT EXISTS idx_audit_at ON audit_log(at DESC)",

    // ── 10/11. FTS5 全文索引 ───────────────────────────────────────────
    // 项目索引：name/description/tags/language/framework
    "CREATE VIRTUAL TABLE IF NOT EXISTS projects_fts USING fts5(
        project_id UNINDEXED,
        name,
        description,
        tags,
        language,
        framework,
        tokenize = 'trigram'
    )",
    // 资产索引：name/description/tags/source_path
    "CREATE VIRTUAL TABLE IF NOT EXISTS assets_fts USING fts5(
        asset_id UNINDEXED,
        name,
        description,
        tags,
        source_path,
        tokenize = 'trigram'
    )",
    // 能力索引
    "CREATE VIRTUAL TABLE IF NOT EXISTS capabilities_fts USING fts5(
        capability_id UNINDEXED,
        name,
        description,
        tokenize = 'trigram'
    )",
];

/// V2：给洞察与机会建立 FTS5 索引。
///
/// # 为什么需要这一版
/// 洞察与机会是系统给出的**结论性**内容，用户提问时最想拿到的正是结论。
/// 但它们此前完全没有检索能力，导致对话式分析师答不出库里明明有的东西：
/// 用户问"我有哪些重复实现的代码？"，库里存着一条标题为
/// "你在 2 个项目中重复实现了「Task Queue」"的洞察（带 10 条证据），
/// 分析师却回答"没有找到相关记录"。
///
/// # 🔴 为什么必须回填存量数据
/// 只建空表的话，**已有数据库升级后旧洞察仍然搜不到**——
/// 用户明明看见洞察页有内容，搜索却一无所获，且没有任何提示。
/// 这类"功能看起来坏了但没人报错"的缺陷最难排查。
/// 因此建表后立刻从主表回填。
///
/// # 🔴 回填的列必须与 `sync_fts` 完全一致
/// 回填是一次性 SQL，增量写入走 Rust 侧 `InsightRepo::sync_fts`。
/// 两者若列集合不同，就会出现"旧洞察搜得到 A 字段、新洞察搜得到 B 字段"，
/// 表现为同一查询的召回质量随数据新旧而异——极难定位。
/// 因此两边共用同一份列定义，见 `InsightRepo::sync_fts` 的注释。
///
/// # 为什么 `insight_type` 存英文原值而非中文标签
/// 迁移是纯 SQL，拿不到 Rust 的 `InsightType::label_zh()`。
/// 若回填存英文、增量存中文，又会造成上面那种新旧不一致。
/// 统一存英文 `as_str()`：洞察标题本身已含描述性文字
/// （"你在 2 个项目中重复实现了…"），搜"重复实现"照样能命中。
const V2: &[&str] = &[
    // 洞察索引：标题 / 描述 / 标签 / 类型 / 证据标签
    "CREATE VIRTUAL TABLE IF NOT EXISTS insights_fts USING fts5(
        insight_id UNINDEXED,
        title,
        description,
        tags,
        insight_type,
        evidence,
        tokenize = 'trigram'
    )",
    // 机会索引：标题 / 描述 / 依据 / 能力清单 / 证据
    "CREATE VIRTUAL TABLE IF NOT EXISTS opportunities_fts USING fts5(
        opportunity_id UNINDEXED,
        title,
        description,
        why,
        capabilities,
        evidence,
        tokenize = 'trigram'
    )",
    // ── 存量回填 ────────────────────────────────────────────────
    // tags_json / evidence_json 存的是 JSON 数组，用 json_each 展开成空格分隔串：
    // trigram 按字符滑窗切分，`["a","b"]` 直接塞进去会把引号和方括号也切成片段，
    // 既污染索引又浪费空间。证据只取 `label`（文件名/项目名），
    // `target` 是 `project_id:相对路径` 形式，含 id 哈希，索引它只会引入噪音。
    "INSERT INTO insights_fts(insight_id, title, description, tags, insight_type, evidence)
     SELECT i.id,
            i.title,
            i.description,
            (SELECT group_concat(value, ' ') FROM json_each(i.tags_json)),
            i.type,
            (SELECT group_concat(json_extract(value, '$.label'), ' ')
               FROM json_each(i.evidence_json))
     FROM insights i
     WHERE NOT EXISTS (SELECT 1 FROM insights_fts f WHERE f.insight_id = i.id)",
    // 机会的 required/missing 能力清单都是字符串数组，合并成一列检索：
    // 用户搜"任务队列"时，无论它出现在"已具备"还是"缺失"清单里都该命中。
    "INSERT INTO opportunities_fts(
         opportunity_id, title, description, why, capabilities, evidence)
     SELECT o.id,
            o.title,
            o.description,
            o.why,
            (SELECT group_concat(value, ' ') FROM json_each(o.required_capabilities_json))
                || ' ' ||
            (SELECT group_concat(value, ' ') FROM json_each(o.missing_capabilities_json)),
            (SELECT group_concat(value, ' ') FROM json_each(o.evidence_json))
     FROM opportunities o
     WHERE NOT EXISTS (SELECT 1 FROM opportunities_fts f WHERE f.opportunity_id = o.id)",
];

/// V3：`audit_log` 增加 `ok` / `error` 两列，让审计能区分模型调用的成败。
///
/// # 为什么需要这一版
/// 此前审计**只在成功路径写入**：`profile.rs` 的 `complete(...).await?` 失败就直接
/// 返回，`audit(...)` 在它后面，根本执行不到；`analyst.rs` 的 `Err` 分支只
/// `tracing::warn!` 后降级。于是"模型调用失败"这件事在审计日志里**完全没有痕迹**。
///
/// 🔴 这比"少记一条日志"严重得多：请求被网关拒绝（400 未开通 / 超时 / 限流）时，
/// **prompt 已经发出去了**——网关是收到之后才拒的。审计要回答的是
/// "我的数据什么时候发给了云端"，只记成功会让用户得到**假答案**。
/// 对一个以 Local-First / 可审计为信任基础的产品，这是审计链的完整性缺口。
///
/// # 🔴 存量行怎么回填（`model = '-'` 是唯一判据）
/// `audit_log` 承担两种语义，回填必须分开处理，否则会把语义搞错：
/// 1. **模型调用**（`model` = `cloud:xxx` / `local:xxx`）：v3 之前只在成功时写入，
///    所以存量行**必然都是成功的** → 回填 `ok = 1`。这是事实，不是猜测。
/// 2. **本地安全事件**（`model = '-'`，如"用户关闭了敏感项目仅本地约束"）：
///    压根不是一次调用，没有成败概念 → 回填 `ok = NULL`。
///
/// 🔴 绝不能把第 2 类也填成 `1`：那会让 UI 在「用户关闭了安全约束」旁边
/// 显示一个绿色对勾，把一次**安全降级**渲染成"操作成功"。
/// `NULL`（Rust 侧 `Option::None`）表示"不适用"，前端据此不渲染成败标记。
///
/// `model = '-'` 这个约定由 `AuditEntry::event()` 构造器保证（见 domain/settings.rs）。
///
/// # 🔴 为什么这里能用 `ALTER TABLE ADD COLUMN`（它不支持 `IF NOT EXISTS`）
/// 本文件开头的纪律写着"每条迁移必须幂等可重放（用 `IF NOT EXISTS`）"，
/// 但 SQLite 的 `ADD COLUMN` 没有这个语法。此处依然安全，理由在 `migrate()`：
/// 每条迁移包在 `BEGIN…COMMIT` 里，且 `user_version` 与 DDL **同事务提交**。
/// - 提交成功 ⇒ `user_version = 3` ⇒ 下次 `version <= before` 直接跳过，不会重放；
/// - 中途崩溃 ⇒ SQLite 的 DDL 是事务性的，列添加随 `ROLLBACK` 一起撤销，
///   且版本号仍停在 2 ⇒ 重放时列并不存在，`ADD COLUMN` 正常成功。
///
/// 两种情况都不会撞上"duplicate column name"。真正危险的是**在事务外**做
/// `ADD COLUMN` 再单独写版本号——那样崩溃会留下"有列但版本号没涨"的库。
const V3: &[&str] = &[
    // 成败：NULL = 本条不是模型调用（安全事件），1 = 成功，0 = 失败
    "ALTER TABLE audit_log ADD COLUMN ok INTEGER",
    // 失败原因：仅 ok = 0 时有值；不含代码原文与 API Key
    "ALTER TABLE audit_log ADD COLUMN error TEXT",
    // ── 存量回填 ────────────────────────────────────────────────
    // 只把"真正的模型调用"标为成功（见上方判据说明）。
    // 安全事件行保持 NULL，不参与回填。
    "UPDATE audit_log SET ok = 1 WHERE ok IS NULL AND model <> '-'",
];

/// 应用所有待执行迁移。
///
/// 整体包在单个事务里：任一语句失败即回滚，不会出现"迁移做了一半"的损坏库。
pub fn migrate(conn: &Connection) -> Result<MigrationOutcome, StorageError> {
    let before = current_version(conn)?;

    // 开启外键（pragma 不参与事务，需单独设置）
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| StorageError::sqlite("启用 foreign_keys", e))?;

    let mut applied: Vec<i32> = Vec::new();
    for (version, desc, statements) in MIGRATIONS {
        if *version <= before {
            continue;
        }
        conn.execute_batch("BEGIN")
            .map_err(|e| StorageError::sqlite("开始迁移事务", e))?;

        let result = apply_one(conn, *version, statements);
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT").map_err(|e| {
                    StorageError::Migration(format!("提交 v{version} 失败: {e}"))
                })?;
                applied.push(*version);
                tracing::info!(version, desc, "已应用数据库迁移");
            }
            Err(e) => {
                // 回滚失败时只能报告：此时库可能处于未知状态，但 WAL 下不会损坏
                let _ = conn.execute_batch("ROLLBACK");
                return Err(StorageError::Migration(format!(
                    "迁移到 v{version}（{desc}）失败: {e}"
                )));
            }
        }
    }

    Ok(MigrationOutcome {
        from: before,
        to: current_version(conn)?,
        applied,
    })
}

fn apply_one(conn: &Connection, version: i32, statements: &[&str]) -> Result<(), StorageError> {
    for (i, sql) in statements.iter().enumerate() {
        conn.execute_batch(sql).map_err(|e| {
            // 附带语句序号与片段，便于定位是哪条 DDL 出错
            let head: String = sql.split_whitespace().take(12).collect::<Vec<_>>().join(" ");
            StorageError::Migration(format!(
                "v{version} 第 #{i} 条语句失败: {e} | 语句: {head}…"
            ))
        })?;
    }
    // user_version 必须与迁移同事务提交，否则崩溃后版本号与实际结构不一致
    conn.pragma_update(None, "user_version", version)
        .map_err(|e| StorageError::sqlite("写入 user_version", e))?;
    Ok(())
}

/// 读取当前 schema 版本。
pub fn current_version(conn: &Connection) -> Result<i32, StorageError> {
    conn.pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
        .map_err(|e| StorageError::sqlite("读取 user_version", e))
}

/// 迁移结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    pub from: i32,
    pub to: i32,
    pub applied: Vec<i32>,
}

impl MigrationOutcome {
    pub fn is_noop(&self) -> bool {
        self.applied.is_empty()
    }
}

/// 校验 FTS5 虚表是否可用（trigram 需要 SQLite ≥ 3.34）。
///
/// 启动时调用：若不可用，检索层会降级为纯 LIKE，功能不中断但需告知用户。
pub fn verify_fts5(conn: &Connection) -> Result<bool, StorageError> {
    let r = conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS __fts_probe USING fts5(x, tokenize='trigram');
         DROP TABLE IF EXISTS __fts_probe;",
    );
    Ok(r.is_ok())
}

/// 数据库体积与行数统计（设置页「数据与隐私」展示真实占用，替代 mock 的 "214 MB"）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DbStats {
    /// 字节数
    pub size_bytes: u64,
    pub projects: usize,
    pub assets: usize,
    pub capabilities: usize,
    pub relations: usize,
    pub insights: usize,
    pub opportunities: usize,
    pub jobs: usize,
    pub activities: usize,
    pub audit_entries: usize,
}

/// 采集数据库统计。
pub fn collect_stats(conn: &Connection) -> Result<DbStats, StorageError> {
    let count = |table: &str| -> Result<usize, StorageError> {
        // 表名来自本模块常量而非用户输入，不存在注入风险；
        // 仍显式白名单校验，避免将来有人把外部字符串传进来。
        if !TABLE_WHITELIST.contains(&table) {
            return Err(StorageError::sqlite(
                "统计表名不在白名单内",
                format!("illegal table: {table}"),
            ));
        }
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite(format!("统计 {table}"), e))
    };

    // page_count * page_size = 文件字节数
    let pages: i64 = conn
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .map_err(|e| StorageError::sqlite("读取 page_count", e))?;
    let page_size: i64 = conn
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .map_err(|e| StorageError::sqlite("读取 page_size", e))?;

    Ok(DbStats {
        size_bytes: (pages.max(0) as u64).saturating_mul(page_size.max(0) as u64),
        projects: count("projects")?,
        assets: count("assets")?,
        capabilities: count("capabilities")?,
        relations: count("relations")?,
        insights: count("insights")?,
        opportunities: count("opportunities")?,
        jobs: count("jobs")?,
        activities: count("activities")?,
        audit_entries: count("audit_log")?,
    })
}

/// 允许统计的表名白名单。
const TABLE_WHITELIST: &[&str] = &[
    "projects",
    "assets",
    "capabilities",
    "relations",
    "insights",
    "opportunities",
    "jobs",
    "activities",
    "audit_log",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn fresh_db_starts_at_version_zero() {
        assert_eq!(current_version(&mem()).unwrap(), 0);
    }

    /// 全新库应一次跑完全部迁移。
    ///
    /// 🔴 断言用 `SCHEMA_VERSION` 而非字面量：
    /// 硬编码 `vec![1]` 的话，每加一版迁移都要回来改这条测试，
    /// 而改动者很可能只改数字、不去想"这一版到底建了什么"。
    /// 用常量表达"从 0 升到当前版本"这个真实意图，加迁移时无需改动。
    #[test]
    fn migrate_applies_all_versions_on_fresh_db() {
        let c = mem();
        let out = migrate(&c).unwrap();
        assert_eq!(out.from, 0);
        assert_eq!(out.to, projectassests_domain::SCHEMA_VERSION);
        // 版本号必须连续：1, 2, …, SCHEMA_VERSION
        let expected: Vec<i32> = (1..=projectassests_domain::SCHEMA_VERSION).collect();
        assert_eq!(out.applied, expected);
        assert!(!out.is_noop());
    }

    /// 二次迁移必须是空操作（幂等）：应用重启时不能重复建表或报错。
    #[test]
    fn migrate_is_idempotent() {
        let c = mem();
        migrate(&c).unwrap();
        let second = migrate(&c).unwrap();
        assert!(second.is_noop());
        assert_eq!(second.from, projectassests_domain::SCHEMA_VERSION);
        assert_eq!(second.to, projectassests_domain::SCHEMA_VERSION);
    }

    /// 🔴 回归：v1 老库升级到 v2 时，**存量洞察/机会必须被回填进 FTS 并可检索**。
    ///
    /// # 为什么这条测试不可省
    /// 全新库跑 `migrate` 时 `insights` 表是空的，V2 的回填 SQL 回填 0 行——
    /// 即使回填 SQL 写错（列名错、json_each 路径错、WHERE 反了），
    /// 全新库的端到端验证也**完全测不出来**（我第一轮就踩了这个盲区：
    /// 新库 insights_fts 行数与主表都=0，看着"一致"其实啥也没验）。
    ///
    /// 真实用户是从已发布的 v1 库升级，里面可能已有几百条洞察。
    /// 回填一旦坏了，老数据升级后**永远搜不到**，而增量写入的新洞察却能搜到——
    /// 这种"新数据正常、老数据丢失"的割裂最难排查，用户只会觉得"搜索时好时坏"。
    ///
    /// # 测试构造
    /// 1. 只应用 V1 → `user_version=1`，模拟已发布的 v1 库
    /// 2. 直接 INSERT 存量洞察/机会（含中文标题、tags、evidence、能力清单）
    /// 3. `migrate()` → 补上所有更高版本（含 V2），触发回填
    /// 4. 断言 FTS 行数与主表一致，且能按中文子串真实检索到
    #[test]
    fn v2_backfills_existing_rows_from_v1_database() {
        let c = mem();
        // 1. 只建 V1 结构，版本号停在 1（apply_one 会同事务写 user_version）
        apply_one(&c, 1, V1).unwrap();
        assert_eq!(current_version(&c).unwrap(), 1);

        // 2. 塞入存量数据：字段结构必须与 V1 建表一致
        //    洞察：中文标题 + tags + 两条证据（验证 json_each 展开 label）
        c.execute(
            "INSERT INTO insights
                (id, type, title, description, evidence_json, confidence,
                 tags_json, project_ids_json, asset_ids_json, created_at)
             VALUES
                ('ins_old', 'duplicate_capability', '你在两个项目里重复实现了任务队列',
                 '建议抽取为独立组件',
                 '[{\"kind\":\"file\",\"label\":\"src/legacy_queue.rs\",\"target\":null},
                   {\"kind\":\"file\",\"label\":\"src/old_retry.rs\",\"target\":null}]',
                 0.9, '[\"队列\",\"重构\"]', '[\"p1\"]', '[]', '2026-01-01')",
            [],
        )
        .expect("插入存量洞察失败");
        // 机会：中文标题 + 能力清单（required/missing 合并进 capabilities 列）
        c.execute(
            "INSERT INTO opportunities
                (id, title, description, source_project_ids_json, source_asset_ids_json,
                 required_capabilities_json, missing_capabilities_json,
                 coverage, rating, why, evidence_json, status, created_at)
             VALUES
                ('opp_old', '把历史队列能力拼成调度中台', '组合复用',
                 '[\"p1\"]', '[]', '[\"任务队列\"]', '[\"统一配置\"]',
                 0.6, 4, '两个项目能力重合', '[\"src/a.rs\"]', 'new', '2026-01-02')",
            [],
        )
        .expect("插入存量机会失败");

        // 迁移前 FTS 表还不存在，确认存量确实在主表里
        assert_eq!(
            c.query_row("SELECT count(*) FROM insights", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );

        // 3. 升级到最新：应补上所有高于 v1 的版本（含本测试关心的 V2）
        let out = migrate(&c).unwrap();
        assert_eq!(out.from, 1);
        // 🔴 断言用 `SCHEMA_VERSION` 而非字面量 2：
        // 写死"只到 v2"会让每次新增迁移都把这个测试弄坏，
        // 而它真正关心的是"V2 的回填在真实升级路径上生效"，不是版本号。
        assert_eq!(out.to, projectassests_domain::SCHEMA_VERSION);
        assert!(
            out.applied.contains(&2),
            "从 v1 升级必须应用 V2（回填的载体），实际应用 {:?}",
            out.applied
        );

        // 4a. FTS 行数与主表一致（回填不漏不重）
        let ins_fts = c
            .query_row("SELECT count(*) FROM insights_fts", [], |r| r.get::<_, i64>(0))
            .unwrap();
        assert_eq!(ins_fts, 1, "存量洞察未回填进 insights_fts");
        let opp_fts = c
            .query_row("SELECT count(*) FROM opportunities_fts", [], |r| r.get::<_, i64>(0))
            .unwrap();
        assert_eq!(opp_fts, 1, "存量机会未回填进 opportunities_fts");

        // 4b. 关键：回填的内容**真的可检索**（不是塞了空串）
        //     中文子串命中洞察标题
        let hit_title = c
            .query_row(
                "SELECT count(*) FROM insights_fts WHERE insights_fts MATCH '\"任务队列\"'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        assert!(hit_title >= 1, "回填后应能按中文子串检索到存量洞察标题");

        // 证据 label 也要可检索（验证 json_extract($.label) 那一段回填对了）
        let hit_evidence = c
            .query_row(
                "SELECT count(*) FROM insights_fts WHERE insights_fts MATCH '\"legacy_queue\"'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        assert!(hit_evidence >= 1, "证据 label 未被回填检索");

        // 机会的能力清单（来自 required_capabilities_json）可检索
        let hit_cap = c
            .query_row(
                "SELECT count(*) FROM opportunities_fts WHERE opportunities_fts MATCH '\"任务队列\"'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        assert!(hit_cap >= 1, "机会能力清单未被回填检索");

        // 4c. 回填是幂等的：再 migrate 一次不得重复插入
        let again = migrate(&c).unwrap();
        assert!(again.is_noop());
        let ins_fts2 = c
            .query_row("SELECT count(*) FROM insights_fts", [], |r| r.get::<_, i64>(0))
            .unwrap();
        assert_eq!(ins_fts2, 1, "回填不幂等，产生了重复索引行");
    }

    /// V3：`audit_log` 的存量行必须**分类**回填，不能一律填成功。
    ///
    /// # 🔴 这个测试守住的是语义，不只是"列加上了"
    /// v3 之前审计只在成功时写入，所以存量行分两类，回填规则相反：
    /// - 模型调用行（`model` = `cloud:x` / `local:x`）→ `ok = 1`（它们必然成功过）
    /// - 本地安全事件行（`model = '-'`）→ `ok = NULL`（不是调用，没有成败）
    ///
    /// 若图省事写成 `SET ok = 1`（无 WHERE），安全事件也会变成"成功"，
    /// 于是 UI 在「用户关闭了敏感项目仅本地约束」旁显示绿色对勾——
    /// 把一次**安全降级**渲染成"操作成功"。这是本迁移唯一的陷阱。
    #[test]
    fn v3_backfills_audit_ok_by_row_kind() {
        let c = mem();
        // 建到 v2（v3 之前），模拟一个已发布的老库
        apply_one(&c, 1, V1).unwrap();
        apply_one(&c, 2, V2).unwrap();
        assert_eq!(current_version(&c).unwrap(), 2);

        // 老库里的存量审计行：两类混在一起
        // 1) 成功的模型调用（老代码只在成功时写，所以存量必然是成功的）
        c.execute(
            "INSERT INTO audit_log (at, model, route, job_type, summary, project_id)
             VALUES ('2026-01-01','cloud:qwen-plus','cloud','ANALYZE_PROJECT','生成画像','p1')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO audit_log (at, model, route, job_type, summary, project_id)
             VALUES ('2026-01-02','local:qwen3:8b','local','ANALYZE_PROJECT','生成画像','p2')",
            [],
        )
        .unwrap();
        // 2) 本地安全事件（model = '-'）
        c.execute(
            "INSERT INTO audit_log (at, model, route, job_type, summary, project_id)
             VALUES ('2026-01-03','-','local','SETTINGS','用户关闭了「敏感项目仅本地」约束',NULL)",
            [],
        )
        .unwrap();

        // 迁移到最新：应补上 V3
        let out = migrate(&c).unwrap();
        assert!(out.applied.contains(&3), "应应用 V3，实际 {:?}", out.applied);

        // 🔴 模型调用行 → ok = 1
        let cloud_ok: Option<i32> = c
            .query_row(
                "SELECT ok FROM audit_log WHERE model='cloud:qwen-plus'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cloud_ok, Some(1), "存量的云端成功调用应回填为 ok=1");
        let local_ok: Option<i32> = c
            .query_row(
                "SELECT ok FROM audit_log WHERE model='local:qwen3:8b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(local_ok, Some(1), "存量的本地成功调用应回填为 ok=1");

        // 🔴 安全事件行 → ok 必须仍是 NULL，绝不能被填成 1
        let event_ok: Option<i32> = c
            .query_row(
                "SELECT ok FROM audit_log WHERE model='-'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            event_ok, None,
            "🔴 安全事件不是模型调用，ok 必须保持 NULL，否则 UI 会给安全降级打绿勾"
        );

        // error 列全部为 NULL（老库没有失败记录，也不该凭空造原因）
        let with_err: i64 = c
            .query_row(
                "SELECT count(*) FROM audit_log WHERE error IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(with_err, 0, "存量行不该有 error");

        // 幂等：再 migrate 一次不得改动已回填的值
        let again = migrate(&c).unwrap();
        assert!(again.is_noop(), "V3 应幂等");
    }

    /// 🔴 `has_successful_llm_call` 必须只认 `ok = 1`。
    ///
    /// 首页引导第 4 步用它判定"模型可用过"。若它把失败记录（`ok = 0`）
    /// 或安全事件（`ok IS NULL`）也算进去，一次 400 未开通的调用
    /// 就会把该步标成"已完成"——而那恰恰证明模型不可用。
    #[test]
    fn has_successful_llm_call_ignores_failed_and_event_rows() {
        use crate::Database;
        let d = Database::in_memory().unwrap();
        let s = d.settings();

        // 起点：没有任何记录
        assert!(!s.has_successful_llm_call().unwrap(), "空库应为 false");

        // 只有失败记录 → 仍为 false
        s.audit(&projectassests_domain::AuditEntry::llm_failed(
            "2026-01-01", "cloud:bad", projectassests_domain::RouteTarget::Cloud,
            "ANALYZE_PROJECT", "失败", None, "400",
        )).unwrap();
        assert!(
            !s.has_successful_llm_call().unwrap(),
            "🔴 只有失败记录时不得判定为'成功调用过'"
        );

        // 再加一条安全事件 → 仍为 false
        s.audit(&projectassests_domain::AuditEntry::event(
            "2026-01-02", "SETTINGS", "取消敏感标记", None,
        )).unwrap();
        assert!(
            !s.has_successful_llm_call().unwrap(),
            "安全事件不是模型调用，不得算成功"
        );

        // 出现一条成功记录 → 终于为 true
        s.audit(&projectassests_domain::AuditEntry::llm_ok(
            "2026-01-03", "cloud:good", projectassests_domain::RouteTarget::Cloud,
            "ANALYZE_PROJECT", "成功", None,
        )).unwrap();
        assert!(
            s.has_successful_llm_call().unwrap(),
            "有成功记录后应判定为可用过"
        );
    }

    /// 迁移语句本身也必须幂等（中断后重放安全）。
    #[test]
    fn reapplying_statements_does_not_fail() {
        let c = mem();
        migrate(&c).unwrap();
        // 手动重放全部 V1 语句：因为都带 IF NOT EXISTS，不应报错
        for sql in V1 {
            c.execute_batch(sql).expect("V1 语句必须幂等");
        }
    }

    #[test]
    fn all_eleven_core_tables_exist() {
        let c = mem();
        migrate(&c).unwrap();
        for t in [
            "projects",
            "assets",
            "capabilities",
            "relations",
            "insights",
            "opportunities",
            "jobs",
            "activities",
            "audit_log",
            "projects_fts",
            "assets_fts",
            "capabilities_fts",
            // v2 新增：洞察与机会的索引（对话式分析师要靠它们回答"有哪些重复实现"）
            "insights_fts",
            "opportunities_fts",
            "settings",
        ] {
            let n: i64 = c
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "缺少表 {t}");
        }
    }

    #[test]
    fn fts5_trigram_is_available() {
        let c = mem();
        assert!(verify_fts5(&c).unwrap());
    }

    /// trigram 对 ≥3 字中文查询有效；2 字需 LIKE 回退（检索层负责）。
    #[test]
    fn fts5_trigram_matches_chinese_substring() {
        let c = mem();
        migrate(&c).unwrap();
        c.execute(
            "INSERT INTO assets_fts(asset_id, name, description) VALUES('a1','视频管线','生成视频的完整流程')",
            [],
        )
        .unwrap();
        let three: i64 = c
            .query_row(
                "SELECT count(*) FROM assets_fts WHERE assets_fts MATCH '生成视频'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(three, 1, "4 字中文应命中");
        let two: i64 = c
            .query_row("SELECT count(*) FROM assets_fts WHERE assets_fts MATCH '视频'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(two, 0, "2 字查询 trigram 无法命中，须由检索层 LIKE 回退");
    }

    #[test]
    fn fts5_matches_english() {
        let c = mem();
        migrate(&c).unwrap();
        c.execute(
            "INSERT INTO projects_fts(project_id, name, description) VALUES('p1','video-tool','batch video pipeline')",
            [],
        )
        .unwrap();
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM projects_fts WHERE projects_fts MATCH 'pipeline'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    /// 外键级联删除：删项目必须连带删资产，否则会留下孤儿数据。
    #[test]
    fn deleting_project_cascades_to_assets() {
        let c = mem();
        migrate(&c).unwrap();
        c.execute("INSERT INTO projects(id, name, path) VALUES('p1','demo','/tmp/demo')", [])
            .unwrap();
        c.execute(
            "INSERT INTO assets(id, project_id, type, name, created_at) VALUES('a1','p1','code','Foo','2026-01-01')",
            [],
        )
        .unwrap();
        assert_eq!(count(&c, "assets"), 1);
        c.execute("DELETE FROM projects WHERE id='p1'", []).unwrap();
        assert_eq!(count(&c, "assets"), 0, "资产应随项目级联删除");
    }

    /// 能力表自引用级联：删 Domain 应带走子能力。
    #[test]
    fn deleting_domain_cascades_to_children() {
        let c = mem();
        migrate(&c).unwrap();
        c.execute(
            "INSERT INTO capabilities(id,name,layer) VALUES('d','AI','domain')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO capabilities(id,name,layer,parent_id) VALUES('c','RAG','capability','d')",
            [],
        )
        .unwrap();
        c.execute("DELETE FROM capabilities WHERE id='d'", []).unwrap();
        assert_eq!(count(&c, "capabilities"), 0);
    }

    /// path 唯一约束：同一目录不得重复登记（扫描重复执行的保护）。
    #[test]
    fn project_path_is_unique() {
        let c = mem();
        migrate(&c).unwrap();
        c.execute("INSERT INTO projects(id,name,path) VALUES('p1','a','/tmp/x')", [])
            .unwrap();
        let dup = c.execute("INSERT INTO projects(id,name,path) VALUES('p2','b','/tmp/x')", []);
        assert!(dup.is_err(), "重复 path 应被拒绝");
    }

    /// 同名能力在同一父节点下唯一：防止重复抽取导致标签爆炸。
    #[test]
    fn capability_name_unique_per_parent() {
        let c = mem();
        migrate(&c).unwrap();
        c.execute("INSERT INTO capabilities(id,name,layer) VALUES('d','AI','domain')", [])
            .unwrap();
        c.execute("INSERT INTO capabilities(id,name,layer,parent_id) VALUES('c1','RAG','capability','d')", [])
            .unwrap();
        let dup = c.execute(
            "INSERT INTO capabilities(id,name,layer,parent_id) VALUES('c2','RAG','capability','d')",
            [],
        );
        assert!(dup.is_err(), "同父同名能力应被拒绝");
        // 不同父节点下同名是允许的
        c.execute("INSERT INTO capabilities(id,name,layer) VALUES('d2','Web','domain')", [])
            .unwrap();
        c.execute("INSERT INTO capabilities(id,name,layer,parent_id) VALUES('c3','RAG','capability','d2')", [])
            .unwrap();
        assert_eq!(count(&c, "capabilities"), 4);
    }

    /// 关系表唯一约束：避免重复分析产生重复边。
    #[test]
    fn relation_is_unique_per_triple() {
        let c = mem();
        migrate(&c).unwrap();
        let sql = "INSERT INTO relations(id,source_id,source_type,relation_type,target_id,target_type) VALUES(?1,'p1','project','implements','c1','capability')";
        c.execute(sql, ["r1"]).unwrap();
        assert!(c.execute(sql, ["r2"]).is_err(), "同一三元组不应重复插入");
    }

    #[test]
    fn stats_reflect_real_row_counts() {
        let c = mem();
        migrate(&c).unwrap();
        c.execute("INSERT INTO projects(id,name,path) VALUES('p1','a','/tmp/a')", [])
            .unwrap();
        c.execute("INSERT INTO projects(id,name,path) VALUES('p2','b','/tmp/b')", [])
            .unwrap();
        c.execute(
            "INSERT INTO assets(id,project_id,type,name,created_at) VALUES('a1','p1','code','F','2026-01-01')",
            [],
        )
        .unwrap();
        let s = collect_stats(&c).unwrap();
        assert_eq!(s.projects, 2);
        assert_eq!(s.assets, 1);
        assert_eq!(s.capabilities, 0);
    }

    #[test]
    fn stats_reject_unknown_table() {
        let c = mem();
        migrate(&c).unwrap();
        // 直接调内部 count 逻辑不可行（闭包），改为验证白名单常量
        assert!(!TABLE_WHITELIST.contains(&"sqlite_master"));
        assert!(TABLE_WHITELIST.contains(&"projects"));
    }

    /// 迁移版本号必须与 domain 的 SCHEMA_VERSION 一致。
    /// 这是"加了迁移却忘记改常量"的护栏。
    #[test]
    fn migration_version_matches_domain_constant() {
        let max = MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0);
        assert_eq!(
            max,
            projectassests_domain::SCHEMA_VERSION,
            "MIGRATIONS 最大版本必须等于 projectassests_domain::SCHEMA_VERSION"
        );
        // 版本号必须严格递增且从 1 开始
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(m.0, (i + 1) as i32, "迁移版本必须连续递增");
        }
    }

    fn count(c: &Connection, table: &str) -> i64 {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}
