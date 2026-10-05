//! FTS5 / LIKE 检索原语。
//!
//! 本模块是存储层唯一构造检索 SQL 的地方。上层（`spolia-search`）
//! 只拿到 `(id, score)` 候选集，**不接触任何 SQL**——这样换检索后端
//! （例如未来接向量库）时，改动只发生在这里。
//!
//! # 为什么必须有 LIKE 回退
//! SQLite 的 FTS5 `trigram` 分词器要求查询片段 **≥3 个字符**才能命中。
//! 中文里大量有效查询是 2 字（"视频"、"登录"、"缓存"），
//! 走 FTS 会静默返回空集——用户以为"没有这个功能"，实际是引擎的盲区。
//! 因此短查询必须降级为 LIKE 子串匹配。
//!
//! 实测确认（SQLite 3.53.2）：
//! - `MATCH '生成视频'` 命中 "生成视频的完整流程" ✅
//! - `MATCH '视频'` 返回 0 ❌（2 字低于 trigram 门槛）
//!
//! # 为什么必须转义 FTS 查询
//! 用户输入直接拼进 MATCH 会引发语法错误：搜 `C++`、`a"b`、`foo*`
//! 都会让 FTS5 解析失败并抛错。这里统一转成**双引号短语**
//! （内部 `"` 翻倍），语义即"子串匹配"，且对任意输入都安全。

use rusqlite::params;

use spolia_domain::{Asset, Capability, Insight, Opportunity, Project, StorageError};

use crate::assets;
use crate::capabilities;
use crate::insights;
use crate::opportunities;
use crate::projects;
use crate::Pool;

/// trigram 分词器的最小可匹配长度。低于此值 FTS 必然返回空。
pub const FTS_MIN_TOKEN_LEN: usize = 3;

/// 对含 CJK 的 token 触发 trigram 展开的最小字符数。
///
/// 🔴 为什么 3 字的词不展开：`"视频融"` 这样的 3 字短语本身就是**精确**匹配，
/// 展开成 trigram 只会引入 `"频融"` 这类更宽松的片段、降低精度。
/// 4 字起才值得展开——此时完整短语仍保留在表达式里（见下），
/// 展开只是**补充**召回能力，不牺牲精度。
const GRAM_EXPAND_MIN_CHARS: usize = 4;

/// 单个 token 展开后的最大 trigram 数。
///
/// 🔴 必须有上限：`SearchRequest` 允许 200 字查询，
/// 不截断的话会生成近 200 个 OR 分支，MATCH 表达式膨胀到几 KB，
/// 查询计划变慢且难以排查。正常中文问句 10-30 字（8-28 个 gram），
/// 64 的上限留了足够余量，超出部分丢弃对召回影响很小
/// （前半句的 gram 已足以定位相关文档）。
const MAX_GRAMS_PER_TOKEN: usize = 64;

/// 字符是否为 CJK（中日韩统一表意文字）。
///
/// 🔴 只按 CJK 判断是否展开，英文词**刻意不展开**：
/// 英文靠空格天然分词，`"pipeline"` 作短语匹配本就精确；
/// 展开成 `"pip"/"ipe"/"pel"…` 会把 `unpipelined`、`pipette` 全召回，
/// 精度明显下降。中文没有空格分词，才是 trigram 展开的唯一受益者。
///
/// 覆盖基本区 U+4E00..=U+9FFF 与扩展 A U+3400..=U+4DBF；
/// 更冷僻的扩展 B+ 区在代码检索场景里几乎不出现，不值得为此增加分支。
fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |   // CJK 统一表意文字（基本区）
        '\u{3400}'..='\u{4DBF}'     // CJK 扩展 A
    )
}

fn contains_cjk(s: &str) -> bool {
    s.chars().any(is_cjk_char)
}

/// 滑动窗口切分：把字符串切成所有连续 `n` 字符片段。
///
/// 与 trigram 分词器的切分方式一致，这是"查询能被索引命中"的前提：
/// 索引里存的是文档的所有连续 3 字片段，查询也必须用同样的片段去匹配。
fn sliding_grams(s: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < n {
        return Vec::new();
    }
    chars
        .windows(n)
        .map(|w| w.iter().collect())
        .take(MAX_GRAMS_PER_TOKEN)
        .collect()
}

/// 把单个 token 转成 FTS 查询片段。
///
/// # 为什么是「完整短语 OR 各 trigram」而不是二选一
/// 实测（SQLite trigram，文档="视频融合平台的多摄像头通道管理"）：
/// | 表达式 | "视频融合平台…" | "视频融资项目的风控系统" |
/// |---|---|---|
/// | `"视频融合"`（纯短语） | bm25 -0.469 | 不召回 |
/// | `"视频融" OR "频融合"`（纯 trigram） | bm25 -0.000 | bm25 -0.000（**误召回**） |
/// | `"视频融合" OR "视频融" OR "频融合"`（混合） | **bm25 -0.938** | bm25 -0.000 |
///
/// 混合式同时拿到两样东西：完整短语命中让 bm25 显著更强（排到最前），
/// trigram 负责在"整句被当成一个短语"时兜住召回。
/// 误召回项的 bm25 趋近 0，会在上层重排中被自然淘汰，不需要额外过滤。
fn token_phrase(t: &str) -> String {
    let full = quote_phrase(t);
    // 只对含 CJK 的长 token 展开
    if t.chars().count() < GRAM_EXPAND_MIN_CHARS || !contains_cjk(t) {
        return full;
    }
    let grams = sliding_grams(t, FTS_MIN_TOKEN_LEN);
    if grams.is_empty() {
        return full;
    }
    let mut parts = Vec::with_capacity(grams.len() + 1);
    parts.push(full);
    parts.extend(grams.iter().map(|g| quote_phrase(g)));
    format!("({})", parts.join(" OR "))
}

/// 把任意文本包成安全的 FTS5 短语字面量（内部 `"` 翻倍）。
///
/// 🔴 用户输入绝不能裸拼进 MATCH：搜 `C++`、`a"b`、`foo*` 都会让
/// FTS5 解析失败并抛错。包成双引号短语后语义即"子串匹配"，对任意输入安全。
fn quote_phrase(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// 单次召回上限。检索层还会重排与分页，这里放宽以保留候选多样性。
const RECALL_LIMIT: u32 = 200;

/// 转义 LIKE 通配符（`%`、`_`、`\`）。
///
/// 🔴 必须配合 SQL 里的 `ESCAPE '\'` 使用，否则转义符本身不生效。
///
/// 用户搜 "100%" 时若不转义，`%` 会匹配任意字符串，返回一堆无关结果。
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// 构造 LIKE 模式串：`%<已转义关键词>%`。
pub fn like_pattern(keyword: &str) -> String {
    format!("%{}%", escape_like(keyword))
}

/// 把用户查询编译为 FTS5 MATCH 表达式。
///
/// 返回 `None` 表示**该查询不适合走 FTS**（存在短于 3 字符的片段），
/// 调用方应改用 [`like_pattern`] 做子串匹配。
///
/// 编译规则：
/// 1. 按空白拆词，每个词独立成片段（词间 AND）——
///    这让 "video pipeline" 也能命中 "pipeline for video"。
/// 2. 每个词经 [`token_phrase`] 处理：含 CJK 的长词会展开成
///    「完整短语 OR 滑动 trigram」，其余保持精确短语。
/// 3. 任一词短于 [`FTS_MIN_TOKEN_LEN`] 即整体放弃 FTS。
///    部分降级（长词走 FTS、短词忽略）会给出**看似正常却漏结果**的答案，
///    比统一走 LIKE 更难排查。
///
/// # 🔴 为什么第 2 条是必需的（曾经的严重缺陷）
/// 中文句子**没有空格**，所以 `split_whitespace` 会把整句当成一个 token。
/// 旧实现直接把这个 token 包成一个短语：
/// ```text
/// "我有哪些重复实现的代码"  →  MATCH '"我有哪些重复实现的代码"'
/// ```
/// 而 trigram 短语匹配要求文档含这段**连续字符序列**——
/// 任何真实文档都不可能逐字包含用户的问法，于是**必然返回 0 条**。
///
/// 更糟的是 `compile_match_query` 对整句返回 `Some`（该"词"远超 3 字符），
/// 所以既不走 FTS 之外的回退，也不报错：系统认为查询合法，静默给出空结果。
/// 实测：库里存着"你在 2 个项目中重复实现了「Task Queue」"，
/// 搜"重复实现"能命中（4 字连续子串），搜"我有哪些重复实现的代码"命中 0 条。
///
/// 展开成 trigram OR 之后，问句里的"重复实现"等片段能各自命中，
/// 而完整短语仍保留在表达式中以保证精确匹配排到最前。
pub fn compile_match_query(user_query: &str) -> Option<String> {
    let tokens: Vec<&str> = user_query.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::with_capacity(tokens.len());
    for t in tokens {
        // 中文按字符数计；英文 3 字母以上。trigram 以字符为单位切分，
        // 故这里用 chars().count() 而非 len()（字节数会让中文全部超标）。
        if t.chars().count() < FTS_MIN_TOKEN_LEN {
            return None;
        }
        parts.push(token_phrase(t));
    }
    Some(parts.join(" AND "))
}

/// 该查询是否需要 LIKE 回退。
///
/// 暴露给前端：搜索结果区据此提示"短查询按子串匹配"，
/// 避免用户困惑于结果偏多（子串匹配不做词形归一）。
pub fn needs_substring_fallback(user_query: &str) -> bool {
    let q = user_query.trim();
    if q.is_empty() {
        return false;
    }
    compile_match_query(q).is_none()
}

/// 一次召回的候选项：实体 id + 相关性分数（**越大越相关**）。
///
/// FTS 的 bm25 是"越小越相关"，此处统一取负并归一，
/// 让上层排序逻辑只需处理一种方向（否则重排时极易搞反）。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub score: f64,
    /// 是否来自 LIKE 回退（影响排序理由的措辞）
    pub from_substring: bool,
}

/// 检索原语仓储。
pub struct RetrievalRepo<'a> {
    pool: &'a Pool,
}

impl<'a> RetrievalRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// 项目召回：FTS5 优先，短查询自动降级 LIKE。
    pub fn projects(&self, query: &str, limit: u32) -> Result<Vec<Candidate>, StorageError> {
        // 空查询必须显式返回空集：否则会降级成 LIKE '%%' 匹配全表，
        // 把"用户没输入"变成"搜到一切"。浏览模式由 list 接口负责，职责不同。
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.pool.get()?;
        let limit = i64::from(limit.clamp(1, RECALL_LIMIT));

        if let Some(match_expr) = compile_match_query(query) {
            let mut stmt = conn
                .prepare(
                    "SELECT project_id, bm25(projects_fts) FROM projects_fts
                     WHERE projects_fts MATCH ?1 ORDER BY bm25(projects_fts) LIMIT ?2",
                )
                .map_err(|e| StorageError::sqlite("准备项目全文检索", e))?;
            let rows = stmt
                .query_map(params![match_expr, limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(|e| StorageError::sqlite("执行项目全文检索", e))?;
            return collect(rows, false);
        }

        // LIKE 回退：按命中字段给分（名称 > 描述 > 标签/语言/框架）
        let pat = like_pattern(query);
        let mut stmt = conn
            .prepare(
                "SELECT id,
                        CASE
                          WHEN name LIKE ?1 ESCAPE '\\' THEN 1.0
                          WHEN description LIKE ?1 ESCAPE '\\' THEN 0.7
                          ELSE 0.5
                        END
                 FROM projects
                 WHERE name LIKE ?1 ESCAPE '\\'
                    OR description LIKE ?1 ESCAPE '\\'
                    OR tags_json LIKE ?1 ESCAPE '\\'
                    OR language LIKE ?1 ESCAPE '\\'
                    OR framework LIKE ?1 ESCAPE '\\'
                 ORDER BY 2 DESC, name ASC LIMIT ?2",
            )
            .map_err(|e| StorageError::sqlite("准备项目子串检索", e))?;
        let rows = stmt
            .query_map(params![pat, limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(|e| StorageError::sqlite("执行项目子串检索", e))?;
        collect(rows, true)
    }

    /// 资产召回：FTS5 优先，短查询自动降级 LIKE。
    pub fn assets(&self, query: &str, limit: u32) -> Result<Vec<Candidate>, StorageError> {
        // 空查询必须显式返回空集：否则会降级成 LIKE '%%' 匹配全表，
        // 把"用户没输入"变成"搜到一切"。浏览模式由 list 接口负责，职责不同。
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.pool.get()?;
        let limit = i64::from(limit.clamp(1, RECALL_LIMIT));

        if let Some(match_expr) = compile_match_query(query) {
            let mut stmt = conn
                .prepare(
                    "SELECT asset_id, bm25(assets_fts) FROM assets_fts
                     WHERE assets_fts MATCH ?1 ORDER BY bm25(assets_fts) LIMIT ?2",
                )
                .map_err(|e| StorageError::sqlite("准备资产全文检索", e))?;
            let rows = stmt
                .query_map(params![match_expr, limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(|e| StorageError::sqlite("执行资产全文检索", e))?;
            return collect(rows, false);
        }

        let pat = like_pattern(query);
        let mut stmt = conn
            .prepare(
                "SELECT id,
                        CASE
                          WHEN name LIKE ?1 ESCAPE '\\' THEN 1.0
                          WHEN description LIKE ?1 ESCAPE '\\' THEN 0.7
                          WHEN tags_json LIKE ?1 ESCAPE '\\' THEN 0.6
                          ELSE 0.45
                        END
                 FROM assets
                 WHERE name LIKE ?1 ESCAPE '\\'
                    OR description LIKE ?1 ESCAPE '\\'
                    OR tags_json LIKE ?1 ESCAPE '\\'
                    OR source_path LIKE ?1 ESCAPE '\\'
                 ORDER BY 2 DESC, reuse_score DESC LIMIT ?2",
            )
            .map_err(|e| StorageError::sqlite("准备资产子串检索", e))?;
        let rows = stmt
            .query_map(params![pat, limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(|e| StorageError::sqlite("执行资产子串检索", e))?;
        collect(rows, true)
    }

    /// 能力召回：FTS5 优先，短查询自动降级 LIKE。
    pub fn capabilities(&self, query: &str, limit: u32) -> Result<Vec<Candidate>, StorageError> {
        // 空查询必须显式返回空集：否则会降级成 LIKE '%%' 匹配全表，
        // 把"用户没输入"变成"搜到一切"。浏览模式由 list 接口负责，职责不同。
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.pool.get()?;
        let limit = i64::from(limit.clamp(1, RECALL_LIMIT));

        if let Some(match_expr) = compile_match_query(query) {
            let mut stmt = conn
                .prepare(
                    "SELECT capability_id, bm25(capabilities_fts) FROM capabilities_fts
                     WHERE capabilities_fts MATCH ?1 ORDER BY bm25(capabilities_fts) LIMIT ?2",
                )
                .map_err(|e| StorageError::sqlite("准备能力全文检索", e))?;
            let rows = stmt
                .query_map(params![match_expr, limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(|e| StorageError::sqlite("执行能力全文检索", e))?;
            return collect(rows, false);
        }

        let pat = like_pattern(query);
        let mut stmt = conn
            .prepare(
                "SELECT id,
                        CASE WHEN name LIKE ?1 ESCAPE '\\' THEN 1.0 ELSE 0.7 END
                 FROM capabilities
                 WHERE name LIKE ?1 ESCAPE '\\' OR description LIKE ?1 ESCAPE '\\'
                 ORDER BY 2 DESC, project_count DESC LIMIT ?2",
            )
            .map_err(|e| StorageError::sqlite("准备能力子串检索", e))?;
        let rows = stmt
            .query_map(params![pat, limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(|e| StorageError::sqlite("执行能力子串检索", e))?;
        collect(rows, true)
    }

    /// 批量取回项目实体（保持传入 id 顺序，缺失的静默跳过）。
    ///
    /// 用 id 列表而非再查一次：召回阶段已经确定了相关性顺序，
    /// 这里只负责"补全数据"，不得改变顺序。
    pub fn projects_by_ids(&self, ids: &[String]) -> Result<Vec<Project>, StorageError> {
        fetch_by_ids(
            self.pool,
            "projects",
            projects::COLS,
            projects::map_project,
            |p| p.id.as_str(),
            ids,
        )
    }

    /// 批量取回资产实体（保持传入 id 顺序）。
    pub fn assets_by_ids(&self, ids: &[String]) -> Result<Vec<Asset>, StorageError> {
        fetch_by_ids(
            self.pool,
            "assets",
            assets::COLS,
            assets::map_asset,
            |a| a.id.as_str(),
            ids,
        )
    }

    /// 批量取回能力实体（保持传入 id 顺序）。
    pub fn capabilities_by_ids(&self, ids: &[String]) -> Result<Vec<Capability>, StorageError> {
        fetch_by_ids(
            self.pool,
            "capabilities",
            capabilities::COLS,
            capabilities::map_capability,
            |c| c.id.as_str(),
            ids,
        )
    }

    /// 洞察召回：FTS5 优先，短查询自动降级 LIKE。
    ///
    /// # 🔴 为什么洞察必须可检索
    /// 洞察是系统给出的**结论**，而用户提问时最想拿到的正是结论。
    /// 此前洞察不在任何召回路径里，导致对话式分析师答不出库里明明有的东西：
    /// 用户问"我有哪些重复实现的代码？"，库里存着标题为
    /// "你在 2 个项目中重复实现了「Task Queue」"的洞察（带 10 条证据），
    /// 分析师却回答"没有找到相关记录"。
    ///
    /// # 为什么不过滤"已忽略"的洞察
    /// 刻意**不**在召回阶段排除 `user_feedback = 'ignored'`：
    /// - 洞察页的列表本来就显示已忽略项（带「已忽略」徽章），
    ///   搜索若排除它们，用户会遇到"页面上有 3 条，搜索只找到 2 条"——
    ///   这种不一致比多显示一条更令人困惑，且无法自查原因。
    /// - 用户是**主动提问**才触发检索的；他曾经忽略某条洞察，
    ///   不代表现在不想在回答里看到它。忽略状态由 `SearchHit.subtitle`
    ///   如实带出，让模型与用户都能看见，而不是悄悄藏起来。
    ///
    /// 隐藏过滤的代价是"用户不知道为什么搜不到"，显式标注的代价只是多一行说明。
    pub fn insights(&self, query: &str, limit: u32) -> Result<Vec<Candidate>, StorageError> {
        // 空查询必须显式返回空集：否则会降级成 LIKE '%%' 匹配全表，
        // 把"用户没输入"变成"搜到一切"。浏览模式由 list 接口负责，职责不同。
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.pool.get()?;
        let limit = i64::from(limit.clamp(1, RECALL_LIMIT));

        if let Some(match_expr) = compile_match_query(query) {
            let mut stmt = conn
                .prepare(
                    "SELECT insight_id, bm25(insights_fts) FROM insights_fts
                     WHERE insights_fts MATCH ?1 ORDER BY bm25(insights_fts) LIMIT ?2",
                )
                .map_err(|e| StorageError::sqlite("准备洞察全文检索", e))?;
            let rows = stmt
                .query_map(params![match_expr, limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(|e| StorageError::sqlite("执行洞察全文检索", e))?;
            return collect(rows, false);
        }

        // LIKE 回退：标题 > 描述 > 标签/类型/证据
        let pat = like_pattern(query);
        let mut stmt = conn
            .prepare(
                "SELECT id,
                        CASE
                          WHEN title LIKE ?1 ESCAPE '\\' THEN 1.0
                          WHEN description LIKE ?1 ESCAPE '\\' THEN 0.8
                          ELSE 0.6
                        END
                 FROM insights
                 WHERE title LIKE ?1 ESCAPE '\\'
                    OR description LIKE ?1 ESCAPE '\\'
                    OR tags_json LIKE ?1 ESCAPE '\\'
                    OR evidence_json LIKE ?1 ESCAPE '\\'
                 ORDER BY 2 DESC, confidence DESC, id ASC LIMIT ?2",
            )
            .map_err(|e| StorageError::sqlite("准备洞察子串检索", e))?;
        let rows = stmt
            .query_map(params![pat, limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(|e| StorageError::sqlite("执行洞察子串检索", e))?;
        collect(rows, true)
    }

    /// 机会召回：FTS5 优先，短查询自动降级 LIKE。
    ///
    /// 不过滤已忽略（Dismissed）的机会，理由与 `insights` 相同：
    /// 隐藏过滤会让"搜索结果与机会页对不上"，而用户主动搜索时
    /// 看到自己忽略过的机会是合理的（状态会如实带出）。
    pub fn opportunities(&self, query: &str, limit: u32) -> Result<Vec<Candidate>, StorageError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.pool.get()?;
        let limit = i64::from(limit.clamp(1, RECALL_LIMIT));

        if let Some(match_expr) = compile_match_query(query) {
            let mut stmt = conn
                .prepare(
                    "SELECT opportunity_id, bm25(opportunities_fts) FROM opportunities_fts
                     WHERE opportunities_fts MATCH ?1
                     ORDER BY bm25(opportunities_fts) LIMIT ?2",
                )
                .map_err(|e| StorageError::sqlite("准备机会全文检索", e))?;
            let rows = stmt
                .query_map(params![match_expr, limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(|e| StorageError::sqlite("执行机会全文检索", e))?;
            return collect(rows, false);
        }

        let pat = like_pattern(query);
        let mut stmt = conn
            .prepare(
                "SELECT id,
                        CASE
                          WHEN title LIKE ?1 ESCAPE '\\' THEN 1.0
                          WHEN description LIKE ?1 ESCAPE '\\' THEN 0.8
                          WHEN why LIKE ?1 ESCAPE '\\' THEN 0.7
                          ELSE 0.55
                        END
                 FROM opportunities
                 WHERE title LIKE ?1 ESCAPE '\\'
                    OR description LIKE ?1 ESCAPE '\\'
                    OR why LIKE ?1 ESCAPE '\\'
                    OR required_capabilities_json LIKE ?1 ESCAPE '\\'
                    OR missing_capabilities_json LIKE ?1 ESCAPE '\\'
                 ORDER BY 2 DESC, rating DESC, id ASC LIMIT ?2",
            )
            .map_err(|e| StorageError::sqlite("准备机会子串检索", e))?;
        let rows = stmt
            .query_map(params![pat, limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(|e| StorageError::sqlite("执行机会子串检索", e))?;
        collect(rows, true)
    }

    /// 批量取回洞察实体（保持传入 id 顺序）。
    pub fn insights_by_ids(&self, ids: &[String]) -> Result<Vec<Insight>, StorageError> {
        fetch_by_ids(
            self.pool,
            "insights",
            insights::COLS,
            insights::map_insight,
            |i| i.id.as_str(),
            ids,
        )
    }

    /// 批量取回机会实体（保持传入 id 顺序）。
    pub fn opportunities_by_ids(&self, ids: &[String]) -> Result<Vec<Opportunity>, StorageError> {
        fetch_by_ids(
            self.pool,
            "opportunities",
            opportunities::COLS,
            opportunities::map_opportunity,
            |o| o.id.as_str(),
            ids,
        )
    }
}

/// 收集召回行并把 bm25 归一为"越大越相关"。
fn collect(
    rows: impl Iterator<Item = rusqlite::Result<(String, f64)>>,
    from_substring: bool,
) -> Result<Vec<Candidate>, StorageError> {
    let mut out: Vec<Candidate> = Vec::new();
    for r in rows {
        let (id, raw) = r.map_err(|e| StorageError::sqlite("映射检索结果行", e))?;
        out.push(Candidate {
            id,
            score: normalize(raw, from_substring),
            from_substring,
        });
    }
    Ok(out)
}

/// 把原始分数归一到 0..=1（越大越相关）。
///
/// bm25 是**负值且越负越相关**（-12.3 比 -1.5 更相关）；
/// LIKE 分支已在 SQL 里给出 0..=1 的合成分，直接钳位透传。
///
/// 🔴 方向不能搞反：早期实现用 `1/(1+|bm25|)`，得到的是"越相关分越低"，
/// 整个排序倒置。正确映射是 `1 - 1/(1+|bm25|)`，随相关度单调递增。
fn normalize(raw: f64, from_substring: bool) -> f64 {
    if from_substring {
        return raw.clamp(0.0, 1.0);
    }
    let magnitude = raw.abs();
    1.0 - 1.0 / (1.0 + magnitude)
}

/// 按 id 列表取回实体，**保持传入顺序**。
///
/// SQLite 的 `IN (…)` 不保证结果顺序，故先查回再按原 id 序列重排。
/// 这一步不能省：否则搜索结果的相关性排序会被数据库的存储顺序覆盖。
///
/// `id_of` 由调用方提供（三类实体无公共 trait），避免在此引入 downcast
/// 或 serde 反射——那两种做法都会把"取 id"变成运行时才可能失败的操作。
fn fetch_by_ids<T>(
    pool: &Pool,
    table: &str,
    cols: &str,
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    id_of: fn(&T) -> &str,
    ids: &[String],
) -> Result<Vec<T>, StorageError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = pool.get()?;
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("SELECT {cols} FROM {table} WHERE id IN ({placeholders})");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StorageError::sqlite(format!("准备 {table} 批量查询"), e))?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = ids
        .iter()
        .map(|s| s as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(refs.iter()), map)
        .map_err(|e| StorageError::sqlite(format!("执行 {table} 批量查询"), e))?;

    let mut found: Vec<(String, T)> = Vec::with_capacity(ids.len());
    for r in rows {
        let item = r.map_err(|e| StorageError::sqlite(format!("映射 {table} 行"), e))?;
        let id = id_of(&item).to_string();
        found.push((id, item));
    }

    // 按召回顺序重排；库中已不存在的 id 自然被跳过
    let mut out: Vec<T> = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(pos) = found.iter().position(|(fid, _)| fid == id) {
            let (_, item) = found.remove(pos);
            out.push(item);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use spolia_domain::{AssetType, CapabilityLayer, CodeStats, Evidence, ProjectStatus};

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    fn sample_project(id: &str, name: &str, desc: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("/tmp/{id}"),
            description: desc.into(),
            language: "Python".into(),
            framework: "FastAPI".into(),
            created_at: None,
            updated_at: Some("2026-09-01".into()),
            last_commit_at: Some("2026-09-20".into()),
            status: ProjectStatus::Active,
            health_score: 80,
            completeness: None,
            tags: vec!["video".into()],
            sensitive: false,
            stats: CodeStats::default(),
            scan: spolia_domain::ScanFacts::default(),
            ai_profile: None,
        }
    }

    fn sample_asset(id: &str, pid: &str, name: &str, desc: &str) -> Asset {
        Asset {
            id: id.into(),
            project_id: pid.into(),
            asset_type: AssetType::Component,
            name: name.into(),
            description: desc.into(),
            content: None,
            source_path: format!("src/{id}.py"),
            confidence: 0.9,
            reuse_score: 0.8,
            generality: 0.7,
            stability: 0.6,
            tags: vec!["python".into()],
            created_at: "2026-09-01".into(),
            // 证据门禁要求至少一个来源文件/提交，否则资产不会入库（产品红线）。
            // 测试样本必须带证据，否则 upsert 静默返回 false、召回为空。
            evidence: Evidence {
                files: vec![format!("src/{id}.py")],
                ..Evidence::default()
            },
            user_feedback: None,
        }
    }

    fn sample_cap(id: &str, name: &str, desc: &str) -> Capability {
        let mut c = Capability::new(id, name, CapabilityLayer::Domain, None, 0.9).unwrap();
        c.description = desc.to_string();
        c
    }

    // ── 转义（安全与正确性的基础）────────────────────────────────

    #[test]
    fn escapes_like_wildcards() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("C:\\path"), "C:\\\\path");
        assert_eq!(escape_like("plain"), "plain");
    }

    /// 用户搜 "100%" 不得匹配任意串——这是 escape_like 存在的唯一理由。
    #[test]
    fn percent_in_query_is_literal_not_wildcard() {
        let d = db();
        d.projects()
            .upsert_batch(&[
                sample_project("p1", "折扣 100% 覆盖", "全额"),
                sample_project("p2", "其它项目", "无关内容"),
            ])
            .unwrap();
        let repo = RetrievalRepo::new(d.pool());
        let hits = repo.projects("100%", 20).unwrap();
        assert_eq!(hits.len(), 1, "含 % 的查询应按字面匹配，只命中 p1");
        assert_eq!(hits[0].id, "p1");
    }

    /// 搜 `C++`、`a"b` 等含 FTS 元字符的输入不得让 SQL 报错。
    #[test]
    fn fts_metacharacters_do_not_break_query() {
        let d = db();
        d.projects()
            .upsert_batch(&[sample_project("p1", "C++ 渲染器", "图形")])
            .unwrap();
        let repo = RetrievalRepo::new(d.pool());
        for q in ["C++", "a\"b", "foo*", "(video", "\"\""] {
            // 关键断言：不 panic、不返回 Err
            let r = repo.projects(q, 10);
            assert!(r.is_ok(), "查询 {q:?} 不应导致 SQL 错误: {:?}", r.err());
        }
    }

    #[test]
    fn compiles_match_query_as_phrases() {
        // 英文：靠空格天然分词，短语匹配本就精确，**不展开**
        // （展开成 "pip"/"ipe"/… 会把 unpipelined、pipette 全召回）
        assert_eq!(
            compile_match_query("video pipeline").as_deref(),
            Some("\"video\" AND \"pipeline\"")
        );
    }

    /// 🔴 3 字 CJK 不展开：`"生成视"` 本身就是精确短语，
    /// 展开只会引入更宽松的片段、降低精度。
    #[test]
    fn three_char_cjk_stays_exact_phrase() {
        assert_eq!(
            compile_match_query("生成视").as_deref(),
            Some("\"生成视\"")
        );
    }

    /// 🔴 4 字及以上 CJK 必须展开成「完整短语 OR 滑动 trigram」。
    ///
    /// 完整短语保留在表达式里，保证精确匹配拿到更强的 bm25（排到最前）；
    /// trigram 负责兜住召回。
    #[test]
    fn four_char_cjk_expands_to_grams() {
        assert_eq!(
            compile_match_query("生成视频").as_deref(),
            Some("(\"生成视频\" OR \"生成视\" OR \"成视频\")")
        );
    }

    /// 🔴 回归：中文整句曾被当成一个短语，而 trigram 短语匹配要求文档
    /// 含这段**连续字符序列**——真实文档不可能逐字包含用户问法，
    /// 于是必然返回 0 条。而 `compile_match_query` 又返回 `Some`
    /// （该"词"远超 3 字符），既不回退也不报错：静默给出空结果。
    ///
    /// 实测对照：库里存着"你在 2 个项目中重复实现了「Task Queue」"，
    /// 搜"重复实现"能命中，搜整句问法命中 0 条。
    #[test]
    fn full_chinese_sentence_expands_rather_than_becoming_one_phrase() {
        let expr = compile_match_query("我有哪些重复实现的代码").expect("长句应可编译");
        // 不是单个整句短语
        assert_ne!(expr, "\"我有哪些重复实现的代码\"");
        // 关键内容片段被切出来，才能命中文档里的"重复实现"
        assert!(expr.contains("\"重复实\""), "{expr}");
        assert!(expr.contains("\"复实现\""), "{expr}");
        // 完整问句仍保留（虽然几乎不会命中，但不影响正确性）
        assert!(expr.contains("\"我有哪些重复实现的代码\""), "{expr}");
    }

    /// 多个 token 之间仍是 AND；每个 token 各自决定是否展开。
    #[test]
    fn mixed_tokens_expand_individually() {
        // "Task"（4 字母，无 CJK）不展开；"视频融合"（4 字 CJK）展开
        let expr = compile_match_query("Task 视频融合").expect("应可编译");
        assert!(expr.contains("\"Task\""), "{expr}");
        assert!(!expr.contains("\"Tas\""), "英文不该被 trigram 化: {expr}");
        assert!(expr.contains("\"视频融\""), "{expr}");
        assert!(expr.contains(" AND "), "token 之间应是 AND: {expr}");
    }

    /// 🔴 展开数量必须有上限：200 字查询若不截断，
    /// MATCH 表达式会膨胀到几 KB，查询计划变慢且难以排查。
    #[test]
    fn expansion_is_bounded_for_very_long_query() {
        let long = "视".repeat(300);
        let expr = compile_match_query(&long).expect("应可编译");
        let gram_count = expr.matches(" OR ").count() + 1;
        assert!(
            gram_count <= MAX_GRAMS_PER_TOKEN + 1,
            "展开数应受 MAX_GRAMS_PER_TOKEN 限制，实得 {gram_count}"
        );
    }

    #[test]
    fn cjk_detection_covers_main_ranges() {
        assert!(contains_cjk("视频"));
        assert!(contains_cjk("AI视频")); // 混合也算
        assert!(!contains_cjk("pipeline"));
        assert!(!contains_cjk("C++"));
        assert!(!contains_cjk("123"));
        // 扩展 A 区（U+3400..=U+4DBF）
        assert!(contains_cjk("\u{3400}"));
    }

    /// 🔴 特殊字符必须仍然安全：展开逻辑不得破坏引号转义。
    /// 用户搜 `a"b`、`C++`、`foo*` 都不能让 FTS5 解析失败。
    #[test]
    fn expansion_preserves_escaping() {
        // 含引号的 CJK token：内部 " 必须翻倍
        let expr = compile_match_query("他说\"视频融合\"很好").expect("应可编译");
        assert!(expr.contains("\"\""), "内部引号应被翻倍: {expr}");

        // 编译出的表达式必须是**合法 FTS5 语法**（能被真实解析）。
        // 用真库跑一次：语法错误会直接抛，比人工比对字符串可靠得多。
        let d = db();
        let conn = d.conn().unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE __probe USING fts5(x, tokenize='trigram');
             INSERT INTO __probe VALUES('他说视频融合很好');",
        )
        .unwrap();

        for q in [
            "他说\"视频融合\"很好",
            "C++ 模板元编程",
            "a\"b 引号测试",
            "foo* 星号查询",
            "(括号) 与 AND 关键字",
        ] {
            let Some(expr) = compile_match_query(q) else {
                continue; // 走 LIKE 回退，不需要 FTS 语法校验
            };
            let r = conn.query_row(
                "SELECT count(*) FROM __probe WHERE __probe MATCH ?1",
                params![expr],
                |r| r.get::<_, i64>(0),
            );
            assert!(r.is_ok(), "查询 {q:?} 编译出的表达式语法非法: {expr}");
        }
    }

    #[test]
    fn doubles_inner_quotes() {
        // FTS5 短语内 `"` 必须翻倍，否则语法错误
        assert_eq!(
            compile_match_query("say \"hello\" now").as_deref(),
            Some("\"say\" AND \"\"\"hello\"\"\" AND \"now\"")
        );
    }

    /// 🔴 2 字中文查询必须走 LIKE，否则静默返回空集。
    #[test]
    fn short_chinese_query_falls_back() {
        assert_eq!(compile_match_query("视频"), None);
        assert!(needs_substring_fallback("视频"));
        assert!(!needs_substring_fallback("视频生成"));
        assert!(!needs_substring_fallback("pipeline"));
        assert!(!needs_substring_fallback("   "), "空白查询不算回退");
    }

    #[test]
    fn short_token_in_multiword_query_forces_fallback() {
        // "AI 视频生成" 含 2 字 token：部分降级会给出漏结果的假答案，
        // 统一走 LIKE 更可预测
        assert_eq!(compile_match_query("AI 视频生成"), None);
    }

    #[test]
    fn blank_query_compiles_to_none() {
        assert_eq!(compile_match_query(""), None);
        assert_eq!(compile_match_query("   \t "), None);
    }

    // ── 召回 ─────────────────────────────────────────────────────

    #[test]
    fn fts_recall_finds_chinese_substring() {
        let d = db();
        d.projects()
            .upsert_batch(&[
                sample_project("p1", "视频管线", "生成视频的完整流程"),
                sample_project("p2", "图片工具", "批处理图片"),
            ])
            .unwrap();
        let repo = RetrievalRepo::new(d.pool());
        let hits = repo.projects("生成视频", 20).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "p1");
        assert!(!hits[0].from_substring, "3 字以上应走 FTS");
        assert!(hits[0].score > 0.0 && hits[0].score <= 1.0);
    }

    #[test]
    fn like_fallback_finds_two_char_chinese() {
        let d = db();
        d.projects()
            .upsert_batch(&[
                sample_project("p1", "视频管线", "生成视频的完整流程"),
                sample_project("p2", "图片工具", "批处理图片"),
            ])
            .unwrap();
        let repo = RetrievalRepo::new(d.pool());
        let hits = repo.projects("视频", 20).unwrap();
        assert_eq!(hits.len(), 1, "LIKE 回退必须能找到 2 字中文");
        assert_eq!(hits[0].id, "p1");
        assert!(hits[0].from_substring);
    }

    /// 名称命中必须排在描述命中之前（否则搜项目名时被无关项目淹没）。
    #[test]
    fn name_match_outranks_description_match() {
        let d = db();
        d.projects()
            .upsert_batch(&[
                sample_project("p_desc", "其它工具", "包含 视频 处理功能"),
                sample_project("p_name", "视频编辑器", "剪辑工具"),
            ])
            .unwrap();
        let repo = RetrievalRepo::new(d.pool());
        let hits = repo.projects("视频", 20).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "p_name", "名称命中应排第一");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn asset_and_capability_recall_work() {
        let d = db();
        d.projects().upsert(&sample_project("p1", "demo", "d")).unwrap();
        d.assets()
            .upsert(&sample_asset("a1", "p1", "VideoPipeline", "视频生成管线"))
            .unwrap();
        d.capabilities().upsert(&sample_cap("c1", "AI", "人工智能能力")).unwrap();

        let repo = RetrievalRepo::new(d.pool());
        let a = repo.assets("Pipeline", 20).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].id, "a1");

        let c = repo.capabilities("人工智能", 20).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id, "c1");
    }

    #[test]
    fn empty_query_returns_nothing() {
        let d = db();
        d.projects().upsert(&sample_project("p1", "demo", "d")).unwrap();
        let repo = RetrievalRepo::new(d.pool());
        // 空查询不应返回全表（浏览模式由 list 接口负责，职责不同）
        assert!(repo.projects("", 20).unwrap().is_empty());
        assert!(repo.assets("  ", 20).unwrap().is_empty());
    }

    // ── 批量取回（顺序必须保持）──────────────────────────────────

    #[test]
    fn by_ids_preserves_recall_order() {
        let d = db();
        d.projects()
            .upsert_batch(&[
                sample_project("p1", "a", "d"),
                sample_project("p2", "b", "d"),
                sample_project("p3", "c", "d"),
            ])
            .unwrap();
        let repo = RetrievalRepo::new(d.pool());
        // 故意打乱顺序：IN 查询不保证顺序，必须重排
        let got = repo
            .projects_by_ids(&["p3".into(), "p1".into(), "p2".into()])
            .unwrap();
        let ids: Vec<&str> = got.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["p3", "p1", "p2"], "必须保持召回顺序");
    }

    #[test]
    fn by_ids_skips_missing_rows() {
        let d = db();
        d.projects().upsert(&sample_project("p1", "a", "d")).unwrap();
        let repo = RetrievalRepo::new(d.pool());
        let got = repo
            .projects_by_ids(&["p1".into(), "ghost".into()])
            .unwrap();
        assert_eq!(got.len(), 1, "已删除的 id 应被跳过而非报错");
        assert_eq!(got[0].id, "p1");
    }

    #[test]
    fn by_ids_handles_empty_input() {
        let d = db();
        let repo = RetrievalRepo::new(d.pool());
        assert!(repo.projects_by_ids(&[]).unwrap().is_empty());
        assert!(repo.assets_by_ids(&[]).unwrap().is_empty());
        assert!(repo.capabilities_by_ids(&[]).unwrap().is_empty());
    }

    #[test]
    fn assets_by_ids_returns_full_entities() {
        let d = db();
        d.projects().upsert(&sample_project("p1", "demo", "d")).unwrap();
        d.assets()
            .upsert(&sample_asset("a1", "p1", "VideoPipeline", "视频生成管线"))
            .unwrap();
        let repo = RetrievalRepo::new(d.pool());
        let got = repo.assets_by_ids(&["a1".into()]).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "VideoPipeline");
        assert_eq!(got[0].asset_type, AssetType::Component);
    }

    #[test]
    fn limit_is_respected_and_clamped() {
        let d = db();
        let batch: Vec<Project> = (0..10)
            .map(|i| sample_project(&format!("p{i}"), &format!("视频工具{i}"), "d"))
            .collect();
        d.projects().upsert_batch(&batch).unwrap();
        let repo = RetrievalRepo::new(d.pool());
        assert_eq!(repo.projects("视频", 3).unwrap().len(), 3);
        // limit 0 应钳到 1 而非报错或返回全部
        assert_eq!(repo.projects("视频", 0).unwrap().len(), 1);
    }

    #[test]
    fn normalize_prefers_smaller_bm25() {
        // bm25 越负越相关 → 归一后分越高
        assert!(normalize(-12.0, false) > normalize(-1.5, false));
        assert!(normalize(0.0, false) <= 1.0);
        // LIKE 分支已在 SQL 里给出 0..=1，越界值被钳住
        assert_eq!(normalize(5.0, true), 1.0);
        assert_eq!(normalize(-1.0, true), 0.0);
    }
}

