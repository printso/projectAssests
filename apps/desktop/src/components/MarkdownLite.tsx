/**
 * 轻量 Markdown 渲染（**加粗** / 列表 / 分隔线 / 换行）。
 *
 * # 🔴 为什么不用现成库，也不用 dangerouslySetInnerHTML
 * 后端回答里含**用户数据**（项目名、资产名、文件路径），
 * 用 `dangerouslySetInnerHTML` 渲染等于把 XSS 的口子重新打开
 * （刚在 snippet 上修过一个同类漏洞，不能在这里再开一个）。
 *
 * 引入 marked / react-markdown 则为一个极小的子集（加粗+列表+分隔线）
 * 拉进几十 KB 依赖与一整类解析器攻击面，不值得。
 *
 * 这里只支持后端模板实际会产出的四种结构，逐行解析成 React 元素：
 * - `**文本**` → <strong>（行内可多处）
 * - `- 文本`   → 列表项
 * - `---`      → 分隔线
 * - 空行       → 段落间隔
 * 其余按纯文本换行渲染。React 自动转义文本节点，天然安全。
 */

import type { ReactNode } from "react";

/** 把一行内的 `**...**` 解析成 <strong> 与普通文本的交替序列。 */
function renderInline(line: string, keyPrefix: string): ReactNode[] {
  const out: ReactNode[] = [];
  const re = /\*\*(.+?)\*\*/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = re.exec(line)) !== null) {
    if (m.index > last) out.push(line.slice(last, m.index));
    out.push(<strong key={`${keyPrefix}-b${i++}`}>{m[1]}</strong>);
    last = m.index + m[0].length;
  }
  if (last < line.length) out.push(line.slice(last));
  return out;
}

export function MarkdownLite({ text }: { text: string }) {
  const lines = text.split("\n");
  const blocks: ReactNode[] = [];
  let listBuffer: string[] = [];
  let key = 0;

  const flushList = () => {
    if (listBuffer.length === 0) return;
    const items = listBuffer;
    listBuffer = [];
    blocks.push(
      <ul key={`ul${key++}`} style={{ margin: "6px 0", paddingLeft: 20 }}>
        {items.map((it, i) => (
          <li key={i} style={{ margin: "2px 0" }}>
            {renderInline(it, `li${key}-${i}`)}
          </li>
        ))}
      </ul>,
    );
  };

  for (const raw of lines) {
    const line = raw.trimEnd();
    if (line.trim() === "") {
      flushList();
      continue;
    }
    if (/^-{3,}$/.test(line.trim())) {
      flushList();
      blocks.push(
        <hr
          key={`hr${key++}`}
          style={{ border: "none", borderTop: "1px solid var(--color-border)", margin: "10px 0" }}
        />,
      );
      continue;
    }
    if (line.trimStart().startsWith("- ")) {
      listBuffer.push(line.trimStart().slice(2));
      continue;
    }
    flushList();
    blocks.push(
      <p key={`p${key++}`} style={{ margin: "4px 0", whiteSpace: "pre-wrap" }}>
        {renderInline(line, `p${key}`)}
      </p>,
    );
  }
  flushList();

  return <div style={{ lineHeight: "var(--lh-base)" }}>{blocks}</div>;
}
