/**
 * projectAssests API 类型契约。
 *
 * 🔴 **本文件的每个类型都对应 `crates/service/src/*.rs` 里的一个 `pub struct`**，
 * 字段名与顺序按 Rust 侧 serde 的实际输出（snake_case）。
 *
 * 不要凭直觉改字段名或加"看起来该有"的字段：
 * 后端不会报错，前端只会在运行时读到 `undefined`，表现为界面某处莫名空白——
 * 这类 bug 极难定位，因为类型检查全绿。
 *
 * # Rust → TS 映射约定
 * - `String` / `&'static str`      → `string`
 * - `usize` / `u32` / `u8` / `f64` → `number`
 * - `bool`                         → `boolean`
 * - `Vec<T>`                       → `T[]`
 * - `Option<T>`                    → `T | null`（serde 默认输出 null）
 * - `#[serde(skip_serializing_if = "Option::is_none")]` → `T | undefined`（字段可能整个不存在）
 *
 * # 枚举
 * Rust 侧的字段枚举（`JobStatus`、`HitKind`、`AnswerSource` 等）都带
 * `#[serde(rename_all = "snake_case")]`，序列化成小写下划线字符串。
 * 这里用字符串字面量联合类型表达，让 switch 能被穷举检查。
 */

// ══════════════════════════════════════════════════════════════════
// 响应信封（apps/server/src/error.rs）
// ══════════════════════════════════════════════════════════════════

/** 成功响应：`{ success: true, data: T }` */
export interface Envelope<T> {
  success: true;
  data: T;
}

/**
 * 错误响应：`{ success: false, error: { code, message, hint? } }`
 *
 * `code` 是**稳定的机器可读标识**，前端据它分支（例如 `no_scan_dirs` → 显示设置入口）。
 * 🔴 绝不要匹配 `message` 文本来判断错误类型——那是给人看的，措辞随时会调整。
 */
export interface ErrorBody {
  success: false;
  error: {
    code: ErrorCode;
    message: string;
    /** 可操作引导（"打开 设置 → 扫描目录…"）。后端按错误类型给出，可能不存在。 */
    hint?: string;
  };
}

/** 触发类端点的空响应体（`Empty`，字段均可缺省）。 */
export interface EmptyData {
  affected?: number;
  message?: string;
}

/**
 * 已知错误码全集（`ServiceError::code()`）。
 *
 * 🔴 加新错误码时后端是唯一真相源，这里跟着补；
 * 漏补不会导致运行时错误（未知码会落到 `string`），
 * 但会让前端失去针对该错误的专门引导能力。
 */
export type ErrorCode =
  | "bad_request"
  | "not_found"
  | "precondition_failed"
  | "conflict"
  | "storage_error"
  | "storage_unavailable"
  | "job_not_found"
  | "job_already_running"
  | "job_already_finished"
  | "job_error"
  | "llm_not_configured"
  | "llm_unauthorized"
  | "llm_rate_limited"
  | "llm_timeout"
  | "llm_error"
  | "sensitive_blocked"
  | "cancelled"
  | "search_error"
  | "index_not_ready"
  | "no_scan_dirs"
  | "config_error"
  | "dir_not_found"
  | "not_authorized"
  | "scan_error"
  | "asset_error"
  | "internal_error"
  | (string & {}); // 允许未知码，避免后端新增时前端直接崩

// ══════════════════════════════════════════════════════════════════
// 概览页（service/overview.rs :: Overview）
// ══════════════════════════════════════════════════════════════════

export interface Overview {
  stats: StatCard[];
  graph_preview: GraphPreview;
  recent_insights: InsightBrief[];
  top_opportunities: OpportunityBrief[];
  activities: ActivityBrief[];
  /** 当前/最近一个任务。从未跑过任务时为 null。 */
  job: JobBrief | null;
  /**
   * 首次使用引导。全部步骤完成后为 null。
   * 🔴 这是"首页简单但内心强大"的关键：
   * 空库时不给用户一片空白，而是告诉他下一步该点什么。
   */
  onboarding: Onboarding | null;
  last_scanned_at: string | null;
  unread_insights: number;
}

export interface StatCard {
  key: string;
  label: string;
  value: number;
  detail: string | null;
  link_page: string;
  /**
   * 该指标为 0 是否属于预期（例如"洞察数"在未生成洞察前本就应为 0）。
   * 🔴 前端据此决定 0 值该显示成中性还是"需要处理"，
   * 否则满屏红色告警会让用户误以为系统坏了。
   */
  empty_is_expected: boolean;
}

export interface GraphPreview {
  node_count: number;
  edge_count: number;
  top_capabilities: TopItem[];
}

export interface TopItem {
  name: string;
  count: number;
}

export interface InsightBrief {
  id: string;
  title: string;
  summary: string;
  insight_type: string;
  type_label: string;
  confidence: number;
  badge: string;
}

export interface OpportunityBrief {
  id: string;
  title: string;
  description: string;
  rating: number;
  coverage: number;
  reusable_count: number;
  missing_count: number;
}

export interface ActivityBrief {
  id: string;
  icon: string;
  title: string;
  detail: string;
  when: string;
}

export interface JobBrief {
  id: string;
  job_type: string;
  type_label: string;
  status: JobStatus;
  status_label: string;
  percent: number;
  stage: string | null;
  counter: string | null;
  error: string | null;
  cancellable: boolean;
}

export interface Onboarding {
  headline: string;
  steps: OnboardingStep[];
}

export interface OnboardingStep {
  title: string;
  detail: string;
  done: boolean;
  action_page: string | null;
  /**
   * 直达动作 key（比跳页面更短的路径）：
   * - `pick_dirs`：就地打开目录选择弹窗
   * - `start_scan`：直接触发扫描
   * - `start_index`：直接补跑索引
   * 为 null 时回退到 `action_page` 跳转。
   */
  action_key: string | null;
}

// ══════════════════════════════════════════════════════════════════
// 本机目录浏览（service/fs.rs，设置页「选择目录」弹窗）
// ══════════════════════════════════════════════════════════════════

/** 单个目录条目（弹窗只渲染目录，不渲染文件）。 */
export interface FsEntry {
  name: string;
  /** 完整路径：勾选后直接提交给 addScanDir */
  path: string;
  /** 是否还有子目录（false 时禁用展开箭头） */
  has_children: boolean;
}

export interface FsListView {
  path: string;
  /** 父目录；根层为 null */
  parent: string | null;
  entries: FsEntry[];
  /** 单层条目超过后端上限被截断（前端提示用手输/搜索定位） */
  truncated: boolean;
  show_hidden: boolean;
  /** 快速入口（主目录/桌面/文档/下载/当前工作目录） */
  roots: FsEntry[];
}

// ══════════════════════════════════════════════════════════════════
// 项目（service/projects.rs）
// ══════════════════════════════════════════════════════════════════

export type ProjectStatus = "active" | "idle" | "archived" | "unknown";

export interface ProjectListPage {
  items: ProjectListItem[];
  total: number;
  limit: number;
  offset: number;
  facets: ProjectFacets;
}

export interface ProjectListItem {
  id: string;
  name: string;
  path: string;
  description: string;
  language: string;
  framework: string;
  status: ProjectStatus;
  status_label: string;
  health_score: number;
  files: number;
  loc: number;
  tags: string[];
  sensitive: boolean;
  days_idle: number | null;
  updated_display: string;
  has_git: boolean;
  git_commits: number;
  has_readme: boolean;
  has_tests: boolean;
  has_profile: boolean;
  primary_language_pct: number;
}

export interface ProjectFacets {
  languages: FacetItem[];
  statuses: FacetItem[];
}

export interface FacetItem {
  value: string;
  label: string;
  count: number;
}

export interface ProjectDetail {
  summary: ProjectListItem;
  structure: StructureView;
  archaeology: ArchaeologyView | null;
  capabilities: CapabilityCoverage[];
  profile: ProfileView | null;
  similar: SimilarProject[];
  assets: AssetBrief[];
  insights: InsightBriefForProject[];
}

export interface StructureView {
  files: number;
  loc: number;
  loc_display: string;
  symbols: number;
  modules: number;
  languages: LanguageView[];
  has_git: boolean;
  has_readme: boolean;
  has_tests: boolean;
  has_license: boolean;
  has_docker: boolean;
}

export interface LanguageView {
  name: string;
  pct: number;
  loc: number;
  color: string;
}

export interface ArchaeologyView {
  commits: number;
  sessions: number | null;
  completeness: number | null;
  phase: string | null;
  salvage: string[];
  narrative: string;
  narrative_source: string;
  first_commit_at: string | null;
  last_commit_at: string | null;
  days_idle: number | null;
  branch: string | null;
}

export interface CapabilityCoverage {
  id: string;
  name: string;
  domain: string | null;
  confidence: number;
  evidence: string[];
}

export interface ProfileView {
  summary: string;
  purpose: string | null;
  phase: string | null;
  highlights: HighlightView[];
  generated_by: string;
  generated_at: string;
}

export interface HighlightView {
  title: string;
  desc: string;
  evidence_files: EvidenceFile[];
}

/**
 * 亮点引用的文件。
 *
 * 🔴 `exists` 是**防幻觉校验的结果**：后端会逐条比对 LLM 声称的文件是否真实存在。
 * `exists: false` 说明模型编造了这个文件，前端必须显示为"未验证"而非正常链接——
 * 让用户点一个不存在的路径，比不显示更糟。
 */
export interface EvidenceFile {
  path: string;
  absolute: string;
  exists: boolean;
}

export interface SimilarProject {
  id: string;
  name: string;
  similarity: number;
  basis: string[];
}

export interface AssetBrief {
  id: string;
  name: string;
  asset_type: string;
  type_label: string;
  source_path: string;
  reuse_score: number;
  tier: string;
  description: string;
}

export interface InsightBriefForProject {
  id: string;
  title: string;
  description: string;
  insight_type: string;
  type_label: string;
  badge: string;
  confidence: number;
}

/** 项目 AI 画像（service/profile.rs :: ProfileResponse） */
export interface ProfileResponse {
  project_id: string;
  project_name: string;
  profile: ProjectAiProfile | null;
  verification: VerificationReport | null;
  /** true = 本次重新生成了；false = 命中缓存 */
  regenerated: boolean;
  generated_by: string | null;
}

export interface ProjectAiProfile {
  summary: string;
  purpose: string | null;
  phase: string | null;
  highlights: ProjectHighlight[];
  archaeology: Archaeology | null;
  generated_by: string;
  generated_at: string;
}

export interface ProjectHighlight {
  title: string;
  desc: string;
  evidence_files: string[];
}

/** 项目考古报告（domain :: Archaeology）。 */
export interface Archaeology {
  /**
   * AI Coding Session 数。
   * 🔴 阶段三接入会话历史前恒为 0（不是 null）：
   * 前端不应把 0 渲染成"没有会话记录"这种确定性结论，
   * 而应识别为"该数据源尚未接入"。
   */
  sessions: number;
  commits: number;
  completeness: number | null;
  phase: string | null;
  /** 可打捞资产名，来自真实抽取结果（不是模型编的） */
  salvage: string[];
  narrative: string;
}

/**
 * 画像证据校验报告。
 *
 * 🔴 这是"不给用户看编造内容"这条产品红线的执行凭证：
 * `files_rejected` 里的每一项都是模型声称存在、但磁盘上找不到的文件。
 * 前端应把它显示出来（而不是悄悄丢弃），让用户知道 AI 的输出被核验过。
 */
export interface VerificationReport {
  files_claimed: number;
  files_verified: number;
  files_rejected: string[];
  highlights_claimed: number;
  highlights_kept: number;
  archaeology_grounded: boolean;
}

// ══════════════════════════════════════════════════════════════════
// 资产（service/assets.rs）
// ══════════════════════════════════════════════════════════════════

export interface AssetListPage {
  items: AssetListItem[];
  total: number;
  limit: number;
  offset: number;
  facets: AssetFacet[];
}

export interface AssetListItem {
  id: string;
  project_id: string;
  project_name: string;
  asset_type: string;
  type_label: string;
  name: string;
  description: string;
  source_path: string;
  confidence: number;
  reuse_score: number;
  tier: string;
  tier_label: string;
  tags: string[];
  created_at: string;
  evidence_files: number;
  created_relative: string;
  user_feedback: string | null;
}

export interface AssetFacet {
  value: string;
  label: string;
  count: number;
}

export interface AssetDetail {
  id: string;
  project_id: string;
  project_name: string;
  asset_type: string;
  type_label: string;
  name: string;
  description: string;
  /** 代码片段。抽取器未能取到内容时为 null。 */
  content: string | null;
  source_path: string;
  confidence: number;
  reuse_score: number;
  generality: number;
  stability: number;
  tier: string;
  tier_label: string;
  tags: string[];
  created_at: string;
  evidence: AssetEvidenceView;
  user_feedback: string | null;
  /** 其他项目里的同名/同功能资产（"你已经写过一份了"） */
  duplicates: DuplicateItem[];
}

export interface AssetEvidenceView {
  files: string[];
  commits: string[];
  used_by: string[];
  reasoning: string[];
  file_count: number;
  /**
   * 证据是否充分。
   * 🔴 产品红线：无证据的资产不入库、不展示。这个字段告诉前端
   * 该条目的证据强度是否达到"可放心复用"的门槛。
   */
  sufficient: boolean;
}

export interface DuplicateItem {
  id: string;
  name: string;
  project_id: string;
  project_name: string;
  source_path: string;
  reuse_score: number;
}

export interface TypeBreakdown {
  total: number;
  /** 受当前筛选影响的分布（chips 数字必须与列表条数对得上） */
  by_type: AssetFacet[];
  /** 全量类型（含计数为 0 的，否则勾掉后点不回来） */
  all_types: AssetFacet[];
}

// ══════════════════════════════════════════════════════════════════
// 检索（service/search.rs）
// ══════════════════════════════════════════════════════════════════

/**
 * 命中实体类型（domain :: HitKind）。
 *
 * 注意 `knowledge`/`experience`/`decision`/`idea` 是资产的子类型分类，
 * 当前生产代码主要产出前五种。
 */
export type HitKind =
  | "project"
  | "asset"
  | "capability"
  | "insight"
  | "opportunity"
  | "knowledge"
  | "experience"
  | "decision"
  | "idea";

export type SearchScope =
  | "all"
  | "projects"
  | "assets"
  | "capabilities"
  | "insights"
  | "knowledge";

export type SortBy = "relevance" | "reuse_score" | "recently_updated" | "confidence";

export interface SearchView {
  hits: HitView[];
  total: number;
  /** 归一化后的实际查询串，用于回显（用户输入的多余空白已被折叠） */
  query: string;
  /**
   * 是否降级到了 LIKE 子串匹配。
   * 🔴 必须显示给用户：子串匹配的召回质量低于 FTS，
   * 藏着不说会让用户以为"搜索结果就这么点"而不去调整关键词。
   */
  used_substring_fallback: boolean;
  took_ms: number;
  kind_counts: KindCount[];
  applied_filters: AppliedFilters;
  /** 空结果时的引导文案（区分"库是空的→去扫描"与"筛选太严→放宽"） */
  empty_hint: string | null;
}

export interface HitView {
  kind: HitKind;
  kind_label: string;
  id: string;
  title: string;
  subtitle: string;
  snippet: string;
  score: number;
  score_percent: number;
  sources: string[];
  source_labels: string[];
  reasons: string[];
  link: LinkView;
}

export interface LinkView {
  /** 目标页面 key，直接对应前端路由 */
  page: string;
  param: string | null;
}

export interface KindCount {
  kind: HitKind;
  label: string;
  count: number;
}

export interface AppliedFilters {
  scope: SearchScope;
  scope_label: string;
  sort: SortBy;
  sort_label: string;
  asset_type: string | null;
  project_status: string | null;
  language: string | null;
  project_id: string | null;
  min_reuse_score: number | null;
}

// ══════════════════════════════════════════════════════════════════
// 图谱（service/graph.rs）
// ══════════════════════════════════════════════════════════════════

export interface GraphView {
  nodes: GraphNode[];
  edges: GraphEdge[];
  stats: GraphStats;
  legend: LegendItem[];
  relation_legend: RelationLegendItem[];
  center_id: string | null;
  /** 空图时的解释（"图谱还是空的：请先完成一次扫描并生成洞察"） */
  empty_hint: string | null;
}

export interface GraphNode {
  id: string;
  entity_id: string;
  kind: string;
  kind_label: string;
  label: string;
  subtitle: string;
  /** 后端算好的色值，前端直接用（不要在两边各维护一份色板） */
  color: string;
  color_key: string;
  degree: number;
  weight: number;
  /** 建议半径（px），已按对数缩放并量化到 0.1 */
  size: number;
  is_center: boolean;
  link_page: string;
}

export interface GraphEdge {
  id: string;
  source: string;
  target: string;
  relation: string;
  relation_label: string;
  confidence: number;
  evidence_count: number;
  bidirectional: boolean;
  color: string;
}

export interface LegendItem {
  key: string;
  label: string;
  color: string;
  count: number;
}

export interface RelationLegendItem {
  relation: string;
  label: string;
  count: number;
  bidirectional: boolean;
}

/**
 * 图谱统计。
 *
 * 🔴 这些字段存在的唯一目的是**如实报告截断**。
 * 图谱必须限流（156 项目 + 数千资产全量渲染会卡死浏览器），
 * 但静默截断会让用户误以为"我的项目之间就这点关联"。
 * 前端应把 `summary` 显示出来。
 */
export interface GraphStats {
  node_count: number;
  edge_count: number;
  total_capabilities: number;
  total_relations: number;
  truncated: boolean;
  omitted_nodes: number;
  omitted_edges: number;
  /** 两端节点被截断而丢弃的边数（避免悬空边画到画布外） */
  dropped_dangling_edges: number;
  isolated_nodes: number;
  summary: string;
}

export interface NeighborhoodView {
  center: GraphNode;
  nodes: GraphNode[];
  edges: GraphEdge[];
  relation_counts: RelationLegendItem[];
  stats: GraphStats;
}

// ══════════════════════════════════════════════════════════════════
// 洞察与机会（service/insights.rs）
// ══════════════════════════════════════════════════════════════════

export interface InsightListPage {
  items: InsightItem[];
  total: number;
  limit: number;
  offset: number;
  unread: number;
  facets: TypeFacet[];
  adoption: AdoptionView;
}

export interface InsightItem {
  id: string;
  insight_type: string;
  type_label: string;
  title: string;
  description: string;
  confidence: number;
  confidence_percent: number;
  tags: string[];
  created_at: string;
  created_relative: string;
  /**
   * 价值徽章（"高价值"/"高潜力"/"建议查看"），由置信度分档。
   * 🔴 与首页同源（domain :: `Insight::badge()`），不是另一套逻辑。
   */
  badge: string;
  badge_key: string;
  /**
   * 用户处置状态（"待处理"/"已标记有用"/"已标记无用"/"已忽略"）。
   * 🔴 与 `badge` 是**两个独立维度**：价值判断 vs 我的处置。
   * 早期把两者压进一个字段，导致标了"有用"后价值徽章消失。
   */
  state: string;
  state_key: string;
  evidence_count: number;
  related_project_ids: string[];
  related_asset_ids: string[];
  user_feedback: string | null;
}

export interface TypeFacet {
  value: string;
  label: string;
  count: number;
}

/** 洞察采纳率（反馈回流效果） */
export interface AdoptionView {
  useful: number;
  rated: number;
  /** 尚无任何评分时为 null（此时显示"暂无"而非 0%） */
  rate: number | null;
  label: string;
}

export interface InsightDetail {
  item: InsightItem;
  evidence: InsightEvidenceView[];
  related_projects: RelatedProject[];
  related_assets: RelatedAsset[];
}

export interface InsightEvidenceView {
  kind: string;
  kind_label: string;
  label: string;
  target: string | null;
}

export interface RelatedProject {
  id: string;
  name: string;
  language: string;
  status: ProjectStatus;
  status_label: string;
}

export interface RelatedAsset {
  id: string;
  name: string;
  asset_type: string;
  type_label: string;
  source_path: string;
  reuse_score: number;
}

export interface OpportunityListPage {
  items: OpportunityItem[];
  total: number;
  limit: number;
  offset: number;
  facets: TypeFacet[];
  actionable_count: number;
  empty_hint: string | null;
}

export interface OpportunityItem {
  id: string;
  title: string;
  description: string;
  /** "为什么值得关注"——真实依据，比 description 更有说服力 */
  why: string;
  rating: number;
  /** 预渲染的星级串（"★★★☆☆"），前端不必自己算 */
  rating_stars: string;
  coverage_percent: number;
  coverage: number;
  required_capabilities: string[];
  missing_capabilities: string[];
  evidence: string[];
  status: string;
  status_label: string;
  created_at: string;
  created_relative: string;
  source_projects: RelatedProject[];
  actionable: boolean;
  has_analysis: boolean;
}

export interface OpportunityDetail {
  item: OpportunityItem;
  /** 未做深入分析时为 null（前端据此显示"展开分析"按钮） */
  analysis: OpportunityAnalysis | null;
}

export interface OpportunityAnalysis {
  opportunity_id: string;
  rationale: string;
  reusable: ReusableItem[];
  to_build: string[];
  mvp_suggestion: string;
  scaffold: string[];
}

/** 机会分析里「可直接复用」的资产条目（domain :: ReusableItem）。 */
export interface ReusableItem {
  asset_id: string;
  name: string;
  project_id: string;
  source_path: string;
  reuse_score: number;
  /** 迁移动作建议（"可直接复制"、"需抽象参数"） */
  migration_note: string;
}

export interface InsightsSummary {
  insight_total: number;
  insight_unread: number;
  opportunity_total: number;
  opportunity_actionable: number;
  adoption: AdoptionView;
  by_type: TypeFacet[];
}

// ══════════════════════════════════════════════════════════════════
// 任务（service/jobs.rs）
// ══════════════════════════════════════════════════════════════════

export type JobStatus = "queued" | "running" | "completed" | "failed" | "cancelled";

export interface JobListPage {
  items: JobView[];
  /** 当前活跃任务（侧栏进度条的数据源） */
  active: JobView[];
  /** 全部活跃任务的整体进度；无活跃任务时为 null */
  overall_progress: number | null;
  total: number;
}

export interface JobView {
  id: string;
  job_type: string;
  job_type_label: string;
  status: JobStatus;
  status_label: string;
  percent: number;
  progress: number;
  stage: string | null;
  processed: number | null;
  total: number | null;
  /** 预渲染的计数文案（"127/183 个文件"） */
  counter_text: string | null;
  error: string | null;
  created_at: string;
  updated_at: string;
  cancellable: boolean;
}

/** 任务提交回执 */
export interface SubmitResponse {
  job_id: string;
  job_type: string;
  job_type_label: string;
  /**
   * 面向用户的确认文案。
   * 🔴 链式扫描时它会预告后续动作（"已开始扫描 1 个目录，完成后将自动索引并生成洞察"），
   * 前端应原样显示——这是用户理解"为什么几分钟后洞察自己冒出来"的唯一线索。
   */
  message: string;
}

export interface ProgressSnapshot {
  current: ProgressEvent | null;
  subscribers: number;
}

/** SSE 推送的进度事件（jobs/progress.rs :: ProgressEvent） */
export interface ProgressEvent {
  job_id: string;
  job_type: string;
  status: JobStatus;
  progress: number;
  stage: string | null;
  processed: number | null;
  total: number | null;
  error: string | null;
}

export interface ActivityView {
  id: string;
  icon: string;
  title: string;
  detail: string;
  created_at: string;
  relative: string;
}

// ══════════════════════════════════════════════════════════════════
// 对话式分析师（service/analyst.rs + domain/analyst.rs）
// ══════════════════════════════════════════════════════════════════

export interface AnalystResponse {
  answer: AnalystAnswer;
  /** 实际喂给模型的检索结果条数 */
  context_hits: number;
  /** 检索到的候选总数 */
  context_total: number;
  /**
   * 被白名单机制拒绝的引用数。
   * 🔴 这是**防幻觉审计**：模型引用了检索结果之外的东西就会被丢弃并计数。
   * 非 0 时前端应提示用户"部分引用未通过证据校验"。
   */
  rejected_citations: number;
  used_substring_fallback: boolean;
  search_took_ms: number;
}

export interface AnalystAnswer {
  content: string;
  generated_by: AnswerSource;
  citations: Citation[];
  /** 由命中类型推导的后续问题（不是写死的通用问题） */
  followups: string[];
  took_ms: number;
}

/**
 * 回答来源。
 *
 * 🔴 Rust 侧是 `#[serde(rename_all="snake_case")]` 的**外部标签枚举**：
 * - `Deterministic` → 字符串 `"deterministic"`
 * - `Model(String)` → 对象 `{ "model": "qwen2.5:14b" }`
 *
 * 这个双形状是 serde 默认行为，用 `isModelSource()` 收窄，不要直接判断 truthy。
 */
export type AnswerSource = "deterministic" | { model: string };

/** 类型守卫：是否为模型生成（而非离线降级回答）。 */
export function isModelSource(s: AnswerSource): s is { model: string } {
  return typeof s === "object" && s !== null && "model" in s;
}

/** 取来源的展示名。 */
export function answerSourceLabel(s: AnswerSource): string {
  return isModelSource(s) ? s.model : "离线检索";
}

export type CitationKind =
  | "project"
  | "asset"
  | "capability"
  | "insight"
  | "opportunity"
  | "file";

export interface Citation {
  kind: CitationKind;
  label: string;
  link: LinkView;
  /**
   * 该引用支撑的具体论断。
   * 🔴 取自检索层的真实理由（基于分数算出），不是编的一句话。
   */
  supports: string | null;
}

export interface AnalystTurn {
  role: "user" | "assistant";
  content: string;
}

// ══════════════════════════════════════════════════════════════════
// 设置（service/settings.rs）
// ══════════════════════════════════════════════════════════════════

export interface SettingsView {
  llm: LlmView;
  scan: ScanView;
  appearance: AppearanceSettings;
  db: DbView;
}

export interface LlmView {
  cloud_provider: string;
  cloud_provider_label: string;
  cloud_base_url: string;
  /**
   * 已掩码的 key（如 `sk-****abcd`）。
   * 🔴 后端**永不**返回明文 key，这个字段只能用于显示。
   */
  cloud_api_key_masked: string;
  api_key_placeholder: string;
  cloud_model: string;
  local_backend: string;
  local_backend_label: string;
  local_base_url: string;
  local_model: string;
  route_fast: string;
  route_deep: string;
  /** Local-First 红线：敏感项目只走本地模型 */
  sensitive_local_only: boolean;
  embedding_local_only: boolean;
  /**
   * 是否已真正配置。
   * 🔴 判据是"有无真实产出"（审计记录/云端 key），不是"URL 格式对不对"——
   * 默认设置自带 Ollama 地址，只看格式会谎报"已配置模型"。
   */
  cloud_configured: boolean;
  local_configured: boolean;
  cloud_providers: ProviderOption[];
  local_backends: ProviderOption[];
}

export interface ProviderOption {
  value: string;
  label: string;
  default_base_url: string;
  preset_models: string[];
}

export interface ScanView {
  dirs: DirView[];
  watch_enabled: boolean;
  exclude_patterns: string[];
  /**
   * 🔴 字段名有误导性：它位于 `ScanSettings` 且名为 `level2`，
   * 但实际控制的是**是否允许调用大模型**（项目画像 + AI 分析师），与扫描无关。
   *
   * 关掉它不会减少任何抽取：资产/能力/关系/洞察都在 Level 1 与规则计算里完成，
   * 全部照跑。真正需要 LLM 的只有画像与分析师两处。
   *
   * UI 文案已按实际行为写成「AI 分析」（见 `pages/Settings.tsx`）。
   * 改名要动 schema + 三处端点 + 前端镜像，收益不抵成本，故保留字段名、只留此注释。
   */
  level2_enabled: boolean;
  max_depth: number;
  /** 目录配置问题（不存在/无权限等），非空时应在 UI 上警示 */
  problems: string[];
}

export interface DirView {
  path: string;
  enabled: boolean;
  added_at: string;
  last_scanned_at: string | null;
  project_count: number | null;
  /** 目录当前是否真实存在（被移动/删除后这里会变 false） */
  exists: boolean;
}

export interface DbView {
  path: string;
  size_display: string;
  size_bytes: number;
  schema_version: number;
  fts_available: boolean;
  tables: TableCount[];
}

export interface TableCount {
  table: string;
  rows: number;
}

export interface AppearanceSettings {
  theme: "dark" | "light";
  reduce_motion: boolean;
}

export interface TestConnectionView {
  ok: boolean;
  message: string;
  backend: string;
  model: string;
  route: string;
  route_label: string;
  /** 后端实际探测到的可用模型列表（帮用户填对模型名） */
  models: string[];
  latency_ms: number;
}

export interface AuditView {
  at: string;
  model: string;
  route: string;
  route_label: string;
  job_type: string;
  summary: string;
  project_id: string | null;
  /**
   * 模型调用成败。`null` = 本条不是模型调用（本地安全事件，如"取消敏感标记"）。
   *
   * 🔴 三态，不是布尔：`null` 时不得渲染任何成败标记。
   * 给"用户关闭了敏感项目仅本地约束"打一个绿色对勾，
   * 等于把一次安全降级说成"操作成功"。
   */
  ok: boolean | null;
  /** 失败原因（仅 ok === false 时有值）。不含代码原文与 API Key。 */
  error: string | null;
}

export interface ClearResult {
  /** 各表清理的行数 */
  cleared: Record<string, number>;
  /** 被刻意保留的数据说明（设置、用户反馈等不随派生数据清除） */
  preserved: string[];
}

/**
 * 导出配置的单个键值对（Rust `Vec<(String, String)>` 序列化成二元数组）。
 *
 * 🔴 用 `[string, string]` 而非 `{key, value}` 对象：
 * serde 把元组序列化成 JSON 数组。写成对象会读到 `undefined`。
 * 后端选元组数组是为了**保持顺序**，前端渲染时不得转成对象（对象会丢序）。
 */
export type ExportEntry = [string, string];

/**
 * `/api/health` 响应。
 *
 * 🔴 形状是 `{status, db_path, fts_available, stats}`，`stats` 是 `DbStats`
 * （见 crates/storage/src/schema.rs）。这个端点由 handler 直接用 `serde_json::json!`
 * 拼装，不经过 service DTO——改动后端时要同步这里。
 */
export interface HealthView {
  status: string;
  db_path: string;
  fts_available: boolean;
  stats: DbStats;
}

/** 数据库统计（storage :: DbStats）。各计数直接来自真实 `SELECT count(*)`。 */
export interface DbStats {
  size_bytes: number;
  projects: number;
  assets: number;
  capabilities: number;
  relations: number;
  insights: number;
  opportunities: number;
  jobs: number;
  activities: number;
  audit_entries: number;
}
