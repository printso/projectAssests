/**
 * 资产库页。
 *
 * # 🔴 类型 chips 来自后端 `all_types`，不是前端写死的映射
 * 原型把类型硬编码成 `{全部, 代码, 组件, 知识, 决策, 经验, 创意, Prompt}`，
 * 还挂了个 disabled 的「Outcome（未开放）」——那是假 chip，点了没反应。
 *
 * 这里用 `getAssetTypes` 的 `all_types`：后端把**所有已知的资产类型**都列出来，
 * 含当前计数为 0 的。这是刻意的：
 * 若只列 `by_type`（有结果的），用户勾掉某类型后那个 chip 就消失了，
 * 再也点不回来，只能刷新页面——筛选控件不该因为筛选结果而自我销毁。
 *
 * # 🔴 证据是产品红线
 * 资产没有证据就不入库（后端 upsert 门禁）。前端展示时把 `evidence_files`
 * 显式标出来，让用户知道"这条结论有 N 个文件支撑"，而不是凭空冒出来的。
 *
 * # 🔴 反馈走真实端点并就地更新
 * `setAssetFeedback` 返回**更新后的条目**，直接替换列表里对应项，
 * 不重新拉整个列表（资产可能上千条，重拉的延迟肉眼可见）。
 * 标记后必须立刻有视觉反馈，否则用户以为没点上而反复点击。
 */

import { useCallback, useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { getAsset, getAssetTypes, listAssets, setAssetFeedback } from "@/api/endpoints";
import type { AssetDetail, AssetListItem, AssetListPage, TypeBreakdown } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { useToast } from "@/components/Toast";
import { Button, Chips, InlineEmpty, KV, PageHead, Pagination, ResultMeta, Tag, Tags } from "@/components/ui";
import { EmptyState, ErrorStateWithNav, Loading } from "@/components/States";
import { Icon, type IconName } from "@/components/Icon";
import { DEFAULT_PAGE_SIZE } from "@/config";

/** 复用层级筛选（value 与后端 `build_filter` 的 tier 解析一致）。 */
const TIERS = [
  { value: "", label: "全部价值" },
  { value: "high", label: "高价值" },
  { value: "medium", label: "重要" },
  { value: "low", label: "一般" },
];

const SORTS = [
  { value: "", label: "综合" },
  { value: "reuse_score", label: "复用分" },
  { value: "confidence", label: "置信度" },
  { value: "created", label: "最新" },
];

/**
 * 🔴 这一层是**薄分发器**：只读 URL 决定渲染「列表」还是「详情」，自身 hooks 恒定。
 *
 * 早期把列表逻辑与 `?id=` 详情分支写在同一个组件里，用
 * `if (detailId) return <AssetDetailView/>` 在一堆 useCallback/useMemo/useAsync
 * **之前**早返回。从列表点进详情时是同一路由组件重渲染，早返回跳过了后面所有 hooks，
 * hooks 数量骤减 → React 抛 "Rendered fewer hooks than expected" 直接白屏。
 * 拆成两个各自 hooks 恒定的子组件（AssetListView / AssetDetailView）是根治方式。
 */
export function AssetsPage() {
  const [params, setParams] = useSearchParams();
  const navigate = useNavigate();
  const detailId = params.get("id");

  if (detailId !== null && detailId !== "") {
    return (
      <AssetDetailView
        id={detailId}
        onBack={() => {
          const next = new URLSearchParams(params);
          next.delete("id");
          setParams(next, { replace: true });
        }}
        onOpenProject={(pid) => navigate(`/projects/${pid}`)}
      />
    );
  }
  return <AssetListView />;
}

function AssetListView() {
  const navigate = useNavigate();
  const toast = useToast();
  const [params, setParams] = useSearchParams();
  // 反馈提交中的资产 id（用于禁用该卡片的按钮，防重复提交）
  const [pendingFeedback, setPendingFeedback] = useState<string | null>(null);

  const types = params.get("types") ?? "";
  const keyword = params.get("q") ?? "";
  const tier = params.get("tier") ?? "";
  const sort = params.get("sort") ?? "";
  const projectId = params.get("project_id") ?? "";
  const offset = Number.parseInt(params.get("offset") ?? "0", 10) || 0;

  const update = useCallback(
    (patch: Record<string, string | null>) => {
      const next = new URLSearchParams(params);
      for (const [k, v] of Object.entries(patch)) {
        if (v === null || v === "") next.delete(k);
        else next.set(k, v);
      }
      if (!("offset" in patch)) next.delete("offset");
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  const query = useMemo(
    () => ({
      ...(types ? { types: types.split(",") } : {}),
      ...(keyword ? { keyword } : {}),
      ...(tier ? { tier } : {}),
      ...(sort ? { sort } : {}),
      ...(projectId ? { project_id: projectId } : {}),
      limit: DEFAULT_PAGE_SIZE,
      offset,
    }),
    [types, keyword, tier, sort, projectId, offset],
  );

  const { data, error, loading, reload, mutate } = useAsync<AssetListPage>(
    (signal) => listAssets(query, signal),
    [JSON.stringify(query)],
  );

  // 类型分布单独拉：它受当前筛选影响（chips 数字要与列表条数对得上）
  const { data: typeData } = useAsync<TypeBreakdown>(
    (signal) => getAssetTypes(query, signal),
    [JSON.stringify(query)],
  );

  const selectedTypes = types === "" ? [] : types.split(",");

  const toggleType = useCallback(
    (value: string) => {
      const next = selectedTypes.includes(value)
        ? selectedTypes.filter((t) => t !== value)
        : [...selectedTypes, value];
      update({ types: next.length > 0 ? next.join(",") : null });
    },
    [selectedTypes, update],
  );

  const handleFeedback = useCallback(
    async (asset: AssetListItem, feedback: string | null) => {
      setPendingFeedback(asset.id);
      try {
        const updated = await setAssetFeedback(asset.id, feedback);
        // 🔴 用 mutate 就地替换那一条，不重拉整页：
        // 资产可能上千条，重拉的延迟肉眼可见，且会丢掉滚动位置。
        mutate((prev) => ({
          ...prev,
          items: prev.items.map((a) => (a.id === updated.id ? updated : a)),
        }));
        toast.success(
          feedback === null
            ? "已撤销反馈"
            : feedback === "useful"
              ? "已标记为有用"
              : feedback === "useless"
                ? "已标记为无用"
                : "已忽略该资产",
        );
      } catch (err) {
        toast.error(err instanceof Error ? err.message : "反馈提交失败");
      } finally {
        setPendingFeedback(null);
      }
    },
    [toast, mutate],
  );

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }

  // 🔴 all_types 是全库口径：count=0 的类型在库里根本不存在，隐藏。
  // 被当前筛选排除但确实存在的类型，全库计数仍 >0，chips 保留、可点回。
  const typeChips = (typeData?.all_types ?? [])
    .filter((t) => t.count > 0)
    .map((t) => ({
      value: t.value,
      label: t.label,
      count: t.count,
    }));

  return (
    <>
      <PageHead
        title="资产库"
        sub={
          data
            ? `共 ${data.total} 个可复用资产 · 每条都带证据文件`
            : "从你的真实代码中提取的可复用资产（代码、组件、方案、知识）"
        }
        actions={
          <Button size="sm" icon="refresh" onClick={reload} busy={loading}>
            刷新
          </Button>
        }
      />

      <div className="filter-bar">
        <label className="searchbox grow" style={{ maxWidth: 320 }}>
          <Icon name="search" />
          <input
            value={keyword}
            onChange={(e) => update({ q: e.target.value })}
            placeholder="搜索资产名、描述、标签、路径…"
            aria-label="搜索资产"
            spellCheck={false}
          />
        </label>

        <select
          className="chip-btn"
          value={tier}
          onChange={(e) => update({ tier: e.target.value || null })}
          aria-label="复用价值"
        >
          {TIERS.map((t) => (
            <option key={t.value} value={t.value}>
              {t.label}
            </option>
          ))}
        </select>

        <select
          className="chip-btn"
          value={sort}
          onChange={(e) => update({ sort: e.target.value || null })}
          aria-label="排序方式"
        >
          {SORTS.map((s) => (
            <option key={s.value} value={s.value}>
              {s.label}
            </option>
          ))}
        </select>

        {projectId !== "" ? (
          <button type="button" className="chip is-active" onClick={() => update({ project_id: null })}>
            <Icon name="folder" /> 限定项目 <Icon name="x" />
          </button>
        ) : null}
      </div>

      {typeChips.length > 0 ? (
        <div className="filter-bar">
          <Chips options={typeChips} selected={selectedTypes} onSelect={toggleType} multi />
        </div>
      ) : null}

      {loading && data === null ? <Loading rows={6} label="加载资产" /> : null}

      {data !== null ? (
        <>
          <ResultMeta total={data.total} offset={offset} shown={data.items.length} />
          <div className={loading ? "is-refreshing" : undefined}>
            {data.items.length === 0 ? (
              <EmptyState
                icon="box"
                title={keyword || types ? "没有匹配的资产" : "还没有资产"}
                message={
                  keyword || types
                    ? "当前筛选条件下没有资产。试着放宽类型或清空关键词。"
                    : "资产由索引阶段从真实代码中提取。完成一次扫描（含索引）后这里就会出现可复用的代码、组件与方案。"
                }
                action={
                  keyword || types
                    ? { label: "清除筛选", icon: "x", onClick: () => setParams(new URLSearchParams(), { replace: true }) }
                    : { label: "去扫描", icon: "scan", onClick: () => navigate("/") }
                }
              />
            ) : (
              <div className="asset-grid">
                {data.items.map((a) => (
                  <AssetCard
                    key={a.id}
                    asset={a}
                    pending={pendingFeedback === a.id}
                    onOpen={() => navigate(`/assets?id=${encodeURIComponent(a.id)}`)}
                    onFeedback={(fb) => void handleFeedback(a, fb)}
                    onOpenProject={() => navigate(`/projects/${a.project_id}`)}
                  />
                ))}
              </div>
            )}
          </div>

          <Pagination
            total={data.total}
            limit={data.limit}
            offset={data.offset}
            onChange={(next) => update({ offset: String(next) })}
          />
        </>
      ) : null}
    </>
  );
}

interface AssetCardProps {
  asset: AssetListItem;
  pending: boolean;
  onOpen: () => void;
  onFeedback: (feedback: string | null) => void;
  onOpenProject: () => void;
}

function AssetCard({ asset: a, pending, onOpen, onFeedback, onOpenProject }: AssetCardProps) {
  return (
    <article className="asset-card">
      <div className="head">
        <div className="ico-box" style={{ background: "rgba(59,130,246,.22)", color: "#60a5fa" }}>
          <Icon name={iconOfType(a.asset_type)} />
        </div>
        <div className="name" title={a.name}>
          {a.name}
        </div>
        <span className={`badge ${tierBadgeClass(a.tier)}`}>{a.tier_label}</span>
      </div>

      <div className="type mono">
        {a.type_label} · reuse {(a.reuse_score * 100).toFixed(0)}% · 置信{" "}
        {(a.confidence * 100).toFixed(0)}%
      </div>

      <div className="desc">{a.description || "（无描述）"}</div>

      <div className="disc-tags">
        {a.tags.slice(0, 3).map((t) => (
          <Tag key={t}>{t}</Tag>
        ))}
        {/* 🔴 证据文件数显式标出：让用户知道这条结论有多少真实文件支撑 */}
        <Tag mono>证据 {a.evidence_files}</Tag>
      </div>

      <div className="foot">
        <button
          className="meta"
          type="button"
          onClick={onOpenProject}
          title={`来自 ${a.project_name}`}
          style={{ background: "none", border: "none", cursor: "pointer", padding: 0 }}
        >
          <Icon name="folder" /> {a.project_name}
        </button>
        <span className="link-more">
          <button type="button" onClick={onOpen} style={{ background: "none", border: "none", cursor: "pointer", color: "inherit", padding: 0 }}>
            详情 <Icon name="arr" />
          </button>
        </span>
      </div>

      {/* ── 反馈：标记后会驱动采纳率与排序 ─────────────────── */}
      <div
        style={{
          display: "flex",
          gap: 6,
          borderTop: "1px solid var(--color-border)",
          paddingTop: 8,
          marginTop: 2,
        }}
      >
        <FeedbackButton
          active={a.user_feedback === "useful"}
          disabled={pending}
          onClick={() => onFeedback(a.user_feedback === "useful" ? null : "useful")}
          icon="check"
          label="有用"
          color="var(--color-success)"
        />
        <FeedbackButton
          active={a.user_feedback === "useless"}
          disabled={pending}
          onClick={() => onFeedback(a.user_feedback === "useless" ? null : "useless")}
          icon="x"
          label="无用"
          color="var(--color-text-3)"
        />
        <FeedbackButton
          active={a.user_feedback === "ignored"}
          disabled={pending}
          onClick={() => onFeedback(a.user_feedback === "ignored" ? null : "ignored")}
          icon="eyeoff"
          label="忽略"
          color="var(--color-text-3)"
        />
        <span style={{ flex: 1 }} />
        <span className="meta" style={{ alignSelf: "center" }}>
          {a.created_relative}
        </span>
      </div>
    </article>
  );
}

function FeedbackButton({
  active,
  disabled,
  onClick,
  icon,
  label,
  color,
}: {
  active: boolean;
  disabled: boolean;
  onClick: () => void;
  icon: IconName;
  label: string;
  color: string;
}) {
  return (
    <button
      type="button"
      className={`btn btn--ghost btn--sm`}
      onClick={onClick}
      disabled={disabled}
      aria-pressed={active}
      title={active ? `取消「${label}」标记` : `标记为${label}`}
      style={{
        color: active ? color : "var(--color-text-3)",
        borderColor: active ? color : undefined,
        fontSize: "var(--fs-xs)",
        padding: "2px 8px",
      }}
    >
      <Icon name={icon} />
      {label}
    </button>
  );
}

function iconOfType(t: string): IconName {
  switch (t) {
    case "component":
      return "box";
    case "knowledge":
      return "book";
    case "decision":
      return "shield";
    case "experience":
      return "clock";
    case "idea":
      return "bulb";
    case "prompt":
      return "spark";
    default:
      return "code";
  }
}

function tierBadgeClass(tier: string): string {
  switch (tier) {
    case "high":
      return "badge--high";
    case "medium":
      return "badge--potent";
    default:
      return "badge--muted";
  }
}

// ══════════════════════════════════════════════════════════════════
// 资产详情（?id=）
// ══════════════════════════════════════════════════════════════════

interface AssetDetailViewProps {
  id: string;
  onBack: () => void;
  onOpenProject: (projectId: string) => void;
}

/**
 * 单个资产的完整视图。
 *
 * # 🔴 证据链是这一页的核心
 * 产品红线是"无证据不入库"，所以能走到这里的资产必然带证据。
 * 但证据**充分与否**仍要区分（`evidence.sufficient`）：
 * 只有 1 个文件支撑的资产与有 9 处调用的资产，可信度差一个量级，
 * 界面上必须让用户一眼看出差别，而不是都显示成"有证据"。
 *
 * # 🔴 重复资产是最高价值的信息
 * `duplicates` 列出其他项目里的同名/同功能资产——
 * "你已经写过一份了"这句话能直接省下几小时。放在显眼位置。
 */
function AssetDetailView({ id, onBack, onOpenProject }: AssetDetailViewProps) {
  const navigate = useNavigate();
  const toast = useToast();
  const [pending, setPending] = useState(false);

  const { data, error, loading, reload, mutate } = useAsync<AssetDetail>(
    (signal) => getAsset(id, signal),
    [id],
  );

  const handleFeedback = useCallback(
    async (feedback: string | null) => {
      setPending(true);
      try {
        const updated = await setAssetFeedback(id, feedback);
        // 详情视图的字段比列表项多，用返回的列表项覆盖共有字段即可，
        // 但更稳妥的是直接重拉详情——单条请求，代价可忽略
        mutate((prev) => ({
          ...prev,
          user_feedback: updated.user_feedback,
        }));
        toast.success(
          feedback === null
            ? "已撤销反馈"
            : feedback === "useful"
              ? "已标记为有用"
              : feedback === "useless"
                ? "已标记为无用"
                : "已忽略该资产",
        );
      } catch (err) {
        toast.error(err instanceof Error ? err.message : "反馈提交失败");
      } finally {
        setPending(false);
      }
    },
    [id, mutate, toast],
  );

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }
  if (loading && data === null) {
    return <Loading rows={5} label="加载资产详情" />;
  }
  if (data === null) {
    return (
      <EmptyState
        icon="box"
        title="资产不存在"
        message="该资产可能已被重新索引移除。"
        action={{ label: "返回资产库", onClick: onBack }}
      />
    );
  }

  const a = data;
  return (
    <>
      <PageHead
        title={a.name}
        sub={
          <span className="mono" title={a.source_path}>
            {a.source_path}
          </span>
        }
        actions={
          <>
            <Button size="sm" icon="arr" onClick={onBack}>
              返回列表
            </Button>
            <Button size="sm" icon="folder" onClick={() => onOpenProject(a.project_id)}>
              {a.project_name}
            </Button>
          </>
        }
      />

      <div className="grid cols-2" style={{ alignItems: "start" }}>
        {/* ── 左：内容与评分 ─────────────────────────────── */}
        <div>
          <section className="card" style={{ marginBottom: 16 }}>
            <div className="disc-title-row" style={{ marginBottom: 10 }}>
              <span className={`badge ${tierBadgeClass(a.tier)}`}>{a.tier_label}</span>
              <Tag>{a.type_label}</Tag>
              <Tag mono>reuse {(a.reuse_score * 100).toFixed(0)}%</Tag>
              <Tag mono>置信 {(a.confidence * 100).toFixed(0)}%</Tag>
            </div>
            <p style={{ color: "var(--color-text-2)", fontSize: "var(--fs-md)", lineHeight: "var(--lh-base)", margin: "0 0 12px" }}>
              {a.description || "（无描述）"}
            </p>
            <Tags items={a.tags} />

            <div style={{ marginTop: 14 }}>
              <KV k="通用性" v={`${(a.generality * 100).toFixed(0)}%`} />
              <KV k="稳定性" v={`${(a.stability * 100).toFixed(0)}%`} />
              <KV k="抽取时间" v={a.created_at} />
            </div>
          </section>

          {/* 代码内容：抽取器取到才显示 */}
          {a.content !== null ? (
            <section className="card" style={{ marginBottom: 16 }}>
              <div className="card-head">
                <div className="card-title">
                  <Icon name="code" /> 代码内容
                </div>
              </div>
              <div className="code-block">
                <code>{a.content}</code>
              </div>
            </section>
          ) : (
            <section className="card" style={{ marginBottom: 16 }}>
              <InlineEmpty>
                抽取器未能取到该资产的代码内容（可能是二进制或超大文件）。可打开源文件查看：
                <span className="mono"> {a.source_path}</span>
              </InlineEmpty>
            </section>
          )}
        </div>

        {/* ── 右：证据链与重复资产 ───────────────────────── */}
        <div>
          <section className="card" style={{ marginBottom: 16 }}>
            <div className="card-head">
              <div>
                <div className="card-title">
                  <Icon name="shield" /> 证据链
                </div>
                <div className="card-sub">
                  {a.evidence.sufficient
                    ? `证据充分（${a.evidence.file_count} 个文件支撑）`
                    : `证据偏弱（仅 ${a.evidence.file_count} 个文件），复用前建议人工确认`}
                </div>
              </div>
              <span className={`badge ${a.evidence.sufficient ? "badge--high" : "badge--potent"}`}>
                {a.evidence.sufficient ? "充分" : "偏弱"}
              </span>
            </div>

            {a.evidence.files.length > 0 ? (
              <>
                <div className="card-sub" style={{ margin: "6px 0" }}>
                  文件（{a.evidence.files.length}）
                </div>
                <ul style={{ margin: "0 0 10px 4px" }}>
                  {a.evidence.files.map((f) => (
                    <li key={f} className="mono" style={{ padding: "2px 0", fontSize: "var(--fs-sm)" }}>
                      · {f}
                    </li>
                  ))}
                </ul>
              </>
            ) : null}

            {a.evidence.used_by.length > 0 ? (
              <>
                <div className="card-sub" style={{ margin: "6px 0" }}>
                  被调用处（{a.evidence.used_by.length}）
                </div>
                <ul style={{ margin: "0 0 10px 4px" }}>
                  {a.evidence.used_by.map((u) => (
                    <li key={u} className="mono" style={{ padding: "2px 0", fontSize: "var(--fs-sm)" }}>
                      · {u}
                    </li>
                  ))}
                </ul>
              </>
            ) : null}

            {a.evidence.commits.length > 0 ? (
              <>
                <div className="card-sub" style={{ margin: "6px 0" }}>
                  相关提交（{a.evidence.commits.length}）
                </div>
                <ul style={{ margin: "0 0 10px 4px" }}>
                  {a.evidence.commits.map((c) => (
                    <li key={c} className="mono" style={{ padding: "2px 0", fontSize: "var(--fs-sm)" }}>
                      · {c}
                    </li>
                  ))}
                </ul>
              </>
            ) : null}

            {a.evidence.reasoning.length > 0 ? (
              <>
                <div className="card-sub" style={{ margin: "6px 0" }}>
                  判定依据
                </div>
                <ul style={{ margin: 0, paddingLeft: 4 }}>
                  {a.evidence.reasoning.map((r, i) => (
                    <li key={i} style={{ padding: "2px 0", fontSize: "var(--fs-sm)", color: "var(--color-text-2)" }}>
                      · {r}
                    </li>
                  ))}
                </ul>
              </>
            ) : null}
          </section>

          {/* 🔴 重复资产：最高价值的信息，放显眼位置 */}
          {a.duplicates.length > 0 ? (
            <section className="card" style={{ marginBottom: 16 }}>
              <div className="card-head">
                <div>
                  <div className="card-title">
                    <Icon name="repeat" /> 其他项目里的重复实现
                  </div>
                  <div className="card-sub">你已经写过 {a.duplicates.length} 份类似的——考虑抽成共享组件</div>
                </div>
              </div>
              <div className="citation-list">
                {a.duplicates.map((d) => (
                  <button
                    key={d.id}
                    className="citation-item"
                    type="button"
                    onClick={() => navigate(`/assets?id=${encodeURIComponent(d.id)}`)}
                  >
                    <span className="kind">
                      <Icon name="box" />
                    </span>
                    <div className="label">
                      {d.name}
                      <div className="supports mono">
                        {d.project_name} · {d.source_path} · reuse {(d.reuse_score * 100).toFixed(0)}%
                      </div>
                    </div>
                  </button>
                ))}
              </div>
            </section>
          ) : null}

          {/* 反馈 */}
          <section className="card">
            <div className="card-head">
              <div className="card-title">
                <Icon name="check" /> 这个资产有用吗？
              </div>
            </div>
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
              <FeedbackButton
                active={a.user_feedback === "useful"}
                disabled={pending}
                onClick={() => void handleFeedback(a.user_feedback === "useful" ? null : "useful")}
                icon="check"
                label="有用"
                color="var(--color-success)"
              />
              <FeedbackButton
                active={a.user_feedback === "useless"}
                disabled={pending}
                onClick={() => void handleFeedback(a.user_feedback === "useless" ? null : "useless")}
                icon="x"
                label="无用"
                color="var(--color-text-3)"
              />
              <FeedbackButton
                active={a.user_feedback === "ignored"}
                disabled={pending}
                onClick={() => void handleFeedback(a.user_feedback === "ignored" ? null : "ignored")}
                icon="eyeoff"
                label="忽略"
                color="var(--color-text-3)"
              />
            </div>
            <div className="assist-hint" style={{ marginTop: 8 }}>
              反馈会回流到排序：标记"有用"的资产在检索与推荐中优先级更高。
            </div>
          </section>
        </div>
      </div>
    </>
  );
}
