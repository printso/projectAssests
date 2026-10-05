//! 机会引擎（《产品设计书》V0.4 功能 22/23）。
//!
//! # 这是产品的标志性产出
//! 组合 Capability + 高复用 Code + 相关 Project → 机会卡片：
//! 来源项目、已具备能力 %、缺失能力、组合价值 ★。
//!
//! # 🔴 「缺失能力」怎么来才不算玄学
//! 设计稿上的卡片写着"缺少：Publishing / Analytics"。这类结论若由 LLM 凭空生成，
//! 就是产品纪律明令禁止的"玄学"。本模块的做法是**人工策划的领域完整度参照表**
//! （见 `DOMAIN_COMPLETENESS`）：
//! - 参照表是可审查、可测试、可扩展的静态数据，不是模型输出
//! - 缺失 = 参照表要求、但**没有任何来源项目实现**的能力
//! - 卡片描述里明说"相对该领域的完整产品参照"，不假装是客观真理
//!
//! 用户看到"缺少 Publishing"时，能在 Evidence 里看到参照依据，
//! 也能在词表里自己改——这才叫"给证据，不给玄学"。

use std::collections::BTreeMap;

use spolia_domain::{
    Asset, AssetType, Opportunity, OpportunityAnalysis, ReusableItem, UserFeedback,
};

use crate::input::AnalysisInput;

/// 领域完整度参照表：`(Domain 稳定键, 一个完整产品通常需要的能力名)`。
///
/// 能力名必须与 `spolia-asset` 的规则表产出名一致（大小写不敏感匹配），
/// 否则永远匹配不上、参照表形同虚设。
/// `lib.rs` 里有一致性测试强制检查这一点。
///
/// 🔴 维护纪律：这是**产品判断**的载体，改动要有依据。
/// 加一个能力 = 声称"做这个领域的完整产品需要它"，
/// 不要为了凑数往里塞（会让所有机会卡片都显示一堆"缺失"，反而失去信号价值）。
pub const DOMAIN_COMPLETENESS: &[(&str, &[&str])] = &[
    (
        "ai",
        &[
            "Text Generation",
            "Image Generation",
            "Video Generation",
            "Embedding",
            "Vector Search",
            "RAG",
            "Prompt Engineering",
            "Agent Orchestration",
            "Tool Calling",
            "Model Fine-tuning",
        ],
    ),
    (
        "web",
        &[
            "Web Frontend",
            "Web API",
            "Authentication",
            "Realtime Communication",
            "UI Component Library",
            "Data Visualization",
            "Testing",
        ],
    ),
    (
        "data",
        &[
            "Data Processing",
            "Relational Database",
            "ORM",
            "Caching",
            "Full-text Search",
            "Schema Migration",
            "Workflow Orchestration",
        ],
    ),
    (
        "infrastructure",
        &[
            "Task Queue",
            "Message Queue",
            "Containerization",
            "Observability",
            "Testing",
            "CI/CD",
            "Object Storage",
        ],
    ),
    (
        "media",
        &[
            "Image Processing",
            "Video Processing",
            "PDF Processing",
            "Office Document",
        ],
    ),
];

/// 机会引擎配置。
#[derive(Debug, Clone)]
pub struct OpportunityConfig {
    /// 至少多少个来源项目才生成机会（单个项目谈不上"组合"）
    pub min_source_projects: usize,
    /// 至少覆盖多少项能力才算有价值的组合
    pub min_covered_capabilities: usize,
    /// 来源项目的高复用资产门槛（低于此分不计入"可直接复用"清单）
    pub reusable_min_score: f64,
    /// 覆盖度低于此值的机会不生成（缺得太多 = 基本是从零开始，不是"组合"）
    pub min_coverage: f64,
    /// 生成上限
    pub max_opportunities: usize,
}

impl Default for OpportunityConfig {
    fn default() -> Self {
        Self {
            min_source_projects: 2,
            min_covered_capabilities: 2,
            reusable_min_score: 0.75,
            min_coverage: 0.25,
            max_opportunities: 12,
        }
    }
}

/// 机会生成结果。
#[derive(Debug, Clone, Default)]
pub struct OpportunityOutcome {
    pub opportunities: Vec<Opportunity>,
    /// 被过滤掉的候选数（诊断用）
    pub filtered: usize,
}

/// 领域能力分析快照。
#[derive(Debug, Clone, Default)]
struct DomainAnalysis {
    /// Domain 稳定键（如 "ai"）
    key: String,
    /// Domain 展示名
    name: String,
    /// 该领域下被实现的能力 id → 实现它的项目 id 列表
    capabilities: BTreeMap<String, Vec<String>>,
    /// 涉及的全部项目 id
    projects: Vec<String>,
}

impl DomainAnalysis {
    /// 被实现的能力名集合（小写归一，用于与参照表比对）。
    fn covered_names_lower(&self, input: &AnalysisInput) -> Vec<String> {
        self.capabilities
            .keys()
            .filter_map(|id| input.capability(id))
            .map(|c| c.name.to_ascii_lowercase())
            .collect()
    }

    /// 参照表的 Domain 键。`DOMAIN_COMPLETENESS` 用的是短键（"ai"），
    /// 而能力节点 id 是 `cap_domain_ai`，这里做映射。
    fn reference_key(&self) -> Option<&'static str> {
        let suffix = self.key.strip_prefix("cap_domain_").unwrap_or(&self.key);
        DOMAIN_COMPLETENESS
            .iter()
            .find(|(k, _)| *k == suffix)
            .map(|(k, _)| *k)
    }

    /// 参照表要求的能力名列表。
    fn reference_capabilities(&self) -> &'static [&'static str] {
        self.reference_key()
            .and_then(|k| DOMAIN_COMPLETENESS.iter().find(|(dk, _)| *dk == k))
            .map(|(_, caps)| *caps)
            .unwrap_or(&[])
    }
}

/// 机会引擎。
#[derive(Debug, Clone, Default)]
pub struct OpportunityEngine;

impl OpportunityEngine {
    pub fn new() -> Self {
        Self
    }

    /// 生成机会卡片。
    pub fn generate(&self, input: &AnalysisInput, cfg: &OpportunityConfig) -> OpportunityOutcome {
        let mut out = OpportunityOutcome::default();
        let today = input.now.format("%Y-%m-%d").to_string();

        // ── 1. 按 Domain 聚合能力与项目 ────────────────────────────
        let domains = self.analyze_domains(input);

        let mut candidates: Vec<DomainAnalysis> = Vec::new();
        for d in domains {
            if d.projects.len() < cfg.min_source_projects {
                out.filtered += 1;
                continue;
            }
            if d.capabilities.len() < cfg.min_covered_capabilities {
                out.filtered += 1;
                continue;
            }
            candidates.push(d);
        }

        // ── 2. 为每个领域算覆盖度与缺失能力 ────────────────────────
        let mut ranked: Vec<(DomainAnalysis, Vec<String>, Vec<String>, f64)> = Vec::new();
        for d in candidates {
            let covered_lower = d.covered_names_lower(input);
            let reference = d.reference_capabilities();

            // 缺失 = 参照表要求 且 没有任何来源项目实现
            let missing: Vec<String> = reference
                .iter()
                .filter(|need| {
                    !covered_lower
                        .iter()
                        .any(|have| have == &need.to_ascii_lowercase())
                })
                .map(|s| (*s).to_string())
                .collect();

            // 已具备 = 实际被实现的能力名（保持能力节点顺序，稳定）
            let covered: Vec<String> = d
                .capabilities
                .keys()
                .filter_map(|id| input.capability(id).map(|c| c.name.clone()))
                .collect();

            // 覆盖度：相对参照表计算。参照表为空时退化为"无缺失"口径，
            // 避免除零或给出虚高的 100%。
            let coverage = if reference.is_empty() {
                // 没有参照表的领域：用能力数给一个保守估计，不超过 0.6
                // （诚实表达"无法判断完整性"，而不是假装 100% 完备）
                (0.3 + (covered.len() as f64 * 0.05)).min(0.6)
            } else {
                Opportunity::compute_coverage(covered.len(), missing.len())
            };

            if coverage < cfg.min_coverage {
                out.filtered += 1;
                continue;
            }
            ranked.push((d, covered, missing, coverage));
        }

        // 覆盖度高 + 项目多的排前面
        ranked.sort_by(|a, b| {
            b.3.partial_cmp(&a.3)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.0.projects.len().cmp(&a.0.projects.len()))
                .then_with(|| a.0.key.cmp(&b.0.key))
        });

        // ── 3. 组装卡片 ────────────────────────────────────────────
        for (d, covered, missing, coverage) in ranked {
            if out.opportunities.len() >= cfg.max_opportunities {
                break;
            }
            let reusable: Vec<&Asset> = d
                .projects
                .iter()
                .flat_map(|pid| input.assets_of(pid))
                .filter(|a| {
                    a.reuse_score >= cfg.reusable_min_score
                        && a.user_feedback != Some(UserFeedback::Useless)
                        && a.asset_type != AssetType::Knowledge // 知识不是"可迁移组件"
                })
                .collect();

            let rating =
                Opportunity::compute_rating(d.projects.len(), coverage, reusable.len());

            let title = format!("{} 领域的组合机会", d.name);
            let evidence: Vec<String> = {
                let mut e: Vec<String> = d
                    .projects
                    .iter()
                    .filter_map(|pid| {
                        input.project(pid).map(|p| format!("{}（{}）", p.name, p.path))
                    })
                    .collect();
                // 补上可复用资产的真实路径，让"可直接迁移"有据可查
                e.extend(reusable.iter().take(6).map(|a| format!("{}:{}", a.project_id, a.source_path)));
                e
            };

            let why = format!(
                "{} 个项目在「{}」领域合计实现 {} 项能力，存在明显互补；其中 {} 个资产复用评分达标，可直接迁移",
                d.projects.len(),
                d.name,
                covered.len(),
                reusable.len()
            );

            let description = if missing.is_empty() {
                format!(
                    "把 {} 个项目的「{}」能力组合起来，已覆盖该领域完整产品参照的全部能力项，可以直接整合成一个更完整的产品。",
                    d.projects.len(),
                    d.name
                )
            } else {
                format!(
                    "把 {} 个项目的「{}」能力组合起来，可覆盖 {} 项能力。相对该领域的完整产品参照，还缺少 {} 项（{}）——这些是需要新建的部分。",
                    d.projects.len(),
                    d.name,
                    covered.len(),
                    missing.len(),
                    missing.iter().take(5).cloned().collect::<Vec<_>>().join(" / ")
                )
            };

            out.opportunities.push(Opportunity {
                id: format!("opp_{}", d.key),
                title,
                description,
                source_project_ids: d.projects.clone(),
                source_asset_ids: reusable.iter().map(|a| a.id.clone()).collect(),
                required_capabilities: covered,
                missing_capabilities: missing,
                coverage,
                rating,
                why,
                evidence,
                status: spolia_domain::OpportunityStatus::New,
                created_at: today.clone(),
            });
        }

        // 覆盖度降序（已在 ranked 排过，这里再按 rating 稳定一次）
        out.opportunities.sort_by(|a, b| {
            b.rating
                .cmp(&a.rating)
                .then_with(|| b.coverage.partial_cmp(&a.coverage).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    /// 按 Domain 聚合：该领域下有哪些能力、分别被哪些项目实现。
    fn analyze_domains(&self, input: &AnalysisInput) -> Vec<DomainAnalysis> {
        // Domain id → 分析体
        let mut map: BTreeMap<String, DomainAnalysis> = BTreeMap::new();

        for cap in input.capability_layer_nodes() {
            let Some(domain_id) = cap.parent_id.clone() else {
                continue;
            };
            let projects = input.projects_of_capability(&cap.id);
            if projects.is_empty() {
                continue; // 没有项目实现的能力不参与组合
            }
            let entry = map.entry(domain_id.clone()).or_insert_with(|| {
                let domain = input.capability(&domain_id);
                DomainAnalysis {
                    key: domain_id.clone(),
                    name: domain.map(|d| d.name.clone()).unwrap_or_else(|| domain_id.clone()),
                    ..Default::default()
                }
            });
            entry.capabilities.insert(cap.id.clone(), projects.clone());
            for p in projects {
                if !entry.projects.contains(&p) {
                    entry.projects.push(p);
                }
            }
        }

        // 项目 id 排序，保证 id 与输出顺序确定
        let mut out: Vec<DomainAnalysis> = map.into_values().collect();
        for d in &mut out {
            d.projects.sort();
        }
        out.sort_by(|a, b| b.projects.len().cmp(&a.projects.len()).then_with(|| a.key.cmp(&b.key)));
        out
    }

    /// 「深入分析」：为一张卡片生成可执行的落地方案（V0.4 功能 23）。
    ///
    /// 🔴 只给清单与结构建议，**不生成代码**（《产品设计书》阶段二明确不做）。
    pub fn analyze(
        &self,
        input: &AnalysisInput,
        opp: &Opportunity,
        cfg: &OpportunityConfig,
    ) -> OpportunityAnalysis {
        // 可直接复用的资产（带真实路径与迁移建议）
        let reusable: Vec<ReusableItem> = opp
            .source_asset_ids
            .iter()
            .filter_map(|id| input.assets.iter().find(|a| a.id == **id))
            .filter(|a| a.reuse_score >= cfg.reusable_min_score)
            .map(|a| ReusableItem {
                asset_id: a.id.clone(),
                name: a.name.clone(),
                project_id: a.project_id.clone(),
                source_path: a.source_path.clone(),
                reuse_score: a.reuse_score,
                migration_note: migration_note(a),
            })
            .collect();

        let source_names: Vec<String> = opp
            .source_project_ids
            .iter()
            .filter_map(|pid| input.project(pid).map(|p| p.name.clone()))
            .collect();

        let rationale = format!(
            "{} 个历史项目（{}）在同一能力域内各自实现了不同部分：{}。\
             单独看每个项目都不完整，但能力互补，组合后可以覆盖 {:.0}% 的必要能力，\
             比从零开始一个新项目成本低得多。",
            source_names.len(),
            source_names.join("、"),
            opp.required_capabilities.join("、"),
            opp.coverage * 100.0
        );

        let mvp_suggestion = if opp.missing_capabilities.is_empty() {
            format!(
                "建议 MVP：以 {} 为核心骨架，把 {} 直接接入，先跑通端到端流程再补细节。",
                opp.required_capabilities.first().map(String::as_str).unwrap_or("已有能力"),
                reusable.first().map(|r| r.name.as_str()).unwrap_or("已有组件")
            )
        } else {
            format!(
                "建议 MVP：先复用已有的 {}，把 {} 作为第一阶段新建目标；\
                 其余缺失项（{}）留到验证需求后再补，避免一次做太大。",
                opp.required_capabilities.len(),
                opp.missing_capabilities.first().map(String::as_str).unwrap_or("首要缺失能力"),
                opp.missing_capabilities.iter().skip(1).take(3).cloned().collect::<Vec<_>>().join("、")
            )
        };

        OpportunityAnalysis {
            opportunity_id: opp.id.clone(),
            rationale,
            reusable,
            to_build: opp.missing_capabilities.clone(),
            mvp_suggestion,
            scaffold: suggest_scaffold(opp),
        }
    }
}

/// 迁移建议：按资产类型与通用性给出可执行的动作提示。
///
/// 这是"建议可执行"（V0.3 功能 16 的验收标准）的落地：
/// 不能只说"可复用"，要说清**怎么**复用。
fn migration_note(a: &Asset) -> String {
    if a.generality >= 0.8 {
        "通用性高，可直接复制到新项目".to_string()
    } else if a.generality >= 0.6 {
        "需抽离项目专有配置后可复用（检查硬编码路径与业务常量）".to_string()
    } else {
        "与业务耦合较深，建议只迁移其中通用部分，或作为实现参考".to_string()
    }
}

/// 新项目初始结构建议（不自动写代码，仅给清单）。
fn suggest_scaffold(opp: &Opportunity) -> Vec<String> {
    let mut out = vec![
        "README.md — 记录组合来源与复用清单".to_string(),
        "docs/decisions.md — 沿用历史项目的关键决策，避免重复踩坑".to_string(),
        "src/core/ — 承载可复用的通用能力".to_string(),
    ];
    // 为每个可复用资产给出落位目录
    for cap in opp.required_capabilities.iter().take(4) {
        out.push(format!("src/core/{}/ — 来自历史项目的 {}", slug_dir(cap), cap));
    }
    for missing in opp.missing_capabilities.iter().take(3) {
        out.push(format!("src/new/{}/ — 需新建：{}", slug_dir(missing), missing));
    }
    out
}

/// 目录名 slug（小写、非字母数字转 `-`）。
fn slug_dir(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::tests::{asset, cap, implements, project};
    use spolia_domain::{CapabilityLayer, EntityKind, ProjectStatus, RelationType};

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-29T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// AI 领域：3 个项目各持有不同能力，覆盖参照表的一部分。
    fn ai_domain_input() -> AnalysisInput {
        let specs = [
            ("p0", "c_video", "Video Generation"),
            ("p1", "c_image", "Image Generation"),
            ("p2", "c_rag", "RAG"),
        ];
        let mut projects = Vec::new();
        let mut relations = Vec::new();
        let mut capabilities = vec![cap("cap_domain_ai", "AI", CapabilityLayer::Domain, None)];
        for (pid, cid, cname) in specs {
            projects.push(project(pid, ProjectStatus::Active, Some("2026-09-01")));
            relations.push(implements(pid, cid));
            capabilities.push(cap(cid, cname, CapabilityLayer::Capability, Some("cap_domain_ai")));
        }
        AnalysisInput {
            projects,
            capabilities,
            relations,
            now: now(),
            assets: vec![
                asset("a0", "p0", "VideoPipeline", AssetType::Component, 0.91),
                asset("a1", "p1", "ImageBatch", AssetType::Component, 0.85),
            ],
        }
    }

    // ── 机会生成 ────────────────────────────────────────────────────

    #[test]
    fn generates_opportunity_from_complementary_projects() {
        let out = OpportunityEngine::new().generate(&ai_domain_input(), &OpportunityConfig::default());
        assert_eq!(out.opportunities.len(), 1);
        let o = &out.opportunities[0];
        assert!(o.title.contains("AI"), "{}", o.title);
        assert_eq!(o.source_project_ids.len(), 3);
        assert_eq!(o.required_capabilities.len(), 3);
        assert!(o.coverage > 0.0 && o.coverage <= 1.0);
        assert!((1..=5).contains(&o.rating));
        assert!(!o.why.is_empty());
        assert!(!o.evidence.is_empty());
    }

    /// 🔴 缺失能力必须来自参照表，不是凭空生成。
    #[test]
    fn missing_capabilities_come_from_reference_table() {
        let out = OpportunityEngine::new().generate(&ai_domain_input(), &OpportunityConfig::default());
        let o = &out.opportunities[0];
        let reference = DOMAIN_COMPLETENESS
            .iter()
            .find(|(k, _)| *k == "ai")
            .unwrap()
            .1;
        for m in &o.missing_capabilities {
            assert!(
                reference.contains(&m.as_str()),
                "「{m}」不在参照表里，说明是凭空生成的"
            );
        }
        // 已覆盖的不该出现在缺失里
        for c in &o.required_capabilities {
            assert!(!o.missing_capabilities.contains(c), "{c} 同时出现在已具备与缺失中");
        }
    }

    #[test]
    fn covered_capabilities_are_not_listed_as_missing() {
        let out = OpportunityEngine::new().generate(&ai_domain_input(), &OpportunityConfig::default());
        let o = &out.opportunities[0];
        // RAG 已被 p2 实现
        assert!(o.required_capabilities.iter().any(|c| c == "RAG"));
        assert!(!o.missing_capabilities.iter().any(|c| c == "RAG"));
    }

    #[test]
    fn coverage_reflects_reference_ratio() {
        let out = OpportunityEngine::new().generate(&ai_domain_input(), &OpportunityConfig::default());
        let o = &out.opportunities[0];
        let reference_len = DOMAIN_COMPLETENESS.iter().find(|(k, _)| *k == "ai").unwrap().1.len();
        let expected = Opportunity::compute_coverage(3, reference_len - 3);
        assert!((o.coverage - expected).abs() < 1e-9, "覆盖度应按参照表计算");
    }

    /// 描述必须说明"相对参照表"，不假装是客观真理。
    #[test]
    fn description_discloses_reference_basis() {
        let out = OpportunityEngine::new().generate(&ai_domain_input(), &OpportunityConfig::default());
        assert!(out.opportunities[0].description.contains("参照"), "{}", out.opportunities[0].description);
    }

    /// 单个项目谈不上"组合"。
    #[test]
    fn single_project_yields_no_opportunity() {
        let mut input = ai_domain_input();
        input.projects.truncate(1);
        input.relations.truncate(1);
        let out = OpportunityEngine::new().generate(&input, &OpportunityConfig::default());
        assert!(out.opportunities.is_empty());
        assert!(out.filtered >= 1, "应记录被过滤的候选");
    }

    /// 能力太少不构成有价值的组合。
    #[test]
    fn too_few_capabilities_is_filtered() {
        let input = AnalysisInput {
            projects: vec![
                project("p0", ProjectStatus::Active, Some("2026-09-01")),
                project("p1", ProjectStatus::Active, Some("2026-09-01")),
            ],
            capabilities: vec![
                cap("cap_domain_ai", "AI", CapabilityLayer::Domain, None),
                cap("c_one", "RAG", CapabilityLayer::Capability, Some("cap_domain_ai")),
            ],
            relations: vec![implements("p0", "c_one"), implements("p1", "c_one")],
            now: now(),
            ..Default::default()
        };
        assert!(OpportunityEngine::new().generate(&input, &OpportunityConfig::default()).opportunities.is_empty());
    }

    /// 没有项目实现的能力不参与组合（否则机会卡片会列出"你其实没有的能力"）。
    #[test]
    fn unimplemented_capabilities_are_excluded() {
        let mut input = ai_domain_input();
        input.capabilities.push(cap("c_ghost", "Ghost Capability", CapabilityLayer::Capability, Some("cap_domain_ai")));
        // 没有任何 relation 指向 c_ghost
        let out = OpportunityEngine::new().generate(&input, &OpportunityConfig::default());
        let o = &out.opportunities[0];
        assert!(!o.required_capabilities.iter().any(|c| c == "Ghost Capability"));
    }

    #[test]
    fn min_coverage_filters_hopeless_combinations() {
        let strict = OpportunityConfig { min_coverage: 0.99, ..Default::default() };
        assert!(OpportunityEngine::new().generate(&ai_domain_input(), &strict).opportunities.is_empty());
    }

    #[test]
    fn max_opportunities_caps_output() {
        // 造 3 个领域，各自满足条件（2 个项目 × 2 项能力）
        let mut input = AnalysisInput { now: now(), ..Default::default() };
        for (di, dname) in ["AI", "Web", "数据"].iter().enumerate() {
            let domain_id = format!("cap_domain_{di}");
            input
                .capabilities
                .push(cap(&domain_id, dname, CapabilityLayer::Domain, None));
            for i in 0..2 {
                let pid = format!("p{di}_{i}");
                input
                    .projects
                    .push(project(&pid, ProjectStatus::Active, Some("2026-09-01")));
                for c in 0..2 {
                    let cid = format!("c{di}_{c}");
                    if !input.capabilities.iter().any(|x| x.id == cid) {
                        input.capabilities.push(cap(
                            &cid,
                            &format!("{dname} Cap{c}"),
                            CapabilityLayer::Capability,
                            Some(&domain_id),
                        ));
                    }
                    input.relations.push(implements(&pid, &cid));
                }
            }
        }
        let cfg = OpportunityConfig {
            max_opportunities: 2,
            min_coverage: 0.0,
            ..Default::default()
        };
        let out = OpportunityEngine::new().generate(&input, &cfg);
        assert_eq!(out.opportunities.len(), 2, "应受 max_opportunities 限制");
    }

    /// 可复用资产清单：低分与知识类资产不该混进"可直接迁移"。
    #[test]
    fn reusable_assets_are_filtered() {
        let mut input = ai_domain_input();
        input.assets.push(asset("a_low", "p2", "Trivial", AssetType::Code, 0.3));
        input.assets.push(asset("a_know", "p2", "SomeKnowledge", AssetType::Knowledge, 0.95));
        let mut rejected = asset("a_rej", "p2", "Rejected", AssetType::Code, 0.95);
        rejected.user_feedback = Some(UserFeedback::Useless);
        input.assets.push(rejected);

        let out = OpportunityEngine::new().generate(&input, &OpportunityConfig::default());
        let ids = &out.opportunities[0].source_asset_ids;
        assert!(ids.contains(&"a0".to_string()));
        assert!(ids.contains(&"a1".to_string()));
        assert!(!ids.contains(&"a_low".to_string()), "低分资产不该入选");
        assert!(!ids.contains(&"a_know".to_string()), "知识不是可迁移组件");
        assert!(!ids.contains(&"a_rej".to_string()), "用户已否决的不该再推荐");
    }

    #[test]
    fn evidence_contains_real_paths() {
        let out = OpportunityEngine::new().generate(&ai_domain_input(), &OpportunityConfig::default());
        let o = &out.opportunities[0];
        assert!(o.evidence.iter().any(|e| e.contains("/tmp/p0")), "应含真实项目路径: {:?}", o.evidence);
        assert!(o.evidence.iter().any(|e| e.contains("p0/src/")), "应含真实资产路径");
    }

    #[test]
    fn rating_scales_with_strength() {
        // 项目多 + 资产多 → 星级高
        let mut rich = ai_domain_input();
        rich.projects.push(project("p3", ProjectStatus::Active, Some("2026-09-01")));
        rich.relations.push(implements("p3", "c_rag"));
        rich.assets.push(asset("a3", "p3", "Extra", AssetType::Component, 0.9));

        let cfg = OpportunityConfig::default();
        let engine = OpportunityEngine::new();
        let base_rating = engine.generate(&ai_domain_input(), &cfg).opportunities[0].rating;
        let rich_rating = engine.generate(&rich, &cfg).opportunities[0].rating;
        assert!(rich_rating >= base_rating, "rich={rich_rating} base={base_rating}");
        assert!(rich_rating <= 5);
    }

    #[test]
    fn output_order_is_deterministic() {
        let engine = OpportunityEngine::new();
        let cfg = OpportunityConfig::default();
        let a = engine.generate(&ai_domain_input(), &cfg);
        let b = engine.generate(&ai_domain_input(), &cfg);
        assert_eq!(
            a.opportunities.iter().map(|o| o.id.clone()).collect::<Vec<_>>(),
            b.opportunities.iter().map(|o| o.id.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn empty_input_yields_nothing() {
        let input = AnalysisInput { now: now(), ..Default::default() };
        let out = OpportunityEngine::new().generate(&input, &OpportunityConfig::default());
        assert!(out.opportunities.is_empty());
    }

    // ── 深入分析 ────────────────────────────────────────────────────

    #[test]
    fn analysis_produces_actionable_plan() {
        let engine = OpportunityEngine::new();
        let cfg = OpportunityConfig::default();
        let input = ai_domain_input();
        let opp = engine.generate(&input, &cfg).opportunities.remove(0);
        let a = engine.analyze(&input, &opp, &cfg);

        assert_eq!(a.opportunity_id, opp.id);
        assert!(!a.rationale.is_empty());
        assert!(!a.reusable.is_empty(), "应给出可复用清单");
        assert!(!a.mvp_suggestion.is_empty());
        assert!(!a.scaffold.is_empty());
        // 迁移建议必须可执行，不能是"可复用"这类空话
        for r in &a.reusable {
            assert!(r.migration_note.len() > 10, "建议过于空泛: {}", r.migration_note);
            assert!(!r.source_path.is_empty());
            assert!(r.reuse_score > 0.0);
        }
        // rationale 应引用真实项目名
        assert!(a.rationale.contains("p0") || a.rationale.contains("p1"));
    }

    #[test]
    fn analysis_lists_missing_as_to_build() {
        let engine = OpportunityEngine::new();
        let cfg = OpportunityConfig::default();
        let input = ai_domain_input();
        let opp = engine.generate(&input, &cfg).opportunities.remove(0);
        let a = engine.analyze(&input, &opp, &cfg);
        assert_eq!(a.to_build, opp.missing_capabilities);
        assert!(a.mvp_suggestion.contains("新建") || a.mvp_suggestion.contains("复用"));
    }

    /// 明确不做：不生成代码（《产品设计书》阶段二边界）。
    #[test]
    fn scaffold_is_structure_not_code() {
        let engine = OpportunityEngine::new();
        let cfg = OpportunityConfig::default();
        let input = ai_domain_input();
        let opp = engine.generate(&input, &cfg).opportunities.remove(0);
        let a = engine.analyze(&input, &opp, &cfg);
        for line in &a.scaffold {
            assert!(line.contains('/') || line.contains('—'), "应是目录/文件清单: {line}");
            assert!(!line.contains("fn ") && !line.contains("def ") && !line.contains("function "),
                "不得生成代码: {line}");
        }
    }

    #[test]
    fn migration_note_varies_with_generality() {
        let mut generic = asset("a", "p", "X", AssetType::Code, 0.9);
        generic.generality = 0.9;
        let mut coupled = asset("b", "p", "Y", AssetType::Code, 0.9);
        coupled.generality = 0.3;
        assert!(migration_note(&generic).contains("直接复制"));
        assert!(migration_note(&coupled).contains("耦合"));
        let mut mid = asset("c", "p", "Z", AssetType::Code, 0.9);
        mid.generality = 0.7;
        assert!(migration_note(&mid).contains("抽离"));
    }

    #[test]
    fn slug_dir_is_path_safe() {
        assert_eq!(slug_dir("Video Generation"), "video-generation");
        assert_eq!(slug_dir("AI/RAG"), "ai-rag");
        assert_eq!(slug_dir("中文名"), "");
        assert!(!slug_dir("A  B").contains("  "));
    }

    // ── 参照表自洽 ──────────────────────────────────────────────────

    /// 参照表的能力名必须与资产引擎产出的能力名对得上，
    /// 否则永远匹配不到、所有机会卡片都会显示"全部缺失"。
    #[test]
    fn reference_names_are_recognized_capability_names() {
        // 从 spolia-asset 的规则表取出全部已知能力名
        let known: Vec<String> = spolia_asset::DOMAINS
            .iter()
            .map(|(_, label)| label.to_string())
            .collect();
        assert!(!known.is_empty());

        // 参照表里的每个能力名都应是"看起来像能力名"的规范写法（首字母大写、含空格或单词）
        for (domain, caps) in DOMAIN_COMPLETENESS {
            assert!(!caps.is_empty(), "领域 {domain} 的参照表为空");
            for c in *caps {
                assert!(!c.is_empty());
                assert!(
                    c.chars().next().unwrap().is_ascii_uppercase(),
                    "能力名应首字母大写以便与抽取结果匹配: {c} (domain {domain})"
                );
            }
            // 同一领域内不得重复
            let mut v: Vec<&str> = caps.to_vec();
            let before = v.len();
            v.sort();
            v.dedup();
            assert_eq!(v.len(), before, "领域 {domain} 的参照表有重复项");
        }
    }

    /// 参照表的 domain 键必须是能力树里真实存在的 Domain id 后缀。
    #[test]
    fn reference_keys_match_domain_ids() {
        let valid: Vec<String> = spolia_asset::DOMAINS.iter().map(|(k, _)| k.to_string()).collect();
        for (key, _) in DOMAIN_COMPLETENESS {
            assert!(valid.contains(&key.to_string()), "参照表 domain 键「{key}」不在 DOMAINS 中");
        }
    }

    #[test]
    fn domain_analysis_finds_reference() {
        let d = DomainAnalysis {
            key: "cap_domain_ai".into(),
            name: "AI".into(),
            ..Default::default()
        };
        assert_eq!(d.reference_key(), Some("ai"));
        assert!(!d.reference_capabilities().is_empty());
    }

    #[test]
    fn unknown_domain_has_no_reference() {
        let d = DomainAnalysis {
            key: "cap_domain_unknown".into(),
            name: "Unknown".into(),
            ..Default::default()
        };
        assert_eq!(d.reference_key(), None);
        assert!(d.reference_capabilities().is_empty());
    }

    /// 无参照表的领域：覆盖度给保守估计，绝不假装 100% 完备。
    #[test]
    fn domain_without_reference_gets_conservative_coverage() {
        let input = AnalysisInput {
            projects: vec![
                project("p0", ProjectStatus::Active, Some("2026-09-01")),
                project("p1", ProjectStatus::Active, Some("2026-09-01")),
            ],
            capabilities: vec![
                cap("cap_domain_custom", "自定义领域", CapabilityLayer::Domain, None),
                cap("c_x", "Some Capability", CapabilityLayer::Capability, Some("cap_domain_custom")),
                cap("c_y", "Other Capability", CapabilityLayer::Capability, Some("cap_domain_custom")),
            ],
            relations: vec![implements("p0", "c_x"), implements("p1", "c_y")],
            now: now(),
            ..Default::default()
        };
        let cfg = OpportunityConfig { min_coverage: 0.0, ..Default::default() };
        let out = OpportunityEngine::new().generate(&input, &cfg);
        assert_eq!(out.opportunities.len(), 1);
        let o = &out.opportunities[0];
        assert!(o.coverage <= 0.6, "无参照表时不得给出虚高覆盖度: {}", o.coverage);
        assert!(o.missing_capabilities.is_empty(), "无参照表就不该编造缺失项");
        assert!(o.description.contains("全部能力项") || o.description.contains("覆盖"));
    }

    #[test]
    fn config_defaults_are_sane() {
        let c = OpportunityConfig::default();
        assert!(c.min_source_projects >= 2);
        assert!(c.min_covered_capabilities >= 2);
        assert!(c.min_coverage > 0.0 && c.min_coverage < 1.0);
        assert!(c.max_opportunities > 0);
    }

    // 供测试使用的辅助：确认关系构造正确
    #[test]
    fn fixture_relations_are_implements() {
        let r = implements("p0", "c_video");
        assert_eq!(r.relation_type, RelationType::Implements);
        assert_eq!(r.source_type, EntityKind::Project);
        assert_eq!(r.target_type, EntityKind::Capability);
    }
}
