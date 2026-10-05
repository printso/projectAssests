/**
 * Spolia API 端点。
 *
 * 🔴 **每个函数对应 `apps/server/src/routes.rs` 里的一条路由**，
 * 路径、方法、参数位置（query / body / path）都按后端实际绑定写。
 * 改后端路由时必须同步这里，否则请求会 404 或 400。
 *
 * # 参数约定
 * - 列表类：可选参数一律**不传即省略**（后端 `#[serde(default)]` 会用其默认值）。
 *   不要传 `undefined` 之外的"空值"——例如传 `project_id: ""` 在索引端点
 *   意味着"全量索引"，那是完全不同的操作。
 * - 触发类（扫描/索引/生成洞察）：返回 `SubmitResponse`，
 *   其 `message` 是给用户看的确认文案，应直接显示。
 */

import { http, toQuery } from "./client";
import type {
  AnalystResponse,
  AnalystTurn,
  AppearanceSettings,
  AssetDetail,
  AssetListItem,
  AssetListPage,
  AuditView,
  ClearResult,
  EmptyData,
  ExportEntry,
  FsListView,
  GraphView,
  HealthView,
  InsightDetail,
  InsightItem,
  InsightListPage,
  JobListPage,
  JobView,
  LlmView,
  NeighborhoodView,
  OpportunityDetail,
  OpportunityItem,
  OpportunityListPage,
  Overview,
  ProfileResponse,
  ProjectDetail,
  ProjectListItem,
  ProjectListPage,
  ScanView,
  SearchScope,
  SearchView,
  SettingsView,
  SortBy,
  SubmitResponse,
  TestConnectionView,
  TypeBreakdown,
} from "./types";

// ══════════════════════════════════════════════════════════════════
// 系统
// ══════════════════════════════════════════════════════════════════

/** 健康检查。用于判断本地服务是否已启动（前端启动时的第一道探测）。 */
export const getHealth = (signal?: AbortSignal) => http.get<HealthView>("/api/health", signal);

/** 首页聚合数据。一次请求拿齐统计卡、图谱预览、新洞察、机会、活动流、引导。 */
export const getOverview = (signal?: AbortSignal) => http.get<Overview>("/api/overview", signal);

// ══════════════════════════════════════════════════════════════════
// 项目
// ══════════════════════════════════════════════════════════════════

export interface ProjectListParams {
  status?: string;
  language?: string;
  keyword?: string;
  sensitive?: boolean;
  sort?: string;
  limit?: number;
  offset?: number;
}

export const listProjects = (params: ProjectListParams = {}, signal?: AbortSignal) =>
  http.get<ProjectListPage>(`/api/projects${toQuery(params)}`, signal);

export const getProject = (id: string, signal?: AbortSignal) =>
  http.get<ProjectDetail>(`/api/projects/${encodeURIComponent(id)}`, signal);

export const removeProject = (id: string) =>
  http.del<EmptyData>(`/api/projects/${encodeURIComponent(id)}`);

/**
 * 标记/取消敏感项目。返回更新后的项目条目。
 *
 * 🔴 敏感项目按 Local-First 策略**只用本地模型**，绝不发往云端。
 * 这个开关直接决定用户的代码内容会不会离开本机，UI 上必须说清后果。
 */
export const setProjectSensitive = (id: string, sensitive: boolean) =>
  http.put<ProjectListItem>(`/api/projects/${encodeURIComponent(id)}/sensitive`, { sensitive });

/** 更新项目描述。返回更新后的项目条目。 */
export const setProjectDescription = (id: string, description: string) =>
  http.put<ProjectListItem>(`/api/projects/${encodeURIComponent(id)}/description`, { description });

/** 重建单个项目索引（不带 chain，不会顺带触发全局洞察）。 */
export const reindexProject = (id: string) =>
  http.post<ReindexResponse>(`/api/projects/${encodeURIComponent(id)}/reindex`);

/**
 * `/api/projects/{id}/reindex` 的响应。
 *
 * 🔴 形状是 `{job_id, message}`（handler 用 `serde_json::json!` 直接拼），
 * **不是** `SubmitResponse`（那有 `job_type`/`job_type_label`）。
 * 两者别混用，否则前端读 `job_type` 会拿到 undefined。
 */
export interface ReindexResponse {
  job_id: string;
  message: string;
}

/**
 * 生成/重新生成 AI 画像。
 *
 * 🔴 这是**耗时且可能调用云端模型**的操作，必须有明确的加载态。
 * `force: true` 表示忽略缓存重新生成（用户点"重新分析"时）。
 */
export const generateProfile = (id: string, force = false) =>
  http.post<ProfileResponse>(`/api/projects/${encodeURIComponent(id)}/profile`, { force });

// ══════════════════════════════════════════════════════════════════
// 资产
// ══════════════════════════════════════════════════════════════════

export interface AssetListParams {
  project_id?: string;
  asset_type?: string;
  /** 多选类型，数组会被序列化为逗号分隔（后端 `types=a,b` 约定） */
  types?: string[];
  keyword?: string;
  min_reuse_score?: number;
  /** high / medium / low */
  tier?: string;
  sort?: string;
  limit?: number;
  offset?: number;
}

export const listAssets = (params: AssetListParams = {}, signal?: AbortSignal) =>
  http.get<AssetListPage>(`/api/assets${toQuery(params)}`, signal);

/** 资产类型分布。`all_types` 含计数为 0 的类型，chips 必须用它渲染。 */
export const getAssetTypes = (params: AssetListParams = {}, signal?: AbortSignal) =>
  http.get<TypeBreakdown>(`/api/assets/types${toQuery(params)}`, signal);

export const getAsset = (id: string, signal?: AbortSignal) =>
  http.get<AssetDetail>(`/api/assets/${encodeURIComponent(id)}`, signal);

/**
 * 资产反馈。
 *
 * 🔴 返回**更新后的条目**（`AssetListItem`），不是空体：
 * 前端应直接用它替换列表里的对应项，这样 `user_feedback` 立刻反映在 UI 上，
 * 不必重新拉整个列表（列表可能有几百条，重拉的代价和延迟都明显）。
 *
 * @param feedback `useful` / `useless` / `ignored`；传 `null` 表示**撤销**已有反馈。
 */
export const setAssetFeedback = (id: string, feedback: string | null) =>
  http.post<AssetListItem>(`/api/assets/${encodeURIComponent(id)}/feedback`, { feedback });

// ══════════════════════════════════════════════════════════════════
// 检索
// ══════════════════════════════════════════════════════════════════

export interface SearchParams {
  /** 空/缺省 = 浏览模式（不做关键词匹配，按质量列出） */
  q?: string;
  scope?: SearchScope;
  asset_type?: string;
  project_status?: string;
  language?: string;
  project_id?: string;
  min_reuse_score?: number;
  sort?: SortBy;
  limit?: number;
  offset?: number;
}

export const search = (params: SearchParams = {}, signal?: AbortSignal) =>
  http.get<SearchView>(`/api/search${toQuery(params)}`, signal);

// ══════════════════════════════════════════════════════════════════
// 图谱
// ══════════════════════════════════════════════════════════════════

export interface GraphParams {
  project_id?: string;
  capability_id?: string;
  /** 能力层级筛选，数组 → 逗号分隔 */
  layers?: string[];
  relation_types?: string[];
  max_nodes?: number;
  max_edges?: number;
  /** 是否展开二跳邻居 */
  expand_neighbors?: boolean;
}

export const getGraph = (params: GraphParams = {}, signal?: AbortSignal) =>
  http.get<GraphView>(`/api/graph${toQuery(params)}`, signal);

/** 某节点的邻域（点击节点后聚焦查看）。 */
export const getNeighborhood = (
  id: string,
  params: GraphParams = {},
  signal?: AbortSignal,
) =>
  http.get<NeighborhoodView>(
    `/api/graph/neighborhood/${encodeURIComponent(id)}${toQuery(params)}`,
    signal,
  );

// ══════════════════════════════════════════════════════════════════
// 洞察与机会
// ══════════════════════════════════════════════════════════════════

export interface InsightListParams {
  types?: string[];
  /** true = 只看未处理；false = 只看已处理；缺省 = 全部 */
  unread_only?: boolean;
  min_confidence?: number;
  limit?: number;
  offset?: number;
}

export const listInsights = (params: InsightListParams = {}, signal?: AbortSignal) =>
  http.get<InsightListPage>(`/api/insights${toQuery(params)}`, signal);

export const getInsight = (id: string, signal?: AbortSignal) =>
  http.get<InsightDetail>(`/api/insights/${encodeURIComponent(id)}`, signal);

/**
 * 洞察反馈。返回更新后的条目（理由同 `setAssetFeedback`）。
 *
 * 🔴 反馈会驱动"采纳率"指标与后续洞察排序，是产品闭环的关键一环，
 * UI 上必须给即时的视觉确认（否则用户以为没点上而反复点击）。
 */
export const setInsightFeedback = (id: string, feedback: string | null) =>
  http.post<InsightItem>(`/api/insights/${encodeURIComponent(id)}/feedback`, { feedback });

export interface OpportunityListParams {
  statuses?: string[];
  include_closed?: boolean;
  min_rating?: number;
  limit?: number;
  offset?: number;
}

export const listOpportunities = (params: OpportunityListParams = {}, signal?: AbortSignal) =>
  http.get<OpportunityListPage>(`/api/opportunities${toQuery(params)}`, signal);

export const getOpportunity = (id: string, signal?: AbortSignal) =>
  http.get<OpportunityDetail>(`/api/opportunities/${encodeURIComponent(id)}`, signal);

/**
 * 变更机会状态。
 * @param status `new` / `explored` / `dismissed` / `adopted` 等（后端校验）
 */
export const setOpportunityStatus = (id: string, status: string) =>
  http.post<OpportunityItem>(
    `/api/opportunities/${encodeURIComponent(id)}/status`,
    { status },
  );

/**
 * 忽略全部可操作机会。
 *
 * 🔴 这是**破坏性批量操作**，UI 必须二次确认。
 * 返回受影响条数，toast 应显示"已忽略 N 条"而非笼统的"成功"。
 */
export const dismissAllOpportunities = () =>
  http.post<EmptyData>("/api/opportunities/dismiss-all");

// ══════════════════════════════════════════════════════════════════
// 任务
// ══════════════════════════════════════════════════════════════════

export const listJobs = (params: { limit?: number; offset?: number } = {}, signal?: AbortSignal) =>
  http.get<JobListPage>(`/api/jobs${toQuery(params)}`, signal);

export interface ScanParams {
  /** 空数组 = 用设置里已启用的目录 */
  dirs?: string[];
  /** Git 历史分析（大仓库很慢，允许用户关掉） */
  analyze_git?: boolean;
  /**
   * 是否自动续跑「索引 → 洞察」全链。**后端默认 true**。
   * 🔴 前端不要显式传 false，除非用户明确选了"只扫描不分析"——
   * 否则用户扫完看到的是空资产页与空洞察页，而界面上没有任何入口能补跑。
   */
  chain?: boolean;
}

export const startScan = (params: ScanParams = {}) =>
  http.post<SubmitResponse>("/api/jobs/scan", params);

/**
 * 触发索引。
 * @param projectId 缺省 = 索引**全部项目**（跨项目分析的素材来源）
 */
export const startIndex = (projectId?: string) =>
  http.post<SubmitResponse>(
    "/api/jobs/index",
    projectId ? { project_id: projectId } : {},
  );

/** 触发洞察生成（无 body）。 */
export const startInsights = () => http.post<SubmitResponse>("/api/jobs/insights");

/** 取消任务。返回更新后的任务视图（status 应为 `cancelled`）。 */
export const cancelJob = (id: string) =>
  http.post<JobView>(`/api/jobs/${encodeURIComponent(id)}/cancel`);

// ══════════════════════════════════════════════════════════════════
// 对话式分析师
// ══════════════════════════════════════════════════════════════════

export interface AskParams {
  question: string;
  /** 多轮上下文。后端会把它一并喂给模型。 */
  history?: AnalystTurn[];
  /** 限定在某个项目内提问 */
  project_id?: string;
  scope?: SearchScope;
}

/**
 * 提问。
 *
 * 🔴 这是**长耗时请求**（要检索 + 可能调用 LLM），必须：
 * 1. 显示明确的等待态（不是静默转圈）
 * 2. 允许用户取消（传 signal）
 * 3. 未配置模型时后端会返回 200 + 降级回答（`generated_by: "deterministic"`），
 *    前端应显示"离线检索回答"标识并引导去配置——**这不是错误**，不要当失败处理。
 */
export const askAnalyst = (params: AskParams, signal?: AbortSignal) =>
  http.post<AnalystResponse>("/api/analyst/ask", params, signal);

// ══════════════════════════════════════════════════════════════════
// 设置
// ══════════════════════════════════════════════════════════════════

export const getSettings = (signal?: AbortSignal) =>
  http.get<SettingsView>("/api/settings", signal);

export interface SettingsUpdatePayload {
  llm?: Partial<{
    cloud_provider: string;
    cloud_base_url: string;
    /**
     * 🔴 只在用户真的输入了新 key 时发送。
     * 后端有掩码防护：把 `sk-****abcd` 原样回传会被识别为"未修改"，
     * 但若前端把掩码当明文发过去，可能覆盖掉真实 key。
     */
    cloud_api_key: string;
    cloud_model: string;
    local_backend: string;
    local_base_url: string;
    local_model: string;
    route_fast: string;
    route_deep: string;
    sensitive_local_only: boolean;
    embedding_local_only: boolean;
  }>;
  appearance?: Partial<{ theme: "dark" | "light"; reduce_motion: boolean }>;
}

/**
 * 更新大模型配置。
 *
 * 🔴 返回的是**扁平的 `LlmView`**（后端 `settings_llm` → `svc::settings::update_llm`
 * 直接返回 `LlmView`，不是带 `.llm/.scan/.appearance/.db` 外层的 `SettingsView`）。
 * 曾把它误标为 `SettingsView` 并用返回值整体覆盖设置页 state，
 * 导致保存后 `view.llm` 变 undefined、设置页白屏（用户误以为"保存没通过"，
 * 实际后端已写库成功）。调用方必须只把它合并进 `state.llm`。
 */
export const updateLlmSettings = (payload: NonNullable<SettingsUpdatePayload["llm"]>) =>
  http.put<LlmView>("/api/settings/llm", payload);

export const updateScanSettings = (
  payload: Partial<{
    watch_enabled: boolean;
    exclude_patterns: string[];
    level2_enabled: boolean;
    max_depth: number;
  }>,
) => http.put<ScanView>("/api/settings/scan", payload);

export const updateAppearance = (payload: Partial<AppearanceSettings>) =>
  http.put<AppearanceSettings>("/api/settings/appearance", payload);

/** 添加扫描目录。后端会校验目录真实存在，不存在返回 400。 */
export const addScanDir = (path: string) => http.post<ScanView>("/api/settings/dirs", { path });

/**
 * 浏览一层本机目录（「选择目录」弹窗的数据源）。
 *
 * 🔴 `path` 留空 = 列根（Windows 盘符 / Unix `/`）。
 * 后端只读不写、只列目录不列文件、单层有上限（`truncated` 为真时
 * 前端要提示用户改用手输定位，不能假装列全了）。
 */
export const listFsDirs = (params: { path?: string; show_hidden?: boolean } = {}, signal?: AbortSignal) =>
  http.get<FsListView>(`/api/fs/list${toQuery(params)}`, signal);

/** 移除扫描目录（DELETE 带 JSON body，这是后端约定）。 */
export const removeScanDir = (path: string) =>
  http.del<ScanView>("/api/settings/dirs", { path });

/** 启用/停用某个目录（停用后不再被扫描，但保留配置）。 */
export const toggleScanDir = (path: string, enabled: boolean) =>
  http.put<ScanView>("/api/settings/dirs/toggle", { path, enabled });

/**
 * 测试模型连接。
 *
 * 🔴 返回的 `models` 是后端**实际探测到的可用模型列表**——
 * 用户填错模型名时，用它给出"你是不是想用 xxx？"的引导，
 * 比一句"连接失败"有用得多。
 */
export const testConnection = (route?: string) =>
  http.post<TestConnectionView>("/api/settings/test-connection", route ? { route } : {});

export const getAuditLog = (params: { limit?: number } = {}, signal?: AbortSignal) =>
  http.get<AuditView[]>(`/api/settings/audit${toQuery(params)}`, signal);

/**
 * 导出配置。
 *
 * 🔴 返回**有序的键值对数组**（`Vec<(String,String)>` → `[[k,v],...]`），
 * 不是 map：后端刻意保持顺序，让用户导出的文件可按顺序阅读。
 * 前端渲染时要按数组顺序输出，不要转成对象（对象会丢顺序）。
 */
export const exportSettings = (signal?: AbortSignal) =>
  http.get<ExportEntry[]>("/api/settings/export", signal);

/**
 * 清理派生数据（资产/能力/关系/洞察/机会/索引）。
 *
 * 🔴 **破坏性操作**，必须二次确认。返回 `cleared`（各表清理行数）
 * 与 `preserved`（被刻意保留的数据说明：设置、项目清单与敏感标记、审计日志）。
 * 确认弹窗应把这两项都展示出来，让用户知道什么会丢、什么会留。
 *
 * ⚠️ `cleared` 里的键是 `projects_reset` 而非 `projects`：项目行只被重置派生列、
 * 并未删除。前端把键名原样渲染成 "xxx: N 行"，键名说谎 = 用户看到
 * "projects: 2 行" 以为项目清单没了。
 *
 * ⚠️ `user_feedback` **不在** preserved 里：它存在 assets/insights 行内，
 * 删行必然一起消失。弹窗必须如实告知反馈标注会丢（见 Settings.tsx）。
 */
export const clearDerived = () => http.post<ClearResult>("/api/settings/clear-derived");
