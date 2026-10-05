//! Spolia 混合检索层。
//!
//! 《技术设计书》§12 定的是**混合检索，而非纯向量**：
//! ```text
//! Query → FTS5 关键词召回 + LIKE 短查询回退 + 结构化过滤 → 合并重排 → 结果 + 排序理由
//! ```
//!
//! # 模块划分
//! | 模块 | 职责 | 是否有 IO |
//! |---|---|---|
//! | `engine` | 编排召回 → 过滤 → 评分 → 排序 → 分页 | 是（经 storage） |
//! | `ranking` | 相关性/质量/新鲜度加权与排序理由 | 否（纯函数） |
//! | `snippet` | 摘要片段生成与命中高亮 | 否（纯函数） |
//!
//! 把评分与摘要拆成纯函数模块，是为了让它们能被穷举单测——
//! 排序逻辑一旦只能靠"跑一次真实库看看"来验证，回归就无法被发现。
//!
//! # 三条产品红线
//! 1. **结果必须带排序理由**：用户不接受"因为 0.87 分所以排第一"。
//!    分数与理由由 `ranking::RankInput` 的同一段代码产出，不可能互相矛盾。
//! 2. **短中文查询不得静默返回空**：FTS5 trigram 要求 ≥3 字符，
//!    "视频""登录"这类 2 字高频查询必须走 LIKE 回退，
//!    并通过 `used_substring_fallback` 告知前端（否则用户以为功能不存在）。
//! 3. **结果顺序必须确定**：同分按 id 兜底排序。
//!    否则"刷新一次顺序就变"，用户无法建立对结果的信任，快照测试也会随机失败。
//!
//! # 依赖方向
//! 本 crate 依赖 `spolia-storage` 取数据，但**自身不出现任何 SQL**。
//! 换检索后端（例如《技术设计书》§25 提到的向量层）时，
//! 改动限于 storage 的 `fts.rs`，本 crate 与上层 API 都不受影响。

mod engine;
mod ranking;
mod snippet;

pub use engine::{query_terms, scope_includes, SearchEngine, NAME_BOOST};
pub use ranking::{
    asset_quality, capability_quality, explicit_sort_key, insight_quality, opportunity_quality,
    project_quality, recency_score, sort_hits, RankInput, SortFacts, NEUTRAL_RECENCY,
    RECENCY_WINDOW_DAYS, W_QUALITY, W_RECENCY, W_RELEVANCE,
};
pub use snippet::{highlight, plain, strip_marks, CONTEXT_CHARS, SNIPPET_CHARS};

#[cfg(test)]
mod tests {
    use super::*;
    use spolia_domain::{
        Asset, AssetType, Capability, CapabilityLayer, CodeStats, Evidence, Project, ProjectStatus,
        SearchQuery, SortBy,
    };
    use spolia_storage::Database;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-29T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("/tmp/{id}"),
            description: "视频处理流程".into(),
            language: "Python".into(),
            framework: "FastAPI".into(),
            created_at: None,
            updated_at: Some("2026-09-20".into()),
            last_commit_at: Some("2026-09-20".into()),
            status: ProjectStatus::Active,
            health_score: 80,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats {
                files: 10,
                loc: 3000,
                symbols: 12,
                modules: 3,
                languages: vec![],
            },
            scan: spolia_domain::ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn asset(id: &str, reuse: f64) -> Asset {
        Asset {
            id: id.into(),
            project_id: "p1".into(),
            asset_type: AssetType::Component,
            name: "视频组件".into(),
            description: "可复用的视频处理组件".into(),
            content: None,
            source_path: format!("src/{id}.py"),
            confidence: 0.9,
            reuse_score: reuse,
            generality: 0.7,
            stability: 0.6,
            tags: vec![],
            created_at: "2026-09-01".into(),
            evidence: Evidence {
                files: vec![format!("src/{id}.py")],
                ..Evidence::default()
            },
            user_feedback: None,
        }
    }

    fn cap(id: &str, count: u32) -> Capability {
        let mut c = Capability::new(id, "视频生成", CapabilityLayer::Domain, None, 0.9).unwrap();
        c.description = "从脚本生成视频".into();
        c.project_count = count;
        c
    }

    // ── 公开 API 可达性 ──────────────────────────────────────────

    #[test]
    fn public_api_is_accessible() {
        // 常量值断言（而非 `> 0` 这类恒真式）：误改常量时测试才会真的失败
        assert_eq!(W_RELEVANCE, 0.55);
        assert_eq!(W_QUALITY, 0.25);
        assert_eq!(W_RECENCY, 0.20);
        assert_eq!(RECENCY_WINDOW_DAYS, 30);
        assert_eq!(NEUTRAL_RECENCY, 0.5);
        assert_eq!(NAME_BOOST, 0.25);
        assert_eq!(SNIPPET_CHARS, 48);
        assert_eq!(CONTEXT_CHARS, 12);

        // 纯函数可直接调用
        assert_eq!(recency_score(Some(0)), 1.0);
        assert!(project_quality(&project("p1", "x")) > 0.0);
        assert_eq!(asset_quality(&asset("a1", 0.8)), 0.8);
        assert_eq!(capability_quality(&cap("c1", 5)), 1.0);
        assert_eq!(
            highlight("视频流程", &["视频".to_string()]),
            "<mark>视频</mark>流程"
        );
        assert_eq!(strip_marks("<mark>x</mark>"), "x");
        assert_eq!(plain("视频流程", &["视频".to_string()]), "视频流程");
        assert!(scope_includes(
            spolia_domain::SearchScope::All,
            spolia_domain::SearchScope::Assets
        ));
        assert_eq!(query_terms("a b").len(), 2);
        // 相关性排序不产出显式主键（由综合分决定）
        assert!(explicit_sort_key(
            SortBy::Relevance,
            &SortFacts::with_timeline(None, None, 0.0)
        )
        .is_none());

        // 排序辅助可用
        let mut items = vec![(0.1_f64, "b".to_string()), (0.9, "a".to_string())];
        sort_hits(&mut items, |i| i.0, |i| i.1.clone());
        assert_eq!(items[0].0, 0.9);
    }

    #[test]
    fn engine_is_constructible_and_stateless() {
        let a = SearchEngine::new();
        // 显式类型标注的 Default::default() 覆盖 derive(Default) 路径；
        // 无状态的单元结构体里它与 new() 等价，这里验证两种构造都可用。
        let b: SearchEngine = Default::default();
        let d = Database::in_memory().unwrap();
        let q = SearchQuery {
            q: "视频".into(),
            ..Default::default()
        };
        assert!(a.search(&d, &q, now()).unwrap().is_empty());
        assert!(b.search(&d, &q, now()).unwrap().is_empty());
    }

    /// 端到端：三类实体混排，且每条都有理由、片段、跳转。
    #[test]
    fn end_to_end_mixed_search() {
        let d = Database::in_memory().unwrap();
        d.projects().upsert(&project("p1", "视频管线")).unwrap();
        d.assets().upsert(&asset("a1", 0.9)).unwrap();
        d.capabilities().upsert(&cap("c1", 3)).unwrap();

        let r = SearchEngine::new()
            .search(&d, &SearchQuery::default(), now())
            .unwrap();
        // 默认查询为空串 → 浏览模式，应能列出三类实体
        assert_eq!(r.hits.len(), 3, "浏览模式应覆盖三类实体");
        let kinds: Vec<_> = r.hits.iter().map(|h| h.kind).collect();
        assert!(kinds.contains(&spolia_domain::HitKind::Project));
        assert!(kinds.contains(&spolia_domain::HitKind::Asset));
        assert!(kinds.contains(&spolia_domain::HitKind::Capability));
        for h in &r.hits {
            assert!(!h.reasons.is_empty(), "{} 缺少理由", h.id);
            assert!(!h.snippet.is_empty(), "{} 缺少片段", h.id);
            assert!(!h.sources.is_empty(), "{} 缺少来源", h.id);
        }
    }
}
