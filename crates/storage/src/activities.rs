//! 活动流 Repository（首页「最近活动」的真实来源）。
//!
//! 原型期这块是写死的 mock（"发现 3 个可复用的组件 · 2 小时前"）。
//! 真实版由各引擎在完成动作后追加一条记录，首页按时间倒序读取。

use rusqlite::{params, Connection};

use projectassests_domain::StorageError;

use crate::pool::Pool;
use crate::row::{self, now_utc};

/// 一条活动记录。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Activity {
    pub id: String,
    /// 图标键（前端映射为 SVG 图标名）
    pub icon: ActivityIcon,
    pub title: String,
    pub detail: String,
    pub created_at: String,
    /// 相对时间文案（"2 小时前"）。由存储层统一计算，避免前端各处格式不一。
    pub relative: String,
}

/// 活动图标。用枚举而非自由字符串：防止各引擎写出不存在的图标键导致前端渲染空白。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityIcon {
    /// 发现可复用组件
    Repeat,
    /// 完成分析
    Check,
    /// 新增关联
    Link,
    /// 生成洞察
    Bulb,
    /// 扫描
    Scan,
    /// 警告/失败
    Alert,
}

impl ActivityIcon {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Repeat => "repeat",
            Self::Check => "check",
            Self::Link => "link",
            Self::Bulb => "bulb",
            Self::Scan => "scan",
            Self::Alert => "alert",
        }
    }

    /// 解析数据库值；未知值降级为 Check（中性图标）而非报错。
    pub fn parse(s: &str) -> Self {
        match s {
            "repeat" => Self::Repeat,
            "check" => Self::Check,
            "link" => Self::Link,
            "bulb" => Self::Bulb,
            "scan" => Self::Scan,
            "alert" => Self::Alert,
            _ => Self::Check,
        }
    }
}

/// 活动仓储。
#[derive(Debug)]
pub struct ActivityRepo<'a> {
    pool: &'a Pool,
}

impl<'a> ActivityRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// 追加一条活动。`id` 为空时自动生成 uuid。
    pub fn push(&self, icon: ActivityIcon, title: impl Into<String>, detail: impl Into<String>) -> Result<Activity, StorageError> {
        let conn = self.pool.get()?;
        Self::push_conn(&conn, icon, title.into(), detail.into())
    }

    /// 在已有事务/连接上追加（供批量操作共用一个事务）。
    pub fn push_conn(
        conn: &Connection,
        icon: ActivityIcon,
        title: String,
        detail: String,
    ) -> Result<Activity, StorageError> {
        let id = uuid::Uuid::new_v4().to_string();
        let at = now_utc();
        conn.execute(
            "INSERT INTO activities (id, icon, title, detail, created_at) VALUES (?1,?2,?3,?4,?5)",
            params![id, icon.as_str(), title, detail, at],
        )
        .map_err(|e| StorageError::sqlite("写入活动", e))?;
        Ok(Activity {
            id,
            icon,
            title,
            detail,
            created_at: at.clone(),
            relative: row::relative_time(&at, chrono::Utc::now()),
        })
    }

    /// 最近 N 条活动。
    pub fn recent(&self, limit: u32) -> Result<Vec<Activity>, StorageError> {
        let conn = self.pool.get()?;
        let now = chrono::Utc::now();
        let mut stmt = conn
            .prepare(
                "SELECT id, icon, title, detail, created_at FROM activities
                 ORDER BY created_at DESC, rowid DESC LIMIT ?1",
            )
            .map_err(|e| StorageError::sqlite("准备活动查询", e))?;
        let rows = stmt
            .query_map(params![i64::from(limit.clamp(1, 100))], |r| {
                let icon_str: String = r.get(1)?;
                let at: String = r.get(4)?;
                Ok(Activity {
                    id: r.get(0)?,
                    icon: ActivityIcon::parse(&icon_str),
                    title: r.get(2)?,
                    detail: r.get(3)?,
                    relative: row::relative_time(&at, now),
                    created_at: at,
                })
            })
            .map_err(|e| StorageError::sqlite("执行活动查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射活动行", e))?);
        }
        Ok(out)
    }

    pub fn count(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM activities", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计活动数", e))
    }

    /// 只保留最近 N 条，清理更早的记录（活动流会无限增长）。
    pub fn trim(&self, keep: usize) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.execute(
            "DELETE FROM activities WHERE rowid NOT IN (
                SELECT rowid FROM activities ORDER BY created_at DESC, rowid DESC LIMIT ?1
             )",
            params![keep as i64],
        )
        .map_err(|e| StorageError::sqlite("清理活动流", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    #[test]
    fn push_and_read_back() {
        let d = db();
        let a = d.activities().push(ActivityIcon::Check, "完成项目分析: yingTech", "提取 12 个能力").unwrap();
        assert!(!a.id.is_empty());
        assert_eq!(a.icon, ActivityIcon::Check);
        assert!(!a.relative.is_empty(), "相对时间应由存储层计算");

        let list = d.activities().recent(10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "完成项目分析: yingTech");
        assert_eq!(list[0].detail, "提取 12 个能力");
        assert_eq!(list[0].icon, ActivityIcon::Check);
    }

    #[test]
    fn recent_orders_newest_first() {
        let d = db();
        d.activities().push(ActivityIcon::Scan, "第一条", "").unwrap();
        // 手工写入更早的时间戳，确保排序可验证（同秒插入时 created_at 相同）
        let conn = d.conn().unwrap();
        conn.execute(
            "UPDATE activities SET created_at='2020-01-01T00:00:00Z' WHERE title='第一条'",
            [],
        ).unwrap();
        drop(conn);
        d.activities().push(ActivityIcon::Bulb, "第二条", "").unwrap();

        let list = d.activities().recent(10).unwrap();
        assert_eq!(list[0].title, "第二条");
        assert_eq!(list[1].title, "第一条");
    }

    #[test]
    fn recent_respects_limit() {
        let d = db();
        for i in 0..20 {
            d.activities().push(ActivityIcon::Check, format!("活动 {i}"), "").unwrap();
        }
        assert_eq!(d.activities().recent(5).unwrap().len(), 5);
        assert_eq!(d.activities().count().unwrap(), 20);
    }

    #[test]
    fn trim_keeps_newest() {
        let d = db();
        for i in 0..10 {
            d.activities().push(ActivityIcon::Check, format!("a{i}"), "").unwrap();
        }
        let removed = d.activities().trim(4).unwrap();
        assert_eq!(removed, 6);
        assert_eq!(d.activities().count().unwrap(), 4);
    }

    #[test]
    fn trim_is_noop_when_under_limit() {
        let d = db();
        d.activities().push(ActivityIcon::Check, "a", "").unwrap();
        assert_eq!(d.activities().trim(10).unwrap(), 0);
        assert_eq!(d.activities().count().unwrap(), 1);
    }

    #[test]
    fn unknown_icon_degrades_to_check() {
        let d = db();
        d.activities().push(ActivityIcon::Link, "x", "").unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE activities SET icon='future-icon'", []).unwrap();
        drop(conn);
        assert_eq!(d.activities().recent(1).unwrap()[0].icon, ActivityIcon::Check);
    }

    #[test]
    fn icon_roundtrips() {
        for i in [
            ActivityIcon::Repeat,
            ActivityIcon::Check,
            ActivityIcon::Link,
            ActivityIcon::Bulb,
            ActivityIcon::Scan,
            ActivityIcon::Alert,
        ] {
            assert_eq!(ActivityIcon::parse(i.as_str()), i);
        }
    }

    #[test]
    fn relative_time_is_human_readable() {
        let d = db();
        d.activities().push(ActivityIcon::Check, "x", "").unwrap();
        let conn = d.conn().unwrap();
        conn.execute(
            "UPDATE activities SET created_at='2020-01-01T00:00:00Z'",
            [],
        ).unwrap();
        drop(conn);
        let a = &d.activities().recent(1).unwrap()[0];
        assert!(a.relative.contains("年前"), "应为 {}", a.relative);
    }

    #[test]
    fn empty_stream_returns_empty_vec() {
        assert!(db().activities().recent(10).unwrap().is_empty());
    }
}
