//! 项目标记识别：判断一个目录是不是"项目"，以及是什么类型的项目。
//!
//! 🔴 产品验收标准（《产品设计书》V0.1 功能 1）：**不误报非项目文件夹**。
//! 判定必须基于确定性证据（标记文件 / .git），不能靠目录名猜。
//!
//! 设计选择：一个目录**只要有任一标记文件即视为项目**。
//! 这比"必须有 .git"更宽松，因为大量有价值的历史项目没有 Git 历史
//! （压缩包解压、早期实验、AI 生成的 MVP），但它们同样包含可复用资产。

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 项目标记文件 → 项目类型。
///
/// 顺序即优先级：多个标记同时存在时，取**第一个匹配**的作为主类型。
/// 排序依据是"特异性"——越具体的构建文件越能代表项目性质
/// （`Cargo.toml` 比 `.git` 更能说明这是个 Rust 项目）。
pub const MARKERS: &[(&str, ProjectKind)] = &[
    ("Cargo.toml", ProjectKind::Rust),
    ("package.json", ProjectKind::Node),
    ("pyproject.toml", ProjectKind::Python),
    ("setup.py", ProjectKind::Python),
    ("go.mod", ProjectKind::Go),
    ("pom.xml", ProjectKind::JavaMaven),
    ("build.gradle", ProjectKind::JavaGradle),
    ("build.gradle.kts", ProjectKind::JavaGradle),
    ("CMakeLists.txt", ProjectKind::Cpp),
    ("composer.json", ProjectKind::Php),
    ("Gemfile", ProjectKind::Ruby),
    ("pubspec.yaml", ProjectKind::Dart),
    ("requirements.txt", ProjectKind::Python),
    ("mix.exs", ProjectKind::Elixir),
    ("Package.swift", ProjectKind::Swift),
];

/// 项目类型（由标记文件确定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    Rust,
    Node,
    Python,
    Go,
    JavaMaven,
    JavaGradle,
    Cpp,
    Php,
    Ruby,
    Dart,
    Elixir,
    Swift,
    /// 无标记文件但有 .git（如纯文档仓库、脚本集合）
    GitOnly,
}

impl ProjectKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Node => "node",
            Self::Python => "python",
            Self::Go => "go",
            Self::JavaMaven => "java_maven",
            Self::JavaGradle => "java_gradle",
            Self::Cpp => "cpp",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Dart => "dart",
            Self::Elixir => "elixir",
            Self::Swift => "swift",
            Self::GitOnly => "git_only",
        }
    }

    /// 该类型的主要编程语言（用于 `Project.language` 的初判）。
    ///
    /// 注意这是**声明式**推断：真实语言构成由实际文件统计覆盖（见 `language.rs`），
    /// 这里只作为"目录里没有任何可统计代码文件"时的兜底。
    pub fn primary_language(&self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Node => "TypeScript", // Node 项目 TS/JS 皆有，统计阶段会修正
            Self::Python => "Python",
            Self::Go => "Go",
            Self::JavaMaven | Self::JavaGradle => "Java",
            Self::Cpp => "C++",
            Self::Php => "PHP",
            Self::Ruby => "Ruby",
            Self::Dart => "Dart",
            Self::Elixir => "Elixir",
            Self::Swift => "Swift",
            Self::GitOnly => "",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "rust" => Self::Rust,
            "node" => Self::Node,
            "python" => Self::Python,
            "go" => Self::Go,
            "java_maven" => Self::JavaMaven,
            "java_gradle" => Self::JavaGradle,
            "cpp" => Self::Cpp,
            "php" => Self::Php,
            "ruby" => Self::Ruby,
            "dart" => Self::Dart,
            "elixir" => Self::Elixir,
            "swift" => Self::Swift,
            "git_only" => Self::GitOnly,
            _ => return None,
        })
    }
}

/// 一个目录的探测结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// 判定为项目的依据（标记文件名，或 ".git"）
    pub marker: String,
    pub kind: ProjectKind,
    /// 是否存在 .git 目录（可与 marker 并存）
    pub has_git: bool,
    /// 是否存在 README（健康度与 AI 画像的输入）
    pub has_readme: bool,
    /// 是否存在测试目录/文件（健康度输入）
    pub has_tests: bool,
    /// 是否存在 LICENSE
    pub has_license: bool,
    /// 是否存在 Dockerfile / docker-compose
    pub has_docker: bool,
}

impl Detection {
    /// 项目名：优先取目录名（与用户心智一致）。
    pub fn project_name(dir: &Path) -> String {
        dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| dir.display().to_string())
    }
}

/// 探测一个目录是否为项目。
///
/// **只看顶层**（不递归）：递归会让 `src/package.json` 之类的嵌套文件
/// 把父目录误判为项目。子目录是否为独立项目由遍历层负责（它会分别探测每一层）。
///
/// 参数 `entries` 是该目录的**直接子项名**，由调用方从遍历结果提供。
/// 刻意不接收路径：这样本函数是纯函数（不发起 IO），
/// 单元测试可以直接用字符串数组驱动，无需创建临时目录。
pub fn detect(entries: &[String]) -> Option<Detection> {
    let has = |name: &str| entries.iter().any(|e| e == name);
    let has_ci = |name: &str| entries.iter().any(|e| e.eq_ignore_ascii_case(name));

    let has_git = has(".git");
    let marker_hit = MARKERS.iter().find(|(file, _)| has(file));

    // 判定：有构建标记文件，或有 .git
    let (marker, kind) = match marker_hit {
        Some((file, kind)) => (file.to_string(), *kind),
        None if has_git => (".git".to_string(), ProjectKind::GitOnly),
        // 无任何证据 → 不是项目（不误报）
        None => return None,
    };

    // 测试目录/文件的常见形态（大小写与命名习惯因语言而异）
    let has_tests = entries.iter().any(|e| {
        let lower = e.to_ascii_lowercase();
        lower == "tests"
            || lower == "test"
            || lower == "__tests__"
            || lower == "spec"
            || lower.starts_with("test_")
            || lower.ends_with("_test.go")
            || lower.ends_with(".test.js")
            || lower.ends_with(".test.ts")
            || lower.ends_with(".spec.js")
            || lower.ends_with(".spec.ts")
            || lower == "conftest.py"
            || lower == "pytest.ini"
    });

    Some(Detection {
        marker,
        kind,
        has_git,
        has_readme: has_ci("readme.md")
            || has_ci("readme.rst")
            || has_ci("readme.txt")
            || has_ci("readme"),
        has_tests,
        has_license: has_ci("license") || has_ci("license.md") || has_ci("license.txt"),
        has_docker: has_ci("dockerfile") || has_ci("docker-compose.yml") || has_ci("docker-compose.yaml"),
    })
}

/// 该目录是否应被跳过（永不进入、也不作为项目）。
///
/// 内置排除项**不可通过配置关闭**——这是隐私底线：
/// 依赖目录（node_modules 等）体积巨大且不是用户资产，
/// 凭证文件（.env）绝不能被读取或入库（《技术设计书》§23 密钥过滤）。
pub fn is_always_excluded(name: &str) -> bool {
    ALWAYS_EXCLUDED.contains(&name)
}

/// 永远跳过的目录/文件名。
pub const ALWAYS_EXCLUDED: &[&str] = &[
    // 依赖与构建产物：体积巨大、非用户资产
    "node_modules",
    "bower_components",
    "vendor",
    "target",
    "build",
    "dist",
    "out",
    ".next",
    ".nuxt",
    ".output",
    ".turbo",
    ".cache",
    ".parcel-cache",
    ".gradle",
    ".mvn",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".venv",
    "venv",
    "env",
    ".eggs",
    "*.egg-info",
    "site-packages",
    // 版本控制与 IDE
    ".git",
    ".svn",
    ".hg",
    ".idea",
    ".vscode",
    // 系统文件
    ".DS_Store",
    "Thumbs.db",
    // 🔒 凭证：绝不读取、绝不入库（§23 密钥过滤）
    ".env",
    ".env.local",
    ".env.production",
    ".env.development",
    ".npmrc",
    ".pypirc",
    ".netrc",
    ".aws",
    ".ssh",
    "id_rsa",
    "credentials",
    "secrets.json",
    "secrets.yaml",
    "keystore.jks",
];

/// 路径中任一段命中排除项即应跳过（用于深层路径判断）。
pub fn path_contains_excluded(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        is_always_excluded(&s)
    })
}

/// 是否为凭证类文件名（即使不在排除列表里，也绝不读取内容）。
///
/// 双保险：`ALWAYS_EXCLUDED` 挡目录遍历，本函数挡"内容读取"环节。
pub fn is_secret_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ALWAYS_EXCLUDED.iter().any(|e| lower == e.to_ascii_lowercase())
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.ends_with(".keystore")
        || lower.starts_with(".env")
        || lower.contains("secret")
        || lower.contains("credential")
        || lower.contains("token")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn dir(s: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(s)
    }

    // ── 项目识别 ────────────────────────────────────────────────────

    #[test]
    fn detects_rust_project() {
        let d = detect(&names(&["Cargo.toml", "src", ".git"])).unwrap();
        assert_eq!(d.kind, ProjectKind::Rust);
        assert_eq!(d.marker, "Cargo.toml");
        assert!(d.has_git);
    }

    #[test]
    fn detects_node_project() {
        let d = detect(&names(&["package.json", "index.js"])).unwrap();
        assert_eq!(d.kind, ProjectKind::Node);
        assert!(!d.has_git);
    }

    #[test]
    fn detects_python_by_pyproject() {
        let d = detect(&names(&["pyproject.toml", "app"])).unwrap();
        assert_eq!(d.kind, ProjectKind::Python);
    }

    #[test]
    fn detects_python_by_requirements_txt() {
        let d = detect(&names(&["requirements.txt", "main.py"])).unwrap();
        assert_eq!(d.kind, ProjectKind::Python);
        assert_eq!(d.marker, "requirements.txt");
    }

    #[test]
    fn detects_go_java_cpp_and_others() {
        assert_eq!(detect(&names(&["go.mod"])).unwrap().kind, ProjectKind::Go);
        assert_eq!(detect(&names(&["pom.xml"])).unwrap().kind, ProjectKind::JavaMaven);
        assert_eq!(detect(&names(&["build.gradle.kts"])).unwrap().kind, ProjectKind::JavaGradle);
        assert_eq!(detect(&names(&["CMakeLists.txt"])).unwrap().kind, ProjectKind::Cpp);
        assert_eq!(detect(&names(&["composer.json"])).unwrap().kind, ProjectKind::Php);
        assert_eq!(detect(&names(&["Gemfile"])).unwrap().kind, ProjectKind::Ruby);
        assert_eq!(detect(&names(&["pubspec.yaml"])).unwrap().kind, ProjectKind::Dart);
        assert_eq!(detect(&names(&["mix.exs"])).unwrap().kind, ProjectKind::Elixir);
        assert_eq!(detect(&names(&["Package.swift"])).unwrap().kind, ProjectKind::Swift);
        assert_eq!(detect(&names(&["setup.py"])).unwrap().kind, ProjectKind::Python);
    }

    /// 验收标准要求"不误报非项目文件夹"。
    #[test]
    fn rejects_plain_directory() {
        assert!(detect(&names(&["a.jpg", "b.png", "docs"])).is_none());
    }

    #[test]
    fn rejects_empty_directory() {
        assert!(detect(&Vec::<String>::new()).is_none());
    }

    /// 只有 .git 也算项目（文档仓库/脚本集合同样有价值）。
    #[test]
    fn git_only_is_a_project() {
        let d = detect(&names(&[".git", "notes.md"])).unwrap();
        assert_eq!(d.kind, ProjectKind::GitOnly);
        assert_eq!(d.marker, ".git");
        assert!(d.has_git);
    }

    /// 标记文件优先级：Cargo.toml 比 .git 更能代表项目性质。
    #[test]
    fn marker_takes_priority_over_git() {
        let d = detect(&names(&[".git", "Cargo.toml", "package.json"])).unwrap();
        assert_eq!(d.kind, ProjectKind::Rust, "Cargo.toml 应优先于 package.json");
        assert_eq!(d.marker, "Cargo.toml");
        assert!(d.has_git);
    }

    /// 只看顶层：嵌套的标记文件不应让父目录被误判。
    #[test]
    fn nested_marker_does_not_trigger_parent() {
        // 目录列表里只有 "src"，src/package.json 不出现在顶层
        assert!(detect(&names(&["src", "README.md"])).is_none());
    }

    // ── 特征探测 ────────────────────────────────────────────────────

    #[test]
    fn detects_readme_case_insensitively() {
        for n in ["README.md", "readme.md", "ReadMe.MD", "README.rst", "README"] {
            let d = detect(&names(&["Cargo.toml", n])).unwrap();
            assert!(d.has_readme, "{n} 应识别为 README");
        }
        let d = detect(&names(&["Cargo.toml"])).unwrap();
        assert!(!d.has_readme);
    }

    #[test]
    fn detects_tests_in_various_conventions() {
        for n in [
            "tests", "test", "__tests__", "spec", "test_foo.py",
            "main_test.go", "app.test.ts", "app.test.js", "x.spec.ts", "conftest.py", "pytest.ini",
        ] {
            let d = detect(&names(&["Cargo.toml", n])).unwrap();
            assert!(d.has_tests, "{n} 应识别为测试");
        }
        let d = detect(&names(&["Cargo.toml", "src"])).unwrap();
        assert!(!d.has_tests);
    }

    #[test]
    fn detects_license_and_docker() {
        let d = detect(&names(&["Cargo.toml", "LICENSE", "Dockerfile"])).unwrap();
        assert!(d.has_license);
        assert!(d.has_docker);
        let d2 = detect(&names(&["Cargo.toml", "docker-compose.yml"])).unwrap();
        assert!(d2.has_docker);
        assert!(!d2.has_license);
    }

    // ── 排除规则 ────────────────────────────────────────────────────

    #[test]
    fn excludes_dependency_dirs() {
        for n in ["node_modules", "target", "dist", ".venv", "__pycache__", ".next", "vendor"] {
            assert!(is_always_excluded(n), "{n} 必须被排除");
        }
    }

    #[test]
    fn excludes_secret_files() {
        for n in [".env", ".env.local", ".npmrc", ".netrc", "credentials", "id_rsa", "secrets.json"] {
            assert!(is_always_excluded(n), "{n} 必须被排除");
        }
    }

    #[test]
    fn does_not_exclude_normal_names() {
        for n in ["src", "app", "main.py", "Cargo.toml", "test"] {
            assert!(!is_always_excluded(n), "{n} 不应被排除");
        }
    }

    /// 🔒 隐私红线：凭证文件绝不被读取内容。
    #[test]
    fn secret_detection_covers_extensions_and_keywords() {
        for n in ["server.pem", "private.key", "cert.p12", "app.keystore", ".env.production"] {
            assert!(is_secret_file(n), "{n} 应判定为凭证文件");
        }
        for n in ["my_secret.yaml", "aws_credentials.json", "access_token.txt"] {
            assert!(is_secret_file(n), "{n} 含敏感关键词，应判定为凭证");
        }
        for n in ["main.py", "index.ts", "Cargo.toml", "README.md"] {
            assert!(!is_secret_file(n), "{n} 是普通文件");
        }
    }

    #[test]
    fn path_contains_excluded_checks_all_segments() {
        assert!(path_contains_excluded(Path::new("/a/node_modules/pkg/index.js")));
        assert!(path_contains_excluded(Path::new("/a/.venv/lib/x.py")));
        assert!(path_contains_excluded(Path::new("/a/b/.env")));
        assert!(!path_contains_excluded(Path::new("/a/src/main.py")));
    }

    // ── 元数据 ──────────────────────────────────────────────────────

    #[test]
    fn project_name_is_dir_basename() {
        assert_eq!(Detection::project_name(&dir("/tmp/my-project")), "my-project");
        assert_eq!(Detection::project_name(&dir("relative")), "relative");
    }

    #[test]
    fn kind_roundtrips_and_has_language() {
        for k in [
            ProjectKind::Rust, ProjectKind::Node, ProjectKind::Python, ProjectKind::Go,
            ProjectKind::JavaMaven, ProjectKind::JavaGradle, ProjectKind::Cpp, ProjectKind::Php,
            ProjectKind::Ruby, ProjectKind::Dart, ProjectKind::Elixir, ProjectKind::Swift,
            ProjectKind::GitOnly,
        ] {
            assert_eq!(ProjectKind::parse(k.as_str()), Some(k));
            // GitOnly 无主语言（返回空串），其余必须有
            if k != ProjectKind::GitOnly {
                assert!(!k.primary_language().is_empty(), "{k:?} 应有主语言");
            }
        }
        assert_eq!(ProjectKind::GitOnly.primary_language(), "");
        assert_eq!(ProjectKind::parse("bogus"), None);
    }

    #[test]
    fn all_markers_map_to_known_kind() {
        for (file, kind) in MARKERS {
            assert!(!file.is_empty());
            assert_eq!(ProjectKind::parse(kind.as_str()), Some(*kind), "{file} 的 kind 无效");
        }
    }
}
