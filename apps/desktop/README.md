# projectAssests Desktop（React + Vite + TypeScript）

projectAssests 的桌面前端。数据全部来自本地 Rust 服务（`apps/server`），**零静态模拟数据**。

## 启动

```bash
# 1. 后端（另一个终端；需 MSVC 环境，见仓库根 scripts/msvc-env.sh）
source scripts/msvc-env.sh
cargo run -p projectassests-server          # 默认监听 127.0.0.1:8787

# 2. 前端
cd apps/desktop
npm install
npm run dev                         # http://127.0.0.1:5174
```

开发期前端通过 Vite proxy 把 `/api` 转发到后端（见 `vite.config.ts`），
因此前端代码里只写相对路径 `/api/...`，无需按环境切换 baseURL。
连非默认端口的后端：`PROJECTASSENTS_API=http://127.0.0.1:9000 npm run dev`。

**端口是 5174 而非 Vite 默认的 5173**：5173 是本机另一个无关项目长期占用的端口。
`strictPort: true` 是刻意的——端口被占时立即报错退出，而不是静默漂移到 5174、5175…
（漂移会让人打开并"验证"到别人家的页面，那次踩坑见 `.workbuddy/memory/2026-10-02.md`）。
需要换端口时用 `PROJECTASSENTS_PORT=5180 npm run dev`，不要改 `strictPort`。

后端未启动时，前端显示全屏启动引导（含启动命令），不会渲染一堆"加载失败"。

## 目录结构

```text
src/
├── api/
│   ├── types.ts        # 103 个 Rust DTO 的 TS 镜像（改后端 DTO 必须同步这里）
│   ├── client.ts       # 信封解包唯一入口；ApiError / NetworkError 区分
│   └── endpoints.ts    # 全部端点封装（路径与后端 routes.rs 一一对应）
├── lib/
│   ├── useAsync.ts     # 数据获取：竞态双防护 + mutate 就地更新
│   ├── useProgress.ts  # SSE 进度订阅与展示辅助
│   ├── ProgressContext.tsx  # 隔离 SSE 高频更新，避免全应用重渲染
│   ├── AppContext.tsx  # 服务状态 + health 统计（侧栏计数派生于此）
│   ├── useAppearance.ts# 主题：真相源是后端设置，localStorage 仅首屏缓存
│   ├── navigate.ts     # 后端 link.page → 前端路由的唯一映射
│   └── format.ts       # ISO 纳秒时间戳格式化（Date.parse 对 9 位小数不可靠）
├── components/         # Icon / States / Toast / ConfirmModal / MarkdownLite / ui
├── pages/              # 11 个页面
└── styles/             # tokens.css + base.css（复用原型设计系统）+ app.css（增量）
```

## 约定（违反会引入真实缺陷，详见各处 🔴 注释）

1. **类型对齐**：`api/types.ts` 每个字段对应一个 Rust `pub struct` 字段。
   改后端 DTO 而不同步这里，类型检查全绿但运行时读到 `undefined`。
2. **查询参数**：未设置的筛选项必须整个不发送（`toQuery` 已处理）。
   后端把"参数不存在"与"空串"当不同语义（空 `project_id` = 全量索引）。
3. **筛选状态在 URL**：可分享、后退可用、刷新不丢。改筛选时 offset 归零。
4. **写操作必有反馈**：成功用后端 `message`（它预告链式行为），失败显示 `hint`。
5. **破坏性操作走 ConfirmModal**：初始焦点在"取消"，点击遮罩 = 取消。
6. **不渲染原始 ISO 时间戳**：用 `lib/format.ts`；后端已给人性化字段时优先用后端的。
7. **CSS 不重复定义**：`base.css` 是原型设计系统的忠实副本，增量只进 `app.css`。

## 校验

```bash
npm run typecheck    # tsc --noEmit（strict + noUncheckedIndexedAccess）
npm run build        # tsc -b && vite build
```
