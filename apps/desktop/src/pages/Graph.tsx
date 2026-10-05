/**
 * 知识图谱页。
 *
 * # 🔴 图谱最大的陷阱是"静默截断"
 * 156 个项目 + 数千资产全量渲染会卡死浏览器，所以后端**必须**限流。
 * 但只返回 top-N 而不说明的话，用户会误以为"我的项目之间就这么点关联"。
 *
 * 因此后端返回 `stats`（omitted_nodes / omitted_edges / dropped_dangling_edges /
 * isolated_nodes / truncated / summary），这一页的职责就是**如实呈现这些数字**。
 * `stats.summary` 是后端拼好的一句话，直接显示。
 *
 * # 🔴 悬空边已在后端过滤
 * 截断节点后若不同步过滤两端不在集内的边，前端会把线画到画布外。
 * 后端已经做了（`dropped_dangling_edges` 就是被丢弃的数量），
 * 前端只需渲染给到的 nodes/edges——**不要**自己再做截断，那会重新引入悬空边。
 *
 * # 🔴 布局是前端职责，但必须确定性
 * 后端不给坐标（它不知道画布尺寸）。这里用确定性的环形+分层布局：
 * 同样的数据两次渲染位置一致，便于视觉回归比对，也不会每次打开都"跳一下"。
 * 刻意不引入 d3-force 这类物理引擎：力导向布局每次结果都不同，
 * 且几百节点的模拟会明显掉帧——对这个产品的规模是过度设计。
 */

import { useCallback, useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { getGraph, getNeighborhood } from "@/api/endpoints";
import type { GraphEdge, GraphNode, GraphView, NeighborhoodView } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { Button, Chips, InlineEmpty, PageHead, Tag } from "@/components/ui";
import { EmptyState, ErrorStateWithNav, Loading } from "@/components/States";
import { Icon } from "@/components/Icon";
import { routeForLink } from "@/lib/navigate";

/** 画布逻辑尺寸（viewBox 坐标系，SVG 自适应缩放）。 */
const W = 720;
const H = 520;

/**
 * 能力层级筛选选项。
 *
 * 🔴 取值必须与后端 `CapabilityLayer::parse` 接受的字符串**逐字一致**
 * （domain / capability / implementation）。写错一个字母，
 * 后端会返回 400「未知的能力层级」，而这个错误只会在用户点击时才暴露。
 */
const CAPABILITY_LAYERS: { value: string; label: string }[] = [
  { value: "domain", label: "领域" },
  { value: "capability", label: "能力" },
  { value: "implementation", label: "实现" },
];

export function GraphPage() {
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const [selectedId, setSelectedId] = useState<string | null>(params.get("id"));

  const layers = params.get("layers") ?? "";
  const relationTypes = params.get("relations") ?? "";
  const maxNodes = Number.parseInt(params.get("max_nodes") ?? "80", 10) || 80;

  const update = useCallback(
    (patch: Record<string, string | null>) => {
      const next = new URLSearchParams(params);
      for (const [k, v] of Object.entries(patch)) {
        if (v === null || v === "") next.delete(k);
        else next.set(k, v);
      }
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  const query = useMemo(
    () => ({
      ...(layers ? { layers: layers.split(",") } : {}),
      ...(relationTypes ? { relation_types: relationTypes.split(",") } : {}),
      max_nodes: maxNodes,
    }),
    [layers, relationTypes, maxNodes],
  );

  const { data, error, loading, reload } = useAsync<GraphView>(
    (signal) => getGraph(query, signal),
    [JSON.stringify(query)],
  );

  const select = useCallback(
    (id: string | null) => {
      setSelectedId(id);
      update({ id });
    },
    [update],
  );

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }

  const selected = data?.nodes.find((n) => n.id === selectedId) ?? data?.nodes.find((n) => n.is_center) ?? null;

  return (
    <>
      <PageHead
        title="知识图谱"
        sub="项目 · 能力 · 资产之间的关联网络。节点大小反映关联广度，颜色区分实体类型。"
        actions={
          <>
            <select
              className="chip-btn"
              value={String(maxNodes)}
              onChange={(e) => update({ max_nodes: e.target.value })}
              aria-label="最多显示节点数"
              title="限制渲染的节点数，避免大图卡死浏览器"
            >
              {[40, 80, 150, 300].map((n) => (
                <option key={n} value={String(n)}>
                  最多 {n} 节点
                </option>
              ))}
            </select>
            <Button size="sm" icon="refresh" onClick={reload} busy={loading}>
              刷新
            </Button>
          </>
        }
      />

      {/* ── 能力层级筛选 ─────────────────────────────────────
          🔴 选项是**能力层级**（domain/capability/implementation），
          不是 `data.legend`（那是 EntityKind 颜色图例：项目/资产/能力/洞察…）。

          早期版本把 legend 的 key 直接当 layers 发给后端，用户点"项目"就会发出
          `layers=project`，后端 `parse_layers` 只认三个层级值，直接返回 400
          「未知的能力层级：project」。两个概念听起来都像"分类"，实则完全不同：
          - `layers` 筛选的是**能力的抽象层级**（只作用于 capability 节点）
          - `legend` 描述的是**节点实体类型**及其配色，纯展示，不可筛选 */}
      <div className="filter-bar">
        <Chips
          options={[
            { value: "", label: "全部层级" },
            ...CAPABILITY_LAYERS.map((l) => ({ value: l.value, label: l.label })),
          ]}
          selected={layers}
          onSelect={(v) => update({ layers: v || null })}
        />
      </div>

      {data !== null && data.relation_legend.length > 1 ? (
        <div className="filter-bar">
          <Chips
            options={[
              { value: "", label: "全部关系" },
              ...data.relation_legend.map((r) => ({ value: r.relation, label: r.label, count: r.count })),
            ]}
            selected={relationTypes}
            onSelect={(v) => update({ relations: v || null })}
          />
        </div>
      ) : null}

      {loading && data === null ? <Loading rows={5} label="加载图谱" /> : null}

      {data !== null ? (
        <div
          className={`cols-graph${loading ? " is-refreshing" : ""}`}
          style={{ display: "grid", gap: 16 }}
        >
          <section className="card" style={{ padding: 8 }}>
            {data.nodes.length === 0 ? (
              <EmptyState
                icon="graph"
                title="图谱还是空的"
                // 🔴 后端 empty_hint 区分"从没生成过关系"与"筛选太严"
                message={data.empty_hint ?? "关系边由跨项目分析生成，完成扫描后这里会出现关联网络。"}
                action={
                  layers !== "" || relationTypes !== ""
                    ? { label: "清除筛选", icon: "x", onClick: () => setParams(new URLSearchParams(), { replace: true }) }
                    : { label: "去扫描", icon: "scan", onClick: () => navigate("/") }
                }
              />
            ) : (
              <>
                <GraphCanvas
                  nodes={data.nodes}
                  edges={data.edges}
                  selectedId={selected?.id ?? null}
                  onSelect={select}
                />
                {/* 图例：颜色与实体类型的对应关系，来自后端 */}
                {/* 🔴 图例只列当前视图里真实存在的类型：
                    "资产 (0)"、"知识 (0)" 这类条目是纯噪音——
                    它们既不提供配色信息（该颜色在图上一个像素都没有），
                    又让用户误以为"应该有资产节点但没显示出来"。 */}
                <div className="legend">
                  {data.legend
                    .filter((l) => l.count > 0)
                    .map((l) => (
                      <span key={l.key}>
                        <i style={{ background: l.color }} />
                        {l.label} ({l.count})
                      </span>
                    ))}
                </div>
                <div className="graph-stats">
                  <div>
                    <div className="k">节点</div>
                    <div className="v">{data.stats.node_count}</div>
                  </div>
                  <div>
                    <div className="k">关系</div>
                    <div className="v">{data.stats.edge_count}</div>
                  </div>
                </div>
              </>
            )}

            {/* 🔴 截断信息必须显示：否则用户会以为看到了全部关联 */}
            {data.stats.truncated ? (
              <div
                style={{
                  margin: "10px 8px 4px",
                  padding: "8px 12px",
                  borderRadius: "var(--r-md)",
                  background: "color-mix(in srgb, var(--color-warning) 10%, transparent)",
                  border: "1px solid color-mix(in srgb, var(--color-warning) 28%, transparent)",
                  fontSize: "var(--fs-sm)",
                  color: "var(--color-text-2)",
                }}
              >
                <Icon name="alert" /> {data.stats.summary}
                <div className="disc-tags" style={{ marginTop: 6 }}>
                  {data.stats.omitted_nodes > 0 ? <Tag mono>省略 {data.stats.omitted_nodes} 节点</Tag> : null}
                  {data.stats.omitted_edges > 0 ? <Tag mono>省略 {data.stats.omitted_edges} 边</Tag> : null}
                  {data.stats.dropped_dangling_edges > 0 ? (
                    <Tag mono>丢弃 {data.stats.dropped_dangling_edges} 悬空边</Tag>
                  ) : null}
                  {data.stats.isolated_nodes > 0 ? <Tag mono>{data.stats.isolated_nodes} 个孤立节点</Tag> : null}
                  <Tag mono>全库 {data.stats.total_capabilities} 能力 / {data.stats.total_relations} 关系</Tag>
                </div>
              </div>
            ) : null}
          </section>

          <section className="card">
            {selected === null || selected === undefined ? (
              <>
                <div className="card-head">
                  <div>
                    <div className="card-title">
                      <Icon name="graph" /> 节点详情
                    </div>
                    <div className="card-sub">点击图中任一节点查看它的关联</div>
                  </div>
                </div>
                <InlineEmpty>未选择节点。</InlineEmpty>
                <NodeListHint nodes={data.nodes} onSelect={select} />
              </>
            ) : (
              <NodeDetail node={selected} data={data} onNavigate={navigate} onSelect={select} />
            )}
          </section>
        </div>
      ) : null}
    </>
  );
}

/** 未选节点时，列出关联最广的几个，给用户一个入口（否则右侧是纯空白）。 */
function NodeListHint({ nodes, onSelect }: { nodes: GraphNode[]; onSelect: (id: string) => void }) {
  const top = [...nodes].sort((a, b) => b.degree - a.degree).slice(0, 8);
  if (top.length === 0) return null;
  return (
    <div style={{ marginTop: 12 }}>
      <div className="card-sub" style={{ marginBottom: 6 }}>
        关联最广的节点
      </div>
      {top.map((n) => (
        <button key={n.id} className="assist-item" type="button" onClick={() => onSelect(n.id)}>
          <span className="ico" style={{ color: n.color }}>
            <Icon name="graph" />
          </span>
          {n.label}
          <span className="arr" style={{ marginLeft: "auto", color: "var(--color-text-3)" }}>
            {n.degree} 关联
          </span>
        </button>
      ))}
    </div>
  );
}

function NodeDetail({
  node,
  data,
  onNavigate,
  onSelect,
}: {
  node: GraphNode;
  data: GraphView;
  onNavigate: (p: string) => void;
  onSelect: (id: string) => void;
}) {
  // 🔴 邻域来自**专门的端点**，不是当前截断视图里的边。
  // 全景图为了性能只渲染 top-N 节点，选中节点的很多真实邻居不在视图里；
  // 若只用视图内的边，用户会以为"这个节点只有 2 个关联"，
  // 而它实际可能有 20 个——截断造成的假象会被当成事实。
  const { data: hood } = useAsync<NeighborhoodView>(
    (signal) => getNeighborhood(node.entity_id, {}, signal),
    [node.entity_id],
  );
  const incident = hood?.edges ?? data.edges.filter((e) => e.source === node.id || e.target === node.id);
  const hoodNodes = hood?.nodes ?? [];
  const nodeById = useMemo(() => {
    const m = new Map<string, GraphNode>();
    for (const n of data.nodes) m.set(n.id, n);
    // 邻域端点返回的节点并入，保证视图外的邻居也有名字与颜色
    for (const n of hoodNodes) m.set(n.id, n);
    return m;
  }, [data.nodes, hoodNodes]);

  const open = useCallback(() => {
    // 🔴 映射收敛在 routeForLink：后端对 Project 实体返回 "project"（单数=详情），
    // 本地硬编码只认 "projects" 曾导致点击项目节点被 * 路由弹回首页。
    if (node.link_page === "") return;
    onNavigate(routeForLink(node.link_page, node.entity_id));
  }, [node, onNavigate]);

  return (
    <>
      <div className="card-head">
        <div>
          <div className="card-title">
            <span style={{ color: node.color }}>
              <Icon name="graph" />
            </span>
            {node.label}
          </div>
          <div className="card-sub">
            {node.kind_label}
            {node.subtitle ? ` · ${node.subtitle}` : ""}
          </div>
        </div>
        {node.link_page !== "" ? (
          <Button size="sm" icon="arr" onClick={open}>
            打开
          </Button>
        ) : null}
      </div>

      <div className="disc-tags" style={{ marginBottom: 12 }}>
        <Tag mono>度数 {node.degree}</Tag>
        {node.weight > 0 ? <Tag mono>权重 {node.weight}</Tag> : null}
        {node.is_center ? <Tag>中心节点</Tag> : null}
      </div>

      <div className="card-sub" style={{ marginBottom: 6 }}>
        关联（{incident.length}）
        {hood !== null && hood !== undefined && hood.stats.edge_count !== incident.length
          ? ` · 全图共 ${hood.stats.edge_count} 条边涉及该节点`
          : ""}
      </div>
      {incident.length === 0 ? (
        <InlineEmpty>
          该节点在当前视图里没有边。
          {data.stats.isolated_nodes > 0 ? `全图共 ${data.stats.isolated_nodes} 个孤立节点。` : ""}
        </InlineEmpty>
      ) : (
        <div className="citation-list">
          {incident.map((e) => {
            const otherId = e.source === node.id ? e.target : e.source;
            const other = nodeById.get(otherId);
            return (
              <button
                key={e.id}
                className="citation-item"
                type="button"
                onClick={() => onSelect(otherId)}
                title={other?.subtitle}
              >
                <span className="kind" style={{ color: other?.color }}>
                  <Icon name={e.bidirectional ? "repeat" : "arr"} />
                </span>
                <div className="label">
                  {other?.label ?? otherId}
                  <div className="supports">
                    {e.relation_label} · 置信 {(e.confidence * 100).toFixed(0)}%
                    {e.evidence_count > 0 ? ` · ${e.evidence_count} 条证据` : ""}
                    {e.bidirectional ? " · 双向" : ""}
                  </div>
                </div>
              </button>
            );
          })}
        </div>
      )}
    </>
  );
}

// ══════════════════════════════════════════════════════════════════
// 画布渲染
// ══════════════════════════════════════════════════════════════════

interface Positioned {
  node: GraphNode;
  x: number;
  y: number;
}

/**
 * 确定性布局：中心节点居中，其余按类型分组排成同心环。
 *
 * 🔴 不用力导向模拟：
 * 1. 力导向每次结果不同，视觉回归无法比对，用户每次打开位置都在变
 * 2. 几百节点的迭代模拟会明显掉帧
 * 3. 这个产品的价值在"看出簇与枢纽"，分层环形已经足够表达
 *
 * 排序键用 `(kind, degree desc, id)` 而非数组原序：
 * 保证同一数据集的布局稳定，且同类型节点相邻（视觉上自然成簇）。
 */
function layout(nodes: GraphNode[], centerId: string | null): Positioned[] {
  if (nodes.length === 0) return [];

  const center = nodes.find((n) => n.id === centerId || n.is_center) ?? null;
  const rest = nodes
    .filter((n) => center === null || n.id !== center.id)
    .slice()
    .sort((a, b) =>
      a.kind === b.kind
        ? b.degree - a.degree || a.id.localeCompare(b.id)
        : a.kind.localeCompare(b.kind),
    );

  const out: Positioned[] = [];
  const cx = W / 2;
  const cy = H / 2;

  if (center !== null) {
    out.push({ node: center, x: cx, y: cy });
  }

  // 按类型分段，每段占圆环的一部分（同类相邻 → 自然成簇）
  const n = rest.length;
  if (n > 0) {
    // 半径随节点数增长，避免节点重叠；上限留出标签空间
    const radius = Math.min(W, H) / 2 - 70;
    const innerR = radius * (n > 24 ? 0.55 : 0.72);
    rest.forEach((node, i) => {
      const angle = (2 * Math.PI * i) / n - Math.PI / 2;
      // 节点多时分两圈，减少拥挤
      const r = n > 24 && i % 2 === 1 ? radius : innerR + (radius - innerR) * 0.55;
      out.push({
        node,
        x: cx + r * Math.cos(angle),
        y: cy + r * Math.sin(angle),
      });
    });
  }

  return out;
}

function GraphCanvas({
  nodes,
  edges,
  selectedId,
  onSelect,
}: {
  nodes: GraphNode[];
  edges: GraphEdge[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}) {
  const positioned = useMemo(() => layout(nodes, selectedId), [nodes, selectedId]);
  const posOf = useMemo(() => {
    const m = new Map<string, Positioned>();
    for (const p of positioned) m.set(p.node.id, p);
    return m;
  }, [positioned]);

  return (
    <svg
      className="graph-svg"
      viewBox={`0 0 ${W} ${H}`}
      role="img"
      aria-label={`知识图谱，${nodes.length} 个节点，${edges.length} 条关系`}
      style={{ width: "100%", height: "auto", display: "block" }}
    >
      <defs>
        <filter id="node-glow" x="-60%" y="-60%" width="220%" height="220%">
          <feGaussianBlur stdDeviation="5" result="b" />
          <feMerge>
            <feMergeNode in="b" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
      </defs>

      {/* 边：先画，让节点盖在上面 */}
      {edges.map((e) => {
        const a = posOf.get(e.source);
        const b = posOf.get(e.target);
        // 🔴 理论上不会发生（后端已过滤悬空边），但防御性跳过：
        // 画一条通向 undefined 的线会让整张图渲染异常
        if (a === undefined || b === undefined) return null;
        const active = selectedId !== null && (e.source === selectedId || e.target === selectedId);
        return (
          <line
            key={e.id}
            x1={a.x}
            y1={a.y}
            x2={b.x}
            y2={b.y}
            stroke={active ? e.color : "var(--color-graph-edge)"}
            strokeWidth={active ? 1.8 : 1}
            strokeDasharray={e.bidirectional ? undefined : "4 3"}
            opacity={active ? 0.95 : 0.7}
          >
            <title>
              {e.relation_label} · 置信 {(e.confidence * 100).toFixed(0)}%
              {e.evidence_count > 0 ? ` · ${e.evidence_count} 条证据` : ""}
            </title>
          </line>
        );
      })}

      {/* 节点 */}
      {positioned.map(({ node, x, y }) => {
        const isSelected = node.id === selectedId;
        const r = Math.max(6, node.size / 2.2);
        return (
          <g
            key={node.id}
            style={{ cursor: "pointer" }}
            onClick={() => onSelect(node.id)}
            role="button"
            tabIndex={0}
            aria-label={`${node.kind_label}：${node.label}，${node.degree} 个关联`}
            onKeyDown={(ev) => {
              if (ev.key === "Enter" || ev.key === " ") {
                ev.preventDefault();
                onSelect(node.id);
              }
            }}
          >
            <title>{`${node.label}\n${node.kind_label}${node.subtitle ? ` · ${node.subtitle}` : ""}\n关联 ${node.degree}`}</title>
            <circle cx={x} cy={y} r={r + 6} fill={node.color} opacity={isSelected ? 0.3 : 0.12} />
            <circle
              cx={x}
              cy={y}
              r={r}
              fill={`${node.color}26`}
              stroke={node.color}
              strokeWidth={isSelected ? 2.4 : 1.4}
              filter="url(#node-glow)"
            />
            <circle cx={x} cy={y} r={Math.max(3, r * 0.34)} fill={node.color} />
            {/* 🔴 标签规则：节点少时全部标注，节点多才按度数收敛。
                早期版本固定用 `degree >= 3`，结果一张 10 节点的图里
                8 个能力节点（度数 1-2）全是无标签的色点——
                图谱的价值恰恰在"看出哪个节点是什么"，无标签等于没画。
                大图（>40 节点）才需要收敛，否则标签互相覆盖糊成一片。 */}
            {isSelected || node.is_center || node.degree >= 3 || positioned.length <= 40 ? (
              <text
                x={x}
                y={y + r + 14}
                textAnchor="middle"
                fill="var(--color-graph-label)"
                fontSize="11"
                fontFamily="Inter, PingFang SC, sans-serif"
                style={{ pointerEvents: "none" }}
              >
                {truncate(node.label, 12)}
              </text>
            ) : null}
          </g>
        );
      })}
    </svg>
  );
}

/** 标签截断（SVG text 不会自动省略，过长会盖住相邻节点）。 */
function truncate(s: string, max: number): string {
  const chars = [...s];
  return chars.length > max ? `${chars.slice(0, max - 1).join("")}…` : s;
}
