/**
 * Toast 反馈系统。
 *
 * # 🔴 为什么要有它：每个写操作都必须有明确反馈
 * 扫描、索引、反馈标记、保存设置、清理数据——这些操作提交后界面往往**看不出变化**
 * （任务在后台跑、数据几秒后才出现）。没有反馈的话用户会：
 * - 以为没点上而反复点击（触发重复任务）
 * - 以为卡死了而刷新页面（丢掉进行中的操作）
 *
 * 所以规则是：**任何写操作成功后都要 toast**，且文案要说清"发生了什么、接下来会怎样"。
 * 例如扫描要显示后端给的 message（"已开始扫描 1 个目录，完成后将自动索引并生成洞察"），
 * 这句预告是用户理解"为什么几分钟后洞察自己冒出来"的唯一线索。
 *
 * # 🔴 与原型 `toast()` 的区别：队列而非单条覆盖
 * 原型是全局函数 + 一个 `toastTimer`，后一条会顶掉前一条。
 * 连续操作（例如快速标记三条洞察）时用户只看到最后一条，
 * 前两次操作是否成功完全无从得知——而"我以为没成功"正是重复点击的根源。
 * 这里维护队列，多条可同时显示。
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Icon, type IconName } from "./Icon";

export type ToastKind = "success" | "error" | "info" | "warning";

export interface ToastItem {
  id: number;
  kind: ToastKind;
  message: string;
  /** 可选的补充说明（第二行，弱化显示） */
  detail?: string;
  /** 自动关闭毫秒数；0 = 不自动关闭（错误类默认如此，用户需要时间读完） */
  duration: number;
}

export interface ToastInput {
  kind?: ToastKind;
  message: string;
  detail?: string;
  duration?: number;
}

export interface ToastApi {
  push: (t: ToastInput) => number;
  success: (message: string, detail?: string) => number;
  error: (message: string, detail?: string) => number;
  info: (message: string, detail?: string) => number;
  warning: (message: string, detail?: string) => number;
  dismiss: (id: number) => void;
}

const ToastContext = createContext<ToastApi | null>(null);

/** 默认停留时长（毫秒）。 */
const DEFAULT_DURATION: Record<ToastKind, number> = {
  success: 3200,
  info: 3200,
  warning: 5000,
  // 🔴 错误不自动消失：用户需要读完 message + hint 才知道下一步做什么。
  // 3 秒后消失的错误提示等于没提示——用户只看到"闪了一下红色"。
  error: 0,
};

const ICON_OF: Record<ToastKind, IconName> = {
  success: "check",
  error: "alert",
  info: "bell",
  warning: "alert",
};

/** 同时显示的最大条数（超出则丢弃最旧的一条）。 */
const MAX_VISIBLE = 4;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const nextId = useRef(1);
  const timers = useRef(new Map<number, ReturnType<typeof setTimeout>>());

  const dismiss = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
    const timer = timers.current.get(id);
    if (timer !== undefined) {
      clearTimeout(timer);
      timers.current.delete(id);
    }
  }, []);

  const push = useCallback(
    (input: ToastInput): number => {
      const kind = input.kind ?? "info";
      const duration = input.duration ?? DEFAULT_DURATION[kind];
      const id = nextId.current++;

      setToasts((prev) => {
        const next = [...prev, { id, kind, message: input.message, detail: input.detail, duration }];
        // 超出上限时丢最旧的：保持界面清爽，同时保证新反馈一定可见
        return next.length > MAX_VISIBLE ? next.slice(next.length - MAX_VISIBLE) : next;
      });

      if (duration > 0) {
        const timer = setTimeout(() => dismiss(id), duration);
        timers.current.set(id, timer);
      }
      return id;
    },
    [dismiss],
  );

  const api = useMemo<ToastApi>(
    () => ({
      push,
      dismiss,
      success: (message, detail) => push({ kind: "success", message, detail }),
      error: (message, detail) => push({ kind: "error", message, detail }),
      info: (message, detail) => push({ kind: "info", message, detail }),
      warning: (message, detail) => push({ kind: "warning", message, detail }),
    }),
    [push, dismiss],
  );

  return (
    <ToastContext.Provider value={api}>
      {children}
      <ToastViewport toasts={toasts} onDismiss={dismiss} />
    </ToastContext.Provider>
  );
}

/**
 * 取 toast API。
 *
 * 🔴 在 Provider 外调用直接抛错而不是静默返回 no-op：
 * 静默失败的话，某个组件忘了包 Provider，所有操作反馈都会消失，
 * 而这种"没有任何报错的静默失效"极难排查。
 */
export function useToast(): ToastApi {
  const ctx = useContext(ToastContext);
  if (ctx === null) {
    throw new Error("useToast 必须在 <ToastProvider> 内使用");
  }
  return ctx;
}

function ToastViewport({
  toasts,
  onDismiss,
}: {
  toasts: ToastItem[];
  onDismiss: (id: number) => void;
}) {
  return (
    // 🔴 role="region" + aria-live="polite"：屏幕阅读器会朗读新提示。
    // 错误类用 assertive（立即打断），成功类用 polite（等当前朗读完）——
    // 全部 assertive 会让连续操作时读屏用户被打断到无法使用。
    <div
      className="toast-stack"
      role="region"
      aria-label="操作反馈"
      style={{
        position: "fixed",
        left: "50%",
        bottom: 28,
        transform: "translateX(-50%)",
        display: "flex",
        flexDirection: "column-reverse",
        gap: 8,
        zIndex: 99,
        pointerEvents: "none",
        maxWidth: "min(520px, 92vw)",
      }}
    >
      {toasts.map((t) => (
        <ToastRow key={t.id} toast={t} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

function ToastRow({ toast, onDismiss }: { toast: ToastItem; onDismiss: (id: number) => void }) {
  // 进入动画：挂载后下一帧加 is-show，让 CSS transition 生效
  const [shown, setShown] = useState(false);

  // 🔴 必须用 useEffect 而非 useMemo：
  // useMemo 的返回值会被当成"计算结果"，返回的清理函数**永远不会被调用**，
  // 定时器也就永远不会清除（组件卸载后仍会 setState，触发 React 警告）。
  useEffect(() => {
    // setTimeout(0) 而非直接 setState：同一次渲染里设 true 不触发过渡
    // （浏览器把两次样式计算合并了，看不到中间状态）
    const id = setTimeout(() => setShown(true), 0);
    return () => clearTimeout(id);
  }, []);

  const kindClass = toast.kind === "error" ? "ico-err" : toast.kind === "success" ? "ico-ok" : "";
  const live = toast.kind === "error" ? "assertive" : "polite";

  return (
    <div className={`toast toast--stacked ${shown ? "is-show" : ""}`} role="status" aria-live={live}>
      <span className={kindClass} style={{ display: "flex" }}>
        <Icon name={ICON_OF[toast.kind]} />
      </span>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ wordBreak: "break-word" }}>{toast.message}</div>
        {toast.detail ? (
          <div style={{ fontSize: "var(--fs-xs)", color: "var(--color-text-3)", marginTop: 2 }}>
            {toast.detail}
          </div>
        ) : null}
      </div>
      {/* 🔴 必须可手动关闭：不自动消失的错误提示若关不掉，会一直挡住底部内容 */}
      <button
        type="button"
        onClick={() => onDismiss(toast.id)}
        aria-label="关闭提示"
        style={{ display: "flex", color: "var(--color-text-3)", padding: 2 }}
      >
        <Icon name="x" />
      </button>
    </div>
  );
}
