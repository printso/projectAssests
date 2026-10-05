//! projectAssests 洞察与机会引擎（《技术设计书》§7 的 Engine 3：AI Reasoning）。
//!
//! # 这是产品与普通"AI 项目搜索工具"拉开距离的地方
//! 搜索回答"找到我写过的 OAuth"；洞察告诉你"你在 5 个项目中重复实现了 OAuth"。
//! 后者才是增量价值（《产品介绍总纲》§2.2 第 3 条：从搜索 → 发现）。
//!
//! # 模块划分
//! | 模块 | 职责 |
//! |---|---|
//! | `input` | 分析输入快照（引擎是纯函数，不直接依赖 storage）|
//! | `detectors` | 5 个洞察检测器 + 反馈合并 + 采纳率统计 |
//! | `opportunities` | 机会卡片生成与"深入分析"落地方案 |
//!
//! # 🔴 三条不可动摇的纪律
//! 1. **每条结论都带 Evidence**：无证据的洞察在 `Insight::validate()` 就被拒绝，
//!    检测器内部也先过滤。产品红线是"无证据的结论一律不展示"。
//! 2. **确定性优先**：全部检测器是纯函数，不调 LLM。
//!    同一份数据两次运行结果逐字节一致（有测试守护），因此可回归、可审计。
//! 3. **区分事实与推断**：重复实现是可验证事实（置信度可达 0.96），
//!    遗忘资产是推断（上限 0.85，措辞必须含"推测"）。
//!    混为一谈会让用户对整份报告失去信任。

mod detectors;
mod input;
mod opportunities;

pub use detectors::{
    adoption_rate, detect_all, detect_combination_hints, detect_duplicate_capabilities,
    detect_forgotten_assets, detect_reusable_components, detect_tech_directions, merge_with_feedback,
    unread, DetectorConfig, DetectionOutcome,
};
pub use input::{idle_days, AnalysisInput};
pub use opportunities::{
    OpportunityConfig, OpportunityEngine, OpportunityOutcome, DOMAIN_COMPLETENESS,
};

#[cfg(test)]
mod tests {
    use super::*;
    use input::tests::{asset, cap, implements, project};
    use projectassests_domain::{
        AssetType, CapabilityLayer, EntityKind, Evidence, EvidenceItem, EvidenceKind,
        ProjectStatus, RelationType, UserFeedback,
    };

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-29T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// 一个尽量真实的综合场景：4 个项目、跨领域能力、高复用资产、一个陈旧项目。
    fn realistic_input() -> AnalysisInput {
        let stale_date = (now() - chrono::Duration::days(500)).format("%Y-%m-%d").to_string();
        let recent = Some("2026-09-01");

        let projects = vec![
            project("p_video", ProjectStatus::Active, recent),
            project("p_image", ProjectStatus::Active, recent),
            project("p_agent", ProjectStatus::Active, recent),
            project("p_old", ProjectStatus::Paused, Some(&stale_date)),
        ];

        let capabilities = vec![
            cap("cap_domain_ai", "AI", CapabilityLayer::Domain, None),
            cap("cap_domain_infrastructure", "基础设施", CapabilityLayer::Domain, None),
            cap("c_video", "Video Generation", CapabilityLayer::Capability, Some("cap_domain_ai")),
            cap("c_image", "Image Generation", CapabilityLayer::Capability, Some("cap_domain_ai")),
            cap("c_rag", "RAG", CapabilityLayer::Capability, Some("cap_domain_ai")),
            cap("c_queue", "Task Queue", CapabilityLayer::Capability, Some("cap_domain_infrastructure")),
        ];

        // p_video 与 p_old 都实现了 Task Queue → 重复实现
        let relations = vec![
            implements("p_video", "c_video"),
            implements("p_image", "c_image"),
            implements("p_agent", "c_rag"),
            implements("p_video", "c_queue"),
            implements("p_old", "c_queue"),
        ];

        let mut old_gem = asset("a_gem", "p_old", "TaskQueueRunner", AssetType::Component, 0.88);
        old_gem.evidence = Evidence {
            files: vec!["p_old/src/queue.py".into()],
            used_by: vec!["submit()".into()],
            ..Default::default()
        };

        AnalysisInput {
            projects,
            capabilities,
            relations,
            now: now(),
            assets: vec![
                asset("a_pipe", "p_video", "VideoPipeline", AssetType::Component, 0.91),
                old_gem,
                asset("a_trivial", "p_image", "get", AssetType::Code, 0.2),
            ],
        }
    }

    /// 端到端：真实场景下应产出多类洞察，且全部通过产品红线校验。
    #[test]
    fn end_to_end_produces_multiple_insight_types() {
        let input = realistic_input();
        let out = detect_all(&input, &DetectorConfig::default());

        assert!(!out.insights.is_empty());
        let types: Vec<_> = out.insights.iter().map(|i| i.insight_type).collect();
        assert!(types.contains(&projectassests_domain::InsightType::DuplicateCapability), "应有重复能力洞察: {types:?}");
        assert!(types.contains(&projectassests_domain::InsightType::ReusableComponent), "应有高复用洞察: {types:?}");
        assert!(types.contains(&projectassests_domain::InsightType::ForgottenAsset), "应有遗忘资产洞察: {types:?}");

        for i in &out.insights {
            assert!(i.validate().is_ok(), "{} 未通过校验", i.title);
            assert!(!i.evidence.is_empty());
            assert!(i.confidence >= projectassests_domain::CONFIDENCE_THRESHOLD);
            // 事实类结论置信度应高于推断类
            if i.insight_type == projectassests_domain::InsightType::ForgottenAsset {
                assert!(i.confidence <= 0.85, "推断类洞察置信度上限 0.85: {}", i.confidence);
                assert!(i.description.contains("推测"), "推断必须标明: {}", i.description);
            }
        }
    }

    /// 重复能力洞察必须指向真实的两个项目与真实文件。
    #[test]
    fn duplicate_insight_points_at_real_evidence() {
        let input = realistic_input();
        let out = detect_duplicate_capabilities(&input, &DetectorConfig::default());
        let dup = out
            .insights
            .iter()
            .find(|i| i.insight_type == projectassests_domain::InsightType::DuplicateCapability)
            .expect("应发现 Task Queue 重复");

        assert!(dup.title.contains("Task Queue"));
        assert_eq!(dup.related_project_ids.len(), 2);
        assert!(dup.related_project_ids.contains(&"p_video".to_string()));
        assert!(dup.related_project_ids.contains(&"p_old".to_string()));
        // 证据含项目与文件两类
        assert!(dup.evidence.iter().any(|e| e.kind == EvidenceKind::Project));
        assert!(dup.evidence.iter().any(|e| e.kind == EvidenceKind::File));
        // 文件证据必须是真实路径
        let file = dup.evidence.iter().find(|e| e.kind == EvidenceKind::File).unwrap();
        assert!(file.label.contains("queue") || file.label.contains("TaskQueue"), "实际: {}", file.label);
        assert!(file.target.as_deref().unwrap().contains("p_"));
    }

    /// 机会引擎与洞察引擎共用同一份输入，产出必须互相印证。
    #[test]
    fn opportunity_engine_consumes_same_input() {
        let input = realistic_input();
        let out = OpportunityEngine::new().generate(&input, &OpportunityConfig::default());
        assert!(!out.opportunities.is_empty(), "AI 领域 3 项目 3 能力应产出机会");

        let o = &out.opportunities[0];
        assert!((1..=5).contains(&o.rating));
        assert!((0.0..=1.0).contains(&o.coverage));
        assert!(!o.source_project_ids.is_empty());
        assert!(!o.evidence.is_empty());

        // 缺失能力必须来自参照表
        let reference = DOMAIN_COMPLETENESS.iter().find(|(k, _)| *k == "ai").unwrap().1;
        for m in &o.missing_capabilities {
            assert!(reference.contains(&m.as_str()), "「{m}」不在参照表");
        }
    }

    /// 洞察与机会的 id 必须稳定（增量更新时反馈才能对上号）。
    #[test]
    fn ids_are_stable_across_runs() {
        let input = realistic_input();
        let cfg = DetectorConfig::default();
        let a = detect_all(&input, &cfg);
        let b = detect_all(&input, &cfg);
        let ids_a: Vec<&str> = a.insights.iter().map(|i| i.id.as_str()).collect();
        let ids_b: Vec<&str> = b.insights.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids_a, ids_b);

        let ocfg = OpportunityConfig::default();
        let oa = OpportunityEngine::new().generate(&input, &ocfg);
        let ob = OpportunityEngine::new().generate(&input, &ocfg);
        assert_eq!(
            oa.opportunities.iter().map(|o| o.id.clone()).collect::<Vec<_>>(),
            ob.opportunities.iter().map(|o| o.id.clone()).collect::<Vec<_>>()
        );
    }

    /// 全链路幂等：两次运行的序列化结果逐字节一致。
    #[test]
    fn full_pipeline_is_deterministic() {
        let input = realistic_input();
        let cfg = DetectorConfig::default();
        let x = serde_json::to_string(&detect_all(&input, &cfg).insights).unwrap();
        let y = serde_json::to_string(&detect_all(&input, &cfg).insights).unwrap();
        assert_eq!(x, y);
    }

    /// 反馈回流闭环：标记有用 → 重新分析 → 反馈保留 → 采纳率可算。
    #[test]
    fn feedback_loop_survives_reanalysis() {
        let input = realistic_input();
        let cfg = DetectorConfig::default();

        let round1 = detect_all(&input, &cfg).insights;
        let target_id = round1[0].id.clone();

        // 用户标记第一条为有用
        let mut with_feedback = round1.clone();
        with_feedback[0].user_feedback = Some(UserFeedback::Useful);
        assert_eq!(adoption_rate(&with_feedback), Some(1.0));

        // 重新分析后合并
        let round2 = detect_all(&input, &cfg).insights;
        let merged = merge_with_feedback(round2, &with_feedback);
        let kept = merged.iter().find(|i| i.id == target_id).unwrap();
        assert_eq!(kept.user_feedback, Some(UserFeedback::Useful), "反馈必须跨轮次保留");
        assert_eq!(adoption_rate(&merged), Some(1.0));

        // 未读的只剩没被处理过的
        assert!(unread(&merged).len() < merged.len());
    }

    /// 用户标记"无用"后，该资产不该再被推荐（避免重复骚扰）。
    #[test]
    fn useless_feedback_suppresses_recommendation() {
        let mut input = realistic_input();
        for a in &mut input.assets {
            if a.name == "VideoPipeline" {
                a.user_feedback = Some(UserFeedback::Useless);
            }
        }
        let out = detect_reusable_components(&input, &DetectorConfig::default());
        assert!(
            !out.insights.iter().any(|i| i.title.contains("VideoPipeline")),
            "已否决的资产不应再推荐"
        );
    }

    /// 空库必须安全：不 panic、不编造。
    #[test]
    fn empty_database_is_safe() {
        let input = AnalysisInput { now: now(), ..Default::default() };
        assert!(detect_all(&input, &DetectorConfig::default()).insights.is_empty());
        assert!(OpportunityEngine::new()
            .generate(&input, &OpportunityConfig::default())
            .opportunities
            .is_empty());
        assert_eq!(adoption_rate(&[]), None);
    }

    /// 数据不足时诚实返回空，而不是降低标准凑数。
    #[test]
    fn insufficient_data_yields_nothing_not_noise() {
        let input = AnalysisInput {
            projects: vec![project("p1", ProjectStatus::Active, Some("2026-09-01"))],
            assets: vec![asset("a1", "p1", "Small", AssetType::Code, 0.5)],
            capabilities: vec![cap("d_ai", "AI", CapabilityLayer::Domain, None)],
            relations: vec![],
            now: now(),
        };
        let out = detect_all(&input, &DetectorConfig::default());
        // 单个项目、低分资产 → 无任何洞察
        assert!(out.insights.is_empty(), "不该凑数: {:?}", out.insights.iter().map(|i| &i.title).collect::<Vec<_>>());
    }

    #[test]
    fn public_api_is_accessible() {
        let c = DetectorConfig::default();
        assert!(c.max_per_detector > 0);
        let o = OpportunityConfig::default();
        assert!(o.max_opportunities > 0);
        assert!(!DOMAIN_COMPLETENESS.is_empty());
        // 引擎无状态：能构造即为可用（为单元结构体实现 PartialEq 只是噪音）
        let engine = OpportunityEngine::new();
        assert!(engine.generate(&AnalysisInput::default(), &o).opportunities.is_empty());
        // parse_date / days_since 的行为由 domain 层测试覆盖，此处不重复断言
        let out = DetectionOutcome::default();
        assert!(out.insights.is_empty() && out.rejected == 0);
        let oo = OpportunityOutcome::default();
        assert!(oo.opportunities.is_empty() && oo.filtered == 0);
        assert!(AnalysisInput::default().project_count() == 0);
    }

    /// 关系构造辅助本身的正确性（fixture 可信度前提）。
    #[test]
    fn fixtures_are_well_formed() {
        let r = implements("p1", "c1");
        assert_eq!(r.relation_type, RelationType::Implements);
        assert_eq!(r.source_type, EntityKind::Project);
        assert_eq!(r.target_type, EntityKind::Capability);
        assert_eq!(r.source_id, "p1");

        let ev = EvidenceItem {
            kind: EvidenceKind::File,
            label: "a.py".into(),
            target: Some("p1:a.py".into()),
        };
        assert_eq!(ev.kind, EvidenceKind::File);
    }
}
