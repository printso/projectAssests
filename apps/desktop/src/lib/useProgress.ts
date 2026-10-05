/**
 * 任务进度订阅（SSE）。
 *
 * # 🔴 为什么用 SSE 而不是轮询
 * 扫描/索引是分钟级操作，进度要**实时**反馈。轮询有两个问题：
 * 1. 间隔短则浪费请求（大部分响应都是"没变化"），间隔长则进度条卡顿跳变
 * 2. 无法表达"任务已完成"这个瞬间——用户可能在两次轮询之间错过它
 *
 * 后端 `/api/events` 用 SSE 推送，连接建立时**立即推一次当前状态**
 * （`ProgressSubscription::current()`），所以中途连上也能立刻看到真实进度，
 * 而不是从 0% 开始等下一次变化。
 *
 * # 🔴 重连策略
 * EventSource 自带重连，但断线期间的事件会丢失。
 * 所以重连后要**重新拉一次任务列表**（`onReconnect` 回调），
 * 用全量状态纠正可能漏掉的终态——否则界面会永远停在"扫描中 60%"，
 * 而实际任务早已完成或失败。这种"卡住的进度条"是最让人不信任产品的 bug。
 */

import { useEffect, useRef, useState } from "react";
import type { ProgressEvent } from "@/api/types";

export interface ProgressState {
  /** 最近一次进度事件。从未跑过任务时为 null。 */
  event: ProgressEvent | null;
  /** SSE 连接状态 */
  connection: "connecting" | "open" | "closed";
  /** 重连次数（>0 说明连接曾断开，期间可能丢事件） */
  reconnects: number;
}

export const SSE_PATH = "/api/events";

export function useProgress(onReconnect?: () => void): ProgressState {
  const [state, setState] = useState<ProgressState>({
    event: null,
    connection: "connecting",
    reconnects: 0,
  });

  // onReconnect 存 ref：调用方通常传内联箭头函数，
  // 若直接进 deps 会导致每次渲染都重建 SSE 连接（连接抖动）。
  const onReconnectRef = useRef(onReconnect);
  onReconnectRef.current = onReconnect;

  useEffect(() => {
    const es = new EventSource(SSE_PATH);
    let reconnects = 0;

    const onOpen = () => {
      setState((prev) => {
        // 🔴 只有"曾经断开过"才算重连：首次连上不该触发全量刷新
        if (prev.connection === "open") return prev;
        const isReconnect = prev.connection === "closed";
        if (isReconnect) {
          reconnects = prev.reconnects + 1;
          // 断线期间的事件已丢失，用全量状态纠正
          onReconnectRef.current?.();
        }
        return { ...prev, connection: "open", reconnects };
      });
    };

    const onProgress = (e: MessageEvent<string>) => {
      let parsed: ProgressEvent;
      try {
        parsed = JSON.parse(e.data) as ProgressEvent;
      } catch {
        // 🔴 单帧解析失败不该断开整个流：
        // 记警告并继续，否则一帧脏数据会让进度条永久停摆。
        console.warn("[spolia] 进度帧解析失败，已跳过:", e.data);
        return;
      }
      setState((prev) => ({ ...prev, event: parsed }));
    };

    const onError = () => {
      // EventSource 会自动重连，这里只标记状态。
      // 🔴 不要在这里 close()：那会终止自动重连，
      // 用户网络抖动一下进度条就永久死了。
      setState((prev) => ({ ...prev, connection: "closed" }));
    };

    es.addEventListener("open", onOpen);
    es.addEventListener("progress", onProgress as EventListener);
    es.addEventListener("error", onError);

    return () => {
      es.removeEventListener("open", onOpen);
      es.removeEventListener("progress", onProgress as EventListener);
      es.removeEventListener("error", onError);
      es.close();
    };
  }, []);

  return state;
}

/**
 * 从进度事件算出展示文案。
 *
 * 🔴 优先用后端给的 `stage`，不要在前端另写一套阶段文案：
 * 后端知道真实执行到哪一步（"扫描目录"、"解析符号"、"生成洞察"），
 * 前端自己按 progress 区间猜文案，会出现"进度 70% 却显示扫描中"的错位。
 */
export function progressLabel(ev: ProgressEvent | null): string {
  if (!ev) return "";
  if (ev.stage && ev.stage.trim() !== "") return ev.stage;
  switch (ev.status) {
    case "queued":
      return "排队中";
    case "running":
      return "执行中";
    case "completed":
      return "已完成";
    case "failed":
      return "失败";
    case "cancelled":
      return "已取消";
  }
}

/** 计数文案（"127/183"）。后端没给总数时只显示已完成数。 */
export function progressCounter(ev: ProgressEvent | null): string {
  if (!ev || ev.processed === null) return "";
  if (ev.total === null || ev.total === 0) return `${ev.processed}`;
  return `${ev.processed}/${ev.total}`;
}

/** 进度百分比（0-100 整数）。 */
export function progressPercent(ev: ProgressEvent | null): number {
  if (!ev) return 0;
  return Math.round(Math.max(0, Math.min(1, ev.progress)) * 100);
}
