//! 共享应用状态。
//!
//! # 为什么状态要显式注入而非用全局单例
//! 全局单例（`once_cell::Lazy`）让测试无法并行：两个测试共用一个数据库，
//! 一个写入会污染另一个的断言。显式 `AppState` 让每个测试能建自己的实例，
//! `#[tokio::test]` 才能真并发跑。
//!
//! # 🔴 为什么 AppState 只是 ServiceContext 的一层壳
//! 早期版本在这里**又构造了一遍** `db + jobs + router + db_path`，
//! 与 `ServiceContext` 完全平行，包括各自都调一次 `reap_stale_jobs`。
//!
//! 两份容器的后果是必然漂移：
//! - 启动清理逻辑写两遍，改了一处忘了另一处 → HTTP 与 Tauri 行为不一致
//! - `AiRouter` 构造两次 → 两个实例各有各的连接池与审计状态
//! - handler 拿到的 `db` 与 service 层用的 `db` 可能来自不同 `Database` 实例
//!
//! 正确的依赖方向是：适配器**只**持有 service 层的上下文，
//! 需要引擎级 API（如 `jobs.subscribe()`）时通过 `Deref` 直接访问，
//! 绝不自己再组装一套。Tauri 适配器将来也持同一个 `ServiceContext`。
//!
//! `Deref` 到 `ServiceContext` 让 `state.db` / `state.jobs` 这类写法保持可用，
//! 既消除了重复，又不必改动所有调用点。

use std::ops::Deref;

use projectassests_service::ServiceContext;

/// 应用状态：被 axum 以 `State<AppState>` 注入每个 handler。
///
/// 廉价克隆（`ServiceContext` 内部全是 `Arc`），axum 每个请求 clone 一次成本可忽略。
#[derive(Clone)]
pub struct AppState {
    /// 唯一的依赖容器。业务逻辑一律走 `projectassests_service`，
    /// handler 不直接使用 `ctx` 里的引擎做业务判断。
    pub ctx: ServiceContext,
}

impl Deref for AppState {
    type Target = ServiceContext;

    fn deref(&self) -> &Self::Target {
        &self.ctx
    }
}

impl AppState {
    /// 用文件数据库构造。
    ///
    /// 🔴 僵尸任务收割由 `ServiceContext::open` 负责（不在这里重复做）：
    /// 上次进程被杀会留下 running 记录，不清理则 `has_active_of_type` 永久为真，
    /// 用户点"扫描"永远提示"已有任务在运行"，而界面上看不到那个任务。
    pub fn open(db_path: impl AsRef<std::path::Path>) -> Result<Self, projectassests_service::ServiceError> {
        Ok(Self {
            ctx: ServiceContext::open(db_path)?,
        })
    }

    /// 用内存数据库构造。
    ///
    /// 🔴 标记 `#[cfg(test)]` 而非仅靠文档注释说明"测试专用"：
    /// 内存库的数据在进程退出后全部消失，若生产路径误用它，
    /// 用户扫描完的项目会在重启后全部消失，而没有任何报错。
    /// 让编译器挡住这条路径，比写在注释里指望读者注意可靠得多。
    #[cfg(test)]
    pub fn in_memory() -> Result<Self, projectassests_service::ServiceError> {
        Ok(Self {
            ctx: ServiceContext::in_memory()?,
        })
    }

    /// 数据库文件路径（健康检查、设置页展示）。
    pub fn db_path(&self) -> &str {
        &self.ctx.db_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_state_is_constructible() {
        let s = AppState::in_memory().unwrap();
        // `db_path` 通过 Deref 访问 ServiceContext 的字段
        assert_eq!(s.db_path, ":memory:");
        assert_eq!(s.db_path(), ":memory:");
        assert!(s.jobs.recent(10).unwrap().is_empty());
    }

    #[test]
    fn state_is_cheap_to_clone() {
        let s = AppState::in_memory().unwrap();
        let c = s.clone();
        // 克隆共享同一个库：一侧写入另一侧可见
        assert_eq!(c.db_path, s.db_path);
        c.db.projects().count().unwrap();
        assert_eq!(s.db.projects().count().unwrap(), 0);
    }

    #[test]
    fn file_state_creates_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projectassests.db");
        let s = AppState::open(&path).unwrap();
        assert!(path.exists(), "数据库文件应被创建");
        assert_eq!(s.db_path, path.display().to_string());
    }

    /// 启动清理必须真的执行：造一条僵尸 running 记录，重开状态后应被收割。
    ///
    /// 🔴 这条测试现在验证的是 `ServiceContext::open` 的行为——
    /// 收割逻辑只存在一处，所以 HTTP 与将来的 Tauri 适配器不可能不一致。
    #[test]
    fn open_reaps_stale_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projectassests.db");
        {
            let s = AppState::open(&path).unwrap();
            s.db.jobs()
                .create("scan_project-zombie", projectassests_domain::JobType::ScanProject, None)
                .unwrap();
            let mut job = s.db.jobs().get("scan_project-zombie").unwrap().unwrap();
            job.status = projectassests_domain::JobStatus::Running;
            s.db.jobs().update(&job).unwrap();
            assert!(
                s.db.jobs()
                    .has_active_of_type(projectassests_domain::JobType::ScanProject)
                    .unwrap(),
                "僵尸任务应处于活跃状态"
            );
        }
        // 重新打开：应清理僵尸，否则扫描功能永久不可用
        let s2 = AppState::open(&path).unwrap();
        assert!(
            !s2.db
                .jobs()
                .has_active_of_type(projectassests_domain::JobType::ScanProject)
                .unwrap(),
            "重开后僵尸任务应已被收割"
        );
    }

    /// AppState 不得自己持有一套平行依赖：所有字段访问都必须穿透到 ctx。
    #[test]
    fn state_derefs_to_the_single_service_context() {
        let s = AppState::in_memory().unwrap();
        // 🔴 同一个 Arc 身份：`s.db` 与 `s.ctx.db` 必须是同一个 Database 实例。
        // 若适配器自己再 open 一次库，就会出现"handler 写的库和 service 读的库
        // 不是同一个"——写入看似成功却永远读不回来，是最难排查的一类故障。
        assert!(std::ptr::eq(
            std::sync::Arc::as_ptr(&s.db),
            std::sync::Arc::as_ptr(&s.ctx.db)
        ));
        // jobs / router 同理都来自 ctx（Deref 而非独立字段）
        assert!(s.jobs.recent(1).is_ok());
    }
}
