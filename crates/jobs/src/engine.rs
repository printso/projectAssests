//! 任务引擎：生命周期编排（创建 → 执行 → 终态）。
//!
//! # 职责边界
//! 本模块只管**调度与状态**，不知道任何具体任务怎么干活。
//! 具体实现由 `JobHandler` 注册进来（见 `pipeline` 模块）。
//! 这样加一种新任务不需要改引擎——开源协作时这是最常见的扩展点。
//!
//! # 三个必须守住的正确性约束
//! 1. **取消优先于完成**：用户点取消后，工作线程的最后一次进度上报
//!    不得把状态改回 running/completed。DB 层有终态保护，本层再挡一道。
//! 2. **panic 不得泄漏为"永久 running"**：任务体崩溃时必须置为 failed，
//!    否则侧栏进度条永远转，用户只能重启应用。
//! 3. **启动时收割僵尸任务**：上次进程被杀会留下 running 记录，
//!    不清理的话"已有同类任务在运行"会永久成立，扫描再也无法触发。

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use projectassests_domain::{Job, JobError, JobStatus, JobType, ProjectAssestsError};
use projectassests_storage::{ActivityIcon, Database};

use crate::cancel::{CancelRegistry, CancelToken};
use crate::progress::{ProgressBroadcaster, ProgressEvent};

/// 任务已被取消。
///
/// # 为什么是具名类型而不是 `()`
/// 处理器签名是 `Result<(), String>`，而 `?` 要求 `From<E> for String`。
/// `()` 没有这个转换，于是 `ctx.report(…)?` 根本编译不过——
/// 每个调用点都得手写 `.map_err(|_| "已取消".to_string())?`，
/// 噪音大且容易漏。具名类型 + 下面那条 `From` 实现让 `?` 直接可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("任务已被取消")
    }
}

impl std::error::Error for Cancelled {}

/// 让 `ctx.report(…)?` 在 `Result<(), String>` 的处理器里直接可用。
impl From<Cancelled> for String {
    fn from(_: Cancelled) -> String {
        "任务已被取消".to_string()
    }
}

/// 任务处理器的执行上下文。
///
/// 只暴露任务真正需要的三样东西：数据库、进度上报、取消检查。
/// 不给 `&JobEngine`——否则任务能反过来提交新任务，形成难以追踪的递归。
pub struct JobContext {
    /// 数据库（内部是连接池，可跨线程共享）
    pub db: Arc<Database>,
    /// 当前任务 id
    pub job_id: String,
    /// 任务载荷（提交时传入，例如目录列表）
    pub payload: Option<serde_json::Value>,
    reporter: Arc<TaskReporter>,
}

impl JobContext {
    /// 上报进度。`progress` 为 0.0-1.0。
    ///
    /// 返回 `Err(Cancelled)` 表示任务已被取消，调用方应尽快退出。
    /// 用 `Result` 而非 bool 是为了让 `?` 可用：
    /// `ctx.report(0.5, "分析中…")?;` 读起来就是"没取消就继续"。
    pub fn report(&self, progress: f64, stage: impl Into<String>) -> Result<(), Cancelled> {
        self.reporter.report(progress, stage.into(), None, None)
    }

    /// 上报带计数的进度（"127 / 183 个项目"）。
    pub fn report_counted(
        &self,
        progress: f64,
        stage: impl Into<String>,
        processed: u64,
        total: u64,
    ) -> Result<(), Cancelled> {
        self.reporter
            .report(progress, stage.into(), Some(processed), Some(total))
    }

    /// 是否已请求取消。
    ///
    /// 长循环里应在每个安全点调用（而非只在 `report` 时检查）：
    /// 一次迭代可能要几十秒，只在迭代边界检查会让取消延迟同样久。
    pub fn is_cancelled(&self) -> bool {
        self.reporter.is_cancelled()
    }

    /// 追加一行任务日志（前端"扫描日志"面板）。
    pub fn log(&self, line: impl Into<String>) {
        self.reporter.log(line.into());
    }

    /// 取出可克隆的进度句柄。
    ///
    /// # 为什么需要它
    /// `JobContext` 持有 `payload` 且不可克隆（所有权语义要求任务体独占它）。
    /// 但同步库（`projectassests_scanner`）跑在 `spawn_blocking` 里，
    /// 内部还用 rayon 多线程并行——那些线程需要能上报进度，
    /// 而 `&JobContext` 活不过 `move` 进闭包的那一刻。
    ///
    /// `TaskHandle` 只包一个 `Arc<TaskReporter>`，克隆廉价、`Send + Sync`，
    /// 可以随意送进任意线程，同时**不携带 payload**，
    /// 避免任务载荷被并发修改。
    pub fn handle(&self) -> TaskHandle {
        TaskHandle {
            reporter: Arc::clone(&self.reporter),
        }
    }
}

/// 可克隆、可跨线程的进度句柄。
///
/// 能力是 [`JobContext`] 进度部分的子集：只能上报进度、查取消、写日志，
/// **不含 `payload` 与 `db`**（需要数据库的地方仍在任务主体里做）。
///
/// 实现 `Send + Sync`：内部只有 `Arc`，可安全送进 rayon 工作线程。
#[derive(Clone)]
pub struct TaskHandle {
    reporter: Arc<TaskReporter>,
}

impl TaskHandle {
    /// 上报进度。已取消时返回 `Err(Cancelled)`。
    pub fn report(&self, progress: f64, stage: impl Into<String>) -> Result<(), Cancelled> {
        self.reporter.report(progress, stage.into(), None, None)
    }

    /// 上报带计数的进度。
    pub fn report_counted(
        &self,
        progress: f64,
        stage: impl Into<String>,
        processed: u64,
        total: u64,
    ) -> Result<(), Cancelled> {
        self.reporter
            .report(progress, stage.into(), Some(processed), Some(total))
    }

    pub fn is_cancelled(&self) -> bool {
        self.reporter.is_cancelled()
    }

    /// 底层取消标志（供同步库共享同一个原子变量，见 `JobContext::cancel_flag`）。
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.reporter.token.flag().clone()
    }

    pub fn log(&self, line: impl Into<String>) {
        self.reporter.log(line.into());
    }
}

/// 任务处理器。
///
/// 实现者只需关心业务：拿到 `JobContext`，干活，上报进度。
/// 返回 `Err(String)` 表示失败——错误文案会直接展示给用户，
/// 因此必须是**人话**且不含内部路径泄漏。
///
/// # 为什么用 `async-trait` 而非原生 AFIT
/// 引擎必须持有 `Arc<dyn JobHandler>`：这是"贡献者注册新任务类型
/// 而不必修改引擎"的前提（开源协作最常见的扩展点）。
/// 而原生 AFIT 的 `-> impl Future` 返回类型**不是 dyn-compatible**，
/// 编译期就会拒绝 `dyn JobHandler`。`async-trait` 通过装箱 `Pin<Box<dyn Future>>`
/// 解决它，代价是每次调用一次堆分配——相对任务本身动辄数秒的 IO，可忽略。
#[async_trait::async_trait]
pub trait JobHandler: Send + Sync {
    async fn run(&self, ctx: JobContext) -> Result<(), String>;
}

/// 任务引擎。
///
/// 可克隆：内部全是 `Arc`，多处持有同一份状态。
#[derive(Clone)]
pub struct JobEngine {
    db: Arc<Database>,
    handlers: Arc<HashMap<JobType, Arc<dyn JobHandler>>>,
    cancels: Arc<CancelRegistry>,
    progress: ProgressBroadcaster,
    /// 引擎是否已关闭（关闭后拒绝新任务，避免退出时还接活）
    shutdown: Arc<AtomicBool>,
}

impl JobEngine {
    /// 构造引擎并注册处理器。
    ///
    /// 处理器表在构造时固定：运行期动态增删会让"某类任务有没有人处理"
    /// 变成时序问题，排查成本远高于收益。
    pub fn new(
        db: Arc<Database>,
        handlers: impl IntoIterator<Item = (JobType, Arc<dyn JobHandler>)>,
    ) -> Self {
        Self {
            db,
            handlers: Arc::new(handlers.into_iter().collect()),
            cancels: Arc::new(CancelRegistry::new()),
            progress: ProgressBroadcaster::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 启动清理：把上次进程遗留的 running/queued 任务标记为失败。
    ///
    /// 🔴 必须在启动时调用。否则僵尸任务会让 `has_active_of_type` 永久为真，
    /// 用户再也无法触发扫描，且没有任何提示说明原因——这是最难排查的一类 bug。
    pub fn reap_stale_jobs(&self) -> Result<usize, ProjectAssestsError> {
        let reaped = self.db.jobs().reap_stale()?;
        if reaped > 0 {
            tracing::warn!(count = reaped, "收割了上次运行遗留的未完成任务");
        }
        Ok(reaped)
    }

    /// 提交任务。返回任务 id。
    ///
    /// 同类任务已在运行时返回 `AlreadyRunning`：
    /// 用户连点两次"扫描"不该产生两个并发扫描（会互相覆盖写库结果）。
    ///
    /// # 为什么保留 `async` 签名却不 await
    /// 本方法内部没有任何 await（只做 DB 校验、建记录、spawn），
    /// 真正的工作在同步的 `spawn_job` 里。保留 `async` 是为了调用方写法稳定
    /// （`engine.submit(...).await`），将来若提交前需要异步校验也不必改所有调用点。
    pub async fn submit(
        &self,
        job_type: JobType,
        payload: Option<serde_json::Value>,
    ) -> Result<String, ProjectAssestsError> {
        self.spawn_job(job_type, payload)
    }

    /// `submit` 的同步实现。
    ///
    /// 🔴 拆出来不只是为了整洁，而是**打破一个类型层面的死循环**：
    /// `submit` 内部 spawn `run_to_completion`，而 `run_to_completion`
    /// 在流水线续跑时又要提交下一阶段任务。
    /// 若续跑处调的是 `async fn submit` 并 `.await` 它，
    /// 则「submit 的 future 是 Send」依赖「run_to_completion 的 future 是 Send」，
    /// 后者又依赖前者——编译器无法判定，直接报 `future cannot be sent between threads safely`。
    ///
    /// 续跑走这个同步版本后，`run_to_completion` 的 future 里不再嵌套 submit 的 future，
    /// 依赖链断掉，Send 可以正常推导。
    fn spawn_job(
        &self,
        job_type: JobType,
        payload: Option<serde_json::Value>,
    ) -> Result<String, ProjectAssestsError> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(ProjectAssestsError::Job(JobError::Execution(
                "服务正在关闭，请稍后再试".into(),
            )));
        }

        let handler = self.handlers.get(&job_type).ok_or_else(|| {
            ProjectAssestsError::Job(JobError::Execution(format!(
                "未注册 {} 类型的任务处理器",
                job_type.label_zh()
            )))
        })?;
        // 借用检查必须在创建记录之前：否则拒掉的请求会留下一条永久 queued 的脏记录
        let handler = Arc::clone(handler);

        if self.db.jobs().has_active_of_type(job_type)? {
            return Err(ProjectAssestsError::Job(JobError::AlreadyRunning(
                job_type.label_zh().to_string(),
            )));
        }

        let job_id = new_job_id(job_type);
        let job = self.db.jobs().create(&job_id, job_type, payload.as_ref())?;

        let token = self.cancels.register(&job_id);
        let reporter = Arc::new(TaskReporter::new(
            job_id.clone(),
            job_type,
            Arc::clone(&self.db),
            token,
            self.progress.clone(),
        ));

        // 立即广播一次 queued：前端提交后能马上看到进度条出现，
        // 而不是等到任务真正开跑（可能有几百毫秒延迟）才有反馈。
        reporter.publish(JobStatus::Queued, 0.0, None, None, None, None);

        // 🔴 链式续跑标记必须在 payload 被 move 进 JobContext 之前取出：
        // 之后就拿不到了。见下方 `chain_requested` 的说明。
        let chain = chain_requested(payload.as_ref());

        let ctx = JobContext {
            db: Arc::clone(&self.db),
            job_id: job_id.clone(),
            payload,
            reporter: Arc::clone(&reporter),
        };

        let cancels = Arc::clone(&self.cancels);
        let id_for_cleanup = job_id.clone();
        // 🔴 链式续跑必须在**引擎侧**做，不能给 handler 一个 `&JobEngine`：
        // 见 `JobContext` 的文档——让任务体自己提交任务会形成难以追踪的递归。
        // 这里由引擎在 handler 返回之后决定是否续跑，控制权始终在一处。
        let chain_spec = ChainSpec {
            engine: self.clone(),
            job_type,
            chain,
        };
        tokio::spawn(async move {
            run_to_completion(handler, ctx, reporter, cancels, &id_for_cleanup, chain_spec).await;
        });

        let _ = job; // create 的返回值已用于校验，状态由 reporter 维护
        Ok(job_id)
    }

    /// 取消任务。
    ///
    /// 返回 `Err(NotFound)` / `Err(AlreadyFinished)` 让 API 能给出准确提示。
    /// 🔴 顺序必须是"先置取消标志，再写 DB"：反过来的话，
    /// 工作线程可能在标志置位前又上报一次进度，把 cancelled 覆盖掉。
    /// （DB 层有终态保护兜底，但这里把顺序摆对能少依赖一层兜底。）
    pub fn cancel(&self, job_id: &str) -> Result<(), ProjectAssestsError> {
        let job = self
            .db
            .jobs()
            .get(job_id)?
            .ok_or_else(|| ProjectAssestsError::Job(JobError::NotFound(job_id.to_string())))?;

        if job.status.is_terminal() {
            return Err(ProjectAssestsError::Job(JobError::AlreadyFinished(
                job.status.label_zh().to_string(),
            )));
        }

        // 1. 置位取消标志（工作线程会在下一个安全点看到）
        let registered = self.cancels.cancel(job_id);

        // 2. 写 DB 终态。即使令牌已不在登记表（进程重启后提交的任务），
        //    也要把状态改掉，否则记录会永远停在 running。
        self.db
            .jobs()
            .set_terminal(job_id, JobStatus::Cancelled, None)?;

        // 3. 广播终态，让前端立即收起进度条
        self.progress.publish(ProgressEvent {
            job_id: job_id.to_string(),
            job_type: job.job_type.as_str().to_string(),
            status: JobStatus::Cancelled,
            progress: job.progress,
            stage: Some("已取消".to_string()),
            processed: job.processed,
            total: job.total,
            error: None,
        });

        if !registered {
            tracing::warn!(job_id, "取消的任务不在登记表中（可能进程重启过），仅更新了状态");
        }
        Ok(())
    }

    /// 当前进度快照（REST 轮询用；SSE 走 `subscribe`）。
    pub fn latest_progress(&self) -> Option<ProgressEvent> {
        self.progress.latest()
    }

    /// 订阅进度流（SSE / Tauri event 用）。
    pub fn subscribe(&self) -> crate::progress::ProgressSubscription {
        self.progress.subscribe()
    }

    /// 当前进度订阅者数量（诊断用）。
    ///
    /// SSE 连接若未正确关闭会在这里累积，
    /// 是排查"内存慢慢涨"最快的观测点。
    pub fn subscriber_count(&self) -> usize {
        self.progress.subscriber_count()
    }

    /// 最近任务列表（任务中心页）。
    pub fn recent(&self, limit: u32) -> Result<Vec<Job>, ProjectAssestsError> {
        Ok(self.db.jobs().recent(limit)?)
    }

    /// 正在运行的任务（侧栏进度卡）。
    pub fn running(&self) -> Result<Vec<Job>, ProjectAssestsError> {
        Ok(self.db.jobs().running()?)
    }

    /// 全局进度（侧栏"索引中 x%"）。
    pub fn overall_progress(&self) -> Result<Option<f64>, ProjectAssestsError> {
        Ok(self.db.jobs().overall_progress()?)
    }

    /// 请求关闭：拒绝新任务，并取消所有在跑的任务。
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.cancels.cancel_all();
    }

    /// 取消登记表（诊断与测试用）。
    pub fn cancel_registry(&self) -> &CancelRegistry {
        &self.cancels
    }
}

/// 本任务结束后如何续跑流水线。
///
/// 把 `engine` / `job_type` / `chain` 收成一组：三者回答的是同一个问题
/// ——"这一阶段做完之后，要不要、以及如何起下一阶段"。
/// 散成三个参数的话，`run_to_completion` 的签名会膨胀到 8 个参数，
/// 调用点上一长串同类型值（两个 `bool`、两个 `JobType`）极易传错顺序，
/// 而编译器不会报错。分组之后顺序错误会变成类型错误。
struct ChainSpec {
    /// 用于提交下一阶段的引擎（克隆廉价：内部全是 `Arc`）
    engine: JobEngine,
    /// 刚跑完的任务类型（决定下一阶段是什么）
    job_type: JobType,
    /// 提交时是否要求续跑（见 `chain_requested`）
    chain: bool,
}

/// 执行任务并保证进入终态。
///
/// 三条路径都必须收尾：正常完成、返回 Err、以及**取消**。
/// panic 由 `catch_unwind` 兜住——任务体崩溃不该让引擎留下永久 running 的记录。
async fn run_to_completion(
    handler: Arc<dyn JobHandler>,
    ctx: JobContext,
    reporter: Arc<TaskReporter>,
    cancels: Arc<CancelRegistry>,
    job_id: &str,
    chain_spec: ChainSpec,
) {
    reporter.publish(JobStatus::Running, 0.0, Some("开始执行".to_string()), None, None, None);
    reporter.mark_running();

    let outcome = run_guarded(Arc::clone(&handler), ctx).await;

    let succeeded = match outcome {
        Ok(()) => {
            // 完成前再查一次取消：任务可能在最后一步之前被取消，
            // 此时报 completed 会让用户困惑（"我明明点了取消"）
            if reporter.is_cancelled() {
                reporter.finish(JobStatus::Cancelled, None);
                false
            } else {
                reporter.finish(JobStatus::Completed, None);
                true
            }
        }
        Err(err) => {
            if reporter.is_cancelled() {
                // 任务因取消而提前退出：这是正常流程，不是失败
                reporter.finish(JobStatus::Cancelled, None);
            } else {
                reporter.finish(JobStatus::Failed, Some(err));
            }
            false
        }
    };

    // 注销令牌：不注销会让登记表随任务数无限增长
    cancels.unregister(job_id);

    // 🔴 续跑只在**本阶段成功**时发生。
    //
    // 失败或取消都必须停下：扫描失败了还去索引，索引必然报"没有可索引的项目"，
    // 用户会在任务列表里看到三条错误，而根因只有第一条——
    // 噪音会让人忽略真正需要处理的那个错误。
    // 取消时停下更是硬要求：用户点了取消就是想让它停，
    // 结果它自己又起了下一阶段，等于取消无效。
    // let-chain：把"要求续跑"与"确实有下一阶段"合并成一个条件，
    // 少一层嵌套，读起来就是一句话。
    if succeeded
        && chain_spec.chain
        && let Some(next) = next_in_chain(chain_spec.job_type)
    {
        let from = chain_spec.job_type;
        // 🔴 用同步的 `spawn_job` 而非 `submit(...).await`：
        // 后者会让本 future 嵌套 submit 的 future，
        // 而 submit 又 spawn 本函数，Send 推导陷入自依赖死循环。
        // 详见 `spawn_job` 的文档。
        match chain_spec.engine.spawn_job(next, Some(chain_payload())) {
            Ok(id) => {
                tracing::info!(from = from.as_str(), next = id, "流水线续跑下一阶段");
                chain_spec
                    .engine
                    .db
                    .activities()
                    .push(
                        activity_icon_for(next),
                        next.label_zh(),
                        format!("由{}完成后自动触发", from.label_zh()),
                    )
                    .ok();
            }
            // 同类任务已在跑：不是错误，说明用户手动触发过同一阶段。
            // 记一条日志即可，不重试——重试只会不断撞同一堵墙。
            Err(e) => {
                tracing::warn!(
                    from = from.as_str(),
                    next = next.as_str(),
                    error = %e,
                    "流水线续跑被拒（下一阶段可能已在运行）"
                );
            }
        }
    }
}

/// 载荷里的 `chain` 标记是否要求续跑。
///
/// 🔴 用**显式标记**而非"某类任务完成后一律续跑"：
/// 用户在项目详情页点"重建索引"是明确的单阶段意图，
/// 若因此自动触发一轮全局洞察分析（可能几分钟 + 消耗模型额度），
/// 就是他没要求、也无法预期的副作用。
///
/// 只有产品入口（首页"开始扫描"）会带上这个标记，
/// 让 Level 0 → 1 → 2 按《技术设计书》§13 的要求自动推进。
fn chain_requested(payload: Option<&serde_json::Value>) -> bool {
    payload
        .and_then(|p| p.get("chain"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// 传给下一阶段的载荷：继承 `chain` 标记，让整条链跑完。
///
/// 刻意**不带** `dirs` / `project_id`：索引与洞察都是全库口径，
/// 带上扫描目录会让 `IndexCodeHandler` 误解为"只索引这些目录"。
fn chain_payload() -> serde_json::Value {
    serde_json::json!({ "chain": true })
}

/// 流水线的下一阶段；`None` 表示已是末端。
///
/// 🔴 必须是一条**有限链**（Scan → Index → Insight → None）。
/// 若在 Insight 之后又指回 Scan，一次扫描就会无限循环跑下去，
/// 而且每一轮都在写库，CPU 与磁盘占用永不回落。
/// 新增阶段时务必确认链条仍能终止（见 `chain_terminates_at_the_last_stage` 测试）。
fn next_in_chain(job_type: JobType) -> Option<JobType> {
    match job_type {
        JobType::ScanProject => Some(JobType::IndexCode),
        JobType::IndexCode => Some(JobType::GenerateInsight),
        // 末端：洞察与机会发现之后没有后续阶段
        _ => None,
    }
}

/// 执行任务体，把 panic 转成 `failed`。
///
/// 🔴 必须用**嵌套 spawn** 而非 `catch_unwind`：
/// panic 发生在 future 被 poll 的时候，`catch_unwind(|| fut)` 只包住了
/// future 的**创建**，poll 阶段的 panic 照样会炸掉整个外层任务，
/// 收尾逻辑（写终态）永远执行不到——任务卡在 running，
/// 侧栏进度条永久转圈，用户只能重启应用。
///
/// 嵌套 spawn 让 panic 变成内层任务的 `JoinError`，外层能接住并收尾。
async fn run_guarded(handler: Arc<dyn JobHandler>, ctx: JobContext) -> Result<(), String> {
    match tokio::spawn(async move { handler.run(ctx).await }).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        // 内层任务 panic：转成人话，不把 Rust 内部信息抛给用户
        Err(e) if e.is_panic() => Err(format!(
            "任务执行时发生内部错误{}",
            panic_detail(e.into_panic())
        )),
        // 内层任务被 abort（例如 runtime 关闭）
        Err(e) => Err(format!("任务执行异常终止: {e}")),
    }
}

/// 从 panic 载荷里提取可读信息。
///
/// 只认 `String` 与 `&str` 两种（`panic!("…")` 的全部常见形态）；
/// 其它类型不猜内容——把 `Box<dyn Any>` 的 Debug 输出抛给用户没有意义。
fn panic_detail(payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        return format!("（已捕获 panic）: {s}");
    }
    if let Some(s) = payload.downcast_ref::<&str>() {
        return format!("（已捕获 panic）: {s}");
    }
    "（已捕获 panic）".to_string()
}

/// 单个任务的进度上报器。
///
/// 负责把进度同时写到 DB（刷新后仍可见）与广播（实时推送）。
struct TaskReporter {
    job_id: String,
    job_type: JobType,
    db: Arc<Database>,
    token: CancelToken,
    progress: ProgressBroadcaster,
    /// 上次写 DB 的时间：用于节流
    last_db_write: std::sync::Mutex<std::time::Instant>,
}

/// DB 写入的最小间隔。
///
/// 扫描 156 个项目会产生上百次进度上报，每次都写 SQLite 没必要；
/// 但广播必须每次都发（那是用户眼前的进度条）。
const DB_WRITE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

impl TaskReporter {
    fn new(
        job_id: String,
        job_type: JobType,
        db: Arc<Database>,
        token: CancelToken,
        progress: ProgressBroadcaster,
    ) -> Self {
        Self {
            job_id,
            job_type,
            db,
            token,
            progress,
            last_db_write: std::sync::Mutex::new(std::time::Instant::now()),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// 上报进度。已取消时返回 `Err(())`。
    fn report(
        &self,
        progress: f64,
        stage: String,
        processed: Option<u64>,
        total: Option<u64>,
    ) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            return Err(Cancelled);
        }
        let p = progress.clamp(0.0, 1.0);
        // 广播每次都发：进度条的流畅度直接取决于它
        self.publish(JobStatus::Running, p, Some(stage), processed, total, None);
        // DB 写入节流：只在间隔到达或进度显著变化时写
        if self.should_persist(p) {
            self.persist(p, processed, total);
        }
        Ok(())
    }

    fn log(&self, line: String) {
        // 日志写入活动流（首页"最近活动"与任务详情都能看到）。
        // 写入失败只忽略不报错：活动流是辅助信息，
        // 不该因为一条日志没记下就让正在跑的扫描任务失败。
        let _ = self.db.activities().push(
            activity_icon_for(self.job_type),
            self.job_type.label_zh(),
            line,
        );
    }

    fn should_persist(&self, progress: f64) -> bool {
        // 进度到 1.0 必须立即写，否则刷新页面会看到 99%
        if progress >= 1.0 {
            return true;
        }
        let mut last = self
            .last_db_write
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if last.elapsed() >= DB_WRITE_INTERVAL {
            *last = std::time::Instant::now();
            return true;
        }
        false
    }

    /// 把进度写入 DB。失败只记日志不中断任务：
    /// 进度丢失是可接受的降级，任务本身失败才是事故。
    fn persist(&self, progress: f64, processed: Option<u64>, total: Option<u64>) {
        let mut job = match self.db.jobs().get(&self.job_id) {
            Ok(Some(j)) => j,
            Ok(None) => {
                tracing::warn!(job_id = %self.job_id, "进度上报时发现任务记录不存在");
                return;
            }
            Err(e) => {
                tracing::warn!(job_id = %self.job_id, error = %e, "读取任务失败，跳过本次进度持久化");
                return;
            }
        };
        job.processed = processed.or(job.processed);
        job.total = total.or(job.total);
        job.set_progress(progress, None);
        if let Err(e) = self.db.jobs().update(&job) {
            tracing::warn!(job_id = %self.job_id, error = %e, "写入任务进度失败");
        }
    }

    fn mark_running(&self) {
        if let Ok(Some(mut job)) = self.db.jobs().get(&self.job_id) {
            job.status = JobStatus::Running;
            let _ = self.db.jobs().update(&job);
        }
    }

    /// 置终态：写 DB + 广播 + 注销由调用方负责。
    fn finish(&self, status: JobStatus, error: Option<String>) {
        let progress = if status == JobStatus::Completed {
            1.0
        } else {
            self.current_progress()
        };
        // DB 优先：即使广播失败，刷新页面也应看到正确的终态
        if let Err(e) = self
            .db
            .jobs()
            .set_terminal(&self.job_id, status, error.as_deref())
        {
            tracing::error!(job_id = %self.job_id, error = %e, "写入任务终态失败");
        }
        self.publish(status, progress, stage_for(status), None, None, error.clone());
    }

    fn current_progress(&self) -> f64 {
        self.db
            .jobs()
            .get(&self.job_id)
            .ok()
            .flatten()
            .map(|j| j.progress)
            .unwrap_or(0.0)
    }

    fn publish(
        &self,
        status: JobStatus,
        progress: f64,
        stage: Option<String>,
        processed: Option<u64>,
        total: Option<u64>,
        error: Option<String>,
    ) {
        self.progress.publish(ProgressEvent {
            job_id: self.job_id.clone(),
            job_type: self.job_type.as_str().to_string(),
            status,
            progress: progress.clamp(0.0, 1.0),
            stage,
            processed,
            total,
            // 🔴 必须透传：失败任务若广播里没有错误文案，
            // 前端只能显示"失败"两个字，用户不知道该去配模型还是该重试。
            error,
        });
    }
}

/// 终态对应的阶段文案（前端直接展示）。
fn stage_for(status: JobStatus) -> Option<String> {
    Some(match status {
        JobStatus::Completed => "已完成".to_string(),
        JobStatus::Failed => "执行失败".to_string(),
        JobStatus::Cancelled => "已取消".to_string(),
        JobStatus::Running => "进行中".to_string(),
        JobStatus::Queued => "排队中".to_string(),
    })
}

/// 任务类型 → 活动流图标。
///
/// `ActivityIcon` 没有通用 `Info` 变体，也不该有：活动流里"扫了目录"和
/// "发现了重复能力"是完全不同的事件，用同一个图标会让用户扫一眼就放弃阅读。
/// 按语义映射，图标本身即信息。
pub fn activity_icon_for(job_type: JobType) -> ActivityIcon {
    match job_type {
        JobType::ScanProject | JobType::IndexCode | JobType::ParseAst => ActivityIcon::Scan,
        JobType::BuildSymbolGraph | JobType::AnalyzeRelations => ActivityIcon::Link,
        JobType::ExtractAssets | JobType::ExtractCapabilities => ActivityIcon::Repeat,
        JobType::GenerateInsight | JobType::DiscoverOpportunity => ActivityIcon::Bulb,
        JobType::AnalyzeProject | JobType::GenerateEmbedding => ActivityIcon::Check,
    }
}

/// 生成任务 id。
///
/// 带类型前缀而非纯 UUID：日志里一眼能看出是什么任务，
/// 排查"哪个任务卡住了"时不必再查数据库。
fn new_job_id(job_type: JobType) -> String {
    format!(
        "{}-{}",
        job_type.as_str().to_lowercase(),
        uuid::Uuid::new_v4().simple()
    )
}

/// 任务 id 是否带合法前缀（诊断辅助）。
pub fn job_id_prefix(id: &str) -> Option<&str> {
    id.split_once('-').map(|(prefix, _)| prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn test_db() -> Arc<Database> {
        Arc::new(Database::in_memory().unwrap())
    }

    /// 立即完成的任务。
    struct QuickHandler;

    #[async_trait::async_trait]
    impl JobHandler for QuickHandler {
        async fn run(&self, ctx: JobContext) -> Result<(), String> {
            ctx.report(0.5, "干了一半")?;
            ctx.report(1.0, "干完了")?;
            Ok(())
        }
    }

    /// 总是失败的任务。
    struct FailHandler;

    #[async_trait::async_trait]
    impl JobHandler for FailHandler {
        async fn run(&self, _ctx: JobContext) -> Result<(), String> {
            Err("模型未配置".to_string())
        }
    }

    /// panic 的任务。
    struct PanicHandler;

    #[async_trait::async_trait]
    impl JobHandler for PanicHandler {
        async fn run(&self, _ctx: JobContext) -> Result<(), String> {
            panic!("任务体崩溃");
        }
    }

    /// 一直跑到被取消的任务。
    struct LongHandler {
        iterations: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl JobHandler for LongHandler {
        async fn run(&self, ctx: JobContext) -> Result<(), String> {
            loop {
                if ctx.is_cancelled() {
                    return Ok(());
                }
                self.iterations.fetch_add(1, Ordering::SeqCst);
                // 忽略上报错误：取消后 report 返回 Err，循环条件已处理退出
                let _ = ctx.report(0.5, "长时间运行中");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }

    fn engine_with(
        db: Arc<Database>,
        handlers: Vec<(JobType, Arc<dyn JobHandler>)>,
    ) -> JobEngine {
        JobEngine::new(db, handlers)
    }

    async fn wait_for_terminal(db: &Database, job_id: &str) -> Job {
        for _ in 0..200 {
            // let-chain：绑定与条件合并，避免嵌套两层只为一个 return
            if let Ok(Some(job)) = db.jobs().get(job_id)
                && job.status.is_terminal()
            {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("任务 {job_id} 在 2 秒内未进入终态");
    }

    /// 上报 100% 之后仍继续工作的任务。
    ///
    /// 🔴 这不是假想场景，而是**真实 pipeline 的行为**：
    /// `ScanProjectHandler` 在 `report_counted(1.0, "扫描完成")` 之后，
    /// 还要做后续的写库与收尾工作才返回。
    /// 这个 handler 用于复现"进度已满但任务仍在跑"的窗口期缺陷。
    struct ReportFullThenKeepWorking {
        /// 上报 1.0 之后是否已经放行主测试继续断言
        gate: Arc<tokio::sync::Notify>,
        /// 主测试是否已允许本任务结束
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl JobHandler for ReportFullThenKeepWorking {
        async fn run(&self, ctx: JobContext) -> Result<(), String> {
            ctx.report_counted(1.0, "扫描完成", 2, 2)?;
            // 通知测试：100% 已上报，现在可以检查状态了
            self.gate.notify_one();
            // 阻塞等待测试放行：让"进度 100% 但任务未结束"的窗口足够长，
            // 否则测试会因时序而偶发通过（flaky）。
            self.release.notified().await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn submit_creates_and_completes_job() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(QuickHandler) as Arc<dyn JobHandler>,
            )],
        );
        let id = engine.submit(JobType::ScanProject, None).await.unwrap();
        assert!(id.starts_with("scan_project-"), "id 应带类型前缀: {id}");

        let job = wait_for_terminal(&db, &id).await;
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.progress, 1.0);
        assert_eq!(job.percent(), 100);
    }

    /// 任务结束后令牌必须注销，否则登记表无限增长。
    #[tokio::test]
    async fn cancel_token_is_unregistered_after_completion() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(QuickHandler) as Arc<dyn JobHandler>,
            )],
        );
        let id = engine.submit(JobType::ScanProject, None).await.unwrap();
        wait_for_terminal(&db, &id).await;
        assert!(
            !engine.cancel_registry().contains(&id),
            "完成后应注销令牌，实际登记表长度 {}",
            engine.cancel_registry().len()
        );
    }

    #[tokio::test]
    async fn failing_job_records_error_message() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::AnalyzeProject,
                Arc::new(FailHandler) as Arc<dyn JobHandler>,
            )],
        );
        let id = engine.submit(JobType::AnalyzeProject, None).await.unwrap();
        let job = wait_for_terminal(&db, &id).await;
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error.as_deref(), Some("模型未配置"));
    }

    /// 🔴 panic 不得让任务永远停在 running——那会让侧栏进度条永久转圈。
    #[tokio::test]
    async fn panicking_job_becomes_failed() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::IndexCode,
                Arc::new(PanicHandler) as Arc<dyn JobHandler>,
            )],
        );
        let id = engine.submit(JobType::IndexCode, None).await.unwrap();
        let job = wait_for_terminal(&db, &id).await;
        assert_eq!(job.status, JobStatus::Failed);
        assert!(
            job.error.as_deref().is_some_and(|e| e.contains("panic")),
            "错误信息应说明是 panic: {:?}",
            job.error
        );
    }

    /// 未注册处理器的任务类型必须明确报错，而不是创建一条永远 queued 的记录。
    #[tokio::test]
    async fn unregistered_handler_is_rejected_without_creating_record() {
        let db = test_db();
        let engine = engine_with(Arc::clone(&db), vec![]);
        let err = engine.submit(JobType::ScanProject, None).await.unwrap_err();
        assert!(matches!(err, ProjectAssestsError::Job(JobError::Execution(_))));
        assert_eq!(db.jobs().count().unwrap(), 0, "被拒的提交不得留下脏记录");
    }

    // ── 流水线续跑（Level 0 → 1 → 2）───────────────────────────

    /// 注册三个阶段的 handler（都立即成功），返回引擎。
    fn chained_engine(db: Arc<Database>) -> JobEngine {
        engine_with(
            db,
            vec![
                (JobType::ScanProject, Arc::new(QuickHandler) as Arc<dyn JobHandler>),
                (JobType::IndexCode, Arc::new(QuickHandler) as Arc<dyn JobHandler>),
                (
                    JobType::GenerateInsight,
                    Arc::new(QuickHandler) as Arc<dyn JobHandler>,
                ),
            ],
        )
    }

    /// 等到任务数达到 `expected` 且全部进入终态。
    ///
    /// 🔴 必须同时校验"数量到了"和"都结束了"：只看数量的话，
    /// 续跑刚创建了 queued 记录就会通过，测不到后续阶段是否真能跑完。
    async fn wait_for_all_terminal(db: &Database, expected: usize) -> Vec<Job> {
        for _ in 0..400 {
            if let Ok(recent) = db.jobs().recent(50)
                && recent.len() >= expected
                && recent.iter().all(|j| j.status.is_terminal())
            {
                return recent;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let jobs = db.jobs().recent(50).unwrap_or_default();
        panic!(
            "等待 {} 个任务全部终态超时，实得 {}: {:?}",
            expected,
            jobs.len(),
            jobs.iter().map(|j| (j.job_type.as_str(), j.status)).collect::<Vec<_>>()
        );
    }

    /// 🔴 核心行为：带 chain 标记的扫描要自动跑完三级流水线。
    ///
    /// 这是《技术设计书》§13 的硬要求，也是产品的核心价值路径：
    /// 用户点一次"开始扫描"，之后资产、能力、洞察应自动出现。
    /// 缺了这一环，扫完只会看到"发现 2 个项目"，而资产页与洞察页全是空的，
    /// 界面上又没有任何入口能触发后续阶段——产品的核心承诺不会兑现。
    #[tokio::test]
    async fn chain_runs_all_three_stages() {
        let db = test_db();
        let engine = chained_engine(Arc::clone(&db));

        engine
            .submit(
                JobType::ScanProject,
                Some(serde_json::json!({"chain": true})),
            )
            .await
            .unwrap();

        let jobs = wait_for_all_terminal(&db, 3).await;
        let types: Vec<&str> = jobs.iter().map(|j| j.job_type.as_str()).collect();
        for expected in ["SCAN_PROJECT", "INDEX_CODE", "GENERATE_INSIGHT"] {
            assert!(
                types.contains(&expected),
                "流水线缺少 {expected} 阶段，实得 {types:?}"
            );
        }
        assert!(
            jobs.iter().all(|j| j.status == JobStatus::Completed),
            "每个阶段都应成功: {:?}",
            jobs.iter().map(|j| (j.job_type.as_str(), j.status, j.error.clone())).collect::<Vec<_>>()
        );
    }

    /// 🔴 链条必须终止：洞察是末端，不得再触发任何阶段。
    ///
    /// 若 `next_in_chain(GenerateInsight)` 指回 ScanProject，
    /// 一次扫描就会无限循环跑下去，每轮都在写库，
    /// CPU 与磁盘占用永不回落——而任务列表看起来"一直在正常工作"，
    /// 用户完全不会意识到出了问题。
    #[tokio::test]
    async fn chain_terminates_at_the_last_stage() {
        let db = test_db();
        let engine = chained_engine(Arc::clone(&db));

        // 直接从末端阶段起步并带上 chain：若链条无限，任务数会持续增长
        engine
            .submit(
                JobType::GenerateInsight,
                Some(serde_json::json!({"chain": true})),
            )
            .await
            .unwrap();

        let jobs = wait_for_all_terminal(&db, 1).await;
        assert_eq!(jobs.len(), 1, "洞察之后不该再有阶段: {:?}", jobs.len());

        // 再等一会儿，确认没有后续任务冒出来
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            db.jobs().count().unwrap(),
            1,
            "链条必须终止，不得无限续跑"
        );
    }

    /// 链条是有限路径，且每一步都必须显式列出。
    #[test]
    fn next_in_chain_is_a_finite_path() {
        assert_eq!(next_in_chain(JobType::ScanProject), Some(JobType::IndexCode));
        assert_eq!(
            next_in_chain(JobType::IndexCode),
            Some(JobType::GenerateInsight)
        );
        // 🔴 末端必须是 None（这是"链条终止"的唯一保证）
        assert_eq!(next_in_chain(JobType::GenerateInsight), None);

        // 未接入流水线的类型不得意外续跑：
        // 用户在项目详情页点"重建索引"之外的操作，不该莫名触发全局分析。
        for t in [
            JobType::ParseAst,
            JobType::BuildSymbolGraph,
            JobType::GenerateEmbedding,
            JobType::AnalyzeProject,
            JobType::ExtractAssets,
            JobType::ExtractCapabilities,
            JobType::AnalyzeRelations,
            JobType::DiscoverOpportunity,
        ] {
            assert_eq!(next_in_chain(t), None, "{} 不该有后续阶段", t.as_str());
        }
    }

    /// 从任意类型出发，沿链条走必须能到达终点。
    ///
    /// 🔴 这是对"链条无环"的**结构性**证明，比只测三个类型更可靠：
    /// 将来有人加阶段时，若不小心接成环，这条测试会立刻失败，
    /// 而不必等到线上 CPU 跑满才发现。
    #[test]
    fn chain_has_no_cycle() {
        for start in [
            JobType::ScanProject,
            JobType::IndexCode,
            JobType::GenerateInsight,
        ] {
            let mut seen = vec![start];
            let mut cur = next_in_chain(start);
            // 上限取 JobType 变体总数：超过它必然出现了环
            for _ in 0..16 {
                let Some(next) = cur else { break };
                assert!(
                    !seen.contains(&next),
                    "链条出现环: {seen:?} -> {}",
                    next.as_str()
                );
                seen.push(next);
                cur = next_in_chain(next);
            }
            assert!(
                cur.is_none(),
                "从 {} 出发未能在 16 步内终止: {seen:?}",
                start.as_str()
            );
        }
    }

    /// `chain` 标记的解析：只有显式 true 才续跑。
    #[test]
    fn chain_flag_is_opt_in_and_strict() {
        assert!(!chain_requested(None), "无载荷不续跑");
        assert!(!chain_requested(Some(&serde_json::json!({}))), "缺字段不续跑");
        assert!(chain_requested(Some(&serde_json::json!({"chain": true}))));
        assert!(!chain_requested(Some(&serde_json::json!({"chain": false}))));
        // 类型不对（字符串 "true"）不得被当成真：宁可少跑一轮，
        // 也不能因为前端传错类型就自动触发几分钟的全局分析
        assert!(!chain_requested(Some(&serde_json::json!({"chain": "true"}))));
    }

    /// 🔴 失败必须停下：扫描失败还去索引，只会产出第二条误导性错误。
    #[tokio::test]
    async fn failure_does_not_continue_chain() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![
                (JobType::ScanProject, Arc::new(FailHandler) as Arc<dyn JobHandler>),
                (JobType::IndexCode, Arc::new(QuickHandler) as Arc<dyn JobHandler>),
            ],
        );

        let id = engine
            .submit(
                JobType::ScanProject,
                Some(serde_json::json!({"chain": true})),
            )
            .await
            .unwrap();
        let job = wait_for_terminal(&db, &id).await;
        assert_eq!(job.status, JobStatus::Failed);

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            db.jobs().count().unwrap(),
            1,
            "失败后不得续跑：用户会在任务列表看到两条错误，而根因只有第一条"
        );
    }

    /// 🔴 取消必须停下：用户点了取消就是想让它停。
    ///
    /// 若取消后还自动起下一阶段，等于取消功能失效——
    /// 而且用户无法理解"我明明取消了，为什么它又在跑"。
    #[tokio::test]
    async fn cancel_does_not_continue_chain() {
        let db = test_db();
        let iterations = Arc::new(AtomicUsize::new(0));
        let engine = engine_with(
            Arc::clone(&db),
            vec![
                (
                    JobType::ScanProject,
                    Arc::new(LongHandler {
                        iterations: Arc::clone(&iterations),
                    }) as Arc<dyn JobHandler>,
                ),
                (JobType::IndexCode, Arc::new(QuickHandler) as Arc<dyn JobHandler>),
            ],
        );

        let id = engine
            .submit(
                JobType::ScanProject,
                Some(serde_json::json!({"chain": true})),
            )
            .await
            .unwrap();
        // 等它真的跑起来再取消
        for _ in 0..100 {
            if iterations.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        engine.cancel(&id).unwrap();
        let job = wait_for_terminal(&db, &id).await;
        assert_eq!(job.status, JobStatus::Cancelled);

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(db.jobs().count().unwrap(), 1, "取消后不得续跑");
    }

    /// 不带 chain 标记时只跑单阶段（项目详情页"重建索引"的语义）。
    #[tokio::test]
    async fn single_stage_without_chain_flag() {
        let db = test_db();
        let engine = chained_engine(Arc::clone(&db));

        let id = engine
            .submit(JobType::ScanProject, Some(serde_json::json!({})))
            .await
            .unwrap();
        wait_for_terminal(&db, &id).await;

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            db.jobs().count().unwrap(),
            1,
            "未要求续跑时不得自动触发全局索引：那是用户没要求、也无法预期的副作用"
        );
    }

    /// 续跑时下一阶段若已在运行，不得报错崩溃，也不得无限重试。
    #[tokio::test]
    async fn chain_tolerates_next_stage_already_running() {
        let db = test_db();
        let engine = chained_engine(Arc::clone(&db));

        // 先占住 IndexCode：用一个会阻塞的任务
        let iterations = Arc::new(AtomicUsize::new(0));
        let engine2 = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::IndexCode,
                Arc::new(LongHandler {
                    iterations: Arc::clone(&iterations),
                }) as Arc<dyn JobHandler>,
            )],
        );
        let blocker = engine2.submit(JobType::IndexCode, None).await.unwrap();

        // 现在跑带 chain 的扫描：续跑提交 IndexCode 会被 AlreadyRunning 拒掉
        let scan = engine
            .submit(
                JobType::ScanProject,
                Some(serde_json::json!({"chain": true})),
            )
            .await
            .unwrap();
        let job = wait_for_terminal(&db, &scan).await;
        // 🔴 扫描本身仍应算成功：下一阶段没排上不是它的错
        assert_eq!(job.status, JobStatus::Completed, "{:?}", job.error);

        engine.cancel(&blocker).unwrap();
        wait_for_terminal(&db, &blocker).await;
        // 任务总数 = 扫描 + 被占住的索引，没有重复提交留下的脏记录
        assert_eq!(db.jobs().count().unwrap(), 2);
    }

    /// 续跑要写活动流：用户得知道"这一步是自动触发的"，
    /// 否则几分钟后洞察突然出现会让人困惑"哪来的"。
    #[tokio::test]
    async fn chain_records_activity_for_auto_trigger() {
        let db = test_db();
        let engine = chained_engine(Arc::clone(&db));

        engine
            .submit(
                JobType::ScanProject,
                Some(serde_json::json!({"chain": true})),
            )
            .await
            .unwrap();
        wait_for_all_terminal(&db, 3).await;

        let acts = db.activities().recent(50).unwrap();
        let details: Vec<String> = acts.iter().map(|a| a.detail.clone()).collect();
        assert!(
            details.iter().any(|d| d.contains("自动触发")),
            "应说明后续阶段是自动触发的: {details:?}"
        );
    }

    /// 🔴 回归：任务上报 100% 后、真正结束前，必须仍算"活跃"。
    ///
    /// 缺陷链条（由真实 pipeline 冒烟测试发现，单测从未覆盖）：
    /// 1. handler 调 `report_counted(1.0, …)`
    /// 2. `should_persist(1.0)` 恒为 true → 立刻 `persist`
    /// 3. `persist` 走 `Job::set_progress(1.0)`，而它把 progress≥1.0 **自动翻成 Completed**
    /// 4. DB 状态变成 completed，但 handler 其实还没返回
    /// 5. `has_active_of_type` 因此返回 false
    /// 6. 用户可以触发**第二个并发扫描**，两个任务同时写同一批表 → 数据互相覆盖
    ///
    /// 正确不变式：**"任务是否活跃"只取决于 handler 有没有返回**，
    /// 与它上报的进度值无关。进度 100% 只是"活干完了"，
    /// 不等于"任务已结束"——收尾写库、日志、注销令牌都还在进行。
    #[tokio::test]
    async fn job_stays_active_after_reporting_full_progress() {
        let db = test_db();
        let gate = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(ReportFullThenKeepWorking {
                    gate: Arc::clone(&gate),
                    release: Arc::clone(&release),
                }) as Arc<dyn JobHandler>,
            )],
        );

        let id = engine.submit(JobType::ScanProject, None).await.unwrap();
        // 等 handler 确实上报过 1.0
        gate.notified().await;
        // 让 persist 落库
        for _ in 0..50 {
            if db.jobs().get(&id).unwrap().unwrap().progress >= 1.0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // ── 核心断言：任务仍在运行，只是进度满了 ────────────────
        let job = db.jobs().get(&id).unwrap().unwrap();
        assert_eq!(job.progress, 1.0, "进度应已满");
        assert_eq!(
            job.status,
            JobStatus::Running,
            "🔴 handler 未返回前状态必须仍是 running，不能因 progress=1.0 被提前翻成 completed"
        );
        assert!(
            !job.status.is_terminal(),
            "任务不得处于终态：handler 还在跑"
        );

        // ── 缺陷的直接后果：并发保护失效 ───────────────────────
        assert!(
            db.jobs().has_active_of_type(JobType::ScanProject).unwrap(),
            "🔴 进度满了但未结束的任务仍应算活跃，否则用户可以触发第二个并发扫描"
        );
        let err = engine.submit(JobType::ScanProject, None).await.unwrap_err();
        assert!(
            matches!(err, ProjectAssestsError::Job(JobError::AlreadyRunning(_))),
            "窗口期内第二次提交必须被拒，实际 {err:?}"
        );

        // 广播侧也必须一致：不能让 SSE 推出 "running @ 1.0" 后又推 "completed"
        let ev = engine.latest_progress().expect("应有进度事件");
        assert_eq!(ev.job_id, id);
        assert_eq!(
            ev.status,
            JobStatus::Running,
            "广播状态与 DB 状态必须一致，否则前端进度条与任务列表会矛盾"
        );

        // 放行 handler，任务才真正结束
        release.notify_one();
        let done = wait_for_terminal(&db, &id).await;
        assert_eq!(done.status, JobStatus::Completed);
        assert_eq!(done.progress, 1.0);

        // 结束后活跃标志必须消失，否则扫描功能会永久卡住
        assert!(
            !db.jobs().has_active_of_type(JobType::ScanProject).unwrap(),
            "任务结束后不该再算活跃"
        );
    }

    /// 同类任务并发提交必须被拒：两次扫描会互相覆盖写库结果。
    #[tokio::test]
    async fn duplicate_submission_is_rejected() {
        let db = test_db();
        let iterations = Arc::new(AtomicUsize::new(0));
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(LongHandler {
                    iterations: Arc::clone(&iterations),
                }) as Arc<dyn JobHandler>,
            )],
        );
        let first = engine.submit(JobType::ScanProject, None).await.unwrap();
        // 第一个任务还在跑，第二次提交应被拒
        let err = engine.submit(JobType::ScanProject, None).await.unwrap_err();
        assert!(
            matches!(err, ProjectAssestsError::Job(JobError::AlreadyRunning(_))),
            "应报 AlreadyRunning，实际 {err:?}"
        );
        engine.cancel(&first).unwrap();
        wait_for_terminal(&db, &first).await;

        // 取消后同类任务应可重新提交（僵尸记录会永久阻塞，这是最常见的卡死场景）
        let second = engine.submit(JobType::ScanProject, None).await;
        assert!(second.is_ok(), "取消后应能重新提交: {:?}", second.err());
        engine.cancel(&second.unwrap()).unwrap();
    }

    #[tokio::test]
    async fn cancel_stops_running_job() {
        let db = test_db();
        let iterations = Arc::new(AtomicUsize::new(0));
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(LongHandler {
                    iterations: Arc::clone(&iterations),
                }) as Arc<dyn JobHandler>,
            )],
        );
        let id = engine.submit(JobType::ScanProject, None).await.unwrap();
        // 等任务真的跑起来
        for _ in 0..100 {
            if iterations.load(Ordering::SeqCst) > 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(iterations.load(Ordering::SeqCst) > 0, "任务应已开始执行");

        engine.cancel(&id).unwrap();
        let job = wait_for_terminal(&db, &id).await;
        assert_eq!(job.status, JobStatus::Cancelled);

        let stopped_at = iterations.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            iterations.load(Ordering::SeqCst),
            stopped_at,
            "取消后工作循环必须真正停止"
        );
    }

    /// 🔴 取消后任务的最后一次上报不得把状态改回 completed。
    #[tokio::test]
    async fn cancel_wins_over_late_completion() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(LongHandler {
                    iterations: Arc::new(AtomicUsize::new(0)),
                }) as Arc<dyn JobHandler>,
            )],
        );
        let id = engine.submit(JobType::ScanProject, None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        engine.cancel(&id).unwrap();
        let job = wait_for_terminal(&db, &id).await;
        assert_eq!(job.status, JobStatus::Cancelled);
        // 再等一会儿，确认没有后续上报覆盖终态
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            db.jobs().get(&id).unwrap().unwrap().status,
            JobStatus::Cancelled,
            "终态不得被后续上报覆盖"
        );
    }

    #[tokio::test]
    async fn cancel_unknown_job_reports_not_found() {
        let db = test_db();
        let engine = engine_with(Arc::clone(&db), vec![]);
        let err = engine.cancel("ghost").unwrap_err();
        assert!(matches!(err, ProjectAssestsError::Job(JobError::NotFound(_))));
    }

    #[tokio::test]
    async fn cancel_finished_job_reports_already_finished() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(QuickHandler) as Arc<dyn JobHandler>,
            )],
        );
        let id = engine.submit(JobType::ScanProject, None).await.unwrap();
        wait_for_terminal(&db, &id).await;
        let err = engine.cancel(&id).unwrap_err();
        assert!(
            matches!(err, ProjectAssestsError::Job(JobError::AlreadyFinished(_))),
            "已完成的任务不得再取消，实际 {err:?}"
        );
    }

    /// 启动清理：僵尸 running 记录必须被收割，否则同类任务永远无法再提交。
    #[tokio::test]
    async fn reap_stale_jobs_unblocks_new_submissions() {
        let db = test_db();
        // 伪造一条 running 记录，模拟上次进程被杀留下的僵尸任务。
        // set_terminal 只接受终态，故用 update 直接改状态。
        let id = "scan_project-zombie";
        db.jobs().create(id, JobType::ScanProject, None).unwrap();
        let mut job = db.jobs().get(id).unwrap().unwrap();
        job.status = JobStatus::Running;
        db.jobs().update(&job).unwrap();
        assert!(
            db.jobs().has_active_of_type(JobType::ScanProject).unwrap(),
            "伪造的僵尸任务应被视为活跃"
        );

        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(QuickHandler) as Arc<dyn JobHandler>,
            )],
        );
        let reaped = engine.reap_stale_jobs().unwrap();
        assert!(reaped >= 1, "应收割僵尸任务");
        assert!(
            !db.jobs().has_active_of_type(JobType::ScanProject).unwrap(),
            "收割后不得再有活跃任务"
        );
        // 关键：现在能正常提交新扫描
        assert!(engine.submit(JobType::ScanProject, None).await.is_ok());
    }

    #[tokio::test]
    async fn progress_is_broadcast_and_queryable() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(QuickHandler) as Arc<dyn JobHandler>,
            )],
        );
        let mut sub = engine.subscribe();
        let id = engine.submit(JobType::ScanProject, None).await.unwrap();

        // 提交后应立即收到 queued 广播（不等任务真正开跑）
        let first = sub.next().await.unwrap();
        assert_eq!(first.job_id, id);
        assert_eq!(first.status, JobStatus::Queued);

        // 最终必须收到终态事件，否则前端进度条永远收不起来
        let mut saw_terminal = false;
        for _ in 0..50 {
            match tokio::time::timeout(Duration::from_millis(100), sub.next()).await {
                Ok(Some(e)) if e.is_terminal() => {
                    saw_terminal = true;
                    assert_eq!(e.status, JobStatus::Completed);
                    assert_eq!(e.percent(), 100);
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        assert!(saw_terminal, "应广播终态事件");
        assert!(engine.latest_progress().is_some());
    }

    #[tokio::test]
    async fn payload_is_passed_to_handler() {
        struct EchoHandler;
            #[async_trait::async_trait]
    impl JobHandler for EchoHandler {
            async fn run(&self, ctx: JobContext) -> Result<(), String> {
                let dirs = ctx
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("dirs"))
                    .and_then(|d| d.as_array())
                    .map(|a| a.len())
                    .ok_or("payload 缺少 dirs")?;
                if dirs != 2 {
                    return Err(format!("期望 2 个目录，实际 {dirs}"));
                }
                ctx.report(1.0, "载荷校验通过")?;
                Ok(())
            }
        }

        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(EchoHandler) as Arc<dyn JobHandler>,
            )],
        );
        let payload = serde_json::json!({ "dirs": ["/a", "/b"], "mode": "full" });
        let id = engine
            .submit(JobType::ScanProject, Some(payload))
            .await
            .unwrap();
        let job = wait_for_terminal(&db, &id).await;
        assert_eq!(job.status, JobStatus::Completed, "载荷应正确传给处理器: {:?}", job.error);
        // 载荷必须持久化（刷新页面后重试仍需要它）
        assert!(job.payload.is_some());
    }

    #[tokio::test]
    async fn shutdown_rejects_new_submissions() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(QuickHandler) as Arc<dyn JobHandler>,
            )],
        );
        engine.shutdown();
        let err = engine.submit(JobType::ScanProject, None).await.unwrap_err();
        assert!(
            matches!(err, ProjectAssestsError::Job(JobError::Execution(_))),
            "关闭后应拒绝新任务，实际 {err:?}"
        );
    }

    #[tokio::test]
    async fn queries_expose_job_lists() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(QuickHandler) as Arc<dyn JobHandler>,
            )],
        );
        let id = engine.submit(JobType::ScanProject, None).await.unwrap();
        wait_for_terminal(&db, &id).await;
        assert_eq!(engine.recent(10).unwrap().len(), 1);
        assert!(engine.running().unwrap().is_empty(), "完成后不应在运行列表");
        // 🔴 无活跃任务时必须是 None：侧栏据此隐藏进度卡。
        // 返回 Some(1.0) 会让"索引中 100%"永久挂在侧栏，
        // 用户以为系统还在忙而不敢操作。
        assert!(
            engine.overall_progress().unwrap().is_none(),
            "全部任务结束后不应有综合进度"
        );
    }

    /// 运行中的任务必须让 `overall_progress` 有值，否则侧栏进度卡不显示。
    #[tokio::test]
    async fn overall_progress_reflects_running_job() {
        let db = test_db();
        let engine = engine_with(
            Arc::clone(&db),
            vec![(
                JobType::ScanProject,
                Arc::new(LongHandler {
                    iterations: Arc::new(AtomicUsize::new(0)),
                }) as Arc<dyn JobHandler>,
            )],
        );
        let id = engine.submit(JobType::ScanProject, None).await.unwrap();
        // 提交后即为 queued（属活跃状态），无需等任务真正跑起来
        assert!(
            engine.overall_progress().unwrap().is_some(),
            "有活跃任务时应报告综合进度"
        );
        assert_eq!(engine.running().unwrap().len() + queued_count(&db), 1);

        engine.cancel(&id).unwrap();
        wait_for_terminal(&db, &id).await;
        assert!(
            engine.overall_progress().unwrap().is_none(),
            "取消后应回到无活跃任务"
        );
    }

    /// 统计排队中的任务数（running 与 queued 都算活跃）。
    fn queued_count(db: &Database) -> usize {
        db.jobs()
            .list_by_status(JobStatus::Queued)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// 活动流图标必须按语义区分：全是同一个图标的话，
    /// 用户扫一眼就放弃阅读活动流。
    #[test]
    fn activity_icon_is_semantic_per_job_type() {
        assert_eq!(
            activity_icon_for(JobType::ScanProject),
            ActivityIcon::Scan
        );
        assert_eq!(
            activity_icon_for(JobType::GenerateInsight),
            ActivityIcon::Bulb
        );
        assert_eq!(
            activity_icon_for(JobType::ExtractAssets),
            ActivityIcon::Repeat
        );
        assert_eq!(
            activity_icon_for(JobType::AnalyzeRelations),
            ActivityIcon::Link
        );
        assert_eq!(
            activity_icon_for(JobType::AnalyzeProject),
            ActivityIcon::Check
        );
        // 不得所有类型都映射到同一个图标
        let distinct: std::collections::HashSet<String> = [
            JobType::ScanProject,
            JobType::GenerateInsight,
            JobType::ExtractAssets,
            JobType::AnalyzeRelations,
            JobType::AnalyzeProject,
        ]
        .iter()
        .map(|t| activity_icon_for(*t).as_str().to_string())
        .collect();
        assert!(distinct.len() >= 4, "图标应有区分度，实际 {} 种", distinct.len());
    }

    /// 取消错误必须能直接用 `?` 传进处理器的 `Result<(), String>`。
    #[test]
    fn cancelled_error_converts_to_string() {
        let s: String = Cancelled.into();
        assert_eq!(s, "任务已被取消");
        assert_eq!(Cancelled.to_string(), "任务已被取消");
    }

    /// `?` 在 `Result<(), String>` 的处理器里必须可用——这是设计 Cancelled
    /// 为具名类型的唯一理由，必须有测试守护，否则有人改回 `()` 时不会被发现。
    #[test]
    fn question_mark_works_in_string_error_handler() {
        fn handler_like() -> Result<(), String> {
            let ctx_result: Result<(), Cancelled> = Err(Cancelled);
            ctx_result?; // 若无 From<Cancelled> for String，此行编译失败
            Ok(())
        }
        assert_eq!(handler_like().unwrap_err(), "任务已被取消");
    }

    #[test]
    fn job_id_carries_type_prefix() {
        let id = new_job_id(JobType::ScanProject);
        assert!(id.starts_with("scan_project-"), "实际 {id}");
        assert_eq!(job_id_prefix(&id), Some("scan_project"));
        assert_eq!(job_id_prefix("noprefix"), None);
    }

    #[test]
    fn stage_text_covers_every_status() {
        for s in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ] {
            assert!(
                stage_for(s).is_some_and(|t| !t.is_empty()),
                "{s:?} 缺少阶段文案"
            );
        }
    }
}
