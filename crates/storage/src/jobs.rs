//! 任务 Repository。
//!
//! 《技术设计书》§15：UI 不允许直接调 scan()/analyze()，一切走任务队列，
//! 且所有任务可取消、进度可见。本模块负责任务的持久化与状态查询；
//! 调度与并发控制在 `spolia-jobs`。

use rusqlite::{params, Connection, OptionalExtension};

use spolia_domain::{Job, JobStatus, JobType, StorageError};

use crate::pool::Pool;
use crate::row::{self, now_utc};

/// 任务仓储。
#[derive(Debug)]
pub struct JobRepo<'a> {
    pool: &'a Pool,
}

const COLS: &str = "id, type, status, progress, stage, processed, total, error, payload_json, created_at, updated_at";

impl<'a> JobRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// 创建任务（初始 queued）。
    pub fn create(&self, id: &str, job_type: JobType, payload: Option<&serde_json::Value>) -> Result<Job, StorageError> {
        let conn = self.pool.get()?;
        let now = now_utc();
        let payload_json = payload.map(row::to_json_or_null);
        conn.execute(
            "INSERT INTO jobs (id, type, status, progress, stage, processed, total, error, payload_json, created_at, updated_at)
             VALUES (?1,?2,'queued',0,NULL,NULL,NULL,NULL,?3,?4,?4)",
            params![id, job_type.as_str(), payload_json, now],
        )
        .map_err(|e| StorageError::sqlite("创建任务", e))?;
        self.get(id)?
            .ok_or_else(|| StorageError::sqlite("创建后读回任务失败", id.to_string()))
    }

    pub fn get(&self, id: &str) -> Result<Option<Job>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM jobs WHERE id = ?1");
        conn.query_row(&sql, [id], map_job)
            .optional()
            .map_err(|e| StorageError::sqlite("查询任务", e))
    }

    /// 更新任务的进度/状态。`updated_at` 自动刷新。
    ///
    /// 🔴 终态保护：若任务已是 completed/failed/cancelled，此更新被忽略并返回 `Ok(false)`。
    /// 这对应"取消后进度条又跳回"这类竞态 bug——用户点了取消，后台线程的最后一次
    /// 进度上报不得覆盖 cancelled 状态。
    pub fn update(&self, job: &Job) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        // 乐观并发：只有当前仍处于非终态时才允许更新
        let n = conn
            .execute(
                "UPDATE jobs SET
                    status = ?2, progress = ?3, stage = ?4, processed = ?5,
                    total = ?6, error = ?7, updated_at = ?8
                 WHERE id = ?1 AND status NOT IN ('completed','failed','cancelled')",
                params![
                    job.id,
                    job.status.as_str(),
                    job.progress,
                    job.stage,
                    job.processed.map(|v| v as i64),
                    job.total.map(|v| v as i64),
                    job.error,
                    now_utc(),
                ],
            )
            .map_err(|e| StorageError::sqlite("更新任务", e))?;
        Ok(n > 0)
    }

    /// 直接置为某终态（完成/失败/取消）。
    ///
    /// 与 `update` 不同：此方法**允许**从任意非终态转入终态，
    /// 但同样拒绝覆盖已存在的终态（先到先得）。
    pub fn set_terminal(&self, id: &str, status: JobStatus, error: Option<&str>) -> Result<bool, StorageError> {
        if !status.is_terminal() {
            return Err(StorageError::sqlite(
                "set_terminal 只接受终态",
                status.as_str().to_string(),
            ));
        }
        let conn = self.pool.get()?;
        let n = conn
            .execute(
                "UPDATE jobs SET status = ?2, error = ?3, progress = CASE
                    WHEN ?2 = 'completed' THEN 1.0 ELSE progress END, updated_at = ?4
                 WHERE id = ?1 AND status NOT IN ('completed','failed','cancelled')",
                params![id, status.as_str(), error, now_utc()],
            )
            .map_err(|e| StorageError::sqlite("设置任务终态", e))?;
        Ok(n > 0)
    }

    /// 当前是否有同类任务在运行或排队（防重复触发扫描）。
    pub fn has_active_of_type(&self, job_type: JobType) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM jobs WHERE type = ?1 AND status IN ('queued','running')",
                [job_type.as_str()],
                |r| r.get(0),
            )
            .map_err(|e| StorageError::sqlite("查询活跃任务", e))?;
        Ok(n > 0)
    }

    /// 正在运行的任务（侧栏索引进度卡）。
    pub fn running(&self) -> Result<Vec<Job>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!(
            "SELECT {COLS} FROM jobs WHERE status = 'running' ORDER BY updated_at DESC"
        );
        Self::query_vec(&conn, &sql, [], "查询运行中任务")
    }

    /// 最近的任务（任务中心列表），默认按创建时间倒序。
    pub fn recent(&self, limit: u32) -> Result<Vec<Job>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM jobs ORDER BY created_at DESC LIMIT ?1");
        Self::query_vec(&conn, &sql, params![i64::from(limit.clamp(1, 200))], "查询最近任务")
    }

    /// 按状态查询。
    pub fn list_by_status(&self, status: JobStatus) -> Result<Vec<Job>, StorageError> {
        let conn = self.pool.get()?;
        let sql = format!("SELECT {COLS} FROM jobs WHERE status = ?1 ORDER BY created_at DESC");
        Self::query_vec(&conn, &sql, params![status.as_str()], "按状态查询任务")
    }

    pub fn count(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .map_err(|e| StorageError::sqlite("统计任务数", e))
    }

    /// 综合进度：所有活跃任务（queued+running）的平均进度 0.0-1.0。
    ///
    /// 侧栏"本地索引中 68%"的真实来源。无活跃任务时返回 `None`
    /// （前端隐藏进度卡，而不是显示假的 68%）。
    pub fn overall_progress(&self) -> Result<Option<f64>, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT AVG(progress), count(*) FROM jobs WHERE status IN ('queued','running')",
            [],
            |r| {
                let avg: Option<f64> = r.get(0)?;
                let n: i64 = r.get(1)?;
                Ok(if n == 0 { None } else { avg })
            },
        )
        .map_err(|e| StorageError::sqlite("统计总体进度", e))
    }

    /// 活跃任务的已处理/总数汇总（"127 / 183 projects"）。
    pub fn overall_counter(&self) -> Result<Option<(u64, u64)>, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT COALESCE(SUM(processed),0), COALESCE(SUM(total),0)
             FROM jobs WHERE status IN ('queued','running')",
            [],
            |r| {
                let p: i64 = r.get(0)?;
                let t: i64 = r.get(1)?;
                let p = p.max(0) as u64;
                let t = t.max(0) as u64;
                Ok(if t == 0 { None } else { Some((p, t)) })
            },
        )
        .map_err(|e| StorageError::sqlite("统计总体计数", e))
    }

    /// 删除已完成的历史任务（任务中心清理），保留活跃任务。
    pub fn purge_finished(&self, keep_recent: usize) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        // 保留最近 N 条终态任务用于排查，其余清理
        conn.execute(
            "DELETE FROM jobs WHERE status IN ('completed','failed','cancelled')
             AND id NOT IN (
                SELECT id FROM jobs WHERE status IN ('completed','failed','cancelled')
                ORDER BY updated_at DESC LIMIT ?1
             )",
            params![keep_recent as i64],
        )
        .map_err(|e| StorageError::sqlite("清理历史任务", e))
    }

    pub fn delete(&self, id: &str) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let n = conn
            .execute("DELETE FROM jobs WHERE id = ?1", [id])
            .map_err(|e| StorageError::sqlite("删除任务", e))?;
        Ok(n > 0)
    }

    /// 启动时把所有遗留的 queued/running 任务标记为失败。
    ///
    /// 崩溃或强杀后，库里可能残留"永远在 running"的任务，
    /// 侧栏会一直显示"索引中 68%"。启动时清理是标准做法。
    pub fn reap_stale(&self) -> Result<usize, StorageError> {
        let conn = self.pool.get()?;
        conn.execute(
            "UPDATE jobs SET status = 'failed', error = '进程重启，任务已中断', updated_at = ?1
             WHERE status IN ('queued','running')",
            params![now_utc()],
        )
        .map_err(|e| StorageError::sqlite("清理遗留任务", e))
    }

    fn query_vec(
        conn: &Connection,
        sql: &str,
        args: impl rusqlite::Params,
        ctx: &str,
    ) -> Result<Vec<Job>, StorageError> {
        let mut stmt = conn.prepare(sql).map_err(|e| StorageError::sqlite(format!("准备: {ctx}"), e))?;
        let rows = stmt
            .query_map(args, map_job)
            .map_err(|e| StorageError::sqlite(format!("执行: {ctx}"), e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite(format!("映射: {ctx}"), e))?);
        }
        Ok(out)
    }
}

fn map_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let type_str: String = r.get(1)?;
    let status_str: String = r.get(2)?;
    let processed: Option<i64> = r.get(5)?;
    let total: Option<i64> = r.get(6)?;
    let payload_raw: Option<String> = r.get(8)?;
    Ok(Job {
        id: row::text(r, 0)?,
        job_type: JobType::parse(&type_str).unwrap_or(JobType::IndexCode),
        status: JobStatus::parse(&status_str).unwrap_or_default(),
        progress: row::real(r, 3)?,
        stage: row::text_opt(r, 4)?,
        processed: processed.map(|v| v.max(0) as u64),
        total: total.map(|v| v.max(0) as u64),
        error: row::text_opt(r, 7)?,
        payload: payload_raw
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .and_then(|s| serde_json::from_str(s).ok()),
        created_at: row::text(r, 9)?,
        updated_at: row::text(r, 10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    #[test]
    fn create_starts_queued() {
        let d = db();
        let j = d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        assert_eq!(j.id, "j1");
        assert_eq!(j.job_type, JobType::ScanProject);
        assert_eq!(j.status, JobStatus::Queued);
        assert_eq!(j.progress, 0.0);
        assert!(j.stage.is_none());
        assert!(j.payload.is_none());
    }

    #[test]
    fn create_with_payload_persists() {
        let d = db();
        let payload = serde_json::json!({ "mode": "full", "dirs": ["/tmp/a"] });
        let j = d.jobs().create("j1", JobType::ScanProject, Some(&payload)).unwrap();
        assert_eq!(j.payload, Some(payload.clone()));
        let got = d.jobs().get("j1").unwrap().unwrap();
        assert_eq!(got.payload, Some(payload));
    }

    #[test]
    fn update_progress_and_status() {
        let d = db();
        let mut j = d.jobs().create("j1", JobType::IndexCode, None).unwrap();
        j.set_progress(0.4, Some("索引中".into()));
        j.processed = Some(50);
        j.total = Some(120);
        assert!(d.jobs().update(&j).unwrap());
        let got = d.jobs().get("j1").unwrap().unwrap();
        assert_eq!(got.status, JobStatus::Running);
        assert!((got.progress - 0.4).abs() < 1e-9);
        assert_eq!(got.stage.as_deref(), Some("索引中"));
        assert_eq!(got.processed, Some(50));
        assert_eq!(got.total, Some(120));
    }

    /// 终态保护：取消后的进度上报不得覆盖 cancelled。
    #[test]
    fn update_cannot_override_terminal_state() {
        let d = db();
        let j = d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        assert!(d.jobs().set_terminal("j1", JobStatus::Cancelled, None).unwrap());

        // 后台线程迟到的进度上报
        let mut late = j.clone();
        late.status = JobStatus::Running;
        late.progress = 0.9;
        late.stage = Some("不应生效".into());
        assert!(!d.jobs().update(&late).unwrap(), "终态任务更新应返回 false");

        let got = d.jobs().get("j1").unwrap().unwrap();
        assert_eq!(got.status, JobStatus::Cancelled);
        assert_eq!(got.progress, 0.0);
        assert_ne!(got.stage.as_deref(), Some("不应生效"));
    }

    #[test]
    fn set_terminal_completed_forces_full_progress() {
        let d = db();
        let mut j = d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        j.set_progress(0.6, None);
        d.jobs().update(&j).unwrap();
        d.jobs().set_terminal("j1", JobStatus::Completed, None).unwrap();
        let got = d.jobs().get("j1").unwrap().unwrap();
        assert_eq!(got.status, JobStatus::Completed);
        assert_eq!(got.progress, 1.0, "完成态进度必须为 1.0");
    }

    #[test]
    fn set_terminal_failed_records_error() {
        let d = db();
        d.jobs().create("j1", JobType::AnalyzeProject, None).unwrap();
        d.jobs().set_terminal("j1", JobStatus::Failed, Some("模型超时")).unwrap();
        let got = d.jobs().get("j1").unwrap().unwrap();
        assert_eq!(got.status, JobStatus::Failed);
        assert_eq!(got.error.as_deref(), Some("模型超时"));
    }

    #[test]
    fn set_terminal_rejects_non_terminal() {
        let d = db();
        d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        let r = d.jobs().set_terminal("j1", JobStatus::Running, None);
        assert!(r.is_err(), "set_terminal 不接受 running");
    }

    /// 终态先到先得：completed 之后不能再改成 failed。
    #[test]
    fn terminal_state_is_first_write_wins() {
        let d = db();
        d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        assert!(d.jobs().set_terminal("j1", JobStatus::Completed, None).unwrap());
        assert!(!d.jobs().set_terminal("j1", JobStatus::Failed, Some("x")).unwrap());
        assert_eq!(d.jobs().get("j1").unwrap().unwrap().status, JobStatus::Completed);
    }

    #[test]
    fn has_active_of_type_detects_running_and_queued() {
        let d = db();
        assert!(!d.jobs().has_active_of_type(JobType::ScanProject).unwrap());
        d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        assert!(d.jobs().has_active_of_type(JobType::ScanProject).unwrap(), "queued 也算活跃");
        d.jobs().set_terminal("j1", JobStatus::Completed, None).unwrap();
        assert!(!d.jobs().has_active_of_type(JobType::ScanProject).unwrap());
    }

    #[test]
    fn running_returns_only_running() {
        let d = db();
        d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        let mut j2 = d.jobs().create("j2", JobType::IndexCode, None).unwrap();
        j2.set_progress(0.5, None);
        d.jobs().update(&j2).unwrap();
        d.jobs().create("j3", JobType::AnalyzeProject, None).unwrap();
        d.jobs().set_terminal("j3", JobStatus::Completed, None).unwrap();

        let running = d.jobs().running().unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, "j2");
    }

    #[test]
    fn overall_progress_is_none_when_idle() {
        assert_eq!(db().jobs().overall_progress().unwrap(), None, "无活跃任务不应返回假进度");
    }

    #[test]
    fn overall_progress_averages_active_jobs() {
        let d = db();
        let mut j1 = d.jobs().create("j1", JobType::IndexCode, None).unwrap();
        j1.set_progress(0.4, None);
        d.jobs().update(&j1).unwrap();
        let mut j2 = d.jobs().create("j2", JobType::IndexCode, None).unwrap();
        j2.set_progress(0.6, None);
        d.jobs().update(&j2).unwrap();
        let p = d.jobs().overall_progress().unwrap().unwrap();
        assert!((p - 0.5).abs() < 1e-9);
    }

    #[test]
    fn overall_counter_sums_active_jobs() {
        let d = db();
        let mut j1 = d.jobs().create("j1", JobType::IndexCode, None).unwrap();
        j1.set_progress(0.5, None);
        j1.processed = Some(60);
        j1.total = Some(100);
        d.jobs().update(&j1).unwrap();
        let mut j2 = d.jobs().create("j2", JobType::IndexCode, None).unwrap();
        j2.set_progress(0.5, None);
        j2.processed = Some(67);
        j2.total = Some(83);
        d.jobs().update(&j2).unwrap();
        assert_eq!(d.jobs().overall_counter().unwrap(), Some((127, 183)));
    }

    #[test]
    fn overall_counter_none_without_total() {
        let d = db();
        let mut j = d.jobs().create("j1", JobType::IndexCode, None).unwrap();
        j.set_progress(0.5, None);
        d.jobs().update(&j).unwrap();
        assert_eq!(d.jobs().overall_counter().unwrap(), None);
    }

    #[test]
    fn recent_orders_by_created_desc() {
        let d = db();
        for i in 0..5 {
            d.jobs().create(&format!("j{i}"), JobType::IndexCode, None).unwrap();
        }
        let recent = d.jobs().recent(3).unwrap();
        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn list_by_status_filters() {
        let d = db();
        d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        d.jobs().create("j2", JobType::ScanProject, None).unwrap();
        d.jobs().set_terminal("j2", JobStatus::Cancelled, None).unwrap();
        assert_eq!(d.jobs().list_by_status(JobStatus::Queued).unwrap().len(), 1);
        assert_eq!(d.jobs().list_by_status(JobStatus::Cancelled).unwrap().len(), 1);
    }

    /// 启动时清理遗留 running 任务，避免侧栏永远显示"索引中"。
    #[test]
    fn reap_stale_marks_interrupted_jobs_failed() {
        let d = db();
        let mut j = d.jobs().create("j1", JobType::IndexCode, None).unwrap();
        j.set_progress(0.68, Some("索引中".into()));
        d.jobs().update(&j).unwrap();
        d.jobs().create("j2", JobType::ScanProject, None).unwrap();
        assert_eq!(d.jobs().running().unwrap().len(), 1);

        let reaped = d.jobs().reap_stale().unwrap();
        assert_eq!(reaped, 2);
        assert_eq!(d.jobs().running().unwrap().len(), 0);
        assert_eq!(d.jobs().overall_progress().unwrap(), None);
        let got = d.jobs().get("j1").unwrap().unwrap();
        assert_eq!(got.status, JobStatus::Failed);
        assert!(got.error.unwrap().contains("重启"));
    }

    #[test]
    fn reap_stale_keeps_finished_jobs() {
        let d = db();
        d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        d.jobs().set_terminal("j1", JobStatus::Completed, None).unwrap();
        assert_eq!(d.jobs().reap_stale().unwrap(), 0);
        assert_eq!(d.jobs().get("j1").unwrap().unwrap().status, JobStatus::Completed);
    }

    #[test]
    fn purge_finished_keeps_recent() {
        let d = db();
        for i in 0..10 {
            d.jobs().create(&format!("j{i}"), JobType::IndexCode, None).unwrap();
            d.jobs().set_terminal(&format!("j{i}"), JobStatus::Completed, None).unwrap();
        }
        let purged = d.jobs().purge_finished(3).unwrap();
        assert_eq!(purged, 7);
        assert_eq!(d.jobs().count().unwrap(), 3);
    }

    #[test]
    fn purge_finished_keeps_active_jobs() {
        let d = db();
        d.jobs().create("active", JobType::IndexCode, None).unwrap();
        d.jobs().create("done", JobType::IndexCode, None).unwrap();
        d.jobs().set_terminal("done", JobStatus::Completed, None).unwrap();
        d.jobs().purge_finished(0).unwrap();
        assert!(d.jobs().get("active").unwrap().is_some(), "活跃任务不得被清理");
        assert!(d.jobs().get("done").unwrap().is_none());
    }

    #[test]
    fn delete_removes_job() {
        let d = db();
        d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        assert!(d.jobs().delete("j1").unwrap());
        assert!(!d.jobs().delete("j1").unwrap());
    }

    #[test]
    fn updated_at_changes_on_update() {
        let d = db();
        let j = d.jobs().create("j1", JobType::IndexCode, None).unwrap();
        let before = j.updated_at.clone();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let mut j2 = j.clone();
        j2.set_progress(0.5, None);
        d.jobs().update(&j2).unwrap();
        let after = d.jobs().get("j1").unwrap().unwrap().updated_at;
        assert_ne!(before, after, "updated_at 应刷新");
    }

    #[test]
    fn unknown_type_and_status_degrade() {
        let d = db();
        d.jobs().create("j1", JobType::IndexCode, None).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE jobs SET type='FUTURE_JOB', status='weird' WHERE id='j1'", []).unwrap();
        drop(conn);
        let got = d.jobs().get("j1").unwrap().unwrap();
        assert_eq!(got.job_type, JobType::IndexCode);
        assert_eq!(got.status, JobStatus::Queued);
    }

    #[test]
    fn corrupt_payload_degrades_to_none() {
        let d = db();
        d.jobs().create("j1", JobType::IndexCode, None).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE jobs SET payload_json='{{bad' WHERE id='j1'", []).unwrap();
        drop(conn);
        assert!(d.jobs().get("j1").unwrap().unwrap().payload.is_none());
    }

    #[test]
    fn count_reflects_rows() {
        let d = db();
        assert_eq!(d.jobs().count().unwrap(), 0);
        d.jobs().create("j1", JobType::ScanProject, None).unwrap();
        d.jobs().create("j2", JobType::ScanProject, None).unwrap();
        assert_eq!(d.jobs().count().unwrap(), 2);
    }
}
