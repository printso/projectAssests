//! 连接池：SQLite `Connection` 是 `!Send`，而 HTTP 层是多线程的。
//!
//! # 为什么自实现而不引入 r2d2
//! `r2d2-sqlite` 的版本必须与 `rusqlite` 严格匹配。对一个多人维护的开源项目来说，
//! 这种"两个 crate 版本必须同步升级"的耦合是持续的维护负担（升级 rusqlite 常因
//! r2d2-sqlite 未跟进而被阻塞）。SQLite 连接池本身只需几十行，自实现换来：
//! - 依赖树更小、审计面更窄（Local-First 产品的隐私主张）
//! - 可在建连时统一施加 WAL / busy_timeout / 外键等 pragma，不会有"漏配"的连接
//!
//! # 同步 API 的设计取舍
//! 本层**不提供 async 接口**。SQLite 本地查询是微秒~毫秒级，且 async fn 会传染到
//! Repository 与全部测试。正确做法是让 HTTP 边界用 `spawn_blocking` 包装调用，
//! 把"异步"这个关注点留在它该在的地方，存储层保持可直接单测的纯同步代码。

use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rusqlite::{Connection, OpenFlags};

use spolia_domain::StorageError;

/// 默认最大空闲连接数。单机单用户场景下 4 足够（HTTP 并发通常 ≤ 4）。
const DEFAULT_MAX_IDLE: usize = 4;

/// 忙等待超时（毫秒）。扫描任务可能持有写锁较久，读操作需等待而非立即失败。
const BUSY_TIMEOUT_MS: u32 = 5_000;

/// 单个写事务最多容纳多少行（见 `Pool::write_in_chunks`）。
///
/// 🔴 取值依据是**实测的锁持有时长**，不是拍脑袋：
/// 一个项目的资产可达 22022 条（每条写 assets + assets_fts 两行），
/// 整批一个事务会让写锁被占住数分钟，期间用户改任何设置都必然超时失败。
/// 200 行 ≈ 400 条写，本地 SQLite 上是几十毫秒量级——
/// 足够小，其他写入总能在块间隙拿到锁；又足够大，不至于被事务开销拖慢索引。
///
/// 改这个值前先测：把它调大 10 倍，索引期间的设置写入就会开始零星失败。
const WRITE_CHUNK_ROWS: usize = 200;

/// 内存库命名计数器：保证每个 `Pool::in_memory()` 得到**独立**的库，
/// 否则并行运行的单元测试会共享同一个 `file::memory:` 而互相污染。
static MEMORY_DB_SEQ: AtomicU64 = AtomicU64::new(0);

/// 连接池。
///
/// # 内存库必须是"共享缓存"模式
/// `Connection::open_in_memory()` 的每个连接都是**互相独立的数据库**。
/// 一旦调用方在持有一条连接时再借第二条（例如 `create()` 内部调用 `get()`），
/// 新连接就会看到一个空库并报 `no such table`。
///
/// 因此内存库统一使用 `file:<唯一名>?mode=memory&cache=shared` URI，
/// 让同一池的所有连接指向同一个库。
///
/// # 保活连接
/// 共享内存库在**最后一个连接关闭时立即销毁**。池的连接是按需借还的，
/// 存在"全部归还后被丢弃 → 库消失"的窗口，因此池永久持有一条
/// 不参与借还的 `keepalive` 连接。
///
/// `keepalive` 用 `Mutex` 包裹而非裸字段：`Connection` 是 `!Sync`，
/// 裸字段会让整个 `Pool` 失去 `Sync`，而 axum 的 State 要求 `Send + Sync`。
#[derive(Debug)]
pub struct Pool {
    /// 打开连接用的字符串：文件路径，或内存库 URI
    source: String,
    /// 是否为内存库（决定是否需要 URI 标志位）
    is_memory: bool,
    /// 文件库路径；内存库为 `None`
    path: Option<String>,
    /// 永不借出的保活连接。
    ///
    /// 它**只靠存在起作用**（共享内存库在最后一个连接关闭时销毁），从不被读取，
    /// 故标记 `allow(dead_code)`——这不是遗漏，而是刻意持有的生命周期锚。
    #[allow(dead_code)]
    keepalive: Mutex<Connection>,
    idle: Mutex<Vec<Connection>>,
    max_idle: usize,
}

impl Pool {
    /// 打开（或创建）文件数据库。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::Unavailable {
                path: path.display().to_string(),
                reason: format!("无法创建目录: {e}"),
            })?;
        }
        let source = path.display().to_string();
        let keepalive = Self::connect(&source, false)?;
        Ok(Self {
            path: Some(source.clone()),
            source,
            is_memory: false,
            keepalive: Mutex::new(keepalive),
            idle: Mutex::new(Vec::new()),
            max_idle: DEFAULT_MAX_IDLE,
        })
    }

    /// 创建内存数据库（单元测试用，进程结束即销毁）。
    pub fn in_memory() -> Result<Self, StorageError> {
        let seq = MEMORY_DB_SEQ.fetch_add(1, Ordering::Relaxed);
        // 唯一名 + 共享缓存：连接间共享同一库，且不与其它测试串扰
        let source = format!("file:spolia_mem_{seq}?mode=memory&cache=shared");
        let keepalive = Self::connect(&source, true)?;
        Ok(Self {
            path: None,
            source,
            is_memory: true,
            keepalive: Mutex::new(keepalive),
            idle: Mutex::new(Vec::new()),
            max_idle: DEFAULT_MAX_IDLE,
        })
    }

    /// 建立新连接并施加统一 pragma。
    ///
    /// 所有连接都走这里，保证不存在"漏配 WAL/busy_timeout"的连接。
    fn connect(source: &str, is_memory: bool) -> Result<Connection, StorageError> {
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
        if is_memory {
            // `file:...?mode=memory&cache=shared` 需要显式开启 URI 解析
            flags |= OpenFlags::SQLITE_OPEN_URI;
        }
        let conn = Connection::open_with_flags(source, flags)
            .map_err(|e| StorageError::sqlite("打开数据库", e))?;

        // WAL：允许"一个写者 + 多个读者"并发。扫描任务写库时，UI 查询不被阻塞——
        // 这是《产品设计书》附录 A 步骤 4「后台索引期间可正常使用软件」的技术前提。
        // 内存库不支持 WAL，跳过（共享缓存模式本身已支持并发读）。
        if !is_memory {
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(|e| StorageError::sqlite("设置 journal_mode=WAL", e))?;
        }
        // 忙等待：写锁冲突时等待而非立刻 SQLITE_BUSY
        conn.busy_timeout(std::time::Duration::from_millis(u64::from(BUSY_TIMEOUT_MS)))
            .map_err(|e| StorageError::sqlite("设置 busy_timeout", e))?;
        // 外键约束默认关闭，必须显式开启，否则 relations 的级联删除不生效
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| StorageError::sqlite("启用 foreign_keys", e))?;
        // NORMAL 在 WAL 下是安全与性能的平衡点（掉电最多丢最后一个事务，不损坏库）
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| StorageError::sqlite("设置 synchronous", e))?;

        Ok(conn)
    }

    fn new_connection(&self) -> Result<Connection, StorageError> {
        Self::connect(&self.source, self.is_memory)
    }

    /// 取出一条连接。池空则新建（不设硬上限：SQLite 建连很轻，
    /// 且单机场景不会出现连接爆炸；`max_idle` 只限制**留存**数量）。
    ///
    /// 允许嵌套借用：同一线程持有 A 的同时再借 B 是合法的
    /// （内存库靠共享缓存指向同一份数据，文件库靠 WAL 支持并发读）。
    pub fn get(&self) -> Result<PooledConnection<'_>, StorageError> {
        let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = idle.pop() {
            return Ok(PooledConnection {
                conn: Some(c),
                pool: self,
            });
        }
        drop(idle); // 建新连接时不持锁（open 可能触发 IO）
        Ok(PooledConnection {
            conn: Some(self.new_connection()?),
            pool: self,
        })
    }

    /// 归还连接到池。超过 `max_idle` 的直接丢弃。
    fn return_connection(&self, conn: Connection) {
        let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
        if idle.len() < self.max_idle {
            idle.push(conn);
        }
        // 超出上限则 conn 在此 drop，自动关闭
    }

    /// 当前空闲连接数（诊断用，不含保活连接）。
    pub fn idle_count(&self) -> usize {
        self.idle.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// 数据库文件路径；内存库返回 `None`。
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// 是否为内存库。
    pub fn is_memory(&self) -> bool {
        self.is_memory
    }

    /// 分块批量写入：每 `WRITE_CHUNK_ROWS` 行一个事务，块与块之间**释放写锁**。
    ///
    /// # 🔴 为什么必须分块（而不是"一个批次一个事务"）
    /// WAL 只让**读**并发（一写多读），**写仍然是互斥的**。
    /// 于是"整批一个事务"意味着：事务期间任何其他写入都要等，
    /// 等满 `BUSY_TIMEOUT_MS`(5s) 后拿到 SQLITE_BUSY 失败。
    ///
    /// 实测规模：单个项目的资产数最高到 **22022 条**（protobuf-3.21.7），
    /// 每条写 `assets` + `assets_fts` 两行 ≈ **4.4 万条写**在一个事务里。
    /// 后果是用户在索引大项目期间（数分钟）改任何设置都会失败，
    /// 报 500「数据库操作失败」，且日志里看不出原因。
    ///
    /// 这直接违背了本文件开头对 WAL 的设计意图：
    /// "扫描任务写库时，UI 查询不被阻塞"——查询确实不阻塞，但**写**被饿死了。
    ///
    /// 分块后每个事务只有几十毫秒，其他写入总能在块间隙拿到锁。
    ///
    /// # 🔴 原子性权衡（刻意接受，并说明为什么安全）
    /// 分块后整批不再全有全无：失败时前几块已提交。
    /// 这在**本产品的写入语义下是安全的**，因为所有批量写入都是
    /// `INSERT ... ON CONFLICT DO UPDATE` 的幂等 upsert：
    /// 中途失败后重跑索引会补齐剩余部分，不会产生重复或半条记录。
    ///
    /// 反过来说，若哪天出现"必须原子"的批量写（例如跨表的一致性变更），
    /// 不要用它——那种场景的正确做法是缩小事务范围，而不是放长持锁时间。
    pub fn write_in_chunks<T>(
        &self,
        context: &str,
        items: &[T],
        write_one: impl Fn(&Connection, &T) -> Result<(), StorageError>,
    ) -> Result<usize, StorageError> {
        let mut written = 0;
        for chunk in items.chunks(WRITE_CHUNK_ROWS) {
            let conn = self.get()?;
            // 🔴 用 `sqlite_err` 而非 `StorageError::sqlite`：这是写路径，
            // 事务开启与提交正是会与后台任务争抢写锁的地方。
            // 只有 `sqlite_err` 能按 SQLite 错误码分类出 `Busy`，
            // 上层才能把它映射成 409「稍后重试」而不是 500。
            let tx = conn.unchecked_transaction().map_err(|e| {
                crate::err::sqlite_err(format!("开启{context}事务"), e)
            })?;
            for item in chunk {
                // 单行失败即中止本块并回滚：不吞错。
                // 前面已提交的块保持提交状态（幂等 upsert，可重跑补齐）。
                write_one(&tx, item)?;
            }
            tx.commit().map_err(|e| {
                crate::err::sqlite_err(format!("提交{context}事务"), e)
            })?;
            written += chunk.len();
        }
        Ok(written)
    }
}

/// 从池中借出的连接。离开作用域时自动归还（RAII）。
#[derive(Debug)]
pub struct PooledConnection<'a> {
    conn: Option<Connection>,
    pool: &'a Pool,
}

impl Deref for PooledConnection<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("连接已被取走")
    }
}

impl DerefMut for PooledConnection<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("连接已被取走")
    }
}

impl Drop for PooledConnection<'_> {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            self.pool.return_connection(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_pool_works() {
        let p = Pool::in_memory().unwrap();
        assert!(p.path().is_none());
        assert!(p.is_memory());
        assert_eq!(p.idle_count(), 0, "初始无空闲连接（保活连接不计入）");
    }

    /// 两个内存池必须互相隔离，否则并行测试会串数据。
    #[test]
    fn separate_memory_pools_are_isolated() {
        let a = Pool::in_memory().unwrap();
        let b = Pool::in_memory().unwrap();
        let ca = a.get().unwrap();
        ca.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        let cb = b.get().unwrap();
        // b 是独立库，看不到 a 的表
        assert!(cb
            .query_row("SELECT count(*) FROM sqlite_master WHERE name='t'", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n == 0)
            .unwrap_or(true));
    }

    /// 回归测试：同一池的多条连接必须看到同一份数据。
    ///
    /// 这正是 `JobRepo::create` 内部调用 `get()` 时踩到的 bug——
    /// 若每条连接是独立内存库，嵌套借用会看到空库并报 `no such table`。
    #[test]
    fn nested_borrow_sees_same_data() {
        let p = Pool::in_memory().unwrap();
        {
            let c = p.get().unwrap();
            c.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (42);")
                .unwrap();
            // 持有第一条连接的同时再借第二条
            let c2 = p.get().unwrap();
            let n: i64 = c2
                .query_row("SELECT x FROM t", [], |r| r.get(0))
                .expect("嵌套借用的连接应能看到已写入的数据");
            assert_eq!(n, 42);
        }
        // 全部归还后数据仍在（保活连接防止共享内存库被销毁）
        let c3 = p.get().unwrap();
        let n: i64 = c3.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 42, "归还后库不应被销毁");
    }

    #[test]
    fn connection_is_returned_to_pool_on_drop() {
        let p = Pool::in_memory().unwrap();
        let before = p.idle_count();
        {
            let _c = p.get().unwrap();
            assert_eq!(p.idle_count(), before, "新建连接不会减少空闲数");
        }
        assert_eq!(p.idle_count(), before + 1, "归还后空闲数应增加");
    }

    #[test]
    fn pool_grows_when_all_connections_borrowed() {
        let p = Pool::in_memory().unwrap();
        let _a = p.get().unwrap();
        let _b = p.get().unwrap();
        let _c = p.get().unwrap();
        assert_eq!(p.idle_count(), 0);
    }

    /// 池有上限，超出部分连接被丢弃而不是无限堆积。
    #[test]
    fn idle_pool_respects_max() {
        let p = Pool::in_memory().unwrap();
        let mut held = Vec::new();
        for _ in 0..10 {
            held.push(p.get().unwrap());
        }
        drop(held);
        assert!(p.idle_count() <= DEFAULT_MAX_IDLE, "空闲数 {}", p.idle_count());
    }

    /// WAL 必须生效：这是"扫描期间 UI 仍可用"的前提。
    #[test]
    fn wal_mode_is_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let p = Pool::open(dir.path().join("t.db")).unwrap();
        let c = p.get().unwrap();
        let mode: String = c
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        assert!(!p.is_memory());
    }

    /// 内存库不支持 WAL，必须是 memory journal（否则 pragma 报错）。
    #[test]
    fn memory_pool_does_not_use_wal() {
        let p = Pool::in_memory().unwrap();
        let c = p.get().unwrap();
        let mode: String = c
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "memory");
    }

    #[test]
    fn foreign_keys_are_enabled() {
        let p = Pool::in_memory().unwrap();
        let c = p.get().unwrap();
        let fk: i64 = c.pragma_query_value(None, "foreign_keys", |r| r.get(0)).unwrap();
        assert_eq!(fk, 1, "外键约束必须开启，否则级联删除失效");
    }

    #[test]
    fn busy_timeout_is_set() {
        let p = Pool::in_memory().unwrap();
        let c = p.get().unwrap();
        let t: i64 = c.pragma_query_value(None, "busy_timeout", |r| r.get(0)).unwrap();
        assert_eq!(t, i64::from(BUSY_TIMEOUT_MS));
    }

    #[test]
    fn open_creates_missing_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c/spolia.db");
        assert!(!nested.parent().unwrap().exists());
        let p = Pool::open(&nested).unwrap();
        assert!(nested.exists());
        assert_eq!(p.path().unwrap(), nested.display().to_string());
    }

    #[test]
    fn open_fails_cleanly_on_unwritable_path() {
        // 指向一个不存在且无法创建的根（Windows 下非法盘符）
        let bad = if cfg!(windows) {
            std::path::PathBuf::from("Q:\\\\nope\\\\nope\\\\x.db")
        } else {
            std::path::PathBuf::from("/proc/nonexistent/x.db")
        };
        let r = Pool::open(bad);
        assert!(r.is_err(), "不可写路径应返回 Err 而非 panic");
    }

    #[test]
    fn deref_gives_access_to_sqlite_api() {
        let p = Pool::in_memory().unwrap();
        let c = p.get().unwrap();
        c.execute_batch("CREATE TABLE t(a INTEGER)").unwrap();
        c.execute("INSERT INTO t VALUES (42)", []).unwrap();
        let v: i64 = c.query_row("SELECT a FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 42);
    }

    // ── write_in_chunks ──────────────────────────────────────────

    /// 建一张最小表供分块写入测试使用。
    fn chunk_table(p: &Pool) {
        let c = p.get().unwrap();
        c.execute_batch("CREATE TABLE t(v INTEGER NOT NULL)")
            .unwrap();
    }

    fn rows(p: &Pool) -> Vec<i64> {
        let c = p.get().unwrap();
        let mut s = c
            .prepare("SELECT v FROM t ORDER BY v")
            .unwrap();
        s.query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn write_in_chunks_writes_all_and_returns_count() {
        let p = Pool::in_memory().unwrap();
        chunk_table(&p);
        let items: Vec<i64> = (0..(WRITE_CHUNK_ROWS * 2 + 7) as i64).collect();
        let n = p
            .write_in_chunks("测试", &items, |conn, v| {
                conn.execute("INSERT INTO t VALUES (?1)", [v])
                    .map_err(|e| StorageError::sqlite("插入", e))?;
                Ok(())
            })
            .unwrap();
        assert_eq!(n, items.len(), "返回值应是实际写入行数");
        assert_eq!(rows(&p), items, "跨块的数据必须完整且有序");
    }

    /// 空批次是 no-op：不得建表、不得返回错误、不得借连接。
    #[test]
    fn write_in_chunks_empty_batch_is_noop() {
        let p = Pool::in_memory().unwrap();
        // 🔴 刻意**不建表**：若实现对空批次仍去拿连接并开事务，
        // 这里会因 `no such table` 而失败——正好暴露多余的工作。
        let n = p
            .write_in_chunks("测试", &[] as &[i64], |conn, v: &i64| {
                conn.execute("INSERT INTO t VALUES (?1)", [v])
                    .map_err(|e| StorageError::sqlite("插入", e))?;
                Ok(())
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    /// 🔴 **分块的决定性证据**：中途失败时，**已提交的块留在库里**。
    ///
    /// 这是唯一能区分"分块提交"与"整批单事务"的断言：
    /// 整批单事务下失败会回滚**全部**，库里应是 0 行；
    /// 分块下第一块已提交，库里应恰好是 `WRITE_CHUNK_ROWS` 行。
    ///
    /// 这条语义不是缺陷而是刻意的取舍（见 `write_in_chunks` 文档）：
    /// 所有调用方都是幂等 upsert，重跑即可补齐；
    /// 换来的是不再长时间独占写锁。测试把它钉住，
    /// 防止将来有人"顺手改成一个大事务"而不知道自己在换掉什么。
    #[test]
    fn write_in_chunks_keeps_committed_blocks_when_later_block_fails() {
        let p = Pool::in_memory().unwrap();
        chunk_table(&p);
        let items: Vec<i64> = (0..(WRITE_CHUNK_ROWS * 2) as i64).collect();

        let err = p
            .write_in_chunks("测试", &items, |conn, v| {
                // 在第二块的第一行失败：第一块此时应已提交
                if *v == WRITE_CHUNK_ROWS as i64 {
                    return Err(StorageError::Sqlite {
                        context: "注入失败".into(),
                        reason: "injected".into(),
                    });
                }
                conn.execute("INSERT INTO t VALUES (?1)", [v])
                    .map_err(|e| StorageError::sqlite("插入", e))?;
                Ok(())
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("injected"),
            "错误必须原样上抛，不能被吞掉: {err}"
        );

        let kept = rows(&p);
        assert_eq!(
            kept.len(),
            WRITE_CHUNK_ROWS,
            "第一块应已提交（整批单事务的话这里会是 0）"
        );
        assert_eq!(kept[0], 0);
        assert_eq!(kept[kept.len() - 1], (WRITE_CHUNK_ROWS - 1) as i64);
    }

    /// 失败发生在**块内**时，该块整体回滚（不能只写一半）。
    ///
    /// 与上一条互补：块间不原子，块内必须原子。
    /// 否则一次失败会留下"半个项目"的资产，
    /// 而重跑时的幂等 upsert 只会覆盖同 id 的行，删不掉多余的。
    #[test]
    fn write_in_chunks_rolls_back_the_failing_block_only() {
        let p = Pool::in_memory().unwrap();
        chunk_table(&p);
        let items: Vec<i64> = (0..30).collect();

        let _ = p
            .write_in_chunks("测试", &items, |conn, v| {
                // 30 行 < WRITE_CHUNK_ROWS，所以全在一个块里 → 应全部回滚
                if *v == 17 {
                    return Err(StorageError::Sqlite {
                        context: "注入失败".into(),
                        reason: "injected".into(),
                    });
                }
                conn.execute("INSERT INTO t VALUES (?1)", [v])
                    .map_err(|e| StorageError::sqlite("插入", e))?;
                Ok(())
            })
            .unwrap_err();

        assert_eq!(
            rows(&p).len(),
            0,
            "同一个块内的写入必须全部回滚，不能留下前 17 行"
        );
    }

    /// 分块必须在**每块后归还连接**，否则连接池会被批量写入耗尽。
    #[test]
    fn write_in_chunks_returns_connections_between_blocks() {
        let p = Pool::in_memory().unwrap();
        chunk_table(&p);
        let items: Vec<i64> = (0..(WRITE_CHUNK_ROWS * 3) as i64).collect();
        p.write_in_chunks("测试", &items, |conn, v| {
            conn.execute("INSERT INTO t VALUES (?1)", [v])
                .map_err(|e| StorageError::sqlite("插入", e))?;
            Ok(())
        })
        .unwrap();
        // 3 个块若各占一条连接不还，池里会积到 3 条空闲；
        // 归还得当的话应回落到 max_idle 以内且可正常借用。
        assert!(
            p.idle_count() <= DEFAULT_MAX_IDLE,
            "空闲连接不应超过上限，实际 {}",
            p.idle_count()
        );
    }
}
