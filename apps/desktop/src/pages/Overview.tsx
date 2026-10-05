/**
 * 首页（My R&D）。
 *
 * # 🔴 产品原则：界面简单但内心强大
 * 首屏只回答三个问题：
 * 1. 我现在有什么（统计卡，真实数字）
 * 2. 我该做什么（onboarding 引导 / 扫描入口）
 * 3. 系统替我发现了什么（AI 洞察 + 机会）
 *
 * 不堆功能入口、不放装饰性图表。深度能力藏在各页面里，首页只做导航与状态。
 *
 * # 🔴 与原型的关键差异：所有数字都来自后端
 * 原型这一页写死了：
 * - `o.greeting` / `o.summary`（编造的问候语与总结）
 * - statCard 的 `↑ {delta} 本月`（**后端根本没有 delta 字段**，是纯编造的增长数据）
 * - 「扫描 3 个目录 · 上次扫描 2 小时前」
 * - 图谱预览的节点坐标、chips「全部/AI/Web/数据…」
 * - AI 助手徽标「在线」
 * - promo「已支持 Cursor、Claude Code、Codex」（MCP 从未实现）
 *
 * 这些全部换成真实数据；后端没给的（如月度增量）就**不显示**，
 * 而不是编一个看起来合理的数字——假指标一旦被用户当真，
 * 他会基于错误信息做决策，这比没有指标更有害。
 */

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { getOverview, startIndex, startScan } from "@/api/endpoints";
import type { Overview, StatCard as StatCardData } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { useApp } from "@/lib/AppContext";
import { useProgressState } from "@/lib/ProgressContext";
import { useAddDirs } from "@/lib/useAddDirs";
import { useToast } from "@/components/Toast";
import { DirPickerModal } from "@/components/DirPickerModal";
import { Button, Card, InlineEmpty, MoreLink, ProgressBar, Tag, ValueBadge } from "@/components/ui";
import { EmptyState, ErrorStateWithNav, Loading } from "@/components/States";
import { Icon, type IconName } from "@/components/Icon";
import { progressCounter, progressLabel, progressPercent } from "@/lib/useProgress";
import { routeForLink } from "@/lib/navigate";
import { formatRelativeWithAbsolute, timeAgo } from "@/lib/format";

export interface OverviewPageProps {
  /** 扫描提交后调用（刷新全局统计） */
  onScanStart: () => void;
}

/** 统计卡 key → 图标与配色。纯展示映射，不含业务判断。 */
const STAT_VISUAL: Record<string, { icon: IconName; color: string }> = {
  projects: { icon: "folder", color: "#8b5cf6" },
  assets: { icon: "box", color: "#3b82f6" },
  capabilities: { icon: "graph", color: "#a855f7" },
  knowledge: { icon: "book", color: "#eab308" },
  insights: { icon: "drop", color: "#22c55e" },
  opportunities: { icon: "bulb", color: "#06b6d4" },
  relations: { icon: "flow", color: "#14b8a6" },
  skills: { icon: "skill", color: "#ec4899" },
};

export function OverviewPage({ onScanStart }: OverviewPageProps) {
  const navigate = useNavigate();
  const toast = useToast();
  const { bumpHealth } = useApp();
  // 🔴 进度从 context 读（理由见 ProgressContext）：
  // 走 props 会让 SSE 每帧都重建 App 的整棵路由树。
  const progress = useProgressState();
  const [scanning, setScanning] = useState(false);
  const [indexing, setIndexing] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);

  const { data, error, loading, reload } = useAsync<Overview>(
    (signal) => getOverview(signal),
    [],
  );

  // 🔴 任务进入终态时自动刷新首页。
  // 扫描→索引→洞察是链式流水线，跑完后洞察与资产会凭空出现，
  // 若首页不刷新，用户看到的就是"进度条结束了，但数字一个没变"——
  // 那会让人以为刚才那一整轮白跑了。
  const jobStatus = progress.event?.status;
  useEffect(() => {
    if (jobStatus === "completed" || jobStatus === "failed" || jobStatus === "cancelled") {
      reload();
      bumpHealth();
    }
    // 只在状态**变为**终态时触发；reload/bumpHealth 都是稳定引用
  }, [jobStatus, reload, bumpHealth]);

  const handleScan = useCallback(async () => {
    setScanning(true);
    try {
      const r = await startScan({});
      // 🔴 原样显示后端 message：它预告了链式续跑
      // （"已开始扫描 N 个目录，完成后将自动索引并生成洞察"）。
      // 这句预告是用户理解"为什么几分钟后洞察自己冒出来"的唯一线索，
      // 换成前端自己写的"扫描已开始"会丢掉关键信息。
      toast.success(r.message, "进度见左下角，可在「任务」页查看详情");
      onScanStart();
      bumpHealth();
    } catch (err) {
      const msg = err instanceof Error ? err.message : "扫描启动失败";
      const hint = err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
      toast.error(msg, hint || undefined);
    } finally {
      setScanning(false);
    }
  }, [toast, onScanStart, bumpHealth]);

  /** 补跑索引（onboarding 第 3 步直达：扫描完成但资产为空时）。 */
  const handleIndex = useCallback(async () => {
    setIndexing(true);
    try {
      const r = await startIndex();
      toast.success(r.message, "进度见左下角");
      onScanStart();
      bumpHealth();
    } catch (err) {
      const msg = err instanceof Error ? err.message : "索引启动失败";
      const hint = err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
      toast.error(msg, hint || undefined);
    } finally {
      setIndexing(false);
    }
  }, [toast, onScanStart, bumpHealth]);

  /** onboarding / 扫描卡共用的目录选择回调（逻辑见 lib/useAddDirs）。 */
  const handlePick = useAddDirs({
    onChanged: () => reload(),
    onSuccess: (msg) => toast.success(msg),
  });

  /**
   * onboarding 步骤按钮：优先走 `action_key` 直达动作，
   * 没有才回退到 `action_page` 跳转。
   * 🔴 直达的意义：新用户的第一步是"添加目录"，跳去设置页后还要
   * 自己找到输入框、粘贴路径——每多一步都流失一批人。
   */
  const runOnboardingAction = useCallback(
    (step: { action_key: string | null; action_page: string | null }) => {
      switch (step.action_key) {
        case "pick_dirs":
          setPickerOpen(true);
          return;
        case "start_scan":
          void handleScan();
          return;
        case "start_index":
          void handleIndex();
          return;
        default:
          navigate(step.action_page !== null ? routeForLink(step.action_page, null) : "/");
      }
    },
    [handleScan, handleIndex, navigate],
  );

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }
  if (loading && data === null) {
    return <Loading rows={6} label="加载首页数据" />;
  }
  if (data === null) {
    return <EmptyState message="首页数据尚未就绪。" action={{ label: "重试", onClick: reload }} />;
  }

  const scanRunning =
    progress.event !== null &&
    (progress.event.status === "running" || progress.event.status === "queued");

  return (
    <div className={`shell-2${loading ? " is-refreshing" : ""}`} style={{ display: "grid", gap: 16, alignItems: "start" }}>
      <div>
        {/* ── 概览卡：真实统计 ─────────────────────────────── */}
        <section
          className="card hero-grid"
          style={{
            background: "var(--grad-hero)",
            borderColor: "rgba(139,92,246,.35)",
            display: "grid",
            gap: 16,
            overflow: "hidden",
          }}
        >
          <div style={{ padding: "8px 0" }}>
            <h1 style={{ fontSize: "var(--fs-2xl)", fontWeight: 700, whiteSpace: "nowrap" }}>
              我的研发资产
            </h1>
            <p
              style={{
                color: "var(--color-hero-text)",
                marginTop: 10,
                fontSize: "var(--fs-md)",
                maxWidth: 340,
                lineHeight: "var(--lh-base)",
              }}
            >
              {/* 🔴 描述由真实数据拼成，不用编造的问候语。
                  "上次扫描"来自后端；从未扫描过时明确说明，
                  而不是显示"2 小时前"这种假时间。 */}
              {data.last_scanned_at
                ? // 🔴 不直接显示原始 ISO 纳秒串（见 format.ts）
                  `已索引你本机的代码项目，从中提取可复用的资产与能力。上次扫描：${formatRelativeWithAbsolute(data.last_scanned_at)}`
                : "还没有扫描过任何目录。添加扫描目录并开始扫描，系统会从你的真实代码中提取可复用资产。"}
            </p>
          </div>
          {data.stats.map((s) => (
            <StatCardView key={s.key} stat={s} onNavigate={navigate} />
          ))}
        </section>

        {/* ── 扫描入口 ─────────────────────────────────────── */}
        <section className="card" style={{ marginTop: 16, padding: "12px 16px" }}>
          {scanRunning && progress.event !== null ? (
            <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
              <span style={{ color: "var(--color-primary-2)", flex: "none" }}>
                <Icon name="scan" />
              </span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div
                  style={{
                    display: "flex",
                    justifyContent: "space-between",
                    fontSize: "var(--fs-sm)",
                    marginBottom: 6,
                    gap: 8,
                  }}
                >
                  <span>{progressLabel(progress.event)}</span>
                  <span className="mono">
                    {progressPercent(progress.event)}%
                    {progressCounter(progress.event)
                      ? ` · ${progressCounter(progress.event)}`
                      : ""}
                  </span>
                </div>
                <ProgressBar percent={progressPercent(progress.event)} active />
              </div>
              <Button size="sm" onClick={() => navigate("/jobs")} icon="cpu">
                任务详情
              </Button>
            </div>
          ) : (
            <div style={{ display: "flex", alignItems: "center", gap: 14, flexWrap: "wrap" }}>
              <span
                style={{
                  width: 34,
                  height: 34,
                  borderRadius: 9,
                  background: "rgba(99,102,241,.16)",
                  color: "var(--color-primary-2)",
                  display: "grid",
                  placeItems: "center",
                  flex: "none",
                }}
              >
                <Icon name="scan" />
              </span>
              <div style={{ flex: 1, minWidth: 200 }}>
                <div style={{ fontSize: "var(--fs-md)", fontWeight: 600 }}>项目扫描</div>
                <div style={{ fontSize: "var(--fs-xs)", color: "var(--color-text-3)" }}>
                  {/* 🔴 目录数与上次时间都来自后端，不写死"3 个目录 · 2 小时前" */}
                  扫描设置里已启用的目录 · 完成后自动索引并生成洞察
                  {data.last_scanned_at ? ` · 上次：${timeAgo(data.last_scanned_at)}` : ""}
                </div>
              </div>
              <Button size="sm" icon="folder" onClick={() => setPickerOpen(true)}>
                选择目录
              </Button>
              <Button
                variant="primary"
                size="sm"
                icon="refresh"
                busy={scanning}
                onClick={() => void handleScan()}
              >
                {scanning ? "提交中…" : "开始扫描"}
              </Button>
            </div>
          )}
        </section>

        {/* ── 首次使用引导（后端驱动，全部完成后自动消失）──── */}
        {data.onboarding !== null ? (
          <section className="onboarding" style={{ marginTop: 16 }}>
            <h2>
              <Icon name="spark" /> {data.onboarding.headline}
            </h2>
            {data.onboarding.steps.map((step, i) => (
              <div className={`onboarding-step${step.done ? " is-done" : ""}`} key={step.title}>
                <div className="num">{step.done ? "✓" : i + 1}</div>
                <div className="body">
                  <div className="title">{step.title}</div>
                  <div className="detail">{step.detail}</div>
                </div>
                {step.action_page !== null && !step.done ? (
                  <Button
                    size="sm"
                    onClick={() => runOnboardingAction(step)}
                    busy={
                      (step.action_key === "start_scan" && scanning) ||
                      (step.action_key === "start_index" && indexing)
                    }
                  >
                    {step.action_key === "pick_dirs"
                      ? "选择目录"
                      : step.action_key === "start_scan"
                        ? "开始扫描"
                        : step.action_key === "start_index"
                          ? "开始索引"
                          : "去完成"}
                  </Button>
                ) : null}
              </div>
            ))}
          </section>
        ) : null}

        {/* ── AI 发现 + 图谱概览 ───────────────────────────── */}
        <div className="grid cols-2" style={{ marginTop: 16 }}>
          <Card
            icon="spark"
            title="AI 发现"
            sub="基于你的历史项目算出的结论，每条都带可核查的证据"
            action={<MoreLink label="全部洞察" onClick={() => navigate("/insights")} />}
          >
            {data.recent_insights.length === 0 ? (
              <InlineEmpty>
                还没有洞察。洞察由跨项目分析产生——完成一次扫描（含索引与洞察生成）后，
                系统会找出重复实现、可复用组件与遗忘的资产。
              </InlineEmpty>
            ) : (
              data.recent_insights.map((ins) => (
                <button
                  key={ins.id}
                  className="disc-item"
                  onClick={() => navigate(`/insights?id=${encodeURIComponent(ins.id)}`)}
                  type="button"
                >
                  <div className="disc-ico" style={{ background: "rgba(99,102,241,.16)" }}>
                    <Icon name="drop" />
                  </div>
                  <div className="disc-body">
                    <div className="disc-title-row">
                      <span className="disc-title">{ins.title}</span>
                      {/* 🔴 badge 文案与分档都由后端给（与洞察页同源） */}
                      <ValueBadge label={ins.badge} />
                    </div>
                    <div className="disc-desc">{ins.summary}</div>
                    <div className="disc-tags">
                      <Tag>{ins.type_label}</Tag>
                      <Tag mono>{Math.round(ins.confidence * 100)}%</Tag>
                    </div>
                  </div>
                  <span className="disc-arrow">
                    <Icon name="chev" />
                  </span>
                </button>
              ))
            )}
          </Card>

          <Card
            icon="graph"
            title="研发能力图谱"
            action={<MoreLink label="查看完整图谱" onClick={() => navigate("/graph")} />}
          >
            <div className="graph-stats" style={{ marginBottom: 10 }}>
              <div>
                <div className="k">节点</div>
                <div className="v">{data.graph_preview.node_count}</div>
              </div>
              <div>
                <div className="k">关系</div>
                <div className="v">{data.graph_preview.edge_count}</div>
              </div>
            </div>
            {data.graph_preview.top_capabilities.length === 0 ? (
              <InlineEmpty>
                图谱还没有节点。能力与关系由索引阶段抽取，完成扫描后这里会显示关联最广的能力。
              </InlineEmpty>
            ) : (
              <>
                <div className="card-sub" style={{ marginBottom: 8 }}>
                  关联项目最多的能力
                </div>
                {data.graph_preview.top_capabilities.map((c) => (
                  <div className="cap-row" key={c.name}>
                    <span>{c.name}</span>
                    <span className="bar">
                      <i
                        style={{
                          width: `${barPercent(c.count, data.graph_preview.top_capabilities)}`,
                        }}
                      />
                    </span>
                    <span className="pct">{c.count} 项目</span>
                  </div>
                ))}
              </>
            )}
          </Card>
        </div>

        {/* ── 机会 ─────────────────────────────────────────── */}
        {data.top_opportunities.length > 0 ? (
          <Card
            icon="bulb"
            title="组合机会"
            sub="把已有资产拼成新项目，覆盖度越高说明越少从零写"
            style={{ marginTop: 16 }}
            action={<MoreLink label="全部机会" onClick={() => navigate("/opportunities")} />}
          >
            <div className="grid cols-opp">
              {data.top_opportunities.map((o) => (
                <button
                  key={o.id}
                  className="asset-card"
                  type="button"
                  onClick={() => navigate(`/opportunities?id=${encodeURIComponent(o.id)}`)}
                  style={{ textAlign: "left" }}
                >
                  <div className="head">
                    <div className="name">{o.title}</div>
                    <span className="badge badge--potent">{o.rating}★</span>
                  </div>
                  <div className="type">
                    覆盖度 {Math.round(o.coverage * 100)}% · 可复用 {o.reusable_count} 项 · 待建{" "}
                    {o.missing_count} 项
                  </div>
                  <div className="desc">{o.description}</div>
                </button>
              ))}
            </div>
          </Card>
        ) : null}

        {/* ── 快速入口 ─────────────────────────────────────── */}
        <Card icon="grid" title="快速入口" style={{ marginTop: 16 }}>
          <div className="quick">
            <QuickItem
              icon="folder"
              color="#8b5cf6"
              title="我的项目"
              desc="浏览所有已扫描的项目"
              onClick={() => navigate("/projects")}
            />
            <QuickItem
              icon="search"
              color="#22c55e"
              title="资产搜索"
              desc="查找可复用的代码、组件、方案"
              onClick={() => navigate("/assets")}
            />
            <QuickItem
              icon="spark"
              color="#6366f1"
              title="AI 分析师"
              desc="用自然语言提问你的研发历史"
              onClick={() => navigate("/analyst")}
            />
            <QuickItem
              icon="bulb"
              color="#f59e0b"
              title="组合机会"
              desc="看看已有资产能拼出什么新项目"
              onClick={() => navigate("/opportunities")}
            />
          </div>
        </Card>
      </div>

      {/* ── 右侧栏：活动流 ─────────────────────────────────── */}
      <aside className="rail">
        <Card icon="spark" title="AI 分析师">
          <div
            className="assist-item"
            style={{
              background: "rgba(99,102,241,.14)",
              borderColor: "rgba(99,102,241,.4)",
              color: "var(--color-tree-active)",
            }}
          >
            <span className="ico">
              <Icon name="spark" />
            </span>
            我可以帮你：
          </div>
          {ANALYST_PRESETS.map((a) => (
            <button
              key={a.text}
              className="assist-item"
              type="button"
              onClick={() => navigate(`/analyst?q=${encodeURIComponent(a.text)}`)}
            >
              <span className="ico">
                <Icon name={a.icon} />
              </span>
              {a.text}
              <span className="arr">
                <Icon name="chev" />
              </span>
            </button>
          ))}
          <div className="assist-hint">
            未配置模型时会走离线检索式回答：不调用大模型，只从已索引的真实数据里检索并标注出处。
          </div>
        </Card>

        <Card
          icon="clock"
          title="最近活动"
          action={<MoreLink label="任务记录" onClick={() => navigate("/jobs")} />}
        >
          {data.activities.length === 0 ? (
            <InlineEmpty>还没有活动记录。扫描、索引、生成洞察都会记录在这里。</InlineEmpty>
          ) : (
            data.activities.map((a) => (
              <div className="activity-item" key={a.id}>
                <div className="activity-ico" style={{ background: "var(--color-panel-3)" }}>
                  <Icon name={iconOf(a.icon)} />
                </div>
                <div>
                  <div className="activity-t">{a.title}</div>
                  <div className="activity-d">
                    {a.detail} · {a.when}
                  </div>
                </div>
              </div>
            ))
          )}
        </Card>

        {/* 🔴 删掉了原型的 promo 卡：
            它宣称"已支持 Cursor、Claude Code、Codex"并链到 MCP 页，
            而 MCP 从未实现（属阶段三）。展示不存在的能力是最糟的一种假数据。 */}
      </aside>

      {/* 目录选择弹窗：onboarding 第一步与扫描卡共用（免跳转设置页） */}
      {pickerOpen ? <DirPickerModal onClose={() => setPickerOpen(false)} onPick={handlePick} /> : null}
    </div>
  );
}

function StatCardView({ stat, onNavigate }: { stat: StatCardData; onNavigate: (p: string) => void }) {
  const visual = STAT_VISUAL[stat.key] ?? { icon: "doc" as IconName, color: "#64748b" };
  const clickable = stat.link_page !== "";
  return (
    <button
      className="stat"
      type="button"
      onClick={clickable ? () => onNavigate(routeForLink(stat.link_page, null)) : undefined}
      disabled={!clickable}
      style={{ textAlign: "left" }}
      title={clickable ? `查看${stat.label}` : undefined}
    >
      <div
        className="ico-box"
        style={{ background: `${visual.color}22`, color: visual.color }}
      >
        <Icon name={visual.icon} />
      </div>
      <div className="label">{stat.label}</div>
      <div className="value">{stat.value.toLocaleString()}</div>
      {/* 🔴 只显示后端真实给的 detail，不编造"↑ N% 本月"这种增长数据 */}
      <div className="delta">
        {stat.detail ?? (stat.value === 0 && !stat.empty_is_expected ? "待生成" : "")}
      </div>
    </button>
  );
}

function QuickItem({
  icon,
  color,
  title,
  desc,
  onClick,
}: {
  icon: IconName;
  color: string;
  title: string;
  desc: string;
  onClick: () => void;
}) {
  return (
    <button className="quick-item" type="button" onClick={onClick}>
      <div className="ico-box" style={{ background: `${color}26`, color }}>
        <Icon name={icon} />
      </div>
      <div>
        <div className="t">{title}</div>
        <div className="d">{desc}</div>
      </div>
    </button>
  );
}

/** 能力条的宽度：按最大值归一，避免全部条目都顶满看不出差异。 */
function barPercent(count: number, all: { count: number }[]): string {
  const max = Math.max(1, ...all.map((c) => c.count));
  return `${Math.round((count / max) * 100)}%`;
}

/** 后端活动图标名 → 前端图标名。未知图标回落到 doc，不抛错。 */
function iconOf(name: string): IconName {
  const known: IconName[] = [
    "scan",
    "refresh",
    "box",
    "drop",
    "spark",
    "check",
    "alert",
    "gear",
    "folder",
    "graph",
    "cpu",
    "doc",
    "bulb",
  ];
  return (known as string[]).includes(name) ? (name as IconName) : "doc";
}

/**
 * 分析师推荐问题。
 *
 * 🔴 这些是**真实可答**的问题（都能从已索引数据检索到内容），
 * 不是原型的装饰性文案。原型的 `assistant_actions` 里有
 * "帮我评估这个项目的技术债"这类当前检索能力答不好的问题，
 * 用户点了得到"没有找到相关记录"会直接失去信任。
 */
const ANALYST_PRESETS: { icon: IconName; text: string }[] = [
  { icon: "repeat", text: "我有哪些重复实现的代码？" },
  { icon: "box", text: "哪些资产可以直接复用到新项目？" },
  { icon: "clock", text: "我有哪些项目很久没动了，还值得打捞吗？" },
  { icon: "bulb", text: "我具备的能力可以组合出什么新项目？" },
];
