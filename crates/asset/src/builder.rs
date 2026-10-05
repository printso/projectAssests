//! 资产构建器：把符号 + 评分 + 能力抽取组装为可入库的 `Asset` / `Capability` / `Relation`。
//!
//! # 职责边界
//! 本模块是**纯函数式的装配层**：不读文件系统、不碰数据库、不调 LLM。
//! 输入是已解析好的项目信号，输出是领域对象。这样它可以被完整单测，
//! 也便于将来把"符号来源"从启发式换成 Tree-sitter 而不动装配逻辑。
//!
//! # 证据链纪律（《产品设计书》产品纪律 #1）
//! 每条产出的 Asset 都带 `Evidence`（真实文件路径 + 符号签名 + 评分理由）。
//! 无证据的资产在存储层会被拒绝入库，这里也不产出——
//! **"给证据，不给玄学"必须从生成源头就成立**。

use serde::{Deserialize, Serialize};

use spolia_domain::{
    Asset, AssetType, Capability, EntityKind, Evidence, Relation, RelationType,
};

use crate::capability::{domain_capabilities, CapabilityExtraction, CapabilityExtractor, CapabilitySignals};
use crate::scoring::{Score, Scorer};
use crate::symbols::{Symbol, SymbolKind};

/// 构建资产所需的项目侧输入。
#[derive(Debug, Clone)]
pub struct ProjectInput<'a> {
    /// 稳定项目 id（由路径派生，见 `spolia_scanner::project_id_from_path`）
    pub project_id: &'a str,
    pub project_name: &'a str,
    /// 项目根绝对路径（用于把相对路径还原为可点击的绝对路径展示）
    pub project_root: &'a str,
    /// 抽取到的符号
    pub symbols: &'a [Symbol],
    /// 能力抽取所需的信号
    pub capability_signals: &'a CapabilitySignals,
    /// 能力置信度门槛（低于此值不进图谱，《技术设计书》§25）
    pub capability_confidence_floor: f64,
    /// 资产创建时间戳，由调用方注入。
    ///
    /// 🔴 刻意**不在本模块读时钟**：builder 是纯装配层，
    /// 不依赖 `SystemTime` 才能做到"同输入 → 同输出"，
    /// 否则每次测试的 `created_at` 都不同，无法做断言比对。
    /// 同时也避免了 asset 层反向依赖 storage 层（时间函数在那里）。
    pub created_at: &'a str,
}

/// 构建产物。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extraction {
    /// 代码/组件/API 类资产
    pub assets: Vec<Asset>,
    /// 能力节点（Domain + Capability + Implementation 三层）
    pub capabilities: Vec<Capability>,
    /// 关系边（project implements capability、project contains asset 等）
    pub relations: Vec<Relation>,
    /// 统计：被评分但判为不值得入库的符号数（诊断"资产库为什么这么少"）
    pub skipped_symbols: usize,
}

/// 资产构建器。
#[derive(Debug, Clone)]
pub struct AssetBuilder {
    scorer: Scorer,
    extractor: CapabilityExtractor,
}

impl Default for AssetBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetBuilder {
    pub fn new() -> Self {
        Self {
            scorer: Scorer::new(),
            extractor: CapabilityExtractor::new(),
        }
    }

    pub fn with_scorer(scorer: Scorer) -> Self {
        Self { scorer, extractor: CapabilityExtractor::new() }
    }

    /// 从项目输入构建全部资产、能力与关系。
    pub fn build(&self, input: &ProjectInput<'_>) -> Extraction {
        let mut out = Extraction::default();

        // ── 1. Domain 骨架（始终存在，维持图谱顶层结构稳定）──────────
        out.capabilities.extend(domain_capabilities());

        // ── 2. 能力抽取（三层）─────────────────────────────────────
        let caps: CapabilityExtraction = self
            .extractor
            .extract(input.capability_signals, input.capability_confidence_floor);
        out.capabilities.extend(caps.capabilities.iter().cloned());
        out.capabilities.extend(caps.implementations.iter().cloned());

        // project implements capability（带命中信号作为 Evidence）
        for cap in &caps.capabilities {
            let evidence = caps
                .evidence
                .get(&cap.id)
                .cloned()
                .unwrap_or_default();
            out.relations.push(
                Relation::new(
                    relation_id(input.project_id, "implements", &cap.id),
                    input.project_id,
                    EntityKind::Project,
                    RelationType::Implements,
                    &cap.id,
                    EntityKind::Capability,
                    cap.confidence,
                )
                .with_evidence(evidence),
            );
        }

        // ── 3. 符号 → 资产 ─────────────────────────────────────────
        for sym in input.symbols {
            let score: Score = self.scorer.score_symbol(sym, Some(input.project_name));
            if !score.worth_tracking {
                out.skipped_symbols += 1;
                continue;
            }
            let asset = symbol_to_asset(input, sym, &score);
            // project contains asset
            out.relations.push(Relation::new(
                relation_id(input.project_id, "contains", &asset.id),
                input.project_id,
                EntityKind::Project,
                RelationType::Contains,
                &asset.id,
                EntityKind::Asset,
                1.0, // 结构性关系，确定成立
            ));
            out.assets.push(asset);
        }

        out
    }
}

/// 把一个符号转为 `Asset`。
///
/// 🔴 关键设计：**id 由 (项目, 文件, 符号名, 行号) 派生**，保证幂等。
/// 重新扫描同一项目时，同一符号得到同一 asset id，
/// 于是存储层的 upsert 会**更新**而非新增，
/// 且 `user_feedback`（用户标记的"有用"）不会被抹掉。
/// 若用 uuid，每次扫描都会产生全新资产，历史反馈全部丢失。
fn symbol_to_asset(input: &ProjectInput<'_>, sym: &Symbol, score: &Score) -> Asset {
    let asset_id = asset_id_from(input.project_id, &sym.file_path, &sym.qualified_name(), sym.line);

    // 证据链：全部来自真实文件与真实评分，无编造
    let mut reasoning = score.reasoning.clone();
    reasoning.push(format!(
        "评分明细：体量 {:.2} / 通用性 {:.2} / 稳定性 {:.2}",
        score.reuse_score, score.generality, score.stability
    ));

    let evidence = Evidence {
        files: vec![sym.file_path.clone()],
        commits: Vec::new(), // Level 1 不关联 commit（需要 blame，成本高）
        used_by: if sym.referenced_locally {
            vec![format!("{}:{}", sym.file_path, sym.line)]
        } else {
            Vec::new()
        },
        reasoning,
    };

    // 描述：优先用文档注释（作者自己的表述），否则用签名兜底。
    // 🔴 绝不生成"这是一个用于处理数据的函数"这类模板文案——那是假的。
    let description = match &sym.doc_comment {
        Some(d) if !d.trim().is_empty() => d.clone(),
        _ => sym.signature.clone(),
    };

    Asset {
        id: asset_id,
        project_id: input.project_id.to_string(),
        asset_type: symbol_kind_to_asset_type(sym.kind),
        name: sym.qualified_name(),
        description,
        content: Some(sym.signature.clone()),
        source_path: sym.file_path.clone(),
        confidence: score.confidence,
        reuse_score: score.reuse_score,
        generality: score.generality,
        stability: score.stability,
        tags: symbol_tags(sym),
        created_at: input.created_at.to_string(),
        evidence,
        user_feedback: None,
    }
}

/// 符号种类 → 资产类型。
///
/// API 端点与组件单列，因为它们在资产页有独立的筛选价值
/// （用户找"我以前写过哪些接口"与找"哪些组件可复用"是两种不同意图）。
fn symbol_kind_to_asset_type(kind: SymbolKind) -> AssetType {
    match kind {
        SymbolKind::ApiEndpoint => AssetType::Api,
        SymbolKind::Component
        | SymbolKind::Class
        | SymbolKind::Struct
        | SymbolKind::Trait
        | SymbolKind::Interface
        | SymbolKind::Enum => AssetType::Component,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constant | SymbolKind::Type => {
            AssetType::Code
        }
    }
}

/// 资产标签：语言 + 符号种类（中文），供资产页 chips 筛选。
fn symbol_tags(sym: &Symbol) -> Vec<String> {
    let mut tags = vec![sym.language.clone(), sym.kind.label_zh().to_string()];
    if let Some(parent) = &sym.parent {
        tags.push(parent.clone());
    }
    tags
}

/// 生成稳定的资产 id。
fn asset_id_from(project_id: &str, file_path: &str, qualified_name: &str, line: usize) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    project_id.hash(&mut h);
    file_path.hash(&mut h);
    qualified_name.hash(&mut h);
    line.hash(&mut h);
    let hash = h.finish();
    // 前缀用符号名的安全片段，便于在日志与数据库里辨认
    let slug = safe_slug(qualified_name);
    format!("a_{slug}_{hash:x}")
}

/// 生成稳定的关系 id。
fn relation_id(source: &str, kind: &str, target: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    kind.hash(&mut h);
    target.hash(&mut h);
    format!("r_{kind}_{:x}", h.finish())
}

/// 符号名 → id 安全片段（只保留 ASCII 字母数字与 `_`）。
fn safe_slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('_');
        }
        if out.len() >= 24 {
            break;
        }
    }
    out.trim_matches('_').to_string()
}

/// 跨项目重复检测的输入：`(资产名, 项目 id, 文件路径, reuse_score)`。
///
/// 单独定义而不复用 `Asset`：跨项目分析只需要这几个字段，
/// 传完整 Asset 会让调用方被迫先加载全部资产内容（含 code 片段），内存浪费明显。
#[derive(Debug, Clone, PartialEq)]
pub struct AssetRef<'a> {
    pub name: &'a str,
    pub project_id: &'a str,
    pub source_path: &'a str,
    pub reuse_score: f64,
}

/// 构建 `similar_to` 关系：同名资产出现在多个项目中即视为重复实现。
///
/// 这是《产品设计书》V0.2 功能 9「跨项目关联」的确定性实现。
///
/// 🔴 保守策略：**同名 + 同资产类型**才算相似，不做语义相似度。
/// 语义相似需要 Embedding（Level 1 后半），在向量检索就绪前
/// 宁可少报也不误报——错误的"你在 4 个项目重复实现了 X"会直接摧毁用户信任。
pub fn build_duplicate_relations(refs: &[AssetRef<'_>], min_projects: usize) -> Vec<Relation> {
    use std::collections::BTreeMap;

    // 按归一化名字分组（大小写不敏感：Windows/macOS 上 VideoPipeline 与 videopipeline 常并存）
    let mut groups: BTreeMap<String, Vec<&AssetRef<'_>>> = BTreeMap::new();
    for r in refs {
        groups
            .entry(r.name.to_ascii_lowercase())
            .or_default()
            .push(r);
    }

    let mut out = Vec::new();
    for (key, members) in groups {
        // 统计**不同项目**数（同一项目内重复不算跨项目重复）
        let mut project_ids: Vec<&str> = members.iter().map(|m| m.project_id).collect();
        project_ids.sort_unstable();
        project_ids.dedup();
        if project_ids.len() < min_projects.max(2) {
            continue;
        }

        // 两两建边（去重：只建 a→b，不建 b→a，避免关系表膨胀一倍）
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        for i in 0..project_ids.len() {
            for j in (i + 1)..project_ids.len() {
                pairs.push((project_ids[i], project_ids[j]));
            }
        }
        // 置信度：项目数越多越确信；平均分作为强度参考
        let confidence = duplicate_confidence(project_ids.len(), &members);

        for (a, b) in pairs {
            let evidence: Vec<String> = members
                .iter()
                .filter(|m| m.project_id == a || m.project_id == b)
                .map(|m| format!("{}:{}", m.project_id, m.source_path))
                .collect();
            out.push(
                Relation::new(
                    relation_id(a, "similar_to", b),
                    a,
                    EntityKind::Project,
                    RelationType::SimilarTo,
                    b,
                    EntityKind::Project,
                    confidence,
                )
                .with_evidence(evidence),
            );
        }
        let _ = key; // key 仅用于分组
    }
    out
}

/// 重复实现的置信度。
///
/// 因子：涉及项目数（越多越可能是真重复）+ 平均复用分（高分说明都是实质实现，
/// 而不是巧合的同名 getter）。上限 0.95：同名不等于同实现，不给满分。
fn duplicate_confidence(project_count: usize, members: &[&AssetRef<'_>]) -> f64 {
    let base = match project_count {
        2 => 0.62,
        3 => 0.72,
        4 => 0.80,
        _ => 0.86,
    };
    let avg_score = if members.is_empty() {
        0.0
    } else {
        members.iter().map(|m| m.reuse_score).sum::<f64>() / members.len() as f64
    };
    (base + avg_score * 0.12).min(0.95)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::{HeuristicExtractor, SymbolExtractor};

    fn sig(name: &str, kind: SymbolKind, body: usize, path: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            language: "Python".into(),
            file_path: path.to_string(),
            line: 10,
            signature: format!("def {name}(self):"),
            doc_comment: None,
            body_lines: body,
            parent: None,
            referenced_locally: true,
        }
    }

    fn input<'a>(
        project_id: &'a str,
        symbols: &'a [Symbol],
        signals: &'a CapabilitySignals,
    ) -> ProjectInput<'a> {
        ProjectInput {
            project_id,
            project_name: "demo",
            project_root: "/tmp/demo",
            symbols,
            capability_signals: signals,
            capability_confidence_floor: 0.5,
            // 固定时间戳：保证"同输入 → 同输出"，测试才能做精确断言
            created_at: "2026-09-29",
        }
    }

    // ── 构建产物 ────────────────────────────────────────────────────

    #[test]
    fn builds_assets_from_substantial_symbols() {
        let syms = vec![
            sig("batch_process", SymbolKind::Function, 40, "src/a.py"),
            sig("ImagePipeline", SymbolKind::Class, 60, "src/p.py"),
        ];
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &syms, &signals));
        assert_eq!(out.assets.len(), 2);
        assert_eq!(out.skipped_symbols, 0);
        for a in &out.assets {
            assert_eq!(a.project_id, "p1");
            assert!(a.reuse_score > 0.0);
        }
    }

    /// 琐碎符号被过滤，且计数可见（便于诊断"资产为什么少"）。
    #[test]
    fn skips_trivial_symbols_with_count() {
        let syms = vec![
            sig("batch_process", SymbolKind::Function, 40, "src/a.py"),
            sig("get", SymbolKind::Function, 1, "src/b.py"),
            sig("MAX", SymbolKind::Constant, 1, "src/c.py"),
        ];
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &syms, &signals));
        assert_eq!(out.assets.len(), 1);
        assert_eq!(out.skipped_symbols, 2);
    }

    #[test]
    fn disposable_symbols_never_become_assets() {
        let syms = vec![sig("debug_tmp_2024", SymbolKind::Function, 80, "src/x.py")];
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &syms, &signals));
        assert!(out.assets.is_empty());
    }

    // ── 证据链（产品红线）───────────────────────────────────────────

    /// 每条资产都必须有证据，且证据指向真实文件。
    #[test]
    fn every_asset_carries_real_evidence() {
        let syms = vec![sig("batch_process", SymbolKind::Function, 40, "src/pipe.py")];
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &syms, &signals));
        let a = &out.assets[0];
        assert!(a.evidence.is_sufficient(), "无证据的资产不得产出");
        assert_eq!(a.evidence.files, vec!["src/pipe.py".to_string()]);
        assert!(!a.evidence.reasoning.is_empty(), "必须给出评分理由");
        assert!(a.evidence.reasoning.iter().any(|r| r.contains("评分明细")));
    }

    #[test]
    fn evidence_records_local_usage() {
        let mut s = sig("worker", SymbolKind::Function, 30, "src/w.py");
        s.referenced_locally = true;
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &[s], &signals));
        assert_eq!(out.assets[0].evidence.used_by, vec!["src/w.py:10".to_string()]);
    }

    /// 描述优先用作者自己的文档注释，绝不生成模板文案。
    #[test]
    fn description_prefers_doc_comment() {
        let mut s = sig("resize_image", SymbolKind::Function, 30, "src/i.py");
        s.doc_comment = Some("按比例缩放图片并保留 EXIF".into());
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &[s], &signals));
        assert_eq!(out.assets[0].description, "按比例缩放图片并保留 EXIF");
    }

    #[test]
    fn description_falls_back_to_signature_not_template() {
        let s = sig("resize_image", SymbolKind::Function, 30, "src/i.py");
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &[s], &signals));
        let d = &out.assets[0].description;
        assert!(d.contains("resize_image"), "应回退到签名: {d}");
        assert!(!d.contains("这是一个"), "不得生成模板文案");
    }

    // ── id 稳定性（幂等的关键）──────────────────────────────────────

    /// 🔴 重新扫描同一项目必须得到同一 asset id，
    /// 否则用户标记的"有用"反馈会全部丢失。
    #[test]
    fn asset_ids_are_stable_across_builds() {
        let syms = vec![sig("batch_process", SymbolKind::Function, 40, "src/a.py")];
        let signals = CapabilitySignals::default();
        let b = AssetBuilder::new();
        let first = b.build(&input("p1", &syms, &signals));
        let second = b.build(&input("p1", &syms, &signals));
        assert_eq!(first.assets[0].id, second.assets[0].id);
    }

    #[test]
    fn different_symbols_get_different_ids() {
        let syms = vec![
            sig("alpha", SymbolKind::Function, 40, "src/a.py"),
            sig("beta", SymbolKind::Function, 40, "src/b.py"),
        ];
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &syms, &signals));
        assert_ne!(out.assets[0].id, out.assets[1].id);
    }

    #[test]
    fn asset_id_is_readable_and_bounded() {
        let id = asset_id_from("p1", "src/a.py", "batch_process", 10);
        assert!(id.starts_with("a_batch_process_"), "id 应含可读名: {id}");
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert!(id.len() < 60);
    }

    // ── 资产类型映射 ────────────────────────────────────────────────

    #[test]
    fn symbol_kinds_map_to_asset_types() {
        assert_eq!(symbol_kind_to_asset_type(SymbolKind::ApiEndpoint), AssetType::Api);
        assert_eq!(symbol_kind_to_asset_type(SymbolKind::Component), AssetType::Component);
        assert_eq!(symbol_kind_to_asset_type(SymbolKind::Class), AssetType::Component);
        assert_eq!(symbol_kind_to_asset_type(SymbolKind::Function), AssetType::Code);
        assert_eq!(symbol_kind_to_asset_type(SymbolKind::Method), AssetType::Code);
        assert_eq!(symbol_kind_to_asset_type(SymbolKind::Trait), AssetType::Component);
    }

    #[test]
    fn api_endpoint_becomes_api_asset() {
        let mut s = sig("GET /api/projects", SymbolKind::ApiEndpoint, 20, "routes.js");
        s.language = "JavaScript".into();
        s.signature = "app.get('/api/projects', handler)".into();
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &[s], &signals));
        assert_eq!(out.assets[0].asset_type, AssetType::Api);
    }

    #[test]
    fn tags_include_language_and_kind() {
        let syms = vec![sig("batch_process", SymbolKind::Function, 40, "src/a.py")];
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &syms, &signals));
        let tags = &out.assets[0].tags;
        assert!(tags.contains(&"Python".to_string()));
        assert!(tags.contains(&"函数".to_string()));
    }

    // ── 能力与关系 ──────────────────────────────────────────────────

    /// Domain 骨架必须始终存在，否则图谱顶层结构不稳定。
    #[test]
    fn always_emits_domain_skeleton() {
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &[], &signals));
        let domains: Vec<_> = out
            .capabilities
            .iter()
            .filter(|c| c.layer == spolia_domain::CapabilityLayer::Domain)
            .collect();
        assert_eq!(domains.len(), 5, "应始终有 5 个 Domain 根节点");
        assert!(out.assets.is_empty());
    }

    #[test]
    fn emits_three_layer_capabilities_with_relations() {
        let signals = CapabilitySignals {
            dependencies: vec!["diffusers".into(), "langchain".into()],
            ..Default::default()
        };
        let out = AssetBuilder::new().build(&input("p1", &[], &signals));

        // 三层都在
        assert!(out.capabilities.iter().any(|c| c.layer == spolia_domain::CapabilityLayer::Domain));
        assert!(out.capabilities.iter().any(|c| c.layer == spolia_domain::CapabilityLayer::Capability));
        assert!(out.capabilities.iter().any(|c| c.layer == spolia_domain::CapabilityLayer::Implementation));

        // project implements capability 关系
        let impls: Vec<_> = out
            .relations
            .iter()
            .filter(|r| r.relation_type == RelationType::Implements)
            .collect();
        assert!(!impls.is_empty());
        assert!(impls.iter().all(|r| r.source_id == "p1"));
        assert!(impls.iter().all(|r| r.source_type == EntityKind::Project));
        assert!(impls.iter().all(|r| r.target_type == EntityKind::Capability));
        // Evidence 必须记录命中信号
        assert!(impls.iter().all(|r| !r.evidence.is_empty()));
    }

    #[test]
    fn emits_contains_relation_for_each_asset() {
        let syms = vec![
            sig("batch_process", SymbolKind::Function, 40, "src/a.py"),
            sig("ImagePipeline", SymbolKind::Class, 60, "src/p.py"),
        ];
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &syms, &signals));
        let contains: Vec<_> = out
            .relations
            .iter()
            .filter(|r| r.relation_type == RelationType::Contains)
            .collect();
        assert_eq!(contains.len(), 2);
        assert!(contains.iter().all(|r| r.confidence == 1.0), "结构性关系应确定成立");
        // 每条 contains 的 target 必须是实际产出的 asset
        for r in contains {
            assert!(out.assets.iter().any(|a| a.id == r.target_id), "关系指向不存在的资产");
        }
    }

    /// 能力节点 id 必须跨项目一致，否则图谱会碎裂成互不相连的碎片。
    #[test]
    fn capability_ids_are_shared_across_projects() {
        let signals = CapabilitySignals {
            dependencies: vec!["langchain".into()],
            ..Default::default()
        };
        let b = AssetBuilder::new();
        let p1 = b.build(&input("p1", &[], &signals));
        let p2 = b.build(&input("p2", &[], &signals));
        let ids1: Vec<&str> = p1.capabilities.iter().map(|c| c.id.as_str()).collect();
        let ids2: Vec<&str> = p2.capabilities.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids1, ids2, "同名能力在不同项目必须同 id");
    }

    // ── 跨项目重复检测 ──────────────────────────────────────────────

    fn aref<'a>(name: &'a str, pid: &'a str, path: &'a str, score: f64) -> AssetRef<'a> {
        AssetRef { name, project_id: pid, source_path: path, reuse_score: score }
    }

    #[test]
    fn detects_cross_project_duplicates() {
        let refs = vec![
            aref("TaskQueue", "p1", "a/queue.py", 0.9),
            aref("TaskQueue", "p2", "b/queue.py", 0.85),
            aref("TaskQueue", "p3", "c/queue.py", 0.8),
        ];
        let rels = build_duplicate_relations(&refs, 2);
        // 3 个项目两两组合 = 3 条边
        assert_eq!(rels.len(), 3);
        for r in &rels {
            assert_eq!(r.relation_type, RelationType::SimilarTo);
            assert!(r.confidence > 0.5);
            assert!(!r.evidence.is_empty(), "重复判定必须带文件证据");
        }
    }

    /// 同一项目内重复不算跨项目重复（否则单项目的多个 helper 会互相误报）。
    #[test]
    fn same_project_repeats_are_not_duplicates() {
        let refs = vec![
            aref("Helper", "p1", "a.py", 0.9),
            aref("Helper", "p1", "b.py", 0.9),
        ];
        assert!(build_duplicate_relations(&refs, 2).is_empty());
    }

    #[test]
    fn min_projects_threshold_respected() {
        let refs = vec![
            aref("Q", "p1", "a.py", 0.9),
            aref("Q", "p2", "b.py", 0.9),
        ];
        assert_eq!(build_duplicate_relations(&refs, 2).len(), 1);
        assert!(build_duplicate_relations(&refs, 3).is_empty());
    }

    /// 边不重复：a→b 与 b→a 只建一条。
    #[test]
    fn duplicate_edges_are_not_symmetric_doubled() {
        let refs = vec![
            aref("X", "p1", "a.py", 0.9),
            aref("X", "p2", "b.py", 0.9),
        ];
        let rels = build_duplicate_relations(&refs, 2);
        assert_eq!(rels.len(), 1);
        let r = &rels[0];
        // 反向不应存在
        assert!(!rels.iter().any(|x| x.source_id == r.target_id && x.target_id == r.source_id));
    }

    /// 大小写不同的同名资产应视为同一实现（跨平台常见）。
    #[test]
    fn duplicate_matching_is_case_insensitive() {
        let refs = vec![
            aref("TaskQueue", "p1", "a.py", 0.9),
            aref("taskqueue", "p2", "b.py", 0.9),
        ];
        assert_eq!(build_duplicate_relations(&refs, 2).len(), 1);
    }

    #[test]
    fn more_projects_means_higher_confidence() {
        let two = vec![aref("X", "p1", "a.py", 0.8), aref("X", "p2", "b.py", 0.8)];
        let four = vec![
            aref("X", "p1", "a.py", 0.8),
            aref("X", "p2", "b.py", 0.8),
            aref("X", "p3", "c.py", 0.8),
            aref("X", "p4", "d.py", 0.8),
        ];
        let c2 = build_duplicate_relations(&two, 2)[0].confidence;
        let c4 = build_duplicate_relations(&four, 2)[0].confidence;
        assert!(c4 > c2, "涉及项目越多应越确信: c2={c2} c4={c4}");
        assert!(c4 <= 0.95, "同名不等于同实现，不得给满分");
    }

    #[test]
    fn empty_refs_yield_no_relations() {
        assert!(build_duplicate_relations(&[], 2).is_empty());
    }

    // ── 端到端：真实源码 → 资产 ─────────────────────────────────────

    /// 用启发式抽取器产出的真实符号走完整链路。
    #[test]
    fn end_to_end_from_source_text() {
        let src = r#"
"""视频服务"""


class VideoPipeline:
    """视频生成管道"""

    def render(self, prompt: str) -> bytes:
        """渲染片段"""
        data = self._encode(prompt)
        return data

    def _encode(self, prompt):
        return prompt.encode()


def tmp_debug_2024():
    pass
"#;
        let syms = HeuristicExtractor.extract("Python", "services/video.py", src);
        let signals = CapabilitySignals {
            symbol_names: syms.iter().map(|s| s.name.clone()).collect(),
            language: Some("Python".into()),
            dependencies: vec!["ffmpeg".into()],
            ..Default::default()
        };
        let out = AssetBuilder::new().build(&input("p_video", &syms, &signals));

        // 实质符号成为资产，一次性代码被丢弃
        let names: Vec<&str> = out.assets.iter().map(|a| a.name.as_str()).collect();
        assert!(names.iter().any(|n| n.contains("VideoPipeline")), "实际: {names:?}");
        assert!(!names.iter().any(|n| n.contains("tmp_debug")), "一次性代码不得入库");
        assert!(out.skipped_symbols >= 1);

        // 每条资产都有充分证据
        for a in &out.assets {
            assert!(a.evidence.is_sufficient(), "资产 {} 缺证据", a.name);
            assert_eq!(a.evidence.files[0], "services/video.py");
        }

        // ffmpeg 应命中 Video Processing 能力并建立关系
        let caps: Vec<&str> = out.capabilities.iter().map(|c| c.name.as_str()).collect();
        assert!(caps.contains(&"Video Processing"), "实际: {caps:?}");
        assert!(out.relations.iter().any(|r| r.relation_type == RelationType::Implements));
        assert!(out.relations.iter().any(|r| r.relation_type == RelationType::Contains));
    }

    #[test]
    fn extraction_serializes() {
        let syms = vec![sig("batch_process", SymbolKind::Function, 40, "src/a.py")];
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &syms, &signals));
        let json = serde_json::to_string(&out).unwrap();
        let back: Extraction = serde_json::from_str(&json).unwrap();
        assert_eq!(back.assets.len(), out.assets.len());
        assert_eq!(back.capabilities.len(), out.capabilities.len());
    }

    #[test]
    fn safe_slug_sanitizes() {
        assert_eq!(safe_slug("VideoPipeline.render"), "videopipeline_render");
        assert_eq!(safe_slug("GET /api/x"), "get__api_x");
        assert_eq!(safe_slug("中文名"), "");
        assert!(safe_slug(&"x".repeat(100)).len() <= 25);
    }

    /// `created_at` 必须由调用方注入而非内部读时钟，
    /// 这样"同输入 → 同输出"才成立，测试才能做精确断言。
    #[test]
    fn created_at_comes_from_input() {
        let syms = vec![sig("batch_process", SymbolKind::Function, 40, "src/a.py")];
        let signals = CapabilitySignals::default();
        let out = AssetBuilder::new().build(&input("p1", &syms, &signals));
        assert_eq!(out.assets[0].created_at, "2026-09-29");

        let mut custom = input("p1", &syms, &signals);
        custom.created_at = "2020-01-01";
        let out2 = AssetBuilder::new().build(&custom);
        assert_eq!(out2.assets[0].created_at, "2020-01-01", "时间戳应随输入变化");
    }

    /// 同输入两次构建必须产出完全一致的结果（幂等，增量扫描的前提）。
    #[test]
    fn build_is_fully_deterministic() {
        let syms = vec![
            sig("batch_process", SymbolKind::Function, 40, "src/a.py"),
            sig("ImagePipeline", SymbolKind::Class, 60, "src/p.py"),
        ];
        let signals = CapabilitySignals {
            dependencies: vec!["diffusers".into()],
            ..Default::default()
        };
        let b = AssetBuilder::new();
        let x = b.build(&input("p1", &syms, &signals));
        let y = b.build(&input("p1", &syms, &signals));
        assert_eq!(
            serde_json::to_string(&x).unwrap(),
            serde_json::to_string(&y).unwrap(),
            "两次构建结果必须逐字节一致"
        );
    }
}
