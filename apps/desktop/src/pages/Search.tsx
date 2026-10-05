/**
 * 全局搜索页。
 *
 * # 🔴 这一页直接受益于后端的中文检索修复
 * 后端把含 CJK 的长查询展开为「完整短语 OR 滑动 trigram」，
 * 所以"我有哪些重复实现的代码"这种整句自然语言也能命中洞察/资产。
 * 前端要做的两件关键事：
 * 1. **如实显示 `used_substring_fallback`**：降级到 LIKE 子串匹配时召回质量较低，
 *    藏着不说会让用户以为"就这点结果"而不去调整关键词。
 * 2. **kind_counts 驱动分类 tab**：角标数字来自后端本页分布，不是全库计数。
 *
 * # 🔴 空结果的引导来自后端 empty_hint
 * 后端区分"库是空的→去扫描"与"筛选太严→放宽"，前端不自己猜。
 *
 * # 🔴 查询串与筛选都同步到 URL
 * 搜索结果可分享、刷新不丢、后退能回到上一次的查询。
 */

import { useCallback, useMemo } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { search } from "@/api/endpoints";
import type { HitKind, SearchScope, SearchView, SortBy } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { Button, Chips, PageHead, Pagination, ResultMeta, Tag } from "@/components/ui";
import { EmptyState, ErrorStateWithNav, Loading } from "@/components/States";
import { Icon, type IconName } from "@/components/Icon";
import { DEFAULT_PAGE_SIZE } from "@/config";
import { routeForLink } from "@/lib/navigate";

const SCOPES: { value: SearchScope; label: string }[] = [
  { value: "all", label: "全部" },
  { value: "projects", label: "项目" },
  { value: "assets", label: "资产" },
  { value: "capabilities", label: "能力" },
  { value: "insights", label: "洞察与机会" },
  { value: "knowledge", label: "知识" },
];

const SORTS: { value: SortBy; label: string }[] = [
  { value: "relevance", label: "相关性" },
  { value: "reuse_score", label: "复用价值" },
  { value: "recently_updated", label: "最近更新" },
  { value: "confidence", label: "置信度" },
];

export function SearchPage() {
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();

  const q = params.get("q") ?? "";
  const scope = (params.get("scope") as SearchScope | null) ?? "all";
  const sort = (params.get("sort") as SortBy | null) ?? "relevance";
  const kind = params.get("kind") ?? "";
  const offset = Number.parseInt(params.get("offset") ?? "0", 10) || 0;

  const update = useCallback(
    (patch: Record<string, string | null>) => {
      const next = new URLSearchParams(params);
      for (const [k, v] of Object.entries(patch)) {
        if (v === null || v === "" || v === "all" || v === "relevance") next.delete(k);
        else next.set(k, v);
      }
      if (!("offset" in patch)) next.delete("offset");
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  const query = useMemo(
    () => ({
      ...(q ? { q } : {}),
      ...(scope !== "all" ? { scope } : {}),
      ...(sort !== "relevance" ? { sort } : {}),
      limit: DEFAULT_PAGE_SIZE,
      offset,
    }),
    [q, scope, sort, offset],
  );

  const { data, error, loading, reload } = useAsync<SearchView>(
    (signal) => search(query, signal),
    [JSON.stringify(query)],
  );

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }

  // kind_counts 用于分类 tab：后端已按固定枚举顺序返回（含计数为 0 的类型）
  const kindTabs = (data?.kind_counts ?? []).filter((k) => k.count > 0 || k.kind === kind);

  return (
    <>
      <PageHead
        title="搜索"
        sub={
          data
            ? `「${data.query || "全部"}」· 命中 ${data.total} 项 · 耗时 ${data.took_ms}ms`
            : "跨项目、资产、能力、洞察与机会的统一检索"
        }
        actions={
          <Button size="sm" icon="refresh" onClick={reload} busy={loading}>
            刷新
          </Button>
        }
      />

      {/* 🔴 降级提示：子串匹配的召回质量低于 FTS，必须让用户知道 */}
      {data?.used_substring_fallback ? (
        <div
          style={{
            padding: "8px 12px",
            marginBottom: 12,
            background: "color-mix(in srgb, var(--color-warning) 12%, transparent)",
            border: "1px solid color-mix(in srgb, var(--color-warning) 34%, transparent)",
            borderRadius: "var(--r-md)",
            color: "var(--color-text-2)",
            fontSize: "var(--fs-sm)",
          }}
        >
          <Icon name="alert" /> 当前为子串匹配模式（查询过短或含特殊字符，未能走全文索引）。
          结果可能偏多或偏少，试试输入更具体的关键词。
        </div>
      ) : null}

      <div className="filter-bar">
        <label className="searchbox grow" style={{ maxWidth: 420 }}>
          <Icon name="search" />
          <input
            value={q}
            onChange={(e) => update({ q: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter") reload();
            }}
            placeholder="用自然语言提问，或输入标识符、关键词…"
            aria-label="搜索"
            spellCheck={false}
            autoFocus
          />
          {q !== "" ? (
            <button
              type="button"
              onClick={() => update({ q: null })}
              aria-label="清除搜索词"
              style={{ display: "flex", color: "var(--color-text-3)" }}
            >
              <Icon name="x" />
            </button>
          ) : null}
        </label>

        <select
          className="chip-btn"
          value={sort}
          onChange={(e) => update({ sort: e.target.value })}
          aria-label="排序方式"
        >
          {SORTS.map((s) => (
            <option key={s.value} value={s.value}>
              {s.label}
            </option>
          ))}
        </select>
      </div>

      {/* 范围 chips */}
      <div className="filter-bar">
        <Chips
          options={SCOPES.map((s) => ({ value: s.value, label: s.label }))}
          selected={scope}
          onSelect={(v) => update({ scope: v })}
        />
      </div>

      {/* 类型 tab（仅当有分布时显示） */}
      {kindTabs.length > 1 ? (
        <div className="filter-bar">
          <Chips
            options={[
              { value: "", label: "全部类型", count: data?.total },
              ...kindTabs.map((k) => ({ value: k.kind, label: k.label, count: k.count })),
            ]}
            selected={kind}
            onSelect={(v) => update({ kind: v || null })}
          />
        </div>
      ) : null}

      {loading && data === null ? <Loading rows={6} label="搜索中" /> : null}

      {data !== null ? (
        <>
          <ResultMeta total={data.total} offset={offset} shown={data.hits.length} />
          <div className={loading ? "is-refreshing" : undefined}>
            {data.hits.length === 0 ? (
              <EmptyState
                icon="search"
                title="没有匹配的结果"
                // 🔴 优先用后端 empty_hint：它知道是库空还是筛选太严
                message={
                  data.empty_hint ??
                  (q
                    ? `没有与「${q}」匹配的内容。试试更短的关键词，或检查是否已扫描过相关目录。`
                    : "输入关键词开始搜索，或清空筛选浏览全部内容。")
                }
                action={
                  q || scope !== "all"
                    ? { label: "清除筛选", icon: "x", onClick: () => setParams(new URLSearchParams(), { replace: true }) }
                    : { label: "去扫描", icon: "scan", onClick: () => navigate("/") }
                }
              />
            ) : (
              <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
                {data.hits
                  // kind tab 是纯前端过滤（本页结果的分布），不改后端查询
                  .filter((h) => kind === "" || h.kind === kind)
                  .map((h) => (
                    <HitCard key={`${h.kind}-${h.id}`} hit={h} onOpen={() => openHit(h, navigate)} />
                  ))}
              </div>
            )}
          </div>

          <Pagination
            total={data.total}
            limit={DEFAULT_PAGE_SIZE}
            offset={offset}
            onChange={(next) => update({ offset: String(next) })}
          />
        </>
      ) : null}
    </>
  );
}

/**
 * 据命中的 link 跳转。映射逻辑收敛在 `routeForLink`（单一真相源），
 * 这里只做解构——避免 Search/Graph/Overview 各写一份而漂移。
 */
function openHit(hit: { link: { page: string; param: string | null } }, navigate: (p: string) => void) {
  navigate(routeForLink(hit.link.page, hit.link.param));
}

interface HitCardProps {
  hit: SearchView["hits"][number];
  onOpen: () => void;
}

function HitCard({ hit: h, onOpen }: HitCardProps) {
  return (
    <button className="card" type="button" onClick={onOpen} style={{ textAlign: "left", width: "100%", cursor: "pointer" }}>
      <div style={{ display: "flex", gap: 12, alignItems: "flex-start" }}>
        <div className="disc-ico" style={{ background: "rgba(99,102,241,.14)" }}>
          <Icon name={iconOfKind(h.kind)} />
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="disc-title-row">
            <span className="disc-title">{h.title}</span>
            <span className="badge badge--muted">{h.kind_label}</span>
            <span className="badge badge--info" title={`相关性 ${h.score_percent}%`}>
              {h.score_percent}%
            </span>
          </div>
          <div style={{ color: "var(--color-text-3)", fontSize: "var(--fs-sm)", margin: "2px 0 6px" }}>
            {h.subtitle}
          </div>
          {/* snippet 里的高亮由后端标注（<mark>），这里用 dangerouslySetInnerHTML 是安全的：
              后端 highlight() 只对查询词包 <mark>，且已对原文做 HTML 转义 */}
          {h.snippet ? (
            <div
              style={{ color: "var(--color-text-2)", fontSize: "var(--fs-sm)", lineHeight: "var(--lh-base)" }}
              dangerouslySetInnerHTML={{ __html: h.snippet }}
            />
          ) : null}
          <div className="disc-tags" style={{ marginTop: 8 }}>
            {/* reasons 是"为什么命中"的真实解释（基于分数），不是编的 */}
            {h.reasons.slice(0, 3).map((r, i) => (
              <Tag key={i}>{r}</Tag>
            ))}
            {h.source_labels.slice(0, 3).map((s, i) => (
              <Tag key={`s${i}`} mono>
                {s}
              </Tag>
            ))}
          </div>
        </div>
        <span className="disc-arrow">
          <Icon name="chev" />
        </span>
      </div>
    </button>
  );
}

function iconOfKind(kind: HitKind): IconName {
  switch (kind) {
    case "project":
      return "folder";
    case "asset":
      return "box";
    case "capability":
      return "graph";
    case "insight":
      return "drop";
    case "opportunity":
      return "bulb";
    case "knowledge":
      return "book";
    case "experience":
      return "clock";
    case "decision":
      return "shield";
    case "idea":
      return "bulb";
    default:
      return "doc";
  }
}
