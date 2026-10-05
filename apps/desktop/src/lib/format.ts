/**
 * 日期/时间格式化。
 *
 * # 🔴 为什么需要它：后端给的是原始 ISO 纳秒时间戳
 * `last_scanned_at`、`created_at`、`updated_at` 等字段形如
 * `2026-10-02T12:07:23.126184200+00:00` —— 9 位小数秒。
 *
 * 两个问题：
 * 1. **直接显示是 UX 缺陷**：用户不需要看到纳秒与 `+00:00` 偏移。
 *    首版截图里首页 hero 与扫描条各顶着一串这样的字符串，非常刺眼。
 * 2. **`Date.parse` 对 9 位小数不可靠**：ECMAScript 的 ISO 格式只规定 3 位（毫秒），
 *    更多位属于"实现相关"，某些引擎会返回 `Invalid Date`。
 *    所以解析前必须先把小数截到 3 位。
 *
 * # 后端已有人性化字段时优先用它们
 * `created_relative` / `updated_display` / `relative` / `when` 都是后端算好的，
 * 口径统一（相对时间的"现在"由后端定）。本模块只用于**后端没给**人性化版本的字段
 * （`last_scanned_at`、`created_at`、`updated_at`），不在前端另造一套相对时间口径。
 */

/** 把小数秒截到毫秒（3 位），让 `Date.parse` 在所有引擎上行为一致。 */
function normalizeIso(iso: string): string {
  return iso.replace(/(\.\d{3})\d+/, "$1");
}

function parse(iso: string): Date | null {
  const d = new Date(normalizeIso(iso));
  return Number.isNaN(d.getTime()) ? null : d;
}

const pad = (n: number) => String(n).padStart(2, "0");

/**
 * 绝对时间，本地时区：`2026-10-02 20:07`。
 *
 * 🔴 解析失败时**回退显示原始串**，而不是 "Invalid Date" 或空串：
 * 丢失信息比格式难看更糟，而且原始串至少能让用户/开发者看出数据本身有问题。
 */
export function formatDateTime(iso: string): string {
  const d = parse(iso);
  if (d === null) return iso;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 仅日期：`2026-10-02`。 */
export function formatDate(iso: string): string {
  const d = parse(iso);
  if (d === null) return iso;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/**
 * 相对时间：`刚刚 / N 分钟前 / N 小时前 / N 天前 / 日期`。
 *
 * 超过 30 天退回绝对日期：相对时间在大跨度下没有信息量
 * （"342 天前"不如"2025-10-25"直观）。
 */
export function timeAgo(iso: string, now: Date = new Date()): string {
  const d = parse(iso);
  if (d === null) return iso;
  const ms = now.getTime() - d.getTime();
  if (ms < 0) return formatDateTime(iso); // 未来时间不假装是"之前"
  const sec = Math.floor(ms / 1000);
  if (sec < 45) return "刚刚";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min} 分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} 小时前`;
  const day = Math.floor(hr / 24);
  if (day <= 30) return `${day} 天前`;
  return formatDate(iso);
}

/**
 * 组合展示：`32 分钟前（2026-10-02 20:07）`。
 * 用于"上次扫描"这类既想知道多久前、又想核对具体时刻的场合。
 */
export function formatRelativeWithAbsolute(iso: string): string {
  return `${timeAgo(iso)}（${formatDateTime(iso)}）`;
}
