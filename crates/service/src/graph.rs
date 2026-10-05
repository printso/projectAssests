//! 知识图谱用例：子图抽取 / 邻域聚焦 / 图例与截断统计。
//!
//! # 这一层唯一真正的难点：规模
//! 真实库里有 156 个项目、数千资产、上万条关系边。全量返回会撑爆浏览器，
//! 而"简单取前 N 条"会产生**悬空边**——边指向一个没被返回的节点，
//! 前端要么报错，要么把边画到画布外的 (0,0) 位置，看起来像图坏了。
//!
//! 因此构造顺序必须是：
//! ```text
//! 1. 选出节点集（按度数/权重排序，不是按数据库返回顺序）
//! 2. 只保留**两端都在节点集内**的边
//! 3. 如实报告被省略了多少节点、多少边、多少悬空边
//! ```
//! 第 2 步是硬不变式，有专门的测试锁定（`no_dangling_edges_*`）。
//!
//! # 为什么按度数选节点而不是按名字/时间
//! 图谱的价值在"看出簇与枢纽"。若按 `ORDER BY name LIMIT 60` 取节点，
//! 得到的是一堆互不相连的孤立点，用户看到的是满屏散点而非结构。
//! 按度数优先保证枢纽节点（被最多边连接的能力/项目）一定入选。
//!
//! # 两种模式
//! - **聚焦模式**（给了 `project_id` / `capability_id`）：中心节点 + 一跳邻域。
//!   这是用户点击节点后的主路径，图小、可读、响应快。
//! - **全景模式**（不给中心）：按度数取枢纽子图，用来看整体结构。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use projectassests_domain::{
    colors, Capability, CapabilityLayer, EntityKind, Project, Relation, RelationType,
};

use crate::context::{ServiceContext, ServiceError};

/// 全景模式默认节点数。
fn default_max_nodes() -> u32 {
    120
}

/// 节点数硬上限。
///
/// 🔴 必须有上限：力导向布局是 O(n²) 的，前端渲染上千节点会直接卡死标签页。
/// 用户传 `limit=100000` 时静默 clamp，而不是让浏览器崩掉。
pub const MAX_NODES: u32 = 400;

/// 默认边数上限。
fn default_max_edges() -> u32 {
    400
}

/// 边数硬上限。
pub const MAX_EDGES: u32 = 1500;

/// 图谱请求。
#[derive(Debug, Clone, Deserialize)]
pub struct GraphRequest {
    /// 聚焦中心：项目 id
    #[serde(default)]
    pub project_id: Option<String>,
    /// 聚焦中心：能力 id（与 `project_id` 同时给时以 project 优先）
    #[serde(default)]
    pub capability_id: Option<String>,
    /// 只保留这些能力层级，逗号分隔：`domain,capability,implementation`
    #[serde(default)]
    pub layers: Option<String>,
    /// 只保留这些关系类型，逗号分隔
    #[serde(default)]
    pub relation_types: Option<String>,
    /// 节点数上限
    #[serde(default = "default_max_nodes")]
    pub max_nodes: u32,
    /// 边数上限
    #[serde(default = "default_max_edges")]
    pub max_edges: u32,
    /// 聚焦模式下是否包含二跳邻居（默认 false：一跳已足够可读）
    #[serde(default)]
    pub expand_neighbors: Option<bool>,
}

impl Default for GraphRequest {
    /// 🔴 手写而非 derive：derive 会让 `max_nodes` / `max_edges` 为 0，
    /// 与 serde 默认值不一致，于是代码里构造的默认请求会返回空图。
    fn default() -> Self {
        Self {
            project_id: None,
            capability_id: None,
            layers: None,
            relation_types: None,
            max_nodes: default_max_nodes(),
            max_edges: default_max_edges(),
            expand_neighbors: None,
        }
    }
}

impl GraphRequest {
    pub fn effective_max_nodes(&self) -> u32 {
        self.max_nodes.clamp(1, MAX_NODES)
    }

    pub fn effective_max_edges(&self) -> u32 {
        self.max_edges.clamp(1, MAX_EDGES)
    }
}

/// 图谱节点。
///
/// `id` 是**复合键**（`"capability:cap_1"`），不是实体原始 id：
/// 项目、资产、能力各有独立的 id 命名空间，理论上可能撞号。
/// 用复合键做边的端点匹配，悬空边检测才是精确的。
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    /// 复合键：`"{kind}:{entity_id}"`
    pub id: String,
    /// 实体原始 id（前端跳转详情用）
    pub entity_id: String,
    pub kind: String,
    pub kind_label: String,
    /// 展示名称
    pub label: String,
    /// 副标题：能力层级 / 项目语言 / 资产路径
    pub subtitle: String,
    /// 十六进制色值（取自 domain 的 `GRAPH_COLORS`，全站图例同源）
    pub color: String,
    pub color_key: String,
    /// 在**本子图内**的边数（不是全库度数）
    pub degree: usize,
    /// 能力的项目数；非能力节点为 0
    pub weight: u32,
    /// 渲染半径建议（px），由 degree 与 weight 派生。
    /// 放在 service 是为了让浏览器端与 Tauri 端尺寸一致。
    pub size: f64,
    /// 是否为聚焦中心
    pub is_center: bool,
    /// 跳转目标页面
    pub link_page: &'static str,
}

/// 图谱边。
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub id: String,
    /// 源节点复合键
    pub source: String,
    /// 目标节点复合键
    pub target: String,
    pub relation: String,
    pub relation_label: String,
    pub confidence: f64,
    /// 支撑该关系的证据条数（0 = 纯结构推断，如 contains）
    pub evidence_count: usize,
    /// 🔴 对称关系（similar_to / combines_with）必须标记：
    /// 画箭头会暗示一个并不存在的方向，用户会误读成"A 依赖 B"。
    pub bidirectional: bool,
    /// 边的颜色（统一取自 GRAPH_COLORS 的 relation 键）
    pub color: String,
}

/// 图例条目（前端渲染色块 + 名称）。
#[derive(Debug, Clone, Serialize)]
pub struct LegendItem {
    pub key: String,
    pub label: String,
    pub color: String,
    /// 本子图中该类型的节点数（0 表示图例里有但当前视图没有）
    pub count: usize,
}

/// 截断与规模统计。
///
/// 🔴 这些数字**必须**如实报告。图谱天生是"整体的一部分"，
/// 不告诉用户"你看到的是 120 / 5230 个节点"，
/// 用户就会把子图当成全貌，进而得出"这些项目之间没有关联"的错误结论。
#[derive(Debug, Clone, Serialize)]
pub struct GraphStats {
    /// 本次返回的节点数
    pub node_count: usize,
    /// 本次返回的边数
    pub edge_count: usize,
    /// 库里的能力总数
    pub total_capabilities: usize,
    /// 库里的关系总数
    pub total_relations: usize,
    /// 是否发生了截断
    pub truncated: bool,
    /// 因节点预算被省略的节点数
    pub omitted_nodes: usize,
    /// 因边预算被省略的边数
    pub omitted_edges: usize,
    /// 因端点不在节点集内被丢弃的边数（悬空边）。
    /// 这不是错误，是子图抽取的必然结果，但必须可见。
    pub dropped_dangling_edges: usize,
    /// 子图内的孤立节点数（没有任何边）
    pub isolated_nodes: usize,
    /// 面向用户的规模说明文案
    pub summary: String,
}

/// 图谱响应。
#[derive(Debug, Clone, Serialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub stats: GraphStats,
    /// 节点类型图例
    pub legend: Vec<LegendItem>,
    /// 关系类型图例（只列子图中实际出现的）
    pub relation_legend: Vec<RelationLegendItem>,
    /// 聚焦模式下的中心节点 id；全景模式为 `None`
    pub center_id: Option<String>,
    /// 空图时的引导
    pub empty_hint: Option<String>,
}

/// 关系类型图例条目。
#[derive(Debug, Clone, Serialize)]
pub struct RelationLegendItem {
    pub relation: String,
    pub label: String,
    pub count: usize,
    pub bidirectional: bool,
}

/// 节点邻域查询（点击节点后拉取它的关联）。
#[derive(Debug, Clone, Serialize)]
pub struct NeighborhoodView {
    pub center: GraphNode,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// 按关系类型分组的邻居数（前端做"实现了 4 / 相似于 2"这样的摘要）
    pub relation_counts: Vec<RelationLegendItem>,
    pub stats: GraphStats,
}

// ══════════════════════════════════════════════════════════════════
// 主入口
// ══════════════════════════════════════════════════════════════════

/// 抽取子图。
pub fn graph(ctx: &ServiceContext, req: &GraphRequest) -> Result<GraphView, ServiceError> {
    let layers = parse_layers(&req.layers)?;
    let rel_filter = parse_relation_types(&req.relation_types)?;

    // 一次性载入：能力与关系都是"数千量级"（见 RelationRepo::list_all 的注释），
    // 全量进内存后在内存里选子图，比反复查库简单且更快。
    let capabilities = ctx.db.capabilities().list_all()?;
    let all_relations = ctx.db.relations().list_all()?;

    // 关系类型筛选必须在选节点之前做：
    // 否则按"全部边"算出的度数会选出一批枢纽，然后边被筛掉，图变成散点。
    let relations: Vec<&Relation> = match &rel_filter {
        Some(wanted) => all_relations
            .iter()
            .filter(|r| wanted.contains(&r.relation_type))
            .collect(),
        None => all_relations.iter().collect(),
    };

    let center = resolve_center(ctx, req)?;
    let max_nodes = req.effective_max_nodes() as usize;
    let max_edges = req.effective_max_edges() as usize;

    let selection = match &center {
        Some(c) => focus_nodes(c, &relations, max_nodes, req.expand_neighbors.unwrap_or(false)),
        None => hub_nodes(&capabilities, &relations, &layers, max_nodes),
    };

    assemble(
        ctx,
        &selection,
        &relations,
        center.as_ref(),
        max_edges,
        all_relations.len(),
    )
}

/// 某节点的邻域（点击节点后的展开视图）。
///
/// 与 `graph` 的区别：这里**一定**以给定 id 为中心，
/// 且返回 `center` 字段让前端能高亮它。
pub fn neighborhood(
    ctx: &ServiceContext,
    entity_id: &str,
    req: &GraphRequest,
) -> Result<NeighborhoodView, ServiceError> {
    let id = entity_id.trim();
    if id.is_empty() {
        return Err(ServiceError::Invalid("实体 id 不能为空".to_string()));
    }
    let rel_filter = parse_relation_types(&req.relation_types)?;
    let relations_all = ctx.db.relations().list_all()?;

    let center_key = locate_entity(ctx, id)?.ok_or_else(|| {
        ServiceError::NotFound(format!(
            "图谱中找不到实体 {id}（可能尚未扫描，或该实体没有任何关联关系）"
        ))
    })?;

    let relations: Vec<&Relation> = match &rel_filter {
        Some(wanted) => relations_all
            .iter()
            .filter(|r| wanted.contains(&r.relation_type))
            .collect(),
        None => relations_all.iter().collect(),
    };

    let max_nodes = req.effective_max_nodes() as usize;
    let selection = focus_nodes(
        &center_key,
        &relations,
        max_nodes,
        req.expand_neighbors.unwrap_or(false),
    );

    let view = assemble(
        ctx,
        &selection,
        &relations,
        Some(&center_key),
        req.effective_max_edges() as usize,
        relations_all.len(),
    )?;

    // 中心节点必须存在（focus_nodes 保证它第一个入选）
    let center_node = view
        .nodes
        .iter()
        .find(|n| n.id == center_key)
        .cloned()
        .ok_or(ServiceError::Internal)?;

    Ok(NeighborhoodView {
        relation_counts: relation_legend(&view.edges),
        center: center_node,
        nodes: view.nodes,
        edges: view.edges,
        stats: view.stats,
    })
}

// ══════════════════════════════════════════════════════════════════
// 节点选取
// ══════════════════════════════════════════════════════════════════

/// 节点选取结果。
///
/// `pool` 是**筛选后本可入选的候选总数**，不是全库实体数。
///
/// 🔴 这个区别决定了 `omitted_nodes` 是否有意义。
/// 早期版本用"能力总数 − 节点数"计算省略量，但节点集里还包含项目，
/// 两个不同口径的数相减得出的值没有解释力——甚至可能为负（被 saturating_sub 悄悄截成 0），
/// 于是"未截断"和"省略了 N 个"这两种情况报出同一个数字。
/// 有了 pool，"省略 = pool − 实际渲染数"才是准确的。
struct Selection {
    /// 选中的节点键（顺序即渲染顺序，必须是确定性的）
    nodes: Vec<String>,
    /// 当前筛选条件下可入选的候选总数
    pool: usize,
}

/// 聚焦模式：中心节点 + 邻域。
///
/// 邻居按**边的置信度**排序入选：预算不够时保留最强的关联，
/// 而不是按数据库返回顺序截断。
fn focus_nodes(
    center: &str,
    relations: &[&Relation],
    max_nodes: usize,
    expand: bool,
) -> Selection {
    let mut selected = vec![center.to_string()];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(center.to_string());

    // 一跳：按置信度降序取邻居
    let mut first_hop: Vec<(f64, String)> = relations
        .iter()
        .filter(|r| r.touches(center))
        .filter_map(|r| r.other_end(center).map(|o| (r.confidence, node_key(o))))
        .collect();
    // 确定性排序：置信度降序 + 键升序兜底（同置信度不能随存储顺序漂移）
    first_hop.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    // 去重后才是真正的候选数：同一邻居可能被多条边连接
    let mut candidates: Vec<String> = Vec::new();
    let mut dedup: HashSet<String> = HashSet::new();
    for (_, key) in first_hop {
        if dedup.insert(key.clone()) {
            candidates.push(key);
        }
    }
    let mut pool = candidates.len() + 1; // +1 = 中心节点自身

    for key in candidates {
        if selected.len() >= max_nodes {
            break;
        }
        if seen.insert(key.clone()) {
            selected.push(key);
        }
    }

    // 二跳（可选）：只在预算还有余量时扩展
    if expand && selected.len() < max_nodes {
        let hop1: Vec<String> = selected.clone();
        let mut second: Vec<(f64, String)> = Vec::new();
        for r in relations {
            for c in &hop1 {
                if r.touches(c)
                    && let Some(o) = r.other_end(c)
                {
                    second.push((r.confidence, node_key(o)));
                }
            }
        }
        second.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        for (_, key) in second {
            if !seen.contains(&key) {
                pool += 1;
            }
            if selected.len() >= max_nodes {
                break;
            }
            if seen.insert(key.clone()) {
                selected.push(key);
            }
        }
    }

    Selection {
        nodes: selected,
        pool,
    }
}

/// 全景模式：按度数选枢纽节点。
///
/// 🔴 按度数而非按名字/时间：图谱的价值在看出簇与枢纽。
/// 按 `ORDER BY name LIMIT 60` 取节点会得到一堆互不相连的散点，
/// 用户看到的是"满屏孤点"而不是结构，进而误以为项目之间没有关联。
fn hub_nodes(
    capabilities: &[Capability],
    relations: &[&Relation],
    layers: &Option<HashSet<CapabilityLayer>>,
    max_nodes: usize,
) -> Selection {
    // 1. 统计每个实体的度数
    let mut degree: HashMap<String, usize> = HashMap::new();
    for r in relations {
        *degree.entry(node_key(&r.source_id)).or_insert(0) += 1;
        *degree.entry(node_key(&r.target_id)).or_insert(0) += 1;
    }

    // 2. 候选集：受层级筛选约束的能力 + 所有出现在边里的项目
    //    资产默认不进全景图（数量太大且大多是叶子节点，加进来只会挤掉枢纽）
    let allowed_layers = layers.as_ref();
    let mut candidates: Vec<(usize, u32, String)> = Vec::new();

    for c in capabilities {
        if let Some(wanted) = allowed_layers
            && !wanted.contains(&c.layer)
        {
            continue;
        }
        let key = node_key(&c.id);
        let deg = degree.get(&key).copied().unwrap_or(0);
        // 🔴 度数为 0 的能力不进全景图。
        //
        // 图谱画的是**关系**：一个没有任何连线的节点飘在画布上，
        // 用户无法判断它是"确实没有关联"还是"渲染坏了"，只会觉得图有问题。
        //
        // 这在关系类型筛选下尤其明显：筛"只看 implements"时，
        // 只有 depends_on 边相连的能力（如 Implementation 层）度数会变成 0，
        // 若仍把它们选进来，画布上就凭空多出几个孤立点，
        // 与"我筛掉了那些关系"的预期完全矛盾。
        //
        // 聚焦模式不受此限制：用户明确点了某个节点，它就是中心，
        // 即使暂时没有边也必须显示（`isolated_nodes` 会如实报告）。
        if deg == 0 {
            continue;
        }
        candidates.push((deg, c.project_count, key));
    }

    // 项目节点：从边里推导（只收 project 端），避免把从未参与任何关系的项目也画进来
    let mut project_degrees: HashMap<String, usize> = HashMap::new();
    for r in relations {
        if r.source_type == EntityKind::Project {
            *project_degrees.entry(r.source_id.clone()).or_insert(0) += 1;
        }
        if r.target_type == EntityKind::Project {
            *project_degrees.entry(r.target_id.clone()).or_insert(0) += 1;
        }
    }
    for (pid, deg) in project_degrees {
        candidates.push((deg, 0, node_key(&pid)));
    }

    // 3. 排序：度数降序 → 权重降序 → 键升序（确定性兜底）
    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let pool = candidates.len();
    let nodes = candidates
        .into_iter()
        .take(max_nodes)
        .map(|(_, _, key)| key)
        .collect();
    Selection { nodes, pool }
}

// ══════════════════════════════════════════════════════════════════
// 组装
// ══════════════════════════════════════════════════════════════════

fn assemble(
    ctx: &ServiceContext,
    selection: &Selection,
    relations: &[&Relation],
    center: Option<&String>,
    max_edges: usize,
    total_relations: usize,
) -> Result<GraphView, ServiceError> {
    let selected = &selection.nodes;
    let node_set: HashSet<&str> = selected.iter().map(|s| s.as_str()).collect();

    // 🔴 硬不变式：只保留**两端都在节点集内**的边。
    // 少了这一步，前端会拿到指向不存在节点的边，
    // 渲染时要么抛错，要么把线画到画布外的 (0,0)——看起来像"图坏了"。
    let mut kept: Vec<&Relation> = Vec::new();
    let mut dangling = 0usize;
    for r in relations {
        let s = node_key(&r.source_id);
        let t = node_key(&r.target_id);
        if node_set.contains(s.as_str()) && node_set.contains(t.as_str()) {
            kept.push(r);
        } else {
            dangling += 1;
        }
    }

    // 边预算：按置信度降序保留（确定性兜底用 id）
    kept.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    let omitted_edges = kept.len().saturating_sub(max_edges);
    kept.truncate(max_edges);

    // 子图内真实度数（用于节点尺寸）——必须在边截断之后算，
    // 否则尺寸会反映"本来有多少边"而不是"画出来有多少边"
    let mut degree: HashMap<String, usize> = HashMap::new();
    for r in &kept {
        *degree.entry(node_key(&r.source_id)).or_insert(0) += 1;
        *degree.entry(node_key(&r.target_id)).or_insert(0) += 1;
    }

    let labels = resolve_labels(ctx, selected)?;
    let weights = capability_weights(ctx, selected)?;

    let mut nodes: Vec<GraphNode> = Vec::with_capacity(selected.len());
    for key in selected {
        let Some(l) = labels.get(key.as_str()) else {
            // 边里出现过但实体已被删除：跳过该节点。
            // 它不会有边（边已被上面的两端校验过滤），画出来就是个孤立空点。
            continue;
        };
        let deg = degree.get(key).copied().unwrap_or(0);
        let weight = weights.get(l.entity_id.as_str()).copied().unwrap_or(0);
        let color_key = l.kind.color_key();
        nodes.push(GraphNode {
            id: key.clone(),
            entity_id: l.entity_id.clone(),
            kind: l.kind.as_str().to_string(),
            kind_label: l.kind.label_zh().to_string(),
            label: l.label.clone(),
            subtitle: l.subtitle.clone(),
            color: colors::for_key(color_key).to_string(),
            color_key: color_key.to_string(),
            degree: deg,
            weight,
            size: node_size(deg, weight),
            is_center: center.is_some_and(|c| c == key),
            link_page: link_page_for(l.kind),
        });
    }

    let edges: Vec<GraphEdge> = kept
        .iter()
        .map(|r| GraphEdge {
            id: r.id.clone(),
            source: node_key(&r.source_id),
            target: node_key(&r.target_id),
            relation: r.relation_type.as_str().to_string(),
            relation_label: r.relation_type.label_zh().to_string(),
            confidence: r.confidence,
            evidence_count: r.evidence.len(),
            bidirectional: is_symmetric(r.relation_type),
            color: colors::for_key("relation").to_string(),
        })
        .collect();

    let isolated = nodes.iter().filter(|n| n.degree == 0).count();
    // 🔴 omitted 用"候选池 − 实际渲染"，不是"全库能力数 − 节点数"：
    // 节点集里含项目，与能力总数是两个口径，相减得出的数没有解释力。
    // 这里同时涵盖两种省略：预算不足未入选、以及实体已删除无法解析标签。
    let omitted_nodes = selection.pool.saturating_sub(nodes.len());
    let total_capabilities = ctx.db.capabilities().count()?;
    let stats = GraphStats {
        node_count: nodes.len(),
        edge_count: edges.len(),
        total_capabilities,
        total_relations,
        truncated: omitted_edges > 0 || omitted_nodes > 0,
        omitted_nodes,
        omitted_edges,
        dropped_dangling_edges: dangling,
        isolated_nodes: isolated,
        summary: String::new(),
    };

    let legend = build_legend(&nodes);
    let relation_legend = relation_legend(&edges);
    let empty_hint = empty_hint(&nodes, total_relations);
    let summary = stats_summary(&stats, center.is_some());

    Ok(GraphView {
        nodes,
        edges,
        stats: GraphStats { summary, ..stats },
        legend,
        relation_legend,
        center_id: center.cloned(),
        empty_hint,
    })
}

/// 节点半径建议（px）。
///
/// 对数缩放而非线性：图谱的度数是长尾分布，
/// 线性映射会让唯一的枢纽节点撑满画布、其余全部小成点。
fn node_size(degree: usize, weight: u32) -> f64 {
    let signal = degree.max(weight as usize) as f64;
    let scaled = 9.0 + (signal.max(1.0).ln() * 7.0);
    // 🔴 必须量化到 0.1px，不能返回裸浮点。
    //
    // 原始实现是 `.round() * 10.0 / 10.0`——意图对但算错了：
    // `round()` 已经把值变成整数，再乘除 10 等于什么都没做，
    // 于是响应里出现 `18.704060527839232` 这样 17 位小数的半径值。
    //
    // 危害不只是难看：这份 JSON 会进快照测试与前端缓存比对，
    // 浮点尾数随平台/libm 版本漂移时，diff 里全是噪音，
    // 真正变化的字段反而看不出来。而半径本身是像素值，
    // 亚像素精度没有任何渲染意义（浏览器也画不出 0.704 像素的差别）。
    //
    // 正确写法是先放大、取整、再缩小。
    (scaled.clamp(9.0, 34.0) * 10.0).round() / 10.0
}

/// 关系是否对称。
///
/// 🔴 对称关系画箭头是**语义错误**：`similar_to` 表示"两者相似"，
/// 箭头会让用户读成"A 依赖 B"或"A 派生出 B"。
fn is_symmetric(rt: RelationType) -> bool {
    matches!(
        rt,
        RelationType::SimilarTo | RelationType::CombinesWith | RelationType::CanCombineWith | RelationType::RelatedTo
    )
}

fn link_page_for(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Project => "project",
        EntityKind::Asset => "assets",
        EntityKind::Capability => "graph",
        _ => "graph",
    }
}

fn node_key(entity_id: &str) -> String {
    // 复合键的 kind 部分在选点阶段未知（只有 id），
    // 这里统一用 id 本身作键，kind 由 resolve_labels 补。
    // 🔴 之所以仍保留 node_key 这层间接：将来若发现 id 撞号，
    // 只需改这一个函数（改成 "kind:id"），所有调用点自动生效。
    entity_id.to_string()
}

/// 批量解析节点标签。
///
/// 返回 `None` 的项表示实体已不存在（被删除或从未入库），
/// 调用方必须跳过——不能渲染成一个没有名字的空节点。
/// 节点的展示信息（由 `resolve_labels` 批量解析）。
///
/// 🔴 用结构体而非 `(EntityKind, String, String, String)` 四元组：
/// 调用点写 `labels.1` / `labels.2` 时根本看不出哪个是名称、哪个是副标题，
/// 而四元组的字段顺序一旦调换，编译器不会报错（三个都是 String），
/// 只会在界面上把"项目名"和"语言"对调显示。
struct NodeLabel {
    kind: EntityKind,
    entity_id: String,
    label: String,
    subtitle: String,
}

fn resolve_labels(
    ctx: &ServiceContext,
    selected: &[String],
) -> Result<HashMap<String, NodeLabel>, ServiceError> {
    let mut out = HashMap::new();

    // 能力一次性全取（数量可控），避免逐个 get
    let caps: HashMap<String, Capability> = ctx
        .db
        .capabilities()
        .list_all()?
        .into_iter()
        .map(|c| (c.id.clone(), c))
        .collect();

    for key in selected {
        if let Some(c) = caps.get(key.as_str()) {
            out.insert(
                key.clone(),
                NodeLabel {
                    kind: EntityKind::Capability,
                    entity_id: c.id.clone(),
                    label: c.name.clone(),
                    subtitle: c.layer.label_zh().to_string(),
                },
            );
            continue;
        }
        // 不是能力，试项目
        if let Some(p) = ctx.db.projects().get(key)? {
            out.insert(
                key.clone(),
                NodeLabel {
                    kind: EntityKind::Project,
                    entity_id: p.id.clone(),
                    label: p.name.clone(),
                    subtitle: project_subtitle(&p),
                },
            );
            continue;
        }
        // 再试资产
        if let Some(a) = ctx.db.assets().get(key)? {
            out.insert(
                key.clone(),
                NodeLabel {
                    kind: EntityKind::Asset,
                    entity_id: a.id.clone(),
                    label: a.name.clone(),
                    subtitle: a.source_path.clone(),
                },
            );
        }
        // 都找不到：不插入，调用方跳过该节点
    }
    Ok(out)
}

fn project_subtitle(p: &Project) -> String {
    if p.framework.is_empty() || p.framework == "-" {
        p.language.clone()
    } else {
        format!("{} · {}", p.language, p.framework)
    }
}

/// 能力的项目数（节点权重）。
fn capability_weights(
    ctx: &ServiceContext,
    selected: &[String],
) -> Result<HashMap<String, u32>, ServiceError> {
    // 一次性建集合：selected 可达 MAX_NODES(400)，能力可达数千，
    // 对每个能力线性扫一遍 selected 是 O(n·m)，会明显拖慢大图渲染。
    let wanted: HashSet<&str> = selected.iter().map(|s| s.as_str()).collect();
    let mut out = HashMap::new();
    for c in ctx.db.capabilities().list_all()? {
        if wanted.contains(c.id.as_str()) {
            out.insert(c.id, c.project_count);
        }
    }
    Ok(out)
}

/// 定位实体并返回其节点键。
///
/// 依次试项目 → 能力 → 资产：前端点击节点时只知道 id，不知道类型。
fn locate_entity(ctx: &ServiceContext, id: &str) -> Result<Option<String>, ServiceError> {
    if ctx.db.projects().get(id)?.is_some() {
        return Ok(Some(node_key(id)));
    }
    if ctx.db.capabilities().get(id)?.is_some() {
        return Ok(Some(node_key(id)));
    }
    if ctx.db.assets().get(id)?.is_some() {
        return Ok(Some(node_key(id)));
    }
    // 实体不在三张表里，但可能仍出现在关系中（如 knowledge/experience 节点）。
    // 只要有任何一条边涉及它，就认为它在图里。
    if !ctx.db.relations().touching(id)?.is_empty() {
        return Ok(Some(node_key(id)));
    }
    Ok(None)
}

/// 解析聚焦中心。
fn resolve_center(ctx: &ServiceContext, req: &GraphRequest) -> Result<Option<String>, ServiceError> {
    if let Some(pid) = non_empty(&req.project_id) {
        // 🔴 中心节点不存在必须报错，不能静默降级成全景图：
        // 用户点了某个项目却看到一张完全不同的全局图，会以为点错了。
        if ctx.db.projects().get(&pid)?.is_none() {
            return Err(ServiceError::NotFound(format!("项目 {pid}")));
        }
        return Ok(Some(node_key(&pid)));
    }
    if let Some(cid) = non_empty(&req.capability_id) {
        if ctx.db.capabilities().get(&cid)?.is_none() {
            return Err(ServiceError::NotFound(format!("能力 {cid}")));
        }
        return Ok(Some(node_key(&cid)));
    }
    Ok(None)
}

fn build_legend(nodes: &[GraphNode]) -> Vec<LegendItem> {
    // 图例顺序固定取自 EntityKind::all()：
    // 若按"当前有节点的类型"生成，图例会在切换筛选时跳来跳去。
    EntityKind::all()
        .iter()
        .map(|k| LegendItem {
            key: k.as_str().to_string(),
            label: k.label_zh().to_string(),
            color: colors::for_key(k.color_key()).to_string(),
            count: nodes.iter().filter(|n| n.kind == k.as_str()).count(),
        })
        .collect()
}

fn relation_legend(edges: &[GraphEdge]) -> Vec<RelationLegendItem> {
    // 只列子图里实际出现的关系类型：17 种关系全列出来会有 17 行图例，
    // 而当前视图通常只有 2-3 种，其余全是 0，纯属噪音。
    let mut seen: Vec<(String, String, bool, usize)> = Vec::new();
    for e in edges {
        if let Some(item) = seen.iter_mut().find(|s| s.0 == e.relation) {
            item.3 += 1;
        } else {
            seen.push((
                e.relation.clone(),
                e.relation_label.clone(),
                e.bidirectional,
                1,
            ));
        }
    }
    // 按数量降序、名称升序（确定性）
    seen.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.1.cmp(&b.1)));
    seen.into_iter()
        .map(|(relation, label, bidirectional, count)| RelationLegendItem {
            relation,
            label,
            count,
            bidirectional,
        })
        .collect()
}

fn stats_summary(s: &GraphStats, focused: bool) -> String {
    let mut parts = vec![format!(
        "显示 {} 个节点、{} 条关系",
        s.node_count, s.edge_count
    )];
    if s.dropped_dangling_edges > 0 {
        parts.push(format!(
            "另有 {} 条关系的一端不在当前视图内，已省略",
            s.dropped_dangling_edges
        ));
    }
    if s.omitted_edges > 0 {
        parts.push(format!("因边数上限省略 {} 条", s.omitted_edges));
    }
    if !focused && s.omitted_nodes > 0 {
        parts.push(format!("共 {} 个能力，仅显示关联最密集的节点", s.total_capabilities));
    }
    if s.isolated_nodes > 0 {
        parts.push(format!("{} 个孤立节点", s.isolated_nodes));
    }
    parts.join("；")
}

/// 空图引导。
///
/// 🔴 必须区分三种情况，它们对应完全不同的用户动作：
/// - 库是空的 → 去扫描
/// - 有数据但筛选太严 → 放宽筛选
/// - 有数据但没有任何关系 → 说明还没跑过关系分析
fn empty_hint(nodes: &[GraphNode], total_relations: usize) -> Option<String> {
    if !nodes.is_empty() {
        return None;
    }
    // 边不需要单独判：节点集为空时边集必然为空（两端都得在节点集内）
    if total_relations == 0 {
        return Some(
            "图谱还是空的：关系边由跨项目分析生成，请先完成一次扫描并生成洞察。".to_string(),
        );
    }
    Some("当前筛选下没有节点。试着放宽层级或关系类型筛选。".to_string())
}

// ══════════════════════════════════════════════════════════════════
// 参数解析
// ══════════════════════════════════════════════════════════════════

fn parse_layers(s: &Option<String>) -> Result<Option<HashSet<CapabilityLayer>>, ServiceError> {
    let Some(raw) = non_empty(s) else {
        return Ok(None);
    };
    let mut out = HashSet::new();
    for part in raw.split(',') {
        let v = part.trim();
        if v.is_empty() {
            continue;
        }
        let layer = CapabilityLayer::parse(v).ok_or_else(|| {
            ServiceError::Invalid(format!(
                "未知的能力层级：{v}（可选 domain / capability / implementation）"
            ))
        })?;
        out.insert(layer);
    }
    // 只给了逗号和空白（如 " , "）等价于没给
    if out.is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

fn parse_relation_types(s: &Option<String>) -> Result<Option<HashSet<RelationType>>, ServiceError> {
    let Some(raw) = non_empty(s) else {
        return Ok(None);
    };
    let mut out = HashSet::new();
    for part in raw.split(',') {
        let v = part.trim();
        if v.is_empty() {
            continue;
        }
        let rt = RelationType::parse(v).ok_or_else(|| {
            ServiceError::Invalid(format!(
                "未知的关系类型：{v}（可选 {}）",
                RelationType::all()
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ))
        })?;
        out.insert(rt);
    }
    if out.is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectassests_domain::{CodeStats, ProjectStatus, ScanFacts};

    fn ctx() -> ServiceContext {
        ServiceContext::in_memory().unwrap()
    }

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("/tmp/{id}"),
            description: String::new(),
            language: "Rust".into(),
            framework: "Axum".into(),
            created_at: None,
            updated_at: Some("2026-09-20".into()),
            last_commit_at: None,
            status: ProjectStatus::Active,
            health_score: 70,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats::default(),
            scan: ScanFacts::default(),
            ai_profile: None,
        }
    }

    /// 能力必须走 `Capability::new`：它校验层级不变式
    /// （Domain 不能有父节点，Capability/Implementation 必须有）。
    fn cap(id: &str, name: &str, layer: CapabilityLayer, parent: Option<&str>) -> Capability {
        Capability::new(id, name, layer, parent.map(str::to_string), 0.9).unwrap()
    }

    fn rel(
        id: &str,
        src: &str,
        src_kind: EntityKind,
        rt: RelationType,
        dst: &str,
        dst_kind: EntityKind,
        conf: f64,
    ) -> Relation {
        Relation::new(id, src, src_kind, rt, dst, dst_kind, conf)
    }

    /// 一个有真实结构的小图：
    /// domain(ai) ← capability(queue) ← implementation(tokio)
    /// p1/p2 都 implements queue，p1 contains a1
    fn seeded() -> ServiceContext {
        let c = ctx();
        c.db
            .projects()
            .upsert_batch(&[project("p1", "视频平台"), project("p2", "分身云栖")])
            .unwrap();
        c.db
            .capabilities()
            .upsert_batch(&[
                cap("d_ai", "AI", CapabilityLayer::Domain, None),
                cap("c_queue", "任务队列", CapabilityLayer::Capability, Some("d_ai")),
                cap("i_tokio", "Tokio", CapabilityLayer::Implementation, Some("c_queue")),
            ])
            .unwrap();
        c.db
            .relations()
            .upsert_batch(&[
                rel("r1", "c_queue", EntityKind::Capability, RelationType::DependsOn, "d_ai", EntityKind::Capability, 0.9),
                rel("r2", "i_tokio", EntityKind::Capability, RelationType::DependsOn, "c_queue", EntityKind::Capability, 0.8),
                rel("r3", "p1", EntityKind::Project, RelationType::Implements, "c_queue", EntityKind::Capability, 0.95),
                rel("r4", "p2", EntityKind::Project, RelationType::Implements, "c_queue", EntityKind::Capability, 0.85),
                rel("r5", "p1", EntityKind::Project, RelationType::SimilarTo, "p2", EntityKind::Project, 0.7),
            ])
            .unwrap();
        c
    }

    // ── 默认值一致性 ────────────────────────────────────────────

    #[test]
    fn default_request_matches_serde_default() {
        // 🔴 回归：derive(Default) 会让 max_nodes=0 → 空图
        let built = GraphRequest::default();
        let parsed: GraphRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(built.effective_max_nodes(), parsed.effective_max_nodes());
        assert_eq!(built.effective_max_nodes(), 120);
        assert_eq!(built.effective_max_edges(), 400);
    }

    #[test]
    fn limits_are_clamped() {
        let huge = GraphRequest { max_nodes: 99999, max_edges: 99999, ..Default::default() };
        assert_eq!(huge.effective_max_nodes(), MAX_NODES);
        assert_eq!(huge.effective_max_edges(), MAX_EDGES);
        let zero = GraphRequest { max_nodes: 0, max_edges: 0, ..Default::default() };
        assert_eq!(zero.effective_max_nodes(), 1);
        assert_eq!(zero.effective_max_edges(), 1);
    }

    // ── 悬空边（核心不变式）────────────────────────────────────

    #[test]
    fn no_dangling_edges_in_overview() {
        let c = seeded();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        let ids: HashSet<&str> = v.nodes.iter().map(|n| n.id.as_str()).collect();
        for e in &v.edges {
            // 🔴 核心不变式：边的两端都必须是返回的节点
            assert!(ids.contains(e.source.as_str()), "悬空源节点: {:?}", e.source);
            assert!(ids.contains(e.target.as_str()), "悬空目标节点: {:?}", e.target);
        }
    }

    #[test]
    fn no_dangling_edges_when_nodes_are_truncated() {
        let c = seeded();
        // 强行把节点预算压到 2：边必须随之被丢弃，而不是指向不存在的节点
        let v = graph(
            &c,
            &GraphRequest {
                max_nodes: 2,
                max_edges: 100,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(v.nodes.len() <= 2, "应遵守节点预算，实得 {}", v.nodes.len());
        let ids: HashSet<&str> = v.nodes.iter().map(|n| n.id.as_str()).collect();
        for e in &v.edges {
            assert!(ids.contains(e.source.as_str()));
            assert!(ids.contains(e.target.as_str()));
        }
        assert!(
            v.stats.dropped_dangling_edges > 0,
            "被截断后应如实报告丢弃了多少悬空边: {:?}",
            v.stats
        );
    }

    #[test]
    fn no_dangling_edges_in_focus_mode() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                project_id: Some("p1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let ids: HashSet<&str> = v.nodes.iter().map(|n| n.id.as_str()).collect();
        for e in &v.edges {
            assert!(ids.contains(e.source.as_str()));
            assert!(ids.contains(e.target.as_str()));
        }
        assert!(ids.contains("p1"), "中心节点必须在图里");
    }

    // ── 截断的诚实报告 ──────────────────────────────────────────

    #[test]
    fn stats_report_real_totals() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                max_nodes: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.stats.total_capabilities, 3);
        assert_eq!(v.stats.total_relations, 5);
        assert!(v.stats.truncated, "节点被截断时必须标记");
        // 🔴 不告诉用户"你看到的是一部分"，用户会把子图当全貌，
        // 进而得出"这些项目之间没关联"的错误结论
        assert!(v.stats.summary.contains("显示"), "{:?}", v.stats.summary);
    }

    #[test]
    fn full_view_is_not_marked_truncated() {
        let c = seeded();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        assert!(!v.stats.truncated, "全部装得下时不该说被截断: {:?}", v.stats);
        assert_eq!(v.stats.omitted_edges, 0);
        assert_eq!(v.stats.dropped_dangling_edges, 0);
        assert_eq!(v.stats.node_count, 5, "2 项目 + 3 能力");
        assert_eq!(v.stats.edge_count, 5);
    }

    #[test]
    fn summary_mentions_omitted_edges_when_edge_budget_hits() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                max_edges: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.edges.len(), 2);
        assert_eq!(v.stats.omitted_edges, 3);
        assert!(v.stats.summary.contains("边数上限"), "{:?}", v.stats.summary);
    }

    // ── 节点选取策略 ────────────────────────────────────────────

    #[test]
    fn hubs_are_preferred_over_arbitrary_nodes() {
        let c = seeded();
        // c_queue 度数最高（4 条边），必须入选；预算只给 1 个节点
        let v = graph(
            &c,
            &GraphRequest {
                max_nodes: 1,
                max_edges: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.nodes.len(), 1);
        // 🔴 按度数选枢纽，而不是按数据库返回顺序取第一个
        assert_eq!(v.nodes[0].entity_id, "c_queue", "应选中连接最多的枢纽节点");
    }

    #[test]
    fn isolated_nodes_are_reported() {
        let c = seeded();
        // 预算只给 2 个节点：c_queue 与其最强邻居入选，第三个若入选则无边
        let v = graph(
            &c,
            &GraphRequest {
                max_nodes: 1,
                max_edges: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.stats.isolated_nodes, 1, "唯一节点没有任何边，应报告为孤立");
    }

    #[test]
    fn layer_filter_limits_capabilities() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                layers: Some("domain".into()),
                ..Default::default()
            },
        )
        .unwrap();
        // 只有 domain 层能力可作为候选；项目仍可从边里进来
        let caps: Vec<_> = v.nodes.iter().filter(|n| n.kind == "capability").collect();
        assert!(
            caps.iter().all(|n| n.subtitle == "领域"),
            "应只剩 domain 层: {:?}",
            caps.iter().map(|n| (&n.entity_id, &n.subtitle)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn relation_type_filter_is_applied_before_node_selection() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                relation_types: Some("implements".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(v.edges.iter().all(|e| e.relation == "implements"));
        // 🔴 关键：筛选必须在选点之前生效。若先按全部边算度数再筛边，
        // 会选出一批枢纽然后边被筛掉，图变成散点。
        // 这里 p1/p2/c_queue 都因 implements 边入选，不该有孤立点。
        assert_eq!(v.stats.isolated_nodes, 0, "{:?}", v.stats);
    }

    // ── 对称关系 ────────────────────────────────────────────────

    #[test]
    fn symmetric_relations_are_marked_bidirectional() {
        let c = seeded();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        let similar = v.edges.iter().find(|e| e.relation == "similar_to").unwrap();
        // 🔴 similar_to 画箭头会让用户读成"p1 依赖 p2"，这是语义错误
        assert!(similar.bidirectional);

        let implements = v.edges.iter().find(|e| e.relation == "implements").unwrap();
        assert!(!implements.bidirectional, "implements 有明确方向");
    }

    // ── 聚焦模式 ────────────────────────────────────────────────

    #[test]
    fn focus_mode_centers_on_given_project() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                project_id: Some("p1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.center_id.as_deref(), Some("p1"));
        let center = v.nodes.iter().find(|n| n.entity_id == "p1").unwrap();
        assert!(center.is_center);
        assert_eq!(center.label, "视频平台");
        assert_eq!(center.link_page, "project");
        // 其他节点不该被标成中心
        assert!(v.nodes.iter().filter(|n| n.is_center).count() == 1);
    }

    #[test]
    fn focus_on_unknown_project_is_404_not_silent_fallback() {
        let c = seeded();
        // 🔴 静默降级成全景图的话，用户点了项目却看到完全不同的图，会以为点错了
        let err = graph(
            &c,
            &GraphRequest {
                project_id: Some("ghost".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)), "{err:?}");
        assert_eq!(err.status_code(), 404);
    }

    #[test]
    fn focus_on_capability_works() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                capability_id: Some("c_queue".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.center_id.as_deref(), Some("c_queue"));
        // c_queue 连着 d_ai / i_tokio / p1 / p2
        assert_eq!(v.nodes.len(), 5);
    }

    #[test]
    fn focus_on_unknown_capability_is_404() {
        let c = seeded();
        let err = graph(
            &c,
            &GraphRequest {
                capability_id: Some("ghost".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[test]
    fn project_id_wins_over_capability_id() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                project_id: Some("p1".into()),
                capability_id: Some("c_queue".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.center_id.as_deref(), Some("p1"), "同时给时以 project 优先");
    }

    #[test]
    fn expand_neighbors_adds_second_hop() {
        let c = seeded();
        // 一跳：p1 → c_queue, p2
        let one_hop = graph(
            &c,
            &GraphRequest {
                project_id: Some("p1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        // 二跳：再加上 d_ai, i_tokio
        let two_hop = graph(
            &c,
            &GraphRequest {
                project_id: Some("p1".into()),
                expand_neighbors: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            two_hop.nodes.len() > one_hop.nodes.len(),
            "二跳应带来更多节点: {} vs {}",
            two_hop.nodes.len(),
            one_hop.nodes.len()
        );
        assert!(two_hop.nodes.iter().any(|n| n.entity_id == "d_ai"));
    }

    // ── neighborhood ────────────────────────────────────────────

    #[test]
    fn neighborhood_returns_center_and_relation_counts() {
        let c = seeded();
        let n = neighborhood(&c, "c_queue", &GraphRequest::default()).unwrap();
        assert_eq!(n.center.entity_id, "c_queue");
        assert!(n.center.is_center);
        assert!(!n.relation_counts.is_empty());
        let implements = n
            .relation_counts
            .iter()
            .find(|r| r.relation == "implements")
            .unwrap();
        assert_eq!(implements.count, 2, "两个项目实现了它");
    }

    #[test]
    fn neighborhood_of_unknown_entity_is_404_with_guidance() {
        let c = seeded();
        let err = neighborhood(&c, "ghost", &GraphRequest::default()).unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
        // 提示要说明可能原因，而不只是"不存在"
        assert!(err.to_string().contains("扫描") || err.to_string().contains("关联"), "{err}");
    }

    #[test]
    fn neighborhood_blank_id_is_invalid() {
        let c = seeded();
        assert!(matches!(
            neighborhood(&c, "   ", &GraphRequest::default()).unwrap_err(),
            ServiceError::Invalid(_)
        ));
    }

    // ── 节点视图字段 ────────────────────────────────────────────

    #[test]
    fn nodes_carry_color_label_and_link() {
        let c = seeded();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        for n in &v.nodes {
            assert!(!n.label.is_empty(), "{n:?} 缺名称");
            assert!(!n.kind_label.is_empty(), "{n:?} 缺类型中文标签");
            assert!(n.color.starts_with('#'), "{n:?} 色值非法");
            assert!(!n.link_page.is_empty());
            assert!(n.size >= 9.0 && n.size <= 34.0, "{n:?} 尺寸越界");
        }
        let p1 = v.nodes.iter().find(|n| n.entity_id == "p1").unwrap();
        assert_eq!(p1.kind, "project");
        assert_eq!(p1.kind_label, "项目");
        assert_eq!(p1.subtitle, "Rust · Axum");
    }

    #[test]
    fn colors_come_from_the_shared_palette() {
        let c = seeded();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        let p1 = v.nodes.iter().find(|n| n.entity_id == "p1").unwrap();
        // 🔴 色值必须来自 domain 的 GRAPH_COLORS，不能各页硬编码：
        // 首页图例与图谱页若用不同绿色，用户会以为是两类东西
        assert_eq!(p1.color, colors::for_key("project"));
        assert_eq!(p1.color_key, "project");
        assert!(v.edges.iter().all(|e| e.color == colors::for_key("relation")));
    }

    #[test]
    fn node_size_scales_with_degree() {
        let c = seeded();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        let by_id = |id: &str| v.nodes.iter().find(|n| n.entity_id == id).unwrap();
        // c_queue 度数最高，尺寸应最大
        assert!(
            by_id("c_queue").size > by_id("i_tokio").size,
            "枢纽节点应更大: {:?} vs {:?}",
            by_id("c_queue"),
            by_id("i_tokio")
        );
        assert_eq!(by_id("c_queue").degree, 4);
    }

    #[test]
    fn node_size_is_bounded_for_extreme_degree() {
        // 长尾分布下线性映射会让枢纽撑满画布，必须 clamp
        assert_eq!(node_size(0, 0), 9.0);
        assert_eq!(node_size(1000, 0), 34.0);
        assert_eq!(node_size(100000, 0), 34.0);
    }

    #[test]
    fn capability_weight_is_exposed() {
        let c = seeded();
        c.db.capabilities().refresh_project_counts().unwrap();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        let q = v.nodes.iter().find(|n| n.entity_id == "c_queue").unwrap();
        assert_eq!(q.weight, 2, "两个项目实现了它");
    }

    // ── 图例 ────────────────────────────────────────────────────

    #[test]
    fn legend_always_lists_all_kinds() {
        let c = seeded();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        // 🔴 图例顺序固定，且不随筛选消失：按"当前有节点的类型"生成的话，
        // 图例会在切换筛选时跳动，用户无法建立稳定的颜色记忆
        assert_eq!(v.legend.len(), EntityKind::all().len());
        let order: Vec<&str> = v.legend.iter().map(|l| l.key.as_str()).collect();
        let expected: Vec<&str> = EntityKind::all().iter().map(|k| k.as_str()).collect();
        assert_eq!(order, expected);
        // 计数为 0 的类型也在（knowledge/experience/…）
        assert!(v.legend.iter().any(|l| l.key == "knowledge" && l.count == 0));
        let cap = v.legend.iter().find(|l| l.key == "capability").unwrap();
        assert_eq!(cap.count, 3);
    }

    #[test]
    fn relation_legend_only_lists_present_types() {
        let c = seeded();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        // 17 种关系全列出来会有 17 行图例，当前视图只有 3 种
        assert_eq!(v.relation_legend.len(), 3, "{:?}", v.relation_legend);
        assert!(v.relation_legend.iter().all(|r| r.count > 0));
        let sim = v.relation_legend.iter().find(|r| r.relation == "similar_to").unwrap();
        assert!(sim.bidirectional);
    }

    // ── 空状态 ──────────────────────────────────────────────────

    #[test]
    fn empty_db_hint_points_to_scanning() {
        let c = ctx();
        let v = graph(&c, &GraphRequest::default()).unwrap();
        assert!(v.nodes.is_empty());
        assert!(v.edges.is_empty());
        let hint = v.empty_hint.expect("空库应有引导");
        assert!(hint.contains("扫描"), "{hint}");
        // 空图返回 Ok 而非报错，前端才能渲染引导页
    }

    #[test]
    fn relations_exist_but_filtered_out_hints_at_loosening() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                relation_types: Some("derived_from".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(v.nodes.is_empty());
        let hint = v.empty_hint.expect("应有引导");
        // 🔴 有数据但筛选太严 ≠ 库是空的，两者动作完全不同
        assert!(hint.contains("筛选"), "{hint}");
        assert!(!hint.contains("扫描"), "{hint}");
    }

    // ── 参数校验 ────────────────────────────────────────────────

    #[test]
    fn unknown_layer_is_rejected_with_options() {
        let c = seeded();
        let err = graph(
            &c,
            &GraphRequest {
                layers: Some("domain,bogus".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bogus"), "{msg}");
        assert!(msg.contains("implementation"), "应列出可选值: {msg}");
    }

    #[test]
    fn unknown_relation_type_is_rejected_with_options() {
        let c = seeded();
        let err = graph(
            &c,
            &GraphRequest {
                relation_types: Some("loves".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("loves"), "{msg}");
        assert!(msg.contains("implements"), "应列出可选值: {msg}");
    }

    #[test]
    fn blank_and_comma_only_filters_are_ignored() {
        let c = seeded();
        // " , " 等价于没给筛选，不该报错也不该返回空图
        let v = graph(
            &c,
            &GraphRequest {
                layers: Some("  ,  ".into()),
                relation_types: Some(",".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.nodes.len(), 5);
    }

    #[test]
    fn duplicate_layer_values_are_deduped() {
        let c = seeded();
        let v = graph(
            &c,
            &GraphRequest {
                layers: Some("domain,domain".into()),
                ..Default::default()
            },
        );
        assert!(v.is_ok(), "{v:?}");
    }

    // ── 确定性 ──────────────────────────────────────────────────

    #[test]
    fn graph_is_deterministic() {
        let c = seeded();
        let a = graph(&c, &GraphRequest::default()).unwrap();
        let b = graph(&c, &GraphRequest::default()).unwrap();
        let ids_a: Vec<_> = a.nodes.iter().map(|n| n.id.clone()).collect();
        let ids_b: Vec<_> = b.nodes.iter().map(|n| n.id.clone()).collect();
        // 🔴 同分必须有确定性兜底排序：力导向布局的初始位置来自节点顺序，
        // 顺序随存储布局漂移的话，用户每次刷新看到的图都不一样
        assert_eq!(ids_a, ids_b);
        let edges_a: Vec<_> = a.edges.iter().map(|e| e.id.clone()).collect();
        let edges_b: Vec<_> = b.edges.iter().map(|e| e.id.clone()).collect();
        assert_eq!(edges_a, edges_b);
    }

    #[test]
    fn truncated_graph_is_still_deterministic() {
        let c = seeded();
        let req = GraphRequest {
            max_nodes: 3,
            max_edges: 3,
            ..Default::default()
        };
        let a = graph(&c, &req).unwrap();
        let b = graph(&c, &req).unwrap();
        let ids_a: Vec<_> = a.nodes.iter().map(|n| n.id.clone()).collect();
        let ids_b: Vec<_> = b.nodes.iter().map(|n| n.id.clone()).collect();
        assert_eq!(ids_a, ids_b, "截断也必须是确定性的");
    }

    // ── 与实体删除的交互 ────────────────────────────────────────

    #[test]
    fn edges_to_deleted_entities_are_dropped() {
        let c = seeded();
        // 删项目会级联删关系，但这里手工造一条指向不存在实体的边
        c.db
            .relations()
            .upsert(&rel(
                "r_ghost",
                "p1",
                EntityKind::Project,
                RelationType::Uses,
                "ghost_cap",
                EntityKind::Capability,
                0.9,
            ))
            .unwrap();

        let v = graph(&c, &GraphRequest::default()).unwrap();
        let ids: HashSet<&str> = v.nodes.iter().map(|n| n.id.as_str()).collect();
        // 🔴 ghost_cap 不在任何实体表里，不能被渲染成一个没有名字的空节点
        assert!(!ids.contains("ghost_cap"), "{ids:?}");
        assert!(
            v.edges.iter().all(|e| ids.contains(e.source.as_str()) && ids.contains(e.target.as_str())),
            "指向已删除实体的边必须被丢弃"
        );
        assert!(v.stats.dropped_dangling_edges >= 1);
    }
}
