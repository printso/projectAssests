/**
 * API 客户端：Spolia 前端与本地 Rust 服务之间的**唯一**数据通道。
 *
 * # 🔴 为什么所有请求都必须经过这里
 * 1. **信封解包只写一次**。后端成功返回 `{success, data}`、失败返回
 *    `{success, error:{code, message, hint?}}`。若各页面自己 `fetch` 再自己解析，
 *    必然出现某处忘了判 `success` 就把 `error` 当数据渲染的 bug。
 * 2. **错误码是唯一分支依据**。`ApiError.code` 稳定，`message` 是给人看的措辞。
 *    页面据 `code` 决定"显示设置入口"还是"显示重试按钮"。
 * 3. **零静态数据**。这里没有任何兜底假数据：请求失败就是失败，
 *    由调用方决定显示错误态还是空态。绝不返回"看起来正常"的编造内容。
 */

import type { Envelope, ErrorBody, ErrorCode } from "./types";

/**
 * 后端返回的业务错误。
 *
 * 与 JS 原生 `Error` 区分开：`ApiError` 携带后端给的 `code` 与 `hint`，
 * 页面可以据此渲染可操作的引导，而不是笼统一句"出错了"。
 */
export class ApiError extends Error {
  /** 稳定错误码，前端分支的唯一依据 */
  readonly code: ErrorCode;
  /** 后端给出的可操作引导（可能为空） */
  readonly hint?: string;
  readonly status: number;

  constructor(code: ErrorCode, message: string, status: number, hint?: string) {
    super(message);
    this.name = "ApiError";
    this.code = code;
    this.status = status;
    if (hint !== undefined && hint !== "") {
      this.hint = hint;
    }
  }

  /** 是否为服务端故障（5xx）。用户侧问题（没配模型、参数错）不算。 */
  get isServerFault(): boolean {
    return this.status >= 500;
  }
}

/**
 * 网络层错误（后端没启动、连接被拒、DNS 失败）。
 *
 * 🔴 必须与 `ApiError` 区分：
 * - `ApiError` 说明**后端答复了**，只是业务上失败 → 显示后端给的 hint
 * - `NetworkError` 说明**根本没连上** → 引导用户检查服务是否启动
 *
 * 混为一谈的话，用户后端没启动时会看到一堆莫名其妙的业务错误提示。
 */
export class NetworkError extends Error {
  constructor(message: string, readonly cause?: unknown) {
    super(message);
    this.name = "NetworkError";
  }
}

/**
 * 拼查询串。
 *
 * 🔴 跳过 `undefined` / `null` / 空串：
 * 后端把"参数不存在"与"参数为空串"视为不同语义
 * （例如 `project_id=` 空串在索引端点意味着"全量索引"）。
 * 前端未设置的筛选项必须**整个不发送**，否则会把用户的意图改写成另一种操作。
 *
 * # 为什么参数类型是 `object` 而不是 `Record<string, unknown>`
 * `Record<string, unknown>` 要求实参带**索引签名**，而 `interface` 声明的
 * 参数类型（`ProjectListParams` 等）没有索引签名，会报 TS2345
 * （"Index signature for type 'string' is missing"）。
 * 改成 `object` 后既接受 interface 也接受字面量，
 * 同时仍然拒绝 `string`/`number` 这类误传。
 */
export function toQuery(params: object): string {
  const sp = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === null) continue;
    if (typeof value === "string" && value.trim() === "") continue;
    if (Array.isArray(value)) {
      if (value.length === 0) continue;
      // 数组参数用逗号连接，与后端 `types=a,b,c` 的解析约定一致
      sp.set(key, value.join(","));
      continue;
    }
    if (typeof value === "boolean") {
      sp.set(key, value ? "true" : "false");
      continue;
    }
    sp.set(key, String(value));
  }
  const s = sp.toString();
  return s ? `?${s}` : "";
}

interface RequestOptions {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  /** JSON 请求体 */
  body?: unknown;
  /** 中断信号（页面卸载时取消未完成的请求，避免卸载后 setState） */
  signal?: AbortSignal;
}

/**
 * 发请求并解包信封。
 *
 * 泛型 `T` 是 `data` 字段的类型——调用方声明它期望的形状，
 * TypeScript 据此检查后续字段访问。
 */
async function request<T>(path: string, opts: RequestOptions = {}): Promise<T> {
  const { method = "GET", body, signal } = opts;

  const headers: Record<string, string> = {};
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
  }

  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers,
      // 🔴 只在有 body 时传：`JSON.stringify(undefined)` 会得到字符串 "undefined"，
      // 后端解析它会报格式错误，而根因其实在前端。
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
      ...(signal ? { signal } : {}),
    });
  } catch (err) {
    // fetch 只在网络层失败时抛（连接被拒、跨域、断网）
    if (err instanceof DOMException && err.name === "AbortError") {
      throw err; // 主动取消，原样上抛让调用方静默处理
    }
    throw new NetworkError(
      "无法连接到 Spolia 本地服务。请确认服务已启动（默认 127.0.0.1:8787）。",
      err,
    );
  }

  // ── 解析响应体 ────────────────────────────────────────────
  // 🔴 先取文本再手动 JSON.parse：
  // 若后端返回了非 JSON（例如反向代理的 HTML 错误页），
  // `res.json()` 抛出的 SyntaxError 会掩盖真实的 HTTP 状态码，
  // 用户只看到"解析失败"而不知道其实是 502。
  const text = await res.text();
  let parsed: unknown = null;
  if (text !== "") {
    try {
      parsed = JSON.parse(text);
    } catch {
      throw new ApiError(
        "internal_error",
        `服务返回了非 JSON 响应（HTTP ${res.status}）。可能是代理或版本不匹配。`,
        res.status,
      );
    }
  }

  // ── 错误信封 ──────────────────────────────────────────────
  if (!res.ok || isErrorBody(parsed)) {
    if (isErrorBody(parsed)) {
      const e = parsed.error;
      throw new ApiError(e.code, e.message, res.status, e.hint);
    }
    // HTTP 失败但响应体不是我们的信封（少见：框架层拦截）
    throw new ApiError("internal_error", `请求失败（HTTP ${res.status}）`, res.status);
  }

  // ── 成功信封 ──────────────────────────────────────────────
  if (!isEnvelope(parsed)) {
    throw new ApiError(
      "internal_error",
      "服务响应缺少 data 字段，前后端契约可能不一致。",
      res.status,
    );
  }
  return parsed.data as T;
}

/** 运行时判别成功信封。 */
function isEnvelope(v: unknown): v is Envelope<unknown> {
  return (
    typeof v === "object" &&
    v !== null &&
    "success" in v &&
    (v as { success: unknown }).success === true &&
    "data" in v
  );
}

/** 运行时判别错误信封。 */
function isErrorBody(v: unknown): v is ErrorBody {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  if (o.success !== false) return false;
  const e = o.error;
  return typeof e === "object" && e !== null && typeof (e as { code?: unknown }).code === "string";
}

const get = <T>(path: string, signal?: AbortSignal) =>
  request<T>(path, { method: "GET", ...(signal ? { signal } : {}) });

const post = <T>(path: string, body?: unknown, signal?: AbortSignal) =>
  request<T>(path, { method: "POST", ...(body !== undefined ? { body } : {}), ...(signal ? { signal } : {}) });

const put = <T>(path: string, body?: unknown) =>
  request<T>(path, { method: "PUT", ...(body !== undefined ? { body } : {}) });

const del = <T>(path: string, body?: unknown) =>
  request<T>(path, { method: "DELETE", ...(body !== undefined ? { body } : {}) });

export const http = { get, post, put, del, request, toQuery };
