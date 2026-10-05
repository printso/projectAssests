//! 项目内源文件遍历（Level 1 符号抽取的输入）。
//!
//! # 为什么不复用扫描器的遍历
//! `spolia_scanner` 的遍历目标是**统计**（数行数、判生成物、算语言构成），
//! 它刻意不返回文件内容——Level 0 要快，不能把 9 万个文件读进内存。
//! 本模块的目标相反：只对**值得抽取符号**的文件读内容，
//! 且必须受严格的数量与体积上限约束。
//!
//! 两者共享排除规则（`markers::is_always_excluded` / `path_contains_excluded`），
//! 保证"扫描时跳过的目录，抽取时也不会读"——这是隐私底线的一致性来源。

use std::path::{Path, PathBuf};

use spolia_scanner::{is_always_excluded, language_of, path_contains_excluded, is_secret_file};

/// 单个文件的最大读取字节数。
///
/// 超过此体积的通常是生成物、数据文件或压缩包，
/// 读它既慢又抽不出有意义的符号。
pub const MAX_FILE_BYTES: u64 = 512 * 1024;

/// 单项目最多读取的文件数。
///
/// 🔴 必须有上限：用户可能授权一个巨型 monorepo，
/// 无上限会让"索引代码"跑上几十分钟，而任务队列只有这一条线程在干活，
/// 后续所有分析都会被它堵住。
pub const MAX_FILES_PER_PROJECT: usize = 3_000;

/// 项目内最大遍历深度。
pub const MAX_DEPTH: usize = 10;

/// 一个可读的源文件。
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// 相对项目根的路径（用 `/` 分隔，与数据库口径一致）
    pub relative_path: String,
    /// 绝对路径（读取用，不入库）
    pub absolute_path: PathBuf,
    /// 语言名（`language_of` 的返回值）
    pub language: &'static str,
}

/// 列目录阶段的统计（诊断"为什么这个项目抽出的符号这么少"）。
///
/// 🔴 只包含**列目录阶段**能判定的计数。
/// 早期版本里有 `read` 与 `skipped_binary` 两个字段，但本阶段根本不读内容，
/// 于是它们永远是 0——"生产者填不满的结构体"会让读代码的人误以为统计有漏洞。
/// 读取阶段的计数（二进制跳过）由调用方自己统计，见 `read_source`。
#[derive(Debug, Clone, Default)]
pub struct WalkStats {
    /// 通过全部过滤、待读取的源文件数
    pub listed: usize,
    /// 因体积过大跳过（在读取**之前**判定，避免为注定丢弃的文件付 IO）
    pub skipped_large: usize,
    /// 因排除规则跳过（node_modules/.git/dist/target 等）
    pub skipped_excluded: usize,
    /// 🔒 凭证文件：只计数，绝不读取内容
    pub skipped_secret: usize,
    /// 因达到 `MAX_FILES_PER_PROJECT` 上限而截断
    pub truncated: bool,
}

impl WalkStats {
    /// 人类可读的跳过原因汇总（写入任务日志，帮助用户理解结果）。
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = vec![format!("发现 {} 个源文件", self.listed)];
        if self.skipped_secret > 0 {
            parts.push(format!("跳过 {} 个凭证文件", self.skipped_secret));
        }
        if self.skipped_large > 0 {
            parts.push(format!("跳过 {} 个超大文件", self.skipped_large));
        }
        if self.truncated {
            parts.push(format!("已达单项目 {} 文件上限", MAX_FILES_PER_PROJECT));
        }
        parts.join("，")
    }
}

/// 列出项目内值得抽取符号的源文件。
///
/// **不读取内容**：内容由调用方按需读（配合 `read_source`），
/// 这样调用方可以先看数量决定是否继续，避免为注定丢弃的文件付 IO 成本
/// （这是扫描器早期 204 秒性能事故的根因，不能重犯）。
pub fn list_source_files(project_root: &Path) -> (Vec<SourceFile>, WalkStats) {
    let mut files: Vec<SourceFile> = Vec::new();
    let mut stats = WalkStats::default();

    // 广度优先：保证浅层文件（通常是核心模块）先被处理，
    // 达到上限被截断时丢弃的是深层文件，符合"重要的先抽"的直觉。
    let mut queue: Vec<(PathBuf, usize)> = vec![(project_root.to_path_buf(), 0)];

    while let Some((dir, depth)) = queue.pop() {
        if files.len() >= MAX_FILES_PER_PROJECT {
            stats.truncated = true;
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            // 权限问题：跳过该子树，不算致命错误（扫描器同样处理）
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

            if is_dir {
                // 目录：命中排除规则就整棵子树不下探（node_modules/.git/dist/target）
                if is_always_excluded(&name) || path_contains_excluded(&path) {
                    stats.skipped_excluded += 1;
                    continue;
                }
                if depth < MAX_DEPTH {
                    queue.push((path, depth + 1));
                }
                continue;
            }

            // 🔒 文件：**凭证判定必须排在通用排除之前**。
            // `.env` 同时命中 `is_secret_file` 与 `is_always_excluded`，
            // 若先查排除规则，它会被计入 `skipped_excluded`——
            // 于是"跳过了 N 个凭证文件"永远显示 0，
            // 而这条恰恰是用户最该看到的隐私保证。
            if is_secret_file(&name) {
                stats.skipped_secret += 1;
                continue;
            }
            if is_always_excluded(&name) {
                stats.skipped_excluded += 1;
                continue;
            }
            let Some(language) = language_of(&path) else {
                continue; // 非源码（图片/字体/未知扩展名）
            };
            // 体积过滤放在读取之前：为注定丢弃的文件付 IO 是最贵的浪费
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.len() > MAX_FILE_BYTES {
                stats.skipped_large += 1;
                continue;
            }

            if files.len() >= MAX_FILES_PER_PROJECT {
                stats.truncated = true;
                break;
            }
            files.push(SourceFile {
                relative_path: relative_to(project_root, &path),
                absolute_path: path,
                language,
            });
        }
    }

    // 排序保证确定性：同一项目两次抽取产出相同顺序，
    // 便于比对与回归测试（文件系统遍历顺序在不同平台并不稳定）
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    stats.listed = files.len();
    (files, stats)
}

/// 读取源文件内容。返回 `None` 表示不可读（二进制/编码异常）——
/// 这不是错误：调用方跳过该文件并自行计数即可，
/// 一个非 UTF-8 文件不该让整轮抽取失败。
pub fn read_source(file: &SourceFile) -> Option<String> {
    std::fs::read_to_string(&file.absolute_path).ok()
}

/// 项目根下的顶层目录名（模块结构信号）。
///
/// 排除 `node_modules`/`target`/`dist` 等：把它们算作"模块"会让
/// 一个 Node 项目的模块数虚高到几十，而真实业务模块只有五六个。
///
/// 排序保证确定性：同一目录两次调用结果一致。
pub fn top_dirs_of(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<String> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            e.file_type().map(|t| t.is_dir()).unwrap_or(false) && !is_always_excluded(&name)
        })
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    dirs.sort();
    dirs
}

/// 计算绝对路径相对项目根的相对路径，统一用 `/` 分隔。
///
/// 🔴 必须存相对路径：数据库可跨机器复制，
/// 且派生数据里不该固化用户的目录结构（隐私）。
fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 造一个临时项目：`(相对路径, 内容)` 列表。
    fn temp_project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let p = dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        dir
    }

    #[test]
    fn lists_source_files_with_relative_paths() {
        let dir = temp_project(&[
            ("src/main.py", "def main():\n    pass\n"),
            ("src/util/helper.py", "def help():\n    pass\n"),
        ]);
        let (files, stats) = list_source_files(dir.path());
        assert_eq!(files.len(), 2);
        assert_eq!(stats.listed, 2, "listed 应等于待读取文件数");
        // 统一正斜杠：Windows 上 strip_prefix 会得到反斜杠
        assert!(files.iter().all(|f| !f.relative_path.contains('\\')));
        assert!(files.iter().any(|f| f.relative_path == "src/main.py"));
        assert_eq!(files[0].language, "Python");
    }

    /// 确定性：同一目录两次遍历必须给出相同顺序。
    #[test]
    fn listing_is_deterministic() {
        let dir = temp_project(&[
            ("b.py", "x=1\n"),
            ("a.py", "y=2\n"),
            ("c/d.py", "z=3\n"),
        ]);
        let first: Vec<String> = list_source_files(dir.path())
            .0
            .iter()
            .map(|f| f.relative_path.clone())
            .collect();
        for _ in 0..3 {
            let again: Vec<String> = list_source_files(dir.path())
                .0
                .iter()
                .map(|f| f.relative_path.clone())
                .collect();
            assert_eq!(first, again, "遍历顺序必须稳定");
        }
        assert_eq!(first, vec!["a.py", "b.py", "c/d.py"]);
    }

    /// 排除规则必须与扫描器一致：node_modules 里的代码不得被抽取。
    #[test]
    fn excluded_directories_are_not_walked() {
        let dir = temp_project(&[
            ("src/app.js", "function a(){}\n"),
            ("node_modules/lib/index.js", "function b(){}\n"),
            (".git/hooks/pre-commit", "#!/bin/sh\n"),
            ("dist/bundle.js", "var x=1;\n"),
            ("target/debug/main.rs", "fn main(){}\n"),
        ]);
        let (files, stats) = list_source_files(dir.path());
        let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(paths, vec!["src/app.js"], "只应留下真实源码: {paths:?}");
        assert!(stats.skipped_excluded > 0, "应统计被排除的条目");
    }

    /// 🔒 凭证文件绝不读取——连内容都不该进内存。
    #[test]
    fn secret_files_are_never_read() {
        let dir = temp_project(&[
            ("src/main.py", "print(1)\n"),
            (".env", "API_KEY=***"),
            ("id_rsa", "-----BEGIN PRIVATE KEY-----\n"),
        ]);
        let (files, stats) = list_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "src/main.py");
        assert!(
            stats.skipped_secret > 0,
            "凭证文件应被计数为跳过，实际 {stats:?}"
        );
    }

    #[test]
    fn large_files_are_skipped_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat((MAX_FILE_BYTES + 1024) as usize);
        fs::write(dir.path().join("huge.py"), &big).unwrap();
        fs::write(dir.path().join("small.py"), "x=1\n").unwrap();

        let (files, stats) = list_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "small.py");
        assert_eq!(stats.skipped_large, 1);
    }

    #[test]
    fn binary_files_return_none_from_read() {
        let dir = tempfile::tempdir().unwrap();
        // 非法 UTF-8 字节序列
        fs::write(dir.path().join("data.py"), [0xff_u8, 0xfe, 0x00]).unwrap();
        let (files, _) = list_source_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(read_source(&files[0]).is_none(), "二进制应返回 None");
    }

    #[test]
    fn read_source_returns_content() {
        let dir = temp_project(&[("a.py", "def f():\n    return 1\n")]);
        let (files, _) = list_source_files(dir.path());
        let content = read_source(&files[0]).unwrap();
        assert!(content.contains("def f()"));
    }

    #[test]
    fn file_count_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        // 造出超过上限的文件数（用小文件，避免测试太慢）
        let total = MAX_FILES_PER_PROJECT + 50;
        for i in 0..total {
            fs::write(dir.path().join(format!("f{i}.py")), "x=1\n").unwrap();
        }
        let (files, stats) = list_source_files(dir.path());
        assert!(
            files.len() <= MAX_FILES_PER_PROJECT,
            "不得超过上限，实际 {}",
            files.len()
        );
        assert!(stats.truncated, "截断必须被标记，否则用户以为抽全了");
        assert!(stats.summary().contains("上限"));
    }

    #[test]
    fn depth_is_capped() {
        let mut deep = "a".to_string();
        for i in 0..(MAX_DEPTH + 5) {
            deep = format!("{deep}/d{i}");
        }
        let dir = temp_project(&[(
            &format!("{deep}/leaf.py"),
            "x=1\n",
        )]);
        let (files, _) = list_source_files(dir.path());
        assert!(
            files.is_empty(),
            "超过最大深度的文件不应被读取，实际 {:?}",
            files.iter().map(|f| &f.relative_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn empty_project_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let (files, stats) = list_source_files(dir.path());
        assert!(files.is_empty());
        assert_eq!(stats.listed, 0);
        assert!(!stats.truncated);
        assert!(stats.summary().contains("发现 0 个源文件"));
    }

    /// 不存在的路径不得 panic（项目可能被用户在扫描后删除）。
    #[test]
    fn missing_project_returns_empty() {
        let (files, stats) = list_source_files(Path::new("/nonexistent-spolia-project"));
        assert!(files.is_empty());
        assert_eq!(stats.listed, 0);
    }

    #[test]
    fn summary_lists_all_skip_reasons() {
        let mut s = WalkStats {
            listed: 5,
            skipped_large: 2,
            skipped_excluded: 9,
            skipped_secret: 1,
            truncated: true,
        };
        let text = s.summary();
        assert!(text.contains("发现 5 个源文件"));
        assert!(text.contains("凭证"));
        assert!(text.contains("超大"));
        assert!(text.contains("上限"));
        // 排除数不进摘要：它对用户无意义（node_modules 本就该跳过）
        s.skipped_excluded = 0;
        assert!(s.summary().contains("发现 5 个源文件"));
    }

    /// 摘要在没有任何跳过时不应出现"跳过"字样（避免噪音）。
    #[test]
    fn summary_omits_zero_counts() {
        let s = WalkStats {
            listed: 3,
            ..Default::default()
        };
        let text = s.summary();
        assert!(text.contains("发现 3 个源文件"));
        assert!(!text.contains("凭证"), "计数为 0 时不该提及: {text}");
        assert!(!text.contains("超大"));
        assert!(!text.contains("上限"));
    }

    #[test]
    fn constants_are_sane() {
        assert_eq!(MAX_FILE_BYTES, 512 * 1024);
        assert_eq!(MAX_FILES_PER_PROJECT, 3_000);
        assert_eq!(MAX_DEPTH, 10);
    }
}
