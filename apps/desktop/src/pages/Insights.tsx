/**
 * 洞察页。
 *
 * # 🔴 双徽章：价值 vs 处置，两个独立维度
 * 每条洞察显示两个徽章，**刻意不合并**：
 * - `ValueBadge`（badge/badge_key）：这条洞察有多可信——"高价值/高潜力/建议查看"，
 *   由置信度分档，后端 domain 层 `Insight::badge()` 算出，与首页同源。
 * - `StateBadge`（state/state_key）：我处理过它没有——"待处理/已标记有用/已忽略"。
 *
 * 早期把两者压进一个字段，导致用户标了"有用"后，价值徽章"高价值"就消失了——
 * 用户失去"这条到底可不可信"的判断依据。两个维度必须各自独立显示。
 *
 * # 🔴 证据必须可展开、可查看
 * 洞察的价值在于"可核查"：每条都带真实证据文件。折叠展示，
 * 点开能看到具体是哪些文件/提交支撑了这个结论——这是防幻觉的最后一道展示层。
 *
 * # 🔴 反馈驱动采纳率
 * `useful/useless/ignored` 反馈回流，`adoption` 显示采纳率。
 * 这是产品闭环：用户标注哪些洞察有用，系统据此调整后续洞察排序。
 */

import { useCallback, useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { getInsight, listInsights, listProjects, setInsightFeedback } from "@/api/endpoints";
import type { InsightDetail, InsightItem, InsightListPage, ProjectListPage } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { useToast } from "@/components/Toast";
import {
  Button,
  Chips,
  InlineEmpty,
  PageHead,
  Pagination,
  ResultMeta,
  StateBadge,
  Tag,
  ValueBadge,
} from "@/components/ui";
import { EmptyState, ErrorStateWithNav, Loading } from "@/components/States";
import { Icon } from "@/components/Icon";
import { DEFAULT_PAGE_SIZE } from "@/config";

export function InsightsPage() {
  const navigate = useNavigate();
  const toast = useToast();
  const [params, setParams] = useSearchParams();
  const [pendingId, setPendingId] = useState<string | null>(null);

  const types = params.get("types") ?? "";
  const unreadOnly = params.get("unread");
  // 🔴 展开哪一条洞察的证据详情，由 URL 的 ?id= 控制（可分享、刷新不丢、后退可收起）
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
      ...(types ? { types: types.split(",") } : {}),
      ...(unreadOnly === "true" ? { unread_only: true } : {}),
      limit: DEFAULT_PAGE_SIZE,
      offset,
    }),
    [types, unreadOnly, offset],
  );

  const { data, error, loading, reload, mutate } = useAsync<InsightListPage>(
    (signal) => listInsights(query, signal),
    [JSON.stringify(query)],
  );

  // 🔴 洞察只带 related_project_ids（内部 id，形如 p_xxx_哈希），
  // 直接显示用户完全看不出是哪两个项目重复。这里拉一次项目列表做 id→name 映射。
  // 项目数量级是百，一次请求可接受；映射失败时回退显示截断 id（不假装知道名字）。
  const { data: projectsData } = useAsync<ProjectListPage>(
    (signal) => listProjects({ limit: 500 }, signal),
    [],
  );
  const projectNameOf = useCallback(
    (id: string): string => {
      const hit = projectsData?.items.find((p) => p.id === id);
      return hit?.name ?? id.slice(0, 18);
    },
    [projectsData],
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
    async (item: InsightItem, feedback: string | null) => {
      setPendingId(item.id);
      try {
        const updated = await setInsightFeedback(item.id, feedback);
        mutate((prev) => ({
          ...prev,
          items: prev.items.map((i) => (i.id === updated.id ? updated : i)),
          // 采纳率随之变化：后端返回的 adoption 是全局的，这里无法精确重算，
          // 标记为需刷新（下次 reload 会带回准确的采纳率）
        }));
        toast.success(
          feedback === null
            ? "已撤销反馈"
            : feedback === "useful"
              ? "已标记有用 · 谢谢，这会提升同类洞察的优先级"
              : feedback === "useless"
                ? "已标记无用"
                : "已忽略",
        );
      } catch (err) {
        toast.error(err instanceof Error ? err.message : "反馈提交失败");
      } finally {
        setPendingId(null);
      }
    },
    [toast, mutate],
  );

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }

  // 🔴 全库口径下计数为 0 的类型不存在，隐藏（理由同 Assets 页）
  const facets = (data?.facets ?? [])
    .filter((f) => f.count > 0)
    .map((f) => ({ value: f.value, label: f.label, count: f.count }));

  return (
    <>
      <PageHead
        title="洞察"
        sub="系统从你的真实代码中算出的结论：重复实现、可复用组件、被遗忘的资产。每条都带可核查的证据。"
        actions={
          <>
            <button
              type="button"
              className={`chip${unreadOnly === "true" ? " is-active" : ""}`}
              onClick={() => update({ unread: unreadOnly === "true" ? null : "true" })}
              aria-pressed={unreadOnly === "true"}
            >
              <Icon name="drop" /> 只看待处理
              {data && data.unread > 0 ? <span className="chip-count">{data.unread}</span> : null}
            </button>
            <Button size="sm" icon="refresh" onClick={reload} busy={loading}>
              刷新
            </Button>
          </>
        }
      />

      {facets.length > 0 ? (
        <div className="filter-bar">
          <Chips options={facets} selected={selectedTypes} onSelect={toggleType} multi />
        </div>
      ) : null}

      {/* 采纳率：反馈回流的闭环指标 */}
      {data && data.adoption.rated > 0 ? (
        <div className="result-meta">
          <span>
            <Icon name="star" /> 采纳率{" "}
            <b style={{ color: "var(--color-text)" }}>
              {data.adoption.rate !== null ? `${Math.round(data.adoption.rate * 100)}%` : "—"}
            </b>
            （{data.adoption.useful} 有用 / {data.adoption.rated} 已评）· {data.adoption.label}
          </span>
        </div>
      ) : null}

      {loading && data === null ? <Loading rows={5} label="加载洞察" /> : null}

      {data !== null ? (
        <>
          <ResultMeta total={data.total} offset={offset} shown={data.items.length} />
          <div className={loading ? "is-refreshing" : undefined}>
            {data.items.length === 0 ? (
              <EmptyState
                icon="drop"
                title={selectedTypes.length > 0 || unreadOnly ? "没有匹配的洞察" : "还没有洞察"}
                message={
                  selectedTypes.length > 0 || unreadOnly
                    ? "当前筛选下没有洞察。试着放宽类型或关闭「只看待处理」。"
                    : "洞察由跨项目分析产生。完成一次扫描（含索引与洞察生成）后，系统会找出重复实现、可复用组件与被遗忘的资产。"
                }
                action={
                  selectedTypes.length > 0 || unreadOnly
                    ? { label: "清除筛选", icon: "x", onClick: () => setParams(new URLSearchParams(), { replace: true }) }
                    : { label: "去扫描", icon: "scan", onClick: () => navigate("/") }
                }
              />
            ) : (
              data.items.map((i) => (
                <InsightCard
                  key={i.id}
                  item={i}
                  pending={pendingId === i.id}
                  expanded={expandedId === i.id}
                  projectNameOf={projectNameOf}
                  onToggleExpand={() =>
                    // 🔴 必须显式带上 offset：update() 对未指定的筛选键会重置分页，
                    // 那是"改筛选条件"时想要的行为，但展开/收起证据不是筛选——
                    // 若在第 3 页展开一条洞察却跳回第 1 页，用户展开的卡片会直接消失，
                    // 看起来就像"点了没反应"。
                    update({
                      id: expandedId === i.id ? null : i.id,
                      offset: offset === 0 ? null : String(offset),
                    })
                  }
                  onFeedback={(fb) => void handleFeedback(i, fb)}
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
    </>
  );
}

interface InsightCardProps {
  item: InsightItem;
  pending: boolean;
  /** URL 上带 `?id=` 时展开这一条的证据详情 */
  expanded: boolean;
  /** 内部项目 id → 可读名称（见 InsightsPage 的映射说明） */
  projectNameOf: (id: string) => string;
  onToggleExpand: () => void;
  onFeedback: (feedback: string | null) => void;
  onOpenProject: (projectId: string) => void;
}

function InsightCard({ item: i, pending, expanded, projectNameOf, onToggleExpand, onFeedback, onOpenProject }: InsightCardProps) {
  return (
    <section className="card" style={{ marginBottom: 14 }}>
      <div style={{ display: "flex", gap: 14, alignItems: "flex-start" }}>
        <div className="disc-ico" style={{ background: "rgba(99,102,241,.16)" }}>
          <Icon name="drop" />
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="disc-title-row">
            <button
              className="disc-title"
              onClick={onToggleExpand}
              type="button"
              aria-expanded={expanded}
              title={expanded ? "收起证据详情" : "展开证据详情"}
              style={{ background: "none", border: "none", cursor: "pointer", color: "inherit", textAlign: "left", padding: 0, font: "inherit" }}
            >
              <Icon name={expanded ? "chev" : "chev"} style={{ transform: expanded ? "rotate(90deg)" : "none", transition: "transform var(--t-fast)" }} />
              {i.title}
            </button>
            {/* 🔴 双徽章：价值（与首页同源）+ 处置状态 */}
            <ValueBadge label={i.badge} badgeKey={i.badge_key} />
            <StateBadge state={i.state} stateKey={i.state_key} />
          </div>

          <p style={{ color: "var(--color-text-2)", fontSize: 13, margin: "6px 0", lineHeight: "var(--lh-base)" }}>
            {i.description}
          </p>

          <div className="disc-tags" style={{ marginBottom: 8 }}>
            <Tag>{i.type_label}</Tag>
            <Tag mono>置信度 {i.confidence_percent}%</Tag>
            <Tag mono>证据 {i.evidence_count}</Tag>
            {i.tags.slice(0, 4).map((t) => (
              <Tag key={t}>{t}</Tag>
            ))}
          </div>

          {/* 关联项目：点击跳转 */}
          {i.related_project_ids.length > 0 ? (
            <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginBottom: 8 }}>
              {i.related_project_ids.map((pid) => (
                <button
                  key={pid}
                  type="button"
                  className="btn btn--ghost btn--sm"
                  onClick={() => onOpenProject(pid)}
                  title={pid}
                  style={{ fontSize: "var(--fs-xs)", padding: "2px 8px" }}
                >
                  {/* 🔴 显示可读名称；完整内部 id 放 title，需要时仍可核对 */}
                  <Icon name="folder" /> {projectNameOf(pid)}
                </button>
              ))}
            </div>
          ) : null}

          <div style={{ display: "flex", gap: 8, marginTop: 10, alignItems: "center", flexWrap: "wrap" }}>
            <FeedbackBtn
              active={i.user_feedback === "useful"}
              disabled={pending}
              onClick={() => onFeedback(i.user_feedback === "useful" ? null : "useful")}
              icon="check"
              label="有用"
              activeColor="var(--color-success)"
            />
            <FeedbackBtn
              active={i.user_feedback === "useless"}
              disabled={pending}
              onClick={() => onFeedback(i.user_feedback === "useless" ? null : "useless")}
              icon="x"
              label="无用"
              activeColor="var(--color-danger)"
            />
            <FeedbackBtn
              active={i.user_feedback === "ignored"}
              disabled={pending}
              onClick={() => onFeedback(i.user_feedback === "ignored" ? null : "ignored")}
              icon="eyeoff"
              label="忽略"
              activeColor="var(--color-text-3)"
            />
            <span className="tag" style={{ marginLeft: "auto" }}>
              {i.created_relative}
            </span>
          </div>

          {expanded ? <InsightEvidencePanel id={i.id} /> : null}
        </div>
      </div>
    </section>
  );
}

/**
 * 就地展开的证据详情。
 *
 * 🔴 洞察的价值在于"可核查"：这里把后端返回的真实证据文件、关联项目、
 * 关联资产都列出来，用户能逐条点开验证。展开时才请求详情（懒加载），
 * 避免列表页一次性拉全部洞察的证据。
 */
function InsightEvidencePanel({ id }: { id: string }) {
  const navigate = useNavigate();
  const { data, error, loading } = useAsync<InsightDetail>((signal) => getInsight(id, signal), [id]);

  if (loading) return <Loading rows={2} label="加载证据" />;
  if (error !== null) {
    return <InlineEmpty>证据加载失败：{error.message}</InlineEmpty>;
  }
  if (data === null) return null;

  return (
    <div
      style={{
        marginTop: 12,
        paddingTop: 12,
        borderTop: "1px solid var(--color-border)",
      }}
    >
      {data.evidence.length > 0 ? (
        <>
          <div className="card-sub" style={{ marginBottom: 6 }}>
            证据（{data.evidence.length}）
          </div>
          <ul style={{ margin: "0 0 8px 4px" }}>
            {data.evidence.map((e, idx) => (
              <li key={`${e.label}-${idx}`} className="mono" style={{ padding: "2px 0", fontSize: "var(--fs-sm)" }}>
                <span style={{ color: "var(--color-text-3)" }}>· [{e.kind_label}] </span>
                {e.label}
              </li>
            ))}
          </ul>
        </>
      ) : (
        <InlineEmpty>该洞察未附带可展示的证据条目。</InlineEmpty>
      )}

      {data.related_projects.length > 0 ? (
        <>
          <div className="card-sub" style={{ margin: "10px 0 6px" }}>
            关联项目
          </div>
          <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            {data.related_projects.map((p) => (
              <button
                key={p.id}
                type="button"
                className="btn btn--ghost btn--sm"
                onClick={() => navigate(`/projects/${p.id}`)}
                style={{ fontSize: "var(--fs-xs)" }}
              >
                <Icon name="folder" /> {p.name} · {p.status_label}
              </button>
            ))}
          </div>
        </>
      ) : null}

      {data.related_assets.length > 0 ? (
        <>
          <div className="card-sub" style={{ margin: "10px 0 6px" }}>
            关联资产
          </div>
          <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            {data.related_assets.map((a) => (
              <button
                key={a.id}
                type="button"
                className="btn btn--ghost btn--sm"
                onClick={() => navigate(`/assets?id=${encodeURIComponent(a.id)}`)}
                style={{ fontSize: "var(--fs-xs)" }}
              >
                <Icon name="box" /> {a.name} · {a.type_label}
              </button>
            ))}
          </div>
        </>
      ) : null}
    </div>
  );
}

function FeedbackBtn({
  active,
  disabled,
  onClick,
  icon,
  label,
  activeColor,
}: {
  active: boolean;
  disabled: boolean;
  onClick: () => void;
  icon: Parameters<typeof Icon>[0]["name"];
  label: string;
  activeColor: string;
}) {
  return (
    <button
      className="btn btn--ghost btn--sm"
      onClick={onClick}
      disabled={disabled}
      aria-pressed={active}
      type="button"
      style={active ? { borderColor: activeColor, color: activeColor } : undefined}
    >
      <Icon name={icon} /> {label}
    </button>
  );
}
