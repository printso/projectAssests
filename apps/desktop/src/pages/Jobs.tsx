/**
 * 任务页。
 *
 * # 🔴 进度来自 SSE，不是轮询、更不是假动画
 * 原型的扫描进度是 `setTimeout` 驱动的假动画：
 * `stages` 数组写死了每一步的 `at`/`found`/`label`，
 * 最后 toast 硬编码「扫描完成: 发现 128 个项目, 新增 3 个」。
 * 不管后台真实发生什么，界面都按剧本演一遍——这是最典型的模拟数据。
 *
 * 这里：活跃任务的实时进度来自 `ProgressContext`（SSE 推送），
 * 历史任务列表来自 `/api/jobs`（真实 DB 记录，含 error 文案）。
 *
 * # 🔴 取消要如实反映结果
 * 取消是异步的（handler 要跑到下一个检查点才退出），
 * 所以点击后按钮进入 busy 态，成功后端返回的 JobView 才是真相。
 * 若乐观地立刻显示"已取消"，用户会以为停了而实际还在写库。
 *
 * # 🔴 终态任务不可取消
 * `cancellable` 由后端给出（completed/failed/cancelled 都为 false）。
 * 前端据此禁用按钮——对已结束的任务点取消会得到 404/409，
 * 那是个让用户困惑的错误。
 */

import { useCallback, useState } from "react";
import { useNavigate } from "react-router-dom";
import { cancelJob, listJobs, startIndex, startInsights } from "@/api/endpoints";
import type { JobListPage, JobView } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { useProgressState } from "@/lib/ProgressContext";
import { progressCounter, progressLabel, progressPercent } from "@/lib/useProgress";
import { useApp } from "@/lib/AppContext";
import { useToast } from "@/components/Toast";
import { Button, PageHead, ProgressBar, ResultMeta, Tag } from "@/components/ui";
import { EmptyState, ErrorStateWithNav, Loading } from "@/components/States";
import { Icon, type IconName } from "@/components/Icon";
import { DEFAULT_PAGE_SIZE } from "@/config";
import { formatDateTime, timeAgo } from "@/lib/format";

export function JobsPage() {
  const navigate = useNavigate();
  const toast = useToast();
  const { bumpHealth } = useApp();
  const progress = useProgressState();

  const { data, error, loading, reload } = useAsync<JobListPage>(
    (signal) => listJobs({ limit: DEFAULT_PAGE_SIZE * 2 }, signal),
    [],
  );

  const [triggering, setTriggering] = useState<"index" | "insights" | null>(null);

  const trigger = useCallback(
    async (which: "index" | "insights") => {
      setTriggering(which);
      try {
        const r = which === "index" ? await startIndex() : await startInsights();
        // 🔴 原样显示后端 message（预告链式行为与范围）
        toast.success(r.message, "进度见上方实时卡片");
        reload();
        bumpHealth();
      } catch (err) {
        const msg = err instanceof Error ? err.message : "触发失败";
        const hint = err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
        toast.error(msg, hint || undefined);
      } finally {
        setTriggering(null);
      }
    },
    [toast, reload, bumpHealth],
  );

  const handleCancel = useCallback(
    async (job: JobView) => {
      try {
        const updated = await cancelJob(job.id);
        toast.success(`已取消「${updated.job_type_label}」`, updated.status_label);
        reload();
        bumpHealth();
      } catch (err) {
        // 🔴 job_not_found / job_already_finished 都说明状态已变化，
        // 刷新列表让用户看到真实状态，而不是只弹一个错误
        toast.error(err instanceof Error ? err.message : "取消失败");
        reload();
      }
    },
    [toast, reload, bumpHealth],
  );

  // SSE 事件可能更新了某个任务的状态：用它就地纠正列表，
  // 避免用户盯着一个不动的列表（轮询间隔内的状态是过期的）
  const liveEvent = progress.event;

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }

  return (
    <>
      <PageHead
        title="任务"
        sub="扫描、索引、洞察生成等后台任务的执行记录与实时进度"
        actions={
          <>
            {/* 🔴 手动触发入口：扫描走首页（要选目录），但索引/洞察可以单独重跑。
                场景：索引逻辑升级后想重建全部索引，或只想重算洞察不想重扫。
                没有这两个入口时，用户只能重新扫描整条链，代价大得多。 */}
            <Button
              size="sm"
              icon="cpu"
              busy={triggering !== null}
              onClick={() => void trigger("index")}
            >
              重建全部索引
            </Button>
            <Button
              size="sm"
              icon="drop"
              busy={triggering !== null}
              onClick={() => void trigger("insights")}
            >
              重算洞察
            </Button>
            <Button size="sm" icon="refresh" onClick={reload} busy={loading}>
              刷新
            </Button>
          </>
        }
      />

      {/* ── 实时进度（SSE）────────────────────────────────── */}
      {liveEvent !== null && (liveEvent.status === "running" || liveEvent.status === "queued") ? (
        <section className="card" style={{ marginBottom: 16, borderColor: "var(--color-primary)" }}>
          <div className="card-head">
            <div>
              <div className="card-title">
                <Icon name="cpu" /> {jobTypeLabel(liveEvent.job_type)} · 进行中
              </div>
              <div className="card-sub">
                {/* 🔴 阶段文案来自后端 stage，不在前端按百分比猜 */}
                {progressLabel(liveEvent)}
                {progressCounter(liveEvent) ? ` · ${progressCounter(liveEvent)}` : ""}
              </div>
            </div>
            <div style={{ fontSize: "var(--fs-lg)", fontWeight: 600 }}>
              {progressPercent(liveEvent)}%
            </div>
          </div>
          <ProgressBar percent={progressPercent(liveEvent)} active />
          {liveEvent.error ? (
            <div style={{ color: "var(--color-danger)", fontSize: "var(--fs-sm)", marginTop: 8 }}>
              {liveEvent.error}
            </div>
          ) : null}
        </section>
      ) : null}

      {loading && data === null ? <Loading rows={6} label="加载任务列表" /> : null}

      {data !== null ? (
        <>
          <ResultMeta total={data.total} shown={data.items.length} />

          {data.items.length === 0 ? (
            <EmptyState
              icon="cpu"
              title="还没有任务记录"
              message="扫描、索引、生成洞察都会记录在这里。从首页触发一次扫描即可开始。"
              action={{ label: "去首页扫描", icon: "scan", onClick: () => navigate("/") }}
            />
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              {data.items.map((j) => (
                <JobRow key={j.id} job={j} onCancel={() => void handleCancel(j)} />
              ))}
            </div>
          )}
        </>
      ) : null}
    </>
  );
}

function JobRow({ job: j, onCancel }: { job: JobView; onCancel: () => void }) {
  const terminal = j.status === "completed" || j.status === "failed" || j.status === "cancelled";
  return (
    <section className="card" style={{ padding: "12px 16px" }}>
      <div style={{ display: "flex", gap: 12, alignItems: "flex-start" }}>
        <div className="disc-ico" style={{ background: statusColor(j.status), color: "#fff" }}>
          <Icon name={statusIcon(j.status)} />
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="disc-title-row">
            <span className="disc-title">{j.job_type_label}</span>
            <span className={`badge ${statusBadge(j.status)}`}>{j.status_label}</span>
            {j.cancellable ? (
              <Button size="sm" icon="x" onClick={onCancel} title="取消该任务">
                取消
              </Button>
            ) : null}
          </div>

          <div className="disc-tags" style={{ marginTop: 6 }}>
            <Tag mono>{j.id.slice(0, 8)}</Tag>
            {j.counter_text ? <Tag mono>{j.counter_text}</Tag> : null}
            {/* 🔴 不直接显示原始 ISO 纳秒串（见 format.ts） */}
            <Tag>{formatDateTime(j.created_at)}</Tag>
            {j.updated_at !== j.created_at ? <Tag>更新于 {timeAgo(j.updated_at)}</Tag> : null}
          </div>

          {j.stage ? (
            <div style={{ color: "var(--color-text-3)", fontSize: "var(--fs-sm)", marginTop: 6 }}>
              {j.stage}
            </div>
          ) : null}

          {/* 🔴 失败原因必须显示：这是用户排查问题的唯一线索。
              只显示"失败"而不给原因，用户只能反复重试同一个必然失败的操作。 */}
          {j.error ? (
            <div
              style={{
                marginTop: 8,
                padding: "8px 12px",
                background: "color-mix(in srgb, var(--color-danger) 10%, transparent)",
                border: "1px solid color-mix(in srgb, var(--color-danger) 30%, transparent)",
                borderRadius: "var(--r-md)",
                color: "var(--color-text-2)",
                fontSize: "var(--fs-sm)",
                wordBreak: "break-word",
              }}
            >
              <Icon name="alert" /> {j.error}
            </div>
          ) : null}

          {!terminal ? <div style={{ marginTop: 8 }}>
            <ProgressBar percent={j.percent} active />
          </div> : null}
        </div>

        <div style={{ textAlign: "right", color: "var(--color-text-3)", fontSize: "var(--fs-sm)", flex: "none" }}>
          {terminal ? `${j.percent}%` : ""}
        </div>
      </div>
    </section>
  );
}

function statusIcon(status: string): IconName {
  switch (status) {
    case "completed":
      return "check";
    case "failed":
      return "alert";
    case "cancelled":
      return "x";
    case "queued":
      return "clock";
    default:
      return "refresh";
  }
}

function statusColor(status: string): string {
  switch (status) {
    case "completed":
      return "var(--color-success)";
    case "failed":
      return "var(--color-danger)";
    case "cancelled":
      return "var(--color-text-3)";
    case "queued":
      return "var(--color-panel-3)";
    default:
      return "var(--color-primary)";
  }
}

function statusBadge(status: string): string {
  switch (status) {
    case "completed":
      return "badge--high";
    case "failed":
      return "badge--pink";
    case "running":
      return "badge--info";
    default:
      return "badge--muted";
  }
}

/** 任务类型中文标签（SSE 事件只给 job_type，列表接口给 job_type_label）。 */
function jobTypeLabel(t: string): string {
  switch (t) {
    case "SCAN_PROJECT":
      return "扫描项目";
    case "INDEX_CODE":
      return "索引代码";
    case "GENERATE_INSIGHT":
      return "生成洞察";
    default:
      return t;
  }
}
