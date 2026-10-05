//! 进度广播。
//!
//! 前端需要实时看到"扫描到哪了"。本模块负责把任务进度推给所有订阅者。
//!
//! # 为什么用 watch 而非 mpsc
//! `tokio::sync::mpsc` 在消费者慢于生产者时会堆积无界消息——
//! 扫描进度每秒可能发几十条，一个卡住的 SSE 连接就能撑爆内存。
//! `watch` 只保留**最新值**，慢消费者自动跳帧：
//! 进度条本来就只关心"现在到哪了"，中间帧没有意义。
//!
//! 这是有意的设计选择，不是偷懒：进度是状态流，不是事件流。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use spolia_domain::JobStatus;
use tokio::sync::watch;

/// 进度事件（前端 SSE / Tauri event 的载荷）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub job_id: String,
    pub job_type: String,
    pub status: JobStatus,
    /// 0.0-1.0
    pub progress: f64,
    /// 阶段文案（"静态分析: 依赖 / 语言 / 规模… (45%)"）
    pub stage: Option<String>,
    pub processed: Option<u64>,
    pub total: Option<u64>,
    pub error: Option<String>,
}

impl ProgressEvent {
    /// 0-100 整数百分比，前端进度条直接用。
    pub fn percent(&self) -> u8 {
        (self.progress.clamp(0.0, 1.0) * 100.0).round() as u8
    }

    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    /// "127 / 183" 形式的计数文本；无总数时为 `None`
    /// （前端隐藏该段，而不是显示 "0 / 0"）。
    pub fn counter_text(&self) -> Option<String> {
        match (self.processed, self.total) {
            (Some(p), Some(t)) if t > 0 => Some(format!("{p} / {t}")),
            _ => None,
        }
    }
}

/// 广播器。克隆是廉价的（内部 Arc），可传给多个任务。
#[derive(Debug, Clone)]
pub struct ProgressBroadcaster {
    sender: Arc<watch::Sender<Option<ProgressEvent>>>,
}

impl Default for ProgressBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressBroadcaster {
    pub fn new() -> Self {
        // 初值 None 表示"尚无任务"：订阅者据此显示空闲态而非 0%
        let (sender, _) = watch::channel(None);
        Self {
            sender: Arc::new(sender),
        }
    }

    /// 发布进度。无订阅者时静默成功（后台任务不该因无人观看而失败）。
    ///
    /// 🔴 必须用 `send_replace` 而非 `send`：后者在**没有任何接收端时返回 Err
    /// 并且不写入值**。本结构在 `new()` 里丢弃了初始接收端，
    /// 用 `send` 会导致"无人订阅期间的所有进度全部丢失"——
    /// 前端若在扫描开始后才连上 SSE，`latest()` 会永远是 None，
    /// 进度条卡在 0% 而任务其实早已在跑。这类失败还会被 `let _ =` 掩盖。
    pub fn publish(&self, event: ProgressEvent) {
        self.sender.send_replace(Some(event));
    }

    /// 重置为空闲态（所有任务结束后调用，前端隐藏进度条）。
    pub fn clear(&self) {
        self.sender.send_replace(None);
    }

    /// 订阅进度流。
    pub fn subscribe(&self) -> ProgressSubscription {
        ProgressSubscription {
            receiver: self.sender.subscribe(),
        }
    }

    /// 最新进度快照（轮询式消费者用，例如 REST 的 /jobs/current）。
    pub fn latest(&self) -> Option<ProgressEvent> {
        self.sender.borrow().clone()
    }

    /// 当前订阅者数量（诊断用）。
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

/// 进度订阅句柄。
#[derive(Debug)]
pub struct ProgressSubscription {
    receiver: watch::Receiver<Option<ProgressEvent>>,
}

impl ProgressSubscription {
    /// 等待下一次进度变化。
    ///
    /// 返回 `None` 表示广播已结束（所有发送端被 drop），
    /// 消费者应据此结束自己的循环，而不是无限等待。
    pub async fn next(&mut self) -> Option<ProgressEvent> {
        match self.receiver.changed().await {
            Ok(()) => self.receiver.borrow().clone(),
            // 发送端全部关闭：流结束
            Err(_) => None,
        }
    }

    /// 当前值（不等待）。
    pub fn current(&self) -> Option<ProgressEvent> {
        self.receiver.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::JobType;

    fn event(progress: f64, status: JobStatus) -> ProgressEvent {
        ProgressEvent {
            job_id: "j1".into(),
            job_type: JobType::ScanProject.as_str().to_string(),
            status,
            progress,
            stage: Some("扫描中".into()),
            processed: Some(10),
            total: Some(100),
            error: None,
        }
    }

    #[test]
    fn percent_converts_to_integer() {
        assert_eq!(event(0.0, JobStatus::Running).percent(), 0);
        assert_eq!(event(0.456, JobStatus::Running).percent(), 46);
        assert_eq!(event(1.0, JobStatus::Completed).percent(), 100);
    }

    /// 越界进度不得产出越界百分比（脏数据会让进度条画出边界）。
    #[test]
    fn percent_is_clamped() {
        assert_eq!(event(5.0, JobStatus::Running).percent(), 100);
        assert_eq!(event(-1.0, JobStatus::Running).percent(), 0);
    }

    #[test]
    fn counter_text_hidden_when_no_total() {
        let mut e = event(0.5, JobStatus::Running);
        assert_eq!(e.counter_text().as_deref(), Some("10 / 100"));
        e.total = None;
        assert_eq!(e.counter_text(), None);
        e.processed = Some(5);
        e.total = Some(0);
        assert_eq!(e.counter_text(), None, "总数 0 不得显示 5 / 0");
    }

    #[test]
    fn terminal_detection() {
        assert!(event(1.0, JobStatus::Completed).is_terminal());
        assert!(event(0.5, JobStatus::Cancelled).is_terminal());
        assert!(!event(0.5, JobStatus::Running).is_terminal());
        assert!(!event(0.0, JobStatus::Queued).is_terminal());
    }

    #[test]
    fn latest_starts_empty() {
        let b = ProgressBroadcaster::new();
        assert!(b.latest().is_none(), "初始应为空闲态而非 0%");
    }

    #[test]
    fn publish_updates_latest() {
        let b = ProgressBroadcaster::new();
        b.publish(event(0.5, JobStatus::Running));
        assert_eq!(b.latest().unwrap().progress, 0.5);
    }

    #[test]
    fn clear_resets_to_idle() {
        let b = ProgressBroadcaster::new();
        b.publish(event(0.5, JobStatus::Running));
        b.clear();
        assert!(b.latest().is_none());
    }

    /// 无订阅者时发布不得报错：后台任务不该因无人观看而失败。
    #[test]
    fn publish_without_subscribers_is_ok() {
        let b = ProgressBroadcaster::new();
        assert_eq!(b.subscriber_count(), 0);
        b.publish(event(0.5, JobStatus::Running));
        assert_eq!(b.latest().unwrap().progress, 0.5, "发布仍应更新快照");
        b.clear();
        assert!(b.latest().is_none());
    }

    /// 🔴 watch 语义：慢消费者只看到最新值，不堆积历史帧。
    /// 这是选 watch 而非 mpsc 的核心理由——扫描进度每秒几十条，
    /// 一个卡住的 SSE 连接用 mpsc 就能撑爆内存。
    #[test]
    fn slow_subscriber_sees_only_latest() {
        let b = ProgressBroadcaster::new();
        let mut sub = b.subscribe();
        // 连续发布多帧，消费者一次都没读
        for i in 1..=10 {
            b.publish(event(i as f64 / 10.0, JobStatus::Running));
        }
        assert_eq!(sub.current().unwrap().progress, 1.0, "应只保留最新帧");
        // 有未消费的更新，next() 立即返回而不阻塞
        let next = block_on(sub.next());
        assert_eq!(next.unwrap().progress, 1.0);
    }

    #[tokio::test]
    async fn subscriber_receives_updates() {
        let b = ProgressBroadcaster::new();
        let mut sub = b.subscribe();
        b.publish(event(0.25, JobStatus::Running));
        let got = sub.next().await.unwrap();
        assert_eq!(got.progress, 0.25);
        assert_eq!(got.job_id, "j1");
    }

    #[tokio::test]
    async fn multiple_subscribers_all_receive() {
        let b = ProgressBroadcaster::new();
        let mut s1 = b.subscribe();
        let mut s2 = b.subscribe();
        assert_eq!(b.subscriber_count(), 2);
        b.publish(event(0.7, JobStatus::Running));
        assert_eq!(s1.next().await.unwrap().progress, 0.7);
        assert_eq!(s2.next().await.unwrap().progress, 0.7);
    }

    /// 广播器被 drop 后，订阅者的 next() 必须返回 None 而非永久挂起。
    /// 否则前端 SSE 连接会泄漏。
    #[tokio::test]
    async fn subscription_ends_when_broadcaster_dropped() {
        let b = ProgressBroadcaster::new();
        let mut sub = b.subscribe();
        drop(b);
        assert!(sub.next().await.is_none(), "发送端关闭后应结束流");
    }

    #[test]
    fn event_roundtrips_through_json() {
        let e = event(0.5, JobStatus::Running);
        let json = serde_json::to_string(&e).unwrap();
        let back: ProgressEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back, "事件必须可序列化给前端");
    }

    /// 在同步测试里驱动一次 async 调用（避免为单个断言引入额外 runtime 依赖）。
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }
}
