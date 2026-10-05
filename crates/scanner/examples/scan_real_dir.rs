//! 真实目录扫描验证工具。
//!
//! 用途：对本机真实目录跑一遍完整 Level 0 扫描，打印发现的项目与统计。
//! 这是"数据来自真实文件系统而非 mock"的可执行证据，也是贡献者排查
//! 扫描问题（误报/漏报/排除规则）的第一手工具。
//!
//! 用法：
//! ```text
//! cargo run -p projectassests-scanner --example scan_real_dir -- "D:/Projects"
//! cargo run -p projectassests-scanner --example scan_real_dir -- . --no-git
//! ```
//!
//! 刻意只读：本工具不写数据库、不修改任何文件。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use projectassests_scanner::{
    project_id_from_path, to_domain_project, NoProgress, ScanConfig, Scanner,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let analyze_git = !args.iter().any(|a| a == "--no-git");
    let roots: Vec<PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .collect();

    let roots = if roots.is_empty() {
        vec![std::env::current_dir().expect("无法获取当前目录")]
    } else {
        roots
    };

    println!("扫描根目录:");
    for r in &roots {
        println!("  - {}", r.display());
    }
    println!("Git 分析: {}\n", if analyze_git { "开启" } else { "关闭" });

    let scanner = Scanner::new(ScanConfig {
        roots: roots.clone(),
        analyze_git,
        ..Default::default()
    });

    let cancel = AtomicBool::new(false);
    // 说明：本 example 不提供取消入口（跑完即退出）。
    // 产品中的取消由 Job Engine 驱动同一个 AtomicBool，见 projectassests-jobs 与
    // scan.rs 的 `cancellation_stops_scan` 测试。

    let started = std::time::Instant::now();
    let outcome = match scanner.scan(&cancel, &NoProgress) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("扫描失败: {e}");
            std::process::exit(1);
        }
    };

    let now = chrono::Utc::now();
    println!("── 发现 {} 个项目（耗时 {:.2}s）──", outcome.project_count(), outcome.elapsed_ms as f64 / 1000.0);

    // 汇总统计：证明数字来自真实遍历
    let mut total_files = 0usize;
    let mut total_loc = 0usize;
    let mut lang_counts: std::collections::BTreeMap<String, usize> = Default::default();
    let mut framework_counts: std::collections::BTreeMap<String, usize> = Default::default();
    let mut status_counts: std::collections::BTreeMap<String, usize> = Default::default();

    for p in &outcome.projects {
        total_files += p.stats.files;
        total_loc += p.stats.loc;
        lang_counts
            .entry(p.language.clone())
            .and_modify(|c| *c += 1)
            .or_insert(1);
        framework_counts
            .entry(p.framework.clone())
            .and_modify(|c| *c += 1)
            .or_insert(1);
        let dom = to_domain_project(p, now);
        status_counts
            .entry(dom.status.as_str().to_string())
            .and_modify(|c| *c += 1)
            .or_insert(1);
    }

    println!("\n前 15 个项目：");
    // 表头用固定宽度对齐；最后一列（路径）直接写进格式串，
    // 避免把字面量当参数传给 `{}`（clippy::print_literal）
    println!("{:<28} {:<12} {:<14} {:>7} {:>8} {:>5}  路径",
        "名称", "语言", "框架", "文件", "代码行", "健康"
    );
    for p in outcome.projects.iter().take(15) {
        let dom = to_domain_project(p, now);
        println!(
            "{:<28} {:<12} {:<14} {:>7} {:>8} {:>5}  {}",
            truncate(&p.name, 28),
            truncate(&p.language, 12),
            truncate(&p.framework, 14),
            p.stats.files,
            p.stats.loc,
            dom.health_score,
            truncate(&p.path, 60),
        );
    }

    println!("\n── 汇总 ──");
    println!("项目总数: {}", outcome.project_count());
    println!("文件总数: {total_files}");
    println!("代码总行: {total_loc}");
    println!("遍历目录: {}（跳过 {}）", outcome.dirs_walked, outcome.dirs_skipped);
    println!("实际耗时: {:.2}s（含打印）", started.elapsed().as_secs_f64());

    print_top("语言分布", &lang_counts);
    print_top("框架分布", &framework_counts);
    print_top("状态分布", &status_counts);

    if !outcome.warnings.is_empty() {
        println!("\n── 警告（{} 条）──", outcome.warnings.len());
        for w in outcome.warnings.iter().take(10) {
            println!("  ! {w}");
        }
    }

    // 抽样展示 id 稳定性与依赖来源，便于排查"同一项目被重复登记"类问题
    if let Some(first) = outcome.projects.first() {
        println!("\n── 抽样详情: {} ──", first.name);
        println!("id       : {}", project_id_from_path(std::path::Path::new(&first.path)));
        println!("判定依据 : {}", first.marker);
        println!("依赖清单 : {}", if first.dependencies.manifest.is_empty() { "（无）" } else { &first.dependencies.manifest });
        println!("运行时依赖: {} 个，开发依赖: {} 个", first.dependencies.runtime.len(), first.dependencies.dev.len());
        println!("识别框架 : {:?}", first.dependencies.frameworks);
        println!("Git      : available={}, commits={}, 近90天={}, 最后提交={:?}",
            first.git.available, first.git.commit_count, first.git.recent_commits,
            first.git.last_commit_at.as_deref().unwrap_or("（无历史）"));
        println!("特征     : git={} readme={} tests={} license={} docker={}",
            first.detection.has_git, first.detection.has_readme, first.detection.has_tests,
            first.detection.has_license, first.detection.has_docker);
        println!("生成物跳过: {} 个文件（锁文件/压缩产物/依赖目录）", first.stats.skipped_generated);
        println!("README 摘要: {}", if first.readme_excerpt.is_empty() { "（无 README，description 留空）" } else { &first.readme_excerpt });
    }

    if outcome.cancelled {
        println!("\n（扫描被取消）");
    }
}

fn print_top(title: &str, counts: &std::collections::BTreeMap<String, usize>) {
    if counts.is_empty() {
        return;
    }
    let mut v: Vec<(&String, &usize)> = counts.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    println!("\n{title}:");
    for (k, n) in v.iter().take(8) {
        println!("  {k:<20} {n}");
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
