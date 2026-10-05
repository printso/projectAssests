//! 关系 Repository（泛化边表）。
//!
//! 《产品设计书》§3：没有关系只是数据库，有了关系才开始产生智能。
//! 本模块是所有"跨项目智能"的读取入口：图谱、相似项目、能力聚合都走这里。

use rusqlite::{params, Connection, OptionalExtension};

use spolia_domain::{EntityKind, Relation, RelationType, StorageError};

use crate::pool::Pool;
use crate::row;

/// 关系仓储。
#[derive(Debug)]
pub struct RelationRepo<'a> {
    pool: &'a Pool,
}

const COLS: &str = "id, source_id, source_type, relation_type, target_id, target_type, confidence, evidence_json";

impl<'a> RelationRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    pub fn upsert(&self, r: &Relation) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        Self::upsert_conn(&conn, r)
    }

    /// 批量写入关系。
    ///
    /// 🔴 走 `Pool::write_in_chunks`：关系是全部实体里行数最多的
    /// （实测一次全盘扫描产出 36 万行），整批一个事务会把写锁占住极久，
    /// 期间用户改任何设置都会等满 busy_timeout 后失败。
    /// 分块后每块几十毫秒，块间隙能放行其他写入。
    /// 幂等 upsert（`ON CONFLICT(source_id, relation_type, target_id)`），
    /// 中途失败后重跑索引即可补齐。
    pub fn upsert_batch(&self, rels: &[Relation]) -> Result<usize, StorageError> {
        self.pool
            .write_in_chunks("批量关系", rels, Self::upsert_conn)
    }

    fn upsert_conn(conn: &Connection, r: &Relation) -> Result<(), StorageError> {
        let evidence = row::to_json(&r.evidence)?;
        conn.execute(
            "INSERT INTO relations (id, source_id, source_type, relation_type, target_id, target_type, confidence, evidence_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(source_id, relation_type, target_id) DO UPDATE SET
                source_type=excluded.source_type, target_type=excluded.target_type,
                confidence=excluded.confidence, evidence_json=excluded.evidence_json",
            params![
                r.id,
                r.source_id,
                r.source_type.as_str(),
                r.relation_type.as_str(),
                r.target_id,
                r.target_type.as_str(),
                r.confidence,
                evidence,
            ],
        )
        .map(|_| ())
        .map_err(|e| StorageError::sqlite("写入 relations", e))
    }

    pub fn get(&self, id: &str) -> Result<Option<Relation>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM relations WHERE id = ?1");
        conn.query_row(&sql, [id], map_relation)
            .optional()
            .map_err(|e| StorageError::sqlite("查询关系", e))
    }

    /// 从某实体出发的关系。
    pub fn from_source(&self, source_id: &str) -> Result<Vec<Relation>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM relations WHERE source_id = ?1 ORDER BY confidence DESC");
        Self::query_vec(&conn, &sql, params![source_id], "按源查询关系")
    }

    /// 指向某实体的关系。
    pub fn to_target(&self, target_id: &str) -> Result<Vec<Relation>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM relations WHERE target_id = ?1 ORDER BY confidence DESC");
        Self::query_vec(&conn, &sql, params![target_id], "按目标查询关系")
    }

    /// 与某实体相连的全部关系（图谱节点点击用）。
    pub fn touching(&self, id: &str) -> Result<Vec<Relation>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM relations WHERE source_id = ?1 OR target_id = ?1 ORDER BY confidence DESC"
        );
        Self::query_vec(&conn, &sql, params![id], "查询相连关系")
    }

    /// 按类型查询某实体的关系（如"项目的 similar_to"）。
    pub fn by_type(&self, id: &str, rt: RelationType) -> Result<Vec<Relation>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM relations
             WHERE (source_id = ?1 OR target_id = ?1) AND relation_type = ?2
             ORDER BY confidence DESC"
        );
        Self::query_vec(&conn, &sql, params![id, rt.as_str()], "按类型查询关系")
    }

    /// 某类型的全部关系（洞察引擎批量处理用）。
    pub fn list_by_type(&self, rt: RelationType) -> Result<Vec<Relation>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM relations WHERE relation_type = ?1 ORDER BY confidence DESC");
        Self::query_vec(&conn, &sql, params![rt.as_str()], "列举类型关系")
    }

    /// 全部关系边。
    ///
    /// 洞察引擎需要**整张图**才能做跨项目推断（重复能力、组合机会）：
    /// 它按 `source_id → target_id` 聚合"哪些项目实现了哪些能力"，
    /// 只给单一类型的边会漏掉跨类型的推断路径。
    ///
    /// 规模受控：边数 = 项目数 × 能力数，三层结构下通常在数千量级，
    /// 一次性载入内存没有风险（对比 assets 可能上万条，那边必须分页）。
    ///
    /// 排序确定性：按 confidence 降序再按 id，保证同一份数据两次读取顺序一致，
    /// 否则洞察生成的顺序会随存储布局漂移。
    pub fn list_all(&self) -> Result<Vec<Relation>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM relations ORDER BY confidence DESC, id ASC"
        );
        Self::query_vec(&conn, &sql, [], "列举全部关系")
    }

    /// 关系总数（图谱统计）。
    pub fn count(&self) -> Result<usize, StorageError> {        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM relations", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计关系数", e))
    }

    /// 关系类型分布（图谱图例的计数）。
    pub fn count_by_type(&self) -> Result<Vec<(RelationType, usize)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare("SELECT relation_type, count(*) FROM relations GROUP BY relation_type")
            .map_err(|e| StorageError::sqlite("准备关系类型统计", e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize))
            })
            .map_err(|e| StorageError::sqlite("执行关系类型统计", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (t, n) = r.map_err(|e| StorageError::sqlite("映射关系类型统计行", e))?;
            if let Some(rt) = RelationType::parse(&t) {
                out.push((rt, n));
            } else {
                tracing::warn!(relation_type = %t, "relations 表存在未知类型，已跳过");
            }
        }
        Ok(out)
    }

    /// 某项目的能力关系（项目详情页「能力覆盖」）。
    pub fn capabilities_of_project(&self, project_id: &str) -> Result<Vec<Relation>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM relations
             WHERE source_id = ?1 AND relation_type = 'implements' AND target_type = 'capability'
             ORDER BY confidence DESC"
        );
        Self::query_vec(&conn, &sql, params![project_id], "查询项目能力关系")
    }

    /// 相似项目（项目详情页「相关项目」Tab）。
    ///
    /// 返回 `(关系, 相似度)`：相似度直接取 relation.confidence，
    /// 避免原型期"硬编码 87%"那类假数据。
    pub fn similar_projects(&self, project_id: &str, min_confidence: f64) -> Result<Vec<Relation>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM relations
             WHERE relation_type = 'similar_to'
               AND source_type = 'project' AND target_type = 'project'
               AND (source_id = ?1 OR target_id = ?1)
               AND confidence >= ?2
             ORDER BY confidence DESC"
        );
        Self::query_vec(&conn, &sql, params![project_id, min_confidence], "查询相似项目")
    }

    /// 实现某能力的全部项目（图谱节点「Used in」）。
    pub fn projects_implementing(&self, capability_id: &str) -> Result<Vec<String>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT source_id FROM relations
                 WHERE target_id = ?1 AND relation_type = 'implements' AND source_type = 'project'
                 ORDER BY source_id",
            )
            .map_err(|e| StorageError::sqlite("准备能力项目查询", e))?;
        let rows = stmt
            .query_map([capability_id], |r| r.get::<_, String>(0))
            .map_err(|e| StorageError::sqlite("执行能力项目查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射能力项目行", e))?);
        }
        Ok(out)
    }

    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let n = conn
            .execute("DELETE FROM relations WHERE id = ?1", [id])
            .map_err(|e| StorageError::sqlite("删除关系", e))?;
        Ok(n > 0)
    }

    /// 删除涉及某实体的全部关系（项目重新扫描前清理）。
    pub fn delete_touching(&self, id: &str) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.execute(
            "DELETE FROM relations WHERE source_id = ?1 OR target_id = ?1",
            [id],
        )
        .map_err(|e| StorageError::sqlite("删除实体关系", e))
    }

    /// 低于置信度门槛的关系清理（防噪音，见《技术设计书》§25）。
    pub fn prune_low_confidence(&self, threshold: f64) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.execute("DELETE FROM relations WHERE confidence < ?1", [threshold])
            .map_err(|e| StorageError::sqlite("清理低置信度关系", e))
    }

    fn query_vec(
        conn: &Connection,
        sql: &str,
        args: impl rusqlite::Params,
        ctx: &str,
    ) -> Result<Vec<Relation>, StorageError> {
        let mut stmt = conn.prepare(sql).map_err(|e| StorageError::sqlite(format!("准备: {ctx}"), e))?;
        let rows = stmt
            .query_map(args, map_relation)
            .map_err(|e| StorageError::sqlite(format!("执行: {ctx}"), e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite(format!("映射: {ctx}"), e))?);
        }
        Ok(out)
    }
}

fn map_relation(r: &rusqlite::Row<'_>) -> rusqlite::Result<Relation> {
    let st: String = r.get(2)?;
    let rt: String = r.get(3)?;
    let tt: String = r.get(5)?;
    Ok(Relation {
        id: row::text(r, 0)?,
        source_id: row::text(r, 1)?,
        // 未知类型降级为 RelatedTo（最中性的语义），不让整页失败
        source_type: EntityKind::parse(&st).unwrap_or(EntityKind::Project),
        relation_type: RelationType::parse(&rt).unwrap_or(RelationType::RelatedTo),
        target_id: row::text(r, 4)?,
        target_type: EntityKind::parse(&tt).unwrap_or(EntityKind::Capability),
        confidence: row::real(r, 6)?,
        evidence: row::json_col::<Vec<String>>(r, 7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    fn rel(id: &str, src: &str, rt: RelationType, tgt: &str, conf: f64) -> Relation {
        Relation::new(id, src, EntityKind::Project, rt, tgt, EntityKind::Capability, conf)
    }

    #[test]
    fn upsert_and_get_roundtrips() {
        let d = db();
        let r = rel("r1", "p1", RelationType::Implements, "c1", 0.9);
        d.relations().upsert(&r).unwrap();
        let got = d.relations().get("r1").unwrap().unwrap();
        assert_eq!(got.source_id, "p1");
        assert_eq!(got.target_id, "c1");
        assert_eq!(got.relation_type, RelationType::Implements);
        assert_eq!(got.source_type, EntityKind::Project);
        assert!((got.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn evidence_persists() {
        let d = db();
        let r = rel("r1", "p1", RelationType::SimilarTo, "p2", 0.8)
            .with_evidence(["a/src/x.py".into(), "b/src/y.py".into()]);
        d.relations().upsert(&r).unwrap();
        let got = d.relations().get("r1").unwrap().unwrap();
        assert_eq!(got.evidence.len(), 2);
        assert_eq!(got.evidence[0], "a/src/x.py");
    }

    /// 唯一约束 (source, type, target) 保证重复分析不产生重复边。
    #[test]
    fn duplicate_triple_updates_instead_of_inserting() {
        let d = db();
        d.relations().upsert(&rel("r1", "p1", RelationType::Implements, "c1", 0.5)).unwrap();
        d.relations().upsert(&rel("r2", "p1", RelationType::Implements, "c1", 0.9)).unwrap();
        assert_eq!(d.relations().count().unwrap(), 1);
        // 保留的是后写入的置信度
        let all = d.relations().from_source("p1").unwrap();
        assert!((all[0].confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn batch_upsert_writes_all() {
        let d = db();
        let rels: Vec<Relation> = (0..30)
            .map(|i| rel(&format!("r{i}"), &format!("p{i}"), RelationType::Implements, "c1", 0.8))
            .collect();
        assert_eq!(d.relations().upsert_batch(&rels).unwrap(), 30);
        assert_eq!(d.relations().count().unwrap(), 30);
    }

    #[test]
    fn empty_batch_is_noop() {
        assert_eq!(db().relations().upsert_batch(&[]).unwrap(), 0);
    }

    #[test]
    fn from_source_and_to_target() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.9),
            rel("r2", "p1", RelationType::Implements, "c2", 0.8),
            rel("r3", "p2", RelationType::Implements, "c1", 0.7),
        ]).unwrap();
        assert_eq!(d.relations().from_source("p1").unwrap().len(), 2);
        assert_eq!(d.relations().to_target("c1").unwrap().len(), 2);
        assert!(d.relations().from_source("nope").unwrap().is_empty());
    }

    #[test]
    fn touching_returns_both_directions() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.9),
            rel("r2", "p2", RelationType::Implements, "p1", 0.8),
        ]).unwrap();
        assert_eq!(d.relations().touching("p1").unwrap().len(), 2);
    }

    #[test]
    fn by_type_filters() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.9),
            rel("r2", "p1", RelationType::SimilarTo, "p2", 0.8),
        ]).unwrap();
        let impls = d.relations().by_type("p1", RelationType::Implements).unwrap();
        assert_eq!(impls.len(), 1);
        assert_eq!(impls[0].id, "r1");
    }

    #[test]
    fn list_by_type_returns_all() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.9),
            rel("r2", "p2", RelationType::Implements, "c1", 0.8),
            rel("r3", "p1", RelationType::SimilarTo, "p2", 0.7),
        ]).unwrap();
        assert_eq!(d.relations().list_by_type(RelationType::Implements).unwrap().len(), 2);
        assert_eq!(d.relations().list_by_type(RelationType::SimilarTo).unwrap().len(), 1);
        assert!(d.relations().list_by_type(RelationType::DependsOn).unwrap().is_empty());
    }

    #[test]
    fn count_by_type_aggregates() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.9),
            rel("r2", "p2", RelationType::Implements, "c1", 0.8),
            rel("r3", "p1", RelationType::SimilarTo, "p2", 0.7),
        ]).unwrap();
        let dist = d.relations().count_by_type().unwrap();
        let impls = dist.iter().find(|(t, _)| *t == RelationType::Implements).unwrap();
        assert_eq!(impls.1, 2);
    }

    #[test]
    fn capabilities_of_project_filters_correctly() {
        let d = db();
        // 混入一条 target_type 不是 capability 的关系
        let mut non_cap = rel("r3", "p1", RelationType::Implements, "t1", 0.9);
        non_cap.target_type = EntityKind::Technology;
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.9),
            rel("r2", "p1", RelationType::SimilarTo, "p2", 0.8),
            non_cap,
        ]).unwrap();
        let caps = d.relations().capabilities_of_project("p1").unwrap();
        assert_eq!(caps.len(), 1, "只应返回 implements + capability");
        assert_eq!(caps[0].target_id, "c1");
    }

    /// 相似项目的相似度必须来自真实 confidence，不是硬编码。
    #[test]
    fn similar_projects_respects_confidence_threshold() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::SimilarTo, "p2", 0.87),
            rel("r2", "p1", RelationType::SimilarTo, "p3", 0.42),
        ]).unwrap();
        // target_type 需为 project 才会命中
        let conn = d.conn().unwrap();
        conn.execute("UPDATE relations SET target_type='project' WHERE id IN ('r1','r2')", []).unwrap();
        drop(conn);

        let high = d.relations().similar_projects("p1", 0.7).unwrap();
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].target_id, "p2");
        assert!((high[0].confidence - 0.87).abs() < 1e-9, "相似度应取自真实 confidence");

        let all = d.relations().similar_projects("p1", 0.0).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn similar_projects_finds_reverse_direction() {
        let d = db();
        let mut r = rel("r1", "p2", RelationType::SimilarTo, "p1", 0.8);
        r.target_type = EntityKind::Project;
        d.relations().upsert(&r).unwrap();
        let hits = d.relations().similar_projects("p1", 0.5).unwrap();
        assert_eq!(hits.len(), 1, "反向边也应命中");
    }

    #[test]
    fn projects_implementing_returns_distinct() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.9),
            rel("r2", "p2", RelationType::Implements, "c1", 0.8),
            rel("r3", "p1", RelationType::Implements, "c1", 0.7),
        ]).unwrap();
        let ps = d.relations().projects_implementing("c1").unwrap();
        assert_eq!(ps.len(), 2, "同一项目应去重");
        assert!(ps.contains(&"p1".to_string()));
    }

    #[test]
    fn delete_removes_relation() {
        let d = db();
        d.relations().upsert(&rel("r1", "p1", RelationType::Implements, "c1", 0.9)).unwrap();
        assert!(d.relations().delete("r1").unwrap());
        assert!(d.relations().get("r1").unwrap().is_none());
        assert!(!d.relations().delete("r1").unwrap());
    }

    #[test]
    fn delete_touching_clears_both_ends() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.9),
            rel("r2", "p2", RelationType::Implements, "p1", 0.8),
            rel("r3", "p2", RelationType::Implements, "c9", 0.7),
        ]).unwrap();
        assert_eq!(d.relations().delete_touching("p1").unwrap(), 2);
        assert_eq!(d.relations().count().unwrap(), 1);
    }

    #[test]
    fn prune_low_confidence_removes_noise() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.9),
            rel("r2", "p1", RelationType::Implements, "c2", 0.3),
        ]).unwrap();
        assert_eq!(d.relations().prune_low_confidence(0.5).unwrap(), 1);
        assert_eq!(d.relations().count().unwrap(), 1);
        assert_eq!(d.relations().get("r1").unwrap().unwrap().id, "r1");
    }

    #[test]
    fn unknown_types_degrade_gracefully() {
        let d = db();
        d.relations().upsert(&rel("r1", "p1", RelationType::Implements, "c1", 0.9)).unwrap();
        let conn = d.conn().unwrap();
        conn.execute(
            "UPDATE relations SET relation_type='future_rel', source_type='future_kind' WHERE id='r1'",
            [],
        ).unwrap();
        drop(conn);
        let got = d.relations().get("r1").unwrap().unwrap();
        assert_eq!(got.relation_type, RelationType::RelatedTo);
        assert_eq!(got.source_type, EntityKind::Project);
    }

    #[test]
    fn corrupt_evidence_json_degrades_to_empty() {
        let d = db();
        d.relations().upsert(&rel("r1", "p1", RelationType::Implements, "c1", 0.9)).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE relations SET evidence_json='{{bad' WHERE id='r1'", []).unwrap();
        drop(conn);
        let got = d.relations().get("r1").unwrap().unwrap();
        assert!(got.evidence.is_empty());
        assert_eq!(got.source_id, "p1");
    }

    #[test]
    fn results_ordered_by_confidence_desc() {
        let d = db();
        d.relations().upsert_batch(&[
            rel("r1", "p1", RelationType::Implements, "c1", 0.5),
            rel("r2", "p1", RelationType::Implements, "c2", 0.95),
            rel("r3", "p1", RelationType::Implements, "c3", 0.7),
        ]).unwrap();
        let rs = d.relations().from_source("p1").unwrap();
        assert_eq!(rs[0].id, "r2");
        assert_eq!(rs[2].id, "r1");
    }
}
