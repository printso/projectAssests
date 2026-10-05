# projectAssests 交付总览

**projectAssests**（古典建筑术语：从旧建筑拆下、重新砌入新建筑的石材构件）——把历史项目中沉淀的资产重新砌进下一个项目。tagline：「让过去的每一个项目，都成为你未来的可能性」。

个人研发资产引擎：扫描本机代码项目 → 抽取资产/能力/关系 → 跨项目洞察 → LLM 分析。单机桌面形态，数据全部存本地 SQLite，无账号体系。

## 当前状态（2026-10-02）

| 层 | 位置 | 状态 |
|---|---|---|
| 领域模型 | `crates/domain` | ✅ 完成 |
| 存储（SQLite+FTS5 trigram+迁移） | `crates/storage` | ✅ 完成（schema v2） |
| 扫描器（rayon 并行+Git 分析） | `crates/scanner` | ✅ 完成 |
| 资产/洞察/检索/任务/AI | `crates/{asset,insight,search,jobs,ai}` | ✅ 完成 |
| Service 层（10 模块，传输无关） | `crates/service` | ✅ 完成 |
| HTTP 适配器（44 端点+SSE） | `apps/server` | ✅ 完成并真实进程验证 |
| MCP Server | `crates/mcp` | ⏸ 空骨架（产品原则：阶段三） |
| React 前端（11 页，全真实数据） | `apps/desktop` | ✅ 完成并真实浏览器验证 |
| 静态原型（已被前端取代） | `prototype/` | 🗄 保留作设计参照 |

**验证**：后端 1489 测试通过、clippy 零警告；前端 TS strict 零错误、生产构建干净；真实浏览器逐页目检通过。

## 四条产品原则（贯穿全部实现）
1. **首页简单但内心强大**：首屏只答三问（有什么/该做什么/系统发现了什么），深度藏在各页。
2. **一切设计符合架构原则**：业务收敛在传输无关的 service 层；错误码/徽章/排序键等单一真相源在 domain；适配器只做机械转换。
3. **前期不做 MCP，一切围绕 LLM**：MCP 降级阶段三保留骨架；分析师先检索后喂模型，引用走候选集白名单。
4. **面向能编码的用户**：界面给真实证据（文件/提交/置信度），不装饰、不编造。

## 核心机制
- **三级流水线**：扫描(Level 0)→索引(Level 1)→洞察(Level 2)，链式自动续跑（payload `chain` 标记驱动，引擎侧推进）。
- **防幻觉**：画像 evidence 逐条比对真实文件清单；分析师引用走白名单，被拒计数如实上报。
- **中文检索**：trigram FTS + 长 CJK 查询展开为「完整短语 OR 滑动 trigram」；2 字查询 LIKE 回退并如实标注。
- **Local-First**：敏感项目强制只用本地模型；本地端点绕过系统代理。
- **任务并发保护**：同类任务 AlreadyRunning 拒绝；进度 100% 不提前翻终态（终态只能由 handler 返回决定）。

## 关键修复（真实进程/浏览器发现，单测从未覆盖）
- 进度 100% 提前翻终态 → 可触发并发扫描互相覆盖写库。
- 扫描后不自动续跑索引/洞察（设计书 §13 要求）→ 产品核心价值不兑现。
- axum 内建提取器绕过统一信封 → newtype `JsonBody`/`QueryOf`，27 handler 切换。
- `apps/server/main.rs` 曾是 `fn main(){}`，state/error 两文件 25KB 从未编译。
- **存储型 XSS**：`snippet::highlight` 不转义原文，用户仓库描述可注入 `<img onerror>` 执行任意 JS → 后端加 `escape_html` + 5 个回归测试 + 变异验证。
- 中文整句查询在 trigram FTS 下必然落空 → 滑动 trigram OR 展开。
- 洞察/机会不在检索范围 → 四层补齐（domain/storage/search/service），V2 迁移含存量回填。
- `citation_kind` 双份重复映射用 `_ => File` 吞掉新变体 → 收敛到 domain 穷举 match。
- **「清理派生数据」三层契约互相矛盾**（安全级）：前端弹窗承诺"保留审计日志/反馈标注"，
  service 文档称"保留项目"，但实现里 `del("audit_log")` 是**第一行**、`del("projects")` 删整行。
  后果不止 UX 不一致——删项目行会连带抹掉用户手写的 `sensitive` 标记，重扫后敏感项目
  可能被送去云端模型（安全事故）；审计日志能被普通操作抹掉，则「可审计」形同虚设。
  修法：storage 只重置项目派生列（保留行 + sensitive/description）、不清 audit_log；
  service 键名 `projects`→`projects_reset`、preserved 补列"项目清单/审计日志"；
  前端弹窗删除兑现不了的"保留反馈标注"承诺，改为如实告知 user_feedback 会随行删除。
  storage+service 各加保留契约测试，双变异验证（注入 `sensitive=0`、恢复 `del(audit_log)`）均如期失败。

## 前端要点（apps/desktop）
- React 18 + Vite 5 + TS strict（`noUncheckedIndexedAccess`）；设计 token 复用原型 `tokens.css`/`base.css`（WCAG 校验过），`app.css` 只补增量。
- API 类型是 103 个 Rust DTO 的逐字段镜像；信封解包唯一入口；`ApiError`/`NetworkError` 严格区分。
- SSE 进度隔离在 ProgressContext（避免每帧重建路由树）；服务离线显示全屏启动引导而非各页报错。
- 筛选状态同步 URL（可分享/后退可用/刷新不丢）；分页 total 信任后端真 COUNT。
- 刻意移除原型全部假数据：侧栏计数、扫描进度、增长 delta、"已支持 Cursor/Claude Code" 等。

## UX 迭代记录

### 2026-10-03 · 目录选择弹窗（点选替代手输）
用户反馈"手输绝对路径不合习惯，想要点选弹框"。按 RICE 评估后落地为高优先级：这是新用户 onboarding 的第一步，每多一步（切资源管理器→复制→粘贴→怕打错）都流失一批人。

- **后端**：新增 `crates/service/src/fs.rs`（`list_dir`）与 `GET /api/fs/list?path=&show_hidden=`。只读、只列目录、单层上限 500（超限诚实标记 `truncated`）、隐藏目录默认折叠、路径统一归一化为正斜杠（盘符根 `C:`→`C:/`）。13 个单测 + 变异验证。
- **前端**：新增 `components/DirPickerModal.tsx`（浏览/勾选/面包屑/快速根/隐藏开关/手输兜底/部分失败重试）、`lib/useAddDirs.ts`（批量添加：逐个提交而非快速失败，避免"多选里 1 个错→全丢"）。
- **设置页**：主入口改为「选择目录…」弹窗，手输降级为次级兜底。
- **首页 onboarding**：`OnboardingStep` 增加 `action_key`（`pick_dirs`/`start_scan`/`start_index`），第一步就地开弹窗、第三步直接补跑索引，不再"跳设置页让用户自己找"。

### 2026-10-03 · 全站体验审计（修复 6 个交互断点）
对 11 个页面 + 全局组件做体验审计（亲读高频页 + agent 审大文件，每条亲自核对代码），修复：

1. 🔴【崩溃级】`Assets.tsx` 详情分支早返回位于 hooks 之前 → 列表↔详情切换触发 React "fewer hooks" 白屏。改为薄分发器 + `AssetListView` 子组件，两分支 hooks 恒定。
2. 🔴【白屏级】`Analyst.tsx` 自拼 `openCitation` 路由、绕过 `routeForLink`，点项目引用弹回首页。改用单一真相源。
3. `ProjectDetail.tsx`「刷新」按钮 navigate 到当前同路由是 no-op → 改 `reload()` + busy 态。
4. `ProjectDetail.tsx` 敏感标记切换无 pending 态（连点发相反请求）→ 加 busy 锁。
5. `ProjectDetail.tsx` + `States.tsx`「生成画像」生成中仍可点（重复调 LLM）→ `EmptyState.action` 支持 disabled。
6. `Sidebar.tsx` 离线态藏了"每 3 秒自动重连"的事实 → 改 `OfflineCard`：区分连接中/离线、如实说明自动重连、给「立即重试」。

沉淀为项目铁律（见 `.workbuddy/memory/MEMORY.md`）：React Hooks 早返回顺序、路由跳转走 `routeForLink`。

## 运行方式
```bash
# 后端（需 MSVC 环境）
source scripts/msvc-env.sh
cargo run -p projectassests-server            # 默认 127.0.0.1:8787

# 前端
cd apps/desktop && npm install && npm run dev   # 5174（5173 被本机无关项目占用），代理 /api → 8787
```

## 遗留 / 后续
- MCP Server（阶段三）：crate 为空骨架，前端导航如实标"阶段三" disabled。
- 项目详情"代码预览"未做：后端无文件内容端点，读文件内容超出当前范围。
- Tauri 桌面壳（设计书阶段一目标）：当前 HTTP 传输已验证全链路，IPC 适配为薄层待接。
- **未来设想（未排期）**：见 `docs/未来设想.md`。当前 1 条：IDEA-001 多机扫描（跨机器汇总分析、支持 Linux）。
  该文件专门记录"以后也许做"的设想及其与现有产品原则的冲突点，不作为承诺或路线图。
