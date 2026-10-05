/**
 * 共享 UI 原语。
 *
 * 🔴 全部复用 base.css 的既有类（`.card` / `.btn` / `.badge` / `.chip` / `.page-head`…），
 * 不新造平行的一套：两套卡片样式必然在圆角/内距/边框上漂移，
 * 而且改设计时得同时改两处，漏一处就出现视觉不一致。
 */

import type { ReactNode } from "react";
import { Icon, type IconName } from "./Icon";

// ══════════════════════════════════════════════════════════════════
// 页面骨架
// ══════════════════════════════════════════════════════════════════

export interface PageHeadProps {
  title: string;
  /** 副标题。用于说明这个页面是什么、数据从哪来。 */
  sub?: ReactNode;
  actions?: ReactNode;
}

/** 页面标题行（base.css `.page-head` + app.css `.actions`）。 */
export function PageHead({ title, sub, actions }: PageHeadProps) {
  return (
    <div className="page-head">
      <div>
        <h1 className="page-title">{title}</h1>
        {sub ? <div className="page-sub">{sub}</div> : null}
      </div>
      {actions ? <div className="actions">{actions}</div> : null}
    </div>
  );
}

export interface CardProps {
  title?: ReactNode;
  /** 标题下的说明文字 */
  sub?: ReactNode;
  icon?: IconName;
  /** 右上角动作（通常是"更多 →"链接） */
  action?: ReactNode;
  children: ReactNode;
  className?: string;
  style?: React.CSSProperties;
}

/** 卡片容器（base.css `.card` / `.card-head` / `.card-title` / `.card-sub`）。 */
export function Card({ title, sub, icon, action, children, className, style }: CardProps) {
  const cls = ["card", className].filter(Boolean).join(" ");
  return (
    <section className={cls} style={style}>
      {title ? (
        <div className="card-head">
          <div>
            <div className="card-title">
              {icon ? <Icon name={icon} /> : null}
              {title}
            </div>
            {sub ? <div className="card-sub">{sub}</div> : null}
          </div>
          {action}
        </div>
      ) : null}
      {children}
    </section>
  );
}

/** "更多 →" 链接按钮。 */
export function MoreLink({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button className="link-more" onClick={onClick} type="button">
      {label} <Icon name="arr" />
    </button>
  );
}

// ══════════════════════════════════════════════════════════════════
// 按钮
// ══════════════════════════════════════════════════════════════════

export interface ButtonProps {
  children: ReactNode;
  onClick?: () => void;
  variant?: "primary" | "ghost";
  size?: "sm" | "md";
  icon?: IconName;
  disabled?: boolean;
  /** 进行中：显示 spinner 并禁用，避免重复提交 */
  busy?: boolean;
  title?: string;
  type?: "button" | "submit";
}

/**
 * 按钮。
 *
 * 🔴 `busy` 必须同时禁用点击：
 * 写操作（扫描、保存、标记反馈）在请求期间若还能点，
 * 用户连点会提交多个任务，后端只能拒掉后面的并返回"任务已在运行"——
 * 用户看到的是一堆红色错误，而根因是界面没锁住。
 */
export function Button({
  children,
  onClick,
  variant = "ghost",
  size = "md",
  icon,
  disabled = false,
  busy = false,
  title,
  type = "button",
}: ButtonProps) {
  const cls = [
    "btn",
    variant === "primary" ? "btn--primary" : "btn--ghost",
    size === "sm" ? "btn--sm" : "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button
      className={cls}
      onClick={onClick}
      disabled={disabled || busy}
      title={title}
      type={type}
      aria-busy={busy || undefined}
    >
      {busy ? <Icon name="refresh" style={{ animation: "spin 1.2s linear infinite" }} /> : null}
      {!busy && icon ? <Icon name={icon} /> : null}
      {children}
    </button>
  );
}

// ══════════════════════════════════════════════════════════════════
// 徽章
// ══════════════════════════════════════════════════════════════════

/**
 * 价值徽章（洞察的置信度分档）。
 *
 * 🔴 `badgeKey` 来自后端（`high`/`potent`/`info`），前端不自己按置信度分档：
 * 分档阈值是后端的领域知识，两边各算一套必然漂移，
 * 症状是同一条洞察在首页和详情页显示不同的价值等级。
 */
export function ValueBadge({ label, badgeKey }: { label: string; badgeKey?: string }) {
  const cls =
    badgeKey === "high"
      ? "badge badge--high"
      : badgeKey === "potent"
        ? "badge badge--potent"
        : badgeKey === "info"
          ? "badge badge--info"
          : "badge badge--muted";
  return <span className={cls}>{label}</span>;
}

/**
 * 处置状态徽章（洞察的用户反馈状态）。
 *
 * 🔴 与 `ValueBadge` 是**两个独立维度**，配色刻意不同：
 * 价值徽章说"这条洞察有多可信"，状态徽章说"我处理过它没有"。
 * 共用一套配色的话，用户无法区分"高价值"和"我已标记有用"。
 */
export function StateBadge({ state, stateKey }: { state: string; stateKey?: string }) {
  const cls =
    stateKey === "useful"
      ? "badge badge--state-useful"
      : stateKey === "useless"
        ? "badge badge--state-useless"
        : stateKey === "ignored"
          ? "badge badge--state-ignored"
          : "badge badge--state-pending";
  return <span className={cls}>{state}</span>;
}

/** 普通标签（资产的 tags、洞察的 tags）。 */
export function Tag({ children, mono = false }: { children: ReactNode; mono?: boolean }) {
  return <span className={mono ? "tag mono" : "tag"}>{children}</span>;
}

/** 标签组（base.css `.disc-tags`）。 */
export function Tags({ items, max }: { items: string[]; max?: number }) {
  const shown = max !== undefined ? items.slice(0, max) : items;
  if (shown.length === 0) return null;
  return (
    <div className="disc-tags">
      {shown.map((t) => (
        <Tag key={t}>{t}</Tag>
      ))}
      {max !== undefined && items.length > max ? <Tag>+{items.length - max}</Tag> : null}
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// 筛选 chips
// ══════════════════════════════════════════════════════════════════

export interface ChipOption {
  value: string;
  label: string;
  count?: number;
}

export interface ChipsProps {
  options: ChipOption[];
  /** 当前选中值；单选传 string，多选传 string[] */
  selected: string | string[];
  onSelect: (value: string) => void;
  multi?: boolean;
}

/**
 * 筛选 chips。
 *
 * 🔴 **计数为 0 的选项也要渲染**（后端 `all_types` / `facets` 已保证这点）。
 * 若只渲染有结果的项，用户勾掉某个类型后那个 chip 就消失了，
 * 再也点不回来——只能刷新页面重来。
 */
export function Chips({ options, selected, onSelect, multi = false }: ChipsProps) {
  const isSelected = (v: string) =>
    multi ? (selected as string[]).includes(v) : selected === v;

  return (
    <div className="chips">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          className={`chip${isSelected(o.value) ? " is-active" : ""}`}
          onClick={() => onSelect(o.value)}
          aria-pressed={isSelected(o.value)}
        >
          {o.label}
          {o.count !== undefined ? <span className="chip-count">{o.count}</span> : null}
        </button>
      ))}
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// 键值行 / 空提示
// ══════════════════════════════════════════════════════════════════

/** 详情页的键值行（app.css `.kv-row`）。 */
export function KV({ k, v }: { k: ReactNode; v: ReactNode }) {
  return (
    <div className="kv-row">
      <div className="k">{k}</div>
      <div className="v">{v}</div>
    </div>
  );
}

/** 卡片内的局部空提示（区别于整页空态）。 */
export function InlineEmpty({ children }: { children: ReactNode }) {
  return <div className="inline-empty">{children}</div>;
}

/** 结果计数行（"共 128 项 · 第 1-20 项"）。 */
export function ResultMeta({
  total,
  offset,
  shown,
  extra,
}: {
  total: number;
  offset?: number;
  shown?: number;
  extra?: ReactNode;
}) {
  const from = total === 0 ? 0 : (offset ?? 0) + 1;
  const to = (offset ?? 0) + (shown ?? total);
  return (
    <div className="result-meta">
      <span>
        共 {total} 项
        {total > 0 ? ` · 第 ${from}-${Math.min(to, total)} 项` : ""}
      </span>
      {extra}
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// 进度条
// ══════════════════════════════════════════════════════════════════

/**
 * 进度条（base.css `.progress > i`）。
 *
 * 🔴 复用既有类而非新造 `.progress-track`/`.progress-fill`：
 * 两套进度条样式必然在动画时长与圆角上漂移。
 */
export function ProgressBar({ percent, active = false }: { percent: number; active?: boolean }) {
  const p = Math.max(0, Math.min(100, Math.round(percent)));
  return (
    <div
      className={`progress${active ? " is-active" : ""}`}
      role="progressbar"
      aria-valuenow={p}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <i style={{ width: `${p}%` }} />
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// 分页
// ══════════════════════════════════════════════════════════════════

export interface PaginationProps {
  total: number;
  limit: number;
  offset: number;
  onChange: (offset: number) => void;
}

/**
 * 分页控件。
 *
 * 🔴 `total` 必须是后端给的"满足筛选条件的总数"，不是本页条数。
 * 早期后端有个缺陷：`count_filtered` 复用了带 offset 的列表查询，
 * 于是翻到第 2 页时 total 会缩短、总页数跟着变少，用户翻不回去。
 * 那个缺陷已在后端修掉并有回归测试锁定，前端这里只信任 total。
 */
export function Pagination({ total, limit, offset, onChange }: PaginationProps) {
  const pages = Math.max(1, Math.ceil(total / limit));
  const current = Math.floor(offset / limit) + 1;
  if (pages <= 1) return null;

  return (
    <div className="result-meta" style={{ justifyContent: "center", marginTop: 16 }}>
      <Button size="sm" disabled={current <= 1} onClick={() => onChange((current - 2) * limit)}>
        上一页
      </Button>
      <span>
        {current} / {pages}
      </span>
      <Button size="sm" disabled={current >= pages} onClick={() => onChange(current * limit)}>
        下一页
      </Button>
    </div>
  );
}
