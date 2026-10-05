# Spolia

> 让过去的每一个项目，都成为你未来的可能性。

**Spolia**（古典建筑术语：从旧建筑拆下、重新砌入新建筑的石材构件）——一个**本地优先（Local-First）的个人研发资产引擎**。

它扫描你本机的代码项目，抽取其中的资产、能力与关系，做跨项目洞察，并用 LLM 帮你分析。所有数据默认只存在你本机的 SQLite 里——**没有账号体系、没有云端同步、不上传你的代码**。

---

## ✨ 特性

- **本地优先**：单安装包即可运行，数据默认不出本机；敏感项目可标记为 `sensitive`，其任何数据（含 Embedding）都不进云端模型上下文。
- **项目扫描**：识别本机代码项目（Cargo / npm / pyproject / Go / Maven …），并行分析技术栈、规模与 Git 历史。
- **资产抽取**：从代码中提炼可复用的资产、能力及其相互关系。
- **跨项目洞察**：基于 LLM 的分析与建议；分析前先检索、引用走候选集白名单，**防幻觉**。
- **中文检索**：trigram FTS5 + 长中文查询展开为「完整短语 OR 滑动 trigram」，2 字查询走 LIKE 回退并如实标注。
- **可审计**：设置页展示「哪些数据、发给哪个模型、什么时候」，审计日志不会被普通操作抹掉。
- **三级流水线**：扫描 → 索引 → 洞察链式自动续跑。

## 🏗 架构

Spolia 是一个 Cargo workspace，业务收敛在**传输无关的 `service` 层**，HTTP 只是其中一个适配器（未来也可接 Tauri IPC）。

```
crates/
├── domain    领域模型（单一真相源：错误码 / 徽章 / 排序键）
├── storage   SQLite + FTS5 trigram + 迁移（schema v2）
├── scanner   rayon 并行扫描 + Git 分析
├── asset     资产抽取引擎
├── insight   洞察引擎
├── search    检索引擎（中文 trigram）
├── jobs      任务引擎（并发保护）
├── ai        LLM 接入（本地 + 云端）
├── service   传输无关业务层（10 模块）
└── mcp       MCP Server（阶段三，当前为空骨架）
apps/
├── server    HTTP 适配器（axum，44 端点 + SSE）
└── desktop   React + Vite + TypeScript 前端（11 页，零静态假数据）
```

## 🧰 技术栈

| 层 | 技术 |
|---|---|
| 后端 | Rust 2024 edition（≥ 1.88）、axum 0.8、tokio、rusqlite（bundled + FTS5）、rayon |
| 存储 | SQLite（本地、Local-First）+ trigram 全文检索 |
| 前端 | React 18、Vite 5、TypeScript（strict）、react-router-dom |
| 分析 | Git 历史分析、LLM（本地模型 / 兼容 OpenAI 的云端模型） |

## 🚀 快速开始

### 前置要求

- **Rust** ≥ 1.88（edition 2024）
- **Node.js** 22（仅前端）
- **Windows 用户**：编译需要 MSVC 工具集。仓库提供了免 `cmd.exe` 的环境脚本：

```bash
source scripts/msvc-env.sh
```

### 启动后端

```bash
cargo run -p spolia-server          # 默认监听 127.0.0.1:8787
```

可选参数：`--db <path>`（数据库路径，默认 `%APPDATA%/spolia/spolia.db`）、`--addr <host:port>`。

### 启动前端

```bash
cd apps/desktop
npm install
npm run dev                          # 默认 http://localhost:5174（/api 代理到 8787）
```

打开浏览器访问 `http://localhost:5174`。

## 🔧 构建与测试

```bash
# 后端：编译 + 单测 + lint
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets

# 前端：类型检查 + 生产构建
cd apps/desktop
npm run typecheck
npm run build
```

> 后端单测套件（1489 项）通过、clippy 零警告；前端 TS strict 零错误、生产构建干净。

## ⚙️ 配置

Spolia 的大部分行为通过桌面端的「设置」页完成，无需手改配置文件：

- **扫描目录**：在设置页通过目录选择弹窗添加本机项目根目录（支持点选，无需手输绝对路径）。
- **LLM 接入**：配置本地模型（如 Ollama）或兼容 OpenAI 的云端模型；API Key 明文存于本地数据库，请妥善保管本机数据。
- **敏感项目**：将任意项目标记为 `sensitive`，该项目数据只走本地模型，绝不发往云端。

核心产品原则：**默认本地、明确授权云端模型**——这是 Spolia 的信任基础。

## 📁 项目结构

```
.
├── crates/        后端 Rust 引擎（见架构图）
├── apps/
│   ├── server/    HTTP 适配层
│   └── desktop/   前端（React + Vite）
├── docs/          产品设计 / 技术设计（内部资料，默认不随仓库发布）
├── scripts/       构建辅助（如 msvc-env.sh）
└── prototype/     早期静态原型（已被前端取代，仅作设计参照）
```

> `docs/` 与 `.workbuddy/` 默认被 `.gitignore` 排除；如需发布设计文档，移除 `.gitignore` 中的 `docs/*` 即可。

## 🔒 隐私

Spolia 是**本地优先**软件：扫描结果、索引、画像全部存于你本机的 SQLite 数据库。除非你显式配置并授权云端模型，否则你的代码与资产不会离开本机。`sensitive` 项目在授权后也只会使用本地模型。

## 📄 许可证

以双许可证发布，可在 **MIT** 或 **Apache-2.0** 中任选其一（与 `Cargo.toml` 中声明一致）。

> 发布到 GitHub 前，建议在仓库根目录放置 `LICENSE-MIT` 与 `LICENSE-APACHE` 两个文件。
