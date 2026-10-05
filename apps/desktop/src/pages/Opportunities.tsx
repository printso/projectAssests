/**
 * 机会发现页。
 *
 * 机会 = 把历史项目里已有的资产组合成一个新项目方向。
 * 覆盖度越高，说明越多部分能直接复用、越少要从零写。
 *
 * # 🔴 与原型的关键差异
 * 原型的「Explore →」按钮是 `data-toast="原型演示：进入机会深入分析"`——假动作，
 * 「Dismiss」也只是从本地 state 数组里删掉、刷新就回来。
 * 这里两者都走真实端点：
 * - Explore → `getOpportunity` 拉真实的深入分析（rationale/reusable/to_build/mvp/scaffold）
 * - Dismiss → `setOpportunityStatus(id, "dismissed")`，后端持久化
 *
 * # 🔴 批量忽略是破坏性操作，必须二次确认
 * `dismissAllOpportunities` 一次清空全部可操作机会，不可撤销，
 * 所以走确认弹窗，且返回受影响条数（toast 显示"已忽略 N 条"而非笼统"成功"）。
 *
 * # 🔴 星级与覆盖度都来自后端
 * `rating_stars`（"★★★☆☆"）后端已渲染好，前端不自己拼——
 * 两处各拼一遍必然在 clamp 规则上漂移（脏数据 rating=9 会渲染出 9 颗星）。
 */

import { useCallback, useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import {
  dismissAllOpportunities,
  getOpportunity,
  listOpportunities,
  setOpportunityStatus,
} from "@/api/endpoints";
import type { OpportunityDetail, OpportunityItem, OpportunityListPage } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { useToast } from "@/components/Toast";
import { Button, Chips, InlineEmpty, PageHead, Pagination, ResultMeta, Tag } from "@/components/ui";
import { EmptyState, ErrorStateWithNav, Loading } from "@/components/States";
import { Icon } from "@/components/Icon";
import { DEFAULT_PAGE_SIZE } from "@/config";
import { ConfirmModal } from "@/components/ConfirmModal";

export function OpportunitiesPage() {
  const navigate = useNavigate();
  const toast = useToast();
  const [params, setParams] = useSearchParams();
  const [pendingId, setPendingId] = useState<string | null>(null);
  const [confirmingDismissAll, setConfirmingDismissAll] = useState(false);

  const includeClosed = params.get("closed") === "true";
  const minRating = params.get("min_rating");
  const expandedId = params.get("id");
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
      include_closed: includeClosed,
      ...(minRating ? { min_rating: Number.parseInt(minRating, 10) } : {}),
      limit: DEFAULT_PAGE_SIZE,
      offset,
    }),
    [includeClosed, minRating, offset],
  );

  const { data, error, loading, reload, mutate } = useAsync<OpportunityListPage>(
    (signal) => listOpportunities(query, signal),
    [JSON.stringify(query)],
  );

  const handleStatus = useCallback(
    async (item: OpportunityItem, status: string) => {
      setPendingId(item.id);
      try {
        const updated = await setOpportunityStatus(item.id, status);
        mutate((prev) => ({
          ...prev,
          items: prev.items.map((o) => (o.id === updated.id ? updated : o)),
        }));
        toast.success(
          status === "dismissed"
            ? "已忽略这个机会"
            : status === "adopted"
              ? "已标记为采纳"
              : `状态已更新为「${updated.status_label}」`,
        );
      } catch (err) {
        toast.error(err instanceof Error ? err.message : "状态更新失败");
      } finally {
        setPendingId(null);
      }
    },
    [mutate, toast],
  );

  const handleDismissAll = useCallback(async () => {
    setConfirmingDismissAll(false);
    try {
      const r = await dismissAllOpportunities();
      // 🔴 显示后端给的真实条数，不笼统说"成功"
      toast.success(r.message ?? "已忽略全部机会", r.affected !== undefined ? `共 ${r.affected} 条` : undefined);
      reload();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "批量忽略失败");
    }
  }, [toast, reload]);

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }

  // 🔴 全库口径下计数为 0 的星级不存在，隐藏（理由同 Assets 页）
  const facets = (data?.facets ?? [])
    .filter((f) => f.count > 0)
    .map((f) => ({ value: f.value, label: f.label, count: f.count }));

  return (
    <>
      <PageHead
        title="机会发现"
        sub="把你历史项目里已有的能力组合成新方向。覆盖度越高，说明越少要从零写。"
        actions={
          <>
            <button
              type="button"
              className={`chip${includeClosed ? " is-active" : ""}`}
              onClick={() => update({ closed: includeClosed ? null : "true" })}
              aria-pressed={includeClosed}
              title="显示已忽略/已采纳的机会"
            >
              <Icon name="eye" /> 含已处理
            </button>
            {data && data.actionable_count > 0 ? (
              <Button size="sm" icon="trash" onClick={() => setConfirmingDismissAll(true)}>
                忽略全部（{data.actionable_count}）
              </Button>
            ) : null}
            <Button size="sm" icon="refresh" onClick={reload} busy={loading}>
              刷新
            </Button>
          </>
        }
      />

      {facets.length > 0 ? (
        <div className="filter-bar">
          <Chips
            options={[
              { value: "", label: "全部星级" },
              ...facets.map((f) => ({ value: f.value, label: f.label, count: f.count })),
            ]}
            selected={minRating ?? ""}
            onSelect={(v) => update({ min_rating: v || null })}
          />
        </div>
      ) : null}

      {loading && data === null ? <Loading rows={4} label="加载机会" /> : null}

      {data !== null ? (
        <>
          <ResultMeta total={data.total} offset={offset} shown={data.items.length} />
          <div className={loading ? "is-refreshing" : undefined}>
            {data.items.length === 0 ? (
              <EmptyState
                icon="bulb"
                title="暂无机会"
                message={
                  // 🔴 优先用后端给的 empty_hint：它知道是"库空"还是"都被忽略了"
                  data.empty_hint ??
                  "机会由跨项目能力重合分析产生。多个项目都索引过后，系统才能找出可组合的能力。"
                }
                action={
                  includeClosed
                    ? { label: "只看可操作", icon: "x", onClick: () => update({ closed: null }) }
                    : { label: "去扫描", icon: "scan", onClick: () => navigate("/") }
                }
              />
            ) : (
              data.items.map((o) => (
                <OpportunityCard
                  key={o.id}
                  item={o}
                  pending={pendingId === o.id}
                  expanded={expandedId === o.id}
                  onToggleExpand={() =>
                    update({ id: expandedId === o.id ? null : o.id, offset: offset === 0 ? null : String(offset) })
                  }
                  onStatus={(status) => void handleStatus(o, status)}
                  onOpenProject={(pid) => navigate(`/projects/${pid}`)}
                />
              ))
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

      {confirmingDismissAll ? (
        <ConfirmModal
          title="忽略全部机会？"
          body={`这将把当前 ${data?.actionable_count ?? 0} 条可操作机会标记为已忽略。此操作会改变列表默认视图，但可在「含已处理」里找回并恢复。`}
          confirmLabel="确认忽略"
          onConfirm={() => void handleDismissAll()}
          onCancel={() => setConfirmingDismissAll(false)}
        />
      ) : null}
    </>
  );
}

interface OpportunityCardProps {
  item: OpportunityItem;
  pending: boolean;
  expanded: boolean;
  onToggleExpand: () => void;
  onStatus: (status: string) => void;
  onOpenProject: (projectId: string) => void;
}

function OpportunityCard({ item: o, pending, expanded, onToggleExpand, onStatus, onOpenProject }: OpportunityCardProps) {
  return (
    <section className="card accent-panel" style={{ marginBottom: 14 }}>
      <div className="disc-title-row">
        <span className="disc-title" style={{ fontSize: 16 }}>
          <Icon name="star" /> {o.title}
        </span>
        <span className="badge badge--potent">{o.status_label}</span>
      </div>

      <p style={{ color: "var(--color-text-2)", fontSize: 13, margin: "8px 0", lineHeight: "var(--lh-base)" }}>
        {/* 🔴 `why` 是"为什么值得关注"的真实依据，比 description 更能回答用户的疑问 */}
        {o.why || o.description}
      </p>

      <div className="cols-opp" style={{ display: "grid", gap: 20, alignItems: "center" }}>
        <div>
          <div className="cap-row">
            <span>覆盖度</span>
            <span className="bar">
              <i style={{ width: `${o.coverage_percent}%` }} />
            </span>
            <span className="pct">{o.coverage_percent}%</span>
          </div>
          <div className="chips" style={{ marginTop: 8 }}>
            {o.source_projects.map((p) => (
              <button
                key={p.id}
                type="button"
                className="tag mono"
                onClick={() => onOpenProject(p.id)}
                title={`来自 ${p.name}`}
                style={{ cursor: "pointer", border: "1px solid var(--color-border)", background: "var(--color-panel-2)" }}
              >
                {p.name}
              </button>
            ))}
            {o.missing_capabilities.slice(0, 6).map((c) => (
              <span className="tag" key={c} style={{ color: "var(--color-warning)" }}>
                缺 {c}
              </span>
            ))}
          </div>
        </div>
        <div style={{ textAlign: "right" }}>
          {/* 🔴 星级串来自后端 rating_stars，不自己拼 */}
          <div style={{ color: "var(--color-warning)", letterSpacing: 2, fontSize: "var(--fs-lg)" }}>
            {o.rating_stars}
          </div>
          <div style={{ fontSize: 11, color: "var(--color-text-3)", marginTop: 6 }}>
            已具备 {o.required_capabilities.length} 项 · 缺 {o.missing_capabilities.length} 项 ·{" "}
            {o.created_relative}
          </div>
        </div>
      </div>

      <div style={{ display: "flex", gap: 8, marginTop: 14, flexWrap: "wrap" }}>
        <Button size="sm" variant="primary" icon="spark" onClick={onToggleExpand}>
          {expanded ? "收起分析" : o.has_analysis ? "查看深入分析" : "展开分析"}
        </Button>
        {o.actionable ? (
          <>
            <Button size="sm" icon="check" disabled={pending} onClick={() => onStatus("adopted")}>
              采纳
            </Button>
            <Button size="sm" icon="x" disabled={pending} onClick={() => onStatus("dismissed")}>
              忽略
            </Button>
          </>
        ) : null}
      </div>

      {expanded ? <OpportunityAnalysisPanel id={o.id} onOpenProject={onOpenProject} /> : null}
    </section>
  );
}

/**
 * 深入分析面板（懒加载）。
 *
 * 🔴 后端 `analysis` 为 null 表示"尚未做深入分析"，此时显示引导而非空白。
 * 分析内容包含：rationale（为什么值得做）、reusable（可直接复用的资产）、
 * to_build（还需新建的部分）、mvp_suggestion（最小可行方案）、scaffold（脚手架建议）。
 */
function OpportunityAnalysisPanel({
  id,
  onOpenProject,
}: {
  id: string;
  onOpenProject: (pid: string) => void;
}) {
  const navigate = useNavigate();
  const { data, error, loading } = useAsync<OpportunityDetail>((signal) => getOpportunity(id, signal), [id]);

  if (loading) return <Loading rows={2} label="加载分析" />;
  if (error !== null) return <InlineEmpty>分析加载失败：{error.message}</InlineEmpty>;
  if (data === null) return null;

  const a = data.analysis;
  if (a === null) {
    return (
      <div style={{ marginTop: 12, paddingTop: 12, borderTop: "1px solid var(--color-border)" }}>
        <InlineEmpty>
          这条机会还没有做深入分析。深入分析由 AI 生成，需要在设置里配置模型后触发。
        </InlineEmpty>
        <Button size="sm" icon="gear" onClick={() => navigate("/settings")}>
          去配置模型
        </Button>
      </div>
    );
  }

  return (
    <div style={{ marginTop: 12, paddingTop: 12, borderTop: "1px solid var(--color-border)" }}>
      <div className="card-sub" style={{ marginBottom: 6 }}>
        为什么值得做
      </div>
      <p style={{ color: "var(--color-text-2)", fontSize: "var(--fs-sm)", margin: "0 0 12px", lineHeight: "var(--lh-base)" }}>
        {a.rationale}
      </p>

      {a.reusable.length > 0 ? (
        <>
          <div className="card-sub" style={{ marginBottom: 6 }}>
            可直接复用（{a.reusable.length}）
          </div>
          <div className="citation-list" style={{ marginBottom: 12 }}>
            {a.reusable.map((r) => (
              <div className="citation-item" key={r.asset_id}>
                <span className="kind">
                  <Icon name="box" />
                </span>
                <div className="label">
                  {r.name}
                  <div className="supports mono">
                    {r.source_path} · reuse {(r.reuse_score * 100).toFixed(0)}% · {r.migration_note}
                  </div>
                </div>
              </div>
            ))}
          </div>
        </>
      ) : null}

      {a.to_build.length > 0 ? (
        <>
          <div className="card-sub" style={{ marginBottom: 6 }}>
            还需新建（{a.to_build.length}）
          </div>
          <ul style={{ margin: "0 0 12px 4px" }}>
            {a.to_build.map((t) => (
              <li key={t} style={{ padding: "2px 0", fontSize: "var(--fs-sm)", color: "var(--color-text-2)" }}>
                · {t}
              </li>
            ))}
          </ul>
        </>
      ) : null}

      <div className="card-sub" style={{ marginBottom: 6 }}>
        最小可行方案
      </div>
      <p style={{ color: "var(--color-text-2)", fontSize: "var(--fs-sm)", margin: "0 0 12px", lineHeight: "var(--lh-base)" }}>
        {a.mvp_suggestion}
      </p>

      {a.scaffold.length > 0 ? (
        <>
          <div className="card-sub" style={{ marginBottom: 6 }}>
            脚手架建议
          </div>
          <div className="code-block">
            <code>{a.scaffold.join("\n")}</code>
          </div>
        </>
      ) : null}

      {data.item.source_projects.length > 0 ? (
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginTop: 12 }}>
          {data.item.source_projects.map((p) => (
            <Tag key={p.id}>
              <button
                type="button"
                onClick={() => onOpenProject(p.id)}
                style={{ background: "none", border: "none", cursor: "pointer", color: "inherit", padding: 0, font: "inherit" }}
              >
                {p.name}
              </button>
            </Tag>
          ))}
        </div>
      ) : null}
    </div>
  );
}
