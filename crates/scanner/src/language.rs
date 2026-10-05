//! 语言识别与代码统计（Level 0，**零 LLM 成本**）。
//!
//! 《产品设计书》核心判断 #4：不用 LLM 能拿到的绝不调 LLM。
//! 语言构成、代码行数、文件数都属于这一类——纯确定性统计，秒级完成。
//!
//! 🔴 统计口径纪律：**只统计代码文件**。
//! 若把 `package-lock.json`、`.min.js`、`yarn.lock` 这类生成物计入，
//! 一个空壳 Node 项目会显示"5 万行 JavaScript"，健康度与规模全部失真。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 扩展名 → 语言名。
///
/// 只收录能明确判定语言的扩展名。未知扩展名归入 `Other` 并计入文件数，
/// 但**不计入语言构成百分比**（避免"未知"占据主导让饼图失去意义）。
pub const EXTENSION_LANGUAGES: &[(&str, &str)] = &[
    // ── 主流语言 ────────────────────────────────────────────────
    ("rs", "Rust"),
    ("py", "Python"),
    ("pyi", "Python"),
    ("js", "JavaScript"),
    ("jsx", "JavaScript"),
    ("mjs", "JavaScript"),
    ("cjs", "JavaScript"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("mts", "TypeScript"),
    ("cts", "TypeScript"),
    ("go", "Go"),
    ("java", "Java"),
    ("kt", "Kotlin"),
    ("kts", "Kotlin"),
    ("scala", "Scala"),
    ("c", "C"),
    ("h", "C"),
    ("cpp", "C++"),
    ("cc", "C++"),
    ("cxx", "C++"),
    ("hpp", "C++"),
    ("hh", "C++"),
    ("cs", "C#"),
    ("swift", "Swift"),
    ("m", "Objective-C"),
    ("mm", "Objective-C++"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("pl", "Perl"),
    ("pm", "Perl"),
    ("lua", "Lua"),
    ("r", "R"),
    ("jl", "Julia"),
    ("dart", "Dart"),
    ("ex", "Elixir"),
    ("exs", "Elixir"),
    ("erl", "Erlang"),
    ("hs", "Haskell"),
    ("clj", "Clojure"),
    ("cljs", "Clojure"),
    ("elm", "Elm"),
    ("zig", "Zig"),
    ("nim", "Nim"),
    ("groovy", "Groovy"),
    ("gradle", "Groovy"),
    // ── 前端标记/样式（计入构成，它们确实是手写代码）──────────────
    ("vue", "Vue"),
    ("svelte", "Svelte"),
    ("css", "CSS"),
    ("scss", "SCSS"),
    ("sass", "Sass"),
    ("less", "Less"),
    ("styl", "Stylus"),
    // ── 数据与配置（手写，计入）────────────────────────────────
    ("sql", "SQL"),
    ("sh", "Shell"),
    ("bash", "Shell"),
    ("zsh", "Shell"),
    ("fish", "Shell"),
    ("ps1", "PowerShell"),
    ("bat", "Batch"),
    ("yaml", "YAML"),
    ("yml", "YAML"),
    ("toml", "TOML"),
    ("ini", "INI"),
    ("proto", "Protocol Buffers"),
    ("graphql", "GraphQL"),
    ("gql", "GraphQL"),
    // ── 文档与标记 ─────────────────────────────────────────────
    ("md", "Markdown"),
    ("mdx", "Markdown"),
    ("rst", "reStructuredText"),
    ("html", "HTML"),
    ("htm", "HTML"),
    ("xml", "XML"),
    ("json", "JSON"),
    ("jsonc", "JSON"),
];

/// 不计入代码统计的文件名模式（生成物 / 锁文件 / 压缩产物）。
///
/// 这些文件可以有几十万行，但**不是用户写的**，计入会彻底扭曲规模判断。
pub const GENERATED_FILENAMES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "npm-shrinkwrap.json",
    "composer.lock",
    "cargo.lock",
    "poetry.lock",
    "pipfile.lock",
    "gemfile.lock",
    "pubspec.lock",
    "go.sum",
    "bun.lockb",
    "deno.lock",
    "uv.lock",
    "gradle.lockfile",
    "flask.lock",
    "build.gradle.lockfile",
];

/// 不计入统计的路径片段（生成目录）。
pub const GENERATED_PATH_SEGMENTS: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "out",
    "target",
    ".next",
    ".nuxt",
    ".output",
    "coverage",
    "__pycache__",
    ".venv",
    "venv",
    "site-packages",
    "vendor",
    ".git",
    "min",
    "generated",
    "migrations", // 迁移文件多为工具生成，且行数巨大
];

/// 单个文件的统计贡献。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileStats {
    /// 代码行（非空、非纯注释行）
    pub code_lines: usize,
    /// 总行数
    pub total_lines: usize,
    /// 是否为注释主导的文件（用于识别"文档型"文件）
    pub comment_lines: usize,
}

/// 代码统计累加器。
///
/// 用法：遍历文件时逐个 `add()`，最后 `finish()` 得到语言构成。
/// 把累加逻辑集中在一个结构体里，避免在遍历循环中散落 HashMap 操作。
#[derive(Debug, Clone, Default)]
pub struct StatsAccumulator {
    /// 语言 → (代码行, 文件数)
    by_language: HashMap<String, (usize, usize)>,
    /// 未知扩展名的文件数（只计数，不进构成）
    unknown_files: usize,
    /// 被判定为生成物而跳过的文件数（诊断用：可验证排除规则是否过激）
    skipped_generated: usize,
    total_files: usize,
    total_code_lines: usize,
}

impl StatsAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 该文件是否应被排除在代码统计之外。
    ///
    /// 判定顺序（先文件名再路径，两者都便宜）：
    /// 1. 锁文件等已知生成物文件名
    /// 2. 压缩产物（`.min.js` / `.min.css` / `.bundle.js`）
    /// 3. 路径中含生成目录
    pub fn is_generated(path: &Path) -> bool {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if GENERATED_FILENAMES
            .iter()
            .any(|g| file_name == g.to_lowercase())
        {
            return true;
        }
        // 压缩产物：`.min.js`、`.min.css`、`.bundle.js`、`.map`
        if file_name.ends_with(".min.js")
            || file_name.ends_with(".min.css")
            || file_name.ends_with(".bundle.js")
            || file_name.ends_with(".map")
        {
            return true;
        }
        // 路径片段（大小写不敏感比较，Windows 下常见 `Dist`）
        path.components().any(|c| {
            let s = c.as_os_str().to_string_lossy().to_lowercase();
            GENERATED_PATH_SEGMENTS.iter().any(|g| s == *g)
        })
    }

    /// 加入一个文件的统计。`language` 为 `None` 表示未知扩展名。
    pub fn add(&mut self, path: &Path, language: Option<&str>, stats: FileStats) {
        self.total_files += 1;
        if Self::is_generated(path) {
            self.skipped_generated += 1;
            return;
        }
        match language {
            Some(lang) => {
                let entry = self.by_language.entry(lang.to_string()).or_insert((0, 0));
                entry.0 += stats.code_lines;
                entry.1 += 1;
                self.total_code_lines += stats.code_lines;
            }
            None => self.unknown_files += 1,
        }
    }

    /// 只登记文件数（不读内容，用于二进制/超大文件）。
    pub fn add_unread(&mut self, path: &Path) {
        self.total_files += 1;
        if Self::is_generated(path) {
            self.skipped_generated += 1;
        }
    }

    /// 产出语言构成（按代码行降序，百分比取整）。
    pub fn finish(&self) -> LanguageBreakdown {
        let mut langs: Vec<LanguageStat> = self
            .by_language
            .iter()
            .map(|(name, (loc, files))| LanguageStat {
                name: name.clone(),
                loc: *loc,
                files: *files,
                pct: 0,
            })
            .collect();
        langs.sort_by(|a, b| b.loc.cmp(&a.loc).then_with(|| a.name.cmp(&b.name)));

        let total: usize = langs.iter().map(|l| l.loc).sum();
        if total > 0 {
            // 用"最大余数法"分配百分比，保证总和恰为 100。
            // 直接四舍五入会出现 99% 或 101%（原型期设计稿就有这个问题），
            // 前端饼图/进度条看起来像 bug。
            let raw: Vec<f64> = langs
                .iter()
                .map(|l| l.loc as f64 * 100.0 / total as f64)
                .collect();
            let mut floored: Vec<u8> = raw.iter().map(|v| *v as u8).collect();
            let mut assigned: u16 = floored.iter().map(|v| u16::from(*v)).sum();
            // 按小数部分降序补齐差额
            let mut order: Vec<usize> = (0..raw.len()).collect();
            order.sort_by(|&a, &b| {
                let fa = raw[a] - f64::from(floored[a]);
                let fb = raw[b] - f64::from(floored[b]);
                fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal)
            });
            // 差额最多为 lang 数-1，用取模轮转分配（防御性避免死循环）
            let mut i = 0;
            while assigned < 100 && !order.is_empty() {
                floored[order[i % order.len()]] += 1;
                assigned += 1;
                i += 1;
            }
            for (lang, pct) in langs.iter_mut().zip(floored) {
                lang.pct = pct;
            }
        }

        LanguageBreakdown {
            languages: langs,
            total_files: self.total_files,
            total_code_lines: self.total_code_lines,
            unknown_files: self.unknown_files,
            skipped_generated: self.skipped_generated,
        }
    }

    /// 主语言（代码行最多的**真实编程语言**）。
    ///
    /// 🔴 必须排除标记/配置语言：一个 Rust 项目的 `Cargo.toml` 常有 4~5 行，
    /// 而 `main.rs` 可能只有 3 行。若按纯行数排序，"主语言"会变成 TOML——
    /// 项目列表里满屏 TOML/JSON/Markdown 会让语言筛选与统计彻底失真。
    ///
    /// 回退顺序：真实编程语言 → 占比最高的任意语言 → 空串（**不编造**）。
    pub fn primary_language(&self) -> String {
        let breakdown = self.finish();
        breakdown.primary().unwrap_or("").to_string()
    }
}

/// 语言构成结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LanguageBreakdown {
    pub languages: Vec<LanguageStat>,
    pub total_files: usize,
    pub total_code_lines: usize,
    pub unknown_files: usize,
    /// 因判定为生成物而跳过的文件数
    pub skipped_generated: usize,
}

impl LanguageBreakdown {
    /// 主语言（复用 `pick_primary_language`，保证与扫描器口径一致）。
    pub fn primary(&self) -> Option<&str> {
        pick_primary_language(&self.languages)
    }
}

/// 单语言统计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageStat {
    pub name: String,
    pub loc: usize,
    pub files: usize,
    /// 0-100，所有项之和恰为 100（有代码时）
    pub pct: u8,
}

/// 扩展名 → 语言名。未知返回 `None`。
pub fn language_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    EXTENSION_LANGUAGES
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, lang)| *lang)
}

/// 是否为标记语言/配置格式（不作为项目"主语言"）。
pub fn is_markup_or_config(language: &str) -> bool {
    matches!(
        language,
        "Markdown"
            | "reStructuredText"
            | "HTML"
            | "XML"
            | "JSON"
            | "YAML"
            | "TOML"
            | "INI"
            | "CSS"
            | "SCSS"
            | "Sass"
            | "Less"
            | "Stylus"
    )
}

/// 从"按代码行降序"的语言列表中选出主语言。
///
/// 🔴 这是全项目**唯一**的主语言判定入口。
/// 曾经 `scan.rs` 直接取 `languages.first()`，导致 Cargo.toml 行数超过 main.rs 时
/// 把 Rust 项目标成 TOML。规则集中在此，避免同一判断在两处各写一遍而漂移。
///
/// 选择顺序：第一个真实编程语言 → 第一个任意语言 → None（不编造）。
pub fn pick_primary_language<'a, I, N>(languages: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a N>,
    N: HasLanguageName + 'a,
{
    let mut first: Option<&'a str> = None;
    for l in languages {
        let name = l.language_name();
        if first.is_none() {
            first = Some(name);
        }
        if !is_markup_or_config(name) {
            return Some(name);
        }
    }
    first
}

/// 取语言名的能力（让 `pick_primary_language` 同时适用于
/// `LanguageStat` 与 `projectassests_domain::LanguageShare` 两种结构）。
pub trait HasLanguageName {
    fn language_name(&self) -> &str;
}

impl HasLanguageName for LanguageStat {
    fn language_name(&self) -> &str {
        &self.name
    }
}

impl HasLanguageName for projectassests_domain::LanguageShare {
    fn language_name(&self) -> &str {
        &self.name
    }
}

/// 该语言是否使用 `#` 作为行注释（影响代码行统计的注释识别）。
fn uses_hash_comments(language: &str) -> bool {
    matches!(
        language,
        "Python"
            | "Ruby"
            | "Shell"
            | "Perl"
            | "R"
            | "Elixir"
            | "YAML"
            | "TOML"
            | "INI"
            | "PowerShell"
            | "Julia"
            | "Nim"
    )
}

/// 统计单个文件的代码行。
///
/// 代码行 = 非空行 且 非纯注释行。
/// 刻意**不做**完整的块注释状态机（`/* ... */` 跨行）——那需要按语言写解析器，
/// 而 Level 0 的目标是"秒级、够用"。行级启发式对规模判断已足够准确，
/// 精确的符号级统计留给 Level 1 的 Tree-sitter（《技术设计书》§14）。
///
/// `max_bytes`：超过此大小的文件不读内容（避免误读大文件拖慢扫描），只计文件数。
pub fn count_file_lines(content: &str, language: &str) -> FileStats {
    let hash_comments = uses_hash_comments(language);
    let mut code = 0usize;
    let mut comments = 0usize;
    let mut total = 0usize;
    let mut in_block_comment = false;

    for line in content.lines() {
        total += 1;
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        // 行注释
        let is_line_comment = if hash_comments {
            t.starts_with('#')
        } else {
            t.starts_with("//") || t.starts_with("--")
        };

        // 块注释：/* */、<!-- -->、''' """（Python 文档串按块处理过于复杂，跳过）
        if !hash_comments {
            if in_block_comment {
                comments += 1;
                if t.contains("*/") || t.contains("-->") {
                    in_block_comment = false;
                }
                continue;
            }
            if t.starts_with("/*") || t.starts_with("<!--") {
                comments += 1;
                if !(t.contains("*/") || t.contains("-->")) {
                    in_block_comment = true;
                }
                continue;
            }
        }

        if is_line_comment {
            comments += 1;
        } else {
            code += 1;
        }
    }

    FileStats {
        code_lines: code,
        total_lines: total,
        comment_lines: comments,
    }
}

/// 读取文件内容的最大字节数（超过则跳过内容统计）。
///
/// 2 MiB：正常源码文件极少超过此值；超过的基本是生成物或数据文件，
/// 读它们既慢又会污染统计。
pub const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    // ── 语言识别 ────────────────────────────────────────────────────

    #[test]
    fn detects_common_languages() {
        assert_eq!(language_of(&p("main.rs")), Some("Rust"));
        assert_eq!(language_of(&p("app.py")), Some("Python"));
        assert_eq!(language_of(&p("index.tsx")), Some("TypeScript"));
        assert_eq!(language_of(&p("index.jsx")), Some("JavaScript"));
        assert_eq!(language_of(&p("main.go")), Some("Go"));
        assert_eq!(language_of(&p("A.java")), Some("Java"));
        assert_eq!(language_of(&p("x.cpp")), Some("C++"));
        assert_eq!(language_of(&p("x.vue")), Some("Vue"));
        assert_eq!(language_of(&p("x.kt")), Some("Kotlin"));
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert_eq!(language_of(&p("MAIN.RS")), Some("Rust"));
        assert_eq!(language_of(&p("App.Py")), Some("Python"));
    }

    #[test]
    fn unknown_extension_returns_none() {
        assert_eq!(language_of(&p("data.bin")), None);
        assert_eq!(language_of(&p("noext")), None);
        assert_eq!(language_of(&p("image.png")), None);
    }

    #[test]
    fn markup_and_config_are_classified() {
        assert!(is_markup_or_config("Markdown"));
        assert!(is_markup_or_config("JSON"));
        assert!(is_markup_or_config("CSS"));
        assert!(!is_markup_or_config("Rust"));
        assert!(!is_markup_or_config("Python"));
        assert!(!is_markup_or_config("TypeScript"));
    }

    // ── 生成物排除 ──────────────────────────────────────────────────

    /// 🔴 规模统计的核心正确性：锁文件动辄数万行，计入会彻底失真。
    #[test]
    fn lock_files_are_generated() {
        for f in [
            "package-lock.json",
            "yarn.lock",
            "pnpm-lock.yaml",
            "Cargo.lock",
            "poetry.lock",
            "go.sum",
            "composer.lock",
            "uv.lock",
        ] {
            assert!(StatsAccumulator::is_generated(&p(f)), "{f} 应判定为生成物");
            assert!(
                StatsAccumulator::is_generated(&p(&format!("/proj/{f}"))),
                "带路径的 {f} 也应判定"
            );
        }
    }

    #[test]
    fn minified_and_maps_are_generated() {
        assert!(StatsAccumulator::is_generated(&p("app.min.js")));
        assert!(StatsAccumulator::is_generated(&p("style.min.css")));
        assert!(StatsAccumulator::is_generated(&p("main.bundle.js")));
        assert!(StatsAccumulator::is_generated(&p("app.js.map")));
    }

    #[test]
    fn generated_dirs_are_excluded() {
        for path in [
            "/p/node_modules/x/index.js",
            "/p/dist/main.js",
            "/p/build/out.rs",
            "/p/target/debug/x.rs",
            "/p/.next/static/a.js",
            "/p/coverage/lcov.js",
            "/p/migrations/0001.sql",
        ] {
            assert!(StatsAccumulator::is_generated(&p(path)), "{path} 应排除");
        }
    }

    #[test]
    fn generated_detection_is_case_insensitive() {
        assert!(StatsAccumulator::is_generated(&p("/p/Node_Modules/x.js")));
        assert!(StatsAccumulator::is_generated(&p("/p/DIST/main.js")));
        assert!(StatsAccumulator::is_generated(&p("/p/PACKAGE-LOCK.JSON")));
    }

    #[test]
    fn normal_source_is_not_generated() {
        for path in [
            "/p/src/main.rs",
            "/p/app/views/index.tsx",
            "/p/lib/utils.py",
            "/p/tests/test_a.py",
            "/p/Cargo.toml",
        ] {
            assert!(!StatsAccumulator::is_generated(&p(path)), "{path} 不应排除");
        }
    }

    // ── 代码行统计 ──────────────────────────────────────────────────

    #[test]
    fn counts_code_excluding_blank_and_comments() {
        let src = "// 文件头注释\n\nfn main() {\n    // 行内注释\n    println!(\"hi\");\n}\n";
        let s = count_file_lines(src, "Rust");
        assert_eq!(s.total_lines, 6);
        assert_eq!(s.comment_lines, 2);
        assert_eq!(s.code_lines, 3, "fn / println / }} 三行是代码");
    }

    #[test]
    fn hash_comments_for_python_and_shell() {
        let src = "#!/usr/bin/env python\n# 注释\nimport os\n\nprint(os.getcwd())\n";
        let s = count_file_lines(src, "Python");
        assert_eq!(s.comment_lines, 2);
        assert_eq!(s.code_lines, 2);
    }

    /// Shell 的 `#` 是注释，Rust 的不是——注释语法必须按语言区分。
    #[test]
    fn hash_is_not_comment_in_rust() {
        let s = count_file_lines("#[derive(Debug)]\nstruct A;\n", "Rust");
        assert_eq!(s.comment_lines, 0, "Rust 的属性宏不是注释");
        assert_eq!(s.code_lines, 2);
    }

    #[test]
    fn multiline_block_comment_counted_once_per_line() {
        let src = "/* 第一行\n   第二行\n   第三行 */\nint x = 1;\n";
        let s = count_file_lines(src, "C");
        assert_eq!(s.comment_lines, 3);
        assert_eq!(s.code_lines, 1);
    }

    #[test]
    fn single_line_block_comment_does_not_leak_state() {
        let src = "/* 单行块注释 */\nint x = 1;\nint y = 2;\n";
        let s = count_file_lines(src, "C");
        assert_eq!(s.comment_lines, 1);
        assert_eq!(s.code_lines, 2, "块注释闭合后不应继续吞掉代码行");
    }

    #[test]
    fn html_comment_supported() {
        let src = "<!-- 注释 -->\n<div>hi</div>\n";
        let s = count_file_lines(src, "HTML");
        assert_eq!(s.comment_lines, 1);
        assert_eq!(s.code_lines, 1);
    }

    #[test]
    fn sql_line_comment_supported() {
        let src = "-- 注释\nSELECT 1;\n";
        let s = count_file_lines(src, "SQL");
        assert_eq!(s.comment_lines, 1);
        assert_eq!(s.code_lines, 1);
    }

    #[test]
    fn empty_file_yields_zero() {
        assert_eq!(count_file_lines("", "Rust"), FileStats::default());
        assert_eq!(count_file_lines("\n\n\n", "Rust").code_lines, 0);
    }

    // ── 百分比分配 ──────────────────────────────────────────────────

    /// 百分比之和必须恰为 100：直接四舍五入会给出 99 或 101。
    #[test]
    fn percentages_sum_to_exactly_100() {
        let mut acc = StatsAccumulator::new();
        // 构造三等分：33.33% × 3，朴素四舍五入会得 99
        for i in 0..3 {
            let lang = ["Rust", "Python", "Go"][i];
            for j in 0..100 {
                acc.add(
                    &p(&format!("/p/f{i}_{j}.x")),
                    Some(lang),
                    FileStats { code_lines: 1, total_lines: 1, comment_lines: 0 },
                );
            }
        }
        let b = acc.finish();
        let sum: u16 = b.languages.iter().map(|l| u16::from(l.pct)).sum();
        assert_eq!(sum, 100, "三等分应凑满 100，实际 {sum}（各项 {:?}）",
            b.languages.iter().map(|l| l.pct).collect::<Vec<_>>());
    }

    #[test]
    fn percentages_sum_to_100_with_seven_languages() {
        let mut acc = StatsAccumulator::new();
        let langs = ["Rust", "Python", "Go", "Java", "C", "Ruby", "Lua"];
        for (i, lang) in langs.iter().enumerate() {
            for j in 0..(i + 1) * 7 {
                acc.add(
                    &p(&format!("/p/f{i}_{j}.x")),
                    Some(lang),
                    FileStats { code_lines: 1, total_lines: 1, comment_lines: 0 },
                );
            }
        }
        let b = acc.finish();
        let sum: u16 = b.languages.iter().map(|l| u16::from(l.pct)).sum();
        assert_eq!(sum, 100);
    }

    #[test]
    fn single_language_is_100_percent() {
        let mut acc = StatsAccumulator::new();
        acc.add(&p("/p/a.rs"), Some("Rust"), FileStats { code_lines: 50, total_lines: 60, comment_lines: 10 });
        let b = acc.finish();
        assert_eq!(b.languages.len(), 1);
        assert_eq!(b.languages[0].pct, 100);
        assert_eq!(b.languages[0].loc, 50);
    }

    #[test]
    fn no_code_yields_empty_breakdown() {
        let acc = StatsAccumulator::new();
        let b = acc.finish();
        assert!(b.languages.is_empty());
        assert_eq!(b.total_code_lines, 0);
        assert!(b.primary().is_none());
    }

    #[test]
    fn languages_sorted_by_loc_desc() {
        let mut acc = StatsAccumulator::new();
        acc.add(&p("/p/a.py"), Some("Python"), FileStats { code_lines: 10, ..Default::default() });
        acc.add(&p("/p/b.rs"), Some("Rust"), FileStats { code_lines: 500, ..Default::default() });
        acc.add(&p("/p/c.go"), Some("Go"), FileStats { code_lines: 100, ..Default::default() });
        let b = acc.finish();
        assert_eq!(b.languages[0].name, "Rust");
        assert_eq!(b.languages[1].name, "Go");
        assert_eq!(b.languages[2].name, "Python");
    }

    /// 主语言不应是 Markdown/JSON 这类标记格式。
    #[test]
    fn primary_language_prefers_real_code() {
        let mut acc = StatsAccumulator::new();
        acc.add(&p("/p/README.md"), Some("Markdown"), FileStats { code_lines: 2000, ..Default::default() });
        acc.add(&p("/p/main.rs"), Some("Rust"), FileStats { code_lines: 100, ..Default::default() });
        assert_eq!(acc.primary_language(), "Rust", "文档再多也不该成为主语言");
    }

    #[test]
    fn primary_language_falls_back_to_markup_only_project() {
        let mut acc = StatsAccumulator::new();
        acc.add(&p("/p/README.md"), Some("Markdown"), FileStats { code_lines: 100, ..Default::default() });
        assert_eq!(acc.primary_language(), "Markdown");
    }

    #[test]
    fn primary_language_empty_when_no_files() {
        assert_eq!(StatsAccumulator::new().primary_language(), "");
    }

    // ── 生成物不计入统计 ────────────────────────────────────────────

    /// 一个只有 package-lock.json 的项目不应显示"5 万行 JSON"。
    #[test]
    fn generated_files_do_not_inflate_loc() {
        let mut acc = StatsAccumulator::new();
        acc.add(
            &p("/p/package-lock.json"),
            Some("JSON"),
            FileStats { code_lines: 50_000, total_lines: 50_000, comment_lines: 0 },
        );
        acc.add(&p("/p/src/a.ts"), Some("TypeScript"), FileStats { code_lines: 80, ..Default::default() });
        let b = acc.finish();
        assert_eq!(b.total_code_lines, 80, "锁文件的 5 万行不得计入");
        assert_eq!(b.skipped_generated, 1);
        assert_eq!(b.total_files, 2, "文件总数仍如实统计");
        assert_eq!(b.primary(), Some("TypeScript"));
    }

    #[test]
    fn unknown_extensions_counted_but_not_in_breakdown() {
        let mut acc = StatsAccumulator::new();
        acc.add(&p("/p/data.bin"), None, FileStats { code_lines: 999, ..Default::default() });
        acc.add(&p("/p/a.rs"), Some("Rust"), FileStats { code_lines: 10, ..Default::default() });
        let b = acc.finish();
        assert_eq!(b.unknown_files, 1);
        assert_eq!(b.languages.len(), 1);
        assert_eq!(b.languages[0].pct, 100, "未知文件不应稀释百分比");
    }

    #[test]
    fn add_unread_counts_file_only() {
        let mut acc = StatsAccumulator::new();
        acc.add_unread(&p("/p/huge.bin"));
        let b = acc.finish();
        assert_eq!(b.total_files, 1);
        assert_eq!(b.total_code_lines, 0);
    }

    #[test]
    fn files_count_per_language() {
        let mut acc = StatsAccumulator::new();
        acc.add(&p("/p/a.rs"), Some("Rust"), FileStats { code_lines: 10, ..Default::default() });
        acc.add(&p("/p/b.rs"), Some("Rust"), FileStats { code_lines: 20, ..Default::default() });
        let b = acc.finish();
        assert_eq!(b.languages[0].files, 2);
        assert_eq!(b.languages[0].loc, 30);
    }

    #[test]
    fn max_read_bytes_is_reasonable() {
        assert_eq!(MAX_READ_BYTES, 2 * 1024 * 1024);
    }

    #[test]
    fn all_extension_mappings_are_consistent() {
        for (ext, lang) in EXTENSION_LANGUAGES {
            assert!(!ext.is_empty());
            assert!(!lang.is_empty());
            assert!(
                !ext.contains('.'),
                "扩展名不应含点号: {ext}"
            );
            // 每个映射都应能通过 language_of 反查成功
            let path = p(&format!("file.{ext}"));
            assert_eq!(language_of(&path), Some(*lang), "{ext} → {lang} 反查失败");
        }
    }

    #[test]
    fn hash_comment_languages_use_hash() {
        // Python 的 # 是注释，Rust 的不是（已在上面测过），这里验证分类函数本身
        let py = count_file_lines("# c\nx=1\n", "Python");
        assert_eq!(py.comment_lines, 1);
        let rb = count_file_lines("# c\nx=1\n", "Ruby");
        assert_eq!(rb.comment_lines, 1);
        let sh = count_file_lines("# c\nx=1\n", "Shell");
        assert_eq!(sh.comment_lines, 1);
    }
}
