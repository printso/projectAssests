/* ==========================================================================
   Project Intelligence Prototype — 集中模拟数据
   字段命名与《技术设计书》§11 SQLite Schema 保持一致 (snake_case),
   便于后续 1:1 替换为真实接口返回。
   表对应: projects / assets / capabilities / relations / insights /
           opportunities / jobs / sessions(活动流)
   ========================================================================== */
window.MOCK = (function () {
  "use strict";

  /* ---- 品牌 (开源命名, 经 GitHub/npm/crates.io 查重后选定) ----
     spolia: 古典建筑术语, 指从旧建筑上拆下、重新砌入新建筑的石材/构件。
     隐喻本产品核心: 把历史项目中沉淀的资产, 重新砌进下一个项目。 */
  const brand = {
    name: "Spolia",
    sub: "Your Personal R&D OS",
    logo: "S",
    tagline: "让过去的每一个项目，都成为你未来的可能性"
  };

  /* ---- 量级口径: 设计稿数字 (列表数据为采样 mock, 接入真实接口后统一替换) ---- */
  const scale = {
    projects: 128, assets: 1284, capabilities: 47, knowledge: 312,
    insights: 7, skills: 8
  };

  /* ---- 图谱节点分类色 (官方色板, 与设计图图例一一对应) ---- */
  const kind_colors = {
    capability: "#a855f7",  // 能力 - 紫
    project:    "#22c55e",  // 项目 - 绿
    code:       "#3b82f6",  // 代码 - 蓝
    knowledge:  "#eab308",  // 知识 - 黄
    experience: "#06b6d4",  // 经验 - 青
    relation:   "#64748b"   // 关系 - 灰(边)
  };

  /* ---- projects (技术设计书 §11) ---- */
  const projects = [
    {
      id: "p_yingtech",
      name: "yingTech",
      path: "D:/Projects/yingTech",
      description: "基于大模型的 AI 漫剧生成平台，支持从文本到视频的完整创作流程，集成角色一致性、多镜头生成和自动化工作流。",
      language: "Python",
      framework: "FastAPI / Vue",
      created_at: "2024-03-12",
      updated_at: "2025-05-20",
      last_commit_at: "2025-05-20",
      status: "active",
      health_score: 92,
      completeness: 0.86,
      cover_color: "#a855f7",
      tags: ["Python", "FastAPI", "Vue", "ComfyUI", "SQLite", "MCP"],
      code_size: "12.4k 文件",
      structure: {
        files: 12400, loc: "86k", symbols: 72, modules: 13,
        langs: [
          { name: "Python", pct: 68 }, { name: "Vue", pct: 18 },
          { name: "Shell", pct: 8 }, { name: "其他", pct: 6 }
        ]
      },
      archaeology: {
        sessions: 37, commits: 87, completeness: 0.86,
        phase: "多镜头生成与角色一致性优化",
        salvage: ["Prompt Engine", "Task Queue", "Video Pipeline"],
        narrative: "创建于 2024-03，共 37 次 AI Coding Session、87 次 commit；核心功能完成 86%，仍处于活跃迭代。其 Prompt Engine 与 Task Queue 已在 4 个项目中复现，具备独立抽取价值。"
      },
      capabilities: [
        { name: "AI Video Generation", pct: 95 },
        { name: "Image Generation", pct: 88 },
        { name: "Story Generation", pct: 72 },
        { name: "Agent Workflow", pct: 68 },
        { name: "Prompt Engineering", pct: 62 }
      ],
      highlights: [
        { icon: "video", color: "#6366f1", title: "多镜头生成能力", desc: "支持脚本分镜、镜头控制、自动衔接" },
        { icon: "user",  color: "#22c55e", title: "角色一致性", desc: "基于 LoRA / ControlNet 的角色保持方案" },
        { icon: "flow",  color: "#3b82f6", title: "工作流编排", desc: "任务队列 + 异步处理 + 状态管理" },
        { icon: "plug",  color: "#06b6d4", title: "MCP 集成", desc: "支持外部工具调用和上下文增强" }
      ],
      files: [
        { name: "yingTech", type: "dir", level: 1, open: true },
        { name: "api", type: "dir", level: 2, open: true },
        { name: "models", type: "dir", level: 2 },
        { name: "services", type: "dir", level: 2, open: true },
        { name: "video_service.py", type: "file", level: 3, active: true },
        { name: "task_queue.py", type: "file", level: 3 },
        { name: "prompt_engine.py", type: "file", level: 3 },
        { name: "utils", type: "dir", level: 2 },
        { name: "web", type: "dir", level: 2 },
        { name: "tests", type: "dir", level: 2 },
        { name: "main.py", type: "file", level: 2 },
        { name: "requirements.txt", type: "file", level: 2 },
        { name: "README.md", type: "file", level: 2 }
      ],
      code: [
        [{ t: "k", v: "from" }, { v: " typing " }, { t: "k", v: "import" }, { v: " List, Dict, Optional" }],
        [{ t: "k", v: "from" }, { v: " app.models " }, { t: "k", v: "import" }, { v: " VideoTask, VideoResult" }],
        [{ t: "k", v: "from" }, { v: " app.core.task_queue " }, { t: "k", v: "import" }, { v: " TaskQueue" }],
        [{ t: "k", v: "from" }, { v: " app.services.image_service " }, { t: "k", v: "import" }, { v: " ImageService" }],
        [{ t: "k", v: "from" }, { v: " app.core.config " }, { t: "k", v: "import" }, { v: " settings" }],
        [],
        [{ t: "k", v: "class" }, { v: " " }, { t: "t", v: "VideoService" }, { v: ":" }],
        [{ v: "    " }, { t: "s", v: '"""视频生成服务"""' }],
        [],
        [{ v: "    " }, { t: "k", v: "def" }, { v: " " }, { t: "f", v: "__init__" }, { v: "(self):" }],
        [{ v: "        self.queue = " }, { t: "f", v: "TaskQueue" }, { v: "()" }],
        [{ v: "        self.image_service = " }, { t: "f", v: "ImageService" }, { v: "()" }],
        [],
        [{ v: "    " }, { t: "k", v: "async def" }, { v: " " }, { t: "f", v: "generate_video" }, { v: "(" }],
        [{ v: "        self," }],
        [{ v: "        prompt: " }, { t: "t", v: "str" }, { v: "," }],
        [{ v: "        image_path: " }, { t: "t", v: "str" }, { v: "," }],
        [{ v: "        style: " }, { t: "t", v: "str" }, { v: " = " }, { t: "s", v: '"anime"' }, { v: "," }],
        [{ v: "        duration: " }, { t: "t", v: "int" }, { v: " = " }, { t: "n", v: "5" }],
        [{ v: "    ) -> " }, { t: "t", v: "VideoResult" }, { v: ":" }],
        [{ v: "        " }, { t: "s", v: '"""根据提示词与首帧图生成视频片段"""' }],
        [{ v: "        task = " }, { t: "k", v: "await" }, { v: " self.queue." }, { t: "f", v: "submit" }, { v: "(" }],
        [{ v: "            kind=" }, { t: "s", v: '"video"' }, { v: ", prompt=prompt," }],
        [{ v: "            image_path=image_path, style=style," }],
        [{ v: "            duration=duration," }],
        [{ v: "        )" }],
        [{ v: "        " }, { t: "k", v: "return await" }, { v: " self.queue." }, { t: "f", v: "wait_result" }, { v: "(task)" }]
      ]
    },
    {
      id: "p_idearound", name: "IdeaRound", path: "D:/Projects/IdeaRound",
      description: "AI 创意提案与评审协作工具，将碎片想法结构化为可评审的提案。",
      language: "TypeScript", framework: "Next.js",
      created_at: "2025-01-08", updated_at: "2025-06-11", last_commit_at: "2025-06-11",
      status: "active", health_score: 78, completeness: 0.64,
      cover_color: "#3b82f6", tags: ["Next.js", "TypeScript", "PostgreSQL"]
    },
    {
      id: "p_vision", name: "vision", path: "D:/Projects/vision",
      description: "计算机视觉工具箱：检测、分割与 VLM 二次审核的实验集合。",
      language: "Python", framework: "PyTorch",
      created_at: "2024-08-02", updated_at: "2025-02-14", last_commit_at: "2025-02-14",
      status: "paused", health_score: 61, completeness: 0.52,
      cover_color: "#ec4899", tags: ["Python", "PyTorch", "YOLO", "VLM"]
    },
    {
      id: "p_image_tool", name: "image-tool", path: "D:/Projects/image-tool",
      description: "图片批处理工具：下载、缩放、缩略图与批量管线。",
      language: "Python", framework: "-",
      created_at: "2024-05-19", updated_at: "2024-12-03", last_commit_at: "2024-12-03",
      status: "paused", health_score: 55, completeness: 0.71,
      cover_color: "#22c55e", tags: ["Python", "Pillow"]
    },
    {
      id: "p_video_lab", name: "videoLab", path: "D:/Projects/videoLab",
      description: "短视频生成实验：时间轴、转场与 Prompt Pipeline。",
      language: "Python", framework: "ComfyUI",
      created_at: "2025-03-01", updated_at: "2025-07-22", last_commit_at: "2025-07-22",
      status: "active", health_score: 74, completeness: 0.48,
      cover_color: "#06b6d4", tags: ["Python", "ComfyUI"]
    },
    {
      id: "p_shortvideo", name: "ShortVideo", path: "D:/Projects/ShortVideo",
      description: "9:16 竖版短视频自动化生产原型。",
      language: "Python", framework: "-",
      created_at: "2025-04-16", updated_at: "2025-08-09", last_commit_at: "2025-08-09",
      status: "experimental", health_score: 49, completeness: 0.33,
      cover_color: "#f97316", tags: ["Python", "FFmpeg"]
    },
    {
      id: "p_novelgen", name: "NovelGenerator", path: "D:/Projects/NovelGenerator",
      description: "小说到分镜的转换实验（未完成想法：自动分镜）。",
      language: "Python", framework: "-",
      created_at: "2024-11-23", updated_at: "2025-01-30", last_commit_at: "2025-01-30",
      status: "abandoned", health_score: 38, completeness: 0.29,
      cover_color: "#eab308", tags: ["Python", "LLM"]
    },
    {
      id: "p_agent_platform", name: "agent-platform", path: "D:/Projects/agent-platform",
      description: "Agent 执行器与 MCP 工具调用平台。",
      language: "TypeScript", framework: "Node",
      created_at: "2025-02-10", updated_at: "2025-09-02", last_commit_at: "2025-09-02",
      status: "active", health_score: 83, completeness: 0.58,
      cover_color: "#8b5cf6", tags: ["TypeScript", "MCP", "Agent"]
    },
    {
      id: "p_prompt_engine", name: "prompt-engine", path: "D:/Projects/prompt-engine",
      description: "Prompt 管理与版本化引擎，被 4 个项目复用。",
      language: "TypeScript", framework: "-",
      created_at: "2024-06-30", updated_at: "2025-06-28", last_commit_at: "2025-06-28",
      status: "active", health_score: 80, completeness: 0.77,
      cover_color: "#ec4899", tags: ["TypeScript", "Prompt"]
    }
  ];

  /* ---- capabilities (三层: category -> parent -> leaf) ---- */
  const capabilities = [
    { id: "c_ai",    name: "AI",        category: "domain", parent_id: null, confidence: 1 },
    { id: "c_web",   name: "Web",       category: "domain", parent_id: null, confidence: 1 },
    { id: "c_data",  name: "数据",       category: "domain", parent_id: null, confidence: 1 },
    { id: "c_infra", name: "基础设施",   category: "domain", parent_id: null, confidence: 1 },
    { id: "c_media", name: "多媒体",     category: "domain", parent_id: null, confidence: 1 },
    { id: "c_aivideo", name: "AI Video Generation", category: "capability", parent_id: "c_ai", confidence: 0.95 },
    { id: "c_aiimg",   name: "Image Generation",    category: "capability", parent_id: "c_ai", confidence: 0.88 },
    { id: "c_rag",     name: "RAG",                 category: "capability", parent_id: "c_ai", confidence: 0.72 },
    { id: "c_agent",   name: "Agent",               category: "capability", parent_id: "c_ai", confidence: 0.65 },
    { id: "c_mcp",     name: "MCP",                 category: "capability", parent_id: "c_ai", confidence: 0.55 },
    { id: "c_queue",   name: "Task Queue",          category: "capability", parent_id: "c_infra", confidence: 0.78 },
    { id: "c_wf",      name: "Agent Workflow",      category: "capability", parent_id: "c_ai", confidence: 0.68 },
    { id: "c_story",   name: "Story Generation",    category: "capability", parent_id: "c_media", confidence: 0.72 },
    { id: "c_cc",      name: "Character Consistency", category: "capability", parent_id: "c_media", confidence: 0.81 }
  ];

  /* ---- assets (统一资产表, type 判别) ---- */
  const assets = [
    { id: "a_vp", project_id: "p_yingtech", type: "component", name: "VideoPipeline",
      description: "视频生成完整流程管道，支持多种模型和参数配置。",
      source_path: "yingTech/services/video_service.py", confidence: 0.93, reuse_score: 0.91,
      created_at: "2025-05-20", tags: ["Video", "Image", "Pipeline"], color: "#6366f1",
      meta: "来自 2 个项目 · 被 3 个项目引用" },
    { id: "a_cc", project_id: "p_yingtech", type: "component", name: "Character-Consistency",
      description: "基于 LoRA 的角色一致性方案，支持多参考图和风格迁移。",
      source_path: "yingTech/services/character.py", confidence: 0.89, reuse_score: 0.86,
      created_at: "2025-04-02", tags: ["Vision", "Model", "Prompt"], color: "#8b5cf6",
      meta: "来自 2 个项目 · 被 2 个项目引用" },
    { id: "a_tq", project_id: "p_image_tool", type: "code", name: "Task Scheduler",
      description: "任务调度框架，支持优先级、重试与状态持久化。",
      source_path: "image-tool/worker.py", confidence: 0.9, reuse_score: 0.88,
      created_at: "2024-09-11", tags: ["Python", "Redis", "Cron"], color: "#22c55e",
      meta: "出现在 5 个项目 · 建议抽象为独立组件" },
    { id: "a_aw", project_id: "p_agent_platform", type: "code", name: "Agent Workflow",
      description: "Agent 编排执行器：工具调用、上下文管理与失败回退。",
      source_path: "agent-platform/src/executor.ts", confidence: 0.87, reuse_score: 0.8,
      created_at: "2025-06-18", tags: ["Agent", "MCP", "Tool"], color: "#3b82f6",
      meta: "来自 3 个项目 · 被 2 个项目引用" },
    { id: "a_vpt", project_id: "p_video_lab", type: "prompt", name: "Video Prompt Template",
      description: "视频生成 Prompt 模板库，含镜头语言与风格词表。",
      source_path: "videoLab/prompts/", confidence: 0.85, reuse_score: 0.77,
      created_at: "2025-05-02", tags: ["Prompt", "Template", "AI"], color: "#ec4899",
      meta: "来自 4 个项目 · 12 个模板" },
    { id: "a_dec", project_id: "p_yingtech", type: "decision", name: "Decision: 技术选型",
      description: "MVP 阶段使用 SQLite 而不是 PostgreSQL，部署成本低、无需服务端。",
      source_path: "yingTech/README.md", confidence: 0.92, reuse_score: 0.6,
      created_at: "2024-03-15", tags: ["Architecture", "Tech", "Trade-off"], color: "#eab308",
      meta: "适用范围: MVP / Local-first" },
    { id: "a_exp", project_id: "p_vision", type: "experience", name: "Experience: GPU 优化",
      description: "8GB 显存下尝试 INT8 / 4bit / 降分辨率，最终 4bit + batch=1 效果最好。",
      source_path: "vision/notes/gpu.md", confidence: 0.9, reuse_score: 0.72,
      created_at: "2024-10-27", tags: ["CUDA", "Memory", "Performance"], color: "#f97316",
      meta: "尝试 4 种方案 · 最终采纳 1 种" },
    { id: "a_idea", project_id: "p_novelgen", type: "idea", name: "Idea: AI 内容工厂",
      description: "自动把小说转换为分镜再转视频（在 4 个项目中复现的未完成想法）。",
      source_path: "NovelGenerator/TODO.md", confidence: 0.8, reuse_score: 0.66,
      created_at: "2024-12-01", tags: ["Content", "Pipeline", "Product"], color: "#06b6d4",
      meta: "跨 4 个项目复现 · 未实现" },
    { id: "a_kg", project_id: "p_yingtech", type: "knowledge", name: "Knowledge: 增量索引",
      description: "Tree-sitter 增量解析可复用旧语法树，二次索引只重算受影响文件。",
      source_path: "yingTech/docs/indexing.md", confidence: 0.88, reuse_score: 0.64,
      created_at: "2025-03-08", tags: ["Index", "AST", "Performance"], color: "#14b8a6",
      meta: "Pattern · 适用于本地索引场景" }
  ];

  /* ---- relations (泛化边表) ---- */
  const relations = [
    { id: "r1", source_id: "p_yingtech", source_type: "project", relation_type: "implements", target_id: "c_aivideo", target_type: "capability", confidence: 0.95 },
    { id: "r2", source_id: "p_video_lab", source_type: "project", relation_type: "implements", target_id: "c_aivideo", target_type: "capability", confidence: 0.88 },
    { id: "r3", source_id: "p_yingtech", source_type: "project", relation_type: "implements", target_id: "c_cc", target_type: "capability", confidence: 0.81 },
    { id: "r4", source_id: "p_image_tool", source_type: "project", relation_type: "implements", target_id: "c_queue", target_type: "capability", confidence: 0.86 },
    { id: "r5", source_id: "p_agent_platform", source_type: "project", relation_type: "implements", target_id: "c_mcp", target_type: "capability", confidence: 0.9 },
    { id: "r6", source_id: "a_vp", source_type: "asset", relation_type: "similar_to", target_id: "a_tq", target_type: "asset", confidence: 0.74 },
    { id: "r7", source_id: "p_yingtech", source_type: "project", relation_type: "similar_to", target_id: "p_video_lab", target_type: "project", confidence: 0.87 }
  ];

  /* ---- insights ---- */
  const insights = [
    { id: "i1", type: "duplicate_capability", title: "重复实现的能力",
      description: "你在 3 个项目中实现了相似的图片处理 Pipeline，可以考虑抽取为独立模块。",
      evidence: ["image-tool/pipeline.py", "yingTech/services/image_service.py", "videoLab/img/proc.py"],
      confidence: 0.91, created_at: "2026-09-26", user_feedback: null,
      icon: "repeat", color: "#3b82f6", badge: "high", tags: ["图片处理", "Pipeline", "可复用"] },
    { id: "i2", type: "opportunity_hint", title: "潜在的组合机会",
      description: "你的 AI Video + Agent + MCP 能力可以组合成一个新的 AI 内容生产平台。",
      evidence: ["yingTech", "agent-platform", "prompt-engine"],
      confidence: 0.84, created_at: "2026-09-25", user_feedback: null,
      icon: "bulb", color: "#eab308", badge: "potent", tags: ["AI Video", "Agent", "MCP"] },
    { id: "i3", type: "reusable_experience", title: "历史经验可复用",
      description: "你曾在 5 个项目中解决过「任务队列性能优化」问题，现在的项目可以直接参考。",
      evidence: ["image-tool/worker.py", "yingTech/core/task_queue.py", "agent-platform/src/queue.ts"],
      confidence: 0.89, created_at: "2026-09-24", user_feedback: null,
      icon: "clock", color: "#06b6d4", badge: "high", tags: ["任务队列", "性能优化", "经验"] },
    { id: "i4", type: "forgotten_asset", title: "被遗忘的资产",
      description: "一个很有价值的 React 组件库在 2024 年后未再使用，但现在仍然适用当前项目。",
      evidence: ["ui-kit/src/components/"],
      confidence: 0.78, created_at: "2026-09-22", user_feedback: null,
      icon: "doc", color: "#8b5cf6", badge: "info", tags: ["React", "组件库", "前端"] }
  ];

  /* ---- opportunities ---- */
  const opportunities = [
    { id: "o1", title: "AI 内容生产引擎",
      description: "基于你已有的 AI Video + Agent + MCP + Character Consistency 能力，可以组合成一个批量 AI 内容生产平台。",
      source_assets: ["p_yingtech", "p_agent_platform", "p_prompt_engine", "p_video_lab"],
      required_capabilities: ["AI Video Generation", "Agent", "MCP", "Task Queue"],
      missing_capabilities: ["Publishing", "Analytics"],
      evidence: "4 个历史项目存在能力重合",
      status: "new", coverage: 0.78, rating: 5 }
  ];

  /* ---- jobs (索引任务, 用于加载/进度状态) ---- */
  const jobs = [
    { id: "j1", type: "SCAN_PROJECT", status: "completed", progress: 1 },
    { id: "j2", type: "INDEX_CODE", status: "running", progress: 0.68 },
    { id: "j3", type: "ANALYZE_PROJECT", status: "queued", progress: 0 }
  ];

  /* ---- 首页统计 / 图谱 / 活动 / 助手 ---- */
  const overview = {
    greeting: "下午好 👋",
    summary: "你的研发历史已经积累了 1,284 个可复用资产，现在，让 AI 帮你发现更多可能。",
    stats: [
      { key: "projects",  label: "项目总数",   value: "128",    delta: "+12%", color: "#3b82f6", icon: "folder" },
      { key: "assets",    label: "可复用资产", value: "1,284",  delta: "+18%", color: "#8b5cf6", icon: "box" },
      { key: "caps",      label: "能力数量",   value: "47",     delta: "+9%",  color: "#22c55e", icon: "tree" },
      { key: "knowledge", label: "知识条目",   value: "312",    delta: "+16%", color: "#ec4899", icon: "book" }
    ],
    graph: {
      nodes: [
        { id: "ai",   label: "AI",       color: "#a855f7", x: 50, y: 52, r: 30, core: true },
        { id: "agent",label: "Agent",    color: "#22c55e", x: 52, y: 18, r: 19 },
        { id: "mcp",  label: "MCP",      color: "#8b5cf6", x: 78, y: 34, r: 19 },
        { id: "video",label: "Video",    color: "#3b82f6", x: 82, y: 60, r: 19 },
        { id: "image",label: "Image",    color: "#14b8a6", x: 72, y: 82, r: 19 },
        { id: "front",label: "Frontend", color: "#ec4899", x: 47, y: 88, r: 19 },
        { id: "back", label: "Backend",  color: "#f97316", x: 24, y: 78, r: 19 },
        { id: "data", label: "Data",     color: "#eab308", x: 16, y: 52, r: 19 },
        { id: "rag",  label: "RAG",      color: "#06b6d4", x: 20, y: 26, r: 19 }
      ],
      edges: [["ai","agent"],["ai","mcp"],["ai","video"],["ai","image"],["ai","front"],["ai","back"],["ai","data"],["ai","rag"],["agent","mcp"],["video","image"],["back","data"]],
      node_count: 186, relation_count: 742
    },
    activities: [
      { icon: "repeat", color: "#3b82f6", title: "发现 3 个可复用的组件", desc: "来自 2 个项目 · 2 小时前" },
      { icon: "check",  color: "#22c55e", title: "完成项目分析: yingTech", desc: "提取 12 个能力 · 5 小时前" },
      { icon: "link",   color: "#8b5cf6", title: "新增 5 条关联关系", desc: "AI Video 相关 · 8 小时前" },
      { icon: "bulb",   color: "#f97316", title: "生成 2 条新洞察", desc: "基于跨项目分析 · 8 小时前" }
    ],
    assistant_actions: [
      { icon: "search", text: "查找可复用的代码/组件" },
      { icon: "grid",   text: "分析多个项目的共性" },
      { icon: "shield", text: "总结你的技术能力" },
      { icon: "bulb",   text: "发现潜在的组合机会" },
      { icon: "clock",  text: "回顾历史经验和决策" }
    ]
  };

  /* ---- MCP / Skills ---- */
  const mcp = {
    status: "running",
    available_to: ["Cursor", "Claude Code", "Codex"],
    tools: [
      { name: "search_projects" }, { name: "find_similar_project" },
      { name: "search_assets" }, { name: "get_capability" },
      { name: "find_previous_solution" }, { name: "get_experience" },
      { name: "find_reusable_asset" }, { name: "discover_opportunities" }
    ],
    skills: [
      { name: "project-discovery" }, { name: "reuse-analysis" },
      { name: "previous-solution" }, { name: "cross-project-analysis" },
      { name: "project-archaeology" }, { name: "opportunity-discovery" }
    ]
  };

  /* ---- 设置页元数据: 大模型配置选项 (对应技术设计书 §16 LLM 抽象层 / 三级分析路由) ---- */
  const settings_meta = {
    cloud_providers: [
      { id: "openai",     name: "OpenAI Compatible", base_url: "https://api.openai.com/v1",  models: ["gpt-5-mini", "gpt-5", "o4-mini"] },
      { id: "anthropic",  name: "Anthropic",         base_url: "https://api.anthropic.com",  models: ["claude-sonnet-4-5", "claude-haiku-4"] },
      { id: "qwen",       name: "Qwen (DashScope)",  base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", models: ["qwen3-max", "qwen3-coder-plus", "qwen3-8b"] },
      { id: "gemini",     name: "Gemini",            base_url: "https://generativelanguage.googleapis.com/v1beta", models: ["gemini-2.5-pro", "gemini-2.5-flash"] }
    ],
    local_backends: [
      { id: "ollama",    name: "Ollama",     base_url: "http://127.0.0.1:11434", models: ["qwen3:8b", "qwen3:14b", "llama3.1:8b", "gemma3:4b"] },
      { id: "llamacpp",  name: "llama.cpp",  base_url: "http://127.0.0.1:8080",  models: ["qwen3-8b-q4.gguf", "自定义 GGUF"] },
      { id: "lmstudio",  name: "LM Studio",  base_url: "http://127.0.0.1:1234/v1", models: ["本地已加载模型"] }
    ],
    /* 三级分析策略的 LLM 路由说明 (设置页展示) */
    routes: [
      { key: "route_fast", label: "快速分析", desc: "项目画像 / 分类 / 摘要, 高频调用, 推荐本地模型" },
      { key: "route_deep", label: "深度分析", desc: "洞察 / 机会 / 跨项目推理, 低频调用, 推荐云端模型" }
    ],
    /* 扫描流程阶段 (首页扫描按钮 → 进度演示) */
    scan_stages: [
      { at: 8,  label: "扫描目录, 发现项目…",      found: 34 },
      { at: 26, label: "识别 Git / Node / Python…", found: 87 },
      { at: 45, label: "静态分析: 依赖 / 语言 / 规模…", found: 112 },
      { at: 68, label: "Level 1 索引: Tree-sitter AST…", found: 124 },
      { at: 86, label: "写入本地 SQLite…",          found: 128 },
      { at: 100, label: "扫描完成",                 found: 128 }
    ]
  };

  /* ---- 图谱页节点 (更细粒度) ---- */
  const graph_nodes = [
    { id: "g_ai",   label: "AI Video",       color: "#a855f7", x: 50, y: 50, r: 26, core: true },
    { id: "g_yt",   label: "yingTech",       color: "#22c55e", x: 38, y: 22, r: 14, kind: "project" },
    { id: "g_vl",   label: "VideoLab",       color: "#3b82f6", x: 62, y: 20, r: 14, kind: "project" },
    { id: "g_sv",   label: "ShortVideo",     color: "#06b6d4", x: 78, y: 34, r: 14, kind: "project" },
    { id: "g_ig",   label: "Image Generation",color: "#22c55e", x: 82, y: 56, r: 14 },
    { id: "g_cc",   label: "Character Consistency", color: "#8b5cf6", x: 72, y: 78, r: 14 },
    { id: "g_ag",   label: "Agent",          color: "#3b82f6", x: 52, y: 86, r: 14 },
    { id: "g_pe",   label: "Prompt Engine",  color: "#14b8a6", x: 30, y: 82, r: 14 },
    { id: "g_pp",   label: "Pipeline",       color: "#ec4899", x: 18, y: 66, r: 14 },
    { id: "g_tq",   label: "Task Queue",     color: "#f97316", x: 16, y: 42, r: 14 },
    { id: "g_pl",   label: "Workflow",       color: "#eab308", x: 26, y: 56, r: 10 }
  ];
  const graph_edges = [["g_ai","g_yt"],["g_ai","g_vl"],["g_ai","g_sv"],["g_ai","g_ig"],["g_ai","g_cc"],["g_ai","g_ag"],["g_ai","g_pe"],["g_ai","g_pp"],["g_ai","g_tq"],["g_tq","g_pl"]];
  const graph_detail = {
    title: "AI Video Generation", badge: "Capability",
    used_in: ["yingTech", "VideoLab", "ShortVideo"],
    related: ["Image Generation", "Character Consistency", "Video Pipeline"],
    stats: [
      { k: "Reusable Assets", v: 14 },
      { k: "Historical Experiences", v: 6 },
      { k: "Potential Opportunities", v: 3 }
    ]
  };

  /* ---- 项目 Tab (计数在 app.js 中按 mock 数据动态计算, 不再硬编码) ---- */
  const project_tabs = [
    { key: "overview", label: "概览" },
    { key: "structure", label: "代码结构" },
    { key: "assets", label: "资产" },
    { key: "knowledge", label: "知识" },
    { key: "decisions", label: "决策" },
    { key: "experience", label: "经验" },
    { key: "related", label: "相关项目" },
    { key: "ai", label: "AI 分析" }
  ];

  /* ---- AI Analyst 预置问题 ---- */
  const analyst_questions = [
    "我过去做过哪些视频生成相关项目？",
    "哪些代码值得抽出来复用？",
    "我重复实现过什么？",
    "我的技术能力主要集中在哪些方向？",
    "这些项目可以组合成什么新产品？",
    "这个新项目和我过去哪些东西有关？"
  ];

  return {
    brand, scale, kind_colors,
    projects, capabilities, assets, relations, insights, opportunities,
    jobs, overview, mcp, settings_meta, graph_nodes, graph_edges, graph_detail,
    project_tabs, analyst_questions
  };
})();
