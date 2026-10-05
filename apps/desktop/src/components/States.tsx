/**
 * 加载 / 空 / 错误三态组件。
 *
 * # 🔴 与原型的关键区别
 * 原型里这三种状态是**演示用的假状态**：
 * - 错误文案硬编码"本地索引服务无响应（模拟错误状态）"
 * - 按钮是 `data-demo="default"`，点了什么也不发生
 * - 三态靠手动切换 `state.demo` 触发，与真实请求无关
 *
 * 这里全部由真实数据驱动：错误文案来自后端的 `message` + `hint`，
 * 按钮触发真实动作（重试 / 跳转设置 / 清除筛选）。
 *
 * # 🔴 为什么错误态要区分错误码
 * 后端给的 `code` 决定了"用户能做什么"：
 * - `no_scan_dirs` / `dir_not_found` → 引导去设置页加目录（重试无用）
 * - `llm_not_configured` → 引导去配置模型
 * - `storage_unavailable` → 磁盘/权限问题，重试也无用，要看 hint
 * - 其余 5xx → 重试有意义
 *
 * 一律显示"重试"按钮的话，用户会在一个根本不可能成功的操作上反复点击，
 * 这比没有按钮更令人沮丧。
 */

import type { ReactNode } from "react";
import { ApiError, NetworkError } from "@/api/client";
import { Icon, type IconName } from "./Icon";

// ══════════════════════════════════════════════════════════════════
// 加载态
// ══════════════════════════════════════════════════════════════════

export interface LoadingProps {
  /** 骨架条数（按内容形态给，默认 4） */
  rows?: number;
  /** 无障碍标签 */
  label?: string;
}

/**
 * 骨架屏加载态。
 *
 * 🔴 用骨架而非居中 spinner：骨架保留了内容的位置与形状，
 * 用户能预判"这里会出现什么"，视觉跳动更小。
 * 满屏 spinner 会让人以为整个应用卡住了。
 */
export function Loading({ rows = 4, label = "加载中" }: LoadingProps) {
  // 高度刻意不等：等高骨架看起来像一堆灰条，不等高更接近真实内容节奏
  const heights = [70, 90, 60, 80, 70, 90];
  return (
    <div
      role="status"
      aria-live="polite"
      aria-label={label}
      style={{ display: "grid", gap: 12, padding: "8px 0" }}
    >
      {Array.from({ length: rows }, (_, i) => (
        <div
          key={i}
          className="skeleton"
          style={{ height: heights[i % heights.length] }}
        />
      ))}
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// 空态
// ══════════════════════════════════════════════════════════════════

export interface EmptyStateProps {
  title?: string;
  /**
   * 说明文案。
   * 🔴 优先用**后端给的** `empty_hint`：后端知道"为什么空"
   * （库是空的 vs 筛选太严），前端自己写只能猜。
   */
  message?: ReactNode;
  icon?: IconName;
  /** 主动作（例如"清除筛选"、"去扫描"）。
   *  🔴 `disabled` 用于"生成画像"这类长耗时动作：生成中必须禁用按钮，
   *  否则用户重复点击会重复触发 LLM 调用（数秒级、可能计费）。 */
  action?: { label: string; onClick: () => void; icon?: IconName; disabled?: boolean };
  /** 次动作 */
  secondaryAction?: { label: string; onClick: () => void };
}

export function EmptyState({
  title = "暂无数据",
  message,
  icon = "search",
  action,
  secondaryAction,
}: EmptyStateProps) {
  return (
    <div className="state-box">
      <div className="big">
        <Icon name={icon} />
      </div>
      <div className="t">{title}</div>
      {message ? <div>{message}</div> : null}
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", justifyContent: "center" }}>
        {action ? (
          <button
            className="btn btn--primary btn--sm"
            onClick={action.onClick}
            disabled={action.disabled}
            aria-busy={action.disabled || undefined}
          >
            {action.disabled ? (
              <Icon name="refresh" />
            ) : action.icon ? (
              <Icon name={action.icon} />
            ) : null}
            {action.label}
          </button>
        ) : null}
        {secondaryAction ? (
          <button className="btn btn--ghost btn--sm" onClick={secondaryAction.onClick}>
            {secondaryAction.label}
          </button>
        ) : null}
      </div>
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// 错误态
// ══════════════════════════════════════════════════════════════════

export interface ErrorStateProps {
  error: unknown;
  /** 重试动作。省略则不显示重试按钮（用于重试无意义的错误）。 */
  onRetry?: () => void;
  /** 额外动作（例如"去设置页配置模型"） */
  action?: { label: string; onClick: () => void; icon?: IconName };
}

/**
 * 据错误码判断"重试是否有意义"。
 *
 * 🔴 用户侧的前置条件问题（没扫描、没配模型、目录不存在）重试必然还是同样结果，
 * 显示重试按钮等于诱导用户做无用功。这类错误应该给"去配置"的引导。
 */
function retryIsMeaningful(error: unknown): boolean {
  if (error instanceof NetworkError) return true; // 服务可能刚启动，重试有意义
  if (!(error instanceof ApiError)) return true;
  if (error.isServerFault) return true;
  const noRetry: ReadonlySet<string> = new Set([
    "no_scan_dirs",
    "dir_not_found",
    "llm_not_configured",
    "sensitive_blocked",
    "precondition_failed",
    "bad_request",
    "not_found",
  ]);
  return !noRetry.has(error.code);
}

/** 据错误码给出主动作建议。 */
function suggestedAction(error: unknown): { label: string; page: string } | null {
  if (!(error instanceof ApiError)) return null;
  switch (error.code) {
    case "no_scan_dirs":
    case "dir_not_found":
    case "not_authorized":
      return { label: "去设置扫描目录", page: "/settings" };
    case "llm_not_configured":
    case "llm_unauthorized":
      return { label: "去配置大模型", page: "/settings" };
    case "index_not_ready":
      return { label: "去执行扫描", page: "/" };
    default:
      return null;
  }
}

export function ErrorState({ error, onRetry, action }: ErrorStateProps) {
  const message =
    error instanceof ApiError || error instanceof NetworkError
      ? error.message
      : error instanceof Error
        ? error.message
        : "发生未知错误";

  const hint = error instanceof ApiError ? error.hint : undefined;
  const code = error instanceof ApiError ? error.code : undefined;
  const showRetry = onRetry !== undefined && retryIsMeaningful(error);

  return (
    <div className="state-box">
      <div className="big" style={{ color: "var(--color-danger)" }}>
        <Icon name="alert" />
      </div>
      <div className="t">加载失败</div>
      <div style={{ maxWidth: 460 }}>{message}</div>

      {/* 🔴 hint 是后端给的可操作引导，必须显示——
          它比任何前端自己编的文案都更准确（后端知道具体缺什么） */}
      {hint ? (
        <div
          style={{
            maxWidth: 460,
            padding: "8px 12px",
            background: "var(--color-accent-box)",
            border: "1px solid var(--color-accent-box-border)",
            borderRadius: "var(--r-md)",
            color: "var(--color-accent-text)",
            fontSize: "var(--fs-sm)",
          }}
        >
          {hint}
        </div>
      ) : null}

      {/* 错误码便于用户报障时提供精确信息（不是给用户读的技术细节） */}
      {code ? (
        <div style={{ fontSize: "var(--fs-xs)", color: "var(--color-text-3)" }}>
          错误码 <code>{code}</code>
        </div>
      ) : null}

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", justifyContent: "center" }}>
        {action ? (
          <button className="btn btn--primary btn--sm" onClick={action.onClick}>
            {action.icon ? <Icon name={action.icon} /> : null}
            {action.label}
          </button>
        ) : null}
        {showRetry ? (
          <button
            className={`btn ${action ? "btn--ghost" : "btn--primary"} btn--sm`}
            onClick={onRetry}
          >
            <Icon name="refresh" />
            重试
          </button>
        ) : null}
      </div>
    </div>
  );
}

/**
 * 错误态包装器：把 `suggestedAction` 自动接到路由跳转上。
 *
 * 单独抽出来是因为多数页面的错误处理完全一样，
 * 每页自己写一遍 `useNavigate` + 判断逻辑就是重复。
 */
export function ErrorStateWithNav({
  error,
  onRetry,
  navigate,
}: {
  error: unknown;
  onRetry?: () => void;
  navigate: (path: string) => void;
}) {
  const suggested = suggestedAction(error);
  return (
    <ErrorState
      error={error}
      onRetry={onRetry}
      action={
        suggested
          ? { label: suggested.label, onClick: () => navigate(suggested.page), icon: "gear" }
          : undefined
      }
    />
  );
}
