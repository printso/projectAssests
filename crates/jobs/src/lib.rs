//! projectAssests 任务引擎：可取消任务队列、进度广播、任务持久化。
//!
//! # 设计红线（《技术设计书》§15）
//! 1. **UI 不允许直接调用 `scan()` / `analyze()`**——一切走任务队列。
//!    直接调用会让长任务阻塞请求线程，UI 表现为"点了没反应"。
//! 2. **所有 AI 分析都必须可取消**——见 `cancel` 模块。
//! 3. **进度必须实时可见**——见 `progress` 模块。
//!
//! # 模块划分
//! | 模块 | 职责 | 是否 async |
//! |---|---|---|
//! | `cancel` | 取消令牌与登记表（协作式取消） | 否 |
//! | `progress` | 进度广播（watch 语义，慢消费者跳帧） | 订阅端是 |
//! | `engine` | 任务生命周期编排：创建 → 执行 → 终态 | 是 |
//! | `pipeline` | 具体任务实现（扫描 → 抽取 → 洞察） | 是 |
//!
//! 前两个模块不依赖 tokio runtime 即可单测（取消是原子操作，
//! 进度发布是同步的），这让并发语义能被确定性验证而非靠 sleep 碰运气。

mod cancel;
mod engine;
mod pipeline;
mod progress;
mod walk;

pub use cancel::{CancelRegistry, CancelToken};
pub use engine::{
    activity_icon_for, job_id_prefix, Cancelled, JobContext, JobEngine, JobHandler, TaskHandle,
};
pub use pipeline::{
    build_default_handlers, validate_settings, IndexCodeHandler, InsightHandler,
    ScanProjectHandler, CAPABILITY_CONFIDENCE_FLOOR, DUPLICATE_MIN_PROJECTS,
    MAX_FILES_PER_PROJECT_SCAN, MAX_PROJECTS_PER_SCAN,
};
pub use progress::{ProgressBroadcaster, ProgressEvent, ProgressSubscription};
pub use walk::{
    list_source_files, read_source, top_dirs_of, SourceFile, WalkStats, MAX_DEPTH, MAX_FILE_BYTES,
    MAX_FILES_PER_PROJECT,
};

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{JobStatus, JobType};

    /// 公开 API 可达性 + 关键常量（用精确值断言，误改时测试才会红）。
    #[test]
    fn public_api_is_accessible() {
        let registry = CancelRegistry::new();
        let token = registry.register("j1");
        assert!(registry.contains("j1"));
        assert!(!token.is_cancelled());
        assert!(registry.cancel("j1"));
        assert!(token.is_cancelled());
        assert!(registry.unregister("j1"));
        assert!(registry.is_empty());

        let broadcaster = ProgressBroadcaster::new();
        assert!(broadcaster.latest().is_none());
        let event = ProgressEvent {
            job_id: "j1".into(),
            job_type: JobType::ScanProject.as_str().to_string(),
            status: JobStatus::Running,
            progress: 0.5,
            stage: Some("扫描中".into()),
            processed: Some(5),
            total: Some(10),
            error: None,
        };
        broadcaster.publish(event.clone());
        assert_eq!(broadcaster.latest().unwrap().percent(), 50);
        assert_eq!(event.counter_text().as_deref(), Some("5 / 10"));
        assert_eq!(broadcaster.subscriber_count(), 0);
    }
}
