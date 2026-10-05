/**
 * 异步数据获取 hook。
 *
 * # 🔴 为什么需要它，而不是各页面自己 `useEffect` + `fetch`
 * 手写的话每个页面都要重复处理：竞态、卸载后 setState、错误对象、重新加载。
 * 其中**竞态**最容易出错且最难复现：
 * 用户快速切换筛选条件时，先发的请求可能后返回，
 * 于是界面显示的是旧筛选条件的结果——而 URL/状态栏显示的是新条件。
 * 用户看到的数据与自己的操作对不上，且刷新后又正常了，几乎无法报告。
 *
 * 这里用 `AbortController` + 序号双重防护：
 * - abort 让旧请求真正取消（省带宽，也让后端能提前结束）
 * - 序号判断保证即使 abort 没生效，旧响应也不会覆盖新状态
 *
 * # 返回值语义
 * - `data`：最近一次成功的数据。**切换筛选时保留旧值**（配合 `loading` 做半透明过渡），
 *   而不是清空——清空会让界面闪一下空白，看起来像崩了。
 * - `error`：`ApiError` / `NetworkError` / 其他异常。页面据 `code` 决定引导方式。
 * - `loading`：请求进行中（首次加载与重新加载都为 true）。
 * - `reload`：手动重新拉取（提交操作后刷新列表）。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, NetworkError } from "@/api/client";

export interface AsyncState<T> {
  data: T | null;
  error: ApiError | NetworkError | Error | null;
  loading: boolean;
  /** 是否已经成功加载过一次（用于区分"首次加载"与"刷新中"） */
  loaded: boolean;
  reload: () => void;
  /**
   * 就地改写当前数据（不重新请求）。
   *
   * 🔴 写操作后用它做局部更新，而不是 `reload()` 重拉整个列表：
   * 资产/项目列表可能有上千条，重拉的延迟肉眼可见，
   * 而且滚动位置与选中状态都会丢。
   * 后端多数写端点会**返回更新后的条目**，正好用它替换列表里的对应项。
   *
   * 传 `null` 表示清空（例如筛选变化后想强制重新加载）。
   */
  mutate: (updater: (prev: T) => T) => void;
}

/**
 * 获取数据。
 *
 * @param fetcher 返回 Promise 的函数。**必须用 useCallback 包裹或写成稳定引用**，
 *                否则每次渲染都会触发重新请求（无限循环）。
 *                推荐用 `deps` 参数表达依赖，fetcher 内部读它们。
 * @param deps    依赖数组，变化时重新请求。语义与 `useEffect` 的 deps 一致。
 */
export function useAsync<T>(fetcher: (signal: AbortSignal) => Promise<T>, deps: unknown[]): AsyncState<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<ApiError | NetworkError | Error | null>(null);
  const [loading, setLoading] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [nonce, setNonce] = useState(0);

  // fetcher 存进 ref：它每次渲染都是新函数，但**不该**因此重新请求。
  // 真正决定"要不要重新请求"的是 deps。
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;

  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;
    setLoading(true);

    fetcherRef
      .current(controller.signal)
      .then((result) => {
        // 🔴 双重防护：即使 abort 未生效，旧响应也不能覆盖新状态
        if (cancelled) return;
        setData(result);
        setError(null);
        setLoaded(true);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        // 主动取消不是错误：静默忽略，否则切换页面时会闪一下错误态
        if (err instanceof DOMException && err.name === "AbortError") return;
        setError(normalizeError(err));
        setLoaded(true);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
      controller.abort();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce]);

  const reload = useCallback(() => setNonce((n) => n + 1), []);

  const mutate = useCallback((updater: (prev: T) => T) => {
    // 🔴 data 为 null 时什么都不做：还没加载过就没有可改写的对象，
    // 此时静默忽略比抛错更安全（写操作可能在首次加载完成前就返回了）。
    setData((prev) => (prev === null ? prev : updater(prev)));
  }, []);

  return { data, error, loading, loaded, reload, mutate };
}

/**
 * 把未知异常归一化。
 *
 * 🔴 保留 `ApiError` / `NetworkError` 的具体类型（页面要靠 `code` 分支），
 * 其余包成 `Error`——绝不能把原始异常直接塞进 state，
 * 那会让组件在渲染时因为读取不存在的属性而崩掉。
 */
export function normalizeError(err: unknown): ApiError | NetworkError | Error {
  if (err instanceof ApiError || err instanceof NetworkError) return err;
  if (err instanceof Error) return err;
  return new Error(String(err));
}
