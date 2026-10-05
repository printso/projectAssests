/**
 * 应用外壳与路由。
 *
 * # 🔴 服务离线时不渲染任何页面
 * projectAssests 是桌面前端 + 本地 Rust 服务的双进程结构。服务没起来时，
 * 每个页面都会各自请求失败并显示错误态——用户看到七八个"加载失败"，
 * 而根因只有"服务未连接"一个。
 *
 * 所以这里在 status !== "online" 时直接显示全屏引导（含启动命令），
 * **不挂载路由**。这既减少了无谓的请求风暴，也让用户第一眼就知道该做什么。
 *
 * # 🔴 路由与页面一一对应
 * 每条路由对应 `pages/` 下的一个组件。没有"占位页面"：
 * 未实现的功能（MCP）在导航里就是 disabled，不会路由到空白页。
 */

import { useCallback, useMemo } from "react";
import { Navigate, Route, Routes, useNavigate } from "react-router-dom";

import { Sidebar } from "@/components/Sidebar";
import { Topbar } from "@/components/Topbar";
import { Icon } from "@/components/Icon";
import { useApp } from "@/lib/AppContext";
import { ProgressProvider } from "@/lib/ProgressContext";
import { useAppearance } from "@/lib/useAppearance";

import { OverviewPage } from "@/pages/Overview";
import { ProjectsPage } from "@/pages/Projects";
import { ProjectDetailPage } from "@/pages/ProjectDetail";
import { AssetsPage } from "@/pages/Assets";
import { GraphPage } from "@/pages/Graph";
import { InsightsPage } from "@/pages/Insights";
import { OpportunitiesPage } from "@/pages/Opportunities";
import { AnalystPage } from "@/pages/Analyst";
import { SearchPage } from "@/pages/Search";
import { SettingsPage } from "@/pages/Settings";
import { JobsPage } from "@/pages/Jobs";

export function App() {
  const { status, health, reason, recheck, bumpHealth } = useApp();
  const appearance = useAppearance();

  // 主题切换失败要给反馈（乐观更新已回滚，但用户需要知道为什么没生效）
  const handleThemeChange = useCallback(
    (theme: "dark" | "light") => {
      void appearance.setTheme(theme);
    },
    [appearance],
  );

  const navigate = useNavigate();
  const handleSearch = useCallback(
    (q: string) => {
      navigate(q === "" ? "/search" : `/search?q=${encodeURIComponent(q)}`);
    },
    [navigate],
  );

  // 🔴 shell 的 useMemo **刻意不依赖 progress**：
  // SSE 每帧都改 progress，若列进依赖，一次扫描几百帧会重建整棵路由树
  // （memo 完全失效，扫描时全应用疯狂重渲染）。
  // 进度改由 ProgressProvider 隔离，只有真正消费它的 Sidebar/Overview 会重渲染。
  const shell = useMemo(
    () => (
      <div className="app">
        <Sidebar health={health} onNavigate={navigate} />
        <Topbar
          status={status}
          theme={appearance.theme}
          onThemeChange={handleThemeChange}
          onSearch={handleSearch}
        />
        <main className="main">
          <Routes>
            <Route path="/" element={<OverviewPage onScanStart={bumpHealth} />} />
            <Route path="/projects" element={<ProjectsPage />} />
            <Route path="/projects/:id" element={<ProjectDetailPage onChanged={bumpHealth} />} />
            <Route path="/assets" element={<AssetsPage />} />
            <Route path="/graph" element={<GraphPage />} />
            <Route path="/insights" element={<InsightsPage />} />
            <Route path="/opportunities" element={<OpportunitiesPage />} />
            <Route path="/analyst" element={<AnalystPage />} />
            <Route path="/search" element={<SearchPage />} />
            <Route path="/settings" element={<SettingsPage onAppearanceChange={appearance.syncFrom} />} />
            <Route path="/jobs" element={<JobsPage />} />
            {/* 🔴 未知路径回首页而非 404 空白页：
                单机应用没有"不存在的资源"这种概念，回首页最符合预期。 */}
            <Route path="*" element={<Navigate to="/" replace />} />
          </Routes>
        </main>
      </div>
    ),
    [health, status, appearance.theme, appearance.syncFrom, handleThemeChange, handleSearch, navigate, bumpHealth],
  );

  // ── 服务未就绪：全屏引导，不挂载路由 ──────────────────────
  if (status !== "online") {
    return <ServiceGate status={status} reason={reason} onRetry={recheck} />;
  }

  return <ProgressProvider>{shell}</ProgressProvider>;
}

/**
 * 服务未连接时的全屏引导。
 *
 * 🔴 必须给出**可执行的启动命令**，而不只是"服务未连接"。
 * 用户此刻最需要的信息是"我该怎么把它启动起来"。
 * 这是单机桌面应用最常见的首次使用障碍，
 * 一句准确的命令能省掉用户去翻 README 的时间。
 */
function ServiceGate({
  status,
  reason,
  onRetry,
}: {
  status: "checking" | "offline";
  reason: string | null;
  onRetry: () => void;
}) {
  if (status === "checking") {
    return (
      <div className="service-gate">
        <div className="service-gate-card">
          <Icon name="refresh" style={{ animation: "spin 1.2s linear infinite" }} />
          <h1>正在连接本地服务…</h1>
          <p>projectAssests 的前端需要连接本地 Rust 服务才能读取你的项目数据。</p>
        </div>
      </div>
    );
  }

  return (
    <div className="service-gate">
      <div className="service-gate-card">
        <div className="service-gate-icon">
          <Icon name="alert" />
        </div>
        <h1>本地服务未连接</h1>
        <p>{reason ?? "无法连接到 projectAssests 本地服务。"}</p>

        <div className="service-gate-steps">
          <div className="t">启动服务</div>
          <p>在项目根目录执行：</p>
          <pre className="code-block">
            <code>
              {`source scripts/msvc-env.sh
cargo run -p projectassests-server`}
            </code>
          </pre>
          <p style={{ marginTop: 8 }}>
            服务默认监听 <code>127.0.0.1:8787</code>。启动后本页面会自动恢复（每 3 秒探测一次），
            无需手动刷新。
          </p>
        </div>

        <div style={{ display: "flex", gap: 8, marginTop: 16 }}>
          <button className="btn btn--primary btn--sm" onClick={onRetry}>
            <Icon name="refresh" /> 立即重试
          </button>
        </div>
      </div>
    </div>
  );
}
