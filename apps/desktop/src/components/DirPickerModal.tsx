/**
 * 目录选择弹窗：浏览本机目录树并勾选，替代"手输绝对路径"。
 *
 * # 为什么必须有它（用户习惯）
 * 手输路径要求用户：切到资源管理器 → 复制路径 → 回来粘贴 → 祈祷没打错。
 * 任何一步出错都只得到一句"目录不存在"。点选是用户对"选目录"这件事的
 * 默认心智模型（所有安装器/IDE 都这么干），手输降级为兜底入口。
 *
 * # 交互约定
 * - 单击行 = 勾选/取消；双击行或点箭头 = 进入该目录
 * - 面包屑可点回任意上级；「上一级」按钮在根层禁用
 * - 快速入口（主目录/桌面/文档/下载/当前工作目录）一键直达
 * - 隐藏目录默认折叠（与资源管理器默认视图一致），可开关
 * - Esc / 点遮罩 = 取消；初始焦点在「取消」（同 ConfirmModal 的安全约定）
 *
 * # 🔴 只列目录不列文件
 * 授权对象是目录；列出文件只会诱导用户选文件然后被后端拒绝，
 * 等于亲手制造一次失败体验。
 *
 * # 🔴 截断要诚实
 * 后端单层有上限（`truncated`）。为真时明确提示"未列全，请手输定位"，
 * 绝不假装列全了——用户勾不到想要的目录时会以为是我们漏了。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listFsDirs } from "@/api/endpoints";
import type { FsListView } from "@/api/types";
import { Icon } from "@/components/Icon";

export interface DirPickFailure {
  path: string;
  reason: string;
}

export interface DirPickerModalProps {
  onClose: () => void;
  /**
   * 用户点「添加」时回调；由父组件负责调用 addScanDir 与 toast（弹窗不持有写操作）。
   *
   * 🔴 返回**失败清单**而非抛错：多选时常见"3 个里 1 个不存在"，
   * 抛错会让已成功的那 2 个也显得像失败了。返回空数组 = 全部成功 → 关闭弹窗；
   * 非空 = 弹窗保持打开、勾选只保留失败项（成功项自动取消勾选），
   * 用户改勾或改路径后可直接重试，不必重新浏览一遍目录树。
   */
  onPick: (paths: string[]) => Promise<{ failed: DirPickFailure[] }>;
}

export function DirPickerModal({ onClose, onPick }: DirPickerModalProps) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const [view, setView] = useState<FsListView | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [showHidden, setShowHidden] = useState(false);
  const [picking, setPicking] = useState(false);
  const [pickError, setPickError] = useState<string | null>(null);
  const [manualOpen, setManualOpen] = useState(false);
  const [manualPath, setManualPath] = useState("");

  // 当前浏览路径（空串 = 根）
  const [current, setCurrent] = useState("");

  const load = useCallback((path: string, hidden: boolean) => {
    setLoading(true);
    setLoadError(null);
    const controller = new AbortController();
    listFsDirs({ path: path === "" ? undefined : path, show_hidden: hidden || undefined }, controller.signal)
      .then((v) => {
        setView(v);
        setCurrent(v.path);
      })
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        setLoadError(err instanceof Error ? err.message : "读取目录失败");
        setView(null);
      })
      .finally(() => setLoading(false));
    return controller;
  }, []);

  useEffect(() => {
    const c = load(current, showHidden);
    return () => c.abort();
    // 只在打开时加载一次根；后续导航走显式调用
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const navigate = useCallback(
    (path: string) => {
      setCurrent(path);
      load(path, showHidden);
    },
    [load, showHidden],
  );

  const toggleHidden = useCallback(
    (next: boolean) => {
      setShowHidden(next);
      load(current, next);
    },
    [load, current],
  );

  // Esc 关闭（同 ConfirmModal：挂 window，遮罩不在 tab 序列里）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !picking) onClose();
    };
    window.addEventListener("keydown", onKey);
    cancelRef.current?.focus();
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, picking]);

  const toggleSelect = (path: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const handleConfirm = useCallback(async () => {
    if (selected.size === 0) return;
    setPicking(true);
    setPickError(null);
    try {
      const { failed } = await onPick([...selected]);
      if (failed.length === 0) {
        onClose();
        return;
      }
      // 部分失败：保留失败项勾选供重试，成功项取消勾选
      setSelected(new Set(failed.map((f) => f.path)));
      setPickError(failed.map((f) => `${f.path}：${f.reason}`).join("；"));
    } catch (err) {
      setPickError(err instanceof Error ? err.message : "添加失败");
    } finally {
      setPicking(false);
    }
  }, [selected, onPick, onClose]);

  const handleManualAdd = useCallback(async () => {
    const p = manualPath.trim();
    if (p === "") return;
    setPicking(true);
    setPickError(null);
    try {
      const { failed } = await onPick([p]);
      if (failed.length === 0) {
        onClose();
        return;
      }
      setPickError(failed.map((f) => `${f.path}：${f.reason}`).join("；"));
    } catch (err) {
      setPickError(err instanceof Error ? err.message : "添加失败");
    } finally {
      setPicking(false);
    }
  }, [manualPath, onPick, onClose]);

  const crumbs = useMemo(() => splitPath(current), [current]);

  return (
    <div className="modal-mask is-open" role="dialog" aria-modal="true" aria-label="选择扫描目录" onClick={picking ? undefined : onClose}>
      <div className="modal" style={{ width: "min(680px, 94vw)" }} onClick={(e) => e.stopPropagation()}>
        <h3>选择扫描目录</h3>
        <p style={{ marginBottom: 12 }}>勾选要纳入扫描的目录（可多选），或双击进入子目录。只读不写，凭证文件自动跳过。</p>

        {/* ── 快速入口 ─────────────────────────────────────── */}
        {view !== null && view.roots.length > 0 ? (
          <div className="chips" style={{ marginBottom: 10 }}>
            {view.roots.map((r) => (
              <button key={r.path} type="button" className="chip" title={r.path} onClick={() => navigate(r.path)}>
                <Icon name="folder" /> {r.name}
              </button>
            ))}
          </div>
        ) : null}

        {/* ── 面包屑 + 上一级 ──────────────────────────────── */}
        <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8 }}>
          <button
            type="button"
            className="icon-btn"
            title="上一级"
            aria-label="上一级"
            disabled={view === null || view.parent === null}
            onClick={() => view !== null && view.parent !== null && navigate(view.parent)}
          >
            <Icon name="chev" style={{ transform: "rotate(180deg)" }} />
          </button>
          <div className="mono" style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: "var(--fs-sm)" }}>
            {crumbs.length === 0 ? (
              <button type="button" className="chip" onClick={() => navigate("")}>此电脑</button>
            ) : (
              <>
                <button type="button" className="chip" onClick={() => navigate("")}>此电脑</button>
                {crumbs.map((c, i) => (
                  <span key={c.full}>
                    <span style={{ color: "var(--color-text-3)" }}> / </span>
                    {i === crumbs.length - 1 ? (
                      <b>{c.name}</b>
                    ) : (
                      <button type="button" className="chip" onClick={() => navigate(c.full)}>
                        {c.name}
                      </button>
                    )}
                  </span>
                ))}
              </>
            )}
          </div>
          <label style={{ display: "flex", gap: 6, alignItems: "center", fontSize: "var(--fs-xs)", color: "var(--color-text-3)", whiteSpace: "nowrap", cursor: "pointer" }}>
            <input type="checkbox" checked={showHidden} onChange={(e) => toggleHidden(e.target.checked)} />
            显示隐藏目录
          </label>
        </div>

        {/* ── 目录列表 ─────────────────────────────────────── */}
        <div
          style={{
            border: "1px solid var(--color-border)",
            borderRadius: "var(--r-md)",
            maxHeight: 320,
            overflow: "auto",
            background: "var(--color-panel-2)",
          }}
        >
          {loading ? (
            <div style={{ padding: 18, color: "var(--color-text-3)", fontSize: "var(--fs-sm)" }}>读取目录中…</div>
          ) : loadError !== null ? (
            <div style={{ padding: 18, color: "var(--color-danger)", fontSize: "var(--fs-sm)" }}>
              <Icon name="alert" /> {loadError}
            </div>
          ) : view === null || view.entries.length === 0 ? (
            <div style={{ padding: 18, color: "var(--color-text-3)", fontSize: "var(--fs-sm)" }}>
              该目录下没有子目录。可返回上一级，或在下方手动输入路径。
            </div>
          ) : (
            view.entries.map((e) => (
              <div
                key={e.path}
                role="option"
                aria-selected={selected.has(e.path)}
                tabIndex={0}
                onClick={() => toggleSelect(e.path)}
                onDoubleClick={() => e.has_children && navigate(e.path)}
                onKeyDown={(ev) => {
                  if (ev.key === "Enter") {
                    if (e.has_children) navigate(e.path);
                    else toggleSelect(e.path);
                  }
                  if (ev.key === " ") {
                    ev.preventDefault();
                    toggleSelect(e.path);
                  }
                }}
                style={{
                  display: "flex",
                  gap: 8,
                  alignItems: "center",
                  padding: "7px 10px",
                  cursor: "pointer",
                  borderBottom: "1px solid var(--color-border)",
                  background: selected.has(e.path) ? "color-mix(in srgb, var(--color-primary) 12%, transparent)" : undefined,
                }}
              >
                <input type="checkbox" readOnly checked={selected.has(e.path)} tabIndex={-1} />
                <Icon name="folder" />
                <span className="mono" style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", fontSize: "var(--fs-sm)" }} title={e.path}>
                  {e.name}
                </span>
                {e.has_children ? (
                  <button
                    type="button"
                    className="icon-btn"
                    title="进入"
                    aria-label={`进入 ${e.name}`}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      navigate(e.path);
                    }}
                  >
                    <Icon name="chev" />
                  </button>
                ) : (
                  <span style={{ width: 26 }} />
                )}
              </div>
            ))
          )}
        </div>

        {view !== null && view.truncated ? (
          <div style={{ marginTop: 6, fontSize: "var(--fs-xs)", color: "var(--color-warning)" }}>
            <Icon name="alert" /> 该层目录过多，仅显示前 500 个。没找到目标请返回上级或在下方手动输入路径。
          </div>
        ) : null}

        {/* ── 手动输入兜底 ─────────────────────────────────── */}
        <div style={{ marginTop: 10 }}>
          <button type="button" className="chip" onClick={() => setManualOpen((v) => !v)}>
            {manualOpen ? "收起手动输入" : "找不到？手动输入路径"}
          </button>
          {manualOpen ? (
            <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
              <input
                className="mono"
                style={{ flex: 1 }}
                value={manualPath}
                onChange={(e) => setManualPath(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void handleManualAdd();
                }}
                placeholder="例如 F:/CodeProject"
                aria-label="手动输入目录路径"
              />
              <button type="button" className="btn btn--ghost btn--sm" disabled={picking || manualPath.trim() === ""} onClick={() => void handleManualAdd()}>
                添加
              </button>
            </div>
          ) : null}
        </div>

        {pickError !== null ? (
          <div style={{ marginTop: 8, fontSize: "var(--fs-sm)", color: "var(--color-danger)" }}>
            <Icon name="alert" /> {pickError}
          </div>
        ) : null}

        <div className="actions">
          <button className="btn btn--ghost btn--sm" onClick={onClose} type="button" ref={cancelRef} disabled={picking}>
            取消
          </button>
          <button
            className="btn btn--primary btn--sm"
            onClick={() => void handleConfirm()}
            type="button"
            disabled={picking || selected.size === 0}
          >
            {picking ? "添加中…" : selected.size > 0 ? `添加 ${selected.size} 个目录` : "添加所选目录"}
          </button>
        </div>
      </div>
    </div>
  );
}

/** 把路径拆成面包屑段（兼容 `/` 与 `\`）。根层返回空数组。 */
function splitPath(path: string): { name: string; full: string }[] {
  if (path.trim() === "") return [];
  const parts = path.split(/[\\/]+/).filter((p) => p !== "");
  const out: { name: string; full: string }[] = [];
  // Windows 盘符：第一段形如 "F:"
  let acc = "";
  parts.forEach((p, i) => {
    if (i === 0 && /^[a-zA-Z]:$/.test(p)) {
      acc = `${p}\\`;
    } else if (acc === "") {
      acc = `/${p}`;
    } else {
      acc = acc.endsWith("\\") || acc.endsWith("/") ? `${acc}${p}` : `${acc}/${p}`;
    }
    out.push({ name: p, full: acc });
  });
  return out;
}
