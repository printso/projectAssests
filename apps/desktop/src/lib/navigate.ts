/**
 * 后端 `link.page` → 前端路由的唯一映射。
 *
 * # 🔴 为什么必须有这个单一映射
 * 后端在多处给出"点击后去哪"的页面 key：
 * - 搜索命中的 `hit.link.page`
 * - 图谱节点的 `node.link_page`
 * - 首页统计卡的 `stat.link_page`
 * - 分析师引用的 `citation.link.page`
 *
 * 这些 key 的取值集合是**后端的契约**（`overview` / `project` / `projects` /
 * `assets` / `graph` / `insights` / `analyst`）。早期 Search 与 Graph 各自写了一份
 * 映射，且都只认 `projects`（复数）——而后端项目**详情**用的是 `project`（单数）。
 * 于是点击任何项目命中都会跳到 `/project?id=...`，不匹配任何路由，
 * 被 `*` 兜底弹回首页：用户点了一下，页面"跳走了又回来"，完全无法理解。
 *
 * 收敛到这一个函数后，后端加新页面 key 时只需改这里一处，
 * 且未知 key 有显式回退（跳列表页而非静默弹回首页）。
 */

/**
 * 把后端的页面 key 与定位参数翻译成前端路由路径。
 *
 * @param page  后端给的页面 key
 * @param param 页面内定位参数（项目 id / 资产 id / 查询词），可为 null
 */
export function routeForLink(page: string, param: string | null): string {
  const p = param === null ? null : encodeURIComponent(param);
  switch (page) {
    case "overview":
      return "/";
    // 🔴 单数 = 详情页（后端 link_page_for 对 Project 实体返回 "project"）
    case "project":
      return p !== null ? `/projects/${p}` : "/projects";
    // 复数 = 列表页；带 param 时定位到该项详情
    case "projects":
      return p !== null ? `/projects/${p}` : "/projects";
    case "assets":
      return p !== null ? `/assets?id=${p}` : "/assets";
    case "graph":
      return p !== null ? `/graph?id=${p}` : "/graph";
    case "insights":
      return p !== null ? `/insights?id=${p}` : "/insights";
    case "opportunities":
      return p !== null ? `/opportunities?id=${p}` : "/opportunities";
    case "analyst":
      return p !== null ? `/analyst?q=${p}` : "/analyst";
    case "jobs":
      return "/jobs";
    case "settings":
      return "/settings";
    default:
      // 🔴 未知 key 回退到同名路径而非首页：
      // 弹回首页会让用户以为点击无效；跳到 `/unknown` 至少能在 URL 上看出问题。
      return `/${page}`;
  }
}
