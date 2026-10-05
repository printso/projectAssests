/**
 * 任务进度上下文（SSE）。
 *
 * # 🔴 为什么单独一个 Provider，而不是放进 AppContext
 * SSE 每收到一帧就产生一次状态变化。扫描大目录时一秒可能推好几帧，
 * 一次完整扫描累计几百帧。
 *
 * 若把 `progress` 放进 AppContext，那么**所有** useApp 消费者
 * （侧栏、首页、每个页面）都会跟着重渲染几百次——
 * 而其中绝大多数根本不关心进度。
 *
 * 拆成独立 Provider 后，重渲染范围收窄到真正读进度的组件
 * （侧栏进度卡 + 首页扫描条）。这是"高频更新的状态要隔离"的典型场景。
 *
 * # 🔴 全应用只订阅一条 SSE
 * Provider 挂在应用根部，`useProgressState` 只是读 context。
 * 若各页面各自调 `useProgress()`，就有 N 条 EventSource 连接，
 * 后端要为每条连接各广播一份同样的事件。
 */

import { createContext, useContext, type ReactNode } from "react";
import { useApp } from "./AppContext";
import { useProgress, type ProgressState } from "./useProgress";

const ProgressContext = createContext<ProgressState | null>(null);

export function ProgressProvider({ children }: { children: ReactNode }) {
  const { bumpHealth } = useApp();
  // 🔴 断线重连时刷新统计：断线期间的事件已丢失，
  // 用后端的全量状态纠正，否则界面可能永远停在"扫描中 60%"。
  const state = useProgress(bumpHealth);
  return <ProgressContext.Provider value={state}>{children}</ProgressContext.Provider>;
}

/**
 * 读任务进度。
 *
 * 🔴 Provider 外调用直接抛错（理由同 useApp / useToast）：
 * 静默返回空状态会表现为"进度条永远不动"，而没有任何报错指向根因。
 */
export function useProgressState(): ProgressState {
  const ctx = useContext(ProgressContext);
  if (ctx === null) {
    throw new Error("useProgressState 必须在 <ProgressProvider> 内使用");
  }
  return ctx;
}
