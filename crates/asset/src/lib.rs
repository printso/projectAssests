//! projectAssests 资产引擎。
//!
//! 职责：把扫描器产出的真实文件，转化为带证据链的**资产**、**能力**与**关系**。
//! 这是《技术设计书》§7 的 Engine 2（Asset Intelligence）与 Engine 3 的确定性部分。
//!
//! # 模块划分
//! | 模块 | 职责 |
//! |---|---|
//! | `symbols` | 从真实源码抽取符号（函数/类/组件/端点），带行号与签名 |
//! | `scoring` | 确定性计算 reuse_score / generality / stability / confidence |
//! | `capability` | 从依赖/框架/符号名推断三层能力（Domain→Capability→Implementation）|
//! | `builder` | 把符号+评分+能力装配为可入库的 Asset / Capability / Relation |
//!
//! # 三条纪律
//! 1. **给证据，不给玄学**：每个分数都产出人类可读的 `reasoning`，
//!    写入 `Evidence.reasoning`，让用户能追溯"为什么认为它可复用"。
//! 2. **确定性优先**：评分全部由规则计算，不调用 LLM，因此可复现、可单测。
//! 3. **纯装配层不读时钟**：`created_at` 由调用方注入（见 `ProjectInput`），
//!    保证"同输入 → 同输出"，这是增量扫描与测试可比对的前提。

mod builder;
mod capability;
mod scoring;
mod symbols;

pub use builder::{
    build_duplicate_relations, AssetBuilder, AssetRef, Extraction, ProjectInput,
};
pub use capability::{
    domain_capabilities, CapabilityExtraction, CapabilityExtractor, CapabilitySignals, DOMAINS,
};
pub use scoring::{Score, ScoreWeights, Scorer, DEFAULT_THRESHOLD, MIN_SUBSTANTIAL_BODY_LINES};
pub use symbols::{HeuristicExtractor, Symbol, SymbolExtractor, SymbolKind};

#[cfg(test)]
mod tests {
    use super::*;

    /// 跨模块集成：抽取 → 评分 的完整链路。
    ///
    /// 两个模块各自有单测，但"抽取结果能否被评分器正确消费"这条接缝
    /// 只有集成测试能覆盖（例如 body_lines 是否真的被算出来）。
    #[test]
    fn extract_then_score_pipeline() {
        let src = r#"
"""视频服务模块"""


class VideoPipeline:
    """视频生成管道：分镜 → 生成 → 合成"""

    def render(self, clip: Clip, style: str) -> bytes:
        """渲染单个片段"""
        frames = self._prepare(clip)
        return self._encode(frames, style)

    def _prepare(self, clip):
        return clip.frames


def debug_tmp_2024():
    pass
"#;
        let syms = HeuristicExtractor.extract("Python", "services/video.py", src);
        assert!(!syms.is_empty());

        let scorer = Scorer::new();
        let pipeline = syms.iter().find(|s| s.name == "VideoPipeline").unwrap();
        let score = scorer.score_symbol(pipeline, None);
        assert!(score.worth_tracking, "有文档、有体量的类应入库");
        assert!(score.reuse_score > 0.5);
        assert!(!score.reasoning.is_empty());

        let render = syms.iter().find(|s| s.name == "render").unwrap();
        assert_eq!(render.kind, SymbolKind::Method);
        let rs = scorer.score_symbol(render, None);
        assert!(rs.reuse_score > 0.0);

        // 一次性代码必须被判死
        let disposable = syms.iter().find(|s| s.name == "debug_tmp_2024").unwrap();
        let ds = scorer.score_symbol(disposable, None);
        assert!(!ds.worth_tracking);
        assert_eq!(ds.reuse_score, 0.0);
    }

    /// 项目专有名词在完整链路中也要生效。
    #[test]
    fn project_hint_flows_through_pipeline() {
        let src = "class YingtechRenderer:\n    \"\"\"渲染\"\"\"\n    pass\n";
        let syms = HeuristicExtractor.extract("Python", "a.py", src);
        let scorer = Scorer::new();
        let with_hint = scorer.score_symbol(&syms[0], Some("yingtech"));
        let without = scorer.score_symbol(&syms[0], None);
        assert!(with_hint.generality < without.generality);
    }

    #[test]
    fn public_api_is_accessible() {
        // 对常量断言确切值而非范围：`> 0.0 && < 1.0` 这类判断恒真，无守护意义。
        // 改常量时测试必须失败，迫使改动者确认门槛与体量下限的影响面。
        assert_eq!(DEFAULT_THRESHOLD, 0.42);
        assert_eq!(MIN_SUBSTANTIAL_BODY_LINES, 3);
        assert!(ScoreWeights::default().is_valid());
        assert_eq!(HeuristicExtractor.name(), "heuristic-v1");
        assert_eq!(SymbolKind::Component.label_zh(), "组件");
        assert_eq!(DOMAINS.len(), 5);
        assert!(!domain_capabilities().is_empty());
    }

    /// 三层链路集成：真实源码 → 符号 → 评分 + 能力。
    ///
    /// 三个模块各自有单测，但"能力抽取能否消费符号名"这条接缝只有集成测试能覆盖。
    #[test]
    fn symbols_feed_capability_extraction() {
        let src = r#"
"""视频服务"""


class VideoGenerationPipeline:
    """文生视频管道"""

    def render_clip(self, prompt: str) -> bytes:
        frames = self._diffuse(prompt)
        return self._encode(frames)
"#;
        let syms = HeuristicExtractor.extract("Python", "services/video.py", src);
        let scorer = Scorer::new();
        let scores: Vec<_> = syms.iter().map(|s| scorer.score_symbol(s, None)).collect();

        // 符号名喂给能力抽取器
        let extractor = CapabilityExtractor::new();
        let caps = extractor.extract(
            &CapabilitySignals {
                symbol_names: syms.iter().map(|s| s.name.clone()).collect(),
                language: Some("Python".into()),
                dependencies: vec!["diffusers".into(), "langchain".into()],
                ..Default::default()
            },
            0.5,
        );

        let names: Vec<&str> = caps.capabilities.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Video Generation"), "应从符号名 VideoGenerationPipeline 命中，实际 {names:?}");
        assert!(names.contains(&"Image Generation"), "diffusers 应命中");
        assert!(names.contains(&"RAG"), "langchain 应命中");

        // 能力都挂在合法 Domain 下，且有 evidence
        for c in &caps.capabilities {
            assert!(c.parent_id.as_deref().unwrap_or("").starts_with("cap_domain_"));
            assert!(caps.evidence.contains_key(&c.id), "能力 {} 缺 evidence", c.name);
        }

        // 评分链路仍正常
        assert!(scores.iter().any(|s| s.worth_tracking));
    }
}
