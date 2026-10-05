//! projectAssests 服务进程入口。
//!
//! # 职责边界
//! 这里只做三件事：初始化日志、打开数据库、绑定端口。
//! 业务逻辑在 `projectassests_service`，协议映射在 `routes.rs` / `error.rs`。
//!
//! # 🔴 所有模块必须在此显式声明
//! 早期版本只有 `fn main() {}`，`state.rs` 与 `error.rs` 从未被 `mod` 引入，
//! 于是这 25KB 代码**根本没参与编译**——里面的 `#[cfg(test)]` 测试也从未运行过
//! （`cargo test` 显示 `running 0 tests`）。
//!
//! 这类"文件存在但没被编译"的死代码不会有任何报错或警告，
//! 只能靠测试计数为 0 发现。因此每新增一个 `apps/server/src/*.rs`
//! 都必须在此登记，否则它会静默地不参与构建。

mod error;
mod routes;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::state::AppState;

/// 默认监听地址。
///
/// 🔴 只绑 `127.0.0.1`，绝不绑 `0.0.0.0`：
/// 这个服务能读取用户本机任意已授权目录的代码内容，
/// 暴露到局域网等于把代码库开放给同网段所有人。
/// 桌面应用没有跨机器访问的需求。
const DEFAULT_ADDR: &str = "127.0.0.1:8787";

/// 命令行参数。
///
/// 🔴 手写解析而非引入 `clap`：只有 `--addr` 与 `--db` 两个可选参数，
/// 为一个 30 行的解析器增加一棵依赖树（clap 带 derive/syn 等 20+ crate）
/// 与"简洁原则"相悖。参数超过四五个时再考虑换 clap。
#[derive(Debug, PartialEq)]
struct Cli {
    /// 监听地址（host:port）
    addr: String,
    /// 数据库文件路径。缺省时放在用户数据目录。
    db: Option<PathBuf>,
}

impl Cli {
    /// 解析参数。遇到未知参数返回 `Err`（而不是静默忽略）：
    /// 用户拼错 `--dbs` 时若被忽略，服务会用默认库启动，
    /// 数据写进错误的文件，而用户完全看不出来。
    fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut cli = Self {
            addr: DEFAULT_ADDR.to_string(),
            db: None,
        };
        let mut it = args.into_iter().skip(1); // 跳过 argv[0]
        while let Some(a) = it.next() {
            match a.as_str() {
                "--addr" => {
                    cli.addr = it
                        .next()
                        .ok_or_else(|| "--addr 需要一个值，如 --addr 127.0.0.1:8787".to_string())?;
                }
                "--db" => {
                    let v = it
                        .next()
                        .ok_or_else(|| "--db 需要一个文件路径".to_string())?;
                    cli.db = Some(PathBuf::from(v));
                }
                "-h" | "--help" => return Err(usage()),
                other => {
                    return Err(format!("未知参数 {other}\n\n{}", usage()));
                }
            }
        }
        Ok(cli)
    }
}

fn usage() -> String {
    format!(
        "projectAssests 本地服务进程\n\n用法: projectassests-server [选项]\n\n选项:\n  --addr <host:port>  监听地址（默认 {DEFAULT_ADDR}）\n  --db <path>         数据库文件路径（默认放在用户数据目录）\n  -h, --help          显示此帮助"
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 日志：默认 INFO，可用 RUST_LOG 覆盖（排查问题时需要 DEBUG）
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = match Cli::parse_args(std::env::args()) {
        Ok(c) => c,
        Err(msg) => {
            // --help 走 stderr 但退出码 0；参数错误退出码非 0
            let is_help = msg.starts_with("projectAssests 本地服务进程");
            if is_help {
                println!("{msg}");
                return Ok(());
            }
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    let db_path = match cli.db {
        Some(p) => p,
        None => default_db_path()?,
    };

    // 确保父目录存在：用户数据目录在首次运行时可能还没被创建
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let state = AppState::open(&db_path).map_err(|e| {
        // 🔴 数据库打不开是最常见的启动失败原因（路径无权限、磁盘满、文件损坏）。
        // 必须把路径打出来，否则用户只看到"启动失败"却不知道该修哪里。
        anyhow::anyhow!("无法打开数据库 {}: {e}", db_path.display())
    })?;

    let addr: SocketAddr = cli.addr.parse().map_err(|e| {
        anyhow::anyhow!("监听地址无效 `{}`：{e}（期望 host:port，如 127.0.0.1:8787）", cli.addr)
    })?;

    let app = crate::routes::router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    // 实际绑定的端口可能与请求的不同（addr 用 :0 时由系统分配）。
    // 桌面壳需要知道真实端口才能连上来，所以必须打出来。
    let bound = listener.local_addr()?;
    tracing::info!(
        address = %bound,
        database = %db_path.display(),
        "projectAssests 服务已启动"
    );
    // 供父进程（Tauri / 开发脚本）解析
    println!("listening on http://{bound}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("projectAssests 服务已停止");
    Ok(())
}

/// 默认数据库路径。
///
/// 不放在项目目录里：用户可能把仓库 clone 到只读位置，
/// 而且派生数据不该混进版本库。
///
/// 🔴 不引入 `dirs` crate：只用环境变量就能覆盖三个平台
/// （Windows 的 `%APPDATA%`、macOS 的 `$HOME/Library/Application Support`、
/// Linux 的 `$XDG_DATA_HOME` 或 `$HOME/.local/share`），
/// 为一个路径查找增加一个依赖不划算。
fn default_db_path() -> anyhow::Result<PathBuf> {
    // Windows
    if let Some(v) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(v).join("projectassests").join("projectassests.db"));
    }
    // Linux XDG
    if let Some(v) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(v).join("projectassests").join("projectassests.db"));
    }
    // macOS 与 Linux 回退
    if let Some(v) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        let home = PathBuf::from(v);
        #[cfg(target_os = "macos")]
        let base = home.join("Library").join("Application Support");
        #[cfg(not(target_os = "macos"))]
        let base = home.join(".local").join("share");
        return Ok(base.join("projectassests").join("projectassests.db"));
    }
    Err(anyhow::anyhow!(
        "无法确定用户数据目录（APPDATA / XDG_DATA_HOME / HOME 均未设置），请用 --db 显式指定"
    ))
}

/// 优雅关闭信号。
///
/// 🔴 必须处理 Ctrl-C：直接 kill 会在数据库里留下 `running` 状态的任务记录，
/// 下次启动虽然能靠 `reap_stale_jobs` 收割，但用户会看到"上次扫描失败了"
/// 这类实际并不存在的故障提示。
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "无法安装 Ctrl-C 处理器");
            // 安装失败就永远等待，而不是立刻返回导致服务启动后立即退出
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "无法安装 SIGTERM 处理器");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("收到 Ctrl-C，正在关闭"),
        _ = terminate => tracing::info!("收到终止信号，正在关闭"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把参数列表包成带 argv[0] 的迭代器。
    fn parse(args: &[&str]) -> Result<Cli, String> {
        let owned = std::iter::once("projectassests-server".to_string())
            .chain(args.iter().map(|s| s.to_string()));
        Cli::parse_args(owned)
    }

    #[test]
    fn default_addr_is_loopback_only() {
        // 🔴 安全红线：默认绝不能绑 0.0.0.0
        assert!(
            DEFAULT_ADDR.starts_with("127.0.0.1:"),
            "默认地址必须只监听回环，实得 {DEFAULT_ADDR}"
        );
        assert!(!DEFAULT_ADDR.contains("0.0.0.0"));
    }

    #[test]
    fn default_addr_parses() {
        let addr: SocketAddr = DEFAULT_ADDR.parse().unwrap();
        assert!(addr.ip().is_loopback());
    }

    #[test]
    fn cli_parses_defaults() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.addr, DEFAULT_ADDR);
        assert!(cli.db.is_none());
    }

    #[test]
    fn cli_accepts_overrides() {
        let cli = parse(&["--addr", "127.0.0.1:9000", "--db", "/tmp/x.db"]).unwrap();
        assert_eq!(cli.addr, "127.0.0.1:9000");
        assert_eq!(cli.db.unwrap().to_string_lossy(), "/tmp/x.db");
    }

    #[test]
    fn unknown_arg_is_rejected_not_ignored() {
        // 🔴 拼错 --db 为 --dbs 若被静默忽略，服务会用默认库启动，
        // 数据写进错误的文件而用户看不出来
        let err = parse(&["--dbs", "/tmp/x.db"]).unwrap_err();
        assert!(err.contains("未知参数"), "{err}");
        assert!(err.contains("--dbs"), "{err}");
    }

    #[test]
    fn missing_value_is_rejected() {
        let err = parse(&["--addr"]).unwrap_err();
        assert!(err.contains("--addr"), "{err}");
        let err = parse(&["--db"]).unwrap_err();
        assert!(err.contains("--db"), "{err}");
    }

    #[test]
    fn help_exits_cleanly() {
        let msg = parse(&["--help"]).unwrap_err();
        assert!(msg.starts_with("projectAssests 本地服务进程"), "{msg}");
        assert!(msg.contains("--addr"), "{msg}");
    }

    #[test]
    fn default_db_path_ends_with_db_file() {
        // 在无 HOME/APPDATA 的 CI 环境可能返回 Err，两种都可接受，但不能 panic
        match default_db_path() {
            Ok(p) => {
                assert!(p.to_string_lossy().ends_with("projectassests.db"), "{p:?}");
                assert!(p.to_string_lossy().contains("projectassests"), "{p:?}");
            }
            Err(e) => assert!(e.to_string().contains("--db"), "应提示用户显式指定: {e}"),
        }
    }
}
