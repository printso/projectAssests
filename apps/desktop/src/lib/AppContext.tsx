/**
 * 应用级上下文：服务状态 + 全局刷新信号。
 *
 * # 🔴 为什么是 Provider 而不是各自 useServiceStatus
 * `/api/health` 的响应里含各表真实行数（`DbStats`），侧栏计数、首页统计、
 * 服务状态提示都要用它。若每处各调一次 `useServiceStatus`，
 * 就会对同一个端点发起 N 个并行请求——既浪费，也可能因为响应时序不同
 * 导致侧栏显示的数字与首页不一致（两处 health 不是同一时刻的快照）。
 *
 * 用单一 Provider 拉一次、广播给所有消费者，保证**全应用看到的是同一份统计**。
 *
 * # 🔴 数据刷新信号（refreshKey）
 * 写操作（扫描完成、标记反馈、改设置）会让统计数据过时。
 * 各页面直接改 Provider 状态不现实（它们不持有 health），
 * 所以这里提供一个 `bumpHealth()`：任务完成或写操作后调用，
 * 触发 health 重新拉取，侧栏与首页的数字随之更新。
 *
 * 这比"每个页面自己重新拉 health"更可靠：
 * 后者容易出现"A 页面刷新了、B 页面还是旧数字"的分裂。
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
import { getHealth } from "@/api/endpoints";
import type { DbStats, HealthView } from "@/api/types";

export type ServiceStatus = "checking" | "online" | "offline";

export interface AppContextValue {
  status: ServiceStatus;
  health: HealthView | null;
  /** 各表真实行数；服务离线或尚未加载时为 null */
  stats: DbStats | null;
  reason: string | null;
  /** 重新探测服务 + 刷新统计 */
  recheck: () => void;
  /**
   * 请求刷新统计（写操作后调用）。
   * 🔴 与 `recheck` 的区别：`recheck` 用于"服务可能刚起来"的探测，
   * `bumpHealth` 用于"服务在线但数据变了"的刷新。语义分开，调用方不必猜。
   */
  bumpHealth: () => void;
}

const AppContext = createContext<AppContextValue | null>(null);

/** 服务离线时的轮询间隔（毫秒）。上线后停止轮询。 */
const OFFLINE_POLL_MS = 3000;

export function AppProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<ServiceStatus>("checking");
  const [health, setHealth] = useState<HealthView | null>(null);
  const [reason, setReason] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const recheck = useCallback(() => setNonce((n) => n + 1), []);
  const bumpHealth = useCallback(() => setNonce((n) => n + 1), []);

  useEffect(() => {
    let cancelled = false;
    const controller = new AbortController();

    getHealth(controller.signal)
      .then((h) => {
        if (cancelled) return;
        setHealth(h);
        setStatus("online");
        setReason(null);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setStatus("offline");
        setReason(err instanceof Error ? err.message : "无法连接到本地服务");
        // 🔴 离线时持续轮询：用户启动服务后前端要能自动恢复，不必手动刷新。
        // 这个细节决定了"第一次跑起来"的体验——用户往往先开前端、再启动后端。
        timer.current = setTimeout(() => setNonce((n) => n + 1), OFFLINE_POLL_MS);
      });

    return () => {
      cancelled = true;
      controller.abort();
      if (timer.current !== null) {
        clearTimeout(timer.current);
        timer.current = null;
      }
    };
  }, [nonce]);

  const value = useMemo<AppContextValue>(
    () => ({
      status,
      health,
      stats: health?.stats ?? null,
      reason,
      recheck,
      bumpHealth,
    }),
    [status, health, reason, recheck, bumpHealth],
  );

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

/**
 * 取应用上下文。
 *
 * 🔴 Provider 外调用直接抛错：静默返回默认值会让"忘了包 Provider"
 * 表现为"侧栏计数永远是空"，而这种静默失效极难排查。
 */
export function useApp(): AppContextValue {
  const ctx = useContext(AppContext);
  if (ctx === null) {
    throw new Error("useApp 必须在 <AppProvider> 内使用");
  }
  return ctx;
}

/**
 * 侧栏计数：从全局 health 统计派生。
 *
 * 🔴 数据全部来自后端 `DbStats` 的真实 `SELECT count(*)`，
 * 不是原型的 `MOCK.scale`（projects:128 / assets:1284 / capabilities:47 硬编码）。
 * 服务离线或尚未加载时返回 null，调用方据此**不显示**计数——
 * 宁可空着，也不能显示编造或过期的数字。
 */
export function useNavCounts(): {
  projects: number | null;
  assets: number | null;
  insights: number | null;
  opportunities: number | null;
} {
  const { stats, status } = useApp();
  return useMemo(() => {
    if (status !== "online" || stats === null) {
      return { projects: null, assets: null, insights: null, opportunities: null };
    }
    return {
      projects: stats.projects,
      assets: stats.assets,
      insights: stats.insights,
      opportunities: stats.opportunities,
    };
  }, [stats, status]);
}
