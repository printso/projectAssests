//! 取消令牌与登记表。
//!
//! 《技术设计书》§15 的红线：**所有 AI 分析都必须可取消**。
//! 桌面软件里用户关掉窗口、点"取消"、或改主意重扫，都是常态；
//! 不能取消的长任务会让整个应用看起来像卡死。
//!
//! # 为什么用协作式取消而非杀线程
//! Rust 无法安全地终止线程（可能在写数据库的半途被杀，留下损坏的事务）。
//! 协作式取消 = 设置一个原子标志，工作循环在每个安全点检查它。
//! 代价是取消有延迟（最多一个检查周期），换来的是**数据始终一致**。
//!
//! # 为什么需要登记表
//! 取消请求来自 HTTP 层，工作循环在另一个线程。
//! 登记表是两者之间唯一的共享句柄：按 job_id 查到对应标志并置位。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 取消令牌：可跨线程克隆，任一份置位则全部可见。
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消。幂等：重复调用无副作用。
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// 是否已请求取消。
    ///
    /// 用 `SeqCst` 而非 `Relaxed`：取消必须对工作线程立即可见。
    /// Relaxed 在弱内存序架构（ARM）上可能让工作线程长时间看不到标志，
    /// 表现为"点了取消但任务还在跑"——这类 bug 在 x86 上测不出来。
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// 底层原子标志的共享引用。
    ///
    /// 存在的唯一理由：同步库（`projectassests_scanner::Scanner::scan`）接收
    /// `&AtomicBool` 而非本类型。共享同一个 `Arc<AtomicBool>` 意味着
    /// 引擎置位取消后，扫描器的内层循环会立刻看到——
    /// 不需要"外层轮询标志再转发给内层"这种会引入取消延迟的中转。
    pub fn flag(&self) -> &Arc<AtomicBool> {
        &self.flag
    }
}

/// 取消登记表：job_id → 令牌。
///
/// 任务结束时必须注销，否则长跑进程的内存会随任务数线性增长。
#[derive(Debug, Default)]
pub struct CancelRegistry {
    tokens: Mutex<HashMap<String, CancelToken>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 为任务注册一个新令牌。
    ///
    /// 同 id 重复注册会覆盖旧令牌：重新扫描同一目录时，
    /// 旧令牌指向已死的任务，保留它只会让取消请求打到空处。
    pub fn register(&self, job_id: &str) -> CancelToken {
        let token = CancelToken::new();
        let mut map = self.lock();
        map.insert(job_id.to_string(), token.clone());
        token
    }

    /// 请求取消某任务。返回 `false` 表示该任务不在登记表中
    /// （已结束或从未注册），调用方应据此告知用户"任务已结束，无法取消"。
    pub fn cancel(&self, job_id: &str) -> bool {
        let token = self.lock().get(job_id).cloned();
        match token {
            Some(t) => {
                t.cancel();
                true
            }
            None => false,
        }
    }

    /// 注销令牌（任务进入终态时调用）。
    pub fn unregister(&self, job_id: &str) -> bool {
        self.lock().remove(job_id).is_some()
    }

    /// 查询令牌是否存在。
    pub fn contains(&self, job_id: &str) -> bool {
        self.lock().contains_key(job_id)
    }

    /// 当前登记的任务数（诊断用）。
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 取消全部（应用退出时调用，让工作线程尽快退出）。
    pub fn cancel_all(&self) {
        let map = self.lock();
        for token in map.values() {
            token.cancel();
        }
    }

    /// 加锁。锁中毒时取内部值而非 panic：
    /// 取消路径 panic 会让用户连"停止任务"都做不到，那是最糟的失败模式。
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CancelToken>> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_starts_uncancelled() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
    }

    #[test]
    fn cancel_is_visible_across_clones() {
        let t = CancelToken::new();
        let clone = t.clone();
        t.cancel();
        assert!(clone.is_cancelled(), "克隆必须共享同一标志");
    }

    #[test]
    fn cancel_is_idempotent() {
        let t = CancelToken::new();
        t.cancel();
        t.cancel();
        assert!(t.is_cancelled());
    }

    /// 取消必须跨线程立即可见（这是 SeqCst 而非 Relaxed 的理由）。
    #[test]
    fn cancel_is_visible_across_threads() {
        let registry = Arc::new(CancelRegistry::new());
        let token = registry.register("j1");
        let seen = Arc::new(AtomicBool::new(false));
        let seen_worker = seen.clone();

        let worker = std::thread::spawn(move || {
            // 忙等最多 2 秒；正常应在毫秒级看到标志
            for _ in 0..200_000 {
                if token.is_cancelled() {
                    seen_worker.store(true, Ordering::SeqCst);
                    return;
                }
                std::thread::yield_now();
            }
        });

        registry.cancel("j1");
        worker.join().unwrap();
        assert!(seen.load(Ordering::SeqCst), "工作线程应看到取消标志");
    }

    #[test]
    fn register_and_lookup() {
        let r = CancelRegistry::new();
        assert!(r.is_empty());
        let t = r.register("j1");
        assert!(r.contains("j1"));
        assert_eq!(r.len(), 1);
        assert!(!t.is_cancelled());
    }

    #[test]
    fn cancel_unknown_job_returns_false() {
        let r = CancelRegistry::new();
        assert!(!r.cancel("ghost"), "未注册的任务取消应返回 false");
    }

    #[test]
    fn cancel_registered_job_sets_flag() {
        let r = CancelRegistry::new();
        let t = r.register("j1");
        assert!(r.cancel("j1"));
        assert!(t.is_cancelled());
    }

    #[test]
    fn unregister_removes_token() {
        let r = CancelRegistry::new();
        r.register("j1");
        assert!(r.unregister("j1"));
        assert!(!r.contains("j1"));
        assert!(!r.unregister("j1"), "重复注销应返回 false");
    }

    /// 同 id 重复注册必须覆盖：否则取消会打到已死的旧任务上。
    #[test]
    fn reregister_replaces_old_token() {
        let r = CancelRegistry::new();
        let old = r.register("j1");
        let new = r.register("j1");
        assert_eq!(r.len(), 1, "不得累积两个令牌");
        r.cancel("j1");
        assert!(new.is_cancelled());
        assert!(!old.is_cancelled(), "旧令牌已被替换，不应再被触发");
    }

    /// 注销是必须的：否则长跑进程内存随任务数线性增长。
    #[test]
    fn registry_does_not_leak_after_unregister() {
        let r = CancelRegistry::new();
        for i in 0..100 {
            let id = format!("j{i}");
            r.register(&id);
            r.unregister(&id);
        }
        assert!(r.is_empty(), "全部注销后登记表应为空");
    }

    #[test]
    fn cancel_all_sets_every_token() {
        let r = CancelRegistry::new();
        let a = r.register("j1");
        let b = r.register("j2");
        r.cancel_all();
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
    }

    /// 并发注册/取消/注销不得死锁或丢数据。
    #[test]
    fn concurrent_access_is_safe() {
        let r = Arc::new(CancelRegistry::new());
        let mut handles = Vec::new();
        for i in 0..8 {
            let r = Arc::clone(&r);
            handles.push(std::thread::spawn(move || {
                for j in 0..50 {
                    let id = format!("t{i}_{j}");
                    r.register(&id);
                    r.cancel(&id);
                    r.unregister(&id);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(r.is_empty(), "所有线程注销后应为空");
    }
}
