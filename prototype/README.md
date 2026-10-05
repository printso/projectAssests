# Spolia 原型（prototype/）

> **Spolia** — Your Personal R&D OS。开源品牌名（古典建筑术语：从旧建筑拆下、重新砌入新建筑的石材/构件，隐喻"把历史项目的资产重新砌进下一个项目"）。
> 命名查重记录：fallow / vestige / keepsake / loam / understory / palimpsest / colophon / rowen / resow / humus / remint 均已被同赛道或无关项目占用；`spolia` 裸名在 **npm 与 crates.io 均未被占用**（GitHub 仅存在同名个人账号，不影响仓库命名），故选定。
> 独立原型目录，与业务代码完全隔离，**未修改任何现有文件**。
> 打开方式：直接双击 `index.html`（file:// 可用，零依赖），或任意静态服务器托管。

## 目录结构与用途

```text
prototype/
├── index.html          # 唯一入口（SPA，内存路由，9 个页面）
├── README.md           # 本文件：目录用途 / 页面-设计图对应 / 验收说明 / 待确认点
├── css/
│   ├── tokens.css      # 设计变量（配色/字体/间距/圆角/阴影/动效），全部视觉决策的唯一来源
│   └── base.css        # 基础样式 + 组件库（侧栏/顶栏/卡片/按钮/chips/状态/响应式）
├── js/
│   ├── mock-data.js    # 集中模拟数据；字段命名对齐《技术设计书》§11 SQLite Schema
│   └── app.js          # 路由 + 页面渲染 + 交互绑定 + 内联 SVG 图标与图谱
└── assets/
    └── img/            # 视觉验收截图（agent-browser 实截）：
                        #   shot-overview / shot-project / shot-structure / shot-assets /
                        #   shot-graph / shot-insights / shot-analyst / shot-mcp /
                        #   shot-opportunities / shot-tablet(1140px) / shot-mobile(390px) /
                        #   shot-light-overview(亮色首页) / shot-scan-running(扫描中) /
                        #   shot-settings-llm(设置·暗) / shot-settings-llm-light(设置·亮)
```

## 页面清单 ↔ 设计图对应关系

| # | 原型页面（入口） | 设计图来源 | 还原要点 |
|---|----------------|-----------|---------|
| 1 | My R&D 首页（默认页） | `首页.png`（高保真）+ `全局.png` 屏 1 | 紫渐变 Hero + 4 统计卡、AI 发现流（4 条带徽章/标签）、能力图谱 SVG（9 节点+图例+节点/关系统计）、快速入口 4 卡、右栏 AI 助手（推荐动作/输入/在线状态）、最近活动、MCP 推广卡、侧栏索引进度卡与 slogan |
| 2 | 项目列表 | `全局.png` 屏 2 衍生 | 项目卡片网格 + 状态徽章 + 搜索联动 |
| 3 | 项目预览页（点任意项目卡进入） | `项目预览页.png`（高保真）+ `全局.png` 屏 2 | 封面头部+技术栈 chips+健康度环 92+创建/更新/规模信息+操作按钮（含禁用"归档"）、8 个 Tab（概览/代码结构/资产/知识/决策/经验/相关项目/AI 分析，计数按 mock 动态计算）、文件树+Python 语法高亮代码窗、项目亮点 4 卡、AI 资产分析（14 资产/3 统计/能力覆盖条）、核心资产卡（chips 过滤）、相关项目小图谱；**代码结构 Tab**：Level 0 统计（文件/代码行/符号/模块）+语言构成条+目录树+Project Archaeology 卡（Sessions/Commits/完成度/当前阶段/可打捞资产）；**相关项目 Tab**：similar_to 关系项目卡 |
| 4 | 资产库 Assets | `全局.png` 屏 3 | 8 类 chips 过滤（含 1 个禁用项）、资产卡（reuse_score/来源/标签/查看）、搜索框联动、空状态 |
| 5 | 知识图谱 Graph | `全局.png` 屏 4 | 关系图/时间线切换（时间线置为禁用）、节点点选高亮、右栏 Used in / Related / 统计 |
| 6 | 洞察 Insights | `全局.png` 屏 5 + `首页.png` AI 发现 | 分类 chips、Evidence 折叠（confidence）、有用/无用反馈按钮、机会发现入口 |
| 7 | AI 分析师 Analyst | `全局.png` 屏 6 | 居中大输入框 + 6 个预置问题 chips + 提问后"加载中→带证据回答"流程 |
| 8 | MCP / Skills | `全局.png` 屏 7 | Server Running 徽章、Available to、8 个 MCP 工具、6 个 Skills、启停切换（停止→错误状态） |
| 9 | 机会发现 Opportunities | `全局.png` 屏 8 | 机会卡（来源项目/已具备能力 78%/缺失能力/★评级/Explore/Dismiss），Dismiss 全部后进入空状态 |
| 10 | 设置 Settings（2026-09-29 新增） | 无设计图，按产品约束自拟 | 4 分区：**大模型配置**（云端 4 提供商卡片 + Base URL + API Key 掩码/显隐 + 模型下拉 + 连接测试；本地 3 后端；三级分析任务路由 segmented；敏感项目仅本地 / Embedding 强制本地开关）、**扫描目录**（目录级授权列表 + 添加/移除 + 空状态）、**外观**（主题切换）、**数据与隐私**（单机 Local-First 说明 + 危险操作） |

## 2026-09-29 增量需求落地记录

1. **亮色主题切换**：顶栏明暗分段开关 + 设置→外观；`tokens.css` 以 `[data-theme="light"]` 覆盖同名变量实现，组件零裸值；主题持久化到 localStorage；代码窗两主题下保持编辑器深色（与真实 IDE 一致）；强调面板（AI 资产分析/考古/机会卡）走 `--grad-accent` 等变量，亮色下自动变浅紫底。
2. **首页项目扫描按钮**：Hero 下方新增扫描条，含「增量扫描 / 全量扫描」双入口；点击后进入进度态（阶段文案 + 百分比 + 已发现项目数 + 实时日志 + 取消按钮），阶段定义来自 `mock-data.js → settings_meta.scan_stages`，完成/取消后回到空闲态。
3. **去掉用户体系（单机版）**：移除顶栏头像与通知铃铛；同步文案改为「本地数据库 · 单机模式」；AI 助手问候去掉用户名（"我可以帮你："）；设置页明确"无账号体系，所有配置仅保存在本机"。
4. **大模型配置项**：见上表设置页第 1 分区；字段命名对齐《技术设计书》§16 LLM 抽象层（OpenAI-compatible trait / 本地后端 / 三级分析路由 / Local-First 隐私约束）。

## 交互状态覆盖

- **默认 / 悬停 / 选中**：全部按钮、chips、导航、卡片均有 hover 与 is-active 样式（CSS 变量驱动）。
- **禁用**：侧栏"帮助"、图谱"时间线"、资产"Outcome"chip、项目页"归档"按钮。
- **空数据**：搜索无结果、资产/洞察过滤为空、机会全部 Dismiss 后、扫描目录清空后。
- **加载中**：右下角"状态演示"开关可全局切换骨架屏；AI 提问、连接测试、扫描进度也走 loading。
- **错误提示**：状态演示切"错误"、MCP 停止后、空搜索回车、AI 提问空输入、连接测试失败、扫描取消，Toast 错误样式。

## 跳转链路（可完整走通）

侧栏 6+3+1 项互跳（含设置）→ 首页 AI 发现/更多发现 → 洞察；首页图谱/查看完整图谱 → Graph；快速入口 → 项目/资产/分析师；首页扫描条 → 全量/增量扫描进度态（可取消）；项目卡 → 项目预览页（Tab 切换）→ 相关项目"查看完整关系" → Graph；AI 助手推荐动作/回车 → 分析师页带证据回答；洞察/首页机会卡 → 机会发现；MCP 推广卡 → MCP 页；顶栏全局搜索回车 → 资产页带过滤结果；顶栏齿轮 / 侧栏设置 → 设置页（4 分区互切）。

## 与现有代码的一致性说明

当前仓库除 `docs/` 外**不存在业务代码**（0 个 ts/rs/py 文件），故无既有组件/工具函数/样式变量可复用。原型遵循的"现有约定"为：

1. **数据模型**：`mock-data.js` 字段严格对齐《技术设计书》§11（`projects/assets/capabilities/relations/insights/opportunities/jobs`，snake_case，含 `reuse_score/confidence/source_path/user_feedback` 等），后续可直接替换为接口返回。
2. **技术方向**：与《技术设计书》选型一致（React+Vite+TS 为正式栈）；原型刻意采用零依赖静态实现以便 file:// 验收，组件粒度（card/chip/stat/asset-card…）与 tokens 变量可在迁移时 1:1 映射为 React 组件 + CSS Modules/Tailwind。
3. **命名**：页面 key 与产品设计书 §7 六页面命名一致（Overview/Projects/Assets/Graph/Insights/Analyst + MCP/Opportunities）。

## 验收记录

- **运行时冒烟测试**：jsdom 加载 index.html 执行全部脚本，**48/48 断言通过**、零运行时错误（覆盖 10 页渲染、品牌、主题切换/持久化、扫描流程、设置 4 分区、LLM 提供商切换与 Key 掩码、Tab/chips/节点点选/反馈/启停/Dismiss/搜索回车/三态切换）。
- **真实浏览器视觉验收**：Chromium 实截 25 张截图存于 `assets/img/`——暗色 10 页 + 亮色 10 页（overview/projects/project/assets/graph/insights/analyst/mcp/opportunities/settings）+ 扫描进行中态 + 平板 1140（暗/亮）+ 移动 390（暗/亮）；三档断点均无布局错乱或溢出。
- **已修复的视觉问题**：内联 SVG 无尺寸导致 flex 溢出（统一 `svg.ico` 基准尺寸）；Hero 问候语换行；能力条标签换行；代码窗头部换行；图谱重复节点名；1140/390 断点网格收拢；设置页左右栏反置（新增 `.cols-set`）；健康环轨道硬编码深色（改 `var(--color-panel-3)`）；侧栏"知识/资产"双高亮（active 判定改用导航项自身 key）；390px 顶栏"AI 助手"chip 换行溢出（移动端隐藏 chip 与 ⌘K 提示，分析师仍可从首页快速入口进入）。

## 已定稿的决策（原 6 项不确定点，2026-09-28 由产品侧授权自决）

1. **品牌名 = Spolia**。副标题沿用设计图 "Your Personal R&D OS"；文档体系中的 "Project Intelligence" 作为品类描述语保留（README/总纲中表述为"Spolia — 个人研发资产智能平台"）。命名查重与可用性结论见文首。
2. **量级口径 = 设计稿数字**。侧栏计数与统计卡统一取自 `mock-data.js → scale`（128 / 1,284 / 312 / 7 / 8），列表 mock 为采样数据；接入真实接口后 `scale` 由聚合查询替换，UI 无需改动。
3. **项目 Tab 定稿为 8 个**：删除"更多"；"代码结构"补正式内容（Level 0 统计 + 语言构成 + 目录树 + Project Archaeology 卡）；"相关项目"补 similar_to 项目卡。Tab 计数改为按该项目 mock 资产动态计算。
4. **图谱分类色定为官方色板** `mock-data.js → kind_colors`：能力 #a855f7 / 项目 #22c55e / 代码 #3b82f6 / 知识 #eab308 / 经验 #06b6d4 / 关系 #64748b（边）。首页图例与图谱页共用此色板。
5. **移动端形态定稿**：≤1024px 侧栏收为 64px 图标栏；≤768px 隐藏侧栏、单列堆叠、Hero 统计 2 列。设计图仅桌面稿，此方案为原型自拟并经三档实截验证。
6. **Outcome chip 保持禁用**：与产品设计书 V0.2 范围一致（Outcome 资产属阶段二 V0.2 后期），禁用态同时充当"禁用状态"的演示样本。
