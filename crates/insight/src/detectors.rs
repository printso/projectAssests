//! 洞察检测器（《产品设计书》V0.3）。
//!
//! # 设计要点
//! 每个检测器是**独立纯函数**：`(快照, 配置) -> Vec<Insight>`。
//! 这样单个检测器可被穷举单测，新增检测器不影响已有的。
//!
//! # 🔴 产品纪律（贯穿全模块）
//! 1. **每条洞察必须带 ≥1 条证据**：`Insight::validate()` 会拒绝无证据的结论，
//!    存储层也会拒绝入库。检测器内部就先过滤，不做无用功。
//! 2. **证据必须是真实路径/项目名**，不是"某个文件"这类空话。
//! 3. **区分事实与推断**：`EvidenceKind::File` 是事实，
//!    推测性描述必须写明"推测"（见 `forgotten_asset` 的措辞）。
//! 4. **宁可少报，不可误报**：错误的"你在 4 个项目重复实现了 X"
//!    会直接摧毁用户信任，比漏报严重得多。

use std::collections::BTreeMap;

use projectassests_domain::{
    Asset, EvidenceItem, EvidenceKind, Insight, InsightType, ProjectStatus, UserFeedback,
};

use crate::input::{idle_days, AnalysisInput};

/// 检测器配置（阈值集中于此，便于调参与测试）。
#[derive(Debug, Clone)]
pub struct DetectorConfig {
    /// 重复能力：至少多少个项目实现同一能力才算重复
    pub duplicate_min_projects: usize,
    /// 高复用组件：reuse_score 下限
    pub reusable_min_score: f64,
    /// 遗忘资产：项目多少天未更新算"被遗忘"
    pub forgotten_idle_days: i64,
    /// 遗忘资产：reuse_score 下限（低分资产不值得提醒）
    pub forgotten_min_score: f64,
    /// 技术方向：近多少天内的项目参与统计
    pub direction_window_days: i64,
    /// 技术方向：至少多少个项目才算"持续方向"
    pub direction_min_projects: usize,
    /// 置信度门槛（低于此值不生成，与 `Insight::validate` 的门槛一致）
    pub confidence_floor: f64,
    /// 每类洞察最多产出多少条（防止一次刷屏）
    pub max_per_detector: usize,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            duplicate_min_projects: 2,
            reusable_min_score: 0.80,
            forgotten_idle_days: 180,
            forgotten_min_score: 0.70,
            direction_window_days: 180,
            direction_min_projects: 3,
            confidence_floor: projectassests_domain::CONFIDENCE_THRESHOLD,
            max_per_detector: 10,
        }
    }
}

/// 检测器集合的产出。
#[derive(Debug, Clone, Default)]
pub struct DetectionOutcome {
    pub insights: Vec<Insight>,
    /// 被产品红线拦下的候选数（诊断用：说明数据质量不足而非检测器失效）
    pub rejected: usize,
}

impl DetectionOutcome {
    /// 追加一条洞察，自动校验产品红线。
    ///
    /// 返回是否被接受。拒绝不视为错误——检测器会产出很多候选，
    /// 不合格的静默丢弃并计数，避免噪音进入用户视野。
    fn push(&mut self, insight: Insight) -> bool {
        match insight.validate() {
            Ok(()) => {
                self.insights.push(insight);
                true
            }
            Err(e) => {
                tracing::debug!(reason = %e, "洞察未通过校验，已丢弃");
                self.rejected += 1;
                false
            }
        }
    }
}

// ── 1. 重复能力检测 ────────────────────────────────────────────────

/// "你在 N 个项目中重复实现了 X"——产品最核心的价值点（《产品设计书》S4）。
///
/// 判据：同一 Capability 被 ≥2 个项目 `implements`。
/// 证据同时给出**能力级**（哪些项目）与**资产级**（哪些文件），
/// 用户点开能直接看到可对比的真实代码位置。
pub fn detect_duplicate_capabilities(
    input: &AnalysisInput,
    cfg: &DetectorConfig,
) -> DetectionOutcome {
    let mut out = DetectionOutcome::default();
    let today = input.now.format("%Y-%m-%d").to_string();

    // 能力 id → 实现它的项目列表
    let mut by_capability: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for cap in input.capability_layer_nodes() {
        let projects = input.projects_of_capability(&cap.id);
        if projects.len() >= cfg.duplicate_min_projects.max(2) {
            by_capability.insert(cap.id.clone(), projects);
        }
    }

    // 按项目数降序：重复面越广越值得先看
    let mut ranked: Vec<_> = by_capability.into_iter().collect();
    ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

    for (cap_id, project_ids) in ranked {
        if out.insights.len() >= cfg.max_per_detector {
            break;
        }
        let Some(cap) = input.capability(&cap_id) else {
            continue;
        };
        let n = project_ids.len();

        // 证据：项目 + 每个项目里实现该能力的具体文件
        let mut evidence: Vec<EvidenceItem> = project_ids
            .iter()
            .map(|pid| EvidenceItem {
                kind: EvidenceKind::Project,
                label: input
                    .project(pid)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| pid.clone()),
                target: Some(pid.clone()),
            })
            .collect();

        // 补充文件级证据：该项目下与该能力相关的资产路径
        let mut file_evidence: Vec<EvidenceItem> = Vec::new();
        let mut related_assets: Vec<String> = Vec::new();
        for pid in &project_ids {
            for a in input.assets_of(pid) {
                if !asset_matches_capability(a, &cap.name) {
                    continue;
                }
                file_evidence.push(EvidenceItem {
                    kind: EvidenceKind::File,
                    label: a.source_path.clone(),
                    target: Some(format!("{pid}:{}", a.source_path)),
                });
                related_assets.push(a.id.clone());
            }
        }
        // 文件证据按项目分组限量，避免单项目刷屏
        evidence.extend(file_evidence.into_iter().take(8));

        // 置信度：项目数越多越确信（能力重复是真事实，非推断）
        let confidence = duplicate_confidence(n, cap.confidence, related_assets.len());
        if confidence < cfg.confidence_floor {
            out.rejected += 1;
            continue;
        }

        let title = format!("你在 {n} 个项目中重复实现了「{}」", cap.name);
        let description = if related_assets.is_empty() {
            format!(
                "能力「{}」在 {} 个项目中各自实现了一遍。建议抽取为独立组件复用，避免同类问题反复解决。",
                cap.name, n
            )
        } else {
            format!(
                "能力「{}」在 {} 个项目中各自实现了一遍，共发现 {} 处对应代码。建议抽取为独立组件复用。",
                cap.name,
                n,
                related_assets.len()
            )
        };

        let mut tags = vec![cap.name.clone(), "重复实现".to_string(), "可复用".to_string()];
        if let Some(domain) = cap.parent_id.as_ref().and_then(|p| input.capability(p)) {
            tags.push(domain.name.clone());
        }

        out.push(Insight {
            id: format!("ins_dup_{cap_id}"),
            insight_type: InsightType::DuplicateCapability,
            title,
            description,
            evidence,
            confidence,
            created_at: today.clone(),
            user_feedback: None,
            tags,
            related_project_ids: project_ids.clone(),
            related_asset_ids: related_assets,
        });
    }
    out
}

/// 重复能力的置信度。
///
/// 因子：涉及项目数（主）+ 能力本身的抽取置信度 + 是否有文件级证据支撑。
/// 上限 0.96：能力重复是"能力名相同"，不等于"实现相同"，不给满分。
fn duplicate_confidence(projects: usize, cap_confidence: f64, asset_hits: usize) -> f64 {
    let base = match projects {
        2 => 0.60,
        3 => 0.72,
        4 => 0.80,
        _ => 0.86,
    };
    let file_bonus = if asset_hits > 0 { 0.06 } else { 0.0 };
    (base + cap_confidence * 0.08 + file_bonus).min(0.96)
}

/// 资产是否与某能力相关（用于补充文件级证据）。
///
/// 保守匹配：资产名或标签包含能力名的任一关键词。
/// 宁可少给证据也不给错证据——错误的文件指向会让用户点开发现"这跟能力无关"。
fn asset_matches_capability(asset: &Asset, capability_name: &str) -> bool {
    let name_lower = asset.name.to_ascii_lowercase();
    let tags_lower: Vec<String> = asset.tags.iter().map(|t| t.to_ascii_lowercase()).collect();
    let keywords: Vec<String> = capability_name
        .split_whitespace()
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| w.chars().count() >= 4) // 跳过 "of"/"the" 这类短词
        .collect();
    if keywords.is_empty() {
        return name_lower.contains(&capability_name.to_ascii_lowercase());
    }
    keywords.iter().any(|k| {
        name_lower.contains(k.as_str()) || tags_lower.iter().any(|t| t.contains(k.as_str()))
    })
}

// ── 2. 高复用组件 ──────────────────────────────────────────────────

/// 识别高复用价值的资产并给出抽取建议（《产品设计书》V0.3 功能 16）。
///
/// 🔴 建议必须可执行：指向具体文件与函数，而不是"考虑重构"这类空话。
pub fn detect_reusable_components(
    input: &AnalysisInput,
    cfg: &DetectorConfig,
) -> DetectionOutcome {
    let mut out = DetectionOutcome::default();
    let today = input.now.format("%Y-%m-%d").to_string();

    let mut candidates: Vec<&Asset> = input
        .assets
        .iter()
        .filter(|a| {
            a.reuse_score >= cfg.reusable_min_score
                // 用户已标记"无用"的不再推荐（反馈回流，不重复骚扰）
                && a.user_feedback != Some(UserFeedback::Useless)
        })
        .collect();
    // 按复用分降序，同分按名字（确定性）
    candidates.sort_by(|a, b| {
        b.reuse_score
            .partial_cmp(&a.reuse_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });

    for asset in candidates.into_iter().take(cfg.max_per_detector) {
        let project = input.project(&asset.project_id);
        let project_name = project.map(|p| p.name.as_str()).unwrap_or(&asset.project_id);

        let mut evidence = vec![EvidenceItem {
            kind: EvidenceKind::File,
            label: asset.source_path.clone(),
            target: Some(format!("{}:{}", asset.project_id, asset.source_path)),
        }];
        // 带上抽取时记录的调用点与理由（真实证据，非编造）
        evidence.extend(asset.evidence.used_by.iter().map(|u| EvidenceItem {
            kind: EvidenceKind::Symbol,
            label: u.clone(),
            target: None,
        }));
        evidence.extend(asset.evidence.commits.iter().map(|c| EvidenceItem {
            kind: EvidenceKind::Commit,
            label: format!("commit {c}"),
            target: None,
        }));

        // 置信度直接沿用资产抽取的置信度（评分时的确信程度）
        let confidence = asset.confidence;
        if confidence < cfg.confidence_floor {
            out.rejected += 1;
            continue;
        }

        let mut reasoning = asset.evidence.reasoning.clone();
        if asset.evidence.used_by.is_empty() {
            reasoning.push("尚未发现文件内调用点，抽取前建议先确认使用场景".to_string());
        }

        let description = format!(
            "「{}」（{project_name}）复用评分 {:.2}、通用性 {:.2}，适合抽取为独立组件。{}",
            asset.name,
            asset.reuse_score,
            asset.generality,
            if reasoning.is_empty() {
                String::new()
            } else {
                format!("判定依据：{}", reasoning.join("；"))
            }
        );

        out.push(Insight {
            id: format!("ins_reuse_{}", asset.id),
            insight_type: InsightType::ReusableComponent,
            title: format!("高复用资产：{}", asset.name),
            description,
            evidence,
            confidence,
            created_at: today.clone(),
            user_feedback: None,
            tags: vec![
                asset.asset_type.label_zh().to_string(),
                "可复用".to_string(),
                project_name.to_string(),
            ],
            related_project_ids: vec![asset.project_id.clone()],
            related_asset_ids: vec![asset.id.clone()],
        });
    }
    out
}

// ── 3. 遗忘资产 ────────────────────────────────────────────────────

/// 被遗忘但仍有价值的资产（《产品介绍总纲》北极星指标的直接来源）。
///
/// 判据：项目长期未更新（≥180 天）+ 资产 reuse_score 仍高 + 用户尚未反馈过。
///
/// 🔴 措辞纪律：这是**推断**（"可能仍适用"），不是事实。
/// 描述里必须体现不确定性，否则违反"给证据不给玄学"。
pub fn detect_forgotten_assets(input: &AnalysisInput, cfg: &DetectorConfig) -> DetectionOutcome {
    let mut out = DetectionOutcome::default();
    let today = input.now.format("%Y-%m-%d").to_string();

    // 只考虑长期未更新的项目
    let stale_projects: Vec<_> = input
        .projects
        .iter()
        .filter(|p| {
            let idle = idle_days(p, input.now);
            idle >= cfg.forgotten_idle_days
                // 已归档项目是"用户主动放弃"，不该再提醒
                && p.status != ProjectStatus::Abandoned
        })
        .collect();

    // 收集候选资产：高复用分 + 未被反馈过
    let mut candidates: Vec<(&Asset, i64)> = Vec::new();
    for p in &stale_projects {
        let idle = idle_days(p, input.now);
        for a in input.assets_of(&p.id) {
            if a.reuse_score >= cfg.forgotten_min_score && a.user_feedback.is_none() {
                candidates.push((a, idle));
            }
        }
    }
    // 闲置越久 + 分越高 → 越值得提醒
    candidates.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.0.reuse_score.partial_cmp(&a.0.reuse_score).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });

    for (asset, idle) in candidates.into_iter().take(cfg.max_per_detector) {
        let project = input.project(&asset.project_id);
        let project_name = project.map(|p| p.name.as_str()).unwrap_or(&asset.project_id);
        let idle_months = (idle as f64 / 30.0).round() as i64;

        let evidence = vec![
            EvidenceItem {
                kind: EvidenceKind::File,
                label: asset.source_path.clone(),
                target: Some(format!("{}:{}", asset.project_id, asset.source_path)),
            },
            EvidenceItem {
                kind: EvidenceKind::Project,
                label: format!("{project_name}（约 {idle_months} 个月未更新）"),
                target: Some(asset.project_id.clone()),
            },
        ];

        // 置信度：闲置越久越确信"被遗忘"这个事实，但"仍有价值"始终是推断
        let confidence = forgotten_confidence(idle, asset.reuse_score);
        if confidence < cfg.confidence_floor {
            out.rejected += 1;
            continue;
        }

        out.push(Insight {
            id: format!("ins_forgot_{}", asset.id),
            insight_type: InsightType::ForgottenAsset,
            title: format!("被遗忘的资产：{}", asset.name),
            description: format!(
                "「{project_name}」已约 {idle_months} 个月未更新，但其中的「{}」复用评分仍达 {:.2}。推测它可能仍适用于当前项目，建议查看后决定是否复用。",
                asset.name, asset.reuse_score
            ),
            evidence,
            confidence,
            created_at: today.clone(),
            user_feedback: None,
            tags: vec![
                asset.asset_type.label_zh().to_string(),
                "遗忘资产".to_string(),
                project_name.to_string(),
            ],
            related_project_ids: vec![asset.project_id.clone()],
            related_asset_ids: vec![asset.id.clone()],
        });
    }
    out
}

/// 遗忘资产的置信度。
///
/// 上限 0.85：这是推断而非事实，必须低于"重复实现"这类可验证结论。
fn forgotten_confidence(idle_days: i64, reuse_score: f64) -> f64 {
    let idle_factor = match idle_days {
        d if d >= 730 => 0.55,
        d if d >= 365 => 0.45,
        d if d >= 180 => 0.32,
        _ => 0.15,
    };
    (idle_factor + reuse_score * 0.35).min(0.85)
}

// ── 4. 技术方向 ────────────────────────────────────────────────────

/// "你过去 N 个月最持续的技术方向：X"（《产品设计书》V0.3 功能 21）。
///
/// 判据：时间窗口内有 ≥N 个活跃项目都实现了同一能力。
/// 这是**趋势**判断，需要多个项目支撑，单个项目不构成"方向"。
pub fn detect_tech_directions(input: &AnalysisInput, cfg: &DetectorConfig) -> DetectionOutcome {
    let mut out = DetectionOutcome::default();
    let today = input.now.format("%Y-%m-%d").to_string();

    // 窗口内的项目
    let window_projects: Vec<_> = input
        .projects
        .iter()
        .filter(|p| {
            let idle = idle_days(p, input.now);
            idle <= cfg.direction_window_days
        })
        .collect();
    if window_projects.len() < cfg.direction_min_projects {
        return out; // 数据不足，不做趋势判断
    }

    // 能力 → 窗口内实现它的项目
    let mut cap_projects: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in &window_projects {
        for cap_id in input.capabilities_of(&p.id) {
            cap_projects.entry(cap_id).or_default().push(p.id.clone());
        }
    }

    let mut ranked: Vec<_> = cap_projects
        .into_iter()
        .filter(|(_, ps)| ps.len() >= cfg.direction_min_projects)
        .collect();
    ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

    let months = (cfg.direction_window_days as f64 / 30.0).round() as i64;
    for (cap_id, project_ids) in ranked {
        if out.insights.len() >= cfg.max_per_detector {
            break;
        }
        let Some(cap) = input.capability(&cap_id) else {
            continue;
        };
        let n = project_ids.len();
        let evidence: Vec<EvidenceItem> = project_ids
            .iter()
            .map(|pid| EvidenceItem {
                kind: EvidenceKind::Project,
                label: input
                    .project(pid)
                    .map(|p| format!("{}（{}）", p.name, p.updated_at.as_deref().unwrap_or("更新时间未知")))
                    .unwrap_or_else(|| pid.clone()),
                target: Some(pid.clone()),
            })
            .collect();

        // 置信度：占比越高越确信是"主线方向"
        let ratio = n as f64 / window_projects.len() as f64;
        let confidence = (0.55 + ratio * 0.35).min(0.92);
        if confidence < cfg.confidence_floor {
            out.rejected += 1;
            continue;
        }

        out.push(Insight {
            id: format!("ins_dir_{cap_id}"),
            insight_type: InsightType::TechDirection,
            title: format!("持续的技术方向：{}", cap.name),
            description: format!(
                "过去约 {months} 个月内，{window_total} 个活跃项目中有 {n} 个都涉及「{}」（占 {:.0}%）。这是你近期最持续的技术投入方向。",
                cap.name,
                ratio * 100.0,
                window_total = window_projects.len()
            ),
            evidence,
            confidence,
            created_at: today.clone(),
            user_feedback: None,
            tags: vec![cap.name.clone(), "技术方向".to_string()],
            related_project_ids: project_ids,
            related_asset_ids: vec![],
        });
    }
    out
}

// ── 5. 组合机会提示 ────────────────────────────────────────────────

/// 组合机会线索（《产品设计书》V0.3 功能 18 的轻量版）。
///
/// 与 `Opportunity` 表的区别：这里是**线索**（"这几个项目能力互补"），
/// 成型的 Opportunity 卡片由 `opportunities.rs` 生成（含覆盖度、缺失能力、星级）。
///
/// 判据：多个项目各自持有不同能力，且合计覆盖了某个"能力簇"的大部分。
pub fn detect_combination_hints(input: &AnalysisInput, cfg: &DetectorConfig) -> DetectionOutcome {
    let mut out = DetectionOutcome::default();
    let today = input.now.format("%Y-%m-%d").to_string();

    // 按 Domain 分组统计"该领域下有多少项目、覆盖多少能力"
    // 只有当同一 Domain 下 ≥3 个项目、≥3 个不同能力时，才提示组合机会
    let mut domain_projects: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut domain_caps: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for cap in input.capability_layer_nodes() {
        let Some(domain_id) = cap.parent_id.clone() else {
            continue;
        };
        let projects = input.projects_of_capability(&cap.id);
        if projects.is_empty() {
            continue;
        }
        domain_caps.entry(domain_id.clone()).or_default().push(cap.id.clone());
        let entry = domain_projects.entry(domain_id).or_default();
        for p in projects {
            if !entry.contains(&p) {
                entry.push(p);
            }
        }
    }

    for (domain_id, project_ids) in domain_projects {
        if out.insights.len() >= cfg.max_per_detector {
            break;
        }
        let caps = domain_caps.get(&domain_id).cloned().unwrap_or_default();
        if project_ids.len() < 3 || caps.len() < 3 {
            continue; // 证据不足，不提示
        }
        let Some(domain) = input.capability(&domain_id) else {
            continue;
        };

        let cap_names: Vec<String> = caps
            .iter()
            .filter_map(|id| input.capability(id).map(|c| c.name.clone()))
            .collect();

        let mut evidence: Vec<EvidenceItem> = project_ids
            .iter()
            .take(5)
            .map(|pid| EvidenceItem {
                kind: EvidenceKind::Project,
                label: input
                    .project(pid)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| pid.clone()),
                target: Some(pid.clone()),
            })
            .collect();
        evidence.extend(cap_names.iter().take(4).map(|n| EvidenceItem {
            kind: EvidenceKind::Symbol,
            label: format!("能力 {n}"),
            target: None,
        }));

        let confidence = (0.58 + (caps.len().min(6) as f64) * 0.04).min(0.88);
        if confidence < cfg.confidence_floor {
            out.rejected += 1;
            continue;
        }

        out.push(Insight {
            id: format!("ins_combo_{domain_id}"),
            insight_type: InsightType::OpportunityHint,
            title: format!("{}领域存在组合机会", domain.name),
            description: format!(
                "你在「{}」领域有 {} 个项目，合计覆盖 {} 项能力（{}）。这些能力互补，可能组合成一个更完整的产品。",
                domain.name,
                project_ids.len(),
                cap_names.len(),
                cap_names.iter().take(4).cloned().collect::<Vec<_>>().join(" / ")
            ),
            evidence,
            confidence,
            created_at: today.clone(),
            user_feedback: None,
            tags: vec![domain.name.clone(), "组合机会".to_string()],
            related_project_ids: project_ids,
            related_asset_ids: vec![],
        });
    }
    out
}

/// 运行全部检测器。
///
/// 顺序即产出顺序：重复能力 → 高复用 → 遗忘资产 → 技术方向 → 组合机会。
/// 这个顺序对应价值从高到低（重复实现是最痛的点），首页"AI 发现"按此展示。
pub fn detect_all(input: &AnalysisInput, cfg: &DetectorConfig) -> DetectionOutcome {
    let detectors: Vec<fn(&AnalysisInput, &DetectorConfig) -> DetectionOutcome> = vec![
        detect_duplicate_capabilities,
        detect_reusable_components,
        detect_forgotten_assets,
        detect_tech_directions,
        detect_combination_hints,
    ];
    let mut merged = DetectionOutcome::default();
    for d in detectors {
        let part = d(input, cfg);
        merged.insights.extend(part.insights);
        merged.rejected += part.rejected;
    }
    // 全局按置信度降序，同分按类型再按 id（确定性输出）
    merged.insights.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    merged
}

/// 合并多轮洞察：新洞察不得覆盖用户已有反馈。
///
/// 🔴 这是"反馈回流"能成立的前提。若每轮重新分析都把 user_feedback 重置为 null，
/// 用户标记的"有用/无用"会在下次扫描后全部丢失，
/// 洞察采纳率指标（《产品设计书》阶段二 ≥40%）就永远无法积累。
pub fn merge_with_feedback(
    fresh: Vec<Insight>,
    existing: &[Insight],
) -> Vec<Insight> {
    let feedback_by_id: BTreeMap<&str, UserFeedback> = existing
        .iter()
        .filter_map(|i| i.user_feedback.map(|f| (i.id.as_str(), f)))
        .collect();

    fresh
        .into_iter()
        .map(|mut i| {
            if let Some(&fb) = feedback_by_id.get(i.id.as_str()) {
                i.user_feedback = Some(fb);
            }
            i
        })
        .collect()
}

/// 未读洞察（首页"AI 发现"只展示用户没处理过的）。
pub fn unread(insights: &[Insight]) -> Vec<&Insight> {
    insights.iter().filter(|i| i.user_feedback.is_none()).collect()
}

/// 采纳率：标记有用 / (有用 + 无用)。
///
/// 分母排除 `Ignored`：用户"暂时不关心"不等于"结论是错的"，
/// 计入会系统性拉低指标，误导调权方向。
pub fn adoption_rate(insights: &[Insight]) -> Option<f64> {
    let useful = insights
        .iter()
        .filter(|i| i.user_feedback == Some(UserFeedback::Useful))
        .count();
    let useless = insights
        .iter()
        .filter(|i| i.user_feedback == Some(UserFeedback::Useless))
        .count();
    let total = useful + useless;
    if total == 0 {
        return None; // 无反馈时返回 None，由 UI 显示"暂无反馈"而非 0%
    }
    Some(useful as f64 / total as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::tests::{asset, cap, implements, project};
    // 这些类型只在测试里构造 fixture 时需要，主代码通过 input 快照访问，
    // 因此放在测试模块导入而非文件顶部（避免"未使用导入"警告）。
    use projectassests_domain::{AssetType, CapabilityLayer, CodeStats, Evidence, Project};

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-29T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// N 个项目共享一组能力。
    fn shared_capability_input(n: usize) -> AnalysisInput {
        let mut projects = Vec::new();
        let mut relations = Vec::new();
        for i in 0..n {
            let pid = format!("p{i}");
            projects.push(project(&pid, ProjectStatus::Active, Some("2026-09-01")));
            relations.push(implements(&pid, "c_queue"));
        }
        AnalysisInput {
            projects,
            assets: Vec::new(),
            capabilities: vec![
                cap("d_infra", "基础设施", CapabilityLayer::Domain, None),
                cap("c_queue", "Task Queue", CapabilityLayer::Capability, Some("d_infra")),
            ],
            relations,
            now: now(),
        }
    }

    // ── 重复能力 ────────────────────────────────────────────────────

    #[test]
    fn detects_duplicate_capability() {
        let input = shared_capability_input(3);
        let out = detect_duplicate_capabilities(&input, &DetectorConfig::default());
        assert_eq!(out.insights.len(), 1);
        let i = &out.insights[0];
        assert_eq!(i.insight_type, InsightType::DuplicateCapability);
        assert!(i.title.contains("3 个项目"), "标题应含项目数: {}", i.title);
        assert!(i.title.contains("Task Queue"));
        assert_eq!(i.evidence.len(), 3, "每个项目一条证据");
        assert_eq!(i.related_project_ids.len(), 3);
        assert!(i.validate().is_ok());
    }

    #[test]
    fn single_project_is_not_duplicate() {
        let input = shared_capability_input(1);
        assert!(detect_duplicate_capabilities(&input, &DetectorConfig::default()).insights.is_empty());
    }

    #[test]
    fn duplicate_confidence_grows_with_project_count() {
        let two = detect_duplicate_capabilities(&shared_capability_input(2), &DetectorConfig::default());
        let five = detect_duplicate_capabilities(&shared_capability_input(5), &DetectorConfig::default());
        assert!(five.insights[0].confidence > two.insights[0].confidence);
        assert!(five.insights[0].confidence <= 0.96, "同名不等于同实现，不给满分");
    }

    /// 重复检测要带上文件级证据（用户点开要能看到具体代码位置）。
    #[test]
    fn duplicate_includes_file_evidence_when_available() {
        let mut input = shared_capability_input(2);
        // 资产名含能力关键词 "queue"
        input.assets = vec![
            asset("a1", "p0", "TaskQueueRunner", AssetType::Code, 0.9),
            asset("a2", "p1", "TaskQueueRunner", AssetType::Code, 0.85),
        ];
        let out = detect_duplicate_capabilities(&input, &DetectorConfig::default());
        let i = &out.insights[0];
        assert!(i.evidence.iter().any(|e| e.kind == EvidenceKind::File), "应有文件级证据");
        assert_eq!(i.related_asset_ids.len(), 2);
        assert!(i.description.contains("处对应代码"));
    }

    /// 能力关键词匹配必须保守：`ai` 这种短词不该让所有资产都被算作证据。
    #[test]
    fn capability_asset_matching_ignores_short_words() {
        let a = asset("a1", "p1", "SomeRandomThing", AssetType::Code, 0.9);
        assert!(!asset_matches_capability(&a, "AI"));
        assert!(asset_matches_capability(&a, "Random Processing"));
        assert!(!asset_matches_capability(&a, "Video Generation"));
    }

    #[test]
    fn capability_asset_matching_uses_tags() {
        let mut a = asset("a1", "p1", "Runner", AssetType::Code, 0.9);
        a.tags = vec!["queue".into()];
        assert!(asset_matches_capability(&a, "Task Queue"));
    }

    // ── 高复用组件 ──────────────────────────────────────────────────

    #[test]
    fn detects_reusable_components() {
        let mut input = AnalysisInput {
            projects: vec![project("p1", ProjectStatus::Active, Some("2026-09-01"))],
            now: now(),
            ..Default::default()
        };
        let mut a = asset("a1", "p1", "VideoPipeline", AssetType::Component, 0.91);
        a.evidence.used_by = vec!["render()".into(), "batch_render()".into()];
        a.evidence.reasoning = vec!["被多个模块调用".into(), "与业务逻辑耦合低".into()];
        input.assets = vec![a, asset("a2", "p1", "Trivial", AssetType::Code, 0.3)];

        let out = detect_reusable_components(&input, &DetectorConfig::default());
        assert_eq!(out.insights.len(), 1, "低分资产不该被推荐");
        let i = &out.insights[0];
        assert_eq!(i.insight_type, InsightType::ReusableComponent);
        assert!(i.title.contains("VideoPipeline"));
        // 建议必须可执行：指向具体文件
        assert!(i.evidence.iter().any(|e| e.kind == EvidenceKind::File));
        // 带上抽取时的调用点与理由
        assert!(i.evidence.iter().any(|e| e.kind == EvidenceKind::Symbol && e.label == "render()"));
        assert!(i.description.contains("被多个模块调用"), "应含判定依据: {}", i.description);
    }

    /// 用户已标记"无用"的资产不该再被推荐（反馈回流，不重复骚扰）。
    #[test]
    fn skips_assets_marked_useless() {
        let mut input = AnalysisInput {
            projects: vec![project("p1", ProjectStatus::Active, Some("2026-09-01"))],
            now: now(),
            ..Default::default()
        };
        let mut a = asset("a1", "p1", "X", AssetType::Code, 0.95);
        a.user_feedback = Some(UserFeedback::Useless);
        input.assets = vec![a];
        assert!(detect_reusable_components(&input, &DetectorConfig::default()).insights.is_empty());
    }

    #[test]
    fn reusable_sorted_by_score_desc() {
        let mut input = AnalysisInput {
            projects: vec![project("p1", ProjectStatus::Active, Some("2026-09-01"))],
            now: now(),
            ..Default::default()
        };
        input.assets = vec![
            asset("a1", "p1", "Low", AssetType::Code, 0.81),
            asset("a2", "p1", "High", AssetType::Code, 0.95),
            asset("a3", "p1", "Mid", AssetType::Code, 0.88),
        ];
        let out = detect_reusable_components(&input, &DetectorConfig::default());
        let names: Vec<&str> = out.insights.iter().map(|i| i.title.as_str()).collect();
        assert!(names[0].contains("High"), "实际: {names:?}");
        assert!(names[2].contains("Low"));
    }

    #[test]
    fn reusable_respects_score_threshold() {
        let mut input = AnalysisInput {
            projects: vec![project("p1", ProjectStatus::Active, Some("2026-09-01"))],
            now: now(),
            ..Default::default()
        };
        input.assets = vec![asset("a1", "p1", "X", AssetType::Code, 0.79)];
        assert!(detect_reusable_components(&input, &DetectorConfig::default()).insights.is_empty());
    }

    /// 没有调用点证据时应诚实提示，而不是假装很确定。
    #[test]
    fn notes_missing_usage_evidence() {
        let mut input = AnalysisInput {
            projects: vec![project("p1", ProjectStatus::Active, Some("2026-09-01"))],
            now: now(),
            ..Default::default()
        };
        let mut a = asset("a1", "p1", "X", AssetType::Code, 0.9);
        a.evidence = Evidence { files: vec!["src/x.py".into()], ..Default::default() };
        input.assets = vec![a];
        let out = detect_reusable_components(&input, &DetectorConfig::default());
        assert!(out.insights[0].description.contains("尚未发现文件内调用点"));
    }

    // ── 遗忘资产 ────────────────────────────────────────────────────

    fn stale_input(idle_days: i64, score: f64, status: ProjectStatus) -> AnalysisInput {
        let updated = (now() - chrono::Duration::days(idle_days)).format("%Y-%m-%d").to_string();
        AnalysisInput {
            projects: vec![project("p1", status, Some(&updated))],
            assets: vec![asset("a1", "p1", "OldGem", AssetType::Component, score)],
            now: now(),
            ..Default::default()
        }
    }

    #[test]
    fn detects_forgotten_asset() {
        let input = stale_input(400, 0.85, ProjectStatus::Paused);
        let out = detect_forgotten_assets(&input, &DetectorConfig::default());
        assert_eq!(out.insights.len(), 1);
        let i = &out.insights[0];
        assert_eq!(i.insight_type, InsightType::ForgottenAsset);
        assert!(i.description.contains("推测"), "推断性结论必须标明不确定性: {}", i.description);
        assert!(i.description.contains("个月未更新"));
        assert!(i.confidence <= 0.85, "推断类洞察置信度应低于事实类");
    }

    #[test]
    fn recent_project_is_not_forgotten() {
        let input = stale_input(30, 0.95, ProjectStatus::Active);
        assert!(detect_forgotten_assets(&input, &DetectorConfig::default()).insights.is_empty());
    }

    /// 已归档项目是用户主动放弃，不该再提醒（否则是骚扰）。
    #[test]
    fn abandoned_project_is_not_reminded() {
        let input = stale_input(800, 0.95, ProjectStatus::Abandoned);
        assert!(detect_forgotten_assets(&input, &DetectorConfig::default()).insights.is_empty());
    }

    #[test]
    fn low_score_asset_is_not_forgotten_gem() {
        let input = stale_input(800, 0.3, ProjectStatus::Paused);
        assert!(detect_forgotten_assets(&input, &DetectorConfig::default()).insights.is_empty());
    }

    /// 用户已反馈过的资产不再重复提醒。
    #[test]
    fn feedback_suppresses_forgotten_alert() {
        let mut input = stale_input(400, 0.85, ProjectStatus::Paused);
        input.assets[0].user_feedback = Some(UserFeedback::Ignored);
        assert!(detect_forgotten_assets(&input, &DetectorConfig::default()).insights.is_empty());
    }

    #[test]
    fn forgotten_confidence_grows_with_idle_time() {
        let a = forgotten_confidence(200, 0.8);
        let b = forgotten_confidence(800, 0.8);
        assert!(b > a);
        assert!(b <= 0.85);
    }

    // ── 技术方向 ────────────────────────────────────────────────────

    fn direction_input(n_projects: usize, idle_days: i64) -> AnalysisInput {
        let updated = (now() - chrono::Duration::days(idle_days)).format("%Y-%m-%d").to_string();
        let mut projects = Vec::new();
        let mut relations = Vec::new();
        for i in 0..n_projects {
            let pid = format!("p{i}");
            projects.push(project(&pid, ProjectStatus::Active, Some(&updated)));
            relations.push(implements(&pid, "c_video"));
        }
        AnalysisInput {
            projects,
            capabilities: vec![
                cap("d_ai", "AI", CapabilityLayer::Domain, None),
                cap("c_video", "Video Generation", CapabilityLayer::Capability, Some("d_ai")),
            ],
            relations,
            now: now(),
            ..Default::default()
        }
    }

    #[test]
    fn detects_tech_direction() {
        let input = direction_input(4, 30);
        let out = detect_tech_directions(&input, &DetectorConfig::default());
        assert_eq!(out.insights.len(), 1);
        let i = &out.insights[0];
        assert_eq!(i.insight_type, InsightType::TechDirection);
        assert!(i.title.contains("Video Generation"));
        assert!(i.description.contains("4 个"), "{}", i.description);
        assert!(i.description.contains('%'));
        assert_eq!(i.evidence.len(), 4);
    }

    /// 项目太少不构成"方向"（避免把单个项目吹成趋势）。
    #[test]
    fn too_few_projects_means_no_direction() {
        let input = direction_input(2, 30);
        assert!(detect_tech_directions(&input, &DetectorConfig::default()).insights.is_empty());
    }

    /// 项目都在窗口外（很久没动）不该算作"近期方向"。
    #[test]
    fn stale_projects_do_not_form_direction() {
        let input = direction_input(5, 400);
        assert!(detect_tech_directions(&input, &DetectorConfig::default()).insights.is_empty());
    }

    // ── 组合机会 ────────────────────────────────────────────────────

    fn combination_input() -> AnalysisInput {
        let mut projects = Vec::new();
        let mut relations = Vec::new();
        // 4 个项目，各持有 AI 领域下的不同能力
        let caps = ["c_video", "c_image", "c_rag", "c_agent"];
        for (i, pid) in ["p0", "p1", "p2", "p3"].iter().enumerate() {
            projects.push(project(pid, ProjectStatus::Active, Some("2026-09-01")));
            relations.push(implements(pid, caps[i]));
        }
        AnalysisInput {
            projects,
            capabilities: vec![
                cap("d_ai", "AI", CapabilityLayer::Domain, None),
                cap("c_video", "Video Generation", CapabilityLayer::Capability, Some("d_ai")),
                cap("c_image", "Image Generation", CapabilityLayer::Capability, Some("d_ai")),
                cap("c_rag", "RAG", CapabilityLayer::Capability, Some("d_ai")),
                cap("c_agent", "Agent", CapabilityLayer::Capability, Some("d_ai")),
            ],
            relations,
            now: now(),
            ..Default::default()
        }
    }

    #[test]
    fn detects_combination_hint() {
        let out = detect_combination_hints(&combination_input(), &DetectorConfig::default());
        assert_eq!(out.insights.len(), 1);
        let i = &out.insights[0];
        assert_eq!(i.insight_type, InsightType::OpportunityHint);
        assert!(i.title.contains("AI"));
        assert!(i.description.contains("4 个项目"));
        assert!(i.description.contains("能力互补"));
        assert!(i.evidence.iter().any(|e| e.kind == EvidenceKind::Project));
        assert!(i.evidence.iter().any(|e| e.kind == EvidenceKind::Symbol));
    }

    /// 证据不足（项目少或能力少）时不提示——宁缺勿滥。
    #[test]
    fn insufficient_evidence_means_no_hint() {
        let mut input = combination_input();
        input.projects.truncate(2);
        input.relations.truncate(2);
        assert!(detect_combination_hints(&input, &DetectorConfig::default()).insights.is_empty());
    }

    // ── 汇总与反馈 ──────────────────────────────────────────────────

    #[test]
    fn detect_all_merges_and_sorts_by_confidence() {
        let mut input = combination_input();
        // 加入一个跨 3 项目重复的能力
        input.relations.push(implements("p0", "c_queue"));
        input.relations.push(implements("p1", "c_queue"));
        input.relations.push(implements("p2", "c_queue"));
        input.capabilities.push(cap("d_infra", "基础设施", CapabilityLayer::Domain, None));
        input.capabilities.push(cap("c_queue", "Task Queue", CapabilityLayer::Capability, Some("d_infra")));
        input.assets.push(asset("a1", "p0", "TaskQueueImpl", AssetType::Code, 0.92));

        let out = detect_all(&input, &DetectorConfig::default());
        assert!(out.insights.len() >= 2, "应产出多类洞察");
        // 全局按置信度降序
        for w in out.insights.windows(2) {
            assert!(w[0].confidence >= w[1].confidence, "未按置信度排序");
        }
        // 每条都必须通过产品红线校验
        for i in &out.insights {
            assert!(i.validate().is_ok(), "产出的洞察必须合法: {}", i.title);
            assert!(!i.evidence.is_empty());
        }
    }

    /// 检测器 id 必须稳定：同一数据两次运行 id 一致，
    /// 否则 merge_with_feedback 无法把反馈对上号。
    #[test]
    fn insight_ids_are_stable() {
        let input = shared_capability_input(3);
        let a = detect_duplicate_capabilities(&input, &DetectorConfig::default());
        let b = detect_duplicate_capabilities(&input, &DetectorConfig::default());
        assert_eq!(a.insights[0].id, b.insights[0].id);
    }

    /// 🔴 反馈回流的前提：新一轮分析不得抹掉用户已有反馈。
    #[test]
    fn merge_preserves_user_feedback() {
        let input = shared_capability_input(3);
        let first = detect_duplicate_capabilities(&input, &DetectorConfig::default()).insights;
        let id = first[0].id.clone();

        // 用户标记为有用
        let mut with_feedback = first.clone();
        with_feedback[0].user_feedback = Some(UserFeedback::Useful);

        // 重新分析（新一轮产出 feedback = None）
        let fresh = detect_duplicate_capabilities(&input, &DetectorConfig::default()).insights;
        assert!(fresh[0].user_feedback.is_none());

        let merged = merge_with_feedback(fresh, &with_feedback);
        assert_eq!(merged[0].id, id);
        assert_eq!(merged[0].user_feedback, Some(UserFeedback::Useful), "反馈必须保留");
    }

    #[test]
    fn merge_leaves_new_insights_unread() {
        let input = shared_capability_input(3);
        let fresh = detect_duplicate_capabilities(&input, &DetectorConfig::default()).insights;
        let merged = merge_with_feedback(fresh, &[]);
        assert!(merged[0].user_feedback.is_none());
    }

    #[test]
    fn unread_filters_processed() {
        let mut a = detect_duplicate_capabilities(&shared_capability_input(2), &DetectorConfig::default()).insights;
        let b = detect_duplicate_capabilities(&shared_capability_input(3), &DetectorConfig::default()).insights;
        a[0].user_feedback = Some(UserFeedback::Useful);
        let mut all = a;
        all.extend(b);
        assert_eq!(unread(&all).len(), 1);
    }

    #[test]
    fn adoption_rate_excludes_ignored() {
        let mk = |fb: Option<UserFeedback>| Insight {
            id: format!("i{fb:?}"),
            insight_type: InsightType::ReusableComponent,
            title: "t".into(),
            description: "d".into(),
            evidence: vec![EvidenceItem { kind: EvidenceKind::File, label: "a.py".into(), target: None }],
            confidence: 0.9,
            created_at: "2026-09-29".into(),
            user_feedback: fb,
            tags: vec![],
            related_project_ids: vec![],
            related_asset_ids: vec![],
        };
        let insights = vec![
            mk(Some(UserFeedback::Useful)),
            mk(Some(UserFeedback::Useful)),
            mk(Some(UserFeedback::Useless)),
            mk(Some(UserFeedback::Ignored)),
            mk(None),
        ];
        let rate = adoption_rate(&insights).unwrap();
        assert!((rate - 2.0 / 3.0).abs() < 1e-9, "应为 2/3，实际 {rate}");
    }

    /// 无反馈时返回 None（UI 显示"暂无反馈"），不是 0%（会被误读为"全部无用"）。
    #[test]
    fn adoption_rate_none_without_feedback() {
        assert_eq!(adoption_rate(&[]), None);
    }

    // ── 空数据与配置 ────────────────────────────────────────────────

    /// 空数据库不得 panic，也不得编造洞察。
    #[test]
    fn empty_input_yields_nothing() {
        let input = AnalysisInput { now: now(), ..Default::default() };
        let cfg = DetectorConfig::default();
        assert!(detect_duplicate_capabilities(&input, &cfg).insights.is_empty());
        assert!(detect_reusable_components(&input, &cfg).insights.is_empty());
        assert!(detect_forgotten_assets(&input, &cfg).insights.is_empty());
        assert!(detect_tech_directions(&input, &cfg).insights.is_empty());
        assert!(detect_combination_hints(&input, &cfg).insights.is_empty());
        assert!(detect_all(&input, &cfg).insights.is_empty());
    }

    #[test]
    fn max_per_detector_caps_output() {
        let mut input = AnalysisInput { now: now(), ..Default::default() };
        input.projects.push(project("p1", ProjectStatus::Active, Some("2026-09-01")));
        input.assets = (0..30)
            .map(|i| asset(&format!("a{i}"), "p1", &format!("Asset{i}"), AssetType::Code, 0.9))
            .collect();
        let cfg = DetectorConfig { max_per_detector: 5, ..Default::default() };
        let out = detect_reusable_components(&input, &cfg);
        assert_eq!(out.insights.len(), 5);
    }

    #[test]
    fn confidence_floor_filters_weak_insights() {
        let input = shared_capability_input(2);
        let strict = DetectorConfig { confidence_floor: 0.99, ..Default::default() };
        let out = detect_duplicate_capabilities(&input, &strict);
        assert!(out.insights.is_empty());
        assert!(out.rejected >= 1, "应记录被拦下的候选数");
    }

    #[test]
    fn default_config_is_sane() {
        let c = DetectorConfig::default();
        assert!(c.duplicate_min_projects >= 2);
        assert!(c.reusable_min_score > 0.5);
        assert!(c.forgotten_idle_days >= 90);
        assert!(c.max_per_detector > 0);
        assert!((c.confidence_floor - projectassests_domain::CONFIDENCE_THRESHOLD).abs() < f64::EPSILON);
    }

    /// 产出的每条洞察都必须能过领域层校验（这是入库前提）。
    #[test]
    fn all_produced_insights_pass_validation() {
        let mut input = combination_input();
        let updated = (now() - chrono::Duration::days(400)).format("%Y-%m-%d").to_string();
        input.projects.push(project("stale", ProjectStatus::Paused, Some(&updated)));
        input.assets.push(asset("a_old", "stale", "OldGem", AssetType::Component, 0.88));
        input.assets.push(asset("a_new", "p0", "Shiny", AssetType::Component, 0.93));
        input.relations.push(implements("p0", "c_queue"));
        input.relations.push(implements("p1", "c_queue"));
        input.capabilities.push(cap("d_infra", "基础设施", CapabilityLayer::Domain, None));
        input.capabilities.push(cap("c_queue", "Task Queue", CapabilityLayer::Capability, Some("d_infra")));

        let out = detect_all(&input, &DetectorConfig::default());
        assert!(!out.insights.is_empty());
        for i in &out.insights {
            assert!(i.validate().is_ok(), "{} 未通过校验", i.title);
            assert!(i.confidence >= projectassests_domain::CONFIDENCE_THRESHOLD);
            assert!(!i.evidence.is_empty());
            // 每条证据的 label 不得为空（空 label 在前端渲染成空白行）
            for e in &i.evidence {
                assert!(!e.label.trim().is_empty(), "证据 label 为空: {:?}", i.title);
            }
        }
    }

    /// 无 CodeStats 的项目也要能安全处理（防御 None 字段）。
    #[test]
    fn handles_projects_without_dates() {
        let mut input = AnalysisInput { now: now(), ..Default::default() };
        input.projects.push(Project {
            id: "p1".into(),
            name: "p1".into(),
            path: "/tmp/p1".into(),
            description: String::new(),
            language: "Python".into(),
            framework: "-".into(),
            created_at: None,
            updated_at: None, // 无更新时间
            last_commit_at: None,
            status: ProjectStatus::Unknown,
            health_score: 0,
            completeness: None,
            tags: vec![],
            sensitive: false,
            stats: CodeStats::default(),
            scan: projectassests_domain::ScanFacts::default(),
            ai_profile: None,
        });
        input.assets.push(asset("a1", "p1", "X", AssetType::Code, 0.9));
        // updated_at 为 None → days_since 返回 None → 视为 i64::MAX（极久未更新）
        let out = detect_forgotten_assets(&input, &DetectorConfig::default());
        assert_eq!(out.insights.len(), 1, "无更新时间的项目应被视为长期未更新");
    }
}
