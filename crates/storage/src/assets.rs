//! 资产 Repository（统一资产表 + type 判别）。
//!
//! 《技术设计书》§11：用一张 `assets` 表 + `type` 判别，而不是八张表。
//! 本模块同时维护 `assets_fts` 索引，并对外提供"按类型/项目/复用分"的筛选。

use rusqlite::{params, Connection, OptionalExtension};

use projectassests_domain::{Asset, AssetType, Evidence, ReuseTier, StorageError, UserFeedback};

use crate::err::sqlite_err;
use crate::pool::Pool;
use crate::row;

/// 资产筛选条件。
#[derive(Debug, Clone, Default)]
pub struct AssetFilter {
    pub project_id: Option<String>,
    pub asset_type: Option<AssetType>,
    /// 多个类型（UI 的多选 chips）
    pub asset_types: Vec<AssetType>,
    /// LIKE 关键词（中文短查询走此路径，理由见 schema.rs）
    pub keyword: Option<String>,
    pub min_reuse_score: Option<f64>,
    /// 仅返回证据充分的资产（产品红线：无证据不展示）
    pub evidence_required: bool,
    pub limit: Option<u32>,
    pub offset: u32,
}

/// 资产排序。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AssetSort {
    /// 复用价值降序（资产页默认：让用户第一眼看到最有价值的东西）
    #[default]
    ReuseScore,
    Confidence,
    /// 最近创建
    Newest,
    Name,
}

impl AssetSort {
    /// 硬编码白名单 SQL，绝不拼接用户输入。
    pub fn order_by(&self) -> &'static str {
        match self {
            Self::ReuseScore => "ORDER BY reuse_score DESC, name ASC",
            Self::Confidence => "ORDER BY confidence DESC, name ASC",
            Self::Newest => "ORDER BY created_at DESC, name ASC",
            Self::Name => "ORDER BY name COLLATE NOCASE ASC",
        }
    }
}

/// 资产批量写入结果。
///
/// 把"写了多少"和"拒了多少"分开报，扫描任务才能如实告诉用户。
/// 拒绝的原因只有一种：缺少证据（产品红线，见 `Evidence::is_sufficient`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AssetWriteOutcome {
    /// 实际写入条数
    pub written: usize,
    /// 因证据不足被拒条数
    pub rejected_no_evidence: usize,
}

impl AssetWriteOutcome {
    /// 处理过的总条数（= 传入的资产数）。
    pub fn total(&self) -> usize {
        self.written + self.rejected_no_evidence
    }

    /// 是否存在被拒项（前端据此提示"部分资产因缺少证据未收录"）。
    pub fn has_rejections(&self) -> bool {
        self.rejected_no_evidence > 0
    }
}

/// 资产仓储。
#[derive(Debug)]
pub struct AssetRepo<'a> {
    pool: &'a Pool,
}

pub(crate) const COLS: &str = "id, project_id, type, name, description, content, source_path, \
     confidence, reuse_score, generality, stability, tags_json, evidence_json, \
     created_at, user_feedback";

impl<'a> AssetRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// 写入单个资产。
    ///
    /// 返回 `Ok(true)` 表示已入库；`Ok(false)` 表示**被证据门禁拒绝**。
    ///
    /// 🔴 为什么返回 bool 而不是 `()`：早期实现静默丢弃无证据资产却返回 `Ok(())`，
    /// 调用方无法得知"没写进去"，批量接口还会谎报写入总数——
    /// 用户看到"已入库 500 个资产"而实际只有 300 个。
    /// 门禁本身是产品红线（《产品设计书》：无证据的结论一律不展示），
    /// 但拒绝必须**可观测**，否则就是数据静默丢失。
    pub fn upsert(&self, a: &Asset) -> Result<bool, StorageError> {
        if !a.evidence.is_sufficient() {
            tracing::warn!(asset = %a.name, "资产缺少证据，已跳过入库");
            return Ok(false);
        }
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启资产事务", e))?;
        Self::upsert_conn(&tx, a)?;
        tx.commit().map_err(|e| StorageError::sqlite("提交资产事务", e))?;
        Ok(true)
    }

    /// 批量写入。资产抽取一次产出数十~数百条，逐条提交会慢一到两个数量级。
    ///
    /// 返回 [`AssetWriteOutcome`]：区分"实际写入"与"因缺证据被拒"，
    /// 供扫描任务向用户如实汇报（也用于洞察层的资产覆盖率诊断）。
    ///
    /// 🔴 走 `Pool::write_in_chunks` 而非"整批一个事务"：
    /// 单个项目的资产可达 2 万条以上，整批一个事务会把写锁占住数分钟，
    /// 期间用户改设置必然超时失败（报 500「数据库操作失败」）。
    /// 分块后每块几十毫秒，其他写入总能在块间隙拿到锁。
    /// 中途失败不回滚已提交的块——本方法是幂等 upsert，重跑索引即可补齐。
    pub fn upsert_batch(&self, assets: &[Asset]) -> Result<AssetWriteOutcome, StorageError> {
        if assets.is_empty() {
            return Ok(AssetWriteOutcome::default());
        }
        // 先在内存里过滤：被拒的资产不进事务，避免为它们做无谓的 IO
        let (accepted, rejected): (Vec<&Asset>, Vec<&Asset>) =
            assets.iter().partition(|a| a.evidence.is_sufficient());
        for a in &rejected {
            tracing::warn!(asset = %a.name, "资产缺少证据，已跳过入库");
        }

        // 🔴 written 用**实际返回值**而非 `accepted.len()`：
        // 两者只在全部成功时相等。用 accepted.len() 的话，
        // 中途失败会向上层谎报"已写入 N 条"——而用户看到的资产数与它对不上。
        let written = self.pool.write_in_chunks("批量资产", &accepted, |conn, a| {
            Self::upsert_conn(conn, a)
        })?;

        Ok(AssetWriteOutcome {
            written,
            rejected_no_evidence: rejected.len(),
        })
    }

    fn upsert_conn(conn: &Connection, a: &Asset) -> Result<(), StorageError> {
        // 证据门禁在 upsert / upsert_batch 入口处统一执行（见其文档注释）。
        // 此处不再重复校验：门禁属于"是否接受这条数据"的策略，
        // 而本函数只负责"把已接受的数据写进 SQL"。
        let tags = row::to_json(&a.tags)?;
        let evidence = row::to_json(&a.evidence)?;

        conn.execute(
            "INSERT INTO assets (
                id, project_id, type, name, description, content, source_path,
                confidence, reuse_score, generality, stability, tags_json,
                evidence_json, created_at, user_feedback
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
             ON CONFLICT(id) DO UPDATE SET
                project_id=excluded.project_id, type=excluded.type, name=excluded.name,
                description=excluded.description, content=excluded.content,
                source_path=excluded.source_path, confidence=excluded.confidence,
                reuse_score=excluded.reuse_score, generality=excluded.generality,
                stability=excluded.stability, tags_json=excluded.tags_json,
                evidence_json=excluded.evidence_json, created_at=excluded.created_at",
            params![
                a.id,
                a.project_id,
                a.asset_type.as_str(),
                a.name,
                a.description,
                a.content,
                a.source_path,
                a.confidence,
                a.reuse_score,
                a.generality,
                a.stability,
                tags,
                evidence,
                a.created_at,
                // user_feedback 刻意不在 UPDATE 列表中：重新抽取不得抹掉用户反馈
                a.user_feedback_sql(),
            ],
        )
        .map_err(|e| StorageError::sqlite("写入 assets", e))?;

        Self::sync_fts(conn, a)
    }

    /// 同步 FTS：先删后插。
    fn sync_fts(conn: &Connection, a: &Asset) -> Result<(), StorageError> {
        conn.execute("DELETE FROM assets_fts WHERE asset_id = ?1", [&a.id])
            .map_err(|e| StorageError::sqlite("清理资产索引", e))?;
        conn.execute(
            "INSERT INTO assets_fts(asset_id, name, description, tags, source_path)
             VALUES (?1,?2,?3,?4,?5)",
            params![a.id, a.name, a.description, a.tags.join(" "), a.source_path],
        )
        .map(|_| ())
        .map_err(|e| StorageError::sqlite("写入资产索引", e))
    }

    pub fn get(&self, id: &str) -> Result<Option<Asset>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM assets WHERE id = ?1");
        conn.query_row(&sql, [id], map_asset)
            .optional()
            .map_err(|e| StorageError::sqlite("查询资产", e))
    }

    pub fn list(&self, filter: &AssetFilter, sort: AssetSort) -> Result<Vec<Asset>, StorageError> {
        let conn = self.pool.get()?;
        let (where_sql, mut args) = build_where(filter);

        // 分页参数必须在 where 参数之后追加，顺序与 SQL 占位符一致
        let limit = i64::from(filter.limit.unwrap_or(500).clamp(1, 2000));
        let offset = i64::from(filter.offset);
        args.push(Box::new(limit));
        args.push(Box::new(offset));

        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let sql = format!(
            "SELECT {COLS} FROM assets {where_sql} {} LIMIT ? OFFSET ?",
            sort.order_by()
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::sqlite("准备资产查询", e))?;
        let rows = stmt
            .query_map(refs.as_slice(), map_asset)
            .map_err(|e| StorageError::sqlite("执行资产查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射资产行", e))?);
        }
        Ok(out)
    }

    /// 统计满足筛选条件的资产总数（**忽略 `limit` / `offset`**）。
    ///
    /// 🔴 分页的 `total` 必须来自这里，不能用 `list(...).len()`：
    /// 后者最多返回一页数据，前端算出的总页数会随翻页缩短，
    /// 用户翻到第 2 页时发现"总共只有 2 页"——这个缺陷曾在 projects 上出现过。
    ///
    /// 与 `list` 共用 `build_where`，保证"看到的条目"和"报告的总数"口径一致。
    pub fn count_filtered(&self, filter: &AssetFilter) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        let (where_sql, args) = build_where(filter);
        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let sql = format!("SELECT count(*) FROM assets {where_sql}");
        conn.query_row(&sql, refs.as_slice(), |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计筛选后资产数", e))
    }

    pub fn count_all(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM assets", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计资产数", e))
    }

    /// 高复用资产数（首页统计卡「可复用资产」的真实口径）。
    ///
    /// 口径：`reuse_score >= 0.7`（ReuseTier::Medium 及以上）。
    /// 集中定义在此处，避免首页、资产页、洞察引擎各用一套阈值。
    pub fn count_reusable(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT count(*) FROM assets WHERE reuse_score >= ?1",
            [REUSABLE_THRESHOLD],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as usize)
        .map_err(|e| StorageError::sqlite("统计可复用资产数", e))
    }

    /// 按类型统计（资产页 chips 的计数）。
    pub fn count_by_type(&self) -> Result<Vec<(AssetType, usize)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare("SELECT type, count(*) FROM assets GROUP BY type")
            .map_err(|e| StorageError::sqlite("准备类型统计", e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize))
            })
            .map_err(|e| StorageError::sqlite("执行类型统计", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (t, n) = r.map_err(|e| StorageError::sqlite("映射类型统计行", e))?;
            // 未知类型（旧库残留）直接跳过，不让整页失败
            if let Some(ty) = AssetType::parse(&t) {
                out.push((ty, n));
            } else {
                tracing::warn!(type_ = %t, "assets 表存在未知类型，已跳过");
            }
        }
        // 按领域层定义的顺序排序，保证 UI 稳定
        out.sort_by_key(|(t, _)| {
            AssetType::all()
                .iter()
                .position(|x| x == t)
                .unwrap_or(usize::MAX)
        });
        Ok(out)
    }

    /// 按项目统计资产数（项目 Tab 计数）。
    pub fn count_by_project(&self, project_id: &str) -> Result<Vec<(AssetType, usize)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare("SELECT type, count(*) FROM assets WHERE project_id = ?1 GROUP BY type")
            .map_err(|e| StorageError::sqlite("准备项目资产统计", e))?;
        let rows = stmt
            .query_map([project_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize))
            })
            .map_err(|e| StorageError::sqlite("执行项目资产统计", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (t, n) = r.map_err(|e| StorageError::sqlite("映射项目资产统计行", e))?;
            if let Some(ty) = AssetType::parse(&t) {
                out.push((ty, n));
            }
        }
        out.sort_by_key(|(t, _)| {
            AssetType::all().iter().position(|x| x == t).unwrap_or(usize::MAX)
        });
        Ok(out)
    }

    /// 记录用户反馈（Rediscovered Value 北极星指标的数据来源）。
    ///
    /// 传 `None` 表示撤销反馈。
    pub fn set_feedback(
        &self,
        id: &str,
        feedback: Option<UserFeedback>,
    ) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let n = conn
            .execute(
                "UPDATE assets SET user_feedback = ?2 WHERE id = ?1",
                params![id, feedback.map(|f| f.as_str().to_string())],
            )
            // 🔴 用户可触发的写（点「有用/无用」）。反馈是北极星指标
            // "Rediscovered Value" 的唯一数据来源，且用户可能在索引期间就点。
            // 分类 Busy → 409 让他能重试，而不是白等 5 秒后以为标注没存上。
            .map_err(|e| sqlite_err("写入资产反馈", e))?;
        Ok(n > 0)
    }

    /// 已被用户标记为"有用"的资产数（北极星指标：本月重新发现价值）。
    pub fn count_marked_useful(&self, since: Option<&str>) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        match since {
            Some(ts) => conn
                .query_row(
                    "SELECT count(*) FROM assets WHERE user_feedback = 'useful' AND created_at >= ?1",
                    [ts],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n.max(0) as usize)
                .map_err(|e| StorageError::sqlite("统计有用资产", e)),
            None => conn
                .query_row(
                    "SELECT count(*) FROM assets WHERE user_feedback = 'useful'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n.max(0) as usize)
                .map_err(|e| StorageError::sqlite("统计有用资产", e)),
        }
    }

    /// 跨项目重复的资产名（洞察引擎的输入）。
    ///
    /// 返回 `(名称, 出现项目数, 平均复用分)`，按项目数降序。
    /// 这是"你在 4 个项目中重复实现了 X"的真实数据来源。
    pub fn duplicate_names(&self, min_projects: usize) -> Result<Vec<DuplicateGroup>, StorageError> {
        let conn = self.pool.get()?;
        // 用 LOWER(name) 归一：VideoPipeline / video_pipeline 视为不同（保守），
        // 但大小写差异视为相同（Windows/macOS 常见）
        let mut stmt = conn
            .prepare(
                "SELECT LOWER(name) AS key, name, COUNT(DISTINCT project_id) AS projects,
                        AVG(reuse_score) AS avg_score, COUNT(*) AS occurrences
                 FROM assets
                 WHERE type IN ('code','component','api','prompt')
                 GROUP BY key
                 HAVING projects >= ?1
                 ORDER BY projects DESC, avg_score DESC",
            )
            .map_err(|e| StorageError::sqlite("准备重复资产查询", e))?;
        let rows = stmt
            .query_map([min_projects as i64], |r| {
                Ok(DuplicateGroup {
                    name: r.get(1)?,
                    project_count: r.get::<_, i64>(2)?.max(0) as usize,
                    avg_reuse_score: r.get::<_, f64>(3)?,
                    occurrences: r.get::<_, i64>(4)?.max(0) as usize,
                })
            })
            .map_err(|e| StorageError::sqlite("执行重复资产查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射重复资产行", e))?);
        }
        Ok(out)
    }

    /// 取某个名称在各项目中的实例（生成 Insight 的 Evidence）。
    pub fn list_by_name(&self, name: &str) -> Result<Vec<Asset>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM assets WHERE LOWER(name) = LOWER(?1) ORDER BY reuse_score DESC"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::sqlite("准备同名资产查询", e))?;
        let rows = stmt
            .query_map([name], map_asset)
            .map_err(|e| StorageError::sqlite("执行同名资产查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射同名资产行", e))?);
        }
        Ok(out)
    }

    /// 高复用资产 Top N（首页"AI 发现"与机会引擎的输入）。
    pub fn top_reusable(&self, n: u32) -> Result<Vec<Asset>, StorageError> {
        self.list(
            &AssetFilter {
                min_reuse_score: Some(REUSABLE_THRESHOLD),
                evidence_required: true,
                limit: Some(n.clamp(1, 200)),
                ..Default::default()
            },
            AssetSort::ReuseScore,
        )
    }

    /// FTS 检索：返回资产 id 与 bm25 分数（越小越相关）。
    ///
    /// ⚠️ trigram 分词器要求查询 ≥3 字符，2 字中文查询会返回空。
    /// 调用方（`projectassests-search`）负责 LIKE 回退。
    pub fn search_fts(&self, query: &str, limit: u32) -> Result<Vec<(String, f64)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare(
                "SELECT asset_id, bm25(assets_fts) FROM assets_fts
                 WHERE assets_fts MATCH ?1 ORDER BY bm25(assets_fts) LIMIT ?2",
            )
            .map_err(|e| StorageError::sqlite("准备资产 FTS 查询", e))?;
        let rows = stmt
            .query_map(params![query, i64::from(limit.clamp(1, 500))], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(|e| StorageError::sqlite("执行资产 FTS 查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射资产 FTS 行", e))?);
        }
        Ok(out)
    }

    pub fn fts_count(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM assets_fts", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计资产索引", e))
    }

    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启资产删除事务", e))?;
        // ⚠️ FTS 先删
        tx.execute("DELETE FROM assets_fts WHERE asset_id = ?1", [id])
            .map_err(|e| StorageError::sqlite("清理资产索引", e))?;
        let n = tx
            .execute("DELETE FROM assets WHERE id = ?1", [id])
            .map_err(|e| StorageError::sqlite("删除资产", e))?;
        tx.commit().map_err(|e| StorageError::sqlite("提交资产删除事务", e))?;
        Ok(n > 0)
    }

    /// 删除某项目的全部资产（重新扫描前清理旧数据）。
    pub fn delete_by_project(&self, project_id: &str) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::sqlite("开启项目资产清理事务", e))?;
        tx.execute(
            "DELETE FROM assets_fts WHERE asset_id IN (SELECT id FROM assets WHERE project_id = ?1)",
            [project_id],
        )
        .map_err(|e| StorageError::sqlite("清理项目资产索引", e))?;
        let n = tx
            .execute("DELETE FROM assets WHERE project_id = ?1", [project_id])
            .map_err(|e| StorageError::sqlite("删除项目资产", e))?;
        tx.commit()
            .map_err(|e| StorageError::sqlite("提交项目资产清理事务", e))?;
        Ok(n)
    }

    /// 全部标签及频次（资产页标签云/筛选）。
    pub fn tag_cloud(&self, limit: u32) -> Result<Vec<(String, usize)>, StorageError> {
        let conn = self.pool.get()?;
        // tags 存为 JSON 数组，SQLite 的 json_each 可展开（FTS5 bundled 版本含 JSON1）
        let mut stmt = conn
            .prepare(
                "SELECT value AS tag, count(*) AS n
                 FROM assets, json_each(assets.tags_json)
                 GROUP BY tag ORDER BY n DESC, tag ASC LIMIT ?1",
            )
            .map_err(|e| StorageError::sqlite("准备标签统计", e))?;
        let rows = stmt
            .query_map([i64::from(limit.clamp(1, 200))], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize))
            })
            .map_err(|e| StorageError::sqlite("执行标签统计", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射标签统计行", e))?);
        }
        Ok(out)
    }
}

/// "可复用"的判定阈值。集中定义，全站共用一个口径。
pub const REUSABLE_THRESHOLD: f64 = 0.70;

/// 重复资产分组。
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateGroup {
    pub name: String,
    pub project_count: usize,
    pub avg_reuse_score: f64,
    pub occurrences: usize,
}

impl DuplicateGroup {
    pub fn tier(&self) -> ReuseTier {
        ReuseTier::from_score(self.avg_reuse_score)
    }
}

/// 供 upsert 使用：新插入的资产反馈为 NULL。
/// 更新时刻意**不**写入该列（见 `upsert_conn` 的 SQL：UPDATE 列表里没有 user_feedback），
/// 保证重新抽取不会抹掉用户的历史判断。
trait AssetFeedbackSql {
    fn user_feedback_sql(&self) -> Option<String>;
}

impl AssetFeedbackSql for Asset {
    fn user_feedback_sql(&self) -> Option<String> {
        self.user_feedback.map(|f| f.as_str().to_string())
    }
}

/// 由筛选条件构造 WHERE 子句与绑定参数（不含分页）。
///
/// `list` 与 `count_filtered` 共用此函数，是保证两者口径一致的唯一办法：
/// 各写一遍的话，改了 `list` 的过滤条件却忘了改 `count`，
/// 分页总数就会与实际条目悄悄错位——这种缺陷在 UI 上表现为"翻到第 2 页发现总页数变了"，
/// 极难定位。
fn build_where(filter: &AssetFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut where_sql = String::from("WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(pid) = &filter.project_id {
        where_sql.push_str(" AND project_id = ?");
        args.push(Box::new(pid.clone()));
    }

    // 单选类型与多选类型合并处理
    let mut types: Vec<AssetType> = filter.asset_types.clone();
    if let Some(t) = filter.asset_type
        && !types.contains(&t)
    {
        types.push(t);
    }
    if !types.is_empty() {
        // 动态生成 IN (?, ?, …)：占位符个数由类型数决定，值仍走参数绑定
        let placeholders = vec!["?"; types.len()].join(",");
        where_sql.push_str(&format!(" AND type IN ({placeholders})"));
        for t in types {
            args.push(Box::new(t.as_str().to_string()));
        }
    }

    if let Some(k) = filter
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        // 与 projects 一致：必须转义 `%`/`_` 并声明 ESCAPE，
        // 否则搜 "100%" 会变成通配符匹配，返回大量无关资产。
        where_sql.push_str(
            " AND (name LIKE ? ESCAPE '\\' OR description LIKE ? ESCAPE '\\'\
              OR tags_json LIKE ? ESCAPE '\\' OR source_path LIKE ? ESCAPE '\\')",
        );
        let pat = crate::fts::like_pattern(k);
        for _ in 0..4 {
            args.push(Box::new(pat.clone()));
        }
    }

    if let Some(min) = filter.min_reuse_score {
        where_sql.push_str(" AND reuse_score >= ?");
        args.push(Box::new(min));
    }

    if filter.evidence_required {
        // evidence_json 为 '{}' 或空表示无证据（领域层已挡一道，这里是双保险）
        where_sql.push_str(
            " AND evidence_json <> '{}' AND evidence_json <> '' AND evidence_json IS NOT NULL",
        );
    }

    (where_sql, args)
}

pub(crate) fn map_asset(r: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    let type_str: String = r.get(2)?;
    let fb: Option<String> = r.get(14)?;
    Ok(Asset {
        id: row::text(r, 0)?,
        project_id: row::text(r, 1)?,
        // 未知类型降级为 Code（最常见），而非让整页失败
        asset_type: AssetType::parse(&type_str).unwrap_or(AssetType::Code),
        name: row::text(r, 3)?,
        description: row::text(r, 4)?,
        content: row::text_opt(r, 5)?,
        source_path: row::text(r, 6)?,
        confidence: row::real(r, 7)?,
        reuse_score: row::real(r, 8)?,
        generality: row::real(r, 9)?,
        stability: row::real(r, 10)?,
        tags: row::json_col::<Vec<String>>(r, 11)?,
        evidence: row::json_col::<Evidence>(r, 12)?,
        created_at: row::text(r, 13)?,
        // 未知反馈值（旧库残留）视为无反馈，不报错
        user_feedback: fb.as_deref().and_then(UserFeedback::parse),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use projectassests_domain::{CodeStats, Project};

    fn mk_project(d: &Database, id: &str, path: &str) {
        d.projects()
            .upsert(&Project {
                id: id.into(),
                name: id.into(),
                path: path.into(),
                description: String::new(),
                language: "Python".into(),
                framework: "-".into(),
                created_at: None,
                updated_at: None,
                last_commit_at: None,
                status: projectassests_domain::ProjectStatus::Active,
                health_score: 80,
                completeness: None,
                tags: vec![],
                sensitive: false,
                stats: CodeStats {
                    files: 10,
                    loc: 500,
                    symbols: 5,
                    modules: 2,
                    languages: vec![],
                },
                scan: projectassests_domain::ScanFacts::default(),
                ai_profile: None,
            })
            .unwrap();
    }

    fn asset(id: &str, pid: &str, name: &str, ty: AssetType, score: f64) -> Asset {
        Asset {
            id: id.into(),
            project_id: pid.into(),
            asset_type: ty,
            name: name.into(),
            description: format!("{name} 的描述"),
            content: None,
            source_path: format!("src/{name}.py"),
            confidence: 0.9,
            reuse_score: score,
            generality: 0.8,
            stability: 0.7,
            tags: vec!["python".into(), "demo".into()],
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
    fn upsert_and_get_roundtrips() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert(&asset("a1", "p1", "VideoPipeline", AssetType::Component, 0.91)).unwrap();
        let a = d.assets().get("a1").unwrap().unwrap();
        assert_eq!(a.name, "VideoPipeline");
        assert_eq!(a.asset_type, AssetType::Component);
        assert!((a.reuse_score - 0.91).abs() < 1e-9);
        assert_eq!(a.evidence.files, vec!["src/VideoPipeline.py".to_string()]);
        assert_eq!(a.evidence.commits, vec!["abc1234".to_string()]);
        assert_eq!(a.tags.len(), 2);
    }

    /// 产品红线：无证据的资产不得入库。
    #[test]
    fn asset_without_evidence_is_skipped() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let mut a = asset("a1", "p1", "Ghost", AssetType::Code, 0.99);
        a.evidence = Evidence::default();
        d.assets().upsert(&a).unwrap(); // 不报错，静默跳过
        assert!(d.assets().get("a1").unwrap().is_none(), "无证据资产不应入库");
        assert_eq!(d.assets().count_all().unwrap(), 0);
    }

    #[test]
    fn batch_upsert_writes_all() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let list: Vec<Asset> = (0..20)
            .map(|i| asset(&format!("a{i}"), "p1", &format!("Asset{i}"), AssetType::Code, 0.8))
            .collect();
        let outcome = d.assets().upsert_batch(&list).unwrap();
        assert_eq!(outcome.written, 20);
        assert_eq!(outcome.rejected_no_evidence, 0);
        assert_eq!(outcome.total(), list.len());
        assert_eq!(d.assets().count_all().unwrap(), 20);
        assert_eq!(d.assets().fts_count().unwrap(), 20);
    }

    /// 🔴 证据门禁的拒绝必须可观测：静默丢弃会让扫描任务谎报资产数。
    #[test]
    fn batch_upsert_reports_rejected_without_evidence() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let mut no_evidence = asset("a_bad", "p1", "NoEvidence", AssetType::Code, 0.8);
        no_evidence.evidence = Evidence::default(); // files 与 commits 都为空
        let ok = asset("a_ok", "p1", "HasEvidence", AssetType::Code, 0.8);

        let outcome = d.assets().upsert_batch(&[no_evidence, ok]).unwrap();
        assert_eq!(outcome.written, 1, "只有带证据的应入库");
        assert_eq!(outcome.rejected_no_evidence, 1);
        assert!(outcome.has_rejections());
        assert_eq!(outcome.total(), 2);
        // 关键：库里确实只有 1 条，且是带证据的那条
        assert_eq!(d.assets().count_all().unwrap(), 1);
        assert!(d.assets().get("a_bad").unwrap().is_none());
        assert!(d.assets().get("a_ok").unwrap().is_some());
        // FTS 不得为被拒资产留下孤儿索引行
        assert_eq!(d.assets().fts_count().unwrap(), 1);
    }

    /// 单条 upsert 的返回值必须如实反映"写没写进去"。
    #[test]
    fn single_upsert_returns_whether_written() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let ok = asset("a1", "p1", "Good", AssetType::Code, 0.8);
        assert!(d.assets().upsert(&ok).unwrap(), "带证据应写入");

        let mut bad = asset("a2", "p1", "Bad", AssetType::Code, 0.8);
        bad.evidence = Evidence::default();
        assert!(!d.assets().upsert(&bad).unwrap(), "无证据应被拒且如实返回 false");
    }

    /// 全部被拒时不应开启空事务，也不应报错。
    #[test]
    fn batch_upsert_all_rejected_is_not_an_error() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let mut bad = asset("a1", "p1", "Bad", AssetType::Code, 0.8);
        bad.evidence = Evidence::default();
        let outcome = d.assets().upsert_batch(&[bad.clone(), bad]).unwrap();
        assert_eq!(outcome.written, 0);
        assert_eq!(outcome.rejected_no_evidence, 2);
        assert_eq!(d.assets().count_all().unwrap(), 0);
    }

    #[test]
    fn empty_batch_is_a_noop() {
        let d = Database::in_memory().unwrap();
        let outcome = d.assets().upsert_batch(&[]).unwrap();
        assert_eq!(outcome, AssetWriteOutcome::default());
        assert!(!outcome.has_rejections());
    }

    /// 重新抽取不得抹掉用户反馈（反馈是北极星指标的数据来源）。
    #[test]
    fn reupsert_preserves_user_feedback() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert(&asset("a1", "p1", "X", AssetType::Code, 0.8)).unwrap();
        assert!(d.assets().set_feedback("a1", Some(UserFeedback::Useful)).unwrap());
        // 重新抽取同一资产（分数变化）
        d.assets().upsert(&asset("a1", "p1", "X", AssetType::Code, 0.95)).unwrap();
        let conn = d.conn().unwrap();
        let fb: Option<String> = conn
            .query_row("SELECT user_feedback FROM assets WHERE id='a1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fb.as_deref(), Some("useful"), "用户反馈必须保留");
    }

    #[test]
    fn list_filters_by_type() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "C", AssetType::Code, 0.8),
            asset("a2", "p1", "K", AssetType::Knowledge, 0.7),
            asset("a3", "p1", "D", AssetType::Decision, 0.6),
        ]).unwrap();
        let code = d.assets()
            .list(&AssetFilter { asset_type: Some(AssetType::Code), ..Default::default() }, AssetSort::Name)
            .unwrap();
        assert_eq!(code.len(), 1);
        assert_eq!(code[0].id, "a1");
    }

    #[test]
    fn list_filters_by_multiple_types() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "C", AssetType::Code, 0.8),
            asset("a2", "p1", "K", AssetType::Knowledge, 0.7),
            asset("a3", "p1", "D", AssetType::Decision, 0.6),
        ]).unwrap();
        let hits = d.assets().list(
            &AssetFilter { asset_types: vec![AssetType::Code, AssetType::Knowledge], ..Default::default() },
            AssetSort::Name,
        ).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn list_filters_by_project() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        mk_project(&d, "p2", "/tmp/b");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "X", AssetType::Code, 0.8),
            asset("a2", "p2", "Y", AssetType::Code, 0.8),
        ]).unwrap();
        let hits = d.assets()
            .list(&AssetFilter { project_id: Some("p2".into()), ..Default::default() }, AssetSort::Name)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a2");
    }

    #[test]
    fn list_filters_by_min_reuse_score() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "High", AssetType::Code, 0.92),
            asset("a2", "p1", "Low", AssetType::Code, 0.4),
        ]).unwrap();
        let hits = d.assets()
            .list(&AssetFilter { min_reuse_score: Some(0.7), ..Default::default() }, AssetSort::ReuseScore)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a1");
    }

    #[test]
    fn list_keyword_matches_chinese() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let mut a = asset("a1", "p1", "角色一致性", AssetType::Component, 0.86);
        a.description = "基于 LoRA 的角色一致性方案".into();
        let b = asset("a2", "p1", "Other", AssetType::Code, 0.8);
        d.assets().upsert_batch(&[a, b]).unwrap();
        let hits = d.assets()
            .list(&AssetFilter { keyword: Some("一致性".into()), ..Default::default() }, AssetSort::Name)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a1");
    }

    #[test]
    fn list_sort_by_reuse_score_desc() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "Low", AssetType::Code, 0.5),
            asset("a2", "p1", "High", AssetType::Code, 0.95),
            asset("a3", "p1", "Mid", AssetType::Code, 0.75),
        ]).unwrap();
        let hits = d.assets().list(&AssetFilter::default(), AssetSort::ReuseScore).unwrap();
        assert_eq!(hits[0].id, "a2");
        assert_eq!(hits[2].id, "a1");
    }

    #[test]
    fn sort_order_by_is_hardcoded() {
        for s in [AssetSort::ReuseScore, AssetSort::Confidence, AssetSort::Newest, AssetSort::Name] {
            let sql = s.order_by();
            assert!(sql.starts_with("ORDER BY"));
            assert!(!sql.contains('?') && !sql.contains(';'), "不应含占位符或分号: {sql}");
        }
    }

    #[test]
    fn count_reusable_uses_shared_threshold() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "A", AssetType::Code, 0.91),
            asset("a2", "p1", "B", AssetType::Code, 0.70), // 恰好等于阈值，应计入
            asset("a3", "p1", "C", AssetType::Code, 0.69),
        ]).unwrap();
        assert_eq!(d.assets().count_reusable().unwrap(), 2);
        assert_eq!(REUSABLE_THRESHOLD, 0.70);
    }

    #[test]
    fn count_by_type_in_domain_order() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "K1", AssetType::Knowledge, 0.7),
            asset("a2", "p1", "K2", AssetType::Knowledge, 0.7),
            asset("a3", "p1", "C1", AssetType::Code, 0.7),
        ]).unwrap();
        let c = d.assets().count_by_type().unwrap();
        // 领域层 all() 顺序：Code 在 Knowledge 之前
        assert_eq!(c[0].0, AssetType::Code);
        assert_eq!(c[0].1, 1);
        assert_eq!(c[1].0, AssetType::Knowledge);
        assert_eq!(c[1].1, 2);
    }

    #[test]
    fn count_by_project_scopes_correctly() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        mk_project(&d, "p2", "/tmp/b");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "X", AssetType::Code, 0.8),
            asset("a2", "p1", "Y", AssetType::Decision, 0.8),
            asset("a3", "p2", "Z", AssetType::Code, 0.8),
        ]).unwrap();
        let c = d.assets().count_by_project("p1").unwrap();
        assert_eq!(c.iter().map(|(_, n)| n).sum::<usize>(), 2);
    }

    /// "你在 N 个项目中重复实现了 X" 的真实数据来源。
    #[test]
    fn duplicate_names_groups_across_projects() {
        let d = Database::in_memory().unwrap();
        for i in 1..=4 {
            mk_project(&d, &format!("p{i}"), &format!("/tmp/{i}"));
        }
        d.assets().upsert_batch(&[
            asset("a1", "p1", "TaskQueue", AssetType::Code, 0.9),
            asset("a2", "p2", "TaskQueue", AssetType::Code, 0.85),
            asset("a3", "p3", "TaskQueue", AssetType::Code, 0.8),
            asset("a4", "p4", "TaskQueue", AssetType::Code, 0.75),
            asset("a5", "p1", "Unique", AssetType::Code, 0.9),
        ]).unwrap();

        let dups = d.assets().duplicate_names(2).unwrap();
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].name, "TaskQueue");
        assert_eq!(dups[0].project_count, 4);
        assert_eq!(dups[0].occurrences, 4);
        assert!((dups[0].avg_reuse_score - 0.825).abs() < 1e-6);
    }

    /// 同一项目内的多次出现不算"跨项目重复"（否则会误报）。
    #[test]
    fn duplicates_count_distinct_projects() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "Helper", AssetType::Code, 0.9),
            asset("a2", "p1", "Helper", AssetType::Code, 0.9),
        ]).unwrap();
        assert!(d.assets().duplicate_names(2).unwrap().is_empty());
    }

    #[test]
    fn duplicate_min_projects_filter() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        mk_project(&d, "p2", "/tmp/b");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "Q", AssetType::Code, 0.9),
            asset("a2", "p2", "Q", AssetType::Code, 0.9),
        ]).unwrap();
        assert_eq!(d.assets().duplicate_names(2).unwrap().len(), 1);
        assert_eq!(d.assets().duplicate_names(3).unwrap().len(), 0);
    }

    /// 知识/决策/经验类不参与重复检测（它们本就可能合理重复）。
    #[test]
    fn duplicates_exclude_knowledge_types() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        mk_project(&d, "p2", "/tmp/b");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "SQLite 选型", AssetType::Decision, 0.9),
            asset("a2", "p2", "SQLite 选型", AssetType::Decision, 0.9),
        ]).unwrap();
        assert!(d.assets().duplicate_names(2).unwrap().is_empty());
    }

    #[test]
    fn list_by_name_is_case_insensitive() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert(&asset("a1", "p1", "TaskQueue", AssetType::Code, 0.9)).unwrap();
        assert_eq!(d.assets().list_by_name("taskqueue").unwrap().len(), 1);
        assert_eq!(d.assets().list_by_name("TASKQUEUE").unwrap().len(), 1);
        assert!(d.assets().list_by_name("nope").unwrap().is_empty());
    }

    #[test]
    fn top_reusable_requires_evidence_and_threshold() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let mut no_ev = asset("a2", "p1", "NoEvidence", AssetType::Code, 0.99);
        no_ev.evidence = Evidence::default();
        d.assets().upsert_batch(&[
            asset("a1", "p1", "Good", AssetType::Code, 0.92),
            asset("a3", "p1", "LowScore", AssetType::Code, 0.5),
        ]).unwrap();
        d.assets().upsert(&no_ev).unwrap();
        let top = d.assets().top_reusable(10).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].id, "a1");
    }

    #[test]
    fn fts_search_matches_english() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert(&asset("a1", "p1", "VideoPipeline", AssetType::Component, 0.9)).unwrap();
        let hits = d.assets().search_fts("VideoPipeline", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "a1");
    }

    /// trigram 对 ≥3 字中文有效；2 字需上层 LIKE 回退。
    #[test]
    fn fts_search_chinese_needs_three_chars() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let mut a = asset("a1", "p1", "角色一致性方案", AssetType::Component, 0.9);
        a.description = "基于 LoRA 的角色一致性".into();
        d.assets().upsert(&a).unwrap();
        assert_eq!(d.assets().search_fts("角色一致", 10).unwrap().len(), 1);
        assert!(d.assets().search_fts("角色", 10).unwrap().is_empty(), "2 字查询 trigram 无法命中");
    }

    #[test]
    fn delete_removes_asset_and_fts() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert(&asset("a1", "p1", "X", AssetType::Code, 0.8)).unwrap();
        assert!(d.assets().delete("a1").unwrap());
        assert!(d.assets().get("a1").unwrap().is_none());
        assert_eq!(d.assets().fts_count().unwrap(), 0);
        assert!(!d.assets().delete("a1").unwrap());
    }

    #[test]
    fn delete_by_project_scopes() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        mk_project(&d, "p2", "/tmp/b");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "X", AssetType::Code, 0.8),
            asset("a2", "p2", "Y", AssetType::Code, 0.8),
        ]).unwrap();
        assert_eq!(d.assets().delete_by_project("p1").unwrap(), 1);
        assert_eq!(d.assets().count_all().unwrap(), 1);
        assert_eq!(d.assets().fts_count().unwrap(), 1, "FTS 不得残留孤儿行");
    }

    /// 外键约束：资产必须属于存在的项目。
    #[test]
    fn asset_requires_existing_project() {
        let d = Database::in_memory().unwrap();
        let r = d.assets().upsert(&asset("a1", "ghost", "X", AssetType::Code, 0.8));
        assert!(r.is_err(), "外键应拒绝孤儿资产");
    }

    #[test]
    fn tag_cloud_aggregates() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert_batch(&[
            asset("a1", "p1", "X", AssetType::Code, 0.8),
            asset("a2", "p1", "Y", AssetType::Code, 0.8),
        ]).unwrap();
        let cloud = d.assets().tag_cloud(10).unwrap();
        let python = cloud.iter().find(|(t, _)| t == "python").unwrap();
        assert_eq!(python.1, 2);
    }

    #[test]
    fn feedback_can_be_cleared() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        d.assets().upsert(&asset("a1", "p1", "X", AssetType::Code, 0.8)).unwrap();
        d.assets().set_feedback("a1", Some(UserFeedback::Useful)).unwrap();
        assert_eq!(d.assets().count_marked_useful(None).unwrap(), 1);
        d.assets().set_feedback("a1", None).unwrap();
        assert_eq!(d.assets().count_marked_useful(None).unwrap(), 0);
    }

    #[test]
    fn count_marked_useful_respects_since() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let mut a = asset("a1", "p1", "X", AssetType::Code, 0.8);
        a.created_at = "2020-01-01".into();
        d.assets().upsert(&a).unwrap();
        d.assets().set_feedback("a1", Some(UserFeedback::Useful)).unwrap();
        assert_eq!(d.assets().count_marked_useful(None).unwrap(), 1);
        assert_eq!(d.assets().count_marked_useful(Some("2025-01-01")).unwrap(), 0);
    }

    #[test]
    fn set_feedback_on_missing_asset_returns_false() {
        let d = Database::in_memory().unwrap();
        assert!(!d.assets().set_feedback("nope", Some(UserFeedback::Useful)).unwrap());
    }

    #[test]
    fn duplicate_group_tier() {
        let g = DuplicateGroup { name: "X".into(), project_count: 3, avg_reuse_score: 0.9, occurrences: 3 };
        assert_eq!(g.tier(), ReuseTier::High);
    }

    #[test]
    fn list_pagination() {
        let d = Database::in_memory().unwrap();
        mk_project(&d, "p1", "/tmp/a");
        let list: Vec<Asset> = (0..15)
            .map(|i| asset(&format!("a{i:02}"), "p1", &format!("A{i:02}"), AssetType::Code, 0.8))
            .collect();
        d.assets().upsert_batch(&list).unwrap();
        let page = d.assets()
            .list(&AssetFilter { limit: Some(5), offset: 5, ..Default::default() }, AssetSort::Name)
            .unwrap();
        assert_eq!(page.len(), 5);
        assert_eq!(page[0].id, "a05");
    }
}
