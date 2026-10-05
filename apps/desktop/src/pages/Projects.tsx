/**
 * 项目列表页。
 *
 * # 🔴 与原型的关键差异：服务端分页 + facets 驱动筛选
 * 原型是 `M.projects.filter(p => ...)` —— 对 128 条 mock 做客户端过滤，
 * chips「全部/Active/Paused/Archived」写死，点了**根本不生效**（只是切了个 is-active 样式）。
 *
 * 这里：
 * - 筛选条件走 query string 传给后端，由 SQL 真正过滤
 * - chips 选项来自后端 `facets`（真实存在的语言/状态 + 各自数量）
 * - 分页由后端 `total` 驱动，前端不猜测总页数
 *
 * # 🔴 筛选状态同步到 URL
 * 好处是前进/后退可用、当前视图可分享、刷新后不丢失。
 * 更重要的是**避免了"状态与显示不一致"**：若筛选只存在 useState 里，
 * 用户点浏览器后退时页面会回到上一个路由但筛选条件被重置，
 * 看到的列表与他记忆中离开时不同——这类不一致用户很难描述清楚。
 */

import { useCallback, useMemo } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { listProjects } from "@/api/endpoints";
import type { ProjectListItem, ProjectListPage } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { Button, Chips, PageHead, Pagination, ResultMeta, Tag } from "@/components/ui";
import { EmptyState, ErrorStateWithNav, Loading } from "@/components/States";
import { Icon } from "@/components/Icon";
import { DEFAULT_PAGE_SIZE } from "@/config";

/** 排序选项。value 与后端 `ProjectSort` 的解析一致。 */
const SORTS = [
  { value: "", label: "综合" },
  { value: "health", label: "健康度" },
  { value: "updated", label: "最近更新" },
  { value: "loc", label: "代码量" },
  { value: "name", label: "名称" },
];

export function ProjectsPage() {
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();

  // ── 从 URL 读筛选状态（单一真相源是 URL，不是 useState）──
  const status = params.get("status") ?? "";
  const language = params.get("language") ?? "";
  const keyword = params.get("q") ?? "";
  const sort = params.get("sort") ?? "";
  const sensitive = params.get("sensitive");
  const offset = Number.parseInt(params.get("offset") ?? "0", 10) || 0;

  /**
   * 更新筛选。
   *
   * 🔴 改筛选时必须把 offset 归零：
   * 否则用户在第 3 页换了个筛选条件，后端按新条件算出只有 1 页，
   * offset=40 会返回空列表——界面显示"没有结果"，
   * 而用户完全不知道是"翻页位置没重置"导致的。
   */
  const update = useCallback(
    (patch: Record<string, string | null>) => {
      const next = new URLSearchParams(params);
      for (const [k, v] of Object.entries(patch)) {
        if (v === null || v === "") next.delete(k);
        else next.set(k, v);
      }
      // 除非显式指定 offset，否则任何筛选变化都回到第一页
      if (!("offset" in patch)) next.delete("offset");
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  const query = useMemo(
    () => ({
      ...(status ? { status } : {}),
      ...(language ? { language } : {}),
      ...(keyword ? { keyword } : {}),
      ...(sort ? { sort } : {}),
      ...(sensitive === "true" ? { sensitive: true } : sensitive === "false" ? { sensitive: false } : {}),
      limit: DEFAULT_PAGE_SIZE,
      offset,
    }),
    [status, language, keyword, sort, sensitive, offset],
  );

  const { data, error, loading, reload } = useAsync<ProjectListPage>(
    (signal) => listProjects(query, signal),
    // 🔴 用序列化后的 query 作依赖：对象每次渲染都是新引用，
    // 直接列对象会导致无限重渲染。
    [JSON.stringify(query)],
  );

  const hasFilters = status !== "" || language !== "" || keyword !== "" || sensitive !== null;

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }

  return (
    <>
      <PageHead
        title="项目"
        sub={
          data
            ? `共 ${data.total} 个项目 · 点击查看详情、能力覆盖与 AI 画像`
            : "扫描到的本机代码项目"
        }
        actions={
          <>
            {hasFilters ? (
              <Button size="sm" icon="x" onClick={() => setParams(new URLSearchParams(), { replace: true })}>
                清除筛选
              </Button>
            ) : null}
            <Button size="sm" icon="refresh" onClick={reload} busy={loading}>
              刷新
            </Button>
          </>
        }
      />

      {/* ── 筛选条 ─────────────────────────────────────────── */}
      <div className="filter-bar">
        <label className="searchbox grow" style={{ maxWidth: 320 }}>
          <Icon name="search" />
          <input
            value={keyword}
            onChange={(e) => update({ q: e.target.value })}
            placeholder="搜索项目名、描述、标签…"
            aria-label="搜索项目"
            spellCheck={false}
          />
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

        <button
          type="button"
          className={`chip${sensitive === "true" ? " is-active" : ""}`}
          onClick={() => update({ sensitive: sensitive === "true" ? null : "true" })}
          aria-pressed={sensitive === "true"}
          title="只看标记为敏感的项目（这些项目按 Local-First 策略只用本地模型）"
        >
          <Icon name="shield" /> 敏感
        </button>
      </div>

      {/* ── facets：真实存在的状态与语言 ─────────────────────── */}
      {/* 🔴 过滤**全局零计数**的选项：计数来自 facets（全库口径），
          为 0 意味着库里根本不存在该状态/语言，显示出来是纯噪音
          （点了必然空结果）。注意这与"被当前筛选排除"不同——
          后者在全库口径下计数仍 >0，chips 依然可见、可点回。 */}
      {data && data.facets.statuses.some((f) => f.count > 0) ? (
        <div className="filter-bar">
          <Chips
            options={[
              { value: "", label: "全部状态", count: data.total },
              ...data.facets.statuses
                .filter((f) => f.count > 0)
                .map((f) => ({ value: f.value, label: f.label, count: f.count })),
            ]}
            selected={status}
            onSelect={(v) => update({ status: v || null })}
          />
        </div>
      ) : null}

      {data && data.facets.languages.some((f) => f.count > 0) ? (
        <div className="filter-bar">
          <Chips
            options={[
              { value: "", label: "全部语言" },
              ...data.facets.languages
                .filter((f) => f.count > 0)
                .map((f) => ({ value: f.value, label: f.label, count: f.count })),
            ]}
            selected={language}
            onSelect={(v) => update({ language: v || null })}
          />
        </div>
      ) : null}

      {/* ── 结果 ───────────────────────────────────────────── */}
      {loading && data === null ? <Loading rows={6} label="加载项目列表" /> : null}

      {data !== null ? (
        <>
          <ResultMeta
            total={data.total}
            offset={offset}
            shown={data.items.length}
            extra={
              loading ? <span style={{ color: "var(--color-text-3)" }}>刷新中…</span> : undefined
            }
          />
          <div className={loading ? "is-refreshing" : undefined}>
            {data.items.length === 0 ? (
              <EmptyState
                icon="folder"
                title={hasFilters ? "没有匹配的项目" : "还没有任何项目"}
                message={
                  hasFilters
                    ? `当前筛选条件下没有项目。${
                        keyword ? `关键词「${keyword}」可能不在项目名或描述里。` : ""
                      }`
                    : "项目由扫描产生。在设置里添加代码目录后执行扫描，这里就会出现你的真实项目。"
                }
                action={
                  hasFilters
                    ? {
                        label: "清除筛选",
                        icon: "x",
                        onClick: () => setParams(new URLSearchParams(), { replace: true }),
                      }
                    : { label: "去设置扫描目录", icon: "gear", onClick: () => navigate("/settings") }
                }
              />
            ) : (
              <div className="asset-grid">
                {data.items.map((p) => (
                  <ProjectCard key={p.id} project={p} onOpen={() => navigate(`/projects/${p.id}`)} />
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

function ProjectCard({ project: p, onOpen }: { project: ProjectListItem; onOpen: () => void }) {
  return (
    <button className="asset-card" type="button" onClick={onOpen} style={{ textAlign: "left" }}>
      <div className="head">
        <div className="ico-box" style={{ background: "rgba(139,92,246,.22)", color: "#a78bfa" }}>
          <Icon name="folder" />
        </div>
        <div className="name" title={p.name}>
          {p.name}
        </div>
        {/* 🔴 状态标签与配色都由后端 `status_label` / `status` 决定。
            原型自己写了一套 active/paused/abandoned/experimental 映射，
            与后端真实的 ProjectStatus（active/idle/archived/unknown）对不上。 */}
        <span className={`badge ${statusBadgeClass(p.status)}`}>{p.status_label}</span>
      </div>

      <div className="type mono">
        {p.language || "未识别语言"}
        {p.framework ? ` · ${p.framework}` : ""}
      </div>

      <div className="desc">{p.description || "（无描述）"}</div>

      {/* 事实标记：这些是扫描器真实探测到的，不是装饰 */}
      <div className="disc-tags">
        <Tag mono>{p.files} 文件</Tag>
        <Tag mono>{p.loc.toLocaleString()} 行</Tag>
        {p.has_git ? <Tag mono>git {p.git_commits}</Tag> : null}
        {p.has_tests ? <Tag>有测试</Tag> : null}
        {p.sensitive ? <Tag>敏感</Tag> : null}
        {p.has_profile ? <Tag>已画像</Tag> : null}
      </div>

      <div className="foot">
        <span className="meta">
          {/* 🔴 `updated_display` 由后端算好（相对时间 + 绝对时间口径统一），
              前端不再自己 parse 日期——两处各自格式化必然出现口径不一致。 */}
          {p.updated_display}
          {p.days_idle !== null ? ` · 闲置 ${p.days_idle} 天` : ""}
        </span>
        <span className="link-more">
          打开 <Icon name="arr" />
        </span>
      </div>
    </button>
  );
}

function statusBadgeClass(status: string): string {
  switch (status) {
    case "active":
      return "badge--high";
    case "idle":
      return "badge--info";
    case "archived":
      return "badge--muted";
    default:
      return "badge--muted";
  }
}
