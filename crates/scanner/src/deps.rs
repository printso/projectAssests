//! 依赖与框架识别（Level 0，**零 LLM 成本**）。
//!
//! 产出两样东西：
//! - `framework`：主框架（"Next.js" / "FastAPI" / "Axum"），来自依赖清单的确定性匹配
//! - `tags`：技术栈标签（依赖名），来自真实清单而非人工编写
//!
//! 🔴 设计红线：**不猜测**。依赖清单里没有的就不写。
//! 原型期的 mock 给每个项目手写了 `["Python","FastAPI","Vue","ComfyUI","SQLite","MCP"]`，
//! 这类数据无法验证、无法更新，接真实数据后必须全部由解析结果产生。
//!
//! 🔒 安全：只读取依赖清单文件，且限制文件大小。
//! 凭证类文件（.env 等）由 `markers::is_secret_file` 在更上层就被拦截。

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 依赖清单解析结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dependencies {
    /// 运行时依赖名（不含版本号）
    pub runtime: Vec<String>,
    /// 开发依赖名
    pub dev: Vec<String>,
    /// 识别出的框架（按匹配强度排序，第一个为主框架）
    pub frameworks: Vec<String>,
    /// 来源清单文件（相对项目根的展示名，用于 Evidence）
    pub manifest: String,
    /// 清单声明的语言版本（如 Python ">=3.11"、Node ">=18"）
    pub declared_version: Option<String>,
}

impl Dependencies {
    /// 主框架；未识别时返回 `None`（上层显示 "-" 而非编造）。
    pub fn primary_framework(&self) -> Option<&str> {
        self.frameworks.first().map(String::as_str)
    }

    /// 展示用标签：框架 + 运行时依赖，去重后限量。
    ///
    /// 限量是必要的：一个项目的依赖可能有上百个，全塞进 UI 的 chips 会溢出。
    /// 框架优先展示（信息量最大），其余按字母序保证稳定。
    pub fn display_tags(&self, limit: usize) -> Vec<String> {
        let mut out: Vec<String> = self.frameworks.clone();
        let mut seen: BTreeSet<String> = out.iter().cloned().collect();
        let mut rest: Vec<&String> = self.runtime.iter().collect();
        rest.sort();
        for d in rest {
            if out.len() >= limit {
                break;
            }
            if seen.insert(d.clone()) {
                out.push(d.clone());
            }
        }
        out.truncate(limit);
        out
    }

    pub fn is_empty(&self) -> bool {
        self.runtime.is_empty() && self.dev.is_empty() && self.frameworks.is_empty()
    }

    /// 依赖总数（运行时 + 开发）。
    pub fn total(&self) -> usize {
        self.runtime.len() + self.dev.len()
    }
}

/// 框架识别规则：`(依赖名, 框架展示名, 权重)`。
///
/// 权重用于消歧：一个项目同时依赖 `react` 和 `next` 时，
/// Next.js 是更具体的判断（它包含 React），应作为主框架。
/// 权重越高越具体，排序越靠前。
pub const FRAMEWORK_RULES: &[(&str, &str, u8)] = &[
    // ── Rust ─────────────────────────────────────────────────────
    ("tauri", "Tauri", 95),
    ("axum", "Axum", 90),
    ("actix-web", "Actix-web", 90),
    ("rocket", "Rocket", 88),
    ("bevy", "Bevy", 88),
    ("tokio", "Tokio", 40),
    // ── Python ───────────────────────────────────────────────────
    ("fastapi", "FastAPI", 90),
    ("django", "Django", 92),
    ("flask", "Flask", 88),
    ("streamlit", "Streamlit", 85),
    ("gradio", "Gradio", 85),
    ("pytorch", "PyTorch", 80),
    ("torch", "PyTorch", 80),
    ("tensorflow", "TensorFlow", 80),
    ("scrapy", "Scrapy", 82),
    ("celery", "Celery", 75),
    ("langchain", "LangChain", 78),
    ("llama-index", "LlamaIndex", 78),
    // ── Node / 前端 ──────────────────────────────────────────────
    ("next", "Next.js", 95),
    ("nuxt", "Nuxt", 95),
    ("react", "React", 70),
    ("react-dom", "React", 68),
    ("vue", "Vue", 72),
    ("svelte", "Svelte", 75),
    ("sveltekit", "SvelteKit", 90),
    ("@sveltejs/kit", "SvelteKit", 92),
    ("angular", "Angular", 85),
    ("@angular/core", "Angular", 92),
    ("express", "Express", 80),
    ("koa", "Koa", 80),
    ("nestjs", "NestJS", 85),
    ("@nestjs/core", "NestJS", 92),
    ("vite", "Vite", 60),
    ("electron", "Electron", 88),
    ("antd", "Ant Design", 55),
    // ── Go ───────────────────────────────────────────────────────
    ("github.com/gin-gonic/gin", "Gin", 88),
    ("github.com/labstack/echo", "Echo", 85),
    ("github.com/gofiber/fiber", "Fiber", 85),
    // ── 数据库/存储（作为技术栈特征，非主框架）──────────────────
    ("rusqlite", "SQLite", 50),
    ("sqlalchemy", "SQLAlchemy", 60),
    ("prisma", "Prisma", 62),
    ("typeorm", "TypeORM", 60),
    ("redis", "Redis", 45),
    ("mongodb", "MongoDB", 50),
];

/// 前缀匹配规则：`(依赖名前缀, 框架展示名, 权重)`。
///
/// 为什么需要独立的前缀表（而不是塞进 `FRAMEWORK_RULES`）：
/// `FRAMEWORK_RULES` 是**精确/边界**匹配，加前缀语义会误伤
/// （例如前缀 `django` 会命中 `django-celery-beat`，但那不等于用了 Django 本体）。
///
/// 这里收录的是"一个框架的所有 artifact 共享同一前缀"的情况，
/// 典型是 Java/Maven：pom.xml 里出现的是 `spring-boot-starter-web`，
/// 而完整坐标 `org.springframework.boot:spring-boot-starter-web` 只在
/// 显式写了 groupId 时才出现。两种形态都要能识别，故用前缀匹配。
pub const FRAMEWORK_PREFIX_RULES: &[(&str, &str, u8)] = &[
    // Spring Boot 全家桶：starter-web / starter-data-jpa / actuator …
    ("spring-boot", "Spring Boot", 95),
    // Spring 但非 Boot（spring-core / spring-web）：权重更低，Boot 存在时不覆盖
    ("spring-", "Spring", 80),
    // Java EE / Jakarta
    ("jakarta.", "Jakarta EE", 70),
];

/// 依赖名归一化：去掉 scope 前缀之外的噪音，统一小写用于匹配。
///
/// Python 的包名规范里 `-` 与 `_` 等价（`llama-index` == `llama_index`），
/// 不归一会漏匹配一半的规则。
fn normalize(dep: &str) -> String {
    dep.trim().to_lowercase().replace('_', "-")
}

/// 从依赖名集合识别框架，按权重降序去重返回。
pub fn detect_frameworks(deps: &[String]) -> Vec<String> {
    let normalized: Vec<String> = deps.iter().map(|d| normalize(d)).collect();
    let mut hits: Vec<(u8, String)> = Vec::new();

    for (dep_pattern, framework, weight) in FRAMEWORK_RULES {
        let pattern = dep_pattern.to_lowercase();
        let matched = normalized.iter().any(|d| {
            // 完全相等，或 Python 风格的 "pkg[extra]" / Java 的 "group:artifact"
            *d == pattern
                || d.starts_with(&format!("{pattern}["))
                || d.starts_with(&format!("{pattern}:"))
                || d.ends_with(&format!(":{pattern}"))
                || d.ends_with(&format!("/{pattern}"))
        });
        if matched {
            hits.push((*weight, (*framework).to_string()));
        }
    }

    // 前缀规则：每个依赖只取**最长**匹配前缀，避免同一依赖贡献多个框架
    // （`spring-boot-starter-web` 同时匹配 "spring-boot" 与 "spring-"，
    //   若都算进去，Spring Boot 项目会莫名多出一个 "Spring" 框架）
    for dep in &normalized {
        if let Some((_, framework, weight)) = longest_prefix_match(dep) {
            hits.push((weight, framework.to_string()));
        }
    }

    // 权重降序；同权重按名称排序保证稳定输出
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut out: Vec<String> = Vec::new();
    for (_, f) in hits {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// 对单个（已归一化的）依赖名做最长前缀匹配。
///
/// 同时检查两种形态：
/// - 完整字符串（Maven 只写了 artifactId 时，如 `spring-boot-starter-web`）
/// - 最后一段（Maven 写了完整坐标时，如 `org.springframework.boot:spring-boot-…`）
fn longest_prefix_match(dep: &str) -> Option<(&'static str, &'static str, u8)> {
    // 取 ':' 之后的部分作为 artifactId（Maven 坐标 group:artifact）
    let artifact = dep.rsplit(':').next().unwrap_or(dep);
    let candidates = [dep, artifact];

    let mut best: Option<(&'static str, &'static str, u8)> = None;
    for cand in candidates {
        for (prefix, framework, weight) in FRAMEWORK_PREFIX_RULES {
            let p = prefix.to_lowercase();
            if cand.starts_with(&p) {
                // 最长前缀优先；长度相同时取权重更高者（保证结果确定）
                let better = match best {
                    None => true,
                    Some((bp, _, bw)) => p.len() > bp.len() || (p.len() == bp.len() && *weight > bw),
                };
                if better {
                    best = Some((prefix, framework, *weight));
                }
            }
        }
    }
    best
}

/// 读取清单文件的最大字节数。超过则跳过（防御异常大文件）。
pub const MAX_MANIFEST_BYTES: u64 = 512 * 1024;

/// 清单解析函数类型：`(清单文本, 累加结果)`。
///
/// 抽成别名是因为 `&[(&str, fn(&str, &mut Dependencies))]` 这种内联类型
/// 可读性差，且 clippy 的 `type_complexity` 会报警。
type ManifestParser = (&'static str, fn(&str, &mut Dependencies));

/// 解析项目根目录下的所有依赖清单。
///
/// 一个项目可能同时有多个清单（如 `package.json` + `requirements.txt` 的全栈项目），
/// 全部解析并合并，`manifest` 字段记录实际命中的文件名（Evidence 用）。
pub fn parse_manifests(dir: &Path) -> Dependencies {
    let mut out = Dependencies::default();
    let mut manifests: Vec<String> = Vec::new();

    // (文件名, 解析函数)
    let candidates: &[ManifestParser] = &[
        ("package.json", parse_package_json),
        ("Cargo.toml", parse_cargo_toml),
        ("pyproject.toml", parse_pyproject_toml),
        ("requirements.txt", parse_requirements_txt),
        ("go.mod", parse_go_mod),
        ("pom.xml", parse_pom_xml),
        ("Gemfile", parse_gemfile),
        ("composer.json", parse_composer_json),
    ];

    for (name, parser) in candidates {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() > MAX_MANIFEST_BYTES {
            tracing::warn!(file = %name, size = meta.len(), "依赖清单过大，已跳过解析");
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        parser(&content, &mut out);
        manifests.push((*name).to_string());
    }

    out.manifest = manifests.join(" + ");
    // 依赖去重排序（多清单合并时可能重复）
    dedup_sort(&mut out.runtime);
    dedup_sort(&mut out.dev);
    // 框架识别基于合并后的全部依赖
    let mut all = out.runtime.clone();
    all.extend(out.dev.iter().cloned());
    out.frameworks = detect_frameworks(&all);
    out
}

fn dedup_sort(v: &mut Vec<String>) {
    v.sort();
    v.dedup();
}

// ── package.json ────────────────────────────────────────────────────

fn parse_package_json(content: &str, out: &mut Dependencies) {
    #[derive(Deserialize)]
    struct Pkg {
        #[serde(default)]
        dependencies: Option<serde_json::Map<String, serde_json::Value>>,
        // 🔴 必须显式 rename：serde 默认按字段名 snake_case 匹配，
        //    而 package.json 用的是 camelCase `devDependencies`。
        //    漏了这个 rename 会让所有 Node 项目的 dev 依赖静默丢失。
        #[serde(default, rename = "devDependencies")]
        dev_dependencies: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(default)]
        engines: Option<Engines>,
    }
    #[derive(Deserialize)]
    struct Engines {
        #[serde(default)]
        node: Option<String>,
    }

    let Ok(pkg) = serde_json::from_str::<Pkg>(content) else {
        tracing::debug!("package.json 解析失败，已跳过");
        return;
    };
    if let Some(deps) = pkg.dependencies {
        out.runtime.extend(deps.into_iter().map(|(k, _)| k));
    }
    if let Some(deps) = pkg.dev_dependencies {
        out.dev.extend(deps.into_iter().map(|(k, _)| k));
    }
    if let Some(e) = pkg.engines
        && let Some(node) = e.node
    {
        out.declared_version = Some(format!("Node {node}"));
    }
}

// ── Cargo.toml ──────────────────────────────────────────────────────
//
// 刻意**不用** toml crate：只需要 `[dependencies]` 段的键名，
// 引入完整 TOML 解析器会增加依赖树，而手写行解析对这个窄需求足够可靠。
// 局限（已在测试中固化）：不处理 `dep = { version = "..", features = [..] }`
// 之外的复杂内联表嵌套，但键名提取仍然正确。

fn parse_cargo_toml(content: &str, out: &mut Dependencies) {
    let mut section = String::new();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            section = t.trim_matches(|c| c == '[' || c == ']').to_string();
            continue;
        }
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let is_deps = section == "dependencies";
        let is_dev = section == "dev-dependencies";
        let is_build = section == "build-dependencies";
        if !(is_deps || is_dev || is_build) {
            continue;
        }
        // `name = "1.0"` 或 `name = { version = "1.0" }` 或 `name.workspace = true`
        let Some((key, _value)) = t.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').trim_matches('\'');
        // `foo.workspace = true` 的键会带点号，取第一段
        let name = key.split('.').next().unwrap_or(key).trim();
        if name.is_empty() {
            continue;
        }
        if is_deps {
            out.runtime.push(name.to_string());
        } else {
            out.dev.push(name.to_string());
        }
    }
}

// ── pyproject.toml ──────────────────────────────────────────────────

fn parse_pyproject_toml(content: &str, out: &mut Dependencies) {
    let mut in_deps = false;
    let mut in_optional = false;
    let mut requires_python: Option<String> = None;

    for line in content.lines() {
        let t = line.trim();

        if t.starts_with("requires-python") {
            if let Some((_, v)) = t.split_once('=') {
                requires_python = Some(v.trim().trim_matches('"').to_string());
            }
            continue;
        }

        if t.starts_with('[') {
            let s = t.trim_matches(|c| c == '[' || c == ']').to_string();
            in_deps = s == "project" || s == "tool.poetry.dependencies";
            in_optional = s.starts_with("project.optional-dependencies")
                || s.starts_with("tool.poetry.group");
            // `dependencies = [...]` 可能紧跟在 [project] 后，也可能在数组里
            continue;
        }

        // `dependencies = ["fastapi>=0.1", "uvicorn"]`
        if t.starts_with("dependencies") {
            in_deps = true;
            extract_quoted(t, &mut out.runtime);
            // 单行写完后，若已闭合则不再吞后续行
            if t.contains(']') {
                in_deps = false;
            }
            continue;
        }

        if in_deps || in_optional {
            // 数组续行：`"fastapi>=0.1",` 或 poetry 的 `fastapi = "^0.100"`
            if t.starts_with('"') || t.starts_with('\'') {
                extract_quoted(t, if in_optional { &mut out.dev } else { &mut out.runtime });
            } else if let Some((key, _)) = t.split_once('=') {
                let key = key.trim().trim_matches('"');
                if !key.is_empty() && !key.starts_with('[') {
                    if in_optional {
                        out.dev.push(key.to_string());
                    } else {
                        out.runtime.push(key.to_string());
                    }
                }
            }
            if t.contains(']') {
                in_deps = false;
                in_optional = false;
            }
        }
    }

    if let Some(v) = requires_python {
        out.declared_version = Some(format!("Python {v}"));
    }
}

/// 从一行中提取所有引号内的字符串，并剥离版本约束（`fastapi>=0.1` → `fastapi`）。
fn extract_quoted(line: &str, target: &mut Vec<String>) {
    let mut rest = line;
    while let Some(start) = rest.find(['"', '\'']) {
        let quote = rest.as_bytes()[start] as char;
        let after = &rest[start + 1..];
        let Some(end) = after.find(quote) else { break };
        let raw = &after[..end];
        rest = &after[end + 1..];
        if let Some(name) = strip_version_specifier(raw) {
            target.push(name);
        }
    }
}

/// 剥离 PEP 508 版本约束与 extras：`fastapi[all]>=0.1,<1` → `fastapi`。
fn strip_version_specifier(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    // 找到第一个版本操作符或分隔符的位置
    let cut = t
        .find(['>', '<', '=', '!', '~', '[', ';', '(', ' '])
        .unwrap_or(t.len());
    let name = t[..cut].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

// ── requirements.txt ────────────────────────────────────────────────

fn parse_requirements_txt(content: &str, out: &mut Dependencies) {
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with('-') {
            // `-r other.txt` / `--index-url` 等指令行跳过
            continue;
        }
        // 支持 `pkg @ https://...` 与 `pkg==1.0`
        let name = t.split('@').next().unwrap_or(t);
        if let Some(n) = strip_version_specifier(name) {
            out.runtime.push(n);
        }
    }
}

// ── go.mod ──────────────────────────────────────────────────────────

fn parse_go_mod(content: &str, out: &mut Dependencies) {
    let mut in_require = false;
    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("go ") {
            out.declared_version = Some(format!("Go {}", rest.trim()));
            continue;
        }
        if t.starts_with("require (") || t == "require (" {
            in_require = true;
            continue;
        }
        if in_require && t == ")" {
            in_require = false;
            continue;
        }
        if let Some(rest) = t.strip_prefix("require ") {
            // 单行 require：`require golang.org/x/sync v0.5.0`
            if let Some(name) = rest.split_whitespace().next()
                && !name.starts_with('(')
            {
                out.runtime.push(name.to_string());
            }
            continue;
        }
        if in_require
            && let Some(name) = t.split_whitespace().next()
            && !name.starts_with("//")
        {
            out.runtime.push(name.to_string());
        }
    }
}

// ── pom.xml ─────────────────────────────────────────────────────────
//
// 不引入 XML 解析器：只需要 `<artifactId>` 文本，正则式扫描足够。

fn parse_pom_xml(content: &str, out: &mut Dependencies) {
    for artifact in extract_xml_text(content, "artifactId") {
        out.runtime.push(artifact);
    }
    if let Some(v) = extract_xml_text(content, "java.version").first() {
        out.declared_version = Some(format!("Java {v}"));
    }
}

/// 提取 `<tag>value</tag>` 中的 value（简易实现，够用于清单文件）。
fn extract_xml_text(content: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = content;
    while let Some(s) = rest.find(&open) {
        let after = &rest[s + open.len()..];
        let Some(e) = after.find(&close) else { break };
        let v = after[..e].trim();
        if !v.is_empty() {
            out.push(v.to_string());
        }
        rest = &after[e + close.len()..];
    }
    out
}

// ── Gemfile ─────────────────────────────────────────────────────────

fn parse_gemfile(content: &str, out: &mut Dependencies) {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        // `gem "rails"` / `gem 'rails', '~> 7.0'` / `gem "rspec", group: :development`
        let Some(rest) = t.strip_prefix("gem ") else {
            continue;
        };
        let name = rest.trim_start_matches(['"', '\'']);
        let name = name
            .split(['"', '\''])
            .next()
            .unwrap_or(name)
            .trim();
        if !name.is_empty() {
            if t.contains("group: :development") || t.contains("group: :test") {
                out.dev.push(name.to_string());
            } else {
                out.runtime.push(name.to_string());
            }
        }
    }
}

// ── composer.json ───────────────────────────────────────────────────

fn parse_composer_json(content: &str, out: &mut Dependencies) {
    #[derive(Deserialize)]
    struct Composer {
        #[serde(default, rename = "require")]
        require: Option<serde_json::Map<String, serde_json::Value>>,
        #[serde(default, rename = "require-dev")]
        require_dev: Option<serde_json::Map<String, serde_json::Value>>,
    }
    let Ok(c) = serde_json::from_str::<Composer>(content) else {
        return;
    };
    if let Some(r) = c.require {
        out.runtime.extend(
            r.into_iter()
                .map(|(k, _)| k)
                // `php` 与 `ext-*` 是平台要求，不是项目依赖
                .filter(|k| k != "php" && !k.starts_with("ext-")),
        );
    }
    if let Some(r) = c.require_dev {
        out.dev.extend(r.into_iter().map(|(k, _)| k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── package.json ────────────────────────────────────────────────

    #[test]
    fn parses_package_json() {
        let json = r#"{
            "name": "demo",
            "dependencies": { "next": "14.0.0", "react": "^18.2.0", "axios": "^1.0" },
            "devDependencies": { "typescript": "^5", "vite": "^5" },
            "engines": { "node": ">=18" }
        }"#;
        let mut out = Dependencies::default();
        parse_package_json(json, &mut out);
        assert!(out.runtime.contains(&"next".to_string()));
        assert!(out.runtime.contains(&"react".to_string()));
        assert!(out.dev.contains(&"typescript".to_string()));
        assert_eq!(out.declared_version.as_deref(), Some("Node >=18"));
    }

    #[test]
    fn package_json_detects_frameworks() {
        let json = r#"{ "dependencies": { "next": "14", "react": "18", "antd": "5" } }"#;
        let mut out = Dependencies::default();
        parse_package_json(json, &mut out);
        out.frameworks = detect_frameworks(&out.runtime);
        assert_eq!(out.primary_framework(), Some("Next.js"), "Next 应优先于 React");
        assert!(out.frameworks.contains(&"React".to_string()));
        assert!(out.frameworks.contains(&"Ant Design".to_string()));
    }

    #[test]
    fn malformed_package_json_is_skipped() {
        let mut out = Dependencies::default();
        parse_package_json("{ not json", &mut out);
        assert!(out.is_empty());
    }

    // ── Cargo.toml ──────────────────────────────────────────────────

    #[test]
    fn parses_cargo_toml() {
        let toml = r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
axum = "0.8"
tokio = { version = "1", features = ["full"] }
serde.workspace = true

[dev-dependencies]
tempfile = "3"
"#;
        let mut out = Dependencies::default();
        parse_cargo_toml(toml, &mut out);
        assert!(out.runtime.contains(&"axum".to_string()));
        assert!(out.runtime.contains(&"tokio".to_string()));
        assert!(out.runtime.contains(&"serde".to_string()), "workspace 继承也应识别");
        assert!(out.dev.contains(&"tempfile".to_string()));
        // package 段的 name/version 不应被当成依赖
        assert!(!out.runtime.contains(&"demo".to_string()));
        assert!(!out.runtime.contains(&"name".to_string()));
    }

    #[test]
    fn cargo_toml_detects_frameworks() {
        let toml = "[dependencies]\naxum = \"0.8\"\ntokio = \"1\"\nrusqlite = \"0.38\"\n";
        let mut out = Dependencies::default();
        parse_cargo_toml(toml, &mut out);
        out.frameworks = detect_frameworks(&out.runtime);
        assert_eq!(out.primary_framework(), Some("Axum"));
        assert!(out.frameworks.contains(&"SQLite".to_string()));
    }

    // ── pyproject.toml ──────────────────────────────────────────────

    #[test]
    fn parses_pyproject_single_line_array() {
        let toml = r#"
[project]
name = "demo"
requires-python = ">=3.11"
dependencies = ["fastapi>=0.100", "uvicorn", "sqlalchemy[asyncio]>=2"]
"#;
        let mut out = Dependencies::default();
        parse_pyproject_toml(toml, &mut out);
        assert!(out.runtime.contains(&"fastapi".to_string()));
        assert!(out.runtime.contains(&"uvicorn".to_string()));
        assert!(out.runtime.contains(&"sqlalchemy".to_string()), "应剥离 extras");
        assert_eq!(out.declared_version.as_deref(), Some("Python >=3.11"));
    }

    #[test]
    fn parses_pyproject_multiline_array() {
        let toml = r#"
[project]
dependencies = [
    "fastapi>=0.100",
    "django",
    # 一行注释
    "celery",
]
"#;
        let mut out = Dependencies::default();
        parse_pyproject_toml(toml, &mut out);
        assert!(out.runtime.contains(&"fastapi".to_string()));
        assert!(out.runtime.contains(&"django".to_string()));
        assert!(out.runtime.contains(&"celery".to_string()));
        assert_eq!(out.runtime.len(), 3);
    }

    #[test]
    fn parses_poetry_style() {
        let toml = r#"
[tool.poetry.dependencies]
python = "^3.11"
fastapi = "^0.100"
gradio = { version = "^4", optional = true }

[tool.poetry.group.dev.dependencies]
pytest = "^7"
"#;
        let mut out = Dependencies::default();
        parse_pyproject_toml(toml, &mut out);
        assert!(out.runtime.contains(&"fastapi".to_string()));
        assert!(out.runtime.contains(&"gradio".to_string()));
        assert!(out.dev.contains(&"pytest".to_string()));
    }

    #[test]
    fn pyproject_detects_frameworks() {
        let toml = "[project]\ndependencies = [\"fastapi\", \"sqlalchemy\", \"langchain\"]\n";
        let mut out = Dependencies::default();
        parse_pyproject_toml(toml, &mut out);
        out.frameworks = detect_frameworks(&out.runtime);
        assert_eq!(out.primary_framework(), Some("FastAPI"));
        assert!(out.frameworks.contains(&"LangChain".to_string()));
    }

    // ── requirements.txt ────────────────────────────────────────────

    #[test]
    fn parses_requirements_txt() {
        let txt = "# 注释行\nfastapi==0.100.0\nuvicorn>=0.20\n-r other.txt\n--index-url https://x\nnumpy @ https://example.com/numpy.whl\n\ntorch\n";
        let mut out = Dependencies::default();
        parse_requirements_txt(txt, &mut out);
        assert!(out.runtime.contains(&"fastapi".to_string()));
        assert!(out.runtime.contains(&"uvicorn".to_string()));
        assert!(out.runtime.contains(&"numpy".to_string()), "URL 依赖应取包名");
        assert!(out.runtime.contains(&"torch".to_string()));
        assert!(!out.runtime.iter().any(|d| d.starts_with('-')), "指令行应跳过");
    }

    #[test]
    fn requirements_txt_detects_pytorch() {
        let mut out = Dependencies::default();
        parse_requirements_txt("torch==2.1\ntorchvision\n", &mut out);
        out.frameworks = detect_frameworks(&out.runtime);
        assert!(out.frameworks.contains(&"PyTorch".to_string()));
    }

    // ── go.mod ──────────────────────────────────────────────────────

    #[test]
    fn parses_go_mod() {
        let m = r#"module example.com/app

go 1.21

require (
	github.com/gin-gonic/gin v1.9.1
	github.com/stretchr/testify v1.8.4 // indirect
)

require golang.org/x/sync v0.5.0
"#;
        let mut out = Dependencies::default();
        parse_go_mod(m, &mut out);
        assert!(out.runtime.contains(&"github.com/gin-gonic/gin".to_string()));
        assert!(out.runtime.contains(&"github.com/stretchr/testify".to_string()));
        assert!(out.runtime.contains(&"golang.org/x/sync".to_string()), "单行 require 也应解析");
        assert_eq!(out.declared_version.as_deref(), Some("Go 1.21"));
    }

    #[test]
    fn go_mod_detects_framework() {
        let mut out = Dependencies::default();
        parse_go_mod("require (\n\tgithub.com/gin-gonic/gin v1.9.1\n)\n", &mut out);
        out.frameworks = detect_frameworks(&out.runtime);
        assert_eq!(out.primary_framework(), Some("Gin"));
    }

    // ── pom.xml ─────────────────────────────────────────────────────

    #[test]
    fn parses_pom_xml() {
        let xml = r#"<project>
  <properties><java.version>17</java.version></properties>
  <dependencies>
    <dependency>
      <groupId>org.springframework.boot</groupId>
      <artifactId>spring-boot-starter-web</artifactId>
    </dependency>
  </dependencies>
</project>"#;
        let mut out = Dependencies::default();
        parse_pom_xml(xml, &mut out);
        assert!(out.runtime.contains(&"spring-boot-starter-web".to_string()));
        assert_eq!(out.declared_version.as_deref(), Some("Java 17"));
        out.frameworks = detect_frameworks(&out.runtime);
        assert_eq!(out.primary_framework(), Some("Spring Boot"));
    }

    // ── Gemfile / composer.json ─────────────────────────────────────

    #[test]
    fn parses_gemfile() {
        let g = "source 'https://rubygems.org'\ngem 'rails'\ngem \"rspec\", group: :test\n# gem 'x'\n";
        let mut out = Dependencies::default();
        parse_gemfile(g, &mut out);
        assert!(out.runtime.contains(&"rails".to_string()));
        assert!(out.dev.contains(&"rspec".to_string()), "test 组应归入 dev");
    }

    #[test]
    fn parses_composer_json_excluding_platform_reqs() {
        let j = r#"{ "require": { "php": ">=8.1", "ext-json": "*", "laravel/framework": "^10" }, "require-dev": { "phpunit/phpunit": "^10" } }"#;
        let mut out = Dependencies::default();
        parse_composer_json(j, &mut out);
        assert!(out.runtime.contains(&"laravel/framework".to_string()));
        assert!(!out.runtime.contains(&"php".to_string()), "平台要求不是项目依赖");
        assert!(!out.runtime.contains(&"ext-json".to_string()));
        assert!(out.dev.contains(&"phpunit/phpunit".to_string()));
    }

    // ── 版本约束剥离 ────────────────────────────────────────────────

    #[test]
    fn strips_all_version_specifier_forms() {
        let cases = [
            ("fastapi>=0.100", "fastapi"),
            ("fastapi==0.100.0", "fastapi"),
            ("fastapi~=0.100", "fastapi"),
            ("fastapi!=0.99", "fastapi"),
            ("sqlalchemy[asyncio]>=2", "sqlalchemy"),
            ("uvicorn", "uvicorn"),
            ("pkg ; python_version<'3.11'", "pkg"),
            ("  django  ", "django"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                strip_version_specifier(input).as_deref(),
                Some(expected),
                "输入 {input:?}"
            );
        }
        assert_eq!(strip_version_specifier(""), None);
        assert_eq!(strip_version_specifier(">=1.0"), None);
    }

    // ── 框架规则 ────────────────────────────────────────────────────

    #[test]
    fn framework_matching_is_case_and_separator_insensitive() {
        assert_eq!(detect_frameworks(&["Llama_Index".to_string()]), vec!["LlamaIndex"]);
        assert_eq!(detect_frameworks(&["llama-index".to_string()]), vec!["LlamaIndex"]);
    }

    #[test]
    fn framework_matching_handles_scoped_and_extras() {
        assert_eq!(detect_frameworks(&["@nestjs/core".to_string()]), vec!["NestJS"]);
        assert_eq!(detect_frameworks(&["@sveltejs/kit".to_string()]), vec!["SvelteKit"]);
        assert_eq!(detect_frameworks(&["fastapi[all]".to_string()]), vec!["FastAPI"]);
    }

    #[test]
    fn more_specific_framework_wins() {
        // Nuxt 与 Vue 同时存在 → Nuxt（更具体）
        let f = detect_frameworks(&["vue".into(), "nuxt".into()]);
        assert_eq!(f[0], "Nuxt");
        assert!(f.contains(&"Vue".to_string()));
        // Electron + React → Electron
        let f2 = detect_frameworks(&["react".into(), "electron".into()]);
        assert_eq!(f2[0], "Electron");
        // Tauri + Axum → Tauri（桌面容器比 Web 框架更能定义项目）
        let f3 = detect_frameworks(&["axum".into(), "tauri".into()]);
        assert_eq!(f3[0], "Tauri");
    }

    #[test]
    fn no_framework_returns_empty() {
        assert!(detect_frameworks(&["serde".to_string(), "anyhow".to_string()]).is_empty());
        assert!(detect_frameworks(&[]).is_empty());
    }

    #[test]
    fn frameworks_are_deduplicated() {
        // react 与 react-dom 都映射到 React，只应出现一次
        let f = detect_frameworks(&["react".into(), "react-dom".into()]);
        assert_eq!(f.iter().filter(|x| *x == "React").count(), 1);
    }

    // ── 展示标签 ────────────────────────────────────────────────────

    #[test]
    fn display_tags_prefers_frameworks() {
        let d = Dependencies {
            runtime: vec!["axios".into(), "zod".into(), "react".into()],
            frameworks: vec!["Next.js".into()],
            ..Default::default()
        };
        let tags = d.display_tags(3);
        assert_eq!(tags[0], "Next.js");
        assert_eq!(tags.len(), 3);
    }

    #[test]
    fn display_tags_respects_limit_and_dedups() {
        let d = Dependencies {
            runtime: vec!["react".into(), "a".into(), "b".into(), "c".into()],
            frameworks: vec!["React".into()],
            ..Default::default()
        };
        let tags = d.display_tags(3);
        assert_eq!(tags.len(), 3);
        assert_eq!(tags.iter().filter(|t| *t == "React").count(), 1, "不应重复");
    }

    #[test]
    fn display_tags_is_stable_order() {
        let d = Dependencies {
            runtime: vec!["zod".into(), "axios".into()],
            ..Default::default()
        };
        assert_eq!(d.display_tags(10), vec!["axios".to_string(), "zod".to_string()]);
    }

    #[test]
    fn emptiness_and_total() {
        assert!(Dependencies::default().is_empty());
        let d = Dependencies { runtime: vec!["a".into()], dev: vec!["b".into()], ..Default::default() };
        assert!(!d.is_empty());
        assert_eq!(d.total(), 2);
    }

    // ── 真实文件解析（集成）────────────────────────────────────────

    #[test]
    fn parse_manifests_reads_real_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "dependencies": { "next": "14", "react": "18" } }"#,
        ).unwrap();
        let d = parse_manifests(dir.path());
        assert_eq!(d.manifest, "package.json");
        assert_eq!(d.primary_framework(), Some("Next.js"));
        assert!(!d.is_empty());
    }

    #[test]
    fn parse_manifests_merges_multiple() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{ "dependencies": { "react": "18" } }"#).unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "fastapi\n").unwrap();
        let d = parse_manifests(dir.path());
        assert!(d.manifest.contains("package.json"));
        assert!(d.manifest.contains("requirements.txt"));
        assert!(d.frameworks.contains(&"React".to_string()));
        assert!(d.frameworks.contains(&"FastAPI".to_string()));
    }

    #[test]
    fn parse_manifests_no_manifest_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.py"), "print(1)").unwrap();
        let d = parse_manifests(dir.path());
        assert!(d.is_empty());
        assert_eq!(d.manifest, "");
        assert!(d.primary_framework().is_none());
    }

    /// 超大清单应被跳过而不是拖慢/爆内存。
    #[test]
    fn parse_manifests_skips_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let huge = "x".repeat((MAX_MANIFEST_BYTES + 1024) as usize);
        std::fs::write(dir.path().join("requirements.txt"), &huge).unwrap();
        let d = parse_manifests(dir.path());
        assert!(d.is_empty(), "超限清单应跳过");
    }

    #[test]
    fn framework_rules_have_valid_weights() {
        for (_, name, weight) in FRAMEWORK_RULES {
            assert!(!name.is_empty());
            assert!(*weight > 0 && *weight <= 100, "{name} 权重越界: {weight}");
        }
    }
}
