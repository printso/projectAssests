/* ==========================================================================
   Project Intelligence Prototype — App Shell / Router / 页面渲染
   设计来源: docs/设计图/首页.png、项目预览页.png、全局.png
   交互状态覆盖: 默认 / 悬停(CSS) / 选中 / 禁用 / 空数据 / 加载中 / 错误
   ========================================================================== */
(function () {
  "use strict";
  const M = window.MOCK;
  const S = M.scale;          // 量级口径 (设计稿数字, 单一数据源)
  const KC = M.kind_colors;   // 图谱节点分类色板

  /* ---------------- Icons (inline SVG, stroke=currentColor) ---------------- */
  const P = (d) => '<path d="' + d + '" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>';
  const ICONS = {
    home:    P("M3 10.5 12 3l9 7.5M5 9.5V21h14V9.5"),
    folder:  P("M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"),
    box:     P("M12 3 3 7.5v9L12 21l9-4.5v-9zM3 7.5 12 12l9-4.5M12 12v9"),
    book:    P("M4 5a2 2 0 0 1 2-2h13v18H6a2 2 0 0 0-2 2zM8 3v18"),
    graph:   '<circle cx="6" cy="6" r="2.4" fill="none" stroke="currentColor" stroke-width="1.7"/><circle cx="18" cy="7" r="2.4" fill="none" stroke="currentColor" stroke-width="1.7"/><circle cx="12" cy="18" r="2.4" fill="none" stroke="currentColor" stroke-width="1.7"/>' + P("M8 7l7.6.6M7 8l3.6 7.6M16.6 9l-3.4 6.8"),
    drop:    P("M12 3s6 6.6 6 11a6 6 0 0 1-12 0c0-4.4 6-11 6-11z"),
    spark:   P("M12 3v4M12 17v4M3 12h4M17 12h4M6 6l2.5 2.5M15.5 15.5 18 18M18 6l-2.5 2.5M8.5 15.5 6 18"),
    plug:    P("M9 3v6M15 3v6M6 9h12v3a6 6 0 0 1-12 0zM12 18v3"),
    skill:   P("M8 8a4 4 0 1 1 8 0M8 16a4 4 0 1 0 8 0M4 12h16"),
    gear:    '<circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" stroke-width="1.7"/>' + P("M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9 17 7M7 17l-2.1 2.1"),
    help:    '<circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="1.7"/>' + P("M9.5 9.3a2.6 2.6 0 1 1 3.6 2.4c-.8.4-1.1 1-1.1 1.8M12 17h.01"),
    search:  '<circle cx="11" cy="11" r="6.5" fill="none" stroke="currentColor" stroke-width="1.7"/>' + P("M16 16l5 5"),
    bell:    P("M6 9a6 6 0 1 1 12 0c0 5 2 6 2 6H4s2-1 2-6M10 19a2 2 0 0 0 4 0"),
    cloud:   P("M7 18a4 4 0 0 1-.6-7.96A6 6 0 0 1 18 8.7 4.5 4.5 0 0 1 17.5 18z"),
    repeat:  P("M4 9a5 5 0 0 1 5-5h9m0 0-3-3m3 3-3 3M20 15a5 5 0 0 1-5 5H6m0 0 3 3m-3-3 3-3"),
    bulb:    P("M9 18h6M10 21h4M12 3a6 6 0 0 1 4 10.5c-.8.7-1 1.5-1 2.5h-6c0-1-.2-1.8-1-2.5A6 6 0 0 1 12 3z"),
    clock:   '<circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="1.7"/>' + P("M12 7v5l3.5 2"),
    doc:     P("M6 2h9l4 4v16H6zM14 2v5h5M9 12h7M9 16h7"),
    check:   P("M4 12.5 9.5 18 20 6.5"),
    link:    P("M9 15 15 9M8 12l-2.5 2.5a3.5 3.5 0 0 0 5 5L13 17M11 7l2.5-2.5a3.5 3.5 0 0 1 5 5L16 12"),
    grid:    P("M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z"),
    shield:  P("M12 3 5 6v6c0 4.5 3 7.5 7 9 4-1.5 7-4.5 7-9V6z"),
    tree:    P("M12 3v18M12 8h6M12 13H6M12 18h6"),
    video:   P("M3 7h12v10H3zM15 10l6-3v10l-6-3"),
    user:    '<circle cx="12" cy="8" r="3.5" fill="none" stroke="currentColor" stroke-width="1.7"/>' + P("M5 20a7 7 0 0 1 14 0"),
    flow:    P("M5 5h5v5H5zM14 14h5v5h-5zM10 7.5h6.5V14"),
    star:    P("M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.8-5.2-2.8-5.2 2.8 1-5.8L3.5 9.7l5.9-.8z"),
    x:       P("M6 6l12 12M18 6 6 18"),
    plus:    P("M12 5v14M5 12h14"),
    arr:     P("M5 12h14m0 0-5-5m5 5-5 5"),
    chev:    P("M9 6l6 6-6 6"),
    send:    P("M4 12 20 4l-6 16-3-6z"),
    code:    P("M8 7 3 12l5 5M16 7l5 5-5 5"),
    refresh: P("M20 11a8 8 0 1 0-2.3 6.3M20 5v6h-6"),
    alert:   P("M12 3 2 20h20zM12 9v5M12 17h.01"),
    menu:    P("M4 7h16M4 12h16M4 17h16"),
    edit:    P("M4 20h4L19 9l-4-4L4 16zM14 6l4 4"),
    sun:     '<circle cx="12" cy="12" r="4" fill="none" stroke="currentColor" stroke-width="1.7"/>' + P("M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M19.1 4.9l-1.4 1.4M6.3 17.7l-1.4 1.4"),
    moon:    P("M20 14.5A8.5 8.5 0 0 1 9.5 4a8.5 8.5 0 1 0 10.5 10.5z"),
    scan:    P("M4 8V5.5A1.5 1.5 0 0 1 5.5 4H8M16 4h2.5A1.5 1.5 0 0 1 20 5.5V8M20 16v2.5a1.5 1.5 0 0 1-1.5 1.5H16M8 20H5.5A1.5 1.5 0 0 1 4 18.5V16M4 12h16"),
    key:     '<circle cx="8" cy="14" r="4" fill="none" stroke="currentColor" stroke-width="1.7"/>' + P("M11 11 20 3M16 6l3 3"),
    palette: P("M12 3a9 9 0 1 0 0 18c1.5 0 2-1 2-2s-.7-1.8-2-1.8h-1A2 2 0 0 1 9 15a9 9 0 0 0 3-12zM7.5 10.5h.01M11 7.5h.01M15.5 9h.01"),
    db:      P("M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3zM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"),
    cpu:     P("M7 7h10v10H7zM9.5 4v3M14.5 4v3M9.5 17v3M14.5 17v3M4 9.5h3M4 14.5h3M17 9.5h3M17 14.5h3"),
    trash:   P("M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13M10 11v6M14 11v6"),
    eye:     P("M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z") + '<circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" stroke-width="1.7"/>',
    eyeoff:  P("M4 4l16 16M9.9 5.9A9.6 9.6 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a16 16 0 0 1-3.3 4M6.5 8A15.6 15.6 0 0 0 2.5 12S6 18.5 12 18.5c1.3 0 2.4-.3 3.5-.7M9.9 9.9a3 3 0 0 0 4.2 4.2")
  };
  const ic = (n, cls) => '<svg class="ico ' + (cls || "") + '" viewBox="0 0 24 24" aria-hidden="true">' + (ICONS[n] || ICONS.doc) + "</svg>";

  /* ---------------- Global state ---------------- */
  const LS_KEY = "spolia.theme";
  const state = {
    page: "overview",          // overview|projects|project|assets|graph|insights|analyst|mcp|opportunities|settings
    projectId: "p_yingtech",
    projectTab: "overview",
    assetFilter: "全部",
    insightFilter: "全部",
    graphView: "关系图",
    graphSelected: "g_ai",
    search: "",
    demo: "default",           // default|loading|empty|error
    mcpRunning: true,
    opportunities: M.opportunities.slice(),
    insightFeedback: {},
    /* ---- 本轮新增 ---- */
    theme: (function () { try { return localStorage.getItem(LS_KEY) || "dark"; } catch (e) { return "dark"; } })(),
    settingsTab: "llm",        // llm|scan|appearance|data
    llm: {                     // 大模型配置 (对应技术设计书 LlmProvider trait / 三级分析路由)
      cloud_provider: "openai",
      cloud_base_url: "https://api.openai.com/v1",
      cloud_api_key: "sk-****-demo-key",
      cloud_model: "gpt-5-mini",
      local_backend: "ollama",
      local_model: "qwen3:8b",
      local_base_url: "http://127.0.0.1:11434",
      route_fast: "local",     // 快速分析(画像/分类)
      route_deep: "cloud",     // 深度分析(洞察/机会)
      sensitive_local_only: true,
      embedding_local_only: true,
      show_key: false,
      testing: false,
      test_result: null        // {ok:boolean,msg:string}
    },
    scan: {                    // 首页扫描按钮 → 扫描流程状态
      running: false,
      progress: 0,
      found: 0,
      stage: "",               // 当前阶段文案
      log: [],
      dirs: ["D:/Projects", "D:/Code", "F:/CodeProject"]
    }
  };

  function applyTheme() {
    document.documentElement.setAttribute("data-theme", state.theme);
    try { localStorage.setItem(LS_KEY, state.theme); } catch (e) { /* file:// 下可能受限, 忽略 */ }
  }

  const $ = (sel, root) => (root || document).querySelector(sel);
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

  /* ---------------- Shell ---------------- */
  const NAV = [
    { group: null, items: [
      { key: "overview", icon: "home", label: "My R&D", count: null },
      { key: "projects", icon: "folder", label: "项目", count: String(S.projects) },
      { key: "assets", icon: "box", label: "资产", count: S.assets.toLocaleString() },
      { key: "knowledge", icon: "book", label: "知识", count: String(S.knowledge) },
      { key: "graph", icon: "graph", label: "知识图谱", count: null },
      { key: "insights", icon: "drop", label: "洞察", count: String(S.insights) }
    ]},
    { group: "AI ACCESS", items: [
      { key: "analyst", icon: "spark", label: "AI 分析师", count: null },
      { key: "mcp", icon: "plug", label: "MCP", dot: "已连接" },
      { key: "skills", icon: "skill", label: "Skills", count: String(S.skills) }
    ]},
    { group: null, items: [
      { key: "settings", icon: "gear", label: "设置", count: null },
      { key: "help", icon: "help", label: "帮助", count: null }
    ]}
  ];
  /* settings 已实现; 仅 help 保留禁用 (单机版无用户体系) */
  const PAGE_OF_NAV = { knowledge: "assets", skills: "mcp", help: null };

  function renderShell() {
    const nav = NAV.map((g) =>
      '<div class="nav-group">' +
      (g.group ? '<div class="nav-label">' + g.group + "</div>" : "") +
      g.items.map((it) => {
        const target = PAGE_OF_NAV[it.key] !== undefined ? PAGE_OF_NAV[it.key] : it.key;
        /* active 以导航项自身 key 判定 (knowledge→assets / skills→mcp 为跳转映射, 不高亮) */
        const active = it.key === state.page || (it.key === "projects" && state.page === "project");
        const dis = target === null;
        return '<button class="nav-item' + (active ? " is-active" : "") + '"' + (dis ? ' disabled title="原型未覆盖"' : ' data-nav="' + target + '"') + ">" +
          ic(it.icon) + '<span class="txt">' + it.label + "</span>" +
          (it.count ? '<span class="count">' + it.count + "</span>" : "") +
          (it.dot ? '<span class="badge-dot">' + it.dot + "</span>" : "") +
          "</button>";
      }).join("") +
      "</div>"
    ).join("");

    return (
      '<aside class="sidebar">' +
        '<div class="brand"><div class="brand-logo">' + esc(M.brand.logo) + '</div><div><div class="brand-name">' + esc(M.brand.name) + '</div><div class="brand-sub">' + esc(M.brand.sub) + "</div></div></div>" +
        nav +
        '<div class="sidebar-foot">' +
          '<div class="index-card"><div class="row"><span>' + ic("refresh") + " 本地索引中…</span><span>68%</span></div>" +
          '<div class="progress"><i style="width:68%"></i></div>' +
          '<div class="meta">正在分析: 3 个项目<br>预计剩余: 2 分钟</div></div>' +
          '<div class="slogan">"' + esc(M.brand.tagline).replace("，", "，<br>") + '"</div>' +
          '<svg class="slogan-wave" width="120" height="26" viewBox="0 0 120 26"><path d="M0 20 L15 12 L30 18 L45 6 L60 16 L75 9 L90 19 L105 11 L120 17" fill="none" stroke="#6366f1" stroke-width="1.4"/></svg>' +
        "</div>" +
      "</aside>" +
      '<header class="topbar">' +
        '<button class="icon-btn" id="btn-menu" title="菜单">' + ic("menu") + "</button>" +
        '<label class="searchbox">' + ic("search") + '<input id="global-search" placeholder="搜索项目、文件、能力、知识、经验…" value="' + esc(state.search) + '"><span class="kbd">⌘K</span></label>' +
        '<div class="topbar-right">' +
          '<span class="chip-btn" data-nav="analyst">' + ic("spark") + " AI 助手</span>" +
          '<span class="sync-chip"><span class="dot"></span>本地数据库 · 单机模式</span>' +
          /* 主题切换: 单机版无账号体系, 顶栏以主题开关收尾 */
          '<div class="theme-switch" role="group" aria-label="主题切换">' +
            '<button id="theme-light" class="' + (state.theme === "light" ? "is-active" : "") + '" title="亮色主题">' + ic("sun") + "</button>" +
            '<button id="theme-dark" class="' + (state.theme === "dark" ? "is-active" : "") + '" title="暗色主题">' + ic("moon") + "</button>" +
          "</div>" +
          '<button class="icon-btn" id="btn-settings" data-nav="settings" title="设置">' + ic("gear") + "</button>" +
        "</div>" +
      "</header>"
    );
  }

  /* ---------------- Shared renderers ---------------- */
  function statCard(s) {
    return '<div class="stat"><div class="ico-box" style="background:' + s.color + '22;color:' + s.color + '">' + ic(s.icon) + "</div>" +
      '<div class="label">' + s.label + '</div><div class="value">' + s.value + "</div>" +
      '<div class="delta">↑ ' + s.delta + ' <span class="muted">本月</span></div></div>';
  }
  function discItem(d) {
    const badgeCls = { high: "badge--high", potent: "badge--potent", info: "badge--info" }[d.badge] || "badge--muted";
    const badgeTxt = { high: "高价值", potent: "高潜力", info: "建议查看" }[d.badge] || d.badge;
    return '<button class="disc-item" data-nav="insights"><div class="disc-ico" style="background:' + d.color + '">' + ic(d.icon) + "</div>" +
      '<div class="disc-body"><div class="disc-title-row"><span class="disc-title">' + esc(d.title) + '</span><span class="badge ' + badgeCls + '">' + badgeTxt + "</span></div>" +
      '<div class="disc-desc">' + esc(d.description) + "</div>" +
      '<div class="disc-tags">' + d.tags.map((t) => '<span class="tag">' + esc(t) + "</span>").join("") + "</div></div>" +
      '<span class="disc-arrow">' + ic("chev") + "</span></button>";
  }
  function graphSVG(nodes, edges, selectedId) {
    const W = 600, H = 420;
    const px = (n) => ({ x: (n.x / 100) * W, y: (n.y / 100) * H });
    const byId = {}; nodes.forEach((n) => (byId[n.id] = n));
    let s = '<svg class="graph-svg" viewBox="0 0 ' + W + " " + H + '" role="img" aria-label="能力关系图谱">';
    s += '<defs><filter id="glow" x="-60%" y="-60%" width="220%" height="220%"><feGaussianBlur stdDeviation="6" result="b"/><feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter></defs>';
    edges.forEach(([a, b]) => {
      const A = px(byId[a]), B = px(byId[b]);
      s += '<line x1="' + A.x + '" y1="' + A.y + '" x2="' + B.x + '" y2="' + B.y + '" stroke="var(--color-graph-edge)" stroke-width="1"/>';
    });
    for (let i = 0; i < 26; i++) {
      const x = ((i * 97) % W), y = ((i * 173) % H);
      s += '<circle cx="' + x + '" cy="' + y + '" r="' + (1 + (i % 3) * 0.7) + '" fill="rgba(148,163,184,' + (0.12 + (i % 4) * 0.06) + ')"/>';
    }
    nodes.forEach((n) => {
      const p = px(n);
      const sel = n.id === selectedId;
      s += '<g class="gnode" data-node="' + n.id + '" style="cursor:pointer">' +
        '<circle cx="' + p.x + '" cy="' + p.y + '" r="' + (n.r + 7) + '" fill="' + n.color + '" opacity="' + (sel ? 0.28 : 0.12) + '"/>' +
        '<circle cx="' + p.x + '" cy="' + p.y + '" r="' + n.r + '" fill="' + n.color + '26" stroke="' + n.color + '" stroke-width="' + (sel ? 2.4 : 1.4) + '" filter="url(#glow)"/>' +
        '<circle cx="' + p.x + '" cy="' + p.y + '" r="' + Math.max(4, n.r * 0.32) + '" fill="' + n.color + '"/>' +
        '<text x="' + p.x + '" y="' + (p.y + n.r + 15) + '" text-anchor="middle" fill="var(--color-graph-label)" font-size="11" font-family="Inter,PingFang SC,sans-serif">' + esc(n.label) + "</text></g>";
    });
    return s + "</svg>";
  }
  function assetCard(a) {
    const typeMap = { component: "组件", code: "代码", knowledge: "知识", decision: "决策", experience: "经验", idea: "创意", prompt: "Prompt", outcome: "结果" };
    const score = a.reuse_score >= 0.85 ? ["badge--high", "高价值"] : a.reuse_score >= 0.7 ? ["badge--potent", "重要"] : ["badge--muted", "一般"];
    return '<article class="asset-card" data-asset="' + a.id + '">' +
      '<div class="head"><div class="ico-box" style="background:' + a.color + '">' + ic(a.type === "decision" ? "shield" : a.type === "experience" ? "clock" : a.type === "idea" ? "bulb" : a.type === "knowledge" ? "book" : "code") + "</div>" +
      '<div class="name">' + esc(a.name) + '</div><span class="badge ' + score[0] + '">' + score[1] + "</span></div>" +
      '<div class="type">' + (typeMap[a.type] || a.type) + " · reuse_score " + a.reuse_score.toFixed(2) + "</div>" +
      '<div class="desc">' + esc(a.description) + "</div>" +
      '<div class="disc-tags">' + a.tags.slice(0, 3).map((t) => '<span class="tag">' + esc(t) + "</span>").join("") + "</div>" +
      '<div class="meta">' + esc(a.meta) + "</div>" +
      '<div class="foot"><span class="link-more">查看 →</span><span class="tag mono">' + esc(a.source_path.split("/")[0]) + "</span></div>" +
      "</article>";
  }
  function capBars(list) {
    return list.map((c) => '<div class="cap-row"><span>' + esc(c.name) + '</span><span class="bar"><i style="width:' + c.pct + '%"></i></span><span class="pct">' + c.pct + "%</span></div>").join("");
  }
  function healthRing(v) {
    const C = 2 * Math.PI * 26;
    return '<div class="ring"><svg width="64" height="64"><circle cx="32" cy="32" r="26" fill="none" stroke="var(--color-panel-3)" stroke-width="6"/>' +
      '<circle cx="32" cy="32" r="26" fill="none" stroke="var(--color-success)" stroke-width="6" stroke-linecap="round" stroke-dasharray="' + (C * v / 100) + " " + C + '"/></svg><span class="val">' + v + "</span></div>";
  }
  function stateBox(kind, msg) {
    if (kind === "loading")
      return '<div style="display:grid;gap:12px;padding:8px 0">' + [70, 90, 60, 80].map((h) => '<div class="skeleton" style="height:' + h + 'px"></div>').join("") + "</div>";
    if (kind === "empty")
      return '<div class="state-box"><div class="big">' + ic("search") + '</div><div class="t">暂无数据</div><div>' + esc(msg || "没有匹配的结果，试试调整筛选条件或搜索词。") + '</div><button class="btn btn--ghost btn--sm" data-demo="default">清除筛选</button></div>';
    return '<div class="state-box"><div class="big" style="color:var(--color-danger)">' + ic("alert") + '</div><div class="t">加载失败</div><div>' + esc(msg || "本地索引服务无响应（模拟错误状态）。") + '</div><button class="btn btn--primary btn--sm" data-demo="default">' + ic("refresh") + " 重试</button></div>";
  }
  function demoWrap(contentHTML) {
    if (state.demo === "loading") return stateBox("loading");
    if (state.demo === "empty") return stateBox("empty");
    if (state.demo === "error") return stateBox("error");
    return contentHTML;
  }

  /* ---------------- Pages ---------------- */
  function pageOverview() {
    const o = M.overview;
    return (
      '<div class="shell-2" style="display:grid;gap:16px;align-items:start">' +
      "<div>" +
        /* hero */
        '<section class="card hero-grid" style="background:var(--grad-hero);border-color:rgba(139,92,246,.35);display:grid;gap:16px;overflow:hidden">' +
          '<div style="padding:8px 0"><h1 style="font-size:var(--fs-2xl);font-weight:700;white-space:nowrap">' + o.greeting + "</h1>" +
          '<p style="color:var(--color-hero-text);margin-top:10px;font-size:var(--fs-md);max-width:340px">' + o.summary + "</p></div>" +
          o.stats.map(statCard).join("") +
        "</section>" +
        /* 项目扫描条 (首页新增: 手动触发全量/增量扫描) */
        '<section class="card" style="margin-top:16px;padding:12px 16px">' +
          (state.scan.running ? (
            '<div style="display:flex;align-items:center;gap:14px">' +
              '<span style="color:var(--color-primary-2);flex:none">' + ic("scan") + "</span>" +
              '<div style="flex:1;min-width:0">' +
                '<div style="display:flex;justify-content:space-between;font-size:var(--fs-sm);margin-bottom:6px">' +
                  '<span>' + esc(state.scan.stage || "准备扫描…") + '</span><span class="mono">' + state.scan.progress + "% · 发现 " + state.scan.found + " 个项目</span>" +
                "</div>" +
                '<div class="progress" style="margin:0"><i style="width:' + state.scan.progress + '%"></i></div>' +
              "</div>" +
              '<button class="btn btn--ghost btn--sm" id="scan-cancel">' + ic("x") + " 取消</button>" +
            "</div>" +
            (state.scan.log.length ? '<div class="scan-log" style="margin-top:10px">' + state.scan.log.map((l) => "<div>› " + esc(l) + "</div>").join("") + "</div>" : "")
          ) : (
            '<div style="display:flex;align-items:center;gap:14px;flex-wrap:wrap">' +
              '<span style="width:34px;height:34px;border-radius:9px;background:rgba(99,102,241,.16);color:var(--color-primary-2);display:grid;place-items:center;flex:none">' + ic("scan") + "</span>" +
              '<div style="flex:1;min-width:200px"><div style="font-size:var(--fs-md);font-weight:600">项目扫描</div>' +
              '<div style="font-size:var(--fs-xs);color:var(--color-text-3)">扫描 3 个目录 · 上次扫描 2 小时前 · 增量模式仅重算变更文件</div></div>' +
              '<button class="btn btn--ghost btn--sm" data-scan="incremental">增量扫描</button>' +
              '<button class="btn btn--primary btn--sm" data-scan="full">' + ic("refresh") + " 全量扫描</button>" +
            "</div>"
          )) +
        "</section>" +
        /* discoveries + graph */
        '<div class="grid cols-2" style="margin-top:16px">' +
          '<section class="card"><div class="card-head"><div><div class="card-title">' + ic("spark") + ' AI 发现</div><div class="card-sub">基于你的历史项目，AI 为你发现了以下价值</div></div><button class="link-more" data-nav="insights">更多发现 ' + ic("arr") + "</button></div>" +
          demoWrap(M.insights.map(discItem).join("")) + "</section>" +
          '<section class="card"><div class="card-head"><div class="card-title">' + ic("graph") + ' 我的研发能力图谱</div><button class="link-more" data-nav="graph">查看完整图谱 ' + ic("arr") + "</button></div>" +
          '<div class="chips" style="margin-bottom:10px">' + ["全部", "AI", "Web", "数据", "基础设施", "多媒体"].map((c, i) => '<button class="chip' + (i === 0 ? " is-active" : "") + '" data-capchip="' + c + '">' + c + "</button>").join("") + "</div>" +
          '<div class="graph-wrap">' + graphSVG(o.graph.nodes, o.graph.edges, "ai") + "</div>" +
          '<div class="legend">' + [["能力", KC.capability], ["项目", KC.project], ["代码", KC.code], ["知识", KC.knowledge], ["经验", KC.experience], ["关系", KC.relation]].map((l) => "<span><i style=\"background:" + l[1] + "\"></i>" + l[0] + "</span>").join("") + "</div>" +
          '<div class="graph-stats"><div><div class="k">节点总数</div><div class="v">' + o.graph.node_count + '</div></div><div><div class="k">关系总数</div><div class="v">' + o.graph.relation_count + "</div></div></div>" +
          "</section>" +
        "</div>" +
        /* quick entries */
        '<section class="card" style="margin-top:16px"><div class="card-head"><div class="card-title">' + ic("grid") + " 快速入口</div></div>" +
        '<div class="quick">' +
          [["folder", "#8b5cf6", "我的项目", "管理与浏览所有项目", "projects"],
           ["search", "#22c55e", "资产搜索", "查找可复用的代码/组件/方案", "assets"],
           ["spark", "#6366f1", "AI 分析师", "提问你的研发历史", "analyst"],
           ["plus", "#3b82f6", "创建新项目", "基于已有资产快速启动", null]].map((q) =>
            '<button class="quick-item"' + (q[4] ? ' data-nav="' + q[4] + '"' : ' data-toast="原型演示：创建新项目入口"') + '><div class="ico-box" style="background:' + q[1] + '26;color:' + q[1] + '">' + ic(q[0]) + '</div><div><div class="t">' + q[2] + '</div><div class="d">' + q[3] + "</div></div></button>").join("") +
        "</div></section>" +
      "</div>" +
      /* right rail */
      '<aside class="rail">' +
        '<section class="card"><div class="card-head"><div class="card-title">' + ic("spark") + ' AI 助手</div><span class="badge-dot">在线</span></div>' +
        '<div class="assist-item" style="background:rgba(99,102,241,.14);border-color:rgba(99,102,241,.4);color:var(--color-tree-active)"><span class="ico">' + ic("spark") + '</span>我可以帮你：</div>' +
        o.assistant_actions.map((a) => '<button class="assist-item" data-ask="' + esc(a.text) + '"><span class="ico">' + ic(a.icon) + "</span>" + a.text + '<span class="arr">' + ic("chev") + "</span></button>").join("") +
        '<div class="assist-input"><input id="assist-q" placeholder="告诉我你想了解的内容…"><button class="send" id="assist-send" title="发送">' + ic("send") + "</button></div>" +
        '<div class="assist-hint">支持自然语言提问，或使用上方推荐功能</div></section>' +
        '<section class="card"><div class="card-head"><div class="card-title">' + ic("clock") + ' 最近活动</div><button class="link-more">查看全部 ' + ic("arr") + "</button></div>" +
        o.activities.map((a) => '<div class="activity-item"><div class="activity-ico" style="background:' + a.color + '">' + ic(a.icon) + '</div><div><div class="activity-t">' + esc(a.title) + '</div><div class="activity-d">' + esc(a.desc) + "</div></div></div>").join("") + "</section>" +
        '<div class="promo" data-nav="mcp"><div class="t">让 AI 访问你的研发记忆</div><div class="d">已支持 Cursor、Claude Code、Codex 等</div><div style="position:absolute;right:14px;top:14px;color:#c7d2fe">' + ic("plug") + "</div></div>" +
      "</aside></div>"
    );
  }

  function pageProjects() {
    const q = state.search.trim().toLowerCase();
    let list = M.projects;
    if (q) list = list.filter((p) => (p.name + p.description + p.tags.join("")).toLowerCase().includes(q));
    const cards = list.map((p) =>
      '<button class="asset-card" data-open-project="' + p.id + '" style="text-align:left">' +
      '<div class="head"><div class="ico-box" style="background:' + p.cover_color + '">' + ic("folder") + '</div><div class="name">' + esc(p.name) + '</div>' +
      '<span class="badge ' + (p.status === "active" ? "badge--high" : p.status === "abandoned" ? "badge--muted" : "badge--info") + '">' + ({ active: "Active", paused: "Paused", abandoned: "Archived", experimental: "实验" }[p.status]) + "</span></div>" +
      '<div class="type mono">' + esc(p.language) + " · " + esc(p.framework) + "</div>" +
      '<div class="desc">' + esc(p.description) + "</div>" +
      '<div class="disc-tags">' + p.tags.slice(0, 4).map((t) => '<span class="tag">' + esc(t) + "</span>").join("") + "</div>" +
      '<div class="foot"><span class="meta">更新 ' + esc(p.updated_at) + '</span><span class="link-more">打开 →</span></div></button>'
    ).join("");
    return '<div class="page-head"><div><div class="page-title">项目</div><div class="page-sub">共 ' + S.projects + ' 个项目 · 点击查看详情与资产</div></div>' +
      '<div class="chips">' + ["全部", "Active", "Paused", "Archived"].map((c, i) => '<button class="chip' + (i === 0 ? " is-active" : "") + '">' + c + "</button>").join("") + "</div></div>" +
      demoWrap(list.length ? '<div class="asset-grid">' + cards + "</div>" : stateBox("empty", "没有匹配「" + state.search + "」的项目。"));
  }

  function pageProject() {
    const p = M.projects.find((x) => x.id === state.projectId) || M.projects[0];
    const tab = state.projectTab;
    const tree = p.files.map((f) =>
      '<li class="lvl' + f.level + (f.active ? " is-active" : "") + '">' +
      (f.type === "dir" ? '<span class="dir">' + ic("folder") + "</span>" : ic("doc")) + esc(f.name) + "</li>").join("");
    const code = p.code.map((line, i) =>
      '<div class="code-line"><span class="ln">' + (i + 1) + '</span><span>' +
      (line.length ? line.map((t) => '<span class="' + (t.t ? "tok-" + t.t : "") + '">' + esc(t.v) + "</span>").join("") : "&nbsp;") +
      "</span></div>").join("");

    /* Tab 计数: 按该项目 mock 资产动态计算 (单一数据源, 不再硬编码) */
    const mine = M.assets.filter((a) => a.project_id === p.id);
    const tabCount = {
      assets: mine.length,
      knowledge: mine.filter((a) => a.type === "knowledge").length,
      decisions: mine.filter((a) => a.type === "decision").length,
      experience: mine.filter((a) => a.type === "experience").length
    };

    let body = "";
    if (tab === "structure") {
      const st = p.structure || { files: 0, loc: "-", symbols: 0, modules: 0, langs: [] };
      const ar = p.archaeology;
      body =
        '<div class="grid cols-2" style="margin-top:16px">' +
          '<section class="card"><div class="card-head"><div class="card-title">' + ic("grid") + " 代码结构统计</div><span class=\"badge badge--muted\">Level 0 静态分析</span></div>" +
          '<div class="graph-stats" style="border-top:none;margin-top:0;padding-top:0">' +
            [["文件", st.files.toLocaleString()], ["代码行", st.loc], ["符号", st.symbols], ["模块", st.modules]].map((x) =>
              '<div><div class="k">' + x[0] + '</div><div class="v">' + x[1] + "</div></div>").join("") +
          "</div>" +
          '<div class="card-title" style="font-size:12px;margin:14px 0 6px">' + ic("code") + " 语言构成</div>" +
          capBars(st.langs.map((l) => ({ name: l.name, pct: l.pct }))) +
          '<div class="card-title" style="font-size:12px;margin:14px 0 6px">' + ic("folder") + " 目录树</div>" +
          '<ul class="tree">' + tree + "</ul></section>" +
          '<section class="card accent-panel">' +
          '<div class="card-head"><div class="card-title accent-title">' + ic("clock") + " Project Archaeology</div><span class=\"badge badge--info\">项目考古</span></div>" +
          (ar ? '<div class="graph-stats" style="border-top:none;margin-top:0;padding-top:0">' +
            [["AI Sessions", ar.sessions], ["Commits", ar.commits], ["完成度", Math.round(ar.completeness * 100) + "%"]].map((x) =>
              '<div><div class="k">' + x[0] + '</div><div class="v">' + x[1] + "</div></div>").join("") + "</div>" +
            '<p class="accent-text" style="font-size:13px;margin:12px 0">' + esc(ar.narrative) + "</p>" +
            '<div class="card-title accent-soft" style="font-size:12px;margin:6px 0 6px">' + ic("box") + " 当前阶段</div>" +
            '<div style="font-size:13px;color:var(--color-text)">' + esc(ar.phase) + "</div>" +
            '<div class="card-title accent-soft" style="font-size:12px;margin:12px 0 6px">' + ic("star") + " 可打捞资产</div>" +
            '<div class="chips">' + ar.salvage.map((s2) => '<span class="chip" style="cursor:default">' + esc(s2) + "</span>").join("") + "</div>"
            : stateBox("empty", "该项目缺少 Git 历史，无法生成考古报告。")) +
          "</section>" +
        "</div>";
    } else if (tab === "overview") {
      body =
        '<div class="grid cols-3" style="margin-top:16px">' +
          '<section class="card" style="padding:0;overflow:hidden"><div class="card-head" style="padding:12px 16px 8px"><div class="card-title">' + ic("code") + ' 代码预览</div><span class="badge badge--muted">核心模块</span></div>' +
          '<div class="cols-code" style="display:grid;min-height:340px">' +
            '<div style="border-right:1px solid var(--color-border);padding:8px;overflow:auto"><ul class="tree">' + tree + "</ul></div>" +
            '<div class="code-pane" style="border:none;border-radius:0"><div class="code-head" style="white-space:nowrap">' + ic("doc") + '<span style="overflow:hidden;text-overflow:ellipsis">services / video_service.py</span>' +
            '<span style="margin-left:auto" class="tag mono">Python</span></div><div class="code-body" style="overflow:auto;max-height:380px">' + code + "</div></div>" +
          "</div></section>" +
          '<section class="card"><div class="card-head"><div class="card-title">' + ic("star") + " 项目亮点</div></div>" +
          p.highlights.map((h) => '<button class="disc-item" style="margin-bottom:10px"><div class="disc-ico" style="width:32px;height:32px;background:' + h.color + '">' + ic(h.icon) + '</div><div class="disc-body"><div class="disc-title" style="font-size:13px">' + esc(h.title) + '</div><div class="disc-desc">' + esc(h.desc) + "</div></div>" + ic("chev") + "</button>").join("") + "</section>" +
          '<section class="card accent-panel"><div class="card-head"><div class="card-title accent-title">' + ic("spark") + ' AI 资产分析</div><span class="badge badge--info">基于 12 个项目分析</span></div>' +
          '<p class="accent-text" style="font-size:13px">这个项目为你贡献了 <b style="color:var(--color-text)">14</b> 个高价值资产</p>' +
          '<div class="grid cols-3s" style="margin:12px 0">' +
            [["code", "7", "可复用组件"], ["bulb", "3", "关键知识"], ["link", "2", "被引用"]].map((x) =>
              '<div class="accent-box"><div>' + ic(x[0]) + '</div><div class="bx-v">' + x[1] + '</div><div class="bx-k">' + x[2] + "</div></div>").join("") +
          "</div>" +
          '<div class="card-title" style="font-size:12px;margin:8px 0 6px">' + ic("grid") + " 能力覆盖</div>" + capBars(p.capabilities) +
          '<button class="link-more" style="margin-top:8px">查看完整分析 ' + ic("arr") + "</button></section>" +
        "</div>" +
        '<div class="grid cols-2" style="margin-top:16px">' +
          '<section class="card"><div class="card-head"><div class="card-title">' + ic("box") + " 核心资产</div></div>" +
          '<div class="chips" style="margin-bottom:12px">' + ["全部", "代码", "组件", "知识", "决策", "经验", "其他"].map((c, i) => '<button class="chip' + (i === 0 ? " is-active" : "") + '" data-pchip="' + c + '">' + c + "</button>").join("") + "</div>" +
          demoWrap('<div class="asset-grid">' + M.assets.slice(0, 3).map(assetCard).join("") + "</div>") + "</section>" +
          '<section class="card"><div class="card-head"><div class="card-title">' + ic("link") + " 相关项目</div></div>" +
          graphSVG([{ id: "yt", label: "yingTech", color: "#a855f7", x: 50, y: 50, r: 20, core: true },
                    { id: "it", label: "image-tool", color: "#22c55e", x: 30, y: 22, r: 13 },
                    { id: "vl", label: "videoLab", color: "#3b82f6", x: 74, y: 26, r: 13 },
                    { id: "ng", label: "NovelGenerator", color: "#eab308", x: 24, y: 74, r: 13 },
                    { id: "ap", label: "agent-platform", color: "#14b8a6", x: 52, y: 84, r: 13 },
                    { id: "pe", label: "prompt-engine", color: "#ec4899", x: 78, y: 70, r: 13 }],
                   [["yt", "it"], ["yt", "vl"], ["yt", "ng"], ["yt", "ap"], ["yt", "pe"]], "yt") +
          '<div style="display:flex;justify-content:space-between;align-items:center;margin-top:8px"><div class="legend" style="margin:0"><span><i style="background:#a855f7"></i>当前项目</span><span><i style="background:#22c55e"></i>相关项目</span><span><i style="background:#3b82f6"></i>共享能力</span></div>' +
          '<button class="btn btn--ghost btn--sm" data-nav="graph">查看完整关系 ' + ic("arr") + "</button></div></section>" +
        "</div>";
    } else if (tab === "assets") {
      body = '<div class="asset-grid" style="margin-top:16px">' + M.assets.filter((a) => a.project_id === p.id || a.reuse_score > 0.8).map(assetCard).join("") + "</div>";
    } else if (tab === "knowledge" || tab === "decisions" || tab === "experience") {
      const tmap = { knowledge: ["knowledge"], decisions: ["decision"], experience: ["experience"] };
      const list = M.assets.filter((a) => tmap[tab].includes(a.type));
      body = '<div class="asset-grid" style="margin-top:16px">' + (list.length ? list.map(assetCard).join("") : stateBox("empty", "该类型资产暂未提取完成。")) + "</div>";
    } else if (tab === "related") {
      const rel = M.relations.filter((r) => r.source_id === p.id && r.relation_type === "similar_to")
        .map((r) => M.projects.find((x) => x.id === r.target_id)).filter(Boolean);
      body = '<section class="card" style="margin-top:16px"><div class="card-head"><div class="card-title">' + ic("link") + ' 相似项目</div><span class="badge badge--info">similar_to</span></div>' +
        (rel.length ? '<div class="asset-grid">' + rel.map((r) =>
          '<button class="asset-card" data-open-project="' + r.id + '" style="text-align:left"><div class="head"><div class="ico-box" style="background:' + r.cover_color + '">' + ic("folder") + '</div><div class="name">' + esc(r.name) + '</div></div>' +
          '<div class="desc">' + esc(r.description) + '</div><div class="foot"><span class="meta">相似度 87%</span><span class="link-more">打开 →</span></div></button>').join("") + "</div>"
          : stateBox("empty", "暂未发现与该项目相似的历史项目。")) + "</section>";
    } else if (tab === "ai") {
      body = '<section class="card" style="margin-top:16px;max-width:720px"><div class="card-title">' + ic("spark") + " AI 分析摘要</div>" +
        '<p style="color:var(--color-text-2);margin:10px 0">该项目在 3 个项目中使用了相似的视频生成 Pipeline，存在代码复用机会；角色一致性方案可与 image-tool 项目组合，提升整体效果。</p>' +
        '<div class="chips">' + ["复用机会", "组合建议", "性能优化"].map((c) => '<span class="chip" style="cursor:default">' + c + "</span>").join("") + "</div></section>";
    } else {
      body = '<section class="card" style="margin-top:16px">' + stateBox("empty", "「" + M.project_tabs.find((t) => t.key === tab).label + "」视图在原型中未展开，验收时以设计图全局.png 第 2 屏 Tab 为准。") + "</section>";
    }

    return (
      '<section class="card proj-head" style="display:grid;gap:20px;align-items:start">' +
        '<div style="width:88px;height:88px;border-radius:14px;background:linear-gradient(140deg,' + p.cover_color + ',#312e81);display:grid;place-items:center;color:#fff;font-size:26px">' + ic("video") + "</div>" +
        "<div><div style=\"display:flex;align-items:center;gap:10px;flex-wrap:wrap\"><h1 style=\"font-size:22px;font-weight:700\">" + esc(p.name) + '</h1><span class="badge badge--high">● ' + ({ active: "活跃中", paused: "暂停", abandoned: "已归档", experimental: "实验" }[p.status]) + "</span></div>" +
        '<div style="color:var(--color-text-2);margin:4px 0 8px">⌁ AI 漫剧生成平台</div>' +
        '<p style="color:var(--color-text-2);font-size:13px;max-width:640px">' + esc(p.description) + "</p>" +
        '<div class="chips" style="margin-top:10px">' + p.tags.map((t) => '<span class="chip" style="cursor:default">' + esc(t) + "</span>").join("") + "</div></div>" +
        '<div style="display:flex;gap:16px;align-items:flex-start">' +
          '<div style="display:grid;gap:10px;font-size:12px;color:var(--color-text-2)">' +
            '<div><div style="color:var(--color-text-3)">项目创建</div><div class="mono" style="color:var(--color-text)">' + p.created_at + '</div></div>' +
            '<div><div style="color:var(--color-text-3)">最后更新</div><div class="mono" style="color:var(--color-text)">' + p.updated_at + '</div></div>' +
            '<div><div style="color:var(--color-text-3)">代码规模</div><div class="mono" style="color:var(--color-text)">' + p.code_size + "</div></div></div>" +
          '<div style="text-align:center"><div style="font-size:12px;color:var(--color-text-3);margin-bottom:4px">项目健康度</div>' + healthRing(p.health_score) + '<div style="font-size:11px;color:var(--color-success);margin-top:4px">良好</div></div>' +
          '<div style="display:flex;flex-direction:column;gap:8px"><button class="btn btn--ghost btn--sm" data-toast="原型演示：在资源管理器中打开">' + ic("folder") + ' 打开项目路径</button><button class="btn btn--ghost btn--sm" data-toast="原型演示：编辑信息">' + ic("edit") + ' 编辑信息</button><button class="btn btn--ghost btn--sm is-disabled" disabled>' + ic("x") + " 归档</button></div>" +
        "</div>" +
      "</section>" +
      '<div class="tabs" style="margin-top:16px">' + M.project_tabs.map((t) => '<button class="tab' + (t.key === state.projectTab ? " is-active" : "") + '" data-ptab="' + t.key + '">' + t.label + (tabCount[t.key] ? " (" + tabCount[t.key] + ")" : "") + "</button>").join("") + "</div>" +
      body
    );
  }

  function pageAssets() {
    const typeOf = { "全部": null, "代码": "code", "组件": "component", "知识": "knowledge", "决策": "decision", "经验": "experience", "创意": "idea", "Prompt": "prompt" };
    const q = state.search.trim().toLowerCase();
    let list = M.assets;
    const f = typeOf[state.assetFilter];
    if (f) list = list.filter((a) => a.type === f);
    if (q) list = list.filter((a) => (a.name + a.description + a.tags.join("")).toLowerCase().includes(q));
    return '<div class="page-head"><div><div class="page-title">资产库</div><div class="page-sub">发现你历史项目中积累的所有可复用资产</div></div>' +
      '<label class="searchbox" style="max-width:300px">' + ic("search") + '<input id="asset-search" placeholder="Search assets…" value="' + esc(state.search) + '"></label></div>' +
      '<div class="chips" style="margin-bottom:16px">' + Object.keys(typeOf).map((c) => '<button class="chip' + (c === state.assetFilter ? " is-active" : "") + '" data-afilter="' + c + '">' + c + "</button>").join("") + '<button class="chip" disabled>Outcome（未开放）</button></div>' +
      demoWrap(list.length ? '<div class="asset-grid">' + list.map(assetCard).join("") + "</div>" : stateBox("empty"));
  }

  function pageGraph() {
    const d = M.graph_detail;
    return '<div class="page-head"><div><div class="page-title">研发关系图谱</div><div class="page-sub">项目 · 能力 · 资产 · 知识 之间的关联网络（点击节点查看详情）</div></div>' +
      '<div class="chips">' + ["关系图", "时间线"].map((v) => '<button class="chip' + (v === state.graphView ? " is-active" : "") + '" data-gview="' + v + '"' + (v === "时间线" ? " disabled" : "") + ">" + v + "</button>").join("") + "</div></div>" +
      '<div class="cols-graph" style="display:grid;gap:16px">' +
        '<section class="card" style="padding:8px">' + demoWrap(graphSVG(M.graph_nodes, M.graph_edges, state.graphSelected)) + "</section>" +
        '<section class="card"><div class="card-head"><div class="card-title">' + ic("graph") + " " + esc(d.title) + '</div><span class="badge badge--info">' + d.badge + "</span></div>" +
        '<div style="font-size:12px;color:var(--color-text-3);margin-bottom:6px">Used in</div>' +
        d.used_in.map((u) => '<div class="assist-item" style="padding:6px 10px">' + ic("folder") + u + "</div>").join("") +
        '<div style="font-size:12px;color:var(--color-text-3);margin:12px 0 6px">Related</div>' +
        d.related.map((u) => '<div class="assist-item" style="padding:6px 10px">' + ic("link") + u + "</div>").join("") +
        '<div style="border-top:1px solid var(--color-border);margin-top:12px;padding-top:10px">' +
        d.stats.map((s) => '<div style="display:flex;justify-content:space-between;font-size:12px;padding:4px 0;color:var(--color-text-2)"><span>' + s.k + '</span><b style="color:var(--color-text)">' + s.v + "</b></div>").join("") + "</div></section>" +
      "</div>";
  }

  function pageInsights() {
    const filters = ["全部", "重复能力", "组合机会", "遗忘资产"];
    const fmap = { "重复能力": "duplicate_capability", "组合机会": "opportunity_hint", "遗忘资产": "forgotten_asset" };
    let list = M.insights;
    if (state.insightFilter !== "全部") list = list.filter((i) => i.type === fmap[state.insightFilter] || (state.insightFilter === "重复能力" && i.type === "reusable_experience"));
    return '<div class="page-head"><div><div class="page-title">洞察</div><div class="page-sub">AI 主动发现的重复实现、复用机会与被遗忘的资产</div></div>' +
      '<div class="chips">' + filters.map((c) => '<button class="chip' + (c === state.insightFilter ? " is-active" : "") + '" data-ifilter="' + c + '">' + c + "</button>").join("") + "</div></div>" +
      demoWrap(list.length ? list.map((i) => {
        const fb = state.insightFeedback[i.id];
        return '<section class="card" style="margin-bottom:14px"><div style="display:flex;gap:14px;align-items:flex-start">' +
          '<div class="disc-ico" style="background:' + i.color + '">' + ic(i.icon) + "</div>" +
          '<div style="flex:1;min-width:0"><div class="disc-title-row"><span class="disc-title">' + esc(i.title) + '</span><span class="badge ' + ({ high: "badge--high", potent: "badge--potent", info: "badge--info" }[i.badge]) + '">' + ({ high: "High Value", potent: "Powerful", info: "Suggested" }[i.badge]) + "</span></div>" +
          '<p style="color:var(--color-text-2);font-size:13px;margin:6px 0">' + esc(i.description) + "</p>" +
          '<details style="font-size:12px;color:var(--color-text-3)"><summary style="cursor:pointer">Evidence · confidence ' + i.confidence.toFixed(2) + "</summary>" +
          '<ul style="margin:6px 0 0 4px">' + i.evidence.map((e) => '<li class="mono" style="padding:2px 0">· ' + esc(e) + "</li>").join("") + "</ul></details>" +
          '<div style="display:flex;gap:8px;margin-top:10px;align-items:center">' +
            '<button class="btn btn--ghost btn--sm' + (fb === "useful" ? " is-active" : "") + '" data-fb="' + i.id + ":useful" + '" style="' + (fb === "useful" ? "border-color:var(--color-success);color:var(--color-success)" : "") + '">' + ic("check") + ' 有用</button>' +
            '<button class="btn btn--ghost btn--sm" data-fb="' + i.id + ':useless" style="' + (fb === "useless" ? "border-color:var(--color-danger);color:var(--color-danger)" : "") + '">' + ic("x") + ' 无用</button>' +
            '<span class="tag" style="margin-left:auto">created_at ' + i.created_at + "</span></div>" +
          "</div></div></section>";
      }).join("") : stateBox("empty", "该分类下暂无洞察。")) +
      '<section class="card"><div class="card-head"><div class="card-title">' + ic("star") + ' 机会发现</div><button class="link-more" data-nav="opportunities">全部机会 ' + ic("arr") + "</button></div>" +
      M.opportunities.map((o) => '<div class="disc-item" data-nav="opportunities"><div class="disc-ico" style="background:#f59e0b">' + ic("star") + '</div><div class="disc-body"><div class="disc-title-row"><span class="disc-title">' + esc(o.title) + '</span><span class="badge badge--potent">Opportunity</span></div><div class="disc-desc">' + esc(o.description) + '</div></div><span class="disc-arrow">' + ic("chev") + "</span></div>").join("") + "</section>";
  }

  function pageAnalyst() {
    return '<section class="card" style="max-width:760px;margin:40px auto;text-align:center;padding:48px 32px">' +
      '<div style="width:56px;height:56px;border-radius:16px;background:var(--grad-primary);display:grid;place-items:center;margin:0 auto 18px;color:#fff;box-shadow:var(--glow-primary)">' + ic("spark") + "</div>" +
      '<h1 style="font-size:22px;font-weight:700">What are you trying to discover?</h1>' +
      '<p style="color:var(--color-text-2);margin:8px 0 22px">我可以帮你分析历史项目、发现可复用资产、总结技术能力、找到历史经验</p>' +
      '<div class="assist-input" style="max-width:520px;margin:0 auto;padding:10px 10px 10px 16px"><input id="analyst-q" placeholder="输入你的问题，例如：我过去做过哪些视频生成项目？"><button class="send" id="analyst-send">' + ic("send") + "</button></div>" +
      '<div class="chips" style="justify-content:center;margin-top:18px">' + M.analyst_questions.map((q) => '<button class="chip" data-askq="' + esc(q) + '">' + esc(q) + "</button>").join("") + "</div>" +
      '<div id="analyst-result" style="margin-top:26px;text-align:left"></div></section>';
  }

  function pageMcp() {
    const m = M.mcp;
    const run = state.mcpRunning;
    return '<div class="page-head"><div><div class="page-title">MCP / Skills</div><div class="page-sub">让 AI Coding Agent 调用你的研发记忆（出口，不是发动机）</div></div>' +
      '<button class="btn ' + (run ? "btn--ghost" : "btn--primary") + '" id="mcp-toggle">' + (run ? ic("x") + " 停止服务" : ic("refresh") + " 启动服务") + "</button></div>" +
      (!run ? stateBox("error", "MCP Server 已停止，Cursor / Claude Code / Codex 无法访问你的研发记忆。") :
      '<div class="grid cols-2m">' +
        '<section class="card"><div class="card-head"><div class="card-title">' + ic("plug") + ' MCP Server</div><span class="badge badge--high">● Running</span></div>' +
        '<div style="font-size:12px;color:var(--color-text-3);margin-bottom:10px">Available to: ' + m.available_to.map((a) => '<span class="chip" style="cursor:default;margin-left:6px">' + a + "</span>").join("") + "</div>" +
        '<div style="font-size:12px;color:var(--color-text-3);margin:14px 0 8px">Tools (8)</div>' +
        '<div class="grid cols-2s" style="gap:8px">' + m.tools.map((t) => '<div class="assist-item mono" style="padding:7px 10px;font-size:12px">' + ic("code") + t.name + "</div>").join("") + "</div></section>" +
        '<section class="card"><div class="card-head"><div class="card-title">' + ic("skill") + ' Skills</div><button class="link-more">View all ' + ic("arr") + "</button></div>" +
        m.skills.map((s) => '<div class="assist-item mono" style="padding:7px 10px;font-size:12px;margin-bottom:8px">' + ic("doc") + s.name + "</div>").join("") +
        '<div class="assist-hint">Skill = AI 使用这套知识系统的方法论；MCP 提供实时数据与受控工具。</div></section>' +
      "</div>");
  }

  function pageOpportunities() {
    const list = state.opportunities;
    return '<div class="page-head"><div><div class="page-title">机会发现</div><div class="page-sub">AI 发现的潜在组合机会 —— 基于你历史项目的能力重合</div></div></div>' +
      demoWrap(list.length ? list.map((o) =>
        '<section class="card accent-panel" style="margin-bottom:14px">' +
        '<div class="disc-title-row"><span class="disc-title" style="font-size:16px">' + ic("star") + " " + esc(o.title) + '</span><span class="badge badge--potent">OPPORTUNITY #' + o.id.slice(1) + "</span></div>" +
        '<p style="color:var(--color-text-2);font-size:13px;margin:8px 0">' + esc(o.description) + "</p>" +
        '<div class="cols-opp" style="display:grid;gap:20px;align-items:center">' +
          "<div>" + capBars([{ name: "已具备能力", pct: Math.round(o.coverage * 100) }]) +
          '<div class="chips" style="margin-top:8px">' + o.source_assets.map((s) => '<span class="tag mono">' + esc((M.projects.find((p) => p.id === s) || { name: s }).name) + "</span>").join("") +
          o.missing_capabilities.map((s) => '<span class="tag" style="color:var(--color-warning)">缺 ' + esc(s) + "</span>").join("") + "</div></div>" +
          '<div style="text-align:right"><div style="color:var(--color-warning);letter-spacing:2px">' + "★".repeat(o.rating) + '<span style="color:var(--color-panel-3)">' + "★".repeat(5 - o.rating) + "</span></div>" +
          '<div style="font-size:11px;color:var(--color-text-3);margin-top:6px">' + esc(o.evidence) + "</div></div>" +
        "</div>" +
        '<div style="display:flex;gap:8px;margin-top:14px"><button class="btn btn--primary btn--sm" data-toast="原型演示：进入机会深入分析">' + ic("spark") + " Explore →</button>" +
        '<button class="btn btn--ghost btn--sm" data-dismiss="' + o.id + '">Dismiss</button></div></section>').join("")
      : stateBox("empty", "暂无待处理的机会。Dismiss 全部机会后会看到此空状态。"));
  }

  /* ---------------- 设置页 (本轮新增) ----------------
     单机版无用户体系; 设置 = 大模型配置 / 扫描目录 / 外观 / 数据与隐私 */
  function pageSettings() {
    const meta = M.settings_meta;
    const llm = state.llm;
    const cloudP = meta.cloud_providers.find((p) => p.id === llm.cloud_provider) || meta.cloud_providers[0];
    const localB = meta.local_backends.find((p) => p.id === llm.local_backend) || meta.local_backends[0];

    const TABS = [
      { key: "llm",        icon: "cpu",     label: "大模型配置" },
      { key: "scan",       icon: "scan",    label: "扫描目录" },
      { key: "appearance", icon: "palette", label: "外观" },
      { key: "data",       icon: "db",      label: "数据与隐私" }
    ];
    const nav = '<div class="set-nav">' + TABS.map((t) =>
      '<button data-stab="' + t.key + '" class="' + (state.settingsTab === t.key ? "is-active" : "") + '">' + ic(t.icon) + t.label + "</button>").join("") + "</div>";

    let panel = "";
    if (state.settingsTab === "llm") {
      panel =
        '<section class="card"><div class="card-head"><div><div class="card-title">' + ic("cloud") + " 云端模型（OpenAI-Compatible 抽象）</div>" +
        '<div class="card-sub">用于深度分析: 洞察 / 机会 / 跨项目推理 · 需显式授权, 数据不默认出网</div></div></div>' +
        '<div class="form-row"><div class="form-label">提供商</div><div class="form-field"><div class="provider-grid">' +
          meta.cloud_providers.map((p) => '<button class="provider-card' + (p.id === llm.cloud_provider ? " is-active" : "") + '" data-llm-cloud="' + p.id + '"><span class="pdot"></span>' + p.name + "</button>").join("") +
        "</div></div></div>" +
        '<div class="form-row"><div class="form-label">Base URL</div><div class="form-field"><input class="input mono" id="llm-base-url" value="' + esc(llm.cloud_base_url) + '"><div class="form-hint">切换提供商时自动填充默认地址, 可改为代理或自建网关</div></div></div>' +
        '<div class="form-row"><div class="form-label">API Key</div><div class="form-field"><div style="display:flex;gap:8px">' +
          '<input class="input mono" id="llm-api-key" type="' + (llm.show_key ? "text" : "password") + '" value="' + esc(llm.cloud_api_key) + '" style="flex:1">' +
          '<button class="btn btn--ghost btn--sm" id="llm-key-toggle" title="显示/隐藏">' + ic(llm.show_key ? "eyeoff" : "eye") + "</button></div>" +
          '<div class="form-hint">仅存于本机 SQLite, 不上传、不遥测</div></div></div>' +
        '<div class="form-row"><div class="form-label">模型</div><div class="form-field"><select class="select" id="llm-cloud-model">' +
          cloudP.models.map((m) => '<option' + (m === llm.cloud_model ? " selected" : "") + ">" + m + "</option>").join("") + "</select></div></div>" +
        '<div class="form-row"><div class="form-label">连接测试</div><div class="form-field" style="display:flex;align-items:center;gap:10px">' +
          '<button class="btn btn--ghost btn--sm" id="llm-test"' + (llm.testing ? " disabled" : "") + ">" + (llm.testing ? ic("refresh") + " 测试中…" : ic("plug") + " 测试连接") + "</button>" +
          (llm.test_result ? '<span class="badge ' + (llm.test_result.ok ? "badge--high" : "badge--muted") + '" style="' + (llm.test_result.ok ? "" : "color:var(--color-danger);background:rgba(239,68,68,.12)") + '">' + (llm.test_result.ok ? "✓ " : "✕ ") + esc(llm.test_result.msg) + "</span>" : '<span class="form-hint" style="margin:0">向 Base URL 发送一次最小 chat 请求验证连通性</span>') +
        "</div></div></section>" +

        '<section class="card" style="margin-top:16px"><div class="card-head"><div><div class="card-title">' + ic("cpu") + " 本地模型</div>" +
        '<div class="card-sub">用于快速分析: 项目画像 / 分类 / Embedding · 完全离线, 敏感项目强制走本地</div></div></div>' +
        '<div class="form-row"><div class="form-label">本地后端</div><div class="form-field"><div class="provider-grid">' +
          meta.local_backends.map((p) => '<button class="provider-card' + (p.id === llm.local_backend ? " is-active" : "") + '" data-llm-local="' + p.id + '"><span class="pdot"></span>' + p.name + "</button>").join("") +
        "</div></div></div>" +
        '<div class="form-row"><div class="form-label">Base URL</div><div class="form-field"><input class="input mono" id="llm-local-url" value="' + esc(llm.local_base_url) + '"></div></div>' +
        '<div class="form-row"><div class="form-label">模型</div><div class="form-field"><select class="select" id="llm-local-model">' +
          localB.models.map((m) => '<option' + (m === llm.local_model ? " selected" : "") + ">" + m + "</option>").join("") + "</select></div></div></section>" +

        '<section class="card" style="margin-top:16px"><div class="card-head"><div><div class="card-title">' + ic("flow") + " 任务路由（三级分析策略）</div>" +
        '<div class="card-sub">按任务类型分配模型: 高频低价值走本地, 低频高价值走云端</div></div></div>' +
        meta.routes.map((r) =>
          '<div class="form-row"><div class="form-label">' + r.label + "<small>" + r.desc + "</small></div>" +
          '<div class="form-field"><div class="segmented" data-route="' + r.key + '">' +
            [["local", "本地模型"], ["cloud", "云端模型"]].map((opt) =>
              '<button data-route-val="' + opt[0] + '" class="' + (llm[r.key] === opt[0] ? "is-active" : "") + '">' + opt[1] + "</button>").join("") +
          "</div></div></div>").join("") +
        '<div class="form-row"><div class="form-label">敏感项目仅本地<small>标记为敏感的项目, 任何数据不进入云端模型上下文</small></div>' +
          '<div class="form-field"><label class="switch"><input type="checkbox" data-llm-flag="sensitive_local_only"' + (llm.sensitive_local_only ? " checked" : "") + '><i></i></label></div></div>' +
        '<div class="form-row"><div class="form-label">Embedding 强制本地<small>fastembed-rs 本地向量化, 代码永不出网</small></div>' +
          '<div class="form-field"><label class="switch"><input type="checkbox" data-llm-flag="embedding_local_only"' + (llm.embedding_local_only ? " checked" : "") + '><i></i></label></div></div>' +
        '<div style="display:flex;justify-content:flex-end;gap:8px;margin-top:14px"><button class="btn btn--ghost btn--sm" data-toast="已恢复默认配置">恢复默认</button><button class="btn btn--primary btn--sm" id="llm-save">' + ic("check") + " 保存配置</button></div></section>";
    } else if (state.settingsTab === "scan") {
      panel =
        '<section class="card"><div class="card-head"><div><div class="card-title">' + ic("folder") + " 扫描目录</div>" +
        '<div class="card-sub">目录级授权: 未添加的目录不读取 · 自动跳过 node_modules / .git / dist 与凭证文件</div></div>' +
        '<button class="btn btn--primary btn--sm" id="dir-add">' + ic("plus") + " 添加目录</button></div>" +
        state.scan.dirs.map((d, i) =>
          '<div class="dir-row">' + ic("folder") + '<div style="flex:1;min-width:0"><div class="path">' + esc(d) + '</div><div class="sub">已索引 · 上次扫描 2 小时前</div></div>' +
          '<span class="badge badge--high">● 已授权</span>' +
          '<button class="icon-btn" data-dir-del="' + i + '" title="移除">' + ic("trash") + "</button></div>").join("") +
        (state.scan.dirs.length ? "" : stateBox("empty", "尚未添加任何扫描目录，点击右上角「添加目录」开始。")) +
        "</section>" +
        '<section class="card" style="margin-top:16px"><div class="card-head"><div class="card-title">' + ic("gear") + " 扫描行为</div></div>" +
        '<div class="form-row"><div class="form-label">文件监听<small>变更文件实时增量索引 (File Watcher)</small></div><div class="form-field"><label class="switch"><input type="checkbox" checked><i></i></label></div></div>' +
        '<div class="form-row"><div class="form-label">扫描时排除<small>正则模式, 每行一条</small></div><div class="form-field"><textarea class="input mono" rows="3">' + ["**/node_modules/**", "**/.venv/**", "**/target/**"].join("\n") + '</textarea></div></div>' +
        '<div class="form-row"><div class="form-label">Level 2 AI 分析<small>异步执行, 仅送关键模块, 单项目 ≤ 30K tokens</small></div><div class="form-field"><label class="switch"><input type="checkbox" checked><i></i></label></div></div></section>';
    } else if (state.settingsTab === "appearance") {
      panel =
        '<section class="card"><div class="card-head"><div><div class="card-title">' + ic("palette") + ' 外观</div><div class="card-sub">单机版外观设置, 跟随本机保存</div></div></div>' +
        '<div class="form-row"><div class="form-label">主题</div><div class="form-field"><div class="segmented" id="theme-seg">' +
          [["dark", "暗色"], ["light", "亮色"]].map((t) => '<button data-theme-val="' + t[0] + '" class="' + (state.theme === t[0] ? "is-active" : "") + '">' + t[1] + "</button>").join("") +
        "</div></div></div>" +
        '<div class="form-row"><div class="form-label">代码窗主题<small>编辑器区域两种主题下均保持深色底</small></div><div class="form-field"><span class="tag">Dark (固定)</span></div></div>' +
        '<div class="form-row"><div class="form-label">减弱动效<small>尊重系统 prefers-reduced-motion</small></div><div class="form-field"><label class="switch"><input type="checkbox"><i></i></label></div></div></section>';
    } else {
      panel =
        '<section class="card"><div class="card-head"><div><div class="card-title">' + ic("db") + " 数据与隐私</div>" +
        '<div class="card-sub">Local-First: 全部数据存储在本机单文件 SQLite, 无账号、无云端同步、无遥测</div></div></div>' +
        '<div class="form-row"><div class="form-label">数据库位置</div><div class="form-field"><input class="input mono" value="~/.spolia/index.db" readonly><div class="form-hint">单文件数据库, 复制即备份</div></div></div>' +
        '<div class="form-row"><div class="form-label">当前占用</div><div class="form-field"><span class="mono" style="font-size:var(--fs-md)">214 MB</span> <span class="tag" style="margin-left:8px">128 项目 · 1,284 资产 · 42,318 向量</span></div></div>' +
        '<div class="form-row"><div class="form-label">网络访问审计<small>展示哪些数据、发送给了哪个模型、什么时候</small></div><div class="form-field"><button class="btn btn--ghost btn--sm" data-toast="原型演示: 打开审计日志">查看审计日志</button></div></div>' +
        '<div class="form-row"><div class="form-label">危险操作</div><div class="form-field" style="display:flex;gap:8px;flex-wrap:wrap">' +
          '<button class="btn btn--ghost btn--sm" data-toast="原型演示: 重建全部索引">' + ic("refresh") + " 重建索引</button>" +
          '<button class="btn btn--ghost btn--sm" style="color:var(--color-danger)" data-toast="原型演示: 清除全部派生数据（需二次确认）">' + ic("trash") + " 清除全部数据</button>" +
        "</div></div></section>";
    }

    return '<div class="page-head"><div><div class="page-title">设置</div><div class="page-sub">单机版 · 无账号体系, 所有配置仅保存在本机</div></div></div>' +
      '<div class="cols-set" style="display:grid;gap:16px;align-items:start">' + nav + "<div>" + panel + "</div></div>";
  }

  const PAGES = {
    overview: pageOverview, projects: pageProjects, project: pageProject,
    assets: pageAssets, graph: pageGraph, insights: pageInsights,
    analyst: pageAnalyst, mcp: pageMcp, opportunities: pageOpportunities,
    settings: pageSettings
  };

  /* ---------------- Render & bind ---------------- */
  function render() {
    applyTheme();
    const app = $("#app");
    app.innerHTML = renderShell() + '<main class="main" id="main">' + (PAGES[state.page] || pageOverview)() + "</main>" +
      '<div class="toast" id="toast"></div>' +
      /* 状态演示开关 (验收辅助) */
      '<div style="position:fixed;right:16px;bottom:16px;z-index:60;display:flex;gap:6px;background:var(--color-panel);border:1px solid var(--color-border-strong);border-radius:999px;padding:6px 10px;box-shadow:var(--shadow-lg)">' +
      '<span style="font-size:11px;color:var(--color-text-3);align-self:center">状态演示</span>' +
      [["default", "默认"], ["loading", "加载中"], ["empty", "空数据"], ["error", "错误"]].map((d) =>
        '<button class="chip' + (state.demo === d[0] ? " is-active" : "") + '" data-demo="' + d[0] + '" style="padding:2px 10px;font-size:11px">' + d[1] + "</button>").join("") + "</div>";
    bind();
    $("#main").scrollTop = 0;
  }

  let toastTimer = null;
  function toast(msg, kind) {
    const t = $("#toast");
    t.innerHTML = (kind === "err" ? '<span class="ico-err">' + ic("alert") + "</span>" : '<span class="ico-ok">' + ic("check") + "</span>") + esc(msg);
    t.classList.add("is-show");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => t.classList.remove("is-show"), 2400);
  }

  function go(page) { state.page = page; render(); }

  function bind() {
    document.querySelectorAll("[data-nav]").forEach((el) => el.addEventListener("click", () => go(el.dataset.nav)));
    document.querySelectorAll("[data-toast]").forEach((el) => el.addEventListener("click", () => toast(el.dataset.toast)));
    document.querySelectorAll("[data-demo]").forEach((el) => el.addEventListener("click", () => {
      state.demo = el.dataset.demo;
      if (state.demo === "default") { state.demo = "loading"; render(); setTimeout(() => { state.demo = "default"; render(); }, 700); }
      else render();
    }));
    document.querySelectorAll("[data-open-project]").forEach((el) => el.addEventListener("click", () => { state.projectId = el.dataset.openProject; state.projectTab = "overview"; go("project"); }));
    document.querySelectorAll("[data-ptab]").forEach((el) => el.addEventListener("click", () => { state.projectTab = el.dataset.ptab; render(); }));
    document.querySelectorAll("[data-afilter]").forEach((el) => el.addEventListener("click", () => { state.assetFilter = el.dataset.afilter; render(); }));
    document.querySelectorAll("[data-ifilter]").forEach((el) => el.addEventListener("click", () => { state.insightFilter = el.dataset.ifilter; render(); }));
    document.querySelectorAll("[data-gview]").forEach((el) => el.addEventListener("click", () => { if (!el.disabled) { state.graphView = el.dataset.gview; render(); } }));
    document.querySelectorAll("[data-capchip]").forEach((el) => el.addEventListener("click", () => {
      document.querySelectorAll("[data-capchip]").forEach((x) => x.classList.remove("is-active"));
      el.classList.add("is-active");
    }));
    document.querySelectorAll("[data-pchip]").forEach((el) => el.addEventListener("click", () => {
      document.querySelectorAll("[data-pchip]").forEach((x) => x.classList.remove("is-active"));
      el.classList.add("is-active");
    }));
    document.querySelectorAll("[data-fb]").forEach((el) => el.addEventListener("click", () => {
      const [id, v] = el.dataset.fb.split(":");
      state.insightFeedback[id] = v;
      toast(v === "useful" ? "已标记为有用，反馈将回流调整评分权重" : "已标记为无用", v === "useful" ? "ok" : "err");
      render();
    }));
    document.querySelectorAll("[data-dismiss]").forEach((el) => el.addEventListener("click", () => {
      state.opportunities = state.opportunities.filter((o) => o.id !== el.dataset.dismiss);
      toast("已忽略该机会"); render();
    }));
    document.querySelectorAll("[data-asset]").forEach((el) => el.addEventListener("click", () => toast("原型演示：打开资产详情与 Evidence 抽屉（" + el.dataset.asset + "）")));
    document.querySelectorAll(".gnode").forEach((el) => el.addEventListener("click", () => { state.graphSelected = el.dataset.node; if (state.page !== "overview") render(); }));

    /* search */
    const gs = $("#global-search");
    if (gs) gs.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { state.search = gs.value; if (!state.search.trim()) { toast("请输入搜索词", "err"); return; } go("assets"); }
    });
    const as = $("#asset-search");
    if (as) as.addEventListener("input", () => { state.search = as.value; const m = $("#main"); m.innerHTML = pageAssets(); bind(); });

    /* assistant / analyst */
    const ask = (q) => {
      if (!q || !q.trim()) { toast("问题不能为空（错误状态示例）", "err"); return; }
      go("analyst");
      const box = $("#analyst-result");
      box.innerHTML = stateBox("loading");
      setTimeout(() => {
        box.innerHTML = '<section class="card"><div class="card-title">' + ic("spark") + " " + esc(q) + "</div>" +
          '<p style="color:var(--color-text-2);margin:10px 0">你在 <b style="color:var(--color-text)">5</b> 个项目中实现过视频生成相关能力。证据：</p>' +
          '<ul style="font-size:12px;color:var(--color-text-3)" class="mono"><li>① yingTech / services/video_service.py</li><li>② videoLab / pipeline/</li><li>③ ShortVideo / render.py</li></ul>' +
          '<div class="chips" style="margin-top:12px"><span class="chip" style="cursor:default">confidence 0.91</span><span class="chip" style="cursor:default">evidence 3</span></div></section>';
      }, 900);
    };
    document.querySelectorAll("[data-ask]").forEach((el) => el.addEventListener("click", () => ask(el.dataset.ask)));
    document.querySelectorAll("[data-askq]").forEach((el) => el.addEventListener("click", () => ask(el.dataset.askq)));
    const aq = $("#assist-q");
    if (aq) aq.addEventListener("keydown", (e) => { if (e.key === "Enter") ask(aq.value); });
    const asend = $("#assist-send");
    if (asend) asend.addEventListener("click", () => ask(aq ? aq.value : ""));
    const nq = $("#analyst-q");
    if (nq) nq.addEventListener("keydown", (e) => { if (e.key === "Enter") ask(nq.value); });
    const nsend = $("#analyst-send");
    if (nsend) nsend.addEventListener("click", () => ask(nq ? nq.value : ""));

    /* mcp toggle */
    const mt = $("#mcp-toggle");
    if (mt) mt.addEventListener("click", () => { state.mcpRunning = !state.mcpRunning; toast(state.mcpRunning ? "MCP Server 已启动" : "MCP Server 已停止", state.mcpRunning ? "ok" : "err"); render(); });

    /* ---- 主题切换 (顶栏 + 设置页两处入口) ---- */
    const setTheme = (t) => { state.theme = t; applyTheme(); render(); };
    const tl = $("#theme-light"), td = $("#theme-dark");
    if (tl) tl.addEventListener("click", () => setTheme("light"));
    if (td) td.addEventListener("click", () => setTheme("dark"));
    document.querySelectorAll("[data-theme-val]").forEach((el) => el.addEventListener("click", () => setTheme(el.dataset.themeVal)));

    /* ---- 首页项目扫描 (全量/增量 → 进度演示 → 完成) ---- */
    document.querySelectorAll("[data-scan]").forEach((el) => el.addEventListener("click", () => startScan(el.dataset.scan)));
    const sc = $("#scan-cancel");
    if (sc) sc.addEventListener("click", () => {
      clearTimeout(scanTimer); scanTimer = null;
      state.scan = { running: false, progress: 0, found: 0, stage: "", log: [], dirs: state.scan.dirs };
      toast("扫描已取消（所有 AI 分析任务均可取消）", "err"); render();
    });

    /* ---- 设置页 ---- */
    document.querySelectorAll("[data-stab]").forEach((el) => el.addEventListener("click", () => { state.settingsTab = el.dataset.stab; render(); }));
    const llm = state.llm;
    document.querySelectorAll("[data-llm-cloud]").forEach((el) => el.addEventListener("click", () => {
      const p = M.settings_meta.cloud_providers.find((x) => x.id === el.dataset.llmCloud);
      llm.cloud_provider = p.id; llm.cloud_base_url = p.base_url; llm.cloud_model = p.models[0]; llm.test_result = null; render();
    }));
    document.querySelectorAll("[data-llm-local]").forEach((el) => el.addEventListener("click", () => {
      const p = M.settings_meta.local_backends.find((x) => x.id === el.dataset.llmLocal);
      llm.local_backend = p.id; llm.local_base_url = p.base_url; llm.local_model = p.models[0]; render();
    }));
    const bu = $("#llm-base-url");  if (bu) bu.addEventListener("input", () => { llm.cloud_base_url = bu.value; });
    const ak = $("#llm-api-key");  if (ak) ak.addEventListener("input", () => { llm.cloud_api_key = ak.value; llm.test_result = null; });
    const cm = $("#llm-cloud-model"); if (cm) cm.addEventListener("change", () => { llm.cloud_model = cm.value; });
    const lu = $("#llm-local-url"); if (lu) lu.addEventListener("input", () => { llm.local_base_url = lu.value; });
    const lm = $("#llm-local-model"); if (lm) lm.addEventListener("change", () => { llm.local_model = lm.value; });
    const kt = $("#llm-key-toggle"); if (kt) kt.addEventListener("click", () => { llm.show_key = !llm.show_key; render(); });
    document.querySelectorAll("[data-llm-flag]").forEach((el) => el.addEventListener("change", () => {
      llm[el.dataset.llmFlag] = el.checked;
      toast(el.checked ? "已开启" : "已关闭", el.checked ? "ok" : "err");
    }));
    document.querySelectorAll("[data-route-val]").forEach((el) => el.addEventListener("click", () => {
      llm[el.closest("[data-route]").dataset.route] = el.dataset.routeVal; render();
    }));
    const tt = $("#llm-test");
    if (tt) tt.addEventListener("click", () => {
      llm.testing = true; llm.test_result = null; render();
      setTimeout(() => {
        llm.testing = false;
        const ok = llm.cloud_api_key && llm.cloud_base_url.startsWith("http");
        llm.test_result = ok ? { ok: true, msg: llm.cloud_model + " · 延迟 342ms" } : { ok: false, msg: "连接失败: Base URL 或 API Key 无效" };
        toast(ok ? "连接成功" : "连接失败", ok ? "ok" : "err"); render();
      }, 900);
    });
    const ls = $("#llm-save");
    if (ls) ls.addEventListener("click", () => toast("配置已保存到本机（~/.spolia/config）"));
    const da = $("#dir-add");
    if (da) da.addEventListener("click", () => {
      const d = "E:/Projects";
      if (state.scan.dirs.includes(d)) { toast("该目录已在列表中", "err"); return; }
      state.scan.dirs.push(d); toast("已添加目录 " + d + "（目录级授权）"); render();
    });
    document.querySelectorAll("[data-dir-del]").forEach((el) => el.addEventListener("click", () => {
      const i = Number(el.dataset.dirDel);
      const removed = state.scan.dirs[i];
      state.scan.dirs.splice(i, 1); toast("已移除目录 " + removed); render();
    }));

    /* topbar misc */
    const menu = $("#btn-menu");
    if (menu) menu.addEventListener("click", () => toast("原型演示：侧栏折叠（窄屏自动生效）"));
  }

  /* ---- 扫描流程 (定时器驱动, 可取消) ---- */
  let scanTimer = null;
  function startScan(mode) {
    const stages = M.settings_meta.scan_stages;
    state.scan = { running: true, progress: 0, found: 0, stage: mode === "full" ? "全量扫描启动…" : "增量扫描启动…", log: [mode === "full" ? "全量扫描: 重新遍历全部授权目录" : "增量扫描: 仅重算 File Watcher 报告的变更文件"], dirs: state.scan.dirs };
    render();
    let i = 0;
    const tick = () => {
      if (i >= stages.length) {
        scanTimer = null;
        state.scan.running = false;
        toast("扫描完成: 发现 128 个项目, 新增 3 个");
        render();
        return;
      }
      const st = stages[i++];
      state.scan.progress = st.at;
      state.scan.found = st.found;
      state.scan.stage = st.label;
      state.scan.log.push(st.label);
      if (state.scan.log.length > 6) state.scan.log.shift();
      render();
      scanTimer = setTimeout(tick, 620);
    };
    scanTimer = setTimeout(tick, 620);
  }

  document.addEventListener("DOMContentLoaded", render);
})();
