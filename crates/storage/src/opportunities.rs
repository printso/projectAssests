//! 机会 Repository（历史 → 新项目的桥梁）。
//!
//! Dismiss 是**状态变更**而非删除：产品设计要求"Dismiss 全部后进入空状态"，
//! 但机会本身要留存以便审计与"恢复"。真正删除只在清理派生数据时发生。

use rusqlite::{params, Connection, OptionalExtension};

use projectassests_domain::{Opportunity, OpportunityAnalysis, OpportunityStatus, StorageError};

use crate::err::sqlite_err;
use crate::pool::Pool;
use crate::row;

/// 机会筛选。
#[derive(Debug, Clone, Default)]
pub struct OpportunityFilter {
    /// 🔴 `None` = **不加状态过滤**（返回全部状态），`Some(集合)` = 限定这些状态。
    ///
    /// 早期注释把 `None` 写成"仅可操作状态"，与实现相反：
    /// `build_where` 对 `None` 不拼 status 条件，`all_statuses()` 返回的正是 `None`。
    /// 注释错了不会编译失败，但会让调用方以为 default 就自带过滤，
    /// 于是首页把已忽略的机会也列了出来。"只看可操作"必须显式用 `actionable()`。
    ///
    /// `Some(空集合)` 是第三种语义：明确要"什么都不要"，直接返回空。
    pub statuses: Option<Vec<OpportunityStatus>>,
    pub min_rating: Option<u8>,
    pub limit: Option<u32>,
    pub offset: u32,
}

impl OpportunityFilter {
    /// 只看可操作的机会（首页与机会页的默认视图）。
    ///
    /// 状态集合取自 `OpportunityStatus::actionable()`：
    /// "哪些状态算可操作"是领域知识，新增状态时改那一处即可，
    /// 不必在 storage、service、前端各找一遍。
    pub fn actionable() -> Self {
        Self {
            statuses: Some(OpportunityStatus::actionable()),
            ..Default::default()
        }
    }

    /// 含已忽略的全部机会（设置/审计视图）。
    pub fn all_statuses() -> Self {
        Self::default()
    }
}

/// 机会仓储。
#[derive(Debug)]
pub struct OpportunityRepo<'a> {
    pool: &'a Pool,
}

pub(crate) const COLS: &str = "id, title, description, source_project_ids_json, source_asset_ids_json, \
     required_capabilities_json, missing_capabilities_json, coverage, rating, why, \
     evidence_json, status, created_at, analysis_json";

impl<'a> OpportunityRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    pub fn upsert(&self, o: &Opportunity) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        Self::upsert_conn(&conn, o)
    }

    /// 批量写入机会。
    ///
    /// 🔴 走 `Pool::write_in_chunks` 而非整批一个事务：理由见该方法文档
    /// （巨型事务长时间独占写锁，期间用户改设置必然超时失败）。
    pub fn upsert_batch(&self, ops: &[Opportunity]) -> Result<usize, StorageError> {
        self.pool
            .write_in_chunks("批量机会", ops, Self::upsert_conn)
    }

    fn upsert_conn(conn: &Connection, o: &Opportunity) -> Result<(), StorageError> {
        let src_p = row::to_json(&o.source_project_ids)?;
        let src_a = row::to_json(&o.source_asset_ids)?;
        let req = row::to_json(&o.required_capabilities)?;
        let miss = row::to_json(&o.missing_capabilities)?;
        let ev = row::to_json(&o.evidence)?;
        conn.execute(
            "INSERT INTO opportunities (
                id, title, description, source_project_ids_json, source_asset_ids_json,
                required_capabilities_json, missing_capabilities_json, coverage, rating,
                why, evidence_json, status, created_at, analysis_json
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13, NULL)
             ON CONFLICT(id) DO UPDATE SET
                title=excluded.title, description=excluded.description,
                source_project_ids_json=excluded.source_project_ids_json,
                source_asset_ids_json=excluded.source_asset_ids_json,
                required_capabilities_json=excluded.required_capabilities_json,
                missing_capabilities_json=excluded.missing_capabilities_json,
                coverage=excluded.coverage, rating=excluded.rating,
                why=excluded.why, evidence_json=excluded.evidence_json,
                created_at=excluded.created_at",
            params![
                o.id,
                o.title,
                o.description,
                src_p,
                src_a,
                req,
                miss,
                o.coverage,
                o.rating.clamp(1, 5),
                o.why,
                ev,
                o.status.as_str(),
                o.created_at,
            ],
        )
        .map(|_| ())
        .map_err(|e| StorageError::sqlite("写入 opportunities", e))?;
        // 注意：status 与 analysis_json 刻意不在 UPDATE 列表中。
        // 重新生成机会时，用户的 Dismiss 决定与已做的深入分析都不应被抹掉。
        Self::sync_fts(conn, o)
    }

    /// 同步 FTS 索引：先删后插。
    ///
    /// # 🔴 列集合必须与 schema 的 V2 回填 SQL 完全一致
    /// 回填服务老库升级，这里服务新数据；两边口径不同就会让
    /// 新旧机会的可检索内容不一致，且没有任何报错能提示。
    ///
    /// 对应关系（左＝本函数，右＝V2 迁移）：
    /// | 本函数 | V2 回填 |
    /// |---|---|
    /// | `o.title` | `o.title` |
    /// | `o.description` | `o.description` |
    /// | `o.why` | `o.why` |
    /// | `capability_text(o)` | `group_concat(required) ‖ ' ' ‖ group_concat(missing)` |
    /// | `o.evidence.join(" ")` | `group_concat(value,' ') FROM json_each(evidence_json)` |
    ///
    /// 🔴 `status` 与 `analysis_json` **刻意不索引**：
    /// status 是枚举值（new/dismissed/…），用它检索没有语义价值，
    /// 而 `set_status` 频繁触发会导致每次处置都重建一遍索引；
    /// analysis 是 LLM 生成的长文本，索引它会让"搜标题"混进一堆正文噪音。
    /// 因此这两个字段变更时不需要重新同步索引。
    fn sync_fts(conn: &Connection, o: &Opportunity) -> Result<(), StorageError> {
        conn.execute(
            "DELETE FROM opportunities_fts WHERE opportunity_id = ?1",
            [&o.id],
        )
        .map_err(|e| StorageError::sqlite("清理机会索引", e))?;
        conn.execute(
            "INSERT INTO opportunities_fts(
                 opportunity_id, title, description, why, capabilities, evidence)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                o.id,
                o.title,
                o.description,
                o.why,
                capability_text(o),
                o.evidence.join(" "),
            ],
        )
        .map(|_| ())
        .map_err(|e| StorageError::sqlite("写入机会索引", e))
    }

    pub fn get(&self, id: &str) -> Result<Option<Opportunity>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM opportunities WHERE id = ?1");
        conn.query_row(&sql, [id], map_opportunity)
            .optional()
            .map_err(|e| StorageError::sqlite("查询机会", e))
    }

    pub fn list(&self, filter: &OpportunityFilter) -> Result<Vec<Opportunity>, StorageError> {
        // None = 筛选条件本身已决定"什么都查不到"（见 build_where）
        let Some((where_sql, mut args)) = build_where(filter) else {
            return Ok(Vec::new());
        };
        let conn = self.pool.get()?;

        let limit = i64::from(filter.limit.unwrap_or(50).clamp(1, 200));
        let offset = i64::from(filter.offset);
        args.push(Box::new(limit));
        args.push(Box::new(offset));

        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        // 排序：星级降序 + 覆盖度降序（最值得做的排最前）
        let sql = format!(
            "SELECT {COLS} FROM opportunities {where_sql}
             ORDER BY rating DESC, coverage DESC, created_at DESC LIMIT ? OFFSET ?"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::sqlite("准备机会查询", e))?;
        let rows = stmt
            .query_map(refs.as_slice(), map_opportunity)
            .map_err(|e| StorageError::sqlite("执行机会查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射机会行", e))?);
        }
        Ok(out)
    }

    /// 统计满足筛选条件的机会总数（**忽略 `limit` / `offset`**）。
    ///
    /// 🔴 分页 `total` 必须来自这里。`count()` 是全表计数、
    /// `count_actionable()` 是固定的"new + explored"口径，
    /// 两者都不能替代"用户当前筛选条件下有多少条"。
    pub fn count_filtered(&self, filter: &OpportunityFilter) -> Result<usize, StorageError> {
        let Some((where_sql, args)) = build_where(filter) else {
            return Ok(0);
        };
        let conn = self.pool.get()?;
        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let sql = format!("SELECT count(*) FROM opportunities {where_sql}");
        conn.query_row(&sql, refs.as_slice(), |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计筛选后机会数", e))
    }

    pub fn count(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM opportunities", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计机会数", e))
    }

    /// 可操作机会数（侧栏/首页角标）。
    ///
    /// 🔴 状态集合由 `OpportunityStatus::actionable()` 生成，
    /// 不在 SQL 里硬编码 `'new','explored'`：角标数字必须与
    /// `list(OpportunityFilter::actionable())` 的条数一致，
    /// 两处各写一遍的话新增状态时必然漏改一处。
    pub fn count_actionable(&self) -> Result<usize, StorageError> {
        self.count_filtered(&OpportunityFilter {
            statuses: Some(OpportunityStatus::actionable()),
            ..Default::default()
        })
    }

    /// 状态分布（机会页头部统计）。
    pub fn count_by_status(&self) -> Result<Vec<(OpportunityStatus, usize)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare("SELECT status, count(*) FROM opportunities GROUP BY status")
            .map_err(|e| StorageError::sqlite("准备机会状态统计", e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize))
            })
            .map_err(|e| StorageError::sqlite("执行机会状态统计", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (s, n) = r.map_err(|e| StorageError::sqlite("映射机会状态统计行", e))?;
            if let Some(st) = OpportunityStatus::parse(&s) {
                out.push((st, n));
            }
        }
        Ok(out)
    }

    /// 更新状态（Dismiss / Explore / Adopt）。
    ///
    /// 🔴 用户可触发的写，可能在索引期间发生 → 用 `sqlite_err` 分类 `Busy`，
    /// 让上层回 409 可重试，而不是 500（用户会以为"忽略"操作没生效）。
    pub fn set_status(&self, id: &str, status: OpportunityStatus) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let n = conn
            .execute(
                "UPDATE opportunities SET status = ?2 WHERE id = ?1",
                params![id, status.as_str()],
            )
            .map_err(|e| sqlite_err("更新机会状态", e))?;
        Ok(n > 0)
    }

    /// Dismiss 全部可操作机会（机会页的批量操作）。返回受影响条数。
    pub fn dismiss_all(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.execute(
            "UPDATE opportunities SET status = 'dismissed' WHERE status IN ('new','explored')",
            [],
        )
        // 🔴 用户在机会页点「全部忽略」触发；索引期间可能撞写锁 → 分类 Busy 可重试
        .map_err(|e| sqlite_err("批量忽略机会", e))
    }

    /// 保存"深入分析"结果（只在用户点击后生成一次，不重复烧 token）。
    ///
    /// 🔴 这条写尤其不能因为撞锁而失败：分析结果来自一次**已消耗的模型调用**
    /// （可能已烧了云端 token）。若写入报 500 且不重试，用户既看不到结果，
    /// 再点一次又要重新调用模型、重复付费。Busy → 409 让前端重试同一份结果即可。
    pub fn set_analysis(&self, id: &str, analysis: &OpportunityAnalysis) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let json = row::to_json(analysis)?;
        let n = conn
            .execute(
                "UPDATE opportunities SET analysis_json = ?2, status = CASE
                    WHEN status = 'new' THEN 'explored' ELSE status END
                 WHERE id = ?1",
                params![id, json],
            )
            .map_err(|e| sqlite_err("写入机会分析", e))?;
        Ok(n > 0)
    }

    /// 读取"深入分析"结果。
    ///
    /// 注意两层 `Option`：外层来自 `.optional()`（行不存在），内层来自列本身可为 NULL。
    /// 只处理外层会在"行存在但未分析"时抛 `Invalid column type Null`。
    pub fn get_analysis(&self, id: &str) -> Result<Option<OpportunityAnalysis>, StorageError> {
        let conn = self.pool.get()?;
        let raw: Option<Option<String>> = conn
            .query_row(
                "SELECT analysis_json FROM opportunities WHERE id = ?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|e| StorageError::sqlite("查询机会分析", e))?;
        let Some(raw) = raw.flatten().filter(|s| !s.trim().is_empty()) else {
            return Ok(None);
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(StorageError::Serde)
    }

    /// 所有**已有深入分析**的机会 id。
    ///
    /// 🔴 存在这个方法是为了避免列表页的 N+1：
    /// 卡片要显示"已分析"角标，若逐条调 `get_analysis` 就得反序列化整个 JSON，
    /// 20 张卡 = 20 次查询 + 20 次解析，而列表只需要一个"有没有"的布尔。
    /// 一次查询取回 id 集合，调用方用 `contains` 判断。
    pub fn ids_with_analysis(&self) -> Result<Vec<String>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare(
                "SELECT id FROM opportunities
                 WHERE analysis_json IS NOT NULL AND trim(analysis_json) <> ''",
            )
            .map_err(|e| StorageError::sqlite("准备已分析机会查询", e))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| StorageError::sqlite("查询已分析机会", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射机会 id", e))?);
        }
        Ok(out)
    }

    /// 删除单条机会（真正删除只发生在"清理派生数据"时；
    /// 用户的 Dismiss 是状态变更，不走这里）。
    ///
    /// 🔴 必须同事务清掉 FTS 索引行，否则搜索仍会命中已删除的机会，
    /// 用户点进去 404，而没有任何报错提示索引与主表已不一致。
    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启删除机会事务", e))?;
        tx.execute("DELETE FROM opportunities_fts WHERE opportunity_id = ?1", [id])
            .map_err(|e| StorageError::sqlite("清理机会索引", e))?;
        let n = tx
            .execute("DELETE FROM opportunities WHERE id = ?1", [id])
            .map_err(|e| StorageError::sqlite("删除机会", e))?;
        tx.commit()
            .map_err(|e| StorageError::sqlite("提交删除机会事务", e))?;
        Ok(n > 0)
    }
}

/// 把"已具备 + 缺失"两份能力清单压成一个可检索串。
///
/// 🔴 与 schema V2 回填 SQL 的
/// `group_concat(required) || ' ' || group_concat(missing)` 一一对应。
/// 两份清单合并检索是刻意的：用户搜"任务队列"时，
/// 无论它出现在"已具备"还是"缺失"清单里都应命中——
/// 用户关心的是"有没有跟任务队列相关的机会"，不关心它在哪一栏。
fn capability_text(o: &Opportunity) -> String {
    o.required_capabilities
        .iter()
        .chain(o.missing_capabilities.iter())
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 由筛选条件构造 WHERE 子句与绑定参数（不含分页）。
///
/// `list` 与 `count_filtered` 共用，保证条目与总数口径一致。
///
/// 返回 `None` 表示"筛选条件本身已排除全部记录"——
/// `statuses` 是**空集合**时语义为"明确要什么都不要"，而不是"不加状态过滤"。
/// 这个区别若被写成 `Some("WHERE 1=1")`，空集合就会退化成返回全部机会，
/// 用户取消所有状态勾选后反而看到满屏数据。
fn build_where(filter: &OpportunityFilter) -> Option<(String, Vec<Box<dyn rusqlite::types::ToSql>>)> {
    let mut where_sql = String::from("WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(statuses) = &filter.statuses {
        if statuses.is_empty() {
            return None;
        }
        let placeholders = vec!["?"; statuses.len()].join(",");
        where_sql.push_str(&format!(" AND status IN ({placeholders})"));
        for s in statuses {
            args.push(Box::new(s.as_str().to_string()));
        }
    }

    if let Some(min) = filter.min_rating {
        where_sql.push_str(" AND rating >= ?");
        args.push(Box::new(i64::from(min)));
    }

    Some((where_sql, args))
}

pub(crate) fn map_opportunity(r: &rusqlite::Row<'_>) -> rusqlite::Result<Opportunity> {
    let status_str: String = r.get(11)?;
    Ok(Opportunity {
        id: row::text(r, 0)?,
        title: row::text(r, 1)?,
        description: row::text(r, 2)?,
        source_project_ids: row::json_col::<Vec<String>>(r, 3)?,
        source_asset_ids: row::json_col::<Vec<String>>(r, 4)?,
        required_capabilities: row::json_col::<Vec<String>>(r, 5)?,
        missing_capabilities: row::json_col::<Vec<String>>(r, 6)?,
        coverage: row::real(r, 7)?,
        rating: row::u8_col(r, 8)?,
        why: row::text(r, 9)?,
        evidence: row::json_col::<Vec<String>>(r, 10)?,
        status: OpportunityStatus::parse(&status_str).unwrap_or_default(),
        created_at: row::text(r, 12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use projectassests_domain::ReusableItem;

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    fn opp(id: &str, rating: u8, coverage: f64) -> Opportunity {
        Opportunity {
            id: id.into(),
            title: format!("机会 {id}"),
            description: "基于已有能力组合的新产品".into(),
            source_project_ids: vec!["p1".into(), "p2".into()],
            source_asset_ids: vec!["a1".into()],
            required_capabilities: vec!["AI Video".into(), "Agent".into()],
            missing_capabilities: vec!["Publishing".into()],
            coverage,
            rating,
            why: "2 个历史项目存在能力重合".into(),
            evidence: vec!["p1/src/a.py".into()],
            status: OpportunityStatus::New,
            created_at: "2026-09-26".into(),
        }
    }

    fn analysis(id: &str) -> OpportunityAnalysis {
        OpportunityAnalysis {
            opportunity_id: id.into(),
            rationale: "两个项目的能力互补".into(),
            reusable: vec![ReusableItem {
                asset_id: "a1".into(),
                name: "VideoPipeline".into(),
                project_id: "p1".into(),
                source_path: "src/pipeline.py".into(),
                reuse_score: 0.91,
                migration_note: "可直接复制".into(),
            }],
            to_build: vec!["Publishing".into()],
            mvp_suggestion: "先做批量生成 + 本地预览".into(),
            scaffold: vec!["src/pipeline/".into(), "src/publish/".into()],
        }
    }

    #[test]
    fn upsert_and_get_roundtrips() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 5, 0.78)).unwrap();
        let o = d.opportunities().get("o1").unwrap().unwrap();
        assert_eq!(o.title, "机会 o1");
        assert_eq!(o.rating, 5);
        assert!((o.coverage - 0.78).abs() < 1e-9);
        assert_eq!(o.source_project_ids.len(), 2);
        assert_eq!(o.missing_capabilities, vec!["Publishing".to_string()]);
        assert_eq!(o.status, OpportunityStatus::New);
        assert_eq!(o.evidence, vec!["p1/src/a.py".to_string()]);
    }

    /// 星级必须落在 1-5，脏数据不得流到前端渲染出 6 颗星（或 0 颗）。
    #[test]
    fn rating_is_clamped() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 9, 0.5)).unwrap();
        assert_eq!(d.opportunities().get("o1").unwrap().unwrap().rating, 5);
        d.opportunities().upsert(&opp("o2", 0, 0.5)).unwrap();
        assert_eq!(d.opportunities().get("o2").unwrap().unwrap().rating, 1, "0 星应抬到下限 1");
    }

    /// 重新生成机会不得抹掉用户的 Dismiss 决定。
    #[test]
    fn reupsert_preserves_status() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 4, 0.6)).unwrap();
        d.opportunities().set_status("o1", OpportunityStatus::Dismissed).unwrap();
        // 引擎重新生成同一机会（内容略有变化）
        let mut again = opp("o1", 5, 0.9);
        again.description = "更新后的描述".into();
        d.opportunities().upsert(&again).unwrap();

        let o = d.opportunities().get("o1").unwrap().unwrap();
        assert_eq!(o.status, OpportunityStatus::Dismissed, "Dismiss 决定必须保留");
        assert_eq!(o.description, "更新后的描述", "内容应更新");
        assert_eq!(o.rating, 5);
    }

    #[test]
    fn batch_upsert_writes_all() {
        let d = db();
        let list: Vec<Opportunity> = (0..8).map(|i| opp(&format!("o{i}"), 4, 0.5)).collect();
        assert_eq!(d.opportunities().upsert_batch(&list).unwrap(), 8);
        assert_eq!(d.opportunities().count().unwrap(), 8);
    }

    #[test]
    fn empty_batch_is_noop() {
        assert_eq!(db().opportunities().upsert_batch(&[]).unwrap(), 0);
    }

    /// 默认视图只展示可操作机会：Dismissed 不该再出现在首页。
    #[test]
    fn actionable_filter_excludes_dismissed() {
        let d = db();
        d.opportunities().upsert_batch(&[opp("o1", 5, 0.8), opp("o2", 4, 0.6), opp("o3", 3, 0.4)]).unwrap();
        d.opportunities().set_status("o2", OpportunityStatus::Dismissed).unwrap();

        let actionable = d.opportunities().list(&OpportunityFilter::actionable()).unwrap();
        assert_eq!(actionable.len(), 2);
        assert!(actionable.iter().all(|o| o.status != OpportunityStatus::Dismissed));

        let all = d.opportunities().list(&OpportunityFilter::all_statuses()).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn empty_status_list_returns_nothing() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 5, 0.8)).unwrap();
        let hits = d.opportunities()
            .list(&OpportunityFilter { statuses: Some(vec![]), ..Default::default() })
            .unwrap();
        assert!(hits.is_empty(), "空集合应返回空而非全部");
    }

    #[test]
    fn list_ordered_by_rating_then_coverage() {
        let d = db();
        d.opportunities().upsert_batch(&[
            opp("o1", 3, 0.9),
            opp("o2", 5, 0.5),
            opp("o3", 5, 0.95),
        ]).unwrap();
        let list = d.opportunities().list(&OpportunityFilter::all_statuses()).unwrap();
        assert_eq!(list[0].id, "o3", "5 星且覆盖度最高应排最前");
        assert_eq!(list[1].id, "o2");
        assert_eq!(list[2].id, "o1");
    }

    #[test]
    fn min_rating_filter() {
        let d = db();
        d.opportunities().upsert_batch(&[opp("o1", 5, 0.9), opp("o2", 2, 0.9)]).unwrap();
        let hits = d.opportunities()
            .list(&OpportunityFilter { min_rating: Some(4), ..Default::default() })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "o1");
    }

    #[test]
    fn count_actionable_tracks_status() {
        let d = db();
        d.opportunities().upsert_batch(&[opp("o1", 5, 0.9), opp("o2", 4, 0.8)]).unwrap();
        assert_eq!(d.opportunities().count_actionable().unwrap(), 2);
        d.opportunities().set_status("o1", OpportunityStatus::Dismissed).unwrap();
        assert_eq!(d.opportunities().count_actionable().unwrap(), 1);
    }

    #[test]
    fn count_by_status_aggregates() {
        let d = db();
        d.opportunities().upsert_batch(&[opp("o1", 5, 0.9), opp("o2", 4, 0.8)]).unwrap();
        d.opportunities().set_status("o1", OpportunityStatus::Dismissed).unwrap();
        let dist = d.opportunities().count_by_status().unwrap();
        assert!(dist.iter().any(|(s, n)| *s == OpportunityStatus::Dismissed && *n == 1));
        assert!(dist.iter().any(|(s, n)| *s == OpportunityStatus::New && *n == 1));
    }

    /// 机会页批量 Dismiss 后应进入空状态（原型的既定行为）。
    #[test]
    fn dismiss_all_clears_actionable() {
        let d = db();
        d.opportunities().upsert_batch(&[opp("o1", 5, 0.9), opp("o2", 4, 0.8)]).unwrap();
        assert_eq!(d.opportunities().dismiss_all().unwrap(), 2);
        assert_eq!(d.opportunities().count_actionable().unwrap(), 0);
        assert_eq!(d.opportunities().count().unwrap(), 2, "记录仍保留以便审计");
    }

    #[test]
    fn dismiss_all_is_noop_when_empty() {
        assert_eq!(db().opportunities().dismiss_all().unwrap(), 0);
    }

    /// 深入分析应把 new 提升为 explored（原型里状态从不变化）。
    #[test]
    fn set_analysis_marks_explored() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 5, 0.9)).unwrap();
        assert!(d.opportunities().set_analysis("o1", &analysis("o1")).unwrap());
        let o = d.opportunities().get("o1").unwrap().unwrap();
        assert_eq!(o.status, OpportunityStatus::Explored);
    }

    #[test]
    fn set_analysis_does_not_resurrect_dismissed() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 5, 0.9)).unwrap();
        d.opportunities().set_status("o1", OpportunityStatus::Dismissed).unwrap();
        d.opportunities().set_analysis("o1", &analysis("o1")).unwrap();
        assert_eq!(
            d.opportunities().get("o1").unwrap().unwrap().status,
            OpportunityStatus::Dismissed,
            "已忽略的机会不应被分析操作复活"
        );
    }

    #[test]
    fn analysis_roundtrips() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 5, 0.9)).unwrap();
        assert!(d.opportunities().get_analysis("o1").unwrap().is_none(), "初始无分析");
        d.opportunities().set_analysis("o1", &analysis("o1")).unwrap();
        let a = d.opportunities().get_analysis("o1").unwrap().unwrap();
        assert_eq!(a.rationale, "两个项目的能力互补");
        assert_eq!(a.reusable.len(), 1);
        assert_eq!(a.reusable[0].name, "VideoPipeline");
        assert_eq!(a.reusable[0].migration_note, "可直接复制");
        assert_eq!(a.to_build, vec!["Publishing".to_string()]);
        assert_eq!(a.scaffold.len(), 2);
    }

    #[test]
    fn get_analysis_missing_opportunity_returns_none() {
        assert!(db().opportunities().get_analysis("nope").unwrap().is_none());
    }

    #[test]
    fn corrupt_analysis_json_returns_error_not_panic() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 5, 0.9)).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE opportunities SET analysis_json='{{bad' WHERE id='o1'", []).unwrap();
        drop(conn);
        assert!(d.opportunities().get_analysis("o1").is_err());
        // 机会本身仍可读
        assert!(d.opportunities().get("o1").unwrap().is_some());
    }

    #[test]
    fn delete_removes_opportunity() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 5, 0.9)).unwrap();
        assert!(d.opportunities().delete("o1").unwrap());
        assert!(!d.opportunities().delete("o1").unwrap());
        assert_eq!(d.opportunities().count().unwrap(), 0);
    }

    #[test]
    fn set_status_on_missing_returns_false() {
        assert!(!db().opportunities().set_status("nope", OpportunityStatus::Adopted).unwrap());
    }

    #[test]
    fn unknown_status_degrades_to_new() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 5, 0.9)).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE opportunities SET status='future' WHERE id='o1'", []).unwrap();
        drop(conn);
        assert_eq!(d.opportunities().get("o1").unwrap().unwrap().status, OpportunityStatus::New);
    }

    #[test]
    fn corrupt_json_arrays_degrade_to_empty() {
        let d = db();
        d.opportunities().upsert(&opp("o1", 5, 0.9)).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE opportunities SET source_project_ids_json='{{bad' WHERE id='o1'", []).unwrap();
        drop(conn);
        let o = d.opportunities().get("o1").unwrap().unwrap();
        assert!(o.source_project_ids.is_empty());
        assert_eq!(o.title, "机会 o1");
    }

    #[test]
    fn list_pagination() {
        let d = db();
        let list: Vec<Opportunity> = (0..10).map(|i| opp(&format!("o{i}"), 3, 0.5)).collect();
        d.opportunities().upsert_batch(&list).unwrap();
        let page = d.opportunities()
            .list(&OpportunityFilter { limit: Some(3), offset: 3, ..Default::default() })
            .unwrap();
        assert_eq!(page.len(), 3);
    }
}
