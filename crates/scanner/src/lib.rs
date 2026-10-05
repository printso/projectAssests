//! Spolia 扫描器：把用户磁盘上的真实目录变成结构化项目画像。
//!
//! # 模块划分
//! | 模块 | 职责 | 是否 IO |
//! |---|---|---|
//! | `markers` | 项目标记识别、内置排除清单、凭证判定 | 否（纯函数） |
//! | `language` | 扩展名→语言、代码行统计、语言构成 | 否（纯函数） |
//! | `deps` | 依赖清单解析、框架识别 | 是（读清单文件） |
//! | `git` | Git 历史分析（CLI 调用） | 是（子进程） |
//! | `scan` | 目录遍历编排、进度回调、领域模型转换 | 是（遍历） |
//!
//! 这样划分的好处：`markers` 与 `language` 是纯函数，可被穷举单测；
//! 真正碰文件系统的只有 `scan`/`deps`/`git` 三个模块，审计面很小。
//!
//! # Level 归属（《技术设计书》§14）
//! 本 crate 覆盖 **Level 0（秒级，零 LLM）** 与 Level 1 的静态部分：
//! 项目发现、语言统计、依赖解析、Git 历史、活跃度与健康度推断。
//! 符号级 AST 解析（Level 1 的另一半）在 `spolia-asset`。
//!
//! # 三条不可动摇的纪律
//! 1. **不误报**：判定基于标记文件 / .git，不靠目录名猜测
//! 2. **不编造**：没有 README 就是空描述，没有 Git 历史就不给 commit 统计
//! 3. **不读凭证**：`.env` / `*.pem` / `credentials` 等在遍历层就被拦截

mod deps;
mod git;
mod language;
mod markers;
mod scan;

pub use deps::{
    detect_frameworks, parse_manifests, Dependencies, FRAMEWORK_RULES, MAX_MANIFEST_BYTES,
};
pub use git::{
    git_version, mtime_of_newest_file, now_utc, read_branch_from_head, suggest_git_install,
    to_iso_date, GitAnalyzer,
    GitInfo,
};
pub use language::{
    count_file_lines, is_markup_or_config, language_of, FileStats, LanguageBreakdown, LanguageStat,
    StatsAccumulator, EXTENSION_LANGUAGES, GENERATED_FILENAMES, GENERATED_PATH_SEGMENTS,
    MAX_READ_BYTES,
};
pub use markers::{
    detect, is_always_excluded, is_secret_file, path_contains_excluded, Detection, ProjectKind,
    ALWAYS_EXCLUDED, MARKERS,
};
pub use scan::{
    extract_first_paragraph, project_id_from_path, read_readme_excerpt, to_domain_project,
    CandidateDir, DetectionFlags, NoProgress, ProgressSink, ScanConfig, ScanOutcome,
    ScannedProject, ScannedStats, Scanner, TAG_LIMIT,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 公共 API 必须完整可访问。
    ///
    /// 这个看似"废话"的测试有实际价值：漏导出会让上层 crate 编译失败，
    /// 而这类错误在多人协作 rebase 时很容易引入，且报错位置在别的 crate 里，
    /// 不易联想到是本 crate 的 `pub use` 少了一行。
    #[test]
    fn public_api_is_accessible() {
        // markers
        assert!(!MARKERS.is_empty());
        assert!(!ALWAYS_EXCLUDED.is_empty());
        assert!(is_always_excluded("node_modules"));
        assert!(is_secret_file("server.pem"));
        assert!(!path_contains_excluded(Path::new("/a/src/main.rs")));
        assert_eq!(ProjectKind::parse("rust"), Some(ProjectKind::Rust));

        // language
        assert_eq!(language_of(Path::new("a.rs")), Some("Rust"));
        assert!(!EXTENSION_LANGUAGES.is_empty());
        assert!(!GENERATED_FILENAMES.is_empty());
        assert!(!GENERATED_PATH_SEGMENTS.is_empty());
        assert!(is_markup_or_config("JSON"));
        assert_eq!(count_file_lines("fn main(){}\n", "Rust").code_lines, 1);
        // 断言具体值而非 `> 0`：恒真断言没有守护意义，
        // 写成确切值才能在有人误改阈值时让测试失败。
        assert_eq!(MAX_READ_BYTES, 2 * 1024 * 1024, "单文件读取上限 2 MiB");

        // deps
        assert!(!FRAMEWORK_RULES.is_empty());
        assert_eq!(MAX_MANIFEST_BYTES, 512 * 1024, "清单文件上限 512 KiB");
        assert!(detect_frameworks(&["axum".to_string()]).contains(&"Axum".to_string()));
        assert!(Dependencies::default().is_empty());

        // git
        assert!(!GitInfo::unavailable("test").available);
        assert!(suggest_git_install(&GitInfo::unavailable("无法执行 git")).is_some());
        // 不存在的路径必须降级为不可用，绝不 panic
        assert!(!GitAnalyzer::new().analyze(Path::new("/nonexistent-spolia")).available);
        assert!(to_iso_date(std::time::SystemTime::UNIX_EPOCH).is_some());

        // scan
        assert_eq!(TAG_LIMIT, 6, "UI chips 标签上限");
        assert_eq!(ScanConfig::default().max_depth, 6);
        assert_eq!(ScanOutcome::default().project_count(), 0);
        assert_eq!(
            project_id_from_path(Path::new("/tmp/x")),
            project_id_from_path(Path::new("/tmp/x"))
        );
        assert_eq!(extract_first_paragraph("# T\n\n描述。\n"), "描述。");
        NoProgress.report("s", 0, None);
        NoProgress.log("l");
    }

    /// DetectionFlags 的 From 转换必须逐项映射（漏映射会让健康度算错）。
    #[test]
    fn detection_flags_map_all_fields() {
        let d = Detection {
            marker: "Cargo.toml".into(),
            kind: ProjectKind::Rust,
            has_git: true,
            has_readme: true,
            has_tests: false,
            has_license: true,
            has_docker: false,
        };
        let f = DetectionFlags::from(&d);
        assert!(f.has_git);
        assert!(f.has_readme);
        assert!(!f.has_tests);
        assert!(f.has_license);
        assert!(!f.has_docker);
    }

    #[test]
    fn detection_flags_default_all_false() {
        let f = DetectionFlags::default();
        assert!(!f.has_git && !f.has_readme && !f.has_tests && !f.has_license && !f.has_docker);
    }

    /// ScannedStats 默认值必须是"零"而非随机数（避免 UI 显示假数据）。
    #[test]
    fn scanned_stats_default_is_zero() {
        let s = ScannedStats::default();
        assert_eq!(s.files, 0);
        assert_eq!(s.loc, 0);
        assert_eq!(s.symbols, 0);
        assert_eq!(s.modules, 0);
        assert!(s.languages.is_empty());
    }

    /// 扫描结果必须可序列化（要经 HTTP 传给前端）。
    #[test]
    fn scanned_project_serializes() {
        let p = ScannedProject {
            id: "p_x_1".into(),
            name: "x".into(),
            path: "/tmp/x".into(),
            kind: ProjectKind::Rust,
            marker: "Cargo.toml".into(),
            language: "Rust".into(),
            framework: "Axum".into(),
            tags: vec!["axum".into()],
            dependencies: Dependencies::default(),
            git: GitInfo::unavailable("无 .git"),
            detection: DetectionFlags::default(),
            stats: ScannedStats::default(),
            top_level_dirs: vec!["src".into()],
            top_level_files: vec!["Cargo.toml".into()],
            readme_excerpt: String::new(),
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"kind\":\"rust\""));
        assert!(json.contains("\"marker\":\"Cargo.toml\""));
        // 反序列化必须能还原（API 契约稳定性）
        let back: ScannedProject = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, p.id);
        assert_eq!(back.kind, ProjectKind::Rust);
    }
}
