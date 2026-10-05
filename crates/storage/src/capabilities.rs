//! 能力 Repository（三层结构：Domain → Capability → Implementation）。
//!
//! 🔴 防标签爆炸是这里的核心职责：
//! - `(name, parent_id)` 唯一约束在 schema 层，本层负责"先查后插"的幂等 upsert
//! - `project_count` 由关系表聚合刷新，不由调用方随意写入

use rusqlite::{params, Connection, OptionalExtension};

use projectassests_domain::{Capability, CapabilityLayer, StorageError};

use crate::pool::Pool;
use crate::row;

/// 能力仓储。
#[derive(Debug)]
pub struct CapabilityRepo<'a> {
    pool: &'a Pool,
}

pub(crate) const COLS: &str = "id, name, description, layer, parent_id, confidence, project_count";

impl<'a> CapabilityRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    pub fn upsert(&self, c: &Capability) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启能力事务", e))?;
        Self::upsert_conn(&tx, c)?;
        tx.commit().map_err(|e| StorageError::sqlite("提交能力事务", e))
    }

    /// 批量写入能力。
    ///
    /// 🔴 走 `Pool::write_in_chunks` 而非整批一个事务：理由见该方法的文档
    /// （巨型事务会长时间独占写锁，期间用户改设置必然超时失败）。
    pub fn upsert_batch(&self, caps: &[Capability]) -> Result<usize, StorageError> {
        self.pool
            .write_in_chunks("批量能力", caps, Self::upsert_conn)
    }

    fn upsert_conn(conn: &Connection, c: &Capability) -> Result<(), StorageError> {
        // 领域层已校验过层级不变式；这里再挡一次，防止直接构造的结构体绕过 `new()`
        match c.layer {
            CapabilityLayer::Domain => {
                if c.parent_id.is_some() {
                    return Err(StorageError::sqlite(
                        "写入能力：Domain 不能有父节点",
                        format!("{} (id={})", c.name, c.id),
                    ));
                }
            }
            _ => {
                if c.parent_id.is_none() {
                    return Err(StorageError::sqlite(
                        "写入能力：非 Domain 必须有父节点",
                        format!("{} (id={})", c.name, c.id),
                    ));
                }
            }
        }
        if c.name.trim().is_empty() {
            return Err(StorageError::sqlite("写入能力：名称为空", c.id.clone()));
        }

        // 🔑 能力的**身份是自然键 (name, parent_id)**，不是调用方给的 id。
        //
        // 为什么：能力抽取会在每次扫描时重复运行。若按调用方 id 判重，
        // 而抽取器每轮生成新 id（如 uuid），同名的 "RAG under AI" 就会被反复插入，
        // 直接撞上 UNIQUE(name, parent_id) 报错——或退化成《产品设计书》§11 警告的
        // "几千个扁平标签"。
        //
        // 因此先按自然键查已存在的行，若存在则**沿用它原有的 id**：
        // relations 表里可能已有大量边指向旧 id，改写 id 会让那些边全部悬空。
        let existing_id = Self::find_existing_id(conn, &c.name, c.parent_id.as_deref())?;
        let effective_id = existing_id.unwrap_or_else(|| c.id.clone());

        conn.execute(
            "INSERT INTO capabilities (id, name, description, layer, parent_id, confidence, project_count)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, description=excluded.description,
                layer=excluded.layer, parent_id=excluded.parent_id,
                confidence=excluded.confidence, project_count=excluded.project_count",
            params![
                effective_id,
                c.name,
                c.description,
                c.layer.as_str(),
                c.parent_id,
                c.confidence,
                c.project_count as i64,
            ],
        )
        .map_err(|e| StorageError::sqlite("写入 capabilities", e))?;

        // FTS 同步同样要用生效 id，并清掉调用方那个被丢弃的 id 的残留索引
        if effective_id != c.id {
            conn.execute("DELETE FROM capabilities_fts WHERE capability_id = ?1", [&c.id])
                .map_err(|e| StorageError::sqlite("清理失效能力索引", e))?;
        }
        let synced = Capability { id: effective_id, ..c.clone() };
        Self::sync_fts(conn, &synced)
    }

    /// 按自然键查已存在的能力 id。
    ///
    /// Domain 层的 `parent_id` 为 NULL，而 SQL 里 `NULL = NULL` 不成立，
    /// 必须用 `IS NULL` 单独处理，否则 Domain 永远查不到已存在行、每轮都重复插入。
    fn find_existing_id(
        conn: &Connection,
        name: &str,
        parent_id: Option<&str>,
    ) -> Result<Option<String>, StorageError> {
        let sql = match parent_id {
            Some(_) => "SELECT id FROM capabilities WHERE name = ?1 AND parent_id = ?2",
            None => "SELECT id FROM capabilities WHERE name = ?1 AND parent_id IS NULL",
        };
        let row = match parent_id {
            Some(p) => conn
                .query_row(sql, params![name, p], |r| r.get::<_, String>(0))
                .optional(),
            None => conn
                .query_row(sql, params![name], |r| r.get::<_, String>(0))
                .optional(),
        };
        row.map_err(|e| StorageError::sqlite("按自然键查询能力", e))
    }

    /// 返回该 id 及其全部后代 id（能力树最多三层，用递归 CTE 一次取全）。
    ///
    /// 用途：删除节点时同步清理 FTS。外键 `ON DELETE CASCADE` 只删主表行，
    /// **不会**触发 FTS 虚表的清理——不显式处理就会留下"搜得到但点不开"的孤儿索引。
    fn collect_subtree_ids(conn: &Connection, id: &str) -> Result<Vec<String>, StorageError> {
        let mut stmt = conn
            .prepare(
                "WITH RECURSIVE subtree(id) AS (
                    SELECT id FROM capabilities WHERE id = ?1
                    UNION ALL
                    SELECT c.id FROM capabilities c JOIN subtree s ON c.parent_id = s.id
                 )
                 SELECT id FROM subtree",
            )
            .map_err(|e| StorageError::sqlite("准备能力子树查询", e))?;
        let rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| StorageError::sqlite("执行能力子树查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射能力子树行", e))?);
        }
        Ok(out)
    }

    fn sync_fts(conn: &Connection, c: &Capability) -> Result<(), StorageError> {
        conn.execute("DELETE FROM capabilities_fts WHERE capability_id = ?1", [&c.id])
            .map_err(|e| StorageError::sqlite("清理能力索引", e))?;
        conn.execute(
            "INSERT INTO capabilities_fts(capability_id, name, description) VALUES (?1,?2,?3)",
            params![c.id, c.name, c.description],
        )
        .map(|_| ())
        .map_err(|e| StorageError::sqlite("写入能力索引", e))
    }

    /// 按名称与父节点查找（抽取时做"是否已存在"判定，避免重复建标签）。
    pub fn find_by_name(&self, name: &str, parent_id: Option<&str>) -> Result<Option<Capability>, StorageError> {
        let conn = self.pool.get()?;
        let sql = match parent_id {
            Some(_) => format!("SELECT {COLS} FROM capabilities WHERE name = ?1 AND parent_id = ?2"),
            None => format!("SELECT {COLS} FROM capabilities WHERE name = ?1 AND parent_id IS NULL"),
        };
        let result = match parent_id {
            Some(p) => conn.query_row(&sql, params![name, p], map_capability).optional(),
            None => conn.query_row(&sql, params![name], map_capability).optional(),
        };
        result.map_err(|e| StorageError::sqlite("按名称查询能力", e))
    }

    pub fn get(&self, id: &str) -> Result<Option<Capability>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM capabilities WHERE id = ?1");
        conn.query_row(&sql, [id], map_capability)
            .optional()
            .map_err(|e| StorageError::sqlite("查询能力", e))
    }

    /// 全部能力（扁平）。数量受三层结构约束，通常在数百量级。
    pub fn list_all(&self) -> Result<Vec<Capability>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM capabilities ORDER BY layer DESC, project_count DESC, name ASC"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::sqlite("准备能力查询", e))?;
        let rows = stmt
            .query_map([], map_capability)
            .map_err(|e| StorageError::sqlite("执行能力查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射能力行", e))?);
        }
        Ok(out)
    }

    /// 按层级查询。
    pub fn list_by_layer(&self, layer: CapabilityLayer) -> Result<Vec<Capability>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM capabilities WHERE layer = ?1 ORDER BY project_count DESC, name ASC"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::sqlite("准备分层能力查询", e))?;
        let rows = stmt
            .query_map([layer.as_str()], map_capability)
            .map_err(|e| StorageError::sqlite("执行分层能力查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射分层能力行", e))?);
        }
        Ok(out)
    }

    /// 子能力。
    pub fn children(&self, parent_id: &str) -> Result<Vec<Capability>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM capabilities WHERE parent_id = ?1 ORDER BY project_count DESC, name ASC"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::sqlite("准备子能力查询", e))?;
        let rows = stmt
            .query_map([parent_id], map_capability)
            .map_err(|e| StorageError::sqlite("执行子能力查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射子能力行", e))?);
        }
        Ok(out)
    }

    pub fn count(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM capabilities", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计能力数", e))
    }

    /// Capability 层的能力数（首页统计卡「能力数量」的口径）。
    ///
    /// 刻意**不**统计 Domain（AI/Web 这类只有 5 个左右）与 Implementation
    /// （具体技术名，数量会很多），否则会给出虚高的数字。
    pub fn count_capabilities(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT count(*) FROM capabilities WHERE layer = 'capability'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as usize)
        .map_err(|e| StorageError::sqlite("统计能力层数量", e))
    }

    /// 关联项目最多的能力（首页"你的核心能力"）。
    ///
    /// 🔴 只取 Capability 层，不含 Domain 与 Implementation：
    /// Domain 只有固定 5 个（AI/Web/数据/基础设施/多媒体），列出来没有信息量；
    /// Implementation 是具体技术名（如 "PyTorch"），数量多但粒度太细。
    /// 用户想看到的是"我掌握了哪些能力"，那正是 Capability 层。
    ///
    /// 排序确定性：project_count 降序、name 升序，保证同数据两次查询结果一致。
    pub fn top_by_project_count(&self, limit: u32) -> Result<Vec<(String, u32)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare(
                "SELECT name, project_count FROM capabilities
                 WHERE layer = 'capability'
                 ORDER BY project_count DESC, name ASC
                 LIMIT ?1",
            )
            .map_err(|e| StorageError::sqlite("准备核心能力查询", e))?;
        let rows = stmt
            .query_map(params![i64::from(limit.clamp(1, 100))], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.clamp(0, i64::from(u32::MAX)) as u32))
            })
            .map_err(|e| StorageError::sqlite("执行核心能力查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射核心能力行", e))?);
        }
        Ok(out)
    }

    /// 刷新 `project_count`：从 relations 表聚合"多少项目实现了该能力"。
    ///
    /// 单一数据源原则：这个数不存两份，只在关系变更后重算，
    /// 避免出现"图谱显示 4 个项目、能力列表显示 3 个"的不一致。
    pub fn refresh_project_counts(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启计数刷新事务", e))?;
        // 先清零，再按关系表重算：保证被删除的关系不会留下旧计数
        tx.execute("UPDATE capabilities SET project_count = 0", [])
            .map_err(|e| StorageError::sqlite("重置能力计数", e))?;
        let n = tx
            .execute(
                "UPDATE capabilities SET project_count = (
                    SELECT COUNT(DISTINCT r.source_id) FROM relations r
                    WHERE r.target_id = capabilities.id
                      AND r.relation_type = 'implements'
                      AND r.source_type = 'project'
                 )",
                [],
            )
            .map_err(|e| StorageError::sqlite("重算能力计数", e))?;
        tx.commit()
            .map_err(|e| StorageError::sqlite("提交计数刷新事务", e))?;
        Ok(n)
    }

    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启能力删除事务", e))?;
        // ⚠️ 必须先收集子树再删主表：外键 CASCADE 只删主表行，
        // 不会清理 FTS 虚表，不显式处理会留下"搜得到但点不开"的孤儿索引。
        let subtree = Self::collect_subtree_ids(&tx, id)?;
        for node_id in &subtree {
            tx.execute("DELETE FROM capabilities_fts WHERE capability_id = ?1", [node_id])
                .map_err(|e| StorageError::sqlite("清理能力索引", e))?;
        }
        // 子能力由外键 ON DELETE CASCADE 带走
        let n = tx
            .execute("DELETE FROM capabilities WHERE id = ?1", [id])
            .map_err(|e| StorageError::sqlite("删除能力", e))?;
        tx.commit()
            .map_err(|e| StorageError::sqlite("提交能力删除事务", e))?;
        Ok(n > 0)
    }

    /// 删除没有任何项目关联的能力（标签清理，防爆炸）。
    ///
    /// 仅删 Capability / Implementation 层，**不删 Domain**：
    /// Domain 是固定骨架，即使暂时为空也应保留以维持图谱结构。
    pub fn prune_orphans(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启能力清理事务", e))?;
        // 先选出待删 id，以便同步清理 FTS（直接 DELETE 会留下孤儿索引）
        let mut stmt = tx
            .prepare(
                "SELECT id FROM capabilities
                 WHERE layer <> 'domain'
                   AND project_count = 0
                   AND id NOT IN (SELECT parent_id FROM capabilities WHERE parent_id IS NOT NULL)",
            )
            .map_err(|e| StorageError::sqlite("准备孤立能力查询", e))?;
        let ids: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| StorageError::sqlite("执行孤立能力查询", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::sqlite("映射孤立能力行", e))?;
        drop(stmt);
        for id in &ids {
            tx.execute("DELETE FROM capabilities_fts WHERE capability_id = ?1", [id])
                .map_err(|e| StorageError::sqlite("清理孤立能力索引", e))?;
        }
        let n = tx
            .execute(
                "DELETE FROM capabilities
                 WHERE layer <> 'domain'
                   AND project_count = 0
                   AND id NOT IN (SELECT parent_id FROM capabilities WHERE parent_id IS NOT NULL)",
                [],
            )
            .map_err(|e| StorageError::sqlite("清理孤立能力", e))?;
        tx.commit()
            .map_err(|e| StorageError::sqlite("提交能力清理事务", e))?;
        Ok(n)
    }

    pub fn fts_count(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM capabilities_fts", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计能力索引", e))
    }

    pub fn search_fts(&self, query: &str, limit: u32) -> Result<Vec<(String, f64)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare(
                "SELECT capability_id, bm25(capabilities_fts) FROM capabilities_fts
                 WHERE capabilities_fts MATCH ?1 ORDER BY bm25(capabilities_fts) LIMIT ?2",
            )
            .map_err(|e| StorageError::sqlite("准备能力 FTS 查询", e))?;
        let rows = stmt
            .query_map(params![query, i64::from(limit.clamp(1, 500))], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(|e| StorageError::sqlite("执行能力 FTS 查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射能力 FTS 行", e))?);
        }
        Ok(out)
    }
}

pub(crate) fn map_capability(r: &rusqlite::Row<'_>) -> rusqlite::Result<Capability> {
    let layer_str: String = r.get(3)?;
    Ok(Capability {
        id: row::text(r, 0)?,
        name: row::text(r, 1)?,
        description: row::text(r, 2)?,
        layer: CapabilityLayer::parse(&layer_str).unwrap_or(CapabilityLayer::Capability),
        parent_id: row::text_opt(r, 4)?,
        confidence: row::real(r, 5)?,
        project_count: row::u32_col(r, 6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use projectassests_domain::{EntityKind, Relation, RelationType};

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    fn domain(id: &str, name: &str) -> Capability {
        Capability::new(id, name, CapabilityLayer::Domain, None, 1.0).unwrap()
    }

    fn cap(id: &str, name: &str, parent: &str) -> Capability {
        Capability::new(id, name, CapabilityLayer::Capability, Some(parent.into()), 0.8).unwrap()
    }

    fn mk_project(d: &Database, id: &str) {
        d.projects()
            .upsert(&crate::tests::sample_project(id, id, &format!("/tmp/{id}")))
            .unwrap();
    }

    #[test]
    fn upsert_and_get_roundtrips() {
        let d = db();
        d.capabilities().upsert(&domain("ai", "AI")).unwrap();
        let c = d.capabilities().get("ai").unwrap().unwrap();
        assert_eq!(c.name, "AI");
        assert_eq!(c.layer, CapabilityLayer::Domain);
        assert!(c.parent_id.is_none());
    }

    #[test]
    fn three_layer_hierarchy_persists() {
        let d = db();
        let impl_ = Capability::new("qwen", "Qwen", CapabilityLayer::Implementation, Some("rag".into()), 0.7).unwrap();
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai"), impl_])
            .unwrap();

        let rag = d.capabilities().get("rag").unwrap().unwrap();
        assert_eq!(rag.parent_id.as_deref(), Some("ai"));
        let kids = d.capabilities().children("rag").unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, "Qwen");
    }

    /// 防标签爆炸：同父同名不得产生两条记录。
    #[test]
    fn duplicate_capability_is_idempotent() {
        let d = db();
        d.capabilities().upsert(&domain("ai", "AI")).unwrap();
        d.capabilities().upsert(&cap("rag", "RAG", "ai")).unwrap();
        let mut again = cap("rag2", "RAG", "ai");
        again.confidence = 0.95;
        d.capabilities().upsert(&again).unwrap();
        assert_eq!(d.capabilities().count().unwrap(), 2, "同父同名应更新而非新增");
    }

    #[test]
    fn find_by_name_distinguishes_parents() {
        let d = db();
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), domain("web", "Web"), cap("rag", "RAG", "ai")])
            .unwrap();
        let mut rag_web = cap("rag2", "RAG", "web");
        rag_web.id = "rag-web".into();
        d.capabilities().upsert(&rag_web).unwrap();

        assert_eq!(d.capabilities().find_by_name("RAG", Some("ai")).unwrap().unwrap().id, "rag");
        assert_eq!(d.capabilities().find_by_name("RAG", Some("web")).unwrap().unwrap().id, "rag-web");
        assert!(d.capabilities().find_by_name("RAG", None).unwrap().is_none());
    }

    #[test]
    fn find_by_name_handles_null_parent() {
        let d = db();
        d.capabilities().upsert(&domain("ai", "AI")).unwrap();
        assert_eq!(d.capabilities().find_by_name("AI", None).unwrap().unwrap().id, "ai");
        assert!(d.capabilities().find_by_name("nope", None).unwrap().is_none());
    }

    /// 层级不变式必须在存储层也强制（防止绕过 `Capability::new` 直接构造）。
    #[test]
    fn domain_with_parent_is_rejected() {
        let d = db();
        let bad = Capability {
            id: "x".into(),
            name: "X".into(),
            description: String::new(),
            layer: CapabilityLayer::Domain,
            parent_id: Some("ai".into()),
            confidence: 1.0,
            project_count: 0,
        };
        assert!(d.capabilities().upsert(&bad).is_err());
    }

    #[test]
    fn capability_without_parent_is_rejected() {
        let d = db();
        let bad = Capability {
            id: "x".into(),
            name: "X".into(),
            description: String::new(),
            layer: CapabilityLayer::Capability,
            parent_id: None,
            confidence: 1.0,
            project_count: 0,
        };
        assert!(d.capabilities().upsert(&bad).is_err());
    }

    #[test]
    fn empty_name_is_rejected() {
        let d = db();
        let bad = Capability {
            id: "x".into(),
            name: "   ".into(),
            description: String::new(),
            layer: CapabilityLayer::Domain,
            parent_id: None,
            confidence: 1.0,
            project_count: 0,
        };
        assert!(d.capabilities().upsert(&bad).is_err());
    }

    #[test]
    fn list_by_layer_filters() {
        let d = db();
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), domain("web", "Web"), cap("rag", "RAG", "ai")])
            .unwrap();
        assert_eq!(d.capabilities().list_by_layer(CapabilityLayer::Domain).unwrap().len(), 2);
        assert_eq!(d.capabilities().list_by_layer(CapabilityLayer::Capability).unwrap().len(), 1);
        assert_eq!(d.capabilities().list_by_layer(CapabilityLayer::Implementation).unwrap().len(), 0);
    }

    /// 首页「能力数量」只算 Capability 层，不含 Domain/Implementation。
    #[test]
    fn count_capabilities_excludes_domain_and_impl() {
        let d = db();
        let impl_ = Capability::new("qwen", "Qwen", CapabilityLayer::Implementation, Some("rag".into()), 0.7).unwrap();
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai"), cap("agent", "Agent", "ai"), impl_])
            .unwrap();
        assert_eq!(d.capabilities().count().unwrap(), 4);
        assert_eq!(d.capabilities().count_capabilities().unwrap(), 2);
    }

    /// project_count 必须从关系表聚合，不能各存一份导致不一致。
    #[test]
    fn refresh_project_counts_aggregates_from_relations() {
        let d = db();
        mk_project(&d, "p1");
        mk_project(&d, "p2");
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai")])
            .unwrap();

        d.relations().upsert(&Relation::new(
            "r1", "p1", EntityKind::Project, RelationType::Implements, "rag", EntityKind::Capability, 0.9,
        )).unwrap();
        d.relations().upsert(&Relation::new(
            "r2", "p2", EntityKind::Project, RelationType::Implements, "rag", EntityKind::Capability, 0.8,
        )).unwrap();

        assert_eq!(d.capabilities().get("rag").unwrap().unwrap().project_count, 0, "写入关系后尚未刷新");
        d.capabilities().refresh_project_counts().unwrap();
        assert_eq!(d.capabilities().get("rag").unwrap().unwrap().project_count, 2);
    }

    /// 同一项目对同一能力的多条关系只算 1（DISTINCT source_id）。
    #[test]
    fn refresh_counts_distinct_projects() {
        let d = db();
        mk_project(&d, "p1");
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai")])
            .unwrap();
        d.relations().upsert_batch(&[
            Relation::new("r1", "p1", EntityKind::Project, RelationType::Implements, "rag", EntityKind::Capability, 0.9),
            Relation::new("r2", "p1", EntityKind::Project, RelationType::Implements, "rag", EntityKind::Capability, 0.5),
        ]).unwrap();
        d.capabilities().refresh_project_counts().unwrap();
        assert_eq!(d.capabilities().get("rag").unwrap().unwrap().project_count, 1);
    }

    /// 关系被删后计数必须归零，不能留旧值。
    #[test]
    fn refresh_clears_stale_counts() {
        let d = db();
        mk_project(&d, "p1");
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai")])
            .unwrap();
        d.relations().upsert(&Relation::new(
            "r1", "p1", EntityKind::Project, RelationType::Implements, "rag", EntityKind::Capability, 0.9,
        )).unwrap();
        d.capabilities().refresh_project_counts().unwrap();
        assert_eq!(d.capabilities().get("rag").unwrap().unwrap().project_count, 1);

        d.relations().delete("r1").unwrap();
        d.capabilities().refresh_project_counts().unwrap();
        assert_eq!(d.capabilities().get("rag").unwrap().unwrap().project_count, 0);
    }

    /// 非 implements 关系不应计入能力的项目数。
    #[test]
    fn refresh_ignores_other_relation_types() {
        let d = db();
        mk_project(&d, "p1");
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai")])
            .unwrap();
        d.relations().upsert(&Relation::new(
            "r1", "p1", EntityKind::Project, RelationType::SimilarTo, "rag", EntityKind::Capability, 0.9,
        )).unwrap();
        d.capabilities().refresh_project_counts().unwrap();
        assert_eq!(d.capabilities().get("rag").unwrap().unwrap().project_count, 0);
    }

    #[test]
    fn delete_cascades_to_children() {
        let d = db();
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai")])
            .unwrap();
        assert!(d.capabilities().delete("ai").unwrap());
        assert_eq!(d.capabilities().count().unwrap(), 0, "子能力应级联删除");
        assert_eq!(d.capabilities().fts_count().unwrap(), 0);
    }

    /// 标签清理：无项目关联且无子节点的能力应被剪掉，Domain 保留。
    #[test]
    fn prune_orphans_keeps_domains() {
        let d = db();
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai"), cap("agent", "Agent", "ai")])
            .unwrap();
        mk_project(&d, "p1");
        d.relations().upsert(&Relation::new(
            "r1", "p1", EntityKind::Project, RelationType::Implements, "rag", EntityKind::Capability, 0.9,
        )).unwrap();
        d.capabilities().refresh_project_counts().unwrap();

        let pruned = d.capabilities().prune_orphans().unwrap();
        assert_eq!(pruned, 1);
        assert!(d.capabilities().get("rag").unwrap().is_some(), "有项目关联的应保留");
        assert!(d.capabilities().get("agent").unwrap().is_none(), "孤立能力应被清理");
        assert!(d.capabilities().get("ai").unwrap().is_some(), "Domain 是骨架，不得清理");
    }

    /// 有子节点的能力即使 project_count=0 也不该被剪（否则子节点变孤儿）。
    #[test]
    fn prune_orphans_keeps_parents() {
        let d = db();
        let impl_ = Capability::new("qwen", "Qwen", CapabilityLayer::Implementation, Some("rag".into()), 0.7).unwrap();
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai"), impl_])
            .unwrap();
        let pruned = d.capabilities().prune_orphans().unwrap();
        assert_eq!(pruned, 1, "只应剪掉叶子 Qwen");
        assert!(d.capabilities().get("rag").unwrap().is_some());
    }

    #[test]
    fn fts_search_works() {
        let d = db();
        // 父节点必须先存在（外键约束）
        d.capabilities().upsert(&domain("ai", "AI")).unwrap();
        d.capabilities().upsert(&cap("rag", "Retrieval Augmented Generation", "ai")).unwrap();
        assert_eq!(d.capabilities().search_fts("Retrieval", 10).unwrap().len(), 1);
        assert!(d.capabilities().search_fts("nope", 10).unwrap().is_empty());
    }

    #[test]
    fn list_all_returns_everything() {
        let d = db();
        d.capabilities()
            .upsert_batch(&[domain("ai", "AI"), cap("rag", "RAG", "ai")])
            .unwrap();
        assert_eq!(d.capabilities().list_all().unwrap().len(), 2);
    }

    #[test]
    fn unknown_layer_degrades_gracefully() {
        let d = db();
        d.capabilities().upsert(&domain("ai", "AI")).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE capabilities SET layer='future-layer' WHERE id='ai'", [])
            .unwrap();
        drop(conn);
        let c = d.capabilities().get("ai").unwrap().unwrap();
        assert_eq!(c.layer, CapabilityLayer::Capability, "未知层级应降级而非报错");
    }
}
