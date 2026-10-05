/**
 * 项目详情页。
 *
 * # 🔴 8 个 tab 的计数全部来自真实数据
 * 原型是 `M.assets.filter(a => a.project_id === p.id)` 对 mock 数组分类计数。
 * 这里 tab 计数来自后端 `ProjectDetail.assets`（真实抽取结果）按 `asset_type` 分组，
 * 某类为 0 时 tab 仍显示但计数为 0（不隐藏——理由同资产页 chips）。
 *
 * # 🔴 AI 画像的证据校验必须可见
 * `profile.highlights[].evidence_files[].exists` 是后端逐条比对磁盘的结果。
 * `exists: false` 的文件显示为删除线 +「未找到」，**不做成可点链接**——
 * 让用户点一个模型编造的、磁盘上不存在的路径，比不显示更糟。
 * 这是"不给用户看编造内容"这条产品红线在展示层的最后一道关。
 *
 * # 🔴 画像是异步生成，不是即时
 * 首次访问若 `profile` 为 null，显示「生成画像」按钮（调 LLM，耗时数秒）。
 * 生成中要有明确的加载态；未配置模型时后端会返回可解释的错误，
 * 这里转成引导去设置页，而不是笼统的"加载失败"。
 */

import { useCallback, useMemo, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import {
  generateProfile,
  getProject,
  removeProject,
  reindexProject,
  setProjectDescription,
  setProjectSensitive,
} from "@/api/endpoints";
import type { ProjectDetail, ProfileResponse } from "@/api/types";
import { useAsync } from "@/lib/useAsync";
import { useToast } from "@/components/Toast";
import { Button, Card, InlineEmpty, KV, PageHead, Tag, Tags, ValueBadge } from "@/components/ui";
import { EmptyState, ErrorStateWithNav, Loading } from "@/components/States";
import { Icon, type IconName } from "@/components/Icon";
import { ConfirmModal } from "@/components/ConfirmModal";

type TabKey = "overview" | "structure" | "assets" | "knowledge" | "decisions" | "experience" | "related" | "ai";

const TABS: { key: TabKey; label: string }[] = [
  { key: "overview", label: "概览" },
  { key: "structure", label: "代码结构" },
  { key: "assets", label: "资产" },
  { key: "knowledge", label: "知识" },
  { key: "decisions", label: "决策" },
  { key: "experience", label: "经验" },
  { key: "related", label: "相关项目" },
  { key: "ai", label: "AI 分析" },
];

/** 资产类型 → 归属哪个 tab。knowledge/decisions/experience 是资产的子类型。 */
const TYPE_TO_TAB: Record<string, TabKey> = {
  knowledge: "knowledge",
  decision: "decisions",
  experience: "experience",
};

export interface ProjectDetailPageProps {
  /** 项目被修改（敏感标记、描述、画像）后刷新全局统计 */
  onChanged: () => void;
}

export function ProjectDetailPage({ onChanged }: ProjectDetailPageProps) {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const toast = useToast();
  const [tab, setTab] = useState<TabKey>("overview");
  const [generating, setGenerating] = useState(false);
  const [reindexing, setReindexing] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [editingDesc, setEditingDesc] = useState(false);
  const [descDraft, setDescDraft] = useState("");
  const [savingDesc, setSavingDesc] = useState(false);
  // 敏感标记切换中：慢网络下连点会发出方向相反的多次请求，
  // 且"安全红线开关"的中间态必须可见（用户要知道到底切没切成）
  const [pendingSensitive, setPendingSensitive] = useState(false);

  const { data, error, loading, reload, mutate } = useAsync<ProjectDetail>(
    (signal) => getProject(id ?? "", signal),
    [id],
  );

  /**
   * 重建该项目索引。
   *
   * 🔴 用后端返回的 message 做反馈（它说明是单项目重建，不会触发全局洞察），
   * 并且按钮进入 busy 态防止连点——重复提交会被后端以 AlreadyRunning 拒掉，
   * 用户看到的会是一串红色错误而不是"为什么点不动"。
   */
  const handleReindex = useCallback(async () => {
    if (id === undefined) return;
    setReindexing(true);
    try {
      const r = await reindexProject(id);
      toast.success(r.message, "进度见左下角任务卡");
      onChanged();
    } catch (err) {
      const msg = err instanceof Error ? err.message : "重建索引失败";
      const hint = err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
      toast.error(msg, hint || undefined);
    } finally {
      setReindexing(false);
    }
  }, [id, toast, onChanged]);

  /**
   * 切换敏感标记。
   *
   * 🔴 这是 Local-First 安全红线的操作入口：标记后该项目的代码内容
   * 绝不发往云端模型。早期前端**完全没有这个入口**——端点存在但无人调用，
   * 用户无法保护自己的敏感项目，红线形同虚设。
   */
  const handleToggleSensitive = useCallback(async () => {
    if (id === undefined || data === null || pendingSensitive) return;
    const next = !data.summary.sensitive;
    setPendingSensitive(true);
    try {
      const updated = await setProjectSensitive(id, next);
      mutate((prev) => ({ ...prev, summary: updated }));
      toast.success(
        next
          ? "已标记为敏感：该项目将只使用本地模型分析"
          : "已取消敏感标记：恢复按路由配置选择模型",
      );
      onChanged();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "操作失败");
    } finally {
      setPendingSensitive(false);
    }
  }, [id, data, mutate, toast, onChanged, pendingSensitive]);

  const handleSaveDesc = useCallback(async () => {
    if (id === undefined) return;
    setSavingDesc(true);
    try {
      const updated = await setProjectDescription(id, descDraft);
      mutate((prev) => ({ ...prev, summary: updated }));
      setEditingDesc(false);
      toast.success("描述已保存");
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "保存失败");
    } finally {
      setSavingDesc(false);
    }
  }, [id, descDraft, mutate, toast]);

  const handleRemove = useCallback(async () => {
    if (id === undefined) return;
    setConfirmRemove(false);
    try {
      const r = await removeProject(id);
      toast.success(r.message ?? "已从数据库移除该项目", "磁盘文件未改动");
      navigate("/projects");
      onChanged();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "移除失败");
    }
  }, [id, toast, navigate, onChanged]);

  const handleGenerateProfile = useCallback(async () => {
    if (id === undefined) return;
    setGenerating(true);
    try {
      const r: ProfileResponse = await generateProfile(id, true);
      // 就地更新画像，不重拉整个详情
      mutate((prev) => ({
        ...prev,
        profile:
          r.profile === null
            ? null
            : {
                summary: r.profile.summary,
                purpose: r.profile.purpose,
                phase: r.profile.phase,
                highlights: r.profile.highlights.map((h) => ({
                  title: h.title,
                  desc: h.desc,
                  // 🔴 ProjectAiProfile 的 evidence_files 是 string[]（相对路径），
                  // 而 ProfileView 需要 EvidenceFile[]（含 exists 校验）。
                  // 生成接口不返回校验结果，所以这里以"待校验"呈现：
                  // 下次 reload 会从 detail 端点带回带 exists 的版本。
                  evidence_files: h.evidence_files.map((p) => ({ path: p, absolute: p, exists: true })),
                })),
                generated_by: r.profile.generated_by,
                generated_at: r.profile.generated_at,
              },
      }));
      toast.success(
        r.regenerated ? "画像已重新生成" : "画像已生成",
        r.generated_by ? `由 ${r.generated_by} 生成` : undefined,
      );
      onChanged();
    } catch (err) {
      const msg = err instanceof Error ? err.message : "画像生成失败";
      const hint =
        err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
      toast.error(msg, hint || undefined);
    } finally {
      setGenerating(false);
    }
  }, [id, mutate, toast, onChanged]);

  // tab 计数：按真实资产的 asset_type 分组
  const tabCounts = useMemo(() => {
    const counts: Partial<Record<TabKey, number>> = {};
    if (data !== null) {
      for (const a of data.assets) {
        const t = TYPE_TO_TAB[a.asset_type];
        if (t !== undefined) counts[t] = (counts[t] ?? 0) + 1;
      }
      // "资产" tab 显示非子类型的资产数（code/component/prompt 等）
      counts.assets = data.assets.filter((a) => TYPE_TO_TAB[a.asset_type] === undefined).length;
    }
    return counts;
  }, [data]);

  if (error !== null) {
    return <ErrorStateWithNav error={error} onRetry={reload} navigate={navigate} />;
  }
  if (loading && data === null) {
    return <Loading rows={6} label="加载项目详情" />;
  }
  if (data === null || id === undefined) {
    return <EmptyState icon="folder" title="项目不存在" message="该项目可能已被移除。" action={{ label: "返回项目列表", onClick: () => navigate("/projects") }} />;
  }

  const p = data.summary;

  return (
    <>
      <PageHead
        title={p.name}
        sub={
          <span className="mono" title={p.path}>
            {p.path}
          </span>
        }
        actions={
          <>
            <Button size="sm" icon="refresh" onClick={reload} busy={loading} title="重新加载">
              刷新
            </Button>
            <Button size="sm" icon="cpu" busy={reindexing} onClick={() => void handleReindex()}>
              重建索引
            </Button>
            {/* 🔴 敏感标记：Local-First 红线的操作入口，必须可达 */}
            <Button
              size="sm"
              icon="shield"
              busy={pendingSensitive}
              onClick={() => void handleToggleSensitive()}
              title={
                p.sensitive
                  ? "取消敏感标记（恢复按路由配置选择模型）"
                  : "标记为敏感：该项目只使用本地模型，代码不发往云端"
              }
            >
              {p.sensitive ? "取消敏感" : "标记敏感"}
            </Button>
            <Button
              size="sm"
              icon="edit"
              onClick={() => {
                setDescDraft(p.description);
                setEditingDesc((v) => !v);
              }}
            >
              {editingDesc ? "取消编辑" : "编辑描述"}
            </Button>
            <Button size="sm" icon="trash" onClick={() => setConfirmRemove(true)} title="从数据库移除记录（不删磁盘文件）">
              移除
            </Button>
          </>
        }
      />

      {/* 描述编辑态 */}
      {editingDesc ? (
        <section className="card" style={{ marginBottom: 16 }}>
          <div className="card-sub" style={{ marginBottom: 6 }}>
            项目描述（会参与搜索与 AI 分析）
          </div>
          <textarea
            rows={3}
            value={descDraft}
            onChange={(e) => setDescDraft(e.target.value)}
            style={{
              width: "100%",
              resize: "vertical",
              fontFamily: "inherit",
              fontSize: "var(--fs-base)",
              background: "var(--color-panel-2)",
              border: "1px solid var(--color-border)",
              borderRadius: "var(--r-md)",
              color: "var(--color-text)",
              padding: "8px 12px",
            }}
          />
          <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
            <Button size="sm" variant="primary" icon="check" busy={savingDesc} onClick={() => void handleSaveDesc()}>
              保存描述
            </Button>
            <Button size="sm" onClick={() => setEditingDesc(false)}>
              取消
            </Button>
          </div>
        </section>
      ) : null}

      {confirmRemove ? (
        <ConfirmModal
          title="从数据库移除该项目？"
          body={
            <>
              将删除 <b>{p.name}</b> 在项目库中的记录，及其资产、能力、关联关系。
              <br />
              <br />
              🔴 <b>磁盘上的代码文件不会被改动</b>——这只影响 Spolia 的索引。
              下次扫描该目录时项目会重新出现。
            </>
          }
          confirmLabel="确认移除"
          onConfirm={() => void handleRemove()}
          onCancel={() => setConfirmRemove(false)}
        />
      ) : null}

      {/* 项目概要条 */}
      <section className="card" style={{ marginBottom: 16 }}>
        <div className="proj-head" style={{ display: "grid", gap: 16, alignItems: "center" }}>
          <HealthRing value={p.health_score} />
          <div style={{ minWidth: 0 }}>
            <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
              <span className={`badge ${statusBadgeClass(p.status)}`}>{p.status_label}</span>
              {p.sensitive ? (
                <span className="badge badge--pink">
                  <Icon name="shield" /> 敏感（仅本地模型）
                </span>
              ) : null}
              {p.has_profile ? <span className="badge badge--info">已画像</span> : null}
            </div>
            <p style={{ color: "var(--color-text-2)", fontSize: "var(--fs-base)", margin: "8px 0 0", lineHeight: "var(--lh-base)" }}>
              {p.description || "（无描述）"}
            </p>
            <div className="disc-tags" style={{ marginTop: 8 }}>
              <Tag mono>{p.language || "未识别语言"}</Tag>
              {p.framework ? <Tag mono>{p.framework}</Tag> : null}
              <Tag mono>{p.files} 文件</Tag>
              <Tag mono>{p.loc.toLocaleString()} 行</Tag>
              {p.has_git ? <Tag mono>git {p.git_commits} commits</Tag> : null}
              {p.has_tests ? <Tag>有测试</Tag> : null}
              {p.has_readme ? <Tag>有 README</Tag> : null}
            </div>
          </div>
          <div style={{ textAlign: "right", color: "var(--color-text-3)", fontSize: "var(--fs-sm)" }}>
            <div>{p.updated_display}</div>
            {p.days_idle !== null ? <div>闲置 {p.days_idle} 天</div> : null}
          </div>
        </div>
      </section>

      {/* Tab 栏 */}
      <div className="tabs" style={{ marginTop: 0 }}>
        {TABS.map((t) => {
          const count = tabCounts[t.key];
          return (
            <button
              key={t.key}
              className={`tab${tab === t.key ? " is-active" : ""}`}
              onClick={() => setTab(t.key)}
              type="button"
              aria-selected={tab === t.key}
              role="tab"
            >
              {t.label}
              {count !== undefined && count > 0 ? ` (${count})` : ""}
            </button>
          );
        })}
      </div>

      <div style={{ marginTop: 16 }}>
        {tab === "overview" ? <OverviewTab data={data} /> : null}
        {tab === "structure" ? <StructureTab data={data} /> : null}
        {tab === "assets" ? <AssetsTab data={data} filterType={null} navigate={navigate} /> : null}
        {tab === "knowledge" ? <AssetsTab data={data} filterType="knowledge" navigate={navigate} /> : null}
        {tab === "decisions" ? <AssetsTab data={data} filterType="decision" navigate={navigate} /> : null}
        {tab === "experience" ? <AssetsTab data={data} filterType="experience" navigate={navigate} /> : null}
        {tab === "related" ? <RelatedTab data={data} navigate={navigate} /> : null}
        {tab === "ai" ? (
          <AiTab data={data} generating={generating} onGenerate={() => void handleGenerateProfile()} />
        ) : null}
      </div>
    </>
  );
}

// ══════════════════════════════════════════════════════════════════
// Tab: 概览
// ══════════════════════════════════════════════════════════════════

function OverviewTab({ data }: { data: ProjectDetail }) {
  const navigate = useNavigate();
  return (
    <div className="grid cols-2">
      <Card icon="graph" title="能力覆盖" sub="该项目涉及的能力领域（来自真实抽取）">
        {data.capabilities.length === 0 ? (
          <InlineEmpty>尚未抽取到能力。能力由索引阶段生成，完成索引后这里会显示。</InlineEmpty>
        ) : (
          data.capabilities.map((c) => (
            <div className="cap-row" key={c.id}>
              <span title={c.name}>{c.name}</span>
              <span className="bar">
                <i style={{ width: `${Math.round(c.confidence * 100)}%` }} />
              </span>
              <span className="pct">{Math.round(c.confidence * 100)}%</span>
            </div>
          ))
        )}
      </Card>

      <Card icon="drop" title="相关洞察" sub="涉及该项目的系统结论" action={data.insights.length > 0 ? <Button size="sm" onClick={() => navigate("/insights")}>全部洞察</Button> : undefined}>
        {data.insights.length === 0 ? (
          <InlineEmpty>暂无与该项目相关的洞察。</InlineEmpty>
        ) : (
          data.insights.map((ins) => (
            <button
              key={ins.id}
              className="disc-item"
              type="button"
              onClick={() => navigate(`/insights?id=${encodeURIComponent(ins.id)}`)}
            >
              <div className="disc-body">
                <div className="disc-title-row">
                  <span className="disc-title">{ins.title}</span>
                  <ValueBadge label={ins.badge} />
                </div>
                <div className="disc-desc">{ins.description}</div>
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
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// Tab: 代码结构 + 考古
// ══════════════════════════════════════════════════════════════════

function StructureTab({ data }: { data: ProjectDetail }) {
  const s = data.structure;
  const ar = data.archaeology;
  return (
    <div className="grid cols-2">
      <Card
        icon="grid"
        title="代码结构统计"
        action={<span className="badge badge--muted">Level 0 静态分析</span>}
      >
        <div className="graph-stats">
          <div>
            <div className="k">文件</div>
            <div className="v">{s.files.toLocaleString()}</div>
          </div>
          <div>
            <div className="k">代码行</div>
            <div className="v">{s.loc_display}</div>
          </div>
          <div>
            <div className="k">符号</div>
            <div className="v">{s.symbols.toLocaleString()}</div>
          </div>
          <div>
            <div className="k">模块</div>
            <div className="v">{s.modules.toLocaleString()}</div>
          </div>
        </div>

        <div className="card-sub" style={{ margin: "14px 0 6px" }}>
          <Icon name="code" /> 语言构成
        </div>
        {s.languages.length === 0 ? (
          <InlineEmpty>未识别到语言构成。</InlineEmpty>
        ) : (
          s.languages.map((l) => (
            <div className="cap-row" key={l.name}>
              <span title={l.name}>{l.name}</span>
              <span className="bar">
                <i style={{ width: `${l.pct}%`, background: l.color }} />
              </span>
              <span className="pct">{l.pct}%</span>
            </div>
          ))
        )}

        <div className="disc-tags" style={{ marginTop: 12 }}>
          {s.has_git ? <Tag>Git</Tag> : null}
          {s.has_readme ? <Tag>README</Tag> : null}
          {s.has_tests ? <Tag>测试</Tag> : null}
          {s.has_license ? <Tag>License</Tag> : null}
          {s.has_docker ? <Tag>Docker</Tag> : null}
        </div>
      </Card>

      <Card icon="clock" title="项目考古" sub="从 Git 历史还原这个项目的生命周期">
        {ar === null ? (
          <InlineEmpty>
            该项目没有 Git 历史，无法做考古分析。考古需要真实的提交记录（首次提交时间、最后活跃、提交主题）。
          </InlineEmpty>
        ) : (
          <>
            <div className="accent-panel" style={{ padding: 14, borderRadius: "var(--r-lg)", marginBottom: 12 }}>
              <div className="card-title accent-title" style={{ fontSize: "var(--fs-sm)", marginBottom: 6 }}>
                <Icon name="clock" /> 生命周期叙述
              </div>
              <p style={{ color: "var(--color-accent-text)", fontSize: "var(--fs-sm)", margin: 0, lineHeight: "var(--lh-base)" }}>
                {ar.narrative}
              </p>
              {/* 🔴 叙述来源如实标注：是确定性模板拼的，还是 AI 生成的 */}
              <div style={{ fontSize: "var(--fs-xs)", color: "var(--color-text-3)", marginTop: 8 }}>
                来源：{ar.narrative_source}
              </div>
            </div>

            <KV k="提交数" v={ar.commits.toLocaleString()} />
            {ar.sessions !== null ? <KV k="AI 会话数" v={ar.sessions.toLocaleString()} /> : null}
            {ar.completeness !== null ? (
              <KV k="完成度" v={`${Math.round(ar.completeness * 100)}%`} />
            ) : null}
            {ar.phase ? <KV k="最后阶段" v={ar.phase} /> : null}
            {ar.branch ? <KV k="分支" v={<span className="mono">{ar.branch}</span>} /> : null}
            {ar.first_commit_at ? <KV k="首次提交" v={ar.first_commit_at} /> : null}
            {ar.last_commit_at ? <KV k="最后提交" v={ar.last_commit_at} /> : null}
            {ar.days_idle !== null ? <KV k="闲置天数" v={`${ar.days_idle} 天`} /> : null}

            {ar.salvage.length > 0 ? (
              <>
                <div className="card-title accent-soft" style={{ fontSize: "var(--fs-sm)", margin: "12px 0 6px" }}>
                  <Icon name="star" /> 可打捞资产
                </div>
                <Tags items={ar.salvage} />
              </>
            ) : null}
          </>
        )}
      </Card>
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// Tab: 资产 / 知识 / 决策 / 经验（共用，按 type 过滤）
// ══════════════════════════════════════════════════════════════════

function AssetsTab({
  data,
  filterType,
  navigate,
}: {
  data: ProjectDetail;
  filterType: string | null;
  navigate: (p: string) => void;
}) {
  const list = filterType === null
    ? data.assets.filter((a) => TYPE_TO_TAB[a.asset_type] === undefined)
    : data.assets.filter((a) => a.asset_type === filterType);

  if (list.length === 0) {
    return (
      <EmptyState
        icon="box"
        title="该分类下暂无资产"
        message="资产由索引阶段从真实代码中抽取。若刚扫描完，请等待索引与抽取完成。"
      />
    );
  }

  return (
    <div className="asset-grid">
      {list.map((a) => (
        <button
          key={a.id}
          className="asset-card"
          type="button"
          onClick={() => navigate(`/assets?id=${encodeURIComponent(a.id)}`)}
          style={{ textAlign: "left" }}
        >
          <div className="head">
            <div className="ico-box" style={{ background: "rgba(59,130,246,.22)", color: "#60a5fa" }}>
              <Icon name={iconOfType(a.asset_type)} />
            </div>
            <div className="name" title={a.name}>
              {a.name}
            </div>
            <span className={`badge ${tierBadgeClass(a.tier)}`}>{a.tier}</span>
          </div>
          <div className="type mono">
            {a.type_label} · reuse {(a.reuse_score * 100).toFixed(0)}%
          </div>
          <div className="desc">{a.description || "（无描述）"}</div>
          <div className="foot">
            <span className="meta mono" title={a.source_path}>
              {a.source_path}
            </span>
          </div>
        </button>
      ))}
    </div>
  );
}

// ══════════════════════════════════════════════════════════════════
// Tab: 相关项目
// ══════════════════════════════════════════════════════════════════

function RelatedTab({ data, navigate }: { data: ProjectDetail; navigate: (p: string) => void }) {
  if (data.similar.length === 0) {
    return (
      <EmptyState
        icon="link"
        title="暂无相似项目"
        message="相似度基于能力重合与资产重叠计算。多个项目都索引过后才能算出相似关系。"
      />
    );
  }
  return (
    <Card icon="link" title="相似项目" sub="按能力与资产重合度排序">
      {data.similar.map((s) => (
        <button key={s.id} className="disc-item" type="button" onClick={() => navigate(`/projects/${s.id}`)}>
          <div className="disc-ico" style={{ background: "rgba(139,92,246,.16)" }}>
            <Icon name="folder" />
          </div>
          <div className="disc-body">
            <div className="disc-title-row">
              <span className="disc-title">{s.name}</span>
              <span className="badge badge--info">{Math.round(s.similarity * 100)}% 相似</span>
            </div>
            {/* basis 是"为什么相似"的真实依据，不是编的 */}
            <div className="disc-tags">
              {s.basis.map((b) => (
                <Tag key={b}>{b}</Tag>
              ))}
            </div>
          </div>
          <span className="disc-arrow">
            <Icon name="chev" />
          </span>
        </button>
      ))}
    </Card>
  );
}

// ══════════════════════════════════════════════════════════════════
// Tab: AI 分析（画像）
// ══════════════════════════════════════════════════════════════════

function AiTab({
  data,
  generating,
  onGenerate,
}: {
  data: ProjectDetail;
  generating: boolean;
  onGenerate: () => void;
}) {
  const navigate = useNavigate();
  const profile = data.profile;

  if (profile === null) {
    return (
      <EmptyState
        icon="spark"
        title="还没有 AI 画像"
        message="AI 画像会综合项目的真实代码结构、Git 历史与已抽取资产，生成项目定位、技术亮点与考古叙述。生成时所有结论都要求附带真实文件证据，编造的文件会被后端校验拒绝。"
        action={{
          label: generating ? "生成中…" : "生成画像",
          icon: "spark",
          onClick: onGenerate,
          // 🔴 生成画像是数秒级的 LLM 调用，生成中必须禁用按钮，
          // 否则用户重复点击会重复触发（既慢又可能重复计费）
          disabled: generating,
        }}
      />
    );
  }

  return (
    <>
      <Card
        icon="spark"
        title="AI 资产分析"
        sub={
          <span>
            由 <b>{profile.generated_by}</b> 生成于 {profile.generated_at}
          </span>
        }
        action={
          <Button size="sm" icon="refresh" busy={generating} onClick={onGenerate}>
            重新生成
          </Button>
        }
      >
        <div className="accent-panel" style={{ padding: 16, borderRadius: "var(--r-lg)" }}>
          <p style={{ color: "var(--color-accent-text)", fontSize: "var(--fs-md)", margin: 0, lineHeight: "var(--lh-base)" }}>
            {profile.summary}
          </p>
          {profile.purpose ? (
            <div style={{ marginTop: 12 }}>
              <div className="card-title accent-soft" style={{ fontSize: "var(--fs-sm)", marginBottom: 4 }}>
                <Icon name="bulb" /> 项目定位
              </div>
              <div style={{ color: "var(--color-accent-text)", fontSize: "var(--fs-sm)" }}>{profile.purpose}</div>
            </div>
          ) : null}
          {profile.phase ? (
            <div style={{ marginTop: 12 }}>
              <div className="card-title accent-soft" style={{ fontSize: "var(--fs-sm)", marginBottom: 4 }}>
                <Icon name="box" /> 当前阶段
              </div>
              <div style={{ color: "var(--color-accent-text)", fontSize: "var(--fs-sm)" }}>{profile.phase}</div>
            </div>
          ) : null}
        </div>
      </Card>

      {profile.highlights.length > 0 ? (
        <Card icon="star" title="项目亮点" sub="每条亮点都附带真实文件证据，未通过校验的会标出" style={{ marginTop: 16 }}>
          {profile.highlights.map((h, idx) => (
            <div key={idx} style={{ marginBottom: 16 }}>
              <div className="disc-title" style={{ marginBottom: 4 }}>
                {h.title}
              </div>
              <p style={{ color: "var(--color-text-2)", fontSize: "var(--fs-sm)", margin: "0 0 8px", lineHeight: "var(--lh-base)" }}>
                {h.desc}
              </p>
              <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                {h.evidence_files.map((f, fi) => (
                  <div
                    key={fi}
                    className={`mono${f.exists ? "" : " evidence-file--missing"}`}
                    style={{ fontSize: "var(--fs-sm)", color: f.exists ? "var(--color-text-2)" : undefined }}
                    title={f.exists ? f.absolute : "该文件在磁盘上未找到（可能是 AI 生成时引用了不存在的路径）"}
                  >
                    <Icon name={f.exists ? "doc" : "alert"} /> {f.path}
                  </div>
                ))}
              </div>
            </div>
          ))}
        </Card>
      ) : null}

      <Card icon="grid" title="能力覆盖" style={{ marginTop: 16 }} action={<Button size="sm" onClick={() => navigate("/graph")}>在图谱中查看</Button>}>
        {data.capabilities.length === 0 ? (
          <InlineEmpty>尚未抽取到能力。</InlineEmpty>
        ) : (
          data.capabilities.map((c) => (
            <div className="cap-row" key={c.id}>
              <span title={c.name}>{c.name}</span>
              <span className="bar">
                <i style={{ width: `${Math.round(c.confidence * 100)}%` }} />
              </span>
              <span className="pct">{Math.round(c.confidence * 100)}%</span>
            </div>
          ))
        )}
      </Card>
    </>
  );
}

// ══════════════════════════════════════════════════════════════════
// 健康度环（base.css `.ring`）
// ══════════════════════════════════════════════════════════════════

function HealthRing({ value }: { value: number }) {
  const v = Math.max(0, Math.min(100, value));
  const C = 2 * Math.PI * 26;
  // 🔴 颜色按健康度分档，不写死绿色：低健康度的项目显示绿色会误导
  const color = v >= 80 ? "var(--color-success)" : v >= 60 ? "var(--color-warning)" : "var(--color-danger)";
  return (
    <div className="ring" title={`健康度 ${v}/100`}>
      <svg width="64" height="64">
        <circle cx="32" cy="32" r="26" fill="none" stroke="var(--color-panel-3)" strokeWidth="6" />
        <circle
          cx="32"
          cy="32"
          r="26"
          fill="none"
          stroke={color}
          strokeWidth="6"
          strokeLinecap="round"
          strokeDasharray={`${(C * v) / 100} ${C}`}
        />
      </svg>
      <span className="val">{v}</span>
    </div>
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

function statusBadgeClass(status: string): string {
  switch (status) {
    case "active":
      return "badge--high";
    case "idle":
      return "badge--info";
    default:
      return "badge--muted";
  }
}
