//! 洞察 Repository。
//!
//! 产品纪律在此落地：`upsert` 前调用 `Insight::validate()`，
//! **无证据或低置信度的洞察不允许入库**——从存储层杜绝"AI 玄学结论"。

use rusqlite::{params, Connection, OptionalExtension};

use projectassests_domain::{
    EvidenceItem, EvidenceKind, Insight, InsightType, StorageError, UserFeedback,
};

use crate::err::sqlite_err;
use crate::pool::Pool;
use crate::row;

/// 洞察筛选。
#[derive(Debug, Clone, Default)]
pub struct InsightFilter {
    pub insight_type: Option<InsightType>,
    pub types: Vec<InsightType>,
    /// `Some(false)` = 只看未读（首页"新发现"）
    pub unread_only: Option<bool>,
    pub min_confidence: Option<f64>,
    pub limit: Option<u32>,
    pub offset: u32,
}

/// 洞察仓储。
#[derive(Debug)]
pub struct InsightRepo<'a> {
    pool: &'a Pool,
}

pub(crate) const COLS: &str = "id, type, title, description, evidence_json, confidence, tags_json, \
     project_ids_json, asset_ids_json, created_at, user_feedback";

impl<'a> InsightRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// 写入洞察。
    ///
    /// 返回 `Ok(true)` 表示已写入，`Ok(false)` 表示**被产品红线拒绝**（无证据/低置信度）。
    /// 刻意不返回 Err：洞察引擎批量生成时，一条不合格不应中断整批。
    pub fn upsert(&self, i: &Insight) -> Result<bool, StorageError> {
        if let Err(e) = i.validate() {
            tracing::info!(insight = %i.title, reason = %e, "洞察未通过产品红线校验，已跳过");
            return Ok(false);
        }
        let conn = self.pool.get()?;
        Self::upsert_conn(&conn, i)?;
        Ok(true)
    }

    /// 批量写入，返回**实际入库**的条数（被拒绝的不计入）。
    ///
    /// 🔴 校验过滤提到分块**之前**：`write_in_chunks` 返回的是它真正写入的行数。
    /// 若把 `validate()` 塞进闭包里 continue 跳过，"被拒绝的不计入"这个承诺
    /// 就无从兑现——返回值会把被跳过的也算进去，上层拿到虚高的数字。
    /// 先过滤、再分块，两者口径才一致。
    ///
    /// 🔴 走 `Pool::write_in_chunks` 而非整批一个事务：理由见该方法文档
    /// （巨型事务长时间独占写锁，期间用户改设置必然超时失败）。
    pub fn upsert_batch(&self, insights: &[Insight]) -> Result<usize, StorageError> {
        let valid: Vec<&Insight> = insights
            .iter()
            .filter(|i| match i.validate() {
                Ok(()) => true,
                Err(e) => {
                    tracing::info!(insight = %i.title, reason = %e, "洞察未通过校验，已跳过");
                    false
                }
            })
            .collect();
        self.pool
            .write_in_chunks("批量洞察", &valid, |conn, i| Self::upsert_conn(conn, i))
    }

    fn upsert_conn(conn: &Connection, i: &Insight) -> Result<(), StorageError> {
        let evidence = row::to_json(&i.evidence)?;
        let tags = row::to_json(&i.tags)?;
        let projects = row::to_json(&i.related_project_ids)?;
        let assets = row::to_json(&i.related_asset_ids)?;
        conn.execute(
            "INSERT INTO insights (
                id, type, title, description, evidence_json, confidence,
                tags_json, project_ids_json, asset_ids_json, created_at, user_feedback
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(id) DO UPDATE SET
                type=excluded.type, title=excluded.title, description=excluded.description,
                evidence_json=excluded.evidence_json, confidence=excluded.confidence,
                tags_json=excluded.tags_json, project_ids_json=excluded.project_ids_json,
                asset_ids_json=excluded.asset_ids_json, created_at=excluded.created_at",
            params![
                i.id,
                i.insight_type.as_str(),
                i.title,
                i.description,
                evidence,
                i.confidence,
                tags,
                projects,
                assets,
                i.created_at,
                i.user_feedback.map(|f| f.as_str()),
            ],
        )
        .map(|_| ())
        .map_err(|e| StorageError::sqlite("写入 insights", e))?;
        Self::sync_fts(conn, i)
    }

    /// 同步 FTS 索引：先删后插（与 assets/capabilities 同一模式）。
    ///
    /// # 🔴 列集合必须与 schema 的 V2 回填 SQL 完全一致
    /// 回填是一次性 SQL（服务老库升级），这里是增量写入（服务新数据）。
    /// 两边列若不同，就会出现"旧洞察搜得到某字段、新洞察搜不到"，
    /// 表现为同一查询的召回质量随数据新旧而异——这类缺陷没有任何报错，
    /// 只能靠用户发现"为什么我搜不到上周那条洞察"。
    ///
    /// 对应关系（左＝本函数，右＝V2 迁移）：
    /// | 本函数 | V2 回填 |
    /// |---|---|
    /// | `i.title` | `i.title` |
    /// | `i.description` | `i.description` |
    /// | `i.tags.join(" ")` | `group_concat(value,' ') FROM json_each(tags_json)` |
    /// | `i.insight_type.as_str()` | `i.type` |
    /// | `evidence_labels(&i.evidence)` | `group_concat(json_extract(value,'$.label'),' ') FROM json_each(evidence_json)` |
    ///
    /// 证据只索引 `label`（文件名/项目名），不索引 `target`：
    /// 后者是 `project_id:相对路径` 形式，含 id 哈希，索引它只会引入噪音。
    fn sync_fts(conn: &Connection, i: &Insight) -> Result<(), StorageError> {
        conn.execute("DELETE FROM insights_fts WHERE insight_id = ?1", [&i.id])
            .map_err(|e| StorageError::sqlite("清理洞察索引", e))?;
        conn.execute(
            "INSERT INTO insights_fts(insight_id, title, description, tags, insight_type, evidence)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                i.id,
                i.title,
                i.description,
                i.tags.join(" "),
                i.insight_type.as_str(),
                evidence_labels(&i.evidence),
            ],
        )
        .map(|_| ())
        .map_err(|e| StorageError::sqlite("写入洞察索引", e))
    }

    pub fn get(&self, id: &str) -> Result<Option<Insight>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM insights WHERE id = ?1");
        conn.query_row(&sql, [id], map_insight)
            .optional()
            .map_err(|e| StorageError::sqlite("查询洞察", e))
    }

    pub fn list(&self, filter: &InsightFilter) -> Result<Vec<Insight>, StorageError> {
        let conn = self.pool.get()?;
        let (where_sql, mut args) = build_where(filter);

        let limit = i64::from(filter.limit.unwrap_or(100).clamp(1, 500));
        let offset = i64::from(filter.offset);
        args.push(Box::new(limit));
        args.push(Box::new(offset));

        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        // 排序：置信度降序 + 新优先。洞察的价值在"可信且新鲜"
        let sql = format!(
            "SELECT {COLS} FROM insights {where_sql} ORDER BY confidence DESC, created_at DESC LIMIT ? OFFSET ?"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::sqlite("准备洞察查询", e))?;
        let rows = stmt
            .query_map(refs.as_slice(), map_insight)
            .map_err(|e| StorageError::sqlite("执行洞察查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射洞察行", e))?);
        }
        Ok(out)
    }

    /// 统计满足筛选条件的洞察总数（**忽略 `limit` / `offset`**）。
    ///
    /// 🔴 分页 `total` 必须来自这里而非 `list(...).len()`：后者最多返回一页，
    /// 前端总页数会随翻页缩短（同一缺陷曾出现在 projects 上）。
    /// 与 `list` 共用 `build_where` 保证口径一致。
    ///
    /// `count()` 统计的是全表，本方法统计的是"筛选后"，二者不可互相替代。
    pub fn count_filtered(&self, filter: &InsightFilter) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        let (where_sql, args) = build_where(filter);
        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let sql = format!("SELECT count(*) FROM insights {where_sql}");
        conn.query_row(&sql, refs.as_slice(), |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计筛选后洞察数", e))
    }

    pub fn count(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM insights", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计洞察数", e))
    }

    /// 未读洞察数（侧栏「洞察」计数与首页红点）。
    pub fn count_unread(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT count(*) FROM insights WHERE user_feedback IS NULL",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as usize)
        .map_err(|e| StorageError::sqlite("统计未读洞察", e))
    }

    pub fn count_by_type(&self) -> Result<Vec<(InsightType, usize)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare("SELECT type, count(*) FROM insights GROUP BY type")
            .map_err(|e| StorageError::sqlite("准备洞察类型统计", e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize))
            })
            .map_err(|e| StorageError::sqlite("执行洞察类型统计", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (t, n) = r.map_err(|e| StorageError::sqlite("映射洞察类型统计行", e))?;
            if let Some(ty) = InsightType::parse(&t) {
                out.push((ty, n));
            }
        }
        Ok(out)
    }

    /// 记录用户反馈（《产品设计书》V0.3 功能 17：反馈回流）。
    pub fn set_feedback(&self, id: &str, fb: Option<UserFeedback>) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let n = conn
            .execute(
                "UPDATE insights SET user_feedback = ?2 WHERE id = ?1",
                params![id, fb.map(|f| f.as_str().to_string())],
            )
            // 🔴 用户可触发的写（洞察的「有用/无用/忽略」）。
            // 与资产反馈同理：分类 Busy → 409 可重试，避免用户误以为标注丢失。
            .map_err(|e| sqlite_err("写入洞察反馈", e))?;
        Ok(n > 0)
    }

    /// 采纳率（《产品设计书》阶段二关键指标：标记有用 / 总展示 ≥ 40%）。
    ///
    /// 返回 `(useful, total_rated)`；`total_rated` 为 0 时调用方应显示"暂无反馈"而非 0%。
    pub fn adoption_rate(&self) -> Result<(usize, usize), StorageError> {
        let conn = self.pool.get()?;
        let useful: i64 = conn
            .query_row(
                "SELECT count(*) FROM insights WHERE user_feedback = 'useful'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| StorageError::sqlite("统计有用洞察", e))?;
        let total: i64 = conn
            .query_row(
                "SELECT count(*) FROM insights WHERE user_feedback IN ('useful','useless')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| StorageError::sqlite("统计已评洞察", e))?;
        Ok((useful.max(0) as usize, total.max(0) as usize))
    }

    /// 删除单条洞察。
    ///
    /// 🔴 必须同事务清掉 FTS 索引行：
    /// 主表删了、索引留着，搜索仍会命中这条洞察，
    /// 用户点进去却 404——而且没有任何报错能提示索引与主表已经不一致。
    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启删除洞察事务", e))?;
        // ⚠️ FTS 先删（与 assets 的删除顺序一致）
        tx.execute("DELETE FROM insights_fts WHERE insight_id = ?1", [id])
            .map_err(|e| StorageError::sqlite("清理洞察索引", e))?;
        let n = tx
            .execute("DELETE FROM insights WHERE id = ?1", [id])
            .map_err(|e| StorageError::sqlite("删除洞察", e))?;
        tx.commit()
            .map_err(|e| StorageError::sqlite("提交删除洞察事务", e))?;
        Ok(n > 0)
    }

    /// 删除某类型的全部洞察（重新生成前清理，避免旧洞察与新数据矛盾）。
    ///
    /// 🔴 同 `delete`：索引必须一起清，否则旧洞察仍会被搜到。
    pub fn delete_by_type(&self, t: InsightType) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启按类型删除事务", e))?;
        tx.execute(
            "DELETE FROM insights_fts
             WHERE insight_id IN (SELECT id FROM insights WHERE type = ?1)",
            [t.as_str()],
        )
        .map_err(|e| StorageError::sqlite("按类型清理洞察索引", e))?;
        let n = tx
            .execute("DELETE FROM insights WHERE type = ?1", [t.as_str()])
            .map_err(|e| StorageError::sqlite("按类型删除洞察", e))?;
        tx.commit()
            .map_err(|e| StorageError::sqlite("提交按类型删除事务", e))?;
        Ok(n)
    }
}

/// 把证据列表压成可检索的空格分隔串（只取 `label`）。
///
/// 🔴 与 schema V2 的回填 SQL 一一对应：那边用
/// `group_concat(json_extract(value,'$.label'),' ') FROM json_each(evidence_json)`。
/// 两边口径不同就会让新旧数据的可检索内容不一致。
///
/// 不索引 `target`（`project_id:相对路径`）：含 id 哈希，
/// 索引它只会让用户搜一串十六进制时命中一堆无关洞察。
fn evidence_labels(evidence: &[EvidenceItem]) -> String {
    evidence
        .iter()
        .map(|e| e.label.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 由筛选条件构造 WHERE 子句与绑定参数（不含分页）。
///
/// `list` 与 `count_filtered` 共用，保证"看到的条目"与"报告的总数"口径一致。
/// 两处各写一遍是缺陷来源：改了过滤条件却漏改统计，分页总数会与实际条目错位。
fn build_where(filter: &InsightFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut where_sql = String::from("WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    let mut types = filter.types.clone();
    if let Some(t) = filter.insight_type
        && !types.contains(&t)
    {
        types.push(t);
    }
    if !types.is_empty() {
        let placeholders = vec!["?"; types.len()].join(",");
        where_sql.push_str(&format!(" AND type IN ({placeholders})"));
        for t in types {
            args.push(Box::new(t.as_str().to_string()));
        }
    }

    if let Some(unread) = filter.unread_only {
        where_sql.push_str(if unread {
            " AND user_feedback IS NULL"
        } else {
            " AND user_feedback IS NOT NULL"
        });
    }

    if let Some(min) = filter.min_confidence {
        where_sql.push_str(" AND confidence >= ?");
        args.push(Box::new(min));
    }

    (where_sql, args)
}

pub(crate) fn map_insight(r: &rusqlite::Row<'_>) -> rusqlite::Result<Insight> {
    let type_str: String = r.get(1)?;
    let fb: Option<String> = r.get(10)?;
    // 证据反序列化失败时给一条占位证据而非空数组：
    // 空证据的洞察会被 validate() 拒绝，导致"能写入却读不出"的诡异现象。
    let evidence: Vec<EvidenceItem> = row::json_col(r, 4)?;
    let evidence = if evidence.is_empty() {
        vec![EvidenceItem {
            kind: EvidenceKind::File,
            label: "（证据数据损坏，请重新分析）".into(),
            target: None,
        }]
    } else {
        evidence
    };
    Ok(Insight {
        id: row::text(r, 0)?,
        insight_type: InsightType::parse(&type_str).unwrap_or(InsightType::ReusableComponent),
        title: row::text(r, 2)?,
        description: row::text(r, 3)?,
        evidence,
        confidence: row::real(r, 5)?,
        tags: row::json_col::<Vec<String>>(r, 6)?,
        related_project_ids: row::json_col::<Vec<String>>(r, 7)?,
        related_asset_ids: row::json_col::<Vec<String>>(r, 8)?,
        created_at: row::text(r, 9)?,
        user_feedback: fb.as_deref().and_then(UserFeedback::parse),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::CONFIDENCE_THRESHOLD;

    fn db() -> Database {
        Database::in_memory().unwrap()
    }
    use crate::Database;

    fn insight(id: &str, t: InsightType, conf: f64) -> Insight {
        Insight {
            id: id.into(),
            insight_type: t,
            title: "重复实现的能力".into(),
            description: "你在 3 个项目中实现了相似的图片处理 Pipeline".into(),
            evidence: vec![
                EvidenceItem {
                    kind: EvidenceKind::File,
                    label: "image-tool/pipeline.py".into(),
                    target: Some("p1:image-tool/pipeline.py".into()),
                },
                EvidenceItem {
                    kind: EvidenceKind::Project,
                    label: "yingTech".into(),
                    target: Some("p2".into()),
                },
            ],
            confidence: conf,
            created_at: "2026-09-26".into(),
            user_feedback: None,
            tags: vec!["图片处理".into()],
            related_project_ids: vec!["p1".into(), "p2".into()],
            related_asset_ids: vec!["a1".into()],
        }
    }

    #[test]
    fn upsert_and_get_roundtrips() {
        let d = db();
        assert!(d.insights().upsert(&insight("i1", InsightType::DuplicateCapability, 0.91)).unwrap());
        let i = d.insights().get("i1").unwrap().unwrap();
        assert_eq!(i.title, "重复实现的能力");
        assert_eq!(i.insight_type, InsightType::DuplicateCapability);
        assert_eq!(i.evidence.len(), 2);
        assert_eq!(i.evidence[0].kind, EvidenceKind::File);
        assert_eq!(i.evidence[1].target.as_deref(), Some("p2"));
        assert_eq!(i.related_project_ids, vec!["p1".to_string(), "p2".to_string()]);
        assert_eq!(i.tags, vec!["图片处理".to_string()]);
        assert!(i.user_feedback.is_none());
    }

    /// 产品红线：无证据的洞察不得入库，但也不应报错中断整批。
    #[test]
    fn insight_without_evidence_is_rejected_silently() {
        let d = db();
        let mut i = insight("i1", InsightType::DuplicateCapability, 0.91);
        i.evidence.clear();
        assert!(!d.insights().upsert(&i).unwrap(), "应返回 false 而非 Err");
        assert!(d.insights().get("i1").unwrap().is_none());
        assert_eq!(d.insights().count().unwrap(), 0);
    }

    #[test]
    fn low_confidence_insight_is_rejected() {
        let d = db();
        let i = insight("i1", InsightType::DuplicateCapability, CONFIDENCE_THRESHOLD - 0.01);
        assert!(!d.insights().upsert(&i).unwrap());
        assert_eq!(d.insights().count().unwrap(), 0);
    }

    /// 批量写入：不合格的跳过，合格的入库，返回真实入库数。
    #[test]
    fn batch_upsert_skips_invalid_and_reports_count() {
        let d = db();
        let mut no_ev = insight("i2", InsightType::ReusableComponent, 0.9);
        no_ev.evidence.clear();
        let low = insight("i3", InsightType::ForgottenAsset, 0.1);
        let written = d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.91),
            no_ev,
            low,
            insight("i4", InsightType::TechDirection, 0.8),
        ]).unwrap();
        assert_eq!(written, 2, "只应写入 2 条合格的");
        assert_eq!(d.insights().count().unwrap(), 2);
    }

    #[test]
    fn empty_batch_is_noop() {
        assert_eq!(db().insights().upsert_batch(&[]).unwrap(), 0);
    }

    #[test]
    fn list_filters_by_type() {
        let d = db();
        d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.91),
            insight("i2", InsightType::ForgottenAsset, 0.8),
            insight("i3", InsightType::DuplicateCapability, 0.85),
        ]).unwrap();
        let hits = d.insights().list(&InsightFilter {
            insight_type: Some(InsightType::DuplicateCapability),
            ..Default::default()
        }).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn list_filters_by_multiple_types() {
        let d = db();
        d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.91),
            insight("i2", InsightType::ForgottenAsset, 0.8),
            insight("i3", InsightType::TechDirection, 0.85),
        ]).unwrap();
        let hits = d.insights().list(&InsightFilter {
            types: vec![InsightType::DuplicateCapability, InsightType::TechDirection],
            ..Default::default()
        }).unwrap();
        assert_eq!(hits.len(), 2);
    }

    /// 首页"新发现"只展示未读，这是 unread_only 的用途。
    #[test]
    fn list_unread_only() {
        let d = db();
        d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.91),
            insight("i2", InsightType::ForgottenAsset, 0.8),
        ]).unwrap();
        d.insights().set_feedback("i1", Some(UserFeedback::Useful)).unwrap();
        let unread = d.insights().list(&InsightFilter { unread_only: Some(true), ..Default::default() }).unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].id, "i2");
        let read = d.insights().list(&InsightFilter { unread_only: Some(false), ..Default::default() }).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, "i1");
    }

    #[test]
    fn list_ordered_by_confidence_then_recency() {
        let d = db();
        d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.7),
            insight("i2", InsightType::DuplicateCapability, 0.95),
            insight("i3", InsightType::DuplicateCapability, 0.85),
        ]).unwrap();
        let hits = d.insights().list(&InsightFilter::default()).unwrap();
        assert_eq!(hits[0].id, "i2");
        assert_eq!(hits[2].id, "i1");
    }

    #[test]
    fn list_pagination() {
        let d = db();
        let list: Vec<Insight> = (0..12)
            .map(|n| insight(&format!("i{n:02}"), InsightType::DuplicateCapability, 0.8))
            .collect();
        d.insights().upsert_batch(&list).unwrap();
        let page = d.insights().list(&InsightFilter { limit: Some(5), offset: 5, ..Default::default() }).unwrap();
        assert_eq!(page.len(), 5);
    }

    #[test]
    fn count_unread_tracks_feedback() {
        let d = db();
        d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.9),
            insight("i2", InsightType::ForgottenAsset, 0.9),
        ]).unwrap();
        assert_eq!(d.insights().count_unread().unwrap(), 2);
        d.insights().set_feedback("i1", Some(UserFeedback::Ignored)).unwrap();
        assert_eq!(d.insights().count_unread().unwrap(), 1);
    }

    #[test]
    fn count_by_type_aggregates() {
        let d = db();
        d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.9),
            insight("i2", InsightType::DuplicateCapability, 0.85),
            insight("i3", InsightType::TechDirection, 0.8),
        ]).unwrap();
        let dist = d.insights().count_by_type().unwrap();
        let dup = dist.iter().find(|(t, _)| *t == InsightType::DuplicateCapability).unwrap();
        assert_eq!(dup.1, 2);
    }

    /// 阶段二关键指标：采纳率 = useful / (useful + useless)。
    #[test]
    fn adoption_rate_excludes_ignored() {
        let d = db();
        d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.9),
            insight("i2", InsightType::DuplicateCapability, 0.9),
            insight("i3", InsightType::DuplicateCapability, 0.9),
            insight("i4", InsightType::DuplicateCapability, 0.9),
        ]).unwrap();
        d.insights().set_feedback("i1", Some(UserFeedback::Useful)).unwrap();
        d.insights().set_feedback("i2", Some(UserFeedback::Useful)).unwrap();
        d.insights().set_feedback("i3", Some(UserFeedback::Useless)).unwrap();
        d.insights().set_feedback("i4", Some(UserFeedback::Ignored)).unwrap();

        let (useful, total) = d.insights().adoption_rate().unwrap();
        assert_eq!(useful, 2);
        assert_eq!(total, 3, "Ignored 不计入分母");
        assert!((useful as f64 / total as f64 - 0.667).abs() < 0.01);
    }

    #[test]
    fn adoption_rate_zero_when_no_feedback() {
        let (u, t) = db().insights().adoption_rate().unwrap();
        assert_eq!((u, t), (0, 0), "无反馈时应返回 0/0，由调用方显示「暂无反馈」");
    }

    #[test]
    fn feedback_can_be_cleared() {
        let d = db();
        d.insights().upsert(&insight("i1", InsightType::DuplicateCapability, 0.9)).unwrap();
        d.insights().set_feedback("i1", Some(UserFeedback::Useless)).unwrap();
        d.insights().set_feedback("i1", None).unwrap();
        assert!(d.insights().get("i1").unwrap().unwrap().user_feedback.is_none());
    }

    #[test]
    fn set_feedback_on_missing_returns_false() {
        assert!(!db().insights().set_feedback("nope", Some(UserFeedback::Useful)).unwrap());
    }

    #[test]
    fn delete_removes_insight() {
        let d = db();
        d.insights().upsert(&insight("i1", InsightType::DuplicateCapability, 0.9)).unwrap();
        assert!(d.insights().delete("i1").unwrap());
        assert!(!d.insights().delete("i1").unwrap());
        assert_eq!(d.insights().count().unwrap(), 0);
    }

    #[test]
    fn delete_by_type_scopes() {
        let d = db();
        d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.9),
            insight("i2", InsightType::DuplicateCapability, 0.85),
            insight("i3", InsightType::TechDirection, 0.8),
        ]).unwrap();
        assert_eq!(d.insights().delete_by_type(InsightType::DuplicateCapability).unwrap(), 2);
        assert_eq!(d.insights().count().unwrap(), 1);
    }

    /// 脏证据 JSON 不应导致"能写入却读不出"。
    #[test]
    fn corrupt_evidence_json_yields_placeholder() {
        let d = db();
        d.insights().upsert(&insight("i1", InsightType::DuplicateCapability, 0.9)).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE insights SET evidence_json='{{bad' WHERE id='i1'", []).unwrap();
        drop(conn);
        let i = d.insights().get("i1").unwrap().unwrap();
        assert_eq!(i.evidence.len(), 1, "应给占位证据而非空数组");
        assert!(i.evidence[0].label.contains("损坏"));
        assert_eq!(i.title, "重复实现的能力", "其余字段仍可读");
    }

    #[test]
    fn unknown_type_degrades_gracefully() {
        let d = db();
        d.insights().upsert(&insight("i1", InsightType::DuplicateCapability, 0.9)).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE insights SET type='future_type' WHERE id='i1'", []).unwrap();
        drop(conn);
        let i = d.insights().get("i1").unwrap().unwrap();
        assert_eq!(i.insight_type, InsightType::ReusableComponent);
    }

    #[test]
    fn min_confidence_filter() {
        let d = db();
        d.insights().upsert_batch(&[
            insight("i1", InsightType::DuplicateCapability, 0.95),
            insight("i2", InsightType::DuplicateCapability, 0.6),
        ]).unwrap();
        let hits = d.insights().list(&InsightFilter { min_confidence: Some(0.8), ..Default::default() }).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "i1");
    }
}
