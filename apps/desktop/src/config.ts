/**
 * 前端常量配置。
 *
 * 🔴 品牌信息在后端 `spolia_domain::BRAND` 里也有一份，但**没有 API 暴露它**。
 * 这里刻意保留副本并注明来源，而不是加一个只返回两个字符串的端点：
 * 品牌名改动频率极低（改了要同时更新文档、图标、安装包名），
 * 为它增加一次网络往返和一个端点不划算。
 *
 * 若将来品牌需要可配置（例如白标版本），再把它并入 `/api/settings` 或 `/api/health`，
 * 届时删掉这里并改从接口读取。
 */

export const BRAND = {
  name: "Spolia",
  sub: "Your Personal R&D OS",
  logo: "S",
  tagline: "让过去的每一个项目，都成为你未来的可能性",
} as const;

/**
 * 侧栏导航定义。
 *
 * 🔴 与原型 `NAV` 的关键差异：**不含 count 字段**。
 * 原型的计数来自 `MOCK.scale`（projects: 128, assets: 1284, capabilities: 47），
 * 那是硬编码的假数字。真实计数由 `useNavCounts` 从后端拉取后注入，
 * 拉不到就不显示——宁可没有数字，也不能显示编造的数字。
 */
export interface NavItemDef {
  /** 路由路径 */
  path: string;
  /** 图标名 */
  icon: string;
  label: string;
  /**
   * 计数的来源键。
   * 由 `useNavCounts` 据后端真实统计填充；未定义则该项不显示计数。
   */
  countKey?: "projects" | "assets" | "insights" | "opportunities";
  /** 该项处于"开发中/阶段三"，点击后显示说明而非空页面 */
  pending?: string;
}

export interface NavGroupDef {
  /** 分组标题（null = 不显示标题） */
  group: string | null;
  items: NavItemDef[];
}

export const NAV: NavGroupDef[] = [
  {
    group: null,
    items: [
      { path: "/", icon: "home", label: "My R&D" },
      { path: "/projects", icon: "folder", label: "项目", countKey: "projects" },
      { path: "/assets", icon: "box", label: "资产", countKey: "assets" },
      { path: "/graph", icon: "graph", label: "知识图谱" },
      { path: "/insights", icon: "drop", label: "洞察", countKey: "insights" },
      { path: "/opportunities", icon: "bulb", label: "机会", countKey: "opportunities" },
    ],
  },
  {
    group: "AI ACCESS",
    items: [
      { path: "/analyst", icon: "spark", label: "AI 分析师" },
      {
        path: "/mcp",
        icon: "plug",
        label: "MCP",
        // 🔴 如实标注状态，不假装已连接。
        // 原型这里是个 `badge-dot`「已连接」，但 MCP 从未实现——
        // 显示"已连接"会让用户以为能用，点进去发现是空的，
        // 这种落差比直接说"阶段三"更损害信任。
        pending: "MCP 接入属于阶段三范围。当前版本围绕 LLM 直接实现分析能力，不经过 MCP。",
      },
    ],
  },
  {
    group: null,
    items: [
      { path: "/settings", icon: "gear", label: "设置" },
      { path: "/jobs", icon: "cpu", label: "任务" },
    ],
  },
];

/** 需要认证的端点前缀（当前单机版无鉴权，保留常量以便将来接入）。 */
export const API_PREFIX = "/api";

/** 每页默认条数（与后端 `default_page_size` 一致，避免两边分页口径不同）。 */
export const DEFAULT_PAGE_SIZE = 20;

/** 搜索输入的防抖毫秒数。 */
export const SEARCH_DEBOUNCE_MS = 260;
