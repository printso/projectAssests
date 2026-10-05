/**
 * 顶栏：全局搜索 + 连接状态 + 主题切换 + 快捷入口。
 *
 * # 🔴 搜索必须是真实检索，不是原型的本地过滤
 * 原型把 `state.search` 存起来，然后在页面渲染函数里对 **MOCK 数组**做 `filter`——
 * 看起来"能搜"，实际搜的是那 128 条假数据。
 *
 * 这里改为：输入 → 防抖 → 跳 `/search?q=...` → 由后端 FTS5 + 排序返回结果。
 * 搜索质量、召回范围、子串降级提示全部来自后端，前端只负责展示。
 *
 * # 🔴 连接状态指示必须真实
 * 原型这里写死 `<span class="dot"></span>本地数据库 · 单机模式`，
 * 那个绿点永远亮着——即使后端根本没启动。
 * 这会让用户在服务挂掉时以为"连接正常，只是数据没加载出来"，
 * 从而反复刷新而不是去启动服务。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Icon } from "./Icon";
import type { Theme } from "@/lib/useAppearance";
import type { ServiceStatus } from "@/lib/AppContext";
import { SEARCH_DEBOUNCE_MS } from "@/config";

export interface TopbarProps {
  status: ServiceStatus;
  theme: Theme;
  onThemeChange: (theme: Theme) => void;
  /** 初始搜索词（从 /search 页回填，保持输入框与 URL 一致） */
  initialQuery?: string;
  onSearch: (q: string) => void;
}

export function Topbar({ status, theme, onThemeChange, initialQuery = "", onSearch }: TopbarProps) {
  const navigate = useNavigate();
  const [value, setValue] = useState(initialQuery);
  const inputRef = useRef<HTMLInputElement>(null);

  // URL 变化时同步输入框（例如点击搜索结果里的链接后又返回）
  useEffect(() => {
    setValue(initialQuery);
  }, [initialQuery]);

  // 🔴 防抖：每敲一个字符就发请求的话，中文输入法组合期间会打出十几个无效请求，
  // 而且后端 FTS 查询虽快，并发十几个也会让排序结果闪烁（用户看到列表反复跳）。
  const debounce = useRef<ReturnType<typeof setTimeout> | null>(null);
  const handleInput = useCallback(
    (next: string) => {
      setValue(next);
      if (debounce.current !== null) clearTimeout(debounce.current);
      debounce.current = setTimeout(() => {
        const q = next.trim();
        // 🔴 空查询也要提交：后端把空 q 当"浏览模式"（按质量列出全部），
        // 这是有用的行为。但如果用户从没输入过就直接跳搜索页，
        // 反而多一次无意义导航，所以只在"曾经有内容后清空"时提交。
        if (q === "" && initialQuery === "") return;
        onSearch(q);
      }, SEARCH_DEBOUNCE_MS);
    },
    [onSearch, initialQuery],
  );

  useEffect(() => {
    return () => {
      if (debounce.current !== null) clearTimeout(debounce.current);
    };
  }, []);

  // ⌘K / Ctrl+K 聚焦搜索（原型标了这个快捷键但没实现）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        inputRef.current?.focus();
        inputRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const submit = useCallback(() => {
    if (debounce.current !== null) clearTimeout(debounce.current);
    onSearch(value.trim());
  }, [onSearch, value]);

  return (
    <header className="topbar">
      <label className="searchbox">
        <Icon name="search" />
        <input
          ref={inputRef}
          value={value}
          onChange={(e) => handleInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") {
              setValue("");
              if (initialQuery !== "") onSearch("");
            }
          }}
          placeholder="搜索项目、资产、能力、洞察、机会…"
          aria-label="全局搜索"
          spellCheck={false}
          autoComplete="off"
        />
        <span className="kbd">⌘K</span>
      </label>

      <div className="topbar-right">
        <button className="chip-btn" onClick={() => navigate("/analyst")}>
          <Icon name="spark" /> AI 分析师
        </button>

        <ConnectionChip status={status} />

        <div className="theme-switch" role="group" aria-label="主题切换">
          <button
            type="button"
            className={theme === "light" ? "is-active" : ""}
            onClick={() => onThemeChange("light")}
            title="亮色主题"
            aria-pressed={theme === "light"}
          >
            <Icon name="sun" />
          </button>
          <button
            type="button"
            className={theme === "dark" ? "is-active" : ""}
            onClick={() => onThemeChange("dark")}
            title="暗色主题"
            aria-pressed={theme === "dark"}
          >
            <Icon name="moon" />
          </button>
        </div>

        <button className="icon-btn" onClick={() => navigate("/settings")} title="设置" aria-label="设置">
          <Icon name="gear" />
        </button>
      </div>
    </header>
  );
}

/**
 * 连接状态徽标。
 *
 * 🔴 三种状态三种颜色，绝不写死绿色：
 * - online：绿点 + "本地数据库 · 单机模式"
 * - checking：中性 + "连接中…"（启动瞬间，不要闪一下红）
 * - offline：红点 + "服务未连接"（用户需要知道去启动服务）
 */
function ConnectionChip({ status }: { status: ServiceStatus }) {
  const config = {
    online: { color: "var(--color-success)", text: "本地数据库 · 单机模式" },
    checking: { color: "var(--color-text-3)", text: "连接中…" },
    offline: { color: "var(--color-danger)", text: "服务未连接" },
  }[status];

  return (
    <span className="sync-chip" title={config.text} aria-live="polite">
      <span className="dot" style={{ background: config.color }} />
      {config.text}
    </span>
  );
}
