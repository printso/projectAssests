/**
 * 侧栏：品牌 + 导航 + 任务进度卡。
 *
 * # 🔴 导航计数：真实数据，拉不到就不显示
 * 原型是 `count: String(S.projects)`，其中 `S = MOCK.scale`
 * （`projects: 128, assets: 1284, capabilities: 47, knowledge: 312`）——
 * 全是写死的假数字，与库里实际有什么毫无关系。
 *
 * 这里计数来自 `/api/health` 的真实统计（`useNavCounts`）。
 * **服务离线或统计未就绪时传 null，对应徽标整个不渲染**：
 * 显示 `0` 会被误读成"库里真的空了"，显示旧值则是过期数据，
 * 两者都不如不显示诚实。
 *
 * # 🔴 进度卡：真实 SSE，不是假动画
 * 原型这里是
 * `<span>本地索引中…</span><span>68%</span>` + `<i style="width:68%">` +
 * 「正在分析: 3 个项目 / 预计剩余: 2 分钟」——**全部硬编码**，
 * 不管后台有没有任务都永远显示"索引中 68%"。
 *
 * 现在：
 * - 无任务时显示数据库真实状态（项目数 / 库大小 / 最后扫描时间）
 * - 有任务时显示 SSE 推来的真实进度、阶段文案、计数
 * - 失败时显示真实错误并可跳转任务页
 */

import { NavLink } from "react-router-dom";
import { useApp, useNavCounts, type ServiceStatus } from "@/lib/AppContext";
import { useProgressState } from "@/lib/ProgressContext";
import {
  progressCounter,
  progressLabel,
  progressPercent,
  type ProgressState,
} from "@/lib/useProgress";
import { Icon } from "./Icon";
import { NAV } from "@/config";
import type { HealthView } from "@/api/types";

export interface SidebarProps {
  /** 全局统计（离线时为 null） */
  health: HealthView | null;
  onNavigate: (path: string) => void;
}

/**
 * 🔴 进度从 ProgressContext 读，不走 props。
 * 走 props 的话，SSE 每帧都会让 App 的 shell useMemo 失效、重建整棵路由树；
 * 走 context 则重渲染范围收窄到本组件（见 ProgressContext 的说明）。
 */
export function Sidebar({ health, onNavigate }: SidebarProps) {
  const counts = useNavCounts();
  const progress = useProgressState();

  return (
    <aside className="sidebar">
      <div className="brand">
        <div className="brand-logo">S</div>
        <div>
          <div className="brand-name">projectAssests</div>
          <div className="brand-sub">Your Personal R&amp;D OS</div>
        </div>
      </div>

      {NAV.map((group, gi) => (
        <div className="nav-group" key={gi}>
          {group.group ? <div className="nav-label">{group.group}</div> : null}
          {group.items.map((item) => {
            const count = item.countKey ? counts[item.countKey] : null;
            const disabled = item.pending !== undefined;
            return (
              <NavItem
                key={item.path}
                path={item.path}
                icon={item.icon}
                label={item.label}
                count={count}
                disabled={disabled}
                pendingHint={item.pending}
                onNavigate={onNavigate}
              />
            );
          })}
        </div>
      ))}

      <div className="sidebar-foot">
        <ProgressCard progress={progress} health={health} onNavigate={onNavigate} />
        <div className="slogan">
          「让过去的每一个项目，
          <br />
          都成为你未来的可能性」
        </div>
      </div>
    </aside>
  );
}

interface NavItemProps {
  path: string;
  icon: string;
  label: string;
  count: number | null;
  disabled: boolean;
  pendingHint?: string;
  onNavigate: (path: string) => void;
}

function NavItem({ path, icon, label, count, disabled, pendingHint, onNavigate }: NavItemProps) {
  const iconEl = <Icon name={icon as never} />;

  // 🔴 未实现的项（MCP）用 button + disabled，而不是渲染成链接后跳转到空页：
  // 渲染成可点链接却什么都没有，比明确禁用更让人困惑。
  if (disabled) {
    return (
      <button className="nav-item" disabled title={pendingHint}>
        {iconEl}
        <span className="txt">{label}</span>
        <span className="badge-dot" style={{ color: "var(--color-text-3)" }}>
          阶段三
        </span>
      </button>
    );
  }

  return (
    <NavLink
      to={path}
      end={path === "/"}
      className={({ isActive }) => `nav-item${isActive ? " is-active" : ""}`}
      onClick={() => onNavigate(path)}
    >
      {iconEl}
      <span className="txt">{label}</span>
      {/* 🔴 count 为 null 时整个徽标不渲染（见文件头说明） */}
      {count !== null ? (
        <span className="count">{count >= 1000 ? count.toLocaleString() : String(count)}</span>
      ) : null}
    </NavLink>
  );
}

function ProgressCard({
  progress,
  health,
  onNavigate,
}: {
  progress: ProgressState;
  health: HealthView | null;
  onNavigate: (path: string) => void;
}) {
  // 🔴 useApp 必须在所有 early return 之前调用：本组件下方有多个
  // `if (...) return <...>` 分支，把 hook 放在其后会改变每次渲染的 hooks 数量，
  // 触发 React "Rendered fewer hooks" 崩溃（正是 Assets 页刚修掉的同类 bug）。
  const { status, recheck } = useApp();
  const ev = progress.event;
  const isActive = ev !== null && (ev.status === "running" || ev.status === "queued");
  const isFailed = ev !== null && ev.status === "failed";
  const percent = progressPercent(ev);

  // ── 任务进行中：显示真实进度 ────────────────────────────────
  if (isActive) {
    const counter = progressCounter(ev);
    return (
      <div className="index-card">
        <div className="row">
          <span>
            <Icon name="refresh" /> {progressLabel(ev)}
          </span>
          <span>{percent}%</span>
        </div>
        <div className="progress is-active">
          <i style={{ width: `${percent}%` }} />
        </div>
        <div className="meta">
          {/* 🔴 阶段文案来自后端 `stage`，不在前端按百分比区间猜：
              猜出来的文案会出现"进度 70% 却显示扫描中"的错位。 */}
          {ev?.job_type ? jobTypeLabel(ev.job_type) : "任务"}
          {counter ? ` · ${counter}` : ""}
        </div>
      </div>
    );
  }

  // ── 任务失败：显示真实错误 + 跳转 ──────────────────────────
  if (isFailed) {
    return (
      <div className="index-card" style={{ borderColor: "var(--color-danger)" }}>
        <div className="row">
          <span style={{ color: "var(--color-danger)" }}>
            <Icon name="alert" /> 任务失败
          </span>
          <button
            className="link-more"
            onClick={() => onNavigate("/jobs")}
            style={{ background: "none", border: "none", cursor: "pointer" }}
          >
            详情
          </button>
        </div>
        <div className="meta" style={{ marginTop: 6, color: "var(--color-text-2)" }}>
          {ev?.error ?? "未知错误"}
        </div>
      </div>
    );
  }

  // ── 空闲：显示数据库真实状态 ────────────────────────────────
  if (health === null) {
    return <OfflineCard status={status} onRetry={recheck} />;
  }

  const stats = health.stats;
  return (
    <div className="index-card">
      <div className="row">
        <span>
          <Icon name="db" /> 本地索引
        </span>
        {/* 🔴 显示 FTS5 是否可用——这是 health 端点**真实返回**的信息。
            早期版本这里写的是 schema 版本号，但 health 并不返回它，
            只能显示占位符 "—"，那是个没有任何信息量的假字段。
            FTS 不可用是用户真需要知道的：它意味着搜索会降级到 LIKE 子串匹配，
            召回质量明显下降。 */}
        <span style={{ color: health.fts_available ? "var(--color-success)" : "var(--color-warning)" }}>
          {health.fts_available ? "检索就绪" : "检索降级"}
        </span>
      </div>
      <div className="meta" style={{ marginTop: 6 }}>
        {stats.projects} 个项目 · {stats.assets} 个资产
        <br />
        {stats.insights} 条洞察 · {formatBytes(stats.size_bytes)}
      </div>
    </div>
  );
}

/**
 * 服务离线/连接中的状态卡。
 *
 * 🔴 诚实性：后端离线时 AppContext **已在每 3 秒自动轮询重连**
 * （见 OFFLINE_POLL_MS），用户启动服务后前端会自动恢复、无需手动刷新。
 * 旧版只显示一句"本地服务未响应"，把这个自动重连藏了起来——
 * 用户不知道系统在帮他重试，往往会去猛刷页面（反而更慢）。
 * 这里如实说明"正在自动重连"，并给一个"立即重试"用于不想等的场景。
 */
function OfflineCard({ status, onRetry }: { status: ServiceStatus; onRetry: () => void }) {
  const checking = status === "checking";
  return (
    <div className="index-card">
      <div className="row">
        <span style={{ color: "var(--color-danger)" }}>
          <Icon name={checking ? "refresh" : "alert"} /> {checking ? "连接中" : "未连接"}
        </span>
      </div>
      <div className="meta" style={{ marginTop: 6 }}>
        {checking
          ? "正在连接本地服务…"
          : "本地服务未响应，正在自动重连。请确认已启动 projectassests-server。"}
      </div>
      {checking ? null : (
        <button
          type="button"
          className="link-more"
          onClick={onRetry}
          style={{ background: "none", border: "none", cursor: "pointer", marginTop: 6, padding: 0 }}
        >
          立即重试
        </button>
      )}
    </div>
  );
}

/** 任务类型的中文标签。后端 JobView 有 `job_type_label`，但 SSE 事件只给 `job_type`。 */
function jobTypeLabel(t: string): string {
  switch (t) {
    case "SCAN_PROJECT":
      return "扫描项目";
    case "INDEX_CODE":
      return "索引代码";
    case "GENERATE_INSIGHT":
      return "生成洞察";
    case "PARSE_AST":
      return "解析语法树";
    case "BUILD_SYMBOL_GRAPH":
      return "构建符号图";
    case "GENERATE_EMBEDDING":
      return "生成向量";
    case "ANALYZE_PROJECT":
      return "分析项目";
    case "EXTRACT_ASSETS":
      return "抽取资产";
    case "EXTRACT_CAPABILITIES":
      return "抽取能力";
    case "ANALYZE_RELATIONS":
      return "分析关系";
    case "DISCOVER_OPPORTUNITY":
      return "发现机会";
    default:
      return t;
  }
}

/** 字节数格式化。与后端 `size_display` 同口径（B/KB/MB/GB）。 */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${units[i]}`;
}
