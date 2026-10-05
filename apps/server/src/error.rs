//! 统一响应信封与错误映射。
//!
//! # 为什么需要信封
//! 前端对每个请求都要区分三件事：成功了吗、数据是什么、失败该怎么提示。
//! 若成功返回裸数据、失败返回裸错误串，前端就得对每个端点写两套解析逻辑，
//! 且无法统一处理"424 = 需要先去配置"这类需要**引导**而非报错的情况。
//!
//! 信封把这三件事固定下来：
//! ```json
//! { "success": true,  "data": {...} }
//! { "success": false, "error": { "code": "not_configured", "message": "…", "hint": "…" } }
//! ```
//!
//! # hint 字段的意义
//! `message` 说明"发生了什么"，`hint` 说明"该怎么办"。
//! 二者分开，前端就能在 toast 里显示简短消息、在页面里显示可操作的引导，
//! 而不是把一长串技术描述甩给用户。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// 成功响应信封。
#[derive(Debug, Serialize)]
pub struct Envelope<T: Serialize> {
    pub success: bool,
    pub data: T,
}

/// 把数据包成成功信封。
pub fn ok<T: Serialize>(data: T) -> Json<Envelope<T>> {
    Json(Envelope {
        success: true,
        data,
    })
}

/// 空响应体（用于 DELETE / 触发类端点）。
///
/// 刻意用具体类型而非 `()`：让前端始终能拿到 `data` 字段，
/// 不必对某些端点做 `data === undefined` 的特判。
#[derive(Debug, Serialize)]
pub struct Empty {
    /// 受影响的记录数（适用时填充，否则为 `None`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected: Option<usize>,
    /// 面向用户的确认文案（前端 toast 直接展示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Empty {
    pub fn new() -> Self {
        Self {
            affected: None,
            message: None,
        }
    }

    pub fn affected(mut self, n: usize) -> Self {
        self.affected = Some(n);
        self
    }

    pub fn message(mut self, m: impl Into<String>) -> Self {
        self.message = Some(m.into());
        self
    }
}

impl Default for Empty {
    fn default() -> Self {
        Self::new()
    }
}

/// API 错误。实现 `IntoResponse`，可直接作为 handler 的 `Err` 返回。
#[derive(Debug)]
pub struct ApiError {
    /// 机器可读的稳定错误码（前端据此分支处理，不能靠匹配 message 文本）
    pub code: &'static str,
    /// 面向用户的中文说明
    pub message: String,
    /// 可操作的下一步建议；`None` 表示无明确引导
    pub hint: Option<String>,
    pub status: StatusCode,
    /// 是否为服务端故障（5xx）。
    /// 🔴 只有这类错误才记 error 日志：把用户的"没配模型"记成服务端错误，
    /// 会让真正的故障淹没在噪音里。
    pub internal: bool,
    /// 诊断细节：**只写日志，绝不进响应体**（`ErrorDetail` 刻意不含此字段）。
    ///
    /// 🔴 为什么必须与 `message` 分开：
    /// `message` 是给用户看的，对存储故障只能笼统说"数据库操作失败"
    /// （SQLite 错误可能含数据库文件绝对路径，属于本机信息泄露）。
    /// 但排查需要的是 `context (reason)`——比如
    /// `保存扫描设置 (database is locked)`。
    ///
    /// 两者合成一个字段的结果是二选一的坏结局：
    /// 要么泄露路径给用户，要么日志里只剩一句无法定位的"数据库操作失败"。
    /// 后者正是曾经的状态：索引任务占住写锁时，日志刷两行同样的话，
    /// 完全看不出是锁竞争、还是 SQL 写错、还是磁盘满。
    pub detail: Option<String>,
}

/// 错误响应体。
#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    success: bool,
    error: ErrorDetail<'a>,
}

#[derive(Debug, Serialize)]
struct ErrorDetail<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
}

impl ApiError {
    pub fn new(code: &'static str, message: impl Into<String>, status: StatusCode) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            status,
            internal: status.is_server_error(),
            detail: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    // 🔴 这里**刻意没有** `bad_request()` / `not_found()` / `precondition()` /
    // `conflict()` 这类快捷构造器。
    //
    // 业务错误的 code / status / hint 只有一个真相源：`ServiceError`
    // （见 `impl From<ServiceError> for ApiError`）。提供快捷构造器等于
    // 给未来的贡献者开一条"在 handler 里手搓错误"的后门，
    // 而手搓的错误码与 service 层必然漂移——同一个"项目不存在"，
    // 这个端点返回 not_found、那个端点返回 bad_request，前端无法统一处理。
    //
    // 唯一保留的手工入口是 `internal()`：它对应的是适配器自身故障
    // （spawn_blocking 的线程 panic、DB stats 采集失败），
    // 这类错误 service 层根本不知道，只能在这里产生。

    /// 内部错误（500）。
    ///
    /// 🔴 不把底层错误细节透给用户：SQLite 错误可能含数据库文件路径。
    /// 细节写日志，用户只看到"内部错误 + 去哪看日志"。
    pub fn internal(context: &str, err: impl std::fmt::Display) -> Self {
        tracing::error!(context, error = %err, "请求处理失败");
        Self {
            code: "internal_error",
            message: format!("{context}失败，详情请查看日志").to_string(),
            hint: Some("若反复出现，请提交 issue 并附上日志".to_string()),
            status: StatusCode::INTERNAL_SERVER_ERROR,
            internal: true,
            detail: Some(err.to_string()),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // 只有服务端故障记 error；用户侧问题（没配置、参数错）记 debug，
        // 否则日志会被正常使用产生的"错误"淹没。
        //
        // 🔴 detail 只出现在这里，**不进响应体**（下面的 ErrorDetail 不含它）。
        // 用户看到的仍是笼统的 message（存储故障可能含本机路径），
        // 而排查者能从日志里拿到 `context (reason)`，例如
        // `保存扫描设置 (database is locked)` —— 这才定位得了问题。
        if self.internal {
            tracing::error!(
                code = self.code,
                status = %self.status,
                message = %self.message,
                detail = self.detail.as_deref().unwrap_or("-"),
            );
        } else {
            tracing::debug!(
                code = self.code,
                status = %self.status,
                message = %self.message,
                detail = self.detail.as_deref().unwrap_or("-"),
            );
        }

        let body = ErrorBody {
            success: false,
            error: ErrorDetail {
                code: self.code,
                message: &self.message,
                hint: self.hint.as_deref(),
            },
        };
        (self.status, Json(body)).into_response()
    }
}

/// `ServiceError` → `ApiError`。
///
/// 🔴 这里只做**机械转换**，不重新判断语义：
/// `code` / `hint` / `status_code` 都由 service 层的 `ServiceError` 自己给出。
///
/// 为什么不让适配器自己映射状态码：同一类错误若在这里再判断一遍，
/// Tauri 适配器还得判断第三遍。三份映射必然漂移，
/// 典型症状是"网页上提示去设置页配置模型，桌面端只报一个 500"。
/// 语义集中在 `ServiceError`，两种传输都只是把它翻译成各自的格式。
///
/// # 🔴 曾经存在的第二套映射（已删除）
/// 本文件早期还有一个 `impl From<SpoliaError> for ApiError`，
/// 手写了完整的 code/hint 表。它与 `ServiceError` 的映射**平行且已经漂移**：
/// `Scanner(DirNotFound)` 在那边是 404、在 service 层是 424；
/// `Search(IndexNotReady)` 在这边有专属 code、在 service 层却落到通用 500。
///
/// 生产路径只走 `From<ServiceError>`，那套映射因此从未被执行，
/// 但它一直在误导读者——看到两份表的人会以为两份都生效，
/// 于是改了一处、另一处继续错。已把其中更精确的部分
/// （`index_not_ready` / `storage_unavailable`）上移到 `ServiceError`。
impl From<spolia_service::ServiceError> for ApiError {
    fn from(e: spolia_service::ServiceError) -> Self {
        let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let code = e.code();
        let hint = e.hint();
        let message = e.to_string();
        let internal = status.is_server_error();

        // 🔴 detail 沿**错误源链**收集，而不是只用 `e.to_string()`。
        //
        // `ServiceError::Storage` 的 Display 是写死的"数据库操作失败"
        // （刻意笼统：SQLite 错误可能含本机路径，不能外泄给用户），
        // 真正有价值的是内层 `StorageError::Sqlite` 的
        // `context (reason)` —— 例如"保存扫描设置 (database is locked)"。
        // 只记外层等于什么都没记：索引任务占住写锁时，
        // 日志里只有两行"数据库操作失败"，完全看不出是锁竞争还是 SQL 写错。
        let detail = error_chain(&e);

        // 🔴 这里**不再记日志**：`IntoResponse::into_response` 是唯一记录点。
        // 曾经两处都记，于是一次失败的请求在日志里出现**两行完全相同**的 ERROR
        // （实测：写锁竞争时刷出两行 `code="storage_error"`）。
        // 重复行不只是噪音——它让人误以为发生了两次故障，
        // 也让"日志行数 = 故障次数"这个直觉失效。

        // 🔴 与 message 相同时不带 detail：绝大多数错误（NotFound/Invalid/Ai…）
        // 的 Display 已经是完整信息，再附一遍只是让日志更吵。
        // 只有 Storage 这类"外层刻意笼统"的才值得附上内层。
        // 这个比较必须在构造结构体**之前**做完：结构体字面量的 `message,`
        // 会移走所有权，之后再读就是 use-after-move（编译不过）。
        let detail = (detail != message).then_some(detail);

        ApiError {
            code,
            message,
            hint,
            status,
            internal,
            detail,
        }
    }
}

/// 沿 `std::error::Error::source()` 链把每层 Display 拼成一行。
///
/// 🔴 为什么不用 `e.to_string()`：外层 Display 常常是**刻意笼统**的
/// （`ServiceError::Storage` 固定输出"数据库操作失败"，因为 SQLite 错误
/// 可能含本机路径、不能进响应体）。真正的诊断信息在内层
/// `StorageError::Sqlite { context, reason }`。
/// 只记外层，日志里就只剩一句无法定位的话。
///
/// 为什么不用子串匹配（如 `reason.contains("locked")`）来识别锁竞争：
/// 项目已有明确纪律——靠文案匹配决定行为是反模式
/// （见 `context.rs` 中 `Job(AlreadyRunning)` 的注释：文案一改就失效，
/// 且没有任何编译期保护）。类型层面的识别见 `StorageError::is_busy`。
///
/// 输出形如 `数据库操作失败 ← 保存扫描设置 (database is locked)`。
fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    // 深度上限：错误链正常只有 2~3 层。设上限是防止某个错误类型
    // 意外构造出环（`source()` 返回自身），那样这里会无限增长内存。
    for _ in 0..8 {
        let Some(s) = src else { break };
        out.push_str(" ← ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

/// axum 的 `Json<T>` 提取失败 → 统一信封。
///
/// 🔴 必须转换：`Json<T>` 直接放在 handler 参数里时，
/// 请求体畸形会由 axum 返回**它自己的纯文本错误体**，
/// 绕过我们的信封。前端于是得写两套错误解析逻辑
/// （一套读 `{success,error}`，一套读裸字符串），
/// 这正是信封设计要消除的问题。
///
/// handler 因此应写 `body: Result<Json<T>, ApiError>`（见 `JsonBody`）。
impl From<axum::extract::rejection::JsonRejection> for ApiError {
    fn from(e: axum::extract::rejection::JsonRejection) -> Self {
        use axum::extract::rejection::JsonRejection::*;
        // 按拒绝原因区分：请求体缺失/畸形是客户端问题（400），
        // 而 body 读取出错（连接中断）是 400 但不必提示用户改输入。
        let (status, message, hint) = match &e {
            JsonDataError(msg) => (
                StatusCode::BAD_REQUEST,
                format!("请求体格式不正确：{msg}"),
                Some("请检查 JSON 字段名与类型是否符合接口定义".to_string()),
            ),
            JsonSyntaxError(_) => (
                StatusCode::BAD_REQUEST,
                "请求体不是合法的 JSON".to_string(),
                Some("检查是否有多余逗号、未闭合的引号或括号".to_string()),
            ),
            MissingJsonContentType(_) => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "缺少 Content-Type: application/json 请求头".to_string(),
                Some("POST/PUT 请求必须声明 JSON 类型".to_string()),
            ),
            BytesRejection(_) => (
                StatusCode::BAD_REQUEST,
                "无法读取请求体".to_string(),
                None,
            ),
            // axum 用 `#[non_exhaustive]`，新版本可能加变体：
            // 兜底走 400 而不是编译失败。
            _ => (StatusCode::BAD_REQUEST, e.to_string(), None),
        };
        let mut api = ApiError::new("bad_request", message, status);
        if let Some(h) = hint {
            api = api.with_hint(h);
        }
        api
    }
}

/// axum 的 `Query<T>` 提取失败 → 统一信封（理由同 `JsonRejection`）。
impl From<axum::extract::rejection::QueryRejection> for ApiError {
    fn from(e: axum::extract::rejection::QueryRejection) -> Self {
        use axum::extract::rejection::QueryRejection::*;
        // axum 0.8 的 QueryRejection 只有 FailedToDeserializeQueryString 一个变体。
        // 保留 `_` 兜底：该类型是 `#[non_exhaustive]`，升级后新增变体不该编译失败。
        let (message, hint) = match &e {
            FailedToDeserializeQueryString(_) => (
                format!("查询参数无法解析：{e}"),
                Some("检查参数名与取值，例如 `limit` 必须是数字".to_string()),
            ),
            _ => (e.to_string(), None),
        };
        let mut api = ApiError::new("bad_request", message, StatusCode::BAD_REQUEST);
        if let Some(h) = hint {
            api = api.with_hint(h);
        }
        api
    }
}

/// JSON 请求体提取器（拒绝时走统一信封）。
///
/// 🔴 handler 必须用它而不是 axum 的 `Json<T>`：
/// `Json<T>` 的 `Rejection` 是 axum 自己的类型，`IntoResponse` 输出**纯文本**，
/// 绕过我们的信封。前端因此要为"业务错误"和"请求格式错误"写两套解析逻辑。
/// 已实测确认：用 `Json<T>` 时畸形请求体返回的 body 无法被 JSON 解析。
///
/// ```ignore
/// async fn handler(JsonBody(req): JsonBody<MyRequest>) -> ApiResult<impl IntoResponse> {
///     // 走到这里 req 一定是合法的；畸形请求体已在提取阶段变成信封错误
/// }
/// ```
///
/// 用 newtype 而非 `Result<Json<T>, ApiError>` 别名：
/// 别名写法要求每个 handler 都多一行 `let Json(req) = body?;`，
/// 27 个 handler 就是 27 处容易漏写的样板；newtype 保持解构签名不变。
pub struct JsonBody<T>(pub T);

impl<T, S> axum::extract::FromRequest<S> for JsonBody<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        // 🔴 这里**不需要** `use axum::extract::FromRequest`：
        // 在 impl 块内，正在实现的 trait 本身就在作用域中，
        // 因此 `Json::<T>::from_request`（同一 trait 在另一类型上的实现）可直接调用。
        // 多写一行 use 会让人以为 trait 必须显式导入，反而误导。
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(v)| Self(v))
            .map_err(ApiError::from)
    }
}

/// 查询参数提取器（拒绝时走统一信封，理由同 `JsonBody`）。
///
/// 实现 `FromRequestParts` 而非 `FromRequest`：它只读 URI，不消费 body，
/// 因此可以与 `JsonBody` 共存于同一 handler（顺序无关）。
pub struct QueryOf<T>(pub T);

impl<T, S> axum::extract::FromRequestParts<S> for QueryOf<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        // 同上：FromRequestParts 由 impl 头引入作用域，只需导入 Query 类型
        use axum::extract::Query;
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(v)| Self(v))
            .map_err(ApiError::from)
    }
}

/// handler 的统一返回类型。
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    //! 🔴 本模块只测**适配器职责**：信封形状、JSON 线上格式、
    //! `ServiceError` → HTTP 的机械转换是否保真。
    //!
    //! "哪个错误该给哪个状态码 / 错误码 / hint" 属于语义映射，
    //! 已在 `spolia-service/src/context.rs` 的测试里穷举覆盖。
    //! 这里再测一遍只会产生两套断言——改了 service 层忘了改这里，
    //! 就会出现"测试红但行为对"或更糟的"测试绿但行为错"。

    use super::*;
    use spolia_service::ServiceError;

    // ── 信封形状 ────────────────────────────────────────────────

    #[test]
    fn envelope_marks_success() {
        let env = Envelope {
            success: true,
            data: Empty::new(),
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["success"], true);
        // Empty 的两个字段都可空，序列化后应为空对象
        assert!(json["data"].as_object().unwrap().is_empty());
    }

    #[test]
    fn empty_carries_optional_fields() {
        let json = serde_json::to_value(Empty::new().affected(3).message("已删除")).unwrap();
        assert_eq!(json["affected"], 3);
        assert_eq!(json["message"], "已删除");
    }

    #[test]
    fn empty_omits_unset_fields() {
        // 🔴 前端契约：affected/message 未设置时不该出现在 JSON 里，
        // 否则前端会把 null 当成"有影响 0 条"而非"未提供该信息"
        let json = serde_json::to_value(Empty::new()).unwrap();
        assert!(json.get("affected").is_none());
        assert!(json.get("message").is_none());
    }

    /// 错误响应体的**线上格式**（前端解析契约）。
    ///
    /// 这条测试直接序列化真实响应，而不是手工拼一个 json! 再断言——
    /// 手工拼的话，`ErrorBody` 的字段改名或加 `skip_serializing_if`
    /// 都不会让测试失败，契约就白测了。
    #[tokio::test]
    async fn error_response_body_shape_is_stable() {
        let api: ApiError = ServiceError::NotFound("项目 p1".into()).into();
        let resp = api.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(body["success"], false);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(body["error"]["message"].as_str().unwrap().contains("p1"));
        // 无 hint 时字段应省略而非输出 null
        assert!(body["error"].get("hint").is_none(), "{body}");
        // success=false 时不该有 data 字段（前端据此判定）
        assert!(body.get("data").is_none());
    }

    #[tokio::test]
    async fn error_response_includes_hint_when_present() {
        let api: ApiError = ServiceError::Precondition("没有可扫描的目录".into()).into();
        let resp = api.into_response();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let hint = body["error"]["hint"].as_str().expect("应带 hint");
        assert!(!hint.is_empty());
        assert!(hint.contains("设置"), "hint 应指明去哪操作: {hint}");
    }

    // ── ServiceError → ApiError 透传保真 ────────────────────────

    /// 🔴 适配器不得凭空改写 code / status / hint。
    ///
    /// 对同一错误分别取 service 侧与适配器侧的值，必须完全一致。
    /// 这是适配器唯一的职责，也是最容易做错的地方：
    /// 一旦在这里重新判断语义（早期版本就是这么写的），
    /// 就会与 service 层漂移，且 Tauri 适配器还要再判第三遍。
    ///
    /// 🔴 断言必须**跨对象比较**（service 侧的期望值 vs 适配器侧的实际值）。
    /// 写成 `assert_eq!(api.code, api.code)` 这种自比较永远为真，
    /// 看起来有测试、实际保护不了任何东西——比没有测试更危险，
    /// 因为它会让人误以为这条契约已被守住。
    #[test]
    fn adapter_does_not_rewrite_error_codes() {
        let cases: Vec<(ServiceError, &str, u16)> = vec![
            (ServiceError::Invalid("x".into()), "bad_request", 400),
            (ServiceError::NotFound("x".into()), "not_found", 404),
            (
                ServiceError::Precondition("x".into()),
                "precondition_failed",
                424,
            ),
            (ServiceError::Conflict("x".into()), "conflict", 409),
            (ServiceError::Internal, "internal_error", 500),
        ];
        for (err, code, status) in cases {
            let expected_code = err.code();
            let expected_hint = err.hint();
            let expected_status = err.status_code();
            assert_eq!(expected_code, code, "service 侧错误码变了: {err}");
            assert_eq!(expected_status, status, "service 侧状态码变了: {err}");

            let api = ApiError::from(err);
            assert_eq!(api.code, expected_code);
            assert_eq!(api.status.as_u16(), expected_status);
            assert_eq!(api.hint, expected_hint, "hint 必须原样透传");
        }
    }

    /// 非标准状态码（499 = 客户端关闭请求）不得 panic。
    #[test]
    fn nonstandard_status_falls_back_safely() {
        let err = ServiceError::Ai(spolia_domain::AiError::Cancelled);
        let expected = err.status_code();
        let api = ApiError::from(err);
        // StatusCode::from_u16 接受 100..=999，499 合法
        assert_eq!(api.status.as_u16(), expected);
        assert!(!api.internal, "用户取消不是服务端故障");
    }

    /// 🔴 只有 5xx 才算内部错误。
    ///
    /// 这条判断直接决定日志可用性：把"用户没配模型"记成 error，
    /// 真正的故障就会被新用户的正常空库刷屏淹没。
    #[test]
    fn only_5xx_is_marked_internal() {
        assert!(!ApiError::from(ServiceError::NotFound("x".into())).internal);
        assert!(!ApiError::from(ServiceError::Invalid("x".into())).internal);
        assert!(
            !ApiError::from(ServiceError::Ai(spolia_domain::AiError::NotConfigured)).internal,
            "未配置模型是用户状态，不是服务端故障"
        );
        assert!(ApiError::from(ServiceError::Internal).internal);
        assert!(
            ApiError::from(ServiceError::Storage(
                spolia_domain::StorageError::Unavailable {
                    path: "C:/data/spolia.db".into(),
                    reason: "磁盘已满".into(),
                }
            ))
            .internal,
            "503 应记日志"
        );
    }

    /// 存储不可用的 hint 要带路径，便于用户自查（路径不是敏感信息）。
    #[test]
    fn storage_unavailable_hint_reaches_the_client() {
        let api = ApiError::from(ServiceError::Storage(
            spolia_domain::StorageError::Unavailable {
                path: "C:/data/spolia.db".into(),
                reason: "拒绝访问".into(),
            },
        ));
        assert_eq!(api.code, "storage_unavailable");
        assert!(api.hint.unwrap().contains("spolia.db"));
    }

    // ── internal 构造器（唯一允许在适配器里造错误的入口）─────────

    /// 🔴 内部错误不得把底层细节透给用户：SQLite 错误可能含数据库路径、
    /// 表结构、甚至用户代码片段。
    #[test]
    fn internal_error_hides_underlying_details() {
        let api = ApiError::internal("查询项目", "database disk image is malformed");
        assert_eq!(api.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(api.internal);
        assert!(
            !api.message.contains("malformed"),
            "不得透传底层错误: {}",
            api.message
        );
        assert!(api.message.contains("日志"), "应指引用户去看日志");
    }

    // ── 提取器拒绝也走信封 ──────────────────────────────────────

    /// 🔴 axum 内建 `Json`/`Query` 拒绝请求时默认返回**纯文本**，
    /// 绕过我们的信封。前端因此得写两套错误解析逻辑——
    /// 这正是信封要消除的问题。
    ///
    /// 这里直接调用提取器拿 rejection 再转换，精确覆盖 `From` impl，
    /// 不经过 Router（否则要满足 Handler 的 Send/'static bound，
    /// 编译错误会掩盖真正想测的东西）。
    /// 端到端的信封形状由 `routes::tests` 覆盖。
    #[tokio::test]
    async fn malformed_json_body_becomes_enveloped_400() {
        use axum::extract::{FromRequest, Request};

        let req = Request::builder()
            .method("POST")
            .uri("/t")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from("{ not json"))
            .unwrap();
        let rejected = Json::<Probe>::from_request(req, &()).await.unwrap_err();

        let api = ApiError::from(rejected);
        assert_eq!(api.status, StatusCode::BAD_REQUEST);
        assert_eq!(api.code, "bad_request");
        // 必须给出可操作提示，而不是甩一句框架内部术语
        let hint = api.hint.as_deref().unwrap_or_default();
        assert!(!hint.is_empty(), "{api}");
        assert!(!api.internal, "客户端输入错误不是服务端故障");
    }

    #[tokio::test]
    async fn wrong_json_field_type_becomes_enveloped_400() {
        use axum::extract::{FromRequest, Request};

        // 结构合法但字段类型错：这是最常见的真实错误
        let req = Request::builder()
            .method("POST")
            .uri("/t")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(r#"{"name": 123, "count": 1}"#))
            .unwrap();
        let rejected = Json::<Probe>::from_request(req, &()).await.unwrap_err();

        let api = ApiError::from(rejected);
        assert_eq!(api.status, StatusCode::BAD_REQUEST);
        assert_eq!(api.code, "bad_request");
        assert!(api.hint.is_some(), "{api}");
    }

    #[tokio::test]
    async fn missing_content_type_is_415() {
        use axum::extract::{FromRequest, Request};

        let req = Request::builder()
            .method("POST")
            .uri("/t")
            // 故意不给 Content-Type
            .body(axum::body::Body::from("{}"))
            .unwrap();
        let rejected = Json::<Probe>::from_request(req, &()).await.unwrap_err();

        let api = ApiError::from(rejected);
        assert_eq!(api.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(api.message.contains("Content-Type"), "{api}");
    }

    #[tokio::test]
    async fn malformed_query_becomes_enveloped_400() {
        use axum::extract::{FromRequest, Query, Request};

        let req = Request::builder()
            .uri("/t?count=abc")
            .body(axum::body::Body::empty())
            .unwrap();
        let rejected = Query::<Probe>::from_request(req, &()).await.unwrap_err();

        let api = ApiError::from(rejected);
        assert_eq!(api.status, StatusCode::BAD_REQUEST);
        assert_eq!(api.code, "bad_request");
        // 提示要具体到"该填数字"，而不只是"参数错了"
        assert!(api.hint.unwrap().contains("数字"));
    }

    // 🔴 必须 derive Debug：`from_request` 返回 `Result<Json<Probe>, _>`，
    // `unwrap_err()` 在断言失败时要打印 Ok 分支的类型，缺 Debug 就编译不过。
    #[derive(Debug, serde::Deserialize)]
    struct Probe {
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        count: u32,
    }

    // ── 展示 ────────────────────────────────────────────────────

    #[test]
    fn display_includes_code() {
        let api = ApiError::from(ServiceError::Conflict("任务冲突".into()));
        let shown = api.to_string();
        assert!(shown.contains("conflict"), "{shown}");
        assert!(shown.contains("任务冲突"), "{shown}");
    }
}
