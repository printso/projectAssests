//! 项目 Repository。
//!
//! 职责边界：只负责 projects 表的读写与 FTS 同步。
//! 健康度/状态的计算规则在 `projectassests-scanner`（业务规则不进存储层）。

use rusqlite::{params, Connection, OptionalExtension};

use projectassests_domain::{
    CodeStats, LanguageShare, Project, ProjectAiProfile, ProjectStatus, ScanFacts, StorageError,
    SymbolStats,
};

use crate::err::sqlite_err;
use crate::pool::Pool;
use crate::row::{self, now_utc};

/// 项目列表筛选条件。
#[derive(Debug, Clone, Default)]
pub struct ProjectFilter {
    pub status: Option<ProjectStatus>,
    pub language: Option<String>,
    /// 名称/描述/标签的 LIKE 过滤（中文 2 字查询走这里，见 schema.rs 的 trigram 说明）
    pub keyword: Option<String>,
    pub sensitive: Option<bool>,
    pub limit: Option<u32>,
    pub offset: u32,
}

impl ProjectFilter {
    /// 是否无任何过滤（用于选择最简 SQL 路径）。
    pub fn is_unfiltered(&self) -> bool {
        self.status.is_none()
            && self.language.is_none()
            && self.keyword.is_none()
            && self.sensitive.is_none()
    }
}

/// 项目排序方式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProjectSort {
    /// 最近更新优先（默认，符合"我最近在做什么"的直觉）
    #[default]
    RecentlyUpdated,
    /// 健康度降序
    Health,
    /// 名称升序
    Name,
    /// 代码量降序
    Size,
}

impl ProjectSort {
    /// 转为 SQL ORDER BY 片段。
    ///
    /// 🔴 返回值是**硬编码白名单**，绝不拼接用户输入——这是 SQL 注入的唯一防线。
    pub fn order_by(&self) -> &'static str {
        match self {
            Self::RecentlyUpdated => "ORDER BY COALESCE(updated_at, '') DESC, name ASC",
            Self::Health => "ORDER BY health_score DESC, name ASC",
            Self::Name => "ORDER BY name COLLATE NOCASE ASC",
            Self::Size => "ORDER BY loc DESC, name ASC",
        }
    }
}

/// 项目仓储。无状态，可随用随建。
#[derive(Debug)]
pub struct ProjectRepo<'a> {
    pool: &'a Pool,
}

/// projects 表的完整列（顺序必须与 `map_project` 的索引一致）。
///
/// 集中定义为常量：新增列时只改这里 + map 函数，
/// 避免散落在十几条 SQL 里出现"某处漏了一列"的隐蔽 bug。
pub(crate) const COLS: &str = "id, name, path, description, language, framework, \
     created_at, updated_at, last_commit_at, status, health_score, completeness, \
     file_count, loc, symbol_count, module_count, languages_json, tags_json, \
     git_commits, has_git, has_readme, has_tests, sensitive, ai_profile_json, scanned_at";

impl<'a> ProjectRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// 插入或更新项目（按 id）。同时同步 FTS 索引。
    pub fn upsert(&self, p: &Project) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        let tx = conn.unchecked_transaction().map_err(|e| StorageError::sqlite("开启项目事务", e))?;
        Self::upsert_conn(&tx, p)?;
        tx.commit().map_err(|e| StorageError::sqlite("提交项目事务", e))
    }

    /// 批量写入。扫描完 100+ 项目时比逐条提交快一到两个数量级。
    ///
    /// 🔴 但**不是单事务**：走 `Pool::write_in_chunks` 分块提交。
    /// 816 个项目 = 816 行 projects + 816 行 projects_fts，
    /// 整批一个事务会在扫描一开始就把写锁占住，
    /// 而扫描正是用户最可能同时去改设置（加目录、调排除规则）的时刻。
    /// 分块后每块几十毫秒，块间隙能放行用户操作。
    /// 幂等 upsert，中途失败后重扫即可补齐。
    pub fn upsert_batch(&self, projects: &[Project]) -> Result<usize, StorageError> {
        self.pool
            .write_in_chunks("批量项目", projects, Self::upsert_conn)
    }

    fn upsert_conn(conn: &Connection, p: &Project) -> Result<(), StorageError> {
        let languages = row::to_json(&p.stats.languages)?;
        let tags = row::to_json(&p.tags)?;
        let ai_profile = row::to_json_opt(&p.ai_profile);

        // 列所有权划分（避免"两个写入者互相覆盖"）：
        //
        // | 列 | 由谁写 | 何时 |
        // |---|---|---|
        // | name/path/language/stats.files/loc/languages/tags | upsert | Level 0 静态扫描 |
        // | git_commits/has_*/scanned_at | update_scan_meta | Level 0 Git 分析 |
        // | symbol_count/module_count | update_scan_meta | Level 1 AST 解析 |
        // | ai_profile_json | set_ai_profile | Level 2 AI 画像 |
        // | sensitive | set_sensitive | 用户手动 |
        //
        // 机制：`ON CONFLICT DO UPDATE` **只更新 SET 列出的列**，未列出的一律保留原值。
        // 因此 git/符号/画像等列都不出现在 SET 子句里，天然不会被 upsert 覆盖；
        // INSERT 分支（新行）给它们填默认值即可，无需读回旧值。
        conn.execute(
            "INSERT INTO projects (
                id, name, path, description, language, framework,
                created_at, updated_at, last_commit_at, status, health_score, completeness,
                file_count, loc, languages_json, tags_json, sensitive, ai_profile_json
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, path=excluded.path, description=excluded.description,
                language=excluded.language, framework=excluded.framework,
                created_at=excluded.created_at, updated_at=excluded.updated_at,
                last_commit_at=excluded.last_commit_at, status=excluded.status,
                health_score=excluded.health_score, completeness=excluded.completeness,
                file_count=excluded.file_count, loc=excluded.loc,
                languages_json=excluded.languages_json, tags_json=excluded.tags_json,
                sensitive=excluded.sensitive, ai_profile_json=excluded.ai_profile_json",
            params![
                p.id,
                p.name,
                p.path,
                p.description,
                p.language,
                p.framework,
                p.created_at,
                p.updated_at,
                p.last_commit_at,
                p.status.as_str(),
                p.health_score,
                p.completeness,
                p.stats.files as i64,
                p.stats.loc as i64,
                languages,
                tags,
                p.sensitive,
                ai_profile,
            ],
        )
        .map_err(|e| StorageError::sqlite("写入 projects", e))?;

        Self::sync_fts(conn, p)
    }

    /// 同步 FTS 索引：先删后插（FTS5 虚表不支持 UPSERT）。
    fn sync_fts(conn: &Connection, p: &Project) -> Result<(), StorageError> {
        conn.execute("DELETE FROM projects_fts WHERE project_id = ?1", [&p.id])
            .map_err(|e| StorageError::sqlite("清理项目索引", e))?;
        conn.execute(
            "INSERT INTO projects_fts(project_id, name, description, tags, language, framework)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                p.id,
                p.name,
                p.description,
                p.tags.join(" "),
                p.language,
                p.framework
            ],
        )
        .map(|_| ())
        .map_err(|e| StorageError::sqlite("写入项目索引", e))
    }

    /// 按 id 查询。不存在返回 `Ok(None)`（不是错误）。
    pub fn get(&self, id: &str) -> Result<Option<Project>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM projects WHERE id = ?1");
        conn.query_row(&sql, [id], map_project)
            .optional()
            .map_err(|e| StorageError::sqlite("查询项目", e))
    }

    /// 按绝对路径查询（扫描时判断项目是否已存在）。
    pub fn get_by_path(&self, path: &str) -> Result<Option<Project>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM projects WHERE path = ?1");
        conn.query_row(&sql, [path], map_project)
            .optional()
            .map_err(|e| StorageError::sqlite("按路径查询项目", e))
    }

    /// 列出项目（带筛选与排序）。
    pub fn list(&self, filter: &ProjectFilter, sort: ProjectSort) -> Result<Vec<Project>, StorageError> {
        let conn = self.pool.get()?;
        let (where_sql, args) = build_where(filter);

        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let sql = format!("SELECT {COLS} FROM projects {where_sql} {} LIMIT ? OFFSET ?", sort.order_by());
        let limit = i64::from(filter.limit.unwrap_or(500).clamp(1, 1000));
        let offset = i64::from(filter.offset);

        let mut stmt = conn.prepare(&sql).map_err(|e| StorageError::sqlite("准备项目查询", e))?;
        // LIMIT/OFFSET 与筛选参数一起绑定
        let mut all: Vec<&dyn rusqlite::types::ToSql> = refs;
        all.push(&limit);
        all.push(&offset);

        let rows = stmt
            .query_map(all.as_slice(), map_project)
            .map_err(|e| StorageError::sqlite("执行项目查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射项目行", e))?);
        }
        Ok(out)
    }

    /// 项目总数（不受 limit 影响，用于"共 N 个项目"）。
    pub fn count(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM projects", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计项目数", e))
    }

    /// 满足筛选条件的项目数（分页 total）。
    ///
    /// 🔴 必须用真正的 `COUNT(*)`，且**忽略 limit/offset**：
    /// 早期实现是 `list(limit=1000, ..filter.clone())` 再取长度，而 `..filter`
    /// 把调用方的 `offset` 也带了进去——用户翻到第 2 页时 offset=3，
    /// 计数就变成 7，分页器总页数随之缩短，后面的页永远翻不到。
    /// 同时那种写法会把上千行实体全部读进内存只为数个数。
    pub fn count_filtered(&self, filter: &ProjectFilter) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        let (where_sql, args) = build_where(filter);
        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let sql = format!("SELECT count(*) FROM projects {where_sql}");
        conn.query_row(&sql, refs.as_slice(), |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计筛选后项目数", e))
    }

    /// 删除项目（级联删除其资产，由外键保证）。
    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn.unchecked_transaction().map_err(|e| StorageError::sqlite("开启删除事务", e))?;
        // ⚠️ FTS 先删（依赖主表定位）
        tx.execute("DELETE FROM projects_fts WHERE project_id = ?1", [id])
            .map_err(|e| StorageError::sqlite("清理项目索引", e))?;
        let n = tx
            .execute("DELETE FROM projects WHERE id = ?1", [id])
            .map_err(|e| StorageError::sqlite("删除项目", e))?;
        tx.commit().map_err(|e| StorageError::sqlite("提交删除事务", e))?;
        Ok(n > 0)
    }

    /// 标记/取消敏感项目（隐私开关，影响 LLM 路由）。
    ///
    /// 🔴 用 `sqlite_err`（能分类 `Busy`）而非 `StorageError::sqlite`：
    /// 这是**安全相关**的用户操作，且用户最可能在**索引进行中**才想到去标记
    /// （正好撞写锁）。若报成 500「数据库操作失败」，用户很可能就此放弃标记——
    /// 而项目未标敏感意味着它可能走云端路由，代码被送出本机。
    /// 分类成 Busy → 409「稍后重试」才能引导用户真的重试成功。
    pub fn set_sensitive(&self, id: &str, sensitive: bool) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let n = conn
            .execute(
                "UPDATE projects SET sensitive = ?2 WHERE id = ?1",
                params![id, if sensitive { 1 } else { 0 }],
            )
            .map_err(|e| sqlite_err("更新敏感标记", e))?;
        Ok(n > 0)
    }

    /// 写入 AI 画像（Level 2 分析完成后调用）。
    pub fn set_ai_profile(&self, id: &str, profile: &ProjectAiProfile) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let json = row::to_json(profile)?;
        let n = conn
            .execute(
                "UPDATE projects SET ai_profile_json = ?2 WHERE id = ?1",
                params![id, json],
            )
            .map_err(|e| StorageError::sqlite("写入项目画像", e))?;
        Ok(n > 0)
    }

    /// 更新 Level 0 扫描事实（Git 统计 + 检测标志 + 扫描时间）。
    ///
    /// 独立方法而非塞进 `upsert`：这些字段由扫描器填充，
    /// 而 `upsert` 的调用方（例如 AI 画像回写）并不持有它们。
    ///
    /// 🔴 只写 Level 0 的列。`symbol_count` / `module_count` 归
    /// [`Self::update_symbol_stats`]：两者生产者不同，
    /// 合在一个方法里会让后跑的那个用默认值清掉先跑的成果。
    pub fn update_scan_facts(&self, id: &str, facts: &ScanFacts) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let n = conn
            .execute(
                "UPDATE projects SET
                    git_commits = ?2, has_git = ?3, has_readme = ?4, has_tests = ?5,
                    scanned_at = ?6
                 WHERE id = ?1",
                params![
                    id,
                    facts.git_commits as i64,
                    i64::from(facts.has_git),
                    i64::from(facts.has_readme),
                    i64::from(facts.has_tests),
                    facts.scanned_at.clone().unwrap_or_else(now_utc),
                ],
            )
            .map_err(|e| StorageError::sqlite("更新扫描事实", e))?;
        Ok(n > 0)
    }

    /// 更新 Level 1 符号统计（符号数 + 模块数）。
    ///
    /// 与 [`Self::update_scan_facts`] 分开的理由见其文档注释。
    /// 符号抽取阶段调用本方法，不会碰到 Git 列。
    pub fn update_symbol_stats(&self, id: &str, stats: &SymbolStats) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let n = conn
            .execute(
                "UPDATE projects SET symbol_count = ?2, module_count = ?3 WHERE id = ?1",
                params![id, stats.symbol_count as i64, stats.module_count as i64],
            )
            .map_err(|e| StorageError::sqlite("更新符号统计", e))?;
        Ok(n > 0)
    }

    /// 语言分布聚合（首页统计卡与筛选下拉的真实来源）。
    pub fn language_distribution(&self) -> Result<Vec<(String, usize)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare("SELECT language, count(*) FROM projects WHERE language <> '' GROUP BY language ORDER BY count(*) DESC, language ASC")
            .map_err(|e| StorageError::sqlite("准备语言分布查询", e))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize)))
            .map_err(|e| StorageError::sqlite("执行语言分布查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射语言分布行", e))?);
        }
        Ok(out)
    }

    /// 状态分布（首页"活跃/暂停/归档"统计）。
    pub fn status_distribution(&self) -> Result<Vec<(String, usize)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare("SELECT status, count(*) FROM projects GROUP BY status ORDER BY count(*) DESC")
            .map_err(|e| StorageError::sqlite("准备状态分布查询", e))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize)))
            .map_err(|e| StorageError::sqlite("执行状态分布查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射状态分布行", e))?);
        }
        Ok(out)
    }

    /// 全库代码总量（LOC）与文件数，用于"当前占用"展示。
    ///
    /// ⚠️ 返回 `(LOC, 文件数)`，**不是**项目数。项目数请用 [`Self::count`]。
    /// 这个组合容易被误当成"项目数/已扫描数"，故在此显式标注。
    pub fn totals(&self) -> Result<(usize, usize), StorageError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT COALESCE(SUM(loc),0), COALESCE(SUM(file_count),0) FROM projects",
            [],
            |r| Ok((r.get::<_, i64>(0)?.max(0) as usize, r.get::<_, i64>(1)?.max(0) as usize)),
        )
        .map_err(|e| StorageError::sqlite("统计代码总量", e))
    }

    /// 已完成扫描的项目数（`scanned_at` 非空）。
    ///
    /// 与 [`Self::count`] 的区别：后者是库里的项目总数，
    /// 前者是"本轮/上轮扫描真正处理过的"。两者不等说明有历史遗留项目
    /// （目录已删或本轮未覆盖），首页据此提示用户数据可能不完整。
    pub fn count_scanned(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT count(*) FROM projects WHERE scanned_at IS NOT NULL AND scanned_at <> ''",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as usize)
        .map_err(|e| StorageError::sqlite("统计已扫描项目数", e))
    }

    /// 最近一次扫描时间（全库最大值）。
    ///
    /// 首页"数据新鲜度"的唯一来源。返回 `None` 表示从未扫描过——
    /// 此时前端应显示"尚未扫描"而不是编一个时间，
    /// 否则用户会以为数据是新的而不愿重扫。
    pub fn latest_scan_time(&self) -> Result<Option<String>, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT MAX(scanned_at) FROM projects WHERE scanned_at IS NOT NULL AND scanned_at <> ''",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .map_err(|e| StorageError::sqlite("查询最近扫描时间", e))
    }

    /// FTS 索引行数（诊断用：与 projects 行数不一致说明索引不同步）。
    pub fn fts_count(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM projects_fts", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计项目索引", e))
    }

    /// 重建全部项目的 FTS 索引（索引损坏或升级分词器后使用）。
    pub fn rebuild_fts(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn.unchecked_transaction().map_err(|e| StorageError::sqlite("开启索引重建事务", e))?;
        tx.execute_batch("DELETE FROM projects_fts")
            .map_err(|e| StorageError::sqlite("清空项目索引", e))?;
        let mut stmt = tx
            .prepare(&format!("SELECT {COLS} FROM projects"))
            .map_err(|e| StorageError::sqlite("准备索引重建查询", e))?;
        let rows = stmt
            .query_map([], map_project)
            .map_err(|e| StorageError::sqlite("执行索引重建查询", e))?;
        let mut n = 0;
        for r in rows {
            let p = r.map_err(|e| StorageError::sqlite("映射项目行", e))?;
            Self::sync_fts(&tx, &p)?;
            n += 1;
        }
        drop(stmt);
        tx.commit().map_err(|e| StorageError::sqlite("提交索引重建", e))?;
        Ok(n)
    }
}

// `ScanFacts` / `SymbolStats` 定义在 domain 层（它们描述项目属性而非存储细节），
// 本模块只负责写入对应的列。按生产者拆成两个结构体，理由见 domain::project。

/// 构造筛选用的 WHERE 片段与参数。
///
/// 🔴 `list` 与 `count_filtered` **必须共用**这一个实现：
/// 两处各写一遍 WHERE 的后果是列表与总数口径不一致——
/// 例如列表按语言过滤了、计数没过滤，用户就会看到
/// "共 128 个项目"却只列出 3 条，分页器页数也是错的。
///
/// 安全约定：只拼接**本模块生成**的 SQL 片段，用户输入一律走 `?` 参数绑定。
fn build_where(filter: &ProjectFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut where_sql = String::from("WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(s) = filter.status {
        where_sql.push_str(" AND status = ?");
        args.push(Box::new(s.as_str().to_string()));
    }
    if let Some(l) = &filter.language {
        where_sql.push_str(" AND language = ?");
        args.push(Box::new(l.clone()));
    }
    if let Some(b) = filter.sensitive {
        where_sql.push_str(" AND sensitive = ?");
        args.push(Box::new(if b { 1i64 } else { 0i64 }));
    }
    if let Some(k) = filter.keyword.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        // LIKE 子串匹配：中文短查询下比 FTS5 trigram 更可靠。
        // `ESCAPE '\'` 必须显式声明，否则 crate::fts::escape_like 写入的
        // 反斜杠不生效，用户搜 "100%" 会退化成通配符匹配（返回一堆无关项目）。
        where_sql.push_str(
            " AND (name LIKE ? ESCAPE '\\' OR description LIKE ? ESCAPE '\\'\
              OR tags_json LIKE ? ESCAPE '\\')",
        );
        let pat = crate::fts::like_pattern(k);
        args.push(Box::new(pat.clone()));
        args.push(Box::new(pat.clone()));
        args.push(Box::new(pat));
    }

    (where_sql, args)
}

/// 行 → Project。列索引必须与 `COLS` 严格对应。
pub(crate) fn map_project(r: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let status_str: String = r.get(9)?;
    Ok(Project {
        id: row::text(r, 0)?,
        name: row::text(r, 1)?,
        path: row::text(r, 2)?,
        description: row::text(r, 3)?,
        language: row::text(r, 4)?,
        framework: row::text(r, 5)?,
        created_at: row::text_opt(r, 6)?,
        updated_at: row::text_opt(r, 7)?,
        last_commit_at: row::text_opt(r, 8)?,
        status: ProjectStatus::parse(&status_str),
        health_score: row::u8_col(r, 10)?,
        completeness: row::real_opt(r, 11)?,
        stats: CodeStats {
            files: row::usize_col(r, 12)?,
            loc: row::usize_col(r, 13)?,
            symbols: row::usize_col(r, 14)?,
            modules: row::usize_col(r, 15)?,
            languages: row::json_col::<Vec<LanguageShare>>(r, 16)?,
        },
        tags: row::json_col::<Vec<String>>(r, 17)?,
        // 🔴 这四列 + scanned_at 曾被查出却直接丢弃，导致数据库里存着提交数、
        // 项目详情页却只能显示"未知"。列索引必须与 COLS 严格对应。
        scan: ScanFacts {
            git_commits: row::u32_col(r, 18)?,
            has_git: row::bool_col(r, 19)?,
            has_readme: row::bool_col(r, 20)?,
            has_tests: row::bool_col(r, 21)?,
            scanned_at: row::text_opt(r, 24)?,
        },
        sensitive: row::bool_col(r, 22)?,
        ai_profile: row::json_col::<Option<ProjectAiProfile>>(r, 23)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use projectassests_domain::ProjectHighlight;

    fn sample(id: &str, name: &str, path: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            description: format!("{name} 的描述"),
            language: "Python".into(),
            framework: "FastAPI".into(),
            created_at: Some("2024-01-01".into()),
            updated_at: Some("2025-06-01".into()),
            last_commit_at: Some("2025-06-01".into()),
            status: ProjectStatus::Active,
            health_score: 80,
            completeness: Some(0.7),
            tags: vec!["Python".into(), "Video".into()],
            sensitive: false,
            stats: CodeStats {
                files: 42,
                loc: 3000,
                symbols: 12,
                modules: 4,
                languages: vec![LanguageShare { name: "Python".into(), pct: 100, loc: 3000 }],
            },
            scan: projectassests_domain::ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    #[test]
    fn upsert_then_get_roundtrips() {
        let d = db();
        let p = sample("p1", "yingTech", "/tmp/yt");
        d.projects().upsert(&p).unwrap();
        let got = d.projects().get("p1").unwrap().unwrap();
        assert_eq!(got.name, "yingTech");
        assert_eq!(got.path, "/tmp/yt");
        assert_eq!(got.tags, vec!["Python".to_string(), "Video".to_string()]);
        assert_eq!(got.stats.files, 42);
        assert_eq!(got.stats.languages.len(), 1);
        assert_eq!(got.status, ProjectStatus::Active);
        assert!((got.completeness.unwrap() - 0.7).abs() < 1e-9);
        assert!(got.ai_profile.is_none());
    }

    #[test]
    fn get_missing_returns_none_not_error() {
        let d = db();
        assert!(d.projects().get("nope").unwrap().is_none());
    }

    /// upsert 必须是更新而非重复插入。
    #[test]
    fn upsert_updates_existing() {
        let d = db();
        let mut p = sample("p1", "old", "/tmp/a");
        d.projects().upsert(&p).unwrap();
        p.name = "new".into();
        p.health_score = 55;
        d.projects().upsert(&p).unwrap();
        assert_eq!(d.projects().count().unwrap(), 1);
        let got = d.projects().get("p1").unwrap().unwrap();
        assert_eq!(got.name, "new");
        assert_eq!(got.health_score, 55);
    }

    #[test]
    fn batch_upsert_writes_all() {
        let d = db();
        let ps: Vec<Project> = (0..50)
            .map(|i| sample(&format!("p{i}"), &format!("proj{i}"), &format!("/tmp/{i}")))
            .collect();
        assert_eq!(d.projects().upsert_batch(&ps).unwrap(), 50);
        assert_eq!(d.projects().count().unwrap(), 50);
        assert_eq!(d.projects().fts_count().unwrap(), 50, "批量写入也须同步 FTS");
    }

    #[test]
    fn empty_batch_is_noop() {
        let d = db();
        assert_eq!(d.projects().upsert_batch(&[]).unwrap(), 0);
    }

    #[test]
    fn get_by_path_works() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/unique")).unwrap();
        assert_eq!(d.projects().get_by_path("/tmp/unique").unwrap().unwrap().id, "p1");
        assert!(d.projects().get_by_path("/tmp/other").unwrap().is_none());
    }

    #[test]
    fn list_filters_by_status() {
        let d = db();
        let a = sample("p1", "a", "/tmp/a");
        let mut b = sample("p2", "b", "/tmp/b");
        b.status = ProjectStatus::Abandoned;
        d.projects().upsert_batch(&[a, b]).unwrap();
        let active = d.projects()
            .list(&ProjectFilter { status: Some(ProjectStatus::Active), ..Default::default() }, ProjectSort::Name)
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "p1");
    }

    #[test]
    fn list_filters_by_language() {
        let d = db();
        let a = sample("p1", "a", "/tmp/a");
        let mut b = sample("p2", "b", "/tmp/b");
        b.language = "Rust".into();
        d.projects().upsert_batch(&[a, b]).unwrap();
        let rust = d.projects()
            .list(&ProjectFilter { language: Some("Rust".into()), ..Default::default() }, ProjectSort::Name)
            .unwrap();
        assert_eq!(rust.len(), 1);
        assert_eq!(rust[0].id, "p2");
    }

    /// 中文关键词搜索：项目名/描述/标签都要能命中。
    #[test]
    fn list_keyword_matches_chinese_description() {
        let d = db();
        let mut a = sample("p1", "yingTech", "/tmp/a");
        a.description = "基于大模型的 AI 漫剧生成平台".into();
        let b = sample("p2", "other", "/tmp/b");
        d.projects().upsert_batch(&[a, b]).unwrap();

        let hits = d.projects()
            .list(&ProjectFilter { keyword: Some("漫剧".into()), ..Default::default() }, ProjectSort::Name)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "p1");
    }

    #[test]
    fn list_keyword_matches_name_and_tags() {
        let d = db();
        d.projects().upsert(&sample("p1", "VideoTool", "/tmp/a")).unwrap();
        assert_eq!(
            d.projects().list(&ProjectFilter { keyword: Some("video".into()), ..Default::default() }, ProjectSort::Name).unwrap().len(),
            1,
            "名称匹配应大小写不敏感"
        );
        assert_eq!(
            d.projects().list(&ProjectFilter { keyword: Some("Video".into()), ..Default::default() }, ProjectSort::Name).unwrap().len(),
            1,
            "标签也应参与匹配"
        );
    }

    /// 空白关键词应等同于不过滤（原型期 bug：空格导致零结果）。
    #[test]
    fn blank_keyword_returns_everything() {
        let d = db();
        d.projects().upsert_batch(&[sample("p1", "a", "/tmp/a"), sample("p2", "b", "/tmp/b")]).unwrap();
        let hits = d.projects()
            .list(&ProjectFilter { keyword: Some("   ".into()), ..Default::default() }, ProjectSort::Name)
            .unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn list_respects_limit_and_offset() {
        let d = db();
        let ps: Vec<Project> = (0..10)
            .map(|i| sample(&format!("p{i:02}"), &format!("proj{i:02}"), &format!("/tmp/{i}")))
            .collect();
        d.projects().upsert_batch(&ps).unwrap();
        let page = d.projects()
            .list(&ProjectFilter { limit: Some(3), offset: 3, ..Default::default() }, ProjectSort::Name)
            .unwrap();
        assert_eq!(page.len(), 3);
        assert_eq!(page[0].id, "p03");
    }

    /// 🔴 回归守护：`count_filtered` 必须忽略 limit/offset。
    ///
    /// 早期实现是 `list(limit=1000, ..filter.clone()).len()`，而 `..filter`
    /// 把调用方的 offset 一起带了进去：用户翻到第 2 页（offset=3）时
    /// total 从 10 变成 7，分页器总页数随之缩短，**后面的页永远翻不到**。
    /// 且那种写法会把上千行实体全读进内存只为数个数。
    #[test]
    fn count_filtered_ignores_limit_and_offset() {
        let d = db();
        let ps: Vec<Project> = (0..10)
            .map(|i| sample(&format!("p{i:02}"), &format!("proj{i:02}"), &format!("/tmp/{i}")))
            .collect();
        d.projects().upsert_batch(&ps).unwrap();

        // 三种不同的 limit/offset 组合必须给出同一个 total
        let unpaginated = d.projects().count_filtered(&ProjectFilter::default()).unwrap();
        let page1 = d
            .projects()
            .count_filtered(&ProjectFilter { limit: Some(3), offset: 0, ..Default::default() })
            .unwrap();
        let page2 = d
            .projects()
            .count_filtered(&ProjectFilter { limit: Some(3), offset: 3, ..Default::default() })
            .unwrap();
        let last_page = d
            .projects()
            .count_filtered(&ProjectFilter { limit: Some(3), offset: 9, ..Default::default() })
            .unwrap();

        assert_eq!(unpaginated, 10);
        assert_eq!(page1, 10, "第 1 页的 total 应为全量");
        assert_eq!(page2, 10, "第 2 页的 total 必须与第 1 页相同");
        assert_eq!(last_page, 10, "末页的 total 也不得缩短");
    }

    /// `list` 与 `count_filtered` 共用 `build_where`，筛选口径必须完全一致。
    /// 否则会出现"共 10 个项目"却只列出 3 条的自相矛盾。
    #[test]
    fn count_filtered_matches_list_filters() {
        let d = db();
        let mut a = sample("p1", "alpha", "/tmp/a");
        a.language = "Rust".into();
        a.status = ProjectStatus::Abandoned;
        let mut b = sample("p2", "beta", "/tmp/b");
        b.language = "Rust".into();
        let c = sample("p3", "gamma", "/tmp/c"); // Python
        d.projects().upsert_batch(&[a, b, c]).unwrap();

        // 逐个筛选维度都要对上
        let cases: Vec<ProjectFilter> = vec![
            ProjectFilter { language: Some("Rust".into()), ..Default::default() },
            ProjectFilter { status: Some(ProjectStatus::Abandoned), ..Default::default() },
            ProjectFilter { keyword: Some("alpha".into()), ..Default::default() },
            ProjectFilter {
                language: Some("Rust".into()),
                status: Some(ProjectStatus::Abandoned),
                ..Default::default()
            },
            ProjectFilter::default(),
        ];
        for filter in cases {
            let listed = d
                .projects()
                .list(&ProjectFilter { limit: Some(1000), ..filter.clone() }, ProjectSort::Name)
                .unwrap()
                .len();
            let counted = d.projects().count_filtered(&filter).unwrap();
            assert_eq!(
                listed, counted,
                "list 与 count_filtered 口径不一致（筛选: {filter:?}）"
            );
        }
    }

    /// 超过 1000 条时计数仍须准确（旧实现有 `limit: Some(1000)` 硬上限，会少算）。
    #[test]
    fn count_filtered_exceeds_internal_page_cap() {
        let d = db();
        let ps: Vec<Project> = (0..1200)
            .map(|i| sample(&format!("p{i:04}"), &format!("proj{i:04}"), &format!("/tmp/{i}")))
            .collect();
        d.projects().upsert_batch(&ps).unwrap();
        assert_eq!(
            d.projects().count_filtered(&ProjectFilter::default()).unwrap(),
            1200,
            "计数不得被内部页大小截断"
        );
        assert_eq!(d.projects().count().unwrap(), 1200);
    }

    #[test]
    fn count_filtered_on_empty_db_is_zero() {
        let d = db();
        assert_eq!(d.projects().count_filtered(&ProjectFilter::default()).unwrap(), 0);
        assert_eq!(
            d.projects()
                .count_filtered(&ProjectFilter { keyword: Some("anything".into()), ..Default::default() })
                .unwrap(),
            0
        );
    }

    /// LIKE 通配符在计数里也必须按字面处理（与 list 同一套转义）。
    #[test]
    fn count_filtered_escapes_like_wildcards() {
        let d = db();
        d.projects()
            .upsert_batch(&[
                sample("p1", "折扣 100% 覆盖", "/tmp/a"),
                sample("p2", "其它项目", "/tmp/b"),
            ])
            .unwrap();
        let n = d
            .projects()
            .count_filtered(&ProjectFilter { keyword: Some("100%".into()), ..Default::default() })
            .unwrap();
        assert_eq!(n, 1, "含 % 的查询应按字面计数，只命中 p1");
    }

    #[test]
    fn list_sort_orders() {
        let d = db();
        let mut a = sample("p1", "Beta", "/tmp/a");
        a.health_score = 30;
        a.stats.loc = 9000;
        let mut b = sample("p2", "alpha", "/tmp/b");
        b.health_score = 90;
        b.stats.loc = 100;
        d.projects().upsert_batch(&[a, b]).unwrap();

        let by_name = d.projects().list(&ProjectFilter::default(), ProjectSort::Name).unwrap();
        assert_eq!(by_name[0].name, "alpha", "名称升序且大小写不敏感");

        let by_health = d.projects().list(&ProjectFilter::default(), ProjectSort::Health).unwrap();
        assert_eq!(by_health[0].id, "p2");

        let by_size = d.projects().list(&ProjectFilter::default(), ProjectSort::Size).unwrap();
        assert_eq!(by_size[0].id, "p1");
    }

    /// ORDER BY 必须是白名单常量，不能来自用户输入。
    #[test]
    fn sort_order_by_is_hardcoded() {
        for s in [ProjectSort::RecentlyUpdated, ProjectSort::Health, ProjectSort::Name, ProjectSort::Size] {
            let sql = s.order_by();
            assert!(sql.starts_with("ORDER BY"));
            assert!(!sql.contains('?'), "不应含占位符: {sql}");
            assert!(!sql.contains(';'), "不应含分号（防注入）: {sql}");
        }
    }

    #[test]
    fn delete_removes_project_and_fts() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        assert!(d.projects().delete("p1").unwrap());
        assert!(d.projects().get("p1").unwrap().is_none());
        assert_eq!(d.projects().fts_count().unwrap(), 0);
        assert!(!d.projects().delete("p1").unwrap(), "重复删除应返回 false");
    }

    /// 删项目必须级联删资产（外键约束生效的前提是 foreign_keys=ON）。
    #[test]
    fn delete_cascades_to_assets() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        d.assets().upsert(&crate::tests::sample_asset("a1", "p1", "Foo")).unwrap();
        d.projects().delete("p1").unwrap();
        assert_eq!(d.assets().count_all().unwrap(), 0);
    }

    #[test]
    fn set_sensitive_toggles_flag() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        assert!(d.projects().set_sensitive("p1", true).unwrap());
        assert!(d.projects().get("p1").unwrap().unwrap().sensitive);
        d.projects().set_sensitive("p1", false).unwrap();
        assert!(!d.projects().get("p1").unwrap().unwrap().sensitive);
        assert!(!d.projects().set_sensitive("nope", true).unwrap());
    }

    #[test]
    fn list_filters_by_sensitive() {
        let d = db();
        d.projects().upsert_batch(&[sample("p1", "a", "/tmp/a"), sample("p2", "b", "/tmp/b")]).unwrap();
        d.projects().set_sensitive("p1", true).unwrap();
        let sens = d.projects()
            .list(&ProjectFilter { sensitive: Some(true), ..Default::default() }, ProjectSort::Name)
            .unwrap();
        assert_eq!(sens.len(), 1);
        assert_eq!(sens[0].id, "p1");
    }

    #[test]
    fn set_ai_profile_persists() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        let prof = ProjectAiProfile {
            summary: "AI 漫剧生成平台".into(),
            purpose: Some("自动化视频创作".into()),
            phase: Some("多镜头优化".into()),
            highlights: vec![ProjectHighlight {
                title: "多镜头生成".into(),
                desc: "支持脚本分镜".into(),
                evidence_files: vec!["services/video_service.py".into()],
            }],
            archaeology: None,
            generated_by: "qwen3:8b".into(),
            generated_at: now_utc(),
        };
        assert!(d.projects().set_ai_profile("p1", &prof).unwrap());
        let got = d.projects().get("p1").unwrap().unwrap();
        let p = got.ai_profile.expect("应写入画像");
        assert_eq!(p.summary, "AI 漫剧生成平台");
        assert_eq!(p.highlights.len(), 1);
        assert_eq!(p.highlights[0].evidence_files[0], "services/video_service.py");
        assert_eq!(p.generated_by, "qwen3:8b");
    }

    /// upsert 不得覆盖已有的扫描元数据（Level 0 与 Level 1 分阶段写入）。
    #[test]
    fn upsert_preserves_scan_meta() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        d.projects()
            .update_scan_facts(
                "p1",
                &ScanFacts {
                    git_commits: 87,
                    has_git: true,
                    has_readme: true,
                    has_tests: false,
                    scanned_at: Some("2026-09-29T10:00:00Z".into()),
                },
            )
            .unwrap();
        d.projects()
            .update_symbol_stats(
                "p1",
                &SymbolStats {
                    symbol_count: 72,
                    module_count: 13,
                },
            )
            .unwrap();

        // 再次 upsert（如更新描述）不应丢失 git 统计
        let mut p = sample("p1", "a", "/tmp/a");
        p.description = "更新后的描述".into();
        d.projects().upsert(&p).unwrap();

        let conn = d.conn().unwrap();
        let (commits, has_git, symbols): (i64, i64, i64) = conn
            .query_row("SELECT git_commits, has_git, symbol_count FROM projects WHERE id='p1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(commits, 87, "git_commits 应保留");
        assert_eq!(has_git, 1);
        assert_eq!(symbols, 72);
        assert_eq!(d.projects().get("p1").unwrap().unwrap().description, "更新后的描述");
    }

    /// 🔴 回归守护：Level 1 写符号统计不得清掉 Level 0 的 Git 事实。
    ///
    /// 这是拆分 `ScanMeta` 的**唯一理由**。拆分前两者共用一个写方法，
    /// Level 1 用 `..Default::default()` 构造就把 `git_commits`/`has_git` 归零，
    /// 项目状态推断随即退化（有 200 次提交的项目被判成"实验性"）。
    /// 该缺陷静默发生，只有跨阶段跑完整个 pipeline 才会暴露。
    #[test]
    fn symbol_stats_write_does_not_wipe_scan_facts() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        // Level 0 先写 Git 事实
        d.projects()
            .update_scan_facts(
                "p1",
                &ScanFacts {
                    git_commits: 240,
                    has_git: true,
                    has_readme: true,
                    has_tests: true,
                    scanned_at: Some("2026-09-29T10:00:00Z".into()),
                },
            )
            .unwrap();
        // Level 1 后写符号统计（用 Default 构造，正是当年的出错路径）
        d.projects()
            .update_symbol_stats("p1", &SymbolStats { symbol_count: 55, ..Default::default() })
            .unwrap();

        let conn = d.conn().unwrap();
        let (commits, has_git, has_tests, symbols, modules): (i64, i64, i64, i64, i64) = conn
            .query_row(
                "SELECT git_commits, has_git, has_tests, symbol_count, module_count
                 FROM projects WHERE id='p1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(commits, 240, "Level 1 写入不得清掉 git_commits");
        assert_eq!(has_git, 1, "Level 1 写入不得清掉 has_git");
        assert_eq!(has_tests, 1, "Level 1 写入不得清掉 has_tests");
        assert_eq!(symbols, 55, "符号数应已更新");
        assert_eq!(modules, 0, "未提供的模块数保持默认");
    }

    /// 反方向同样成立：Level 0 重扫不得清掉 Level 1 的符号统计。
    #[test]
    fn scan_facts_write_does_not_wipe_symbol_stats() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        d.projects()
            .update_symbol_stats(
                "p1",
                &SymbolStats {
                    symbol_count: 72,
                    module_count: 13,
                },
            )
            .unwrap();
        d.projects()
            .update_scan_facts(
                "p1",
                &ScanFacts {
                    git_commits: 5,
                    has_git: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let conn = d.conn().unwrap();
        let (symbols, modules): (i64, i64) = conn
            .query_row(
                "SELECT symbol_count, module_count FROM projects WHERE id='p1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(symbols, 72, "Level 0 重扫不得清掉 symbol_count");
        assert_eq!(modules, 13, "Level 0 重扫不得清掉 module_count");
    }

    /// 两个更新方法在项目不存在时都应返回 false 而非报错
    /// （项目可能在扫描后被用户删除）。
    #[test]
    fn updates_report_false_for_missing_project() {
        let d = db();
        assert!(!d.projects().update_scan_facts("ghost", &ScanFacts::default()).unwrap());
        assert!(!d.projects().update_symbol_stats("ghost", &SymbolStats::default()).unwrap());
    }

    /// `scanned_at` 为 None 时应由存储层填当前时间，不得写入 NULL。
    #[test]
    fn scanned_at_defaults_to_now_when_unspecified() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        d.projects()
            .update_scan_facts("p1", &ScanFacts { has_git: true, ..Default::default() })
            .unwrap();
        let conn = d.conn().unwrap();
        let scanned: String = conn
            .query_row("SELECT scanned_at FROM projects WHERE id='p1'", [], |r| r.get(0))
            .unwrap();
        assert!(!scanned.is_empty(), "scanned_at 不得为空");
        assert!(scanned.starts_with('2'), "应是 ISO 时间戳，实际 {scanned}");
    }

    #[test]
    fn language_distribution_aggregates() {
        let d = db();
        let a = sample("p1", "a", "/tmp/a");
        let mut b = sample("p2", "b", "/tmp/b");
        let mut c = sample("p3", "c", "/tmp/c");
        b.language = "Rust".into();
        c.language = "Rust".into();
        d.projects().upsert_batch(&[a, b, c]).unwrap();
        let dist = d.projects().language_distribution().unwrap();
        assert_eq!(dist[0], ("Rust".to_string(), 2));
        assert_eq!(dist[1], ("Python".to_string(), 1));
    }

    #[test]
    fn status_distribution_aggregates() {
        let d = db();
        let a = sample("p1", "a", "/tmp/a");
        let mut b = sample("p2", "b", "/tmp/b");
        b.status = ProjectStatus::Paused;
        d.projects().upsert_batch(&[a, b]).unwrap();
        let dist = d.projects().status_distribution().unwrap();
        assert!(dist.iter().any(|(s, n)| s == "active" && *n == 1));
        assert!(dist.iter().any(|(s, n)| s == "paused" && *n == 1));
    }

    #[test]
    fn totals_sum_loc_and_files() {
        let d = db();
        let mut a = sample("p1", "a", "/tmp/a");
        a.stats.loc = 1000;
        a.stats.files = 10;
        let mut b = sample("p2", "b", "/tmp/b");
        b.stats.loc = 2500;
        b.stats.files = 20;
        d.projects().upsert_batch(&[a, b]).unwrap();
        assert_eq!(d.projects().totals().unwrap(), (3500, 30));
    }

    #[test]
    fn totals_are_zero_on_empty_db() {
        assert_eq!(db().projects().totals().unwrap(), (0, 0));
    }

    /// 脏 JSON 不应让整个列表页 500（宽松解析策略）。
    #[test]
    fn corrupt_json_column_degrades_gracefully() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE projects SET tags_json = 'not-json{{' WHERE id='p1'", [])
            .unwrap();
        drop(conn);
        let got = d.projects().get("p1").unwrap().unwrap();
        assert!(got.tags.is_empty(), "坏 JSON 应降级为空数组而非报错");
        assert_eq!(got.name, "a");
    }

    #[test]
    fn unknown_status_degrades_to_unknown() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/a")).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE projects SET status='future-value' WHERE id='p1'", []).unwrap();
        drop(conn);
        assert_eq!(
            d.projects().get("p1").unwrap().unwrap().status,
            ProjectStatus::Unknown
        );
    }

    #[test]
    fn rebuild_fts_restores_index() {
        let d = db();
        d.projects().upsert_batch(&[sample("p1", "a", "/tmp/a"), sample("p2", "b", "/tmp/b")]).unwrap();
        // 模拟索引损坏
        let conn = d.conn().unwrap();
        conn.execute_batch("DELETE FROM projects_fts").unwrap();
        drop(conn);
        assert_eq!(d.projects().fts_count().unwrap(), 0);
        assert_eq!(d.projects().rebuild_fts().unwrap(), 2);
        assert_eq!(d.projects().fts_count().unwrap(), 2);
    }

    #[test]
    fn filter_is_unfiltered_detection() {
        assert!(ProjectFilter::default().is_unfiltered());
        assert!(!ProjectFilter { status: Some(ProjectStatus::Active), ..Default::default() }.is_unfiltered());
    }

    /// path 唯一约束：同一目录被重复扫描时 upsert 应更新而非报冲突。
    #[test]
    fn same_path_different_id_is_rejected() {
        let d = db();
        d.projects().upsert(&sample("p1", "a", "/tmp/same")).unwrap();
        let r = d.projects().upsert(&sample("p2", "b", "/tmp/same"));
        assert!(r.is_err(), "同路径不同 id 应被唯一约束拒绝");
    }
}
