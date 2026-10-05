/**
 * AI 分析师（对话式）。
 *
 * # 🔴 三大 LLM 场景之一，也是本轮检索修复的最大受益者
 * 修复前：问"我有哪些重复实现的代码？"，即使库里有对应洞察也答"没有找到相关记录"
 * （整句中文查询在 trigram FTS 下必然落空 + 洞察根本不在检索范围内）。
 * 修复后：能召回洞察并给出带引用的回答。
 *
 * # 🔴 先检索后喂模型，引用走白名单
 * 后端 `ask()` 的流程是：先检索真实数据 → 把结果作为上下文喂模型 →
 * 模型引用必须在检索候选集白名单内，否则被拒。
 * `rejected_citations` 就是被拒的数量——这是**防幻觉审计**，
 * 非 0 时前端必须提示"部分引用未通过证据校验"，绝不假装模型说的都对。
 *
 * # 🔴 离线降级不是错误
 * 未配置模型时后端返回 200 + `generated_by: "deterministic"`，
 * 内容是直接从数据库检索并按固定模板拼装的、带真实出处的回答。
 * 这是**有用的降级**（比白屏强得多），要显示"离线检索"标识并引导去配置，
 * 而不是当成失败弹红色错误。
 *
 * # 🔴 多轮上下文
 * `history` 传给后端，让模型能理解"它的""这些"这类指代。
 * 但检索式降级回答不消费 history（它只按当前问题检索），
 * 所以降级模式下不假装支持多轮。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { askAnalyst } from "@/api/endpoints";
import type { AnalystResponse, AnalystTurn, SearchScope } from "@/api/types";
import { answerSourceLabel, isModelSource } from "@/api/types";
import { useToast } from "@/components/Toast";
import { Button, PageHead } from "@/components/ui";
import { Icon, type IconName } from "@/components/Icon";
import { MarkdownLite } from "@/components/MarkdownLite";
import { routeForLink } from "@/lib/navigate";

/** 界面上的一条消息（用户问 / 助手答）。 */
interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  /** 用户消息 = 问题文本；助手消息 = answer.content */
  content: string;
  /** 助手消息专有：完整响应（引用、来源、审计信息） */
  response?: AnalystResponse;
}

const SCOPES: { value: SearchScope; label: string }[] = [
  { value: "all", label: "全部" },
  { value: "projects", label: "项目" },
  { value: "assets", label: "资产" },
  { value: "insights", label: "洞察与机会" },
  { value: "capabilities", label: "能力" },
];

/** 初始推荐问题：都是能从真实数据检索到内容的问题，不是装饰文案。 */
const PRESETS: { icon: IconName; text: string }[] = [
  { icon: "repeat", text: "我有哪些重复实现的代码？" },
  { icon: "box", text: "哪些资产可以直接复用到新项目？" },
  { icon: "clock", text: "我有哪些项目很久没动了，还值得打捞吗？" },
  { icon: "bulb", text: "我具备的能力可以组合出什么新项目？" },
];

export function AnalystPage() {
  const navigate = useNavigate();
  const toast = useToast();
  const [params, setParams] = useSearchParams();

  const [input, setInput] = useState(params.get("q") ?? "");
  const [scope, setScope] = useState<SearchScope>((params.get("scope") as SearchScope) ?? "all");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [asking, setAsking] = useState(false);
  const nextId = useRef(1);
  const abortRef = useRef<AbortController | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  // messages 的 ref 镜像：让 ask() 读到最新消息而不必把它列进 useCallback 依赖
  // （否则 ask 每次消息变化都重建，连带 useEffect 的初始提问也会重跑）
  const messagesRef = useRef<ChatMessage[]>(messages);
  messagesRef.current = messages;

  // 从 URL 带入的问题（首页快捷入口 / 顶栏搜索跳转）自动提问
  const initialQ = params.get("q");
  const askedInitial = useRef(false);
  useEffect(() => {
    if (initialQ !== null && initialQ !== "" && !askedInitial.current) {
      askedInitial.current = true;
      void ask(initialQ, scope);
      // 清掉 URL 上的 q，避免刷新时重复提问
      const next = new URLSearchParams(params);
      next.delete("q");
      setParams(next, { replace: true });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 新消息时滚动到底部
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [messages, asking]);

  const ask = useCallback(
    async (question: string, currentScope: SearchScope) => {
      const q = question.trim();
      if (q === "" || asking) return;

      // 中断上一个未完成的请求（用户快速连问时，旧回答不该覆盖新的）
      abortRef.current?.abort();
      const controller = new AbortController();
      abortRef.current = controller;

      // 🔴 用 ref 镜像当前消息来构造 history，**不要在 setState 的 updater 里发请求**。
      //
      // 早期写法是把 `void (async () => askAnalyst(...))()` 塞进
      // `setMessages((prev) => {...})` 的 updater 内部——那是个真实 bug：
      // React 18 StrictMode 会把 updater **调用两次**（用于检测不纯的更新函数），
      // 于是同一个问题发出两次请求，后端跑两遍检索甚至调两次 LLM，
      // 用户还会看到两条重复回答。副作用必须留在 updater 之外。
      // 🔴 必须写 `flatMap<AnalystTurn>`：
      // 不显式指定类型参数时，TS 会用**第一个分支**推断泛型，
      // 于是 `assistant` 分支被判为不可赋值（role: "assistant" 不在 "user" 里）。
      const history = messagesRef.current.flatMap<AnalystTurn>((m) =>
        m.role === "user"
          ? [{ role: "user", content: m.content }]
          : [{ role: "assistant", content: m.content }],
      );

      const userMsg: ChatMessage = { id: nextId.current++, role: "user", content: q };
      setMessages((prev) => [...prev, userMsg]);
      setInput("");
      setAsking(true);

      try {
        const resp = await askAnalyst({ question: q, history, scope: currentScope }, controller.signal);
        setMessages((cur) => [
          ...cur,
          { id: nextId.current++, role: "assistant", content: resp.answer.content, response: resp },
        ]);
        // 🔴 防幻觉审计：被拒引用非 0 时必须提示用户，
        // 否则模型编造的引用被静默剔除，用户无从知道回答被裁剪过。
        if (resp.rejected_citations > 0) {
          toast.warning(
            `回答中有 ${resp.rejected_citations} 处引用未通过证据校验，已被剔除`,
            "系统只保留能在你的真实数据中找到出处的引用",
          );
        }
      } catch (err) {
        if (err instanceof DOMException && err.name === "AbortError") return;
        const msg = err instanceof Error ? err.message : "提问失败";
        const hint =
          err instanceof Error && "hint" in err ? String((err as { hint?: string }).hint ?? "") : "";
        // 错误作为一条助手消息呈现（而不是 toast）：
        // 用户需要看到"这个问题失败了"留在对话流里，
        // 否则对话看起来像凭空断掉，且 hint 里的引导也无处安放。
        setMessages((cur) => [
          ...cur,
          { id: nextId.current++, role: "assistant", content: `⚠️ ${msg}${hint ? `\n\n${hint}` : ""}` },
        ]);
      } finally {
        setAsking(false);
      }
    },
    [asking, toast],
  );

  const submit = useCallback(() => {
    void ask(input, scope);
  }, [ask, input, scope]);

  const clearChat = useCallback(() => {
    abortRef.current?.abort();
    setMessages([]);
    setAsking(false);
  }, []);

  useEffect(() => () => abortRef.current?.abort(), []);

  return (
    <>
      <PageHead
        title="AI 分析师"
        sub="用自然语言提问你的研发历史。回答基于你本机的真实数据，每处结论都标注出处。"
        actions={
          messages.length > 0 ? (
            <Button size="sm" icon="trash" onClick={clearChat}>
              清空对话
            </Button>
          ) : undefined
        }
      />

      <div className="filter-bar">
        <span style={{ color: "var(--color-text-3)", fontSize: "var(--fs-sm)" }}>检索范围</span>
        <div className="chips">
          {SCOPES.map((s) => (
            <button
              key={s.value}
              type="button"
              className={`chip${scope === s.value ? " is-active" : ""}`}
              onClick={() => setScope(s.value)}
              aria-pressed={scope === s.value}
            >
              {s.label}
            </button>
          ))}
        </div>
      </div>

      {messages.length === 0 ? (
        <section className="card" style={{ maxWidth: 720, margin: "24px auto", textAlign: "center", padding: "40px 32px" }}>
          <div
            style={{
              width: 56,
              height: 56,
              borderRadius: 16,
              background: "var(--grad-primary)",
              display: "grid",
              placeItems: "center",
              margin: "0 auto 18px",
              color: "#fff",
              boxShadow: "var(--glow-primary)",
            }}
          >
            <Icon name="spark" />
          </div>
          <h2 style={{ fontSize: "var(--fs-xl)", fontWeight: 700, margin: 0 }}>你想发现什么？</h2>
          <p style={{ color: "var(--color-text-2)", margin: "8px 0 22px", lineHeight: "var(--lh-base)" }}>
            我可以分析历史项目、发现可复用资产、总结技术能力、找出重复实现与组合机会。
          </p>
          <div className="chips" style={{ justifyContent: "center", marginTop: 18 }}>
            {PRESETS.map((p) => (
              <button key={p.text} className="chip" type="button" onClick={() => void ask(p.text, scope)}>
                <Icon name={p.icon} /> {p.text}
              </button>
            ))}
          </div>
          <div className="assist-hint" style={{ marginTop: 18 }}>
            未配置模型时会走离线检索式回答：不调用大模型，直接从已索引的真实数据里检索并标注出处。
            在 设置 → 大模型配置 接入模型后可得到跨项目推理与复用建议。
          </div>
        </section>
      ) : (
        <div className="chat-scroll" ref={scrollRef} style={{ maxHeight: "calc(100vh - 260px)", overflowY: "auto" }}>
          {messages.map((m) => (
            <MessageView key={m.id} msg={m} onNavigate={navigate} onAsk={(q) => void ask(q, scope)} />
          ))}
          {asking ? (
            <div className="chat-msg">
              <div className="chat-bubble" style={{ color: "var(--color-text-3)" }}>
                <Icon name="refresh" style={{ animation: "spin 1.2s linear infinite" }} /> 正在检索并生成回答…
              </div>
            </div>
          ) : null}
        </div>
      )}

      {/* 输入区（始终在底部，方便连续提问） */}
      <div className="card" style={{ marginTop: 16, padding: "10px 10px 10px 16px" }}>
        <div className="assist-input" style={{ border: "none", padding: 0 }}>
          <input
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                submit();
              }
            }}
            placeholder="输入你的问题，例如：我过去做过哪些视频生成项目？"
            aria-label="向 AI 分析师提问"
            disabled={asking}
          />
          <button className="send" onClick={submit} disabled={asking || input.trim() === ""} title="发送" aria-label="发送">
            <Icon name="send" />
          </button>
        </div>
      </div>
    </>
  );
}

function MessageView({
  msg,
  onNavigate,
  onAsk,
}: {
  msg: ChatMessage;
  onNavigate: (p: string) => void;
  onAsk: (q: string) => void;
}) {
  if (msg.role === "user") {
    return (
      <div className="chat-msg is-user">
        <div className="chat-bubble">{msg.content}</div>
      </div>
    );
  }

  const resp = msg.response;
  const source = resp?.answer.generated_by;
  const isOffline = source !== undefined && !isModelSource(source);

  return (
    <div className="chat-msg">
      {/* 🔴 后端回答是带 **加粗** / - 列表 / --- 分隔线的 Markdown。
          直接渲染纯文本会把这些符号原样甩给用户（截图里 "**洞察**（1 条）"
          和孤零零的 "---" 就是这么来的）。用 MarkdownLite 安全渲染。 */}
      <div className="chat-bubble">
        <MarkdownLite text={msg.content} />
      </div>

      {/* 元信息：来源、检索命中、耗时 */}
      {resp ? (
        <div className="chat-meta">
          <span
            title={isOffline ? "未调用大模型，直接从本地已索引数据检索并套用模板" : `由 ${answerSourceLabel(source!)} 生成`}
            style={{
              color: isOffline ? "var(--color-warning)" : "var(--color-success)",
              display: "inline-flex",
              alignItems: "center",
              gap: 4,
            }}
          >
            <Icon name={isOffline ? "db" : "spark"} />
            {isOffline ? "离线检索回答" : `模型：${answerSourceLabel(source!)}`}
          </span>
          <span>
            检索命中 {resp.context_hits}/{resp.context_total}
          </span>
          <span>用时 {resp.answer.took_ms}ms</span>
          {resp.used_substring_fallback ? <span style={{ color: "var(--color-warning)" }}>子串匹配</span> : null}
          {isOffline ? (
            <button
              type="button"
              className="link-more"
              onClick={() => onNavigate("/settings")}
              style={{ background: "none", border: "none", cursor: "pointer" }}
            >
              配置模型以获得综合分析 <Icon name="arr" />
            </button>
          ) : null}
        </div>
      ) : null}

      {/* 🔴 引用：点击跳转到出处。每处引用都对应真实数据。 */}
      {resp && resp.answer.citations.length > 0 ? (
        <div style={{ marginTop: 8, width: "100%" }}>
          <div className="card-sub" style={{ marginBottom: 6 }}>
            <Icon name="link" /> 引用出处（{resp.answer.citations.length}）
          </div>
          <div className="citation-list">
            {resp.answer.citations.map((c, i) => (
              <button
                key={i}
                className="citation-item"
                type="button"
                onClick={() => onNavigate(routeForLink(c.link.page, c.link.param))}
              >
                <span className="kind">
                  <Icon name={iconOfCitation(c.kind)} />
                </span>
                <div className="label">
                  {c.label}
                  {c.supports ? <div className="supports">{c.supports}</div> : null}
                </div>
              </button>
            ))}
          </div>
        </div>
      ) : null}

      {/* 🔴 后续问题必须可点击：它们由命中类型推导，是引导用户继续探索的入口。
          早期版本渲染成静态 Tag，用户看到"你可能还想问"却点不动，
          只能自己手打一遍问题——引导就成了摆设。 */}
      {resp && resp.answer.followups.length > 0 ? (
        <div style={{ marginTop: 8 }}>
          <div className="card-sub" style={{ marginBottom: 6 }}>
            你可能还想问
          </div>
          <div className="chips">
            {resp.answer.followups.map((f) => (
              <button key={f} type="button" className="chip" onClick={() => onAsk(f)}>
                {f}
              </button>
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function iconOfCitation(kind: string): IconName {
  switch (kind) {
    case "project":
      return "folder";
    case "asset":
      return "box";
    case "capability":
      return "graph";
    case "insight":
      return "drop";
    case "opportunity":
      return "bulb";
    default:
      return "doc";
  }
}
