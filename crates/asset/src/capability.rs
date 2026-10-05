//! 能力抽取：从项目信号（依赖 / 框架 / 语言 / 符号名）推断三层能力。
//!
//! # 三层结构（《产品设计书》§2，🔴 强制）
//! ```text
//! Domain（AI / Web / Data / Infrastructure / Media）
//!   └── Capability（Image Generation / Agent / RAG / Task Queue）
//!       └── Implementation（Qwen / ComfyUI / MCP）
//! ```
//! 不做三层会退化成"几千个扁平标签"，图谱直接不可用。
//!
//! # 确定性优先
//! 能力来自**关键词匹配 + 归一化词表**，不调 LLM。这样：
//! - 收敛：同一批项目多次抽取得到同一套能力，不会每轮膨胀
//! - 可解释：每个能力都记录命中它的信号（Evidence）
//! - 可测试：词表变更能被单测捕获
//!
//! # 与 confidence 阈值的关系
//! 《技术设计书》§25：低置信度不进图谱。这里对"仅单一弱信号命中"的能力
//! 给低 confidence，上层（insight/graph）据此过滤，避免噪音能力污染图谱。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use spolia_domain::{Capability, CapabilityLayer};

/// 一条能力规则的判定结果。
#[derive(Debug, Clone)]
struct CapabilityHit {
    /// 所属 Domain 的稳定键（`DOMAINS` 的第一列，如 "ai"）
    domain: &'static str,
    /// 命中的具体实现（Implementation 层），可能为空
    implementation: Option<String>,
    /// 命中的信号（依赖名 / 框架名 / 符号名），作为 Evidence
    signals: Vec<String>,
    /// 命中强度：0.0-1.0
    strength: f64,
}

/// 能力规则的类型别名。
///
/// 抽出来是因为 `&[(&[&str], &str, &str, Option<&str>, f64)]` 这种五元组
/// 内联类型可读性差，clippy 的 `type_complexity` 也会报警。
/// 各字段含义见 `CAPABILITY_RULES` 的文档注释。
type CapabilityRule = (&'static [&'static str], &'static str, &'static str, Option<&'static str>, f64);

/// 能力规则：`(匹配子串集合, 能力名, 所属 Domain 稳定键, 可选实现名, 基础强度)`。
///
/// 匹配对象是归一化后的"信号串"（依赖名 + 框架名 + 符号名 + 顶层目录名，全部小写）。
/// 用子串而非全等：`langchain-community` 也应命中 `langchain`。
/// 匹配词以 `*` 开头表示**词干前缀匹配**（见 `contains_token`），
/// 用于覆盖复数与词形变化，如 `*migration` 命中目录名 `migrations`。
///
/// 🔴 维护纪律：新增能力必须归入 `DOMAINS` 已登记的 5 个稳定键之一。
/// 若确实需要新 Domain，先在 `DOMAINS` 里登记，
/// 否则能力树会失去顶层结构（`CapabilityTree::assemble` 会把它当孤儿丢弃）。
const CAPABILITY_RULES: &[CapabilityRule] = &[
    // ── AI / 生成 ────────────────────────────────────────────────
    (&["diffusers", "stable-diffusion", "comfyui", "sdxl", "text2image", "txt2img"], "Image Generation", "ai", Some("Diffusion"), 0.9),
    (&["openai", "gpt", "dall-e", "dalle"], "Image Generation", "ai", Some("OpenAI"), 0.8),
    (&["qwen-image", "wanx", "tongyi-wanxiang"], "Image Generation", "ai", Some("Qwen"), 0.85),
    (&["torchvision.models.detection", "yolo", "ultralytics", "detectron", "mmdetection"], "Object Detection", "ai", Some("YOLO"), 0.85),
    (&["segment-anything", "sam2", "mask2former", "*segmentation"], "Image Segmentation", "ai", None, 0.8),
    (&["video-generation", "text2video", "img2video", "animatediff", "wan2", "cogvideo", "mochi"], "Video Generation", "ai", None, 0.85),
    (&["moviepy", "ffmpeg", "opencv", "imageio"], "Video Processing", "media", None, 0.7),
    (&["whisper", "faster-whisper", "funasr", "speech-recognition"], "Speech Recognition", "ai", Some("Whisper"), 0.85),
    (&["tts", "edge-tts", "cosyvoice", "vits", "bark", "coqui"], "Text To Speech", "ai", None, 0.85),
    (&["text-generation", "llama", "chatglm", "baichuan", "qwen", "mistral", "gemma"], "Text Generation", "ai", None, 0.8),
    (&["langchain", "llamaindex", "llama-index", "haystack"], "RAG", "ai", Some("LangChain"), 0.85),
    (&["embedding", "bge", "sentence-transformers", "fastembed", "text-embedding"], "Embedding", "ai", None, 0.8),
    (&["chromadb", "chroma", "qdrant", "milvus", "pinecone", "weaviate", "faiss", "lancedb"], "Vector Search", "ai", None, 0.8),
    (&["*rerank", "bge-reranker", "cohere-rerank", "cross-encoder"], "Reranking", "ai", None, 0.75),
    (&["vision-language", "vlm", "qwen-vl", "llava", "clip", "gpt-4-vision", "minicpm"], "Vision Language Model", "ai", None, 0.8),
    (&["ocr", "tesseract", "paddleocr", "easyocr", "rapidocr"], "OCR", "ai", None, 0.8),
    (&["lora", "controlnet", "ip-adapter", "peft", "*finetune", "*fine-tune", "sft"], "Model Fine-tuning", "ai", None, 0.8),
    (&["onnx", "onnxruntime", "tensorrt", "openvino", "*quantiz", "int8", "4bit", "gguf"], "Model Optimization", "ai", None, 0.75),

    // ── AI / Agent ───────────────────────────────────────────────
    (&["langgraph", "autogen", "crewai", "metagpt", "swarm"], "Agent Orchestration", "ai", None, 0.85),
    (&["mcp", "model-context-protocol"], "MCP Integration", "ai", Some("MCP"), 0.9),
    (&["tool-call", "tool_call", "function-call", "function_call", "toolcall"], "Tool Calling", "ai", None, 0.8),
    (&["prompt-template", "prompt_template", "jinja", "*prompt-engineer", "prompthub"], "Prompt Engineering", "ai", None, 0.75),

    // ── Web / 前端 ───────────────────────────────────────────────
    (&["react", "react-dom", "next", "remix"], "Web Frontend", "web", Some("React"), 0.8),
    (&["vue", "nuxt", "vite", "element-plus"], "Web Frontend", "web", Some("Vue"), 0.8),
    (&["svelte", "sveltekit"], "Web Frontend", "web", Some("Svelte"), 0.8),
    (&["angular"], "Web Frontend", "web", Some("Angular"), 0.8),
    (&["tailwind", "tailwindcss", "antd", "ant-design", "mui", "chakra", "shadcn"], "UI Component Library", "web", None, 0.7),
    (&["three", "threejs", "babylonjs", "webgl", "cesium"], "3D Rendering", "web", Some("Three.js"), 0.8),
    (&["echarts", "chart.js", "d3", "highcharts", "plotly"], "Data Visualization", "web", None, 0.75),
    (&["tauri", "electron", "wails"], "Desktop App", "web", None, 0.85),

    // ── Web / 后端 ───────────────────────────────────────────────
    (&["fastapi", "flask", "django", "starlette"], "Web API", "web", None, 0.8),
    (&["express", "koa", "nestjs", "fastify", "axum", "actix", "gin", "spring-boot", "spring-web"], "Web API", "web", None, 0.8),
    (&["websocket", "socket.io", "ws", "sse", "server-sent"], "Realtime Communication", "web", None, 0.75),
    (&["graphql", "apollo", "relay"], "GraphQL", "web", None, 0.8),
    (&["grpc", "protobuf", "tonic", "thrift"], "RPC", "web", None, 0.75),
    (&["oauth", "jwt", "jsonwebtoken", "passlib", "bcrypt", "authlib", "casbin"], "Authentication", "web", None, 0.8),

    // ── Data ─────────────────────────────────────────────────────
    (&["pandas", "polars", "numpy", "dataframe"], "Data Processing", "data", None, 0.75),
    (&["sqlalchemy", "prisma", "typeorm", "sequelize", "diesel", "mybatis", "hibernate"], "ORM", "data", None, 0.75),
    (&["postgres", "postgresql", "mysql", "mariadb", "sqlite", "rusqlite", "clickhouse", "duckdb"], "Relational Database", "data", None, 0.7),
    (&["redis", "memcached", "keydb"], "Caching", "data", None, 0.75),
    (&["mongodb", "pymongo", "mongoose"], "Document Database", "data", None, 0.75),
    (&["elasticsearch", "opensearch", "solr"], "Full-text Search", "data", None, 0.75),
    (&["alembic", "flyway", "liquibase", "*migration"], "Schema Migration", "data", None, 0.65),
    (&["scrapy", "beautifulsoup", "playwright", "puppeteer", "selenium", "*crawl", "*spider"], "Web Scraping", "data", None, 0.75),
    (&["airflow", "dagster", "prefect", "luigi"], "Workflow Orchestration", "data", None, 0.75),

    // ── Infrastructure ───────────────────────────────────────────
    (&["celery", "rq", "dramatiq", "huey", "task-queue", "taskqueue", "bullmq", "arq"], "Task Queue", "infrastructure", None, 0.85),
    (&["rabbitmq", "kafka", "pulsar", "nats", "amqp", "mqtt"], "Message Queue", "infrastructure", None, 0.8),
    (&["docker", "dockerfile", "docker-compose", "podman", "containerd"], "Containerization", "infrastructure", None, 0.75),
    (&["kubernetes", "k8s", "helm", "kubectl"], "Container Orchestration", "infrastructure", None, 0.75),
    (&["terraform", "pulumi", "ansible", "cloudformation"], "Infrastructure As Code", "infrastructure", None, 0.7),
    (&["prometheus", "grafana", "opentelemetry", "otel", "sentry", "datadog"], "Observability", "infrastructure", None, 0.7),
    // 顶层 tests/ __tests__/ spec/ 目录本身就是强结构信号，与测试框架名并列
    (&["pytest", "jest", "vitest", "junit", "mocha", "cypress", "playwright-test", "tests", "test", "__tests__", "spec"], "Testing", "infrastructure", None, 0.6),
    (&["github-actions", "gitlab-ci", "jenkins", "circleci", "ci-cd", "cicd"], "CI/CD", "infrastructure", None, 0.65),
    (&["minio", "boto3", "s3", "oss2", "cos-python", "storage-client"], "Object Storage", "infrastructure", None, 0.7),

    // ── Media / 其它 ─────────────────────────────────────────────
    (&["pillow", "pil", "sharp", "imagemagick", "opencv-python"], "Image Processing", "media", None, 0.7),
    (&["pdf", "pypdf", "pdfplumber", "reportlab", "weasyprint", "pdf-lib"], "PDF Processing", "media", None, 0.75),
    (&["docx", "python-docx", "openpyxl", "xlsxwriter", "exceljs"], "Office Document", "media", None, 0.7),
];

/// 五个固定 Domain（能力树的顶层骨架）。
///
/// 每项是 `(稳定键, 展示名)`：**键**用于生成 id 与规则匹配（永不变），
/// **展示名**用于 UI（可随本地化调整）。两者必须分开——
/// 曾经规则表用英文 "Media" 而这里登记中文 "多媒体"，
/// 导致 `domain_id_for` 只能靠 slug 巧合对上，改一个字就全崩。
///
/// 顺序即图谱/列表的展示顺序。即使某 Domain 下暂无能力也应保留，
/// 以维持图谱结构稳定（对应 `CapabilityRepo::prune_orphans` 不删 Domain 的策略）。
pub const DOMAINS: &[(&str, &str)] = &[
    ("ai", "AI"),
    ("web", "Web"),
    ("data", "数据"),
    ("infrastructure", "基础设施"),
    ("media", "多媒体"),
];

/// 能力抽取结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityExtraction {
    /// 抽取出的能力（Capability 层）
    pub capabilities: Vec<Capability>,
    /// 抽取出的实现（Implementation 层）
    pub implementations: Vec<Capability>,
    /// 每个能力命中的信号（写入 relations 的 Evidence）
    pub evidence: BTreeMap<String, Vec<String>>,
    /// 未命中任何规则的信号（诊断用：提示词表可能需要补充）
    pub unmatched_signals: Vec<String>,
}

/// 能力抽取器。
#[derive(Debug, Clone, Default)]
pub struct CapabilityExtractor;

/// 抽取所需的输入信号。
#[derive(Debug, Clone, Default)]
pub struct CapabilitySignals {
    /// 依赖名（来自清单解析）
    pub dependencies: Vec<String>,
    /// 框架名（来自 `detect_frameworks`）
    pub frameworks: Vec<String>,
    /// 主语言
    pub language: Option<String>,
    /// 符号名（函数/类/组件名，来自符号抽取）
    pub symbol_names: Vec<String>,
    /// 顶层目录名（`worker/`、`migrations/` 这类结构信号）
    pub top_level_dirs: Vec<String>,
}

impl CapabilityExtractor {
    pub fn new() -> Self {
        Self
    }

    /// 抽取能力。
    ///
    /// `project_key` 用于生成稳定的能力 id（跨项目同名能力必须同 id，
    /// 否则关系边会指向不同的节点，图谱碎裂）。
    pub fn extract(&self, signals: &CapabilitySignals, confidence_floor: f64) -> CapabilityExtraction {
        // 归一化信号串：所有来源合并为一个小写串集合
        let mut haystack: Vec<String> = Vec::new();
        for d in &signals.dependencies {
            haystack.push(d.to_ascii_lowercase());
        }
        for f in &signals.frameworks {
            haystack.push(f.to_ascii_lowercase());
        }
        for s in &signals.symbol_names {
            // 符号名按 camelCase / snake_case 拆词，提高召回
            haystack.push(s.to_ascii_lowercase());
            haystack.extend(split_identifier(s).into_iter().map(|t| t.to_ascii_lowercase()));
        }
        for dir in &signals.top_level_dirs {
            haystack.push(dir.to_ascii_lowercase());
        }

        let joined = haystack.join(" ");

        // 按能力名聚合命中（同一能力可能被多条规则命中，取最强）
        let mut best: BTreeMap<String, CapabilityHit> = BTreeMap::new();

        for (needles, cap_name, domain, impl_name, base_strength) in CAPABILITY_RULES {
            let hits: Vec<&str> = needles
                .iter()
                .filter(|n| contains_token(&joined, n))
                .copied()
                .collect();
            if hits.is_empty() {
                continue;
            }
            // 命中多个同义词 → 强度小幅提升（但有上限，避免单一依赖堆高）
            let bonus = ((hits.len() - 1) as f64 * 0.03).min(0.09);
            let strength = (*base_strength + bonus).min(1.0);

            let entry = best.entry((*cap_name).to_string()).or_insert_with(|| CapabilityHit {
                domain,
                implementation: impl_name.map(str::to_string),
                signals: Vec::new(),
                strength: 0.0,
            });
            // 保留最强的那次命中
            if strength > entry.strength {
                entry.strength = strength;
                entry.domain = domain;
                if let Some(i) = impl_name {
                    entry.implementation = Some(i.to_string());
                }
            }
            for h in hits {
                if !entry.signals.contains(&h.to_string()) {
                    entry.signals.push(h.to_string());
                }
            }
        }

        // 语言本身也是一个弱信号（Rust / Python 项目的基础能力）
        if let Some(lang) = &signals.language {
            let l = lang.to_ascii_lowercase();
            if l == "rust" || l == "go" || l == "c++" {
                // 系统级语言：暗示性能敏感/底层能力
                best.entry("Systems Programming".to_string())
                    .or_insert_with(|| CapabilityHit {
                        domain: "infrastructure",
                        implementation: None,
                        signals: vec![lang.clone()],
                        strength: 0.55,
                    });
            }
        }

        let mut out = CapabilityExtraction::default();
        for (cap_name, hit) in best {
            if hit.strength < confidence_floor {
                continue; // 低于门槛的能力不进图谱（《技术设计书》§25）
            }
            let domain_id = domain_id_for(hit.domain);
            let cap_id = capability_id(&cap_name);

            // Capability 层节点
            let mut cap = Capability::new(
                cap_id.clone(),
                &cap_name,
                CapabilityLayer::Capability,
                Some(domain_id.clone()),
                hit.strength,
            )
            .expect("能力构造参数已由规则表保证合法");
            cap.description = format!(
                "由 {} 个信号推断：{}",
                hit.signals.len(),
                hit.signals.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
            );
            out.capabilities.push(cap);
            out.evidence.insert(cap_id.clone(), hit.signals.clone());

            // Implementation 层节点（有具体实现时）
            if let Some(impl_name) = &hit.implementation {
                let impl_id = implementation_id(impl_name, &cap_name);
                if let Ok(mut imp) = Capability::new(
                    impl_id,
                    impl_name,
                    CapabilityLayer::Implementation,
                    Some(cap_id.clone()),
                    (hit.strength * 0.95).clamp(0.0, 1.0),
                ) {
                    imp.description = format!("{cap_name} 的具体实现：{impl_name}");
                    out.implementations.push(imp);
                }
            }
        }

        // 诊断：未命中任何规则的依赖（帮助维护词表）。
        // 只保留前 20 条：一个项目可能有上百个依赖，全量返回会撑大响应体。
        out.unmatched_signals = signals
            .dependencies
            .iter()
            .filter(|d| {
                let dl = d.to_ascii_lowercase();
                !CAPABILITY_RULES
                    .iter()
                    .any(|(needles, ..)| needles.iter().any(|n| contains_token(&dl, n)))
            })
            .take(20)
            .cloned()
            .collect();

        // 稳定排序：能力强→弱，同强度按名称（保证输出确定，可回归测试）
        out.capabilities
            .sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.name.cmp(&b.name)));
        out.implementations.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Domain 稳定键 → 能力树节点 id。
///
/// 参数是 `DOMAINS` 的**第一列**（稳定键，如 "ai"），不是展示名。
/// 规则表的 domain 字段与这里用同一套键，避免"英文键 vs 中文标签"双重命名。
fn domain_id_for(domain_key: &str) -> String {
    let key = domain_key.to_ascii_lowercase();
    // 命中已登记的 Domain 用规范键；未登记的兜底用 slug（保证 id 仍合法）
    let resolved = DOMAINS
        .iter()
        .find(|(k, _)| k.to_ascii_lowercase() == key)
        .map(|(k, _)| k.to_ascii_lowercase())
        .unwrap_or_else(|| slug(domain_key));
    format!("cap_domain_{resolved}")
}

/// 能力名 → 稳定 id。
///
/// 必须稳定且跨项目一致：`Image Generation` 在任何项目里都得到同一 id，
/// 否则 `relations` 边会指向不同节点，图谱碎裂成互不相连的碎片。
fn capability_id(name: &str) -> String {
    format!("cap_{}", slug(name))
}

fn implementation_id(impl_name: &str, cap_name: &str) -> String {
    format!("impl_{}_{}", slug(cap_name), slug(impl_name))
}

/// 生成 id 安全的 slug：小写、非字母数字转 `_`、去重下划线。
///
/// 统一用 `_` 而非 `-`：与 `cap_` / `cap_domain_` / `impl_` 前缀保持一致，
/// 避免出现 `cap_image-generation` 这种混用两种分隔符的 id
/// （既不美观，也让"按前缀切分 id"这类调试代码容易出错）。
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_sep = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_sep = false;
        } else if !prev_sep {
            out.push('_');
            prev_sep = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// 判断 haystack 是否命中 needle。
///
/// # 匹配语义（两种，由 needle 前缀显式声明）
/// - **精确词匹配**（默认）：needle 前后都必须是非字母数字边界。
///   这样 `clip` 不会命中 `clipboard`、`three` 不会命中 `threshold`、
///   `ai` 不会命中 `email` —— 朴素 `contains` 会制造大量假能力。
/// - **词干前缀匹配**（needle 以 `*` 开头）：只要求左边界，右侧开放。
///   用于表达复数与词形变化，如 `*migration` 命中 `migrations`、
///   `*quantiz` 命中 `quantization` / `quantized`。
///
/// 🔑 为什么用显式 `*` 而不是"全部允许前缀"：
/// 全量前缀匹配会引入 `clip`→`clipboard`、`three`→`threshold` 这类假能力，
/// 而能力一旦进入图谱就会污染关系与洞察。让规则表**声明意图**，
/// 既能覆盖词形变化，又保持每条规则可审计、可单测。
///
/// # 分隔符归一
/// 匹配前 haystack 与 needle 都会经 `normalize_signal` 把 `_ - . / +` 统一成空格，
/// 因此规则 `video-generation` 能命中符号 `VideoGenerationPipeline`
/// 拆词后的 `video generation`（否则 camelCase 符号名几乎无法命中任何多词能力）。
fn contains_token(haystack: &str, needle: &str) -> bool {
    let (stem, n) = if let Some(rest) = needle.strip_prefix('*') {
        (true, normalize_signal(rest))
    } else {
        (false, normalize_signal(needle))
    };
    if n.is_empty() {
        return false;
    }
    let h = normalize_signal(haystack);
    let bytes = h.as_bytes();

    let mut from = 0;
    while let Some(rel) = h[from..].find(&n) {
        let start = from + rel;
        let end = start + n.len();
        // 左边界：字符串开头，或前一个字符不是字母数字
        let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        // 右边界：词干匹配时不要求（允许 quantiz → quantization）
        let right_ok = stem || end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if left_ok && right_ok {
            return true;
        }
        from = end.max(start + 1);
    }
    false
}

/// 归一化信号串：小写 + 把分隔符统一为空格。
///
/// 统一分隔符是"camelCase 符号名能命中多词能力"的前提：
/// `VideoGenerationPipeline` 拆词得到 `video generation pipeline`，
/// 规则 `video-generation` 归一后是 `video generation`，两者才能对上。
fn normalize_signal(s: &str) -> String {
    s.chars()
        .map(|c| {
            let l = c.to_ascii_lowercase();
            if matches!(l, '_' | '-' | '.' | '/' | '+' | ':') {
                ' '
            } else {
                l
            }
        })
        .collect()
}

/// 拆分标识符为词元：`VideoPipelineService` → [video, pipeline, service]。
///
/// 提高符号名的召回：`video_generation_service` 与 `VideoGenerationService`
/// 都应命中 "Video Generation" 能力。
fn split_identifier(name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == ' ' || c == '.' {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            continue;
        }
        // camelCase 边界：小写→大写，或 大写→大写后跟小写（HTMLParser → HTML, Parser）
        if c.is_ascii_uppercase() && !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase() || prev.is_ascii_digit() || (prev.is_ascii_uppercase() && next_lower) {
                tokens.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// 构建 Domain 层节点（能力树的根，始终存在）。
pub fn domain_capabilities() -> Vec<Capability> {
    DOMAINS
        .iter()
        .map(|(id, label)| {
            let mut c = Capability::new(
                format!("cap_domain_{id}"),
                *label,
                CapabilityLayer::Domain,
                None,
                1.0,
            )
            .expect("Domain 构造参数固定合法");
            c.description = "能力域（三层结构顶层）".to_string();
            c
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals(deps: &[&str]) -> CapabilitySignals {
        CapabilitySignals {
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    // ── 词元匹配（防假能力的关键）───────────────────────────────────

    /// 🔴 `contains("ai")` 会命中 "email"/"maintain"，必须用词边界匹配。
    #[test]
    fn token_matching_respects_word_boundaries() {
        assert!(contains_token("langchain-community", "langchain"));
        assert!(contains_token("my langchain app", "langchain"));
        // 子串不算命中
        assert!(!contains_token("email-service", "ai"));
        assert!(!contains_token("maintain", "ai"));
        assert!(!contains_token("plainchain", "langchain"));
        // 但连字符分隔的算
        assert!(contains_token("text-generation-inference", "text-generation"));
    }

    #[test]
    fn token_matching_empty_needle_is_false() {
        assert!(!contains_token("anything", ""));
    }

    // ── 标识符拆分 ──────────────────────────────────────────────────

    #[test]
    fn splits_snake_case() {
        assert_eq!(split_identifier("video_generation_service"), vec!["video", "generation", "service"]);
    }

    #[test]
    fn splits_camel_case() {
        assert_eq!(split_identifier("VideoPipelineService"), vec!["Video", "Pipeline", "Service"]);
    }

    #[test]
    fn splits_acronym_boundaries() {
        assert_eq!(split_identifier("HTMLParser"), vec!["HTML", "Parser"]);
        assert_eq!(split_identifier("parseHTTPResponse"), vec!["parse", "HTTP", "Response"]);
    }

    #[test]
    fn splits_mixed_separators() {
        assert_eq!(split_identifier("app.utils-helper"), vec!["app", "utils", "helper"]);
        assert_eq!(split_identifier("single"), vec!["single"]);
        assert!(split_identifier("").is_empty());
    }

    // ── slug 与 id 稳定性 ───────────────────────────────────────────

    /// 能力 id 必须跨项目稳定，否则关系边指向不同节点、图谱碎裂。
    #[test]
    fn capability_id_is_stable() {
        assert_eq!(capability_id("Image Generation"), capability_id("Image Generation"));
        // 分隔符统一用 `_`，与 cap_ / cap_domain_ / impl_ 前缀一致，
        // 不出现 cap_image-generation 这种混用两种分隔符的 id
        assert_eq!(capability_id("Image Generation"), "cap_image_generation");
    }

    #[test]
    fn slug_normalizes() {
        assert_eq!(slug("AI Video"), "ai_video");
        assert_eq!(slug("C++"), "c");
        assert_eq!(slug("  spaced  out  "), "spaced_out");
        assert_eq!(slug(""), "");
        assert!(!slug("A/B").contains('/'));
        assert!(!slug("A-B").contains('-'));
    }

    #[test]
    fn implementation_id_includes_capability() {
        // 同名实现在不同能力下必须不同 id（Qwen 既可能是文生图也可能是文生文）
        let a = implementation_id("Qwen", "Image Generation");
        let b = implementation_id("Qwen", "Text Generation");
        assert_ne!(a, b);
    }

    // ── Domain 骨架 ─────────────────────────────────────────────────

    #[test]
    fn domains_are_the_five_fixed_roots() {
        let ds = domain_capabilities();
        assert_eq!(ds.len(), 5);
        for d in &ds {
            assert_eq!(d.layer, CapabilityLayer::Domain);
            assert!(d.parent_id.is_none());
            assert_eq!(d.confidence, 1.0);
        }
        let labels: Vec<&str> = ds.iter().map(|d| d.name.as_str()).collect();
        assert!(labels.contains(&"AI"));
        assert!(labels.contains(&"Web"));
    }

    #[test]
    fn domain_id_lookup_matches_table() {
        assert_eq!(domain_id_for("AI"), "cap_domain_ai");
        assert_eq!(domain_id_for("Infrastructure"), "cap_domain_infrastructure");
        // 未知 domain 也要产出合法 id（不 panic）
        assert!(domain_id_for("Unknown").starts_with("cap_domain_"));
    }

    // ── 能力抽取 ────────────────────────────────────────────────────

    #[test]
    fn extracts_capability_from_dependency() {
        let out = CapabilityExtractor::new().extract(&signals(&["langchain", "chromadb"]), 0.5);
        let names: Vec<&str> = out.capabilities.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"RAG"), "实际: {names:?}");
        assert!(names.contains(&"Vector Search"));
    }

    #[test]
    fn assigns_correct_domain() {
        let out = CapabilityExtractor::new().extract(&signals(&["fastapi", "uvicorn"]), 0.5);
        let api = out.capabilities.iter().find(|c| c.name == "Web API").unwrap();
        assert_eq!(api.parent_id.as_deref(), Some("cap_domain_web"));
        assert_eq!(api.layer, CapabilityLayer::Capability);
    }

    #[test]
    fn creates_implementation_layer() {
        let out = CapabilityExtractor::new().extract(&signals(&["diffusers"]), 0.5);
        let img = out.capabilities.iter().find(|c| c.name == "Image Generation").unwrap();
        assert_eq!(img.layer, CapabilityLayer::Capability);
        let imp = out.implementations.iter().find(|i| i.name == "Diffusion").unwrap();
        assert_eq!(imp.layer, CapabilityLayer::Implementation);
        assert_eq!(imp.parent_id.as_deref(), Some(img.id.as_str()), "实现应挂在能力下");
    }

    /// 三层结构完整性：每个能力的 parent 必须是已登记的 Domain。
    #[test]
    fn every_capability_has_valid_domain_parent() {
        let all_deps = ["diffusers", "langchain", "react", "fastapi", "celery", "pandas", "ffmpeg", "redis"];
        let out = CapabilityExtractor::new().extract(&signals(&all_deps), 0.5);
        let domain_ids: Vec<String> = DOMAINS.iter().map(|(id, _)| format!("cap_domain_{id}")).collect();
        assert!(!out.capabilities.is_empty());
        for c in &out.capabilities {
            assert_eq!(c.layer, CapabilityLayer::Capability);
            let p = c.parent_id.as_ref().expect("能力必须有父 Domain");
            assert!(domain_ids.contains(p), "{p} 不是已登记的 Domain");
        }
    }

    /// 符号名也应产生能力（不只依赖清单）。
    #[test]
    fn extracts_from_symbol_names() {
        let s = CapabilitySignals {
            symbol_names: vec!["VideoGenerationPipeline".into(), "runOCR".into()],
            ..Default::default()
        };
        let out = CapabilityExtractor::new().extract(&s, 0.5);
        let names: Vec<&str> = out.capabilities.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Video Generation"), "实际: {names:?}");
    }

    /// camelCase 符号名拆词后也能命中（否则大量前端项目能力为空）。
    #[test]
    fn camel_case_symbol_hits_capability() {
        let s = CapabilitySignals {
            symbol_names: vec!["TaskQueueWorker".into()],
            ..Default::default()
        };
        let out = CapabilityExtractor::new().extract(&s, 0.5);
        let names: Vec<&str> = out.capabilities.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Task Queue"), "实际: {names:?}");
    }

    #[test]
    fn extracts_from_top_level_dirs() {
        let s = CapabilitySignals {
            top_level_dirs: vec!["migrations".into(), "tests".into()],
            ..Default::default()
        };
        let out = CapabilityExtractor::new().extract(&s, 0.5);
        let names: Vec<&str> = out.capabilities.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Schema Migration"));
        assert!(names.contains(&"Testing"));
    }

    /// 框架名也是信号（Next.js → Web Frontend）。
    #[test]
    fn extracts_from_frameworks() {
        let s = CapabilitySignals {
            frameworks: vec!["Next.js".into(), "Ant Design".into()],
            ..Default::default()
        };
        let out = CapabilityExtractor::new().extract(&s, 0.5);
        let names: Vec<&str> = out.capabilities.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Web Frontend"));
    }

    // ── 收敛性（防标签爆炸）─────────────────────────────────────────

    /// 🔴 同一能力被多条规则命中时只产生一个节点（否则标签爆炸）。
    #[test]
    fn duplicate_rules_collapse_to_one_capability() {
        // react / next / remix 都映射到 Web Frontend
        let out = CapabilityExtractor::new().extract(&signals(&["react", "next", "remix", "react-dom"]), 0.5);
        let frontend: Vec<_> = out.capabilities.iter().filter(|c| c.name == "Web Frontend").collect();
        assert_eq!(frontend.len(), 1, "Web Frontend 只应出现一次");
    }

    /// 多个同义词命中提升强度，但有上限（不因堆依赖而虚高）。
    #[test]
    fn multiple_synonyms_boost_strength_with_cap() {
        let one = CapabilityExtractor::new().extract(&signals(&["langchain"]), 0.5);
        let many = CapabilityExtractor::new().extract(&signals(&["langchain", "llamaindex", "haystack"]), 0.5);
        let a = one.capabilities.iter().find(|c| c.name == "RAG").unwrap().confidence;
        let b = many.capabilities.iter().find(|c| c.name == "RAG").unwrap().confidence;
        assert!(b > a, "多信号应提升置信度");
        assert!(b <= 1.0, "不得超过 1.0");
    }

    #[test]
    fn confidence_floor_filters_weak_capabilities() {
        // Systems Programming 强度 0.55，门槛 0.6 时应被过滤
        let s = CapabilitySignals { language: Some("Rust".into()), ..Default::default() };
        let low_floor = CapabilityExtractor::new().extract(&s, 0.5);
        assert!(low_floor.capabilities.iter().any(|c| c.name == "Systems Programming"));
        let high_floor = CapabilityExtractor::new().extract(&s, 0.6);
        assert!(!high_floor.capabilities.iter().any(|c| c.name == "Systems Programming"));
    }

    // ── Evidence ────────────────────────────────────────────────────

    /// 产品纪律：能力必须记录命中它的信号，否则无法解释。
    #[test]
    fn evidence_records_matching_signals() {
        let out = CapabilityExtractor::new().extract(&signals(&["langchain", "chromadb"]), 0.5);
        let rag_id = capability_id("RAG");
        let ev = out.evidence.get(&rag_id).expect("RAG 应有 evidence");
        assert!(ev.contains(&"langchain".to_string()), "实际: {ev:?}");
    }

    #[test]
    fn description_lists_signals() {
        let out = CapabilityExtractor::new().extract(&signals(&["langchain"]), 0.5);
        let rag = out.capabilities.iter().find(|c| c.name == "RAG").unwrap();
        assert!(rag.description.contains("langchain"), "描述应含信号: {}", rag.description);
    }

    // ── 排序确定性 ──────────────────────────────────────────────────

    /// 输出顺序必须确定，否则同一项目两次扫描的图谱节点顺序不同。
    #[test]
    fn output_is_deterministically_sorted() {
        let s = signals(&["diffusers", "langchain", "fastapi", "celery", "react"]);
        let a = CapabilityExtractor::new().extract(&s, 0.5);
        let b = CapabilityExtractor::new().extract(&s, 0.5);
        let na: Vec<&str> = a.capabilities.iter().map(|c| c.name.as_str()).collect();
        let nb: Vec<&str> = b.capabilities.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(na, nb);
        // 置信度降序
        for w in na.windows(2) {
            let c0 = a.capabilities.iter().find(|c| c.name == w[0]).unwrap();
            let c1 = a.capabilities.iter().find(|c| c.name == w[1]).unwrap();
            assert!(c0.confidence >= c1.confidence, "{w:?} 未按置信度降序");
        }
    }

    // ── 空输入 ──────────────────────────────────────────────────────

    #[test]
    fn no_signals_yields_no_capabilities() {
        let out = CapabilityExtractor::new().extract(&CapabilitySignals::default(), 0.5);
        assert!(out.capabilities.is_empty());
        assert!(out.implementations.is_empty());
    }

    #[test]
    fn unrelated_dependencies_yield_no_capabilities() {
        let out = CapabilityExtractor::new().extract(&signals(&["serde", "anyhow", "left-pad"]), 0.5);
        assert!(out.capabilities.is_empty());
        // 未命中的依赖应被记录，便于词表维护
        assert!(out.unmatched_signals.contains(&"serde".to_string()));
    }

    // ── 词表自洽 ────────────────────────────────────────────────────

    /// 所有规则的 domain 都必须是已登记的 5 个稳定键之一，否则能力树会丢弃它。
    ///
    /// 🔴 校验对象是 `DOMAINS` 的**第一列（稳定键）**，不是展示名。
    /// 规则表与注册表必须共用同一套键——曾经一边写 "Media" 一边写 "多媒体"，
    /// 只有靠 slug 巧合才能对上，改一个字就全崩。
    #[test]
    fn all_rules_use_registered_domains() {
        let valid: Vec<&str> = DOMAINS.iter().map(|(k, _)| *k).collect();
        for (needles, name, domain, _, strength) in CAPABILITY_RULES {
            assert!(
                valid.contains(domain),
                "能力 {name} 的 domain「{domain}」未登记（合法键: {valid:?}）"
            );
            assert!(!needles.is_empty(), "能力 {name} 没有匹配词");
            assert!(!name.is_empty());
            assert!(*strength > 0.0 && *strength <= 1.0, "能力 {name} 强度越界: {strength}");
            for n in *needles {
                assert!(!n.is_empty());
                assert_eq!(n, &n.to_ascii_lowercase(), "匹配词必须小写: {n}");
                // 词干标记只允许出现在开头，且后面必须有内容
                if let Some(rest) = n.strip_prefix('*') {
                    assert!(!rest.is_empty(), "「{n}」的 * 后必须有词干");
                    assert!(!rest.contains('*'), "「{n}」不应有多个 *");
                }
            }
        }
    }

    /// 能力名不应重复定义在不同 Domain（会导致同 id 不同 parent）。
    #[test]
    fn capability_names_map_to_single_domain() {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (_, name, domain, _, _) in CAPABILITY_RULES {
            if let Some(prev) = seen.get(*name) {
                assert_eq!(prev, domain, "能力 {name} 被分配到两个 Domain: {prev} 与 {domain}");
            }
            seen.insert(name, domain);
        }
    }

    #[test]
    fn extraction_serializes() {
        let out = CapabilityExtractor::new().extract(&signals(&["langchain"]), 0.5);
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains("capabilities"));
        let back: CapabilityExtraction = serde_json::from_str(&json).unwrap();
        assert_eq!(back.capabilities.len(), out.capabilities.len());
    }
}
