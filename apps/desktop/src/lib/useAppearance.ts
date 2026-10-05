/**
 * 主题（深色/浅色）与减弱动效。
 *
 * # 🔴 唯一真相源是后端设置，不是 localStorage
 * 原型的主题存在 `localStorage`（`LS_KEY = "spolia.theme"`），
 * 而后端 `Settings.appearance` 里**也有** theme 字段。两处并存必然漂移：
 * 用户在界面切成浅色，重启后后端设置仍是深色，主题自己跳回去。
 *
 * 这里的规则：
 * 1. 挂载时从后端读 `appearance.theme`，写到 `document.documentElement.dataset.theme`
 * 2. 切换时**先乐观更新 DOM**（用户立刻看到效果），再写回后端
 * 3. 写回失败则回滚 DOM 并提示——绝不留下"界面是浅色、后端记的是深色"的分裂状态
 *
 * # 🔴 首屏闪烁（FOUC）
 * 等后端响应才应用主题会让用户先看到一帧默认深色再跳成浅色。
 * 所以在 `index.html` 里内联一段脚本，用 localStorage 的**缓存值**先渲染，
 * 后端响应到达后再以后端为准纠正。localStorage 在这里只是"上次值的缓存"，
 * 不是真相源——这个区分很重要，缓存丢了最多闪一下，不会导致持久不一致。
 */

import { useCallback, useEffect, useState } from "react";
import * as api from "@/api/endpoints";
import type { AppearanceSettings } from "@/api/types";

export type Theme = AppearanceSettings["theme"];

/** localStorage 缓存键（仅用于避免首屏闪烁，非真相源）。 */
const THEME_CACHE_KEY = "spolia.theme.cache";

export function readCachedTheme(): Theme | null {
  try {
    const v = localStorage.getItem(THEME_CACHE_KEY);
    return v === "light" || v === "dark" ? v : null;
  } catch {
    // localStorage 不可用（隐私模式等）：降级为不缓存，不影响功能
    return null;
  }
}

function writeCachedTheme(theme: Theme): void {
  try {
    localStorage.setItem(THEME_CACHE_KEY, theme);
  } catch {
    // 写不进去就算了，下次首屏会闪一下，不是错误
  }
}

/** 把主题应用到 DOM（CSS 变量按 `[data-theme="light"]` 覆盖）。 */
export function applyThemeToDom(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
}

/** 把"减弱动效"应用到 DOM（base.css 用 `[data-reduce-motion]` 选择器）。 */
function applyReduceMotionToDom(reduce: boolean): void {
  if (reduce) {
    document.documentElement.dataset.reduceMotion = "true";
  } else {
    delete document.documentElement.dataset.reduceMotion;
  }
}

export interface AppearanceState {
  theme: Theme;
  reduceMotion: boolean;
  /** 是否已从后端加载过（未加载前用的是缓存值，切换会被后端值覆盖） */
  loaded: boolean;
  setTheme: (theme: Theme) => Promise<void>;
  setReduceMotion: (reduce: boolean) => Promise<void>;
  /** 用后端设置覆盖本地状态（设置页保存后调用） */
  syncFrom: (appearance: AppearanceSettings) => void;
  /** 切换主题时出错的信息（供调用方 toast） */
  error: string | null;
}

export function useAppearance(): AppearanceState {
  const [theme, setThemeState] = useState<Theme>(() => readCachedTheme() ?? "dark");
  const [reduceMotion, setReduceMotionState] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 挂载时立刻应用缓存值，避免首屏无主题
  useEffect(() => {
    applyThemeToDom(theme);
    applyReduceMotionToDom(reduceMotion);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 从后端拉真实设置
  useEffect(() => {
    let cancelled = false;
    api
      .getSettings()
      .then((settings) => {
        if (cancelled) return;
        setThemeState(settings.appearance.theme);
        setReduceMotionState(settings.appearance.reduce_motion);
        writeCachedTheme(settings.appearance.theme);
        setLoaded(true);
      })
      .catch(() => {
        // 🔴 后端不可用时**不报错**，继续用缓存值：
        // 主题偏好是纯视觉的东西，服务没起来时用户至少该看到界面。
        // 把这里变成错误态会导致"后端没启动 → 整个前端白屏"。
        if (!cancelled) setLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // theme / reduceMotion 变化时同步到 DOM
  useEffect(() => {
    applyThemeToDom(theme);
  }, [theme]);
  useEffect(() => {
    applyReduceMotionToDom(reduceMotion);
  }, [reduceMotion]);

  const setTheme = useCallback(async (next: Theme) => {
    const prev = theme;
    // 乐观更新：用户点击后立刻看到效果，不等网络往返
    setThemeState(next);
    writeCachedTheme(next);
    setError(null);
    try {
      await api.updateAppearance({ theme: next });
    } catch (err) {
      // 🔴 回滚：留下"界面浅色、后端深色"的分裂状态比报错更糟——
      // 下次启动主题会自己跳回去，用户完全无法理解。
      setThemeState(prev);
      writeCachedTheme(prev);
      setError(err instanceof Error ? err.message : "主题保存失败");
    }
  }, [theme]);

  const setReduceMotion = useCallback(
    async (next: boolean) => {
      const prev = reduceMotion;
      setReduceMotionState(next);
      setError(null);
      try {
        await api.updateAppearance({ reduce_motion: next });
      } catch (err) {
        setReduceMotionState(prev);
        setError(err instanceof Error ? err.message : "动效设置保存失败");
      }
    },
    [reduceMotion],
  );

  const syncFrom = useCallback((appearance: AppearanceSettings) => {
    setThemeState(appearance.theme);
    setReduceMotionState(appearance.reduce_motion);
    writeCachedTheme(appearance.theme);
    setLoaded(true);
  }, []);

  return { theme, reduceMotion, loaded, setTheme, setReduceMotion, syncFrom, error };
}
