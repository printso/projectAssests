//! 测试专用的极小 HTTP 桩服务。
//!
//! # 为什么不引 mock 框架
//! 本 crate 只需要一种能力：**按路径返回预置响应**。
//! 为此引入 wiremock/httpmock 会给 Local-First 的桌面应用添一个
//! 只在测试期用得上、却要长期维护版本的重依赖，不划算。
//!
//! # 为什么用 `std::net` 而不是 `tokio::net`
//! workspace 的 tokio 只开了 `macros/rt-multi-thread/sync/time/signal`，
//! 没有 `net` 与 `io-util`。为一个测试桩去扩 tokio 特性会连带影响
//! 所有依赖它的 crate；桩跑在自己的阻塞线程里，`std::net` 足够。
//!
//! 🔴 桩必须能表达"**列表端点正常、推理端点报错**"这种分裂状态——
//! 那正是连接自检假绿的成因（见 `openai.rs::health_check` 的注释）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 一条路由：路径后缀 → (状态码, 响应体)。
pub type Route = (&'static str, u16, &'static str);

/// 跑在后台线程上的桩服务，`Drop` 时自动停止。
pub struct StubServer {
    /// 形如 `http://127.0.0.1:54321`，可直接当 base_url 用。
    pub base_url: String,
    /// 收到的请求路径（按到达顺序），用于断言"探针确实发出去了"。
    pub hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubServer {
    /// 在随机空闲端口上启动桩。`routes` 按顺序匹配，先命中先生效。
    pub fn start(routes: Vec<Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("桩服务应能绑定回环端口");
        let port = listener.local_addr().expect("应有本地地址").port();
        // 🔴 非阻塞 accept + 轮询停止标志：`TcpListener` 没有 `set_read_timeout`，
        // 阻塞式 `incoming()` 又无法在 Drop 时被唤醒（会卡住 join）。
        listener
            .set_nonblocking(true)
            .expect("应能切到非阻塞");

        let stop = Arc::new(AtomicBool::new(false));
        let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let t_stop = Arc::clone(&stop);
        let t_hits = Arc::clone(&hits);
        let handle = std::thread::spawn(move || {
            while !t_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let routes = routes.clone();
                        let hits = Arc::clone(&t_hits);
                        // 每个连接一个线程：reqwest 会并发打 /models 与 /chat/completions，
                        // 串行处理会让自检的两步互相等待而超时。
                        std::thread::spawn(move || serve_one(stream, &routes, &hits));
                    }
                    // WouldBlock = 暂时没有新连接，正常；其余错误则放弃这条连接继续
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => {}
                }
                // 2ms 轮询间隔：对测试足够灵敏，CPU 占用可忽略
                std::thread::sleep(Duration::from_millis(2));
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            hits,
            stop,
            handle: Some(handle),
        }
    }

    /// 是否收到过路径以 `suffix` 结尾的请求。
    pub fn hit(&self, suffix: &str) -> bool {
        self.hits
            .lock()
            .expect("hits 锁不应中毒")
            .iter()
            .any(|p| p.ends_with(suffix))
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            // 监听线程每 2ms 醒一次，join 几乎立刻返回
            let _ = h.join();
        }
    }
}

/// 处理一个连接：读完整请求 → 匹配路由 → 写响应 → 关闭。
fn serve_one(mut stream: TcpStream, routes: &[Route], hits: &Arc<Mutex<Vec<String>>>) {
    // 🔴 必须显式切回**阻塞**模式。
    //
    // 监听 socket 是非阻塞的（否则 Drop 时无法唤醒 accept 循环），
    // 而 **Windows 上 `accept()` 返回的 socket 会继承监听 socket 的非阻塞属性**
    // （Linux/macOS 不继承）。不切回来，`read` 会立刻返回 `WouldBlock`，
    // 请求行根本没读到，连接被当成"无匹配路由"提前关闭，
    // 客户端得到 `error sending request` —— 表现为**随机** flaky 失败
    // （实测 12 次跑挂 6 次）。这类跨平台差异必须在这里一次性掐掉。
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();

    let head = match read_request(&mut stream) {
        Some(h) => h,
        None => return,
    };
    // 请求行形如 `POST /v1/chat/completions HTTP/1.1`
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();

    hits.lock().expect("hits 锁不应中毒").push(path.clone());

    let (status, body) = routes
        .iter()
        .find(|(suffix, _, _)| path.ends_with(suffix))
        .map(|(_, s, b)| (*s, *b))
        .unwrap_or((404, r#"{"error":{"message":"stub: 未预置该路径"}}"#));

    // 🔴 必须发 `Connection: close`：桩不支持 keep-alive，
    // 不声明的话 reqwest 会复用这条已被关闭的连接，得到难以归因的连接错误。
    let resp = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason_phrase(status),
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

/// 读到请求头结束（`\r\n\r\n`），并按 `Content-Length` 把请求体也读干净。
///
/// 🔴 一次 `read` 读不全是常态：头部与请求体常分属不同 TCP 段。
/// 只读一次就走，会在客户端仍在发送时关闭连接，触发 RST。
fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let mut header_end = None;

    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break; // 对端关闭
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            header_end = Some(pos + 4);
            break;
        }
        // 请求头不该无限长；超长说明这不是 HTTP，直接放弃
        if buf.len() > 64 * 1024 {
            return None;
        }
    }

    let end = header_end?;
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();

    // 把声明的请求体读满：客户端发完才会等响应，提前关闭会被它当成故障
    if let Some(len) = content_length(&head) {
        let mut got = buf.len().saturating_sub(end);
        while got < len {
            let n = stream.read(&mut chunk).ok()?;
            if n == 0 {
                break;
            }
            got += n;
        }
    }
    Some(head)
}

/// 从头文本里取 `Content-Length`；未声明或非法则为 `None`。
fn content_length(head: &str) -> Option<usize> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k.trim().eq_ignore_ascii_case("content-length") {
            v.trim().parse::<usize>().ok()
        } else {
            None
        }
    })
}

/// 在字节流里找子串首次出现的位置（仅测试用，朴素实现足够）。
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    }
}

/// 阿里云百炼"模型未开通"的真实响应体（本 bug 的现场证据）。
pub const NOT_ACTIVATED_BODY: &str = r#"{"code":"InvalidParameter","message":"The product is not activated, please confirm that you have activated products and try again after activation.","request_id":"b0509678"}"#;

/// `/models` 的真实形态：**列表里确实有这个模型**，但它并不能被调用。
/// 这个矛盾正是"只列模型就报绿"必然假绿的原因。
pub const MODELS_WITH_UNUSABLE_BODY: &str =
    r#"{"object":"list","data":[{"id":"ZHIPU/GLM-5.3-FlashX"},{"id":"glm-5.3"},{"id":"qwen-plus"}]}"#;

/// 一次成功推理的最小响应体。
pub const CHAT_OK_BODY: &str = r#"{"choices":[{"message":{"role":"assistant","content":"pong"}}],"model":"m","usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
