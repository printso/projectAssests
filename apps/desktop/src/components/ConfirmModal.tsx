/**
 * 通用确认弹窗（base.css `.modal-mask` / `.modal`）。
 *
 * 🔴 破坏性操作（批量忽略机会、清理派生数据、移除项目）必须走它，不能一键执行。
 * 抽到 components 层是因为多个页面都要用——页面之间不该互相 import 组件，
 * 否则 Opportunities 就成了 Settings 的依赖，模块边界被打穿。
 *
 * # 🔴 初始焦点落在「取消」上
 * 弹窗出现时用户的手指往往已经放在回车键上（刚从上一个操作过来），
 * 若焦点在确认按钮，一次习惯性回车就会执行不可撤销的操作。
 *
 * # 🔴 点击遮罩 = 取消
 * 误点空白处不该触发破坏性操作。只有明确点「确认」才执行。
 */

import { useEffect, useRef, type ReactNode } from "react";

export interface ConfirmModalProps {
  title: string;
  /** 说明文案。应讲清后果与可逆性，而不只是重复标题。 */
  body: ReactNode;
  confirmLabel: string;
  cancelLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
  /** 确认中：禁用两个按钮，避免重复提交 */
  busy?: boolean;
}

export function ConfirmModal({
  title,
  body,
  confirmLabel,
  cancelLabel = "取消",
  onConfirm,
  onCancel,
  busy = false,
}: ConfirmModalProps) {
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    cancelRef.current?.focus();
  }, []);

  // Esc 关闭。挂在 window 上而非遮罩 div：
  // 遮罩 div 不在 tab 序列里，不主动点击就收不到 keydown。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel, busy]);

  return (
    <div
      className="modal-mask is-open"
      role="dialog"
      aria-modal="true"
      aria-label={title}
      onClick={busy ? undefined : onCancel}
    >
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h3>{title}</h3>
        <p>{body}</p>
        <div className="actions">
          <button
            className="btn btn--ghost btn--sm"
            onClick={onCancel}
            type="button"
            ref={cancelRef}
            disabled={busy}
          >
            {cancelLabel}
          </button>
          {/* 🔴 破坏性动作用危险色，视觉上先给一次警示 */}
          <button
            className="btn btn--primary btn--sm"
            style={{ background: "var(--color-danger)", borderColor: "var(--color-danger)" }}
            onClick={onConfirm}
            type="button"
            disabled={busy}
          >
            {busy ? "处理中…" : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
