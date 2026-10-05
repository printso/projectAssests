//! 能力三层结构（《产品设计书》§2）：Domain → Capability → Implementation。
//!
//! 🔴 设计红线：能力**必须**是三层结构，否则会退化成几千个扁平标签，图谱不可用。
//! 本类型用 `layer` + `parent_id` 自引用强制该约束，并在构造入口做校验。

use serde::{Deserialize, Serialize};

/// 能力层级。
///
/// 🔴 派生 `Hash` 是必要的，不是"以防万一"：
/// 图谱的层级筛选器把用户勾选的层级存成 `HashSet<CapabilityLayer>`，
/// 缺 `Hash` 就编译不过。`EntityKind` / `RelationType` / `AssetType` 同理都已派生——
/// 这几个枚举在领域里扮演的角色相同（可枚举、可筛选、可做集合运算），
/// 派生集应当一致，否则下一个用到集合的人还得再改一遍 domain。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLayer {
    /// 领域：AI / Web / Data / Infrastructure / Media
    Domain,
    /// 能力：Image Generation / Agent / RAG / Task Queue
    Capability,
    /// 实现：Qwen / ComfyUI / LangGraph / MCP
    Implementation,
}

impl CapabilityLayer {
    /// 全部层级，**按自顶向下的结构顺序**排列。
    ///
    /// 顺序即图谱层级筛选器与图例的展示顺序：Domain → Capability → Implementation
    /// 与 `parent_id` 的指向一致，用户读起来是"从抽象到具体"。
    pub fn all() -> &'static [CapabilityLayer] {
        &[Self::Domain, Self::Capability, Self::Implementation]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Domain => "domain",
            Self::Capability => "capability",
            Self::Implementation => "implementation",
        }
    }

    /// 兼容原型期数据库中 `category` 列的旧取值。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "domain" => Self::Domain,
            "capability" => Self::Capability,
            "implementation" => Self::Implementation,
            _ => return None,
        })
    }

    /// 中文标签（UI 展示用）。
    ///
    /// 与 `AssetType::label_zh` / `HitKind::label_zh` 保持同一约定：
    /// 展示文案集中在领域层，避免各前端页面各写一份导致口径漂移。
    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Domain => "领域",
            Self::Capability => "能力",
            Self::Implementation => "实现",
        }
    }
}

/// 一个能力节点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub id: String,
    pub name: String,
    pub description: String,
    pub layer: CapabilityLayer,
    /// 父能力 id；Domain 层为 `None`
    pub parent_id: Option<String>,
    /// 0.0-1.0
    pub confidence: f64,
    /// 关联到该能力的项目数（由 relations 聚合，前端用于节点大小）
    pub project_count: u32,
}

impl Capability {
    /// 构造并校验层级不变式：
    /// - Domain 不允许有父节点
    /// - Capability / Implementation 必须有父节点
    ///
    /// 返回 `Err` 而非静默修正——脏数据进图谱比报错更糟（标签爆炸的根因就是无校验）。
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        layer: CapabilityLayer,
        parent_id: Option<String>,
        confidence: f64,
    ) -> Result<Self, CapabilityError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(CapabilityError::EmptyName);
        }
        match layer {
            CapabilityLayer::Domain => {
                if parent_id.is_some() {
                    return Err(CapabilityError::DomainMustBeRoot);
                }
            }
            CapabilityLayer::Capability | CapabilityLayer::Implementation => {
                if parent_id.is_none() {
                    return Err(CapabilityError::MissingParent(layer));
                }
            }
        }
        if !(0.0..=1.0).contains(&confidence) {
            return Err(CapabilityError::ConfidenceOutOfRange(confidence));
        }
        Ok(Self {
            id: id.into(),
            name,
            description: String::new(),
            layer,
            parent_id,
            confidence,
            project_count: 0,
        })
    }

    pub fn is_domain(&self) -> bool {
        self.layer == CapabilityLayer::Domain
    }
}

/// 能力构造错误。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CapabilityError {
    #[error("能力名不能为空")]
    EmptyName,
    #[error("Domain 层能力必须是根节点，不能有父节点")]
    DomainMustBeRoot,
    #[error("{0:?} 层能力必须指定父节点，否则会产生扁平标签")]
    MissingParent(CapabilityLayer),
    #[error("置信度必须在 0.0-1.0 之间，实际为 {0}")]
    ConfidenceOutOfRange(f64),
}

/// 能力树（供图谱与"能力覆盖"条使用）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityTree {
    pub domains: Vec<CapabilityBranch>,
    /// 节点总数（含所有层）
    pub total: usize,
}

/// 能力树的一支：Domain 及其子能力（子能力再带实现）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityBranch {
    pub domain: Capability,
    pub capabilities: Vec<CapabilityLeaf>,
}

/// 能力叶子：Capability 层 + 其 Implementation 层子节点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityLeaf {
    pub capability: Capability,
    pub implementations: Vec<Capability>,
}

impl CapabilityTree {
    /// 从扁平能力列表组装为三层树。
    ///
    /// 孤立节点（父 id 找不到）不会丢进虚空，而是挂到 `orphans` 便于排查数据质量；
    /// 但**不**参与树渲染，避免污染图谱。
    pub fn assemble(mut caps: Vec<Capability>) -> (Self, Vec<Capability>) {
        let total = caps.len();
        let by_id: std::collections::HashMap<String, Capability> = caps
            .drain(..)
            .map(|c| (c.id.clone(), c))
            .collect();

        let mut domains: Vec<&Capability> = by_id
            .values()
            .filter(|c| c.layer == CapabilityLayer::Domain)
            .collect();
        // 按项目数降序，让图谱与列表顺序稳定（同一数据多次渲染结果一致）
        domains.sort_by(|a, b| {
            b.project_count
                .cmp(&a.project_count)
                .then_with(|| a.name.cmp(&b.name))
        });

        let branches = domains
            .into_iter()
            .map(|d| {
                let mut caps: Vec<&Capability> = by_id
                    .values()
                    .filter(|c| {
                        c.layer == CapabilityLayer::Capability && c.parent_id.as_deref() == Some(d.id.as_str())
                    })
                    .collect();
                caps.sort_by(|a, b| {
                    b.project_count
                        .cmp(&a.project_count)
                        .then_with(|| a.name.cmp(&b.name))
                });
                CapabilityBranch {
                    domain: d.clone(),
                    capabilities: caps
                        .into_iter()
                        .map(|c| {
                            let mut impls: Vec<Capability> = by_id
                                .values()
                                .filter(|i| {
                                    i.layer == CapabilityLayer::Implementation
                                        && i.parent_id.as_deref() == Some(c.id.as_str())
                                })
                                .cloned()
                                .collect();
                            impls.sort_by(|a, b| a.name.cmp(&b.name));
                            CapabilityLeaf {
                                capability: c.clone(),
                                implementations: impls,
                            }
                        })
                        .collect(),
                }
            })
            .collect();

        // 孤儿：非 Domain 且父节点不存在
        let orphans: Vec<Capability> = by_id
            .values()
            .filter(|c| {
                c.layer != CapabilityLayer::Domain
                    && c
                        .parent_id
                        .as_ref()
                        .is_none_or(|p| !by_id.contains_key(p))
            })
            .cloned()
            .collect();

        (Self { domains: branches, total }, orphans)
    }
}

/// 能力覆盖度条：项目在各能力上的掌握程度（0-100）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityCoverage {
    pub name: String,
    /// 0-100
    pub pct: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_cannot_have_parent() {
        let r = Capability::new("c1", "AI", CapabilityLayer::Domain, Some("p".into()), 1.0);
        assert_eq!(r.unwrap_err(), CapabilityError::DomainMustBeRoot);
    }

    #[test]
    fn capability_requires_parent() {
        let r = Capability::new("c1", "RAG", CapabilityLayer::Capability, None, 0.8);
        assert_eq!(r.unwrap_err(), CapabilityError::MissingParent(CapabilityLayer::Capability));
    }

    #[test]
    fn empty_name_rejected() {
        let r = Capability::new("c1", "  ", CapabilityLayer::Domain, None, 0.8);
        assert_eq!(r.unwrap_err(), CapabilityError::EmptyName);
    }

    #[test]
    fn confidence_must_be_in_range() {
        let r = Capability::new("c1", "AI", CapabilityLayer::Domain, None, 1.5);
        assert!(matches!(r, Err(CapabilityError::ConfidenceOutOfRange(_))));
    }

    #[test]
    fn valid_three_layer_constructs() {
        let d = Capability::new("ai", "AI", CapabilityLayer::Domain, None, 1.0).unwrap();
        let c = Capability::new("rag", "RAG", CapabilityLayer::Capability, Some("ai".into()), 0.8).unwrap();
        let i = Capability::new("qwen", "Qwen", CapabilityLayer::Implementation, Some("rag".into()), 0.7).unwrap();
        assert!(d.is_domain());
        assert!(!c.is_domain());
        assert_eq!(i.parent_id.as_deref(), Some("rag"));
    }

    #[test]
    fn assemble_builds_three_levels_and_reports_orphans() {
        let caps = vec![
            Capability::new("ai", "AI", CapabilityLayer::Domain, None, 1.0).unwrap(),
            Capability::new("web", "Web", CapabilityLayer::Domain, None, 1.0).unwrap(),
            Capability::new("rag", "RAG", CapabilityLayer::Capability, Some("ai".into()), 0.8).unwrap(),
            Capability::new("qwen", "Qwen", CapabilityLayer::Implementation, Some("rag".into()), 0.7).unwrap(),
            // 孤儿：父节点不存在
            Capability::new("ghost", "Ghost", CapabilityLayer::Capability, Some("missing".into()), 0.5).unwrap(),
        ];
        let (tree, orphans) = CapabilityTree::assemble(caps);
        assert_eq!(tree.total, 5);
        assert_eq!(tree.domains.len(), 2);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].id, "ghost");

        let ai = tree.domains.iter().find(|b| b.domain.id == "ai").unwrap();
        assert_eq!(ai.capabilities.len(), 1);
        assert_eq!(ai.capabilities[0].capability.id, "rag");
        assert_eq!(ai.capabilities[0].implementations.len(), 1);
        assert_eq!(ai.capabilities[0].implementations[0].name, "Qwen");
    }

    #[test]
    fn domains_sort_by_project_count_then_name() {
        let mut a = Capability::new("ai", "AI", CapabilityLayer::Domain, None, 1.0).unwrap();
        let mut b = Capability::new("web", "Web", CapabilityLayer::Domain, None, 1.0).unwrap();
        a.project_count = 3;
        b.project_count = 9;
        let (tree, _) = CapabilityTree::assemble(vec![a, b]);
        assert_eq!(tree.domains[0].domain.id, "web");
    }

    #[test]
    fn layer_roundtrips() {
        for l in [
            CapabilityLayer::Domain,
            CapabilityLayer::Capability,
            CapabilityLayer::Implementation,
        ] {
            assert_eq!(CapabilityLayer::parse(l.as_str()), Some(l));
        }
        assert_eq!(CapabilityLayer::parse("bogus"), None);
    }
}
