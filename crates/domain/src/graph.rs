//! 知识图谱的视图模型（《产品设计书》§7-④）。
//!
//! 定位：**不是炫技，是让用户看见自己的能力结构。**
//! 点击任一节点 → 展示它来自哪些历史项目。
//!
//! 本模块只负责"把关系数据投影为可渲染的图"，不做布局算法之外的业务判断。
//! 布局采用确定性同心环布局（非随机、非物理仿真），保证：
//! 1. 同一份数据多次渲染结果一致（可测试、可截图对比）
//! 2. 不引入 d3/cytoscape 等重依赖（Local-First、包体积可控）

use serde::{Deserialize, Serialize};

use crate::relation::colors;

/// 图谱节点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    /// 节点种类（决定配色，取自官方色板）
    pub kind: String,
    /// 十六进制色值（服务端算好，前端零判断）
    pub color: String,
    /// 0-100 相对坐标（前端按 viewBox 缩放）
    pub x: f64,
    pub y: f64,
    /// 相对半径（由 project_count / degree 决定）
    pub r: f64,
    /// 是否为中心节点
    pub core: bool,
    /// 该节点关联的项目数
    pub project_count: u32,
}

/// 图谱边。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    /// 关系类型字符串（tooltip 用）
    pub relation: String,
    /// 关系中文说明
    pub relation_label: String,
    pub confidence: f64,
}

/// 完整图谱。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub node_count: usize,
    pub relation_count: usize,
}

/// 节点详情（点击节点后右栏展示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeDetail {
    pub id: String,
    pub title: String,
    /// 徽章文案（Capability / Project / Asset…）
    pub badge: String,
    pub description: String,
    /// Used in：来自哪些历史项目
    pub used_in: Vec<GraphRef>,
    /// Related：相关节点
    pub related: Vec<GraphRef>,
    /// 统计项（键值对，顺序即展示顺序）
    pub stats: Vec<GraphStat>,
}

/// 图谱中的引用项（项目/资产/能力）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRef {
    pub id: String,
    pub label: String,
    /// 跳转页面 key
    pub page: String,
}

/// 图谱统计项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStat {
    pub key: String,
    pub value: i64,
}

/// 图谱节点输入（供布局函数使用）。
#[derive(Debug, Clone)]
pub struct GraphNodeInput {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub project_count: u32,
    pub core: bool,
}

/// 确定性同心环布局。
///
/// 规则：
/// - `core` 节点置于圆心
/// - 其余节点按 kind 分组，每组占一个同心环；组内按 project_count 降序、id 升序排列
/// - 半径与角度全部由索引计算，**不含随机数**
///
/// 这样同一份数据的布局稳定可复现，视觉回归测试才能做像素级比对。
pub fn layout(nodes: &[GraphNodeInput], center: (f64, f64), base_r: f64, ring_step: f64) -> Vec<GraphNode> {
    let (cx, cy) = center;
    let mut out: Vec<GraphNode> = Vec::with_capacity(nodes.len());

    // 分组：core 单独一组，其余按 kind
    let mut groups: Vec<(String, Vec<&GraphNodeInput>)> = Vec::new();
    let mut cores: Vec<&GraphNodeInput> = Vec::new();
    for n in nodes {
        if n.core {
            cores.push(n);
            continue;
        }
        match groups.iter_mut().find(|(k, _)| k == &n.kind) {
            Some((_, v)) => v.push(n),
            None => groups.push((n.kind.clone(), vec![n])),
        }
    }

    // 组内稳定排序
    for (_, v) in &mut groups {
        v.sort_by(|a, b| {
            b.project_count
                .cmp(&a.project_count)
                .then_with(|| a.id.cmp(&b.id))
        });
    }
    // 组间按名称排序，保证环顺序稳定
    groups.sort_by(|a, b| a.0.cmp(&b.0));

    let node_r = |project_count: u32, core: bool| -> f64 {
        if core {
            return 26.0;
        }
        // 关联项目越多节点越大，上限 18
        (10.0 + (project_count as f64 * 1.6).min(8.0)).clamp(10.0, 18.0)
    };

    // core 节点：圆心附近微错位（多个 core 时不重叠）
    for (i, n) in cores.iter().enumerate() {
        let offset = i as f64 * 12.0;
        out.push(GraphNode {
            id: n.id.clone(),
            label: n.label.clone(),
            kind: n.kind.clone(),
            color: colors::for_key(&n.kind).to_string(),
            x: cx + offset - (cores.len().saturating_sub(1) as f64 * 6.0),
            y: cy,
            r: node_r(n.project_count, true),
            core: true,
            project_count: n.project_count,
        });
    }

    // 各组一个环
    for (ring_idx, (_, members)) in groups.iter().enumerate() {
        let ring_r = base_r + ring_idx as f64 * ring_step;
        let count = members.len().max(1) as f64;
        for (i, n) in members.iter().enumerate() {
            // 起始角 -90°（正上方），顺时针均分
            let angle = (std::f64::consts::TAU * i as f64 / count) - std::f64::consts::FRAC_PI_2;
            out.push(GraphNode {
                id: n.id.clone(),
                label: n.label.clone(),
                kind: n.kind.clone(),
                color: colors::for_key(&n.kind).to_string(),
                x: (cx + ring_r * angle.cos()).round() * 100.0 / 100.0,
                y: (cy + ring_r * angle.sin()).round() * 100.0 / 100.0,
                r: node_r(n.project_count, false),
                core: false,
                project_count: n.project_count,
            });
        }
    }

    out
}

/// 图例（首页与图谱页共用，单一数据源）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphLegendItem {
    pub label: String,
    pub color: String,
}

/// 生成图例。顺序与官方色板一致。
pub fn legend() -> Vec<GraphLegendItem> {
    const LABELS: &[(&str, &str)] = &[
        ("capability", "能力"),
        ("project", "项目"),
        ("code", "代码"),
        ("knowledge", "知识"),
        ("experience", "经验"),
        ("idea", "创意"),
        ("technology", "技术"),
        ("relation", "关系"),
    ];
    LABELS
        .iter()
        .map(|(k, label)| GraphLegendItem {
            label: (*label).to_string(),
            color: colors::for_key(k).to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Vec<GraphNodeInput> {
        vec![
            GraphNodeInput { id: "ai".into(), label: "AI Video".into(), kind: "capability".into(), project_count: 5, core: true },
            GraphNodeInput { id: "p1".into(), label: "yingTech".into(), kind: "project".into(), project_count: 1, core: false },
            GraphNodeInput { id: "p2".into(), label: "videoLab".into(), kind: "project".into(), project_count: 1, core: false },
            GraphNodeInput { id: "c1".into(), label: "Image Generation".into(), kind: "capability".into(), project_count: 3, core: false },
            GraphNodeInput { id: "k1".into(), label: "增量索引".into(), kind: "knowledge".into(), project_count: 1, core: false },
        ]
    }

    /// 布局必须确定性：同输入两次调用结果完全一致（可截图回归）。
    #[test]
    fn layout_is_deterministic() {
        let a = layout(&inputs(), (50.0, 50.0), 26.0, 16.0);
        let b = layout(&inputs(), (50.0, 50.0), 26.0, 16.0);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!((x.id.clone(), x.x, x.y, x.r), (y.id.clone(), y.x, y.y, y.r));
        }
    }

    #[test]
    fn core_node_sits_at_center() {
        let nodes = layout(&inputs(), (50.0, 50.0), 26.0, 16.0);
        let core = nodes.iter().find(|n| n.core).expect("应有 core 节点");
        assert_eq!(core.x, 50.0);
        assert_eq!(core.y, 50.0);
        assert_eq!(core.r, 26.0);
    }

    #[test]
    fn every_node_gets_color_from_palette() {
        let nodes = layout(&inputs(), (50.0, 50.0), 26.0, 16.0);
        assert_eq!(nodes.len(), 5);
        for n in &nodes {
            assert!(n.color.starts_with('#'), "{} 缺少色值", n.id);
            assert_eq!(n.color.len(), 7);
        }
        // 项目应为绿色，能力为紫色（官方色板）
        let p1 = nodes.iter().find(|n| n.id == "p1").unwrap();
        assert_eq!(p1.color, "#22c55e");
        let c1 = nodes.iter().find(|n| n.id == "c1").unwrap();
        assert_eq!(c1.color, "#a855f7");
    }

    #[test]
    fn nodes_lay_on_distinct_rings_per_kind() {
        let nodes = layout(&inputs(), (50.0, 50.0), 26.0, 16.0);
        let dist = |n: &GraphNode| ((n.x - 50.0).powi(2) + (n.y - 50.0).powi(2)).sqrt();
        let d_p1 = dist(nodes.iter().find(|n| n.id == "p1").unwrap());
        let d_k1 = dist(nodes.iter().find(|n| n.id == "k1").unwrap());
        // capability 环与 project 环、knowledge 环半径应不同
        assert!((d_p1 - d_k1).abs() > 1.0, "不同 kind 应落在不同环上");
    }

    #[test]
    fn more_projects_means_bigger_node() {
        let inputs = vec![
            GraphNodeInput { id: "big".into(), label: "B".into(), kind: "capability".into(), project_count: 20, core: false },
            GraphNodeInput { id: "small".into(), label: "S".into(), kind: "capability".into(), project_count: 0, core: false },
        ];
        let nodes = layout(&inputs, (50.0, 50.0), 26.0, 16.0);
        let big = nodes.iter().find(|n| n.id == "big").unwrap();
        let small = nodes.iter().find(|n| n.id == "small").unwrap();
        assert!(big.r > small.r, "big={} small={}", big.r, small.r);
        assert!(big.r <= 18.0, "半径应有上限");
    }

    #[test]
    fn empty_input_yields_empty_layout() {
        assert!(layout(&[], (50.0, 50.0), 26.0, 16.0).is_empty());
    }

    #[test]
    fn legend_uses_official_palette() {
        let l = legend();
        assert!(!l.is_empty());
        let cap = l.iter().find(|i| i.label == "能力").unwrap();
        assert_eq!(cap.color, "#a855f7");
        let proj = l.iter().find(|i| i.label == "项目").unwrap();
        assert_eq!(proj.color, "#22c55e");
        let rel = l.iter().find(|i| i.label == "关系").unwrap();
        assert_eq!(rel.color, "#64748b");
    }

    #[test]
    fn multiple_cores_do_not_overlap() {
        let inputs = vec![
            GraphNodeInput { id: "c1".into(), label: "A".into(), kind: "capability".into(), project_count: 3, core: true },
            GraphNodeInput { id: "c2".into(), label: "B".into(), kind: "capability".into(), project_count: 2, core: true },
        ];
        let nodes = layout(&inputs, (50.0, 50.0), 26.0, 16.0);
        assert_ne!(nodes[0].x, nodes[1].x, "多个 core 节点应错开");
    }
}
