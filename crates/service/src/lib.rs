//! projectAssests 应用服务层。
//!
//! # 这一层为什么必须存在
//! 产品同时有两种前端形态：浏览器（开发调试、开源贡献者验收）与
//! Tauri 桌面壳（最终形态）。若业务逻辑写在 axum handler 里，
//! Tauri IPC 命令就得**再实现一遍**——两份实现必然漂移，
//! 典型症状是"网页上正常，桌面端行为不一致"，且极难排查。
//!
//! 因此：
//! ```text
//! React UI ──HTTP──► axum adapter ─┐
//!                                    ├──► projectassests-service ──► 各引擎
//! React UI ──invoke─► Tauri IPC ────┘
//! ```
//! 适配器只做三件事：解析参数、调用 service、把 `Result` 映射成传输格式。
//! **一行业务逻辑都不许写在适配器里。**
//!
//! # 分层职责
//! | 层 | 职责 | 禁止 |
//! |---|---|---|
//! | adapter（server/tauri） | 协议解析、状态码、序列化 | 业务判断、SQL |
//! | **service（本 crate）** | 用例编排、DTO 组装、跨引擎组合 | 直接写 SQL、感知 HTTP |
//! | engine（scanner/asset/…） | 单一领域的算法 | 跨领域调用、IO 编排 |
//! | storage | SQL、事务、行映射 | 业务规则 |
//!
//! # 两条贯穿纪律
//! 1. **只给真实数据**：所有计数、列表、画像都来自数据库或实时扫描。
//!    数据不足时返回明确的"空状态 + 下一步指引"，绝不用模板文案填充。
//! 2. **LLM 是能力而非装饰**：画像、对话、洞察增强都以 LLM 为主路径，
//!    确定性回退只是**降级**，且必须在返回里如实标注（见 `AnswerSource`）。

pub mod analyst;
pub mod assets;
pub mod context;
pub mod fs;
pub mod graph;
pub mod insights;
pub mod jobs;
pub mod overview;
pub mod profile;
pub mod projects;
pub mod search;
pub mod settings;

pub use context::{ServiceContext, ServiceError};
// 目录浏览：`list_dir` 与 settings 的 `add_dir` 等用例同名风险低，但仍起别名保持风格统一。
pub use fs::{
    list_dir as list_fs_dir, FsEntry, FsListView, MAX_ENTRIES as MAX_FS_ENTRIES,
};
pub use assets::{
    detail as asset_detail, list as list_assets, set_feedback as asset_feedback,
    type_breakdown as asset_type_breakdown, AssetDetail, AssetFacet, AssetListItem, AssetListPage,
    AssetListQuery, DuplicateItem, EvidenceView, FeedbackRequest, TypeBreakdown,
    MAX_PAGE_SIZE as MAX_ASSET_PAGE_SIZE,
};
// 🔴 函数一律用 `as` 起模块前缀别名：assets / projects / jobs 三个模块
// 都有 `list`、`detail`、`get` 这类同名用例。若直接重导出，
// 调用方 `use projectassests_service::*` 会撞上 E0652（ambiguous re-import），
// 而报错信息只说"名字有歧义"，不告诉你是哪两个模块。
pub use jobs::{
    active as active_jobs, activities as recent_activities, cancel as cancel_job,
    generate_insights, get as job_detail, index as index_project_job, list as list_jobs,
    progress as job_progress, scan as scan_now, subscribe as subscribe_progress,
    ActivityView, IndexRequest, JobListPage, JobView, ProgressSnapshot, ScanRequest,
    SubmitResponse, MAX_LIST_LIMIT as MAX_JOB_LIST_LIMIT,
};
// `search::search` 与模块同名，重导出必须起别名：
// `pub use search::search` 会让 `projectassests_service::search` 既指模块又指函数，
// 调用方写 `search::search(...)` 时编译器无法判断哪个在前。
pub use search::{
    search as run_search, AppliedFilters, HitView, KindCount, LinkView, SearchRequest, SearchView,
    MAX_QUERY_CHARS, MAX_SEARCH_LIMIT,
};
// 洞察与机会：`list`/`detail`/`set_feedback`/`summary` 与其他模块同名，一律起别名。
// 机会相关函数统一加 `opportunity_` 前缀，避免 `list`/`detail` 与洞察撞名。
// 图谱：`graph` 既是模块名又是主函数名，同样必须起别名（理由见 search）。
pub use graph::{
    graph as load_graph, neighborhood as load_neighborhood, GraphEdge, GraphNode, GraphRequest,
    GraphStats, GraphView, LegendItem, NeighborhoodView, RelationLegendItem, MAX_EDGES, MAX_NODES,
};
pub use insights::{
    adoption, detail as insight_detail, dismiss_all as dismiss_all_opportunities,
    list as list_insights, list_opportunities, opportunity_detail,
    set_feedback as insight_feedback, set_status as set_opportunity_status,
    summary as insights_summary, AdoptionView, EvidenceView as InsightEvidenceView,
    FeedbackRequest as InsightFeedbackRequest, InsightDetail, InsightItem, InsightListPage,
    InsightListQuery, InsightsSummary, OpportunityDetail, OpportunityItem, OpportunityListPage,
    OpportunityListQuery, RelatedAsset, RelatedProject, StatusRequest, TypeFacet,
    MAX_LIST_LIMIT as MAX_INSIGHT_LIST_LIMIT,
};
pub use overview::{
    insight_type_breakdown, load as load_overview, opportunity_status_breakdown, Overview,
};
// `parse_llm_json` 是 pub(crate)：它返回的 `RawProfile` 是内部结构，
// 导出会泄漏实现细节（且触发 private-in-public 警告）。模块内测试可直接访问。
pub use profile::{
    build_user_prompt, generate as generate_profile, get_cached as get_cached_profile,
    ProfileRequest, ProfileResponse, ProjectFacts, VerificationReport, SYSTEM_PROMPT,
};
pub use analyst::{
    ask as ask_analyst, build_candidates, build_user_prompt as build_analyst_prompt,
    extract_citation_ids, AnalystRequest, AnalystResponse, Candidate, MAX_CONTEXT_HITS,
};
pub use projects::{
    detail as project_detail, format_loc, list as list_projects, reindex as reindex_project,
    remove as remove_project, set_description, set_sensitive, ArchaeologyView, AssetBrief,
    CapabilityCoverage, DescriptionRequest, EvidenceFile, HighlightView,
    InsightBrief as ProjectInsightBrief, LanguageView, NarrativeSource, ProfileView, ProjectDetail,
    ProjectFacets, ProjectListItem, ProjectListPage, ProjectListQuery, SensitiveRequest,
    SimilarProject, StructureView, FacetItem, DETAIL_ASSETS_LIMIT, DETAIL_INSIGHTS_LIMIT,
    MAX_PAGE_SIZE, MIN_SIMILARITY,
};
pub use settings::{
    add_dir, clear_derived_data, export_config, load as load_settings, recent_audit,
    remove_dir, test_connection, toggle_dir, update as update_settings, update_appearance,
    update_llm, update_scan, AppearanceUpdate, AuditView, ClearResult, DirRequest, DirToggleRequest,
    DirView, DbView, LlmUpdate, LlmView, ProviderOption, ScanSettingsUpdate, ScanView,
    SettingsUpdate, SettingsView, TableCount, TestConnectionRequest, TestConnectionView,
};

