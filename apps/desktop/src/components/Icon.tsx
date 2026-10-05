/**
 * 图标集。
 *
 * 🔴 **直接移植自 `prototype/js/app.js` 的 ICONS**，不是重画：
 * 那套图标与设计图、CSS 的尺寸/描边约定是对齐的，重画会引入视觉不一致。
 *
 * 与原型唯一的区别是这里用 React 组件而非 HTML 字符串拼接——
 * 字符串拼接需要调用方手动转义，而 React 天然不会把数据当 HTML 解析。
 */

import type { SVGProps } from "react";

/** SVG path 数据（`fill="none" stroke="currentColor"`）。 */
const P = (d: string) => (
  <path
    d={d}
    fill="none"
    stroke="currentColor"
    strokeWidth={1.7}
    strokeLinecap="round"
    strokeLinejoin="round"
  />
);

/** 描边圆形（部分图标需要）。 */
const C = (cx: number, cy: number, r: number) => (
  <circle cx={cx} cy={cy} r={r} fill="none" stroke="currentColor" strokeWidth={1.7} />
);

export const ICON_PATHS = {
  home: <>{P("M3 10.5 12 3l9 7.5M5 9.5V21h14V9.5")}</>,
  folder: <>{P("M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z")}</>,
  box: <>{P("M12 3 3 7.5v9L12 21l9-4.5v-9zM3 7.5 12 12l9-4.5M12 12v9")}</>,
  book: <>{P("M4 5a2 2 0 0 1 2-2h13v18H6a2 2 0 0 0-2 2zM8 3v18")}</>,
  graph: (
    <>
      {C(6, 6, 2.4)}
      {C(18, 7, 2.4)}
      {C(12, 18, 2.4)}
      {P("M8 7l7.6.6M7 8l3.6 7.6M16.6 9l-3.4 6.8")}
    </>
  ),
  drop: <>{P("M12 3s6 6.6 6 11a6 6 0 0 1-12 0c0-4.4 6-11 6-11z")}</>,
  spark: <>{P("M12 3v4M12 17v4M3 12h4M17 12h4M6 6l2.5 2.5M15.5 15.5 18 18M18 6l-2.5 2.5M8.5 15.5 6 18")}</>,
  plug: <>{P("M9 3v6M15 3v6M6 9h12v3a6 6 0 0 1-12 0zM12 18v3")}</>,
  skill: <>{P("M8 8a4 4 0 1 1 8 0M8 16a4 4 0 1 0 8 0M4 12h16")}</>,
  gear: (
    <>
      {C(12, 12, 3)}
      {P("M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9 17 7M7 17l-2.1 2.1")}
    </>
  ),
  help: (
    <>
      {C(12, 12, 9)}
      {P("M9.5 9.3a2.6 2.6 0 1 1 3.6 2.4c-.8.4-1.1 1-1.1 1.8M12 17h.01")}
    </>
  ),
  search: (
    <>
      {C(11, 11, 6.5)}
      {P("M16 16l5 5")}
    </>
  ),
  bell: <>{P("M6 9a6 6 0 1 1 12 0c0 5 2 6 2 6H4s2-1 2-6M10 19a2 2 0 0 0 4 0")}</>,
  cloud: <>{P("M7 18a4 4 0 0 1-.6-7.96A6 6 0 0 1 18 8.7 4.5 4.5 0 0 1 17.5 18z")}</>,
  repeat: <>{P("M4 9a5 5 0 0 1 5-5h9m0 0-3-3m3 3-3 3M20 15a5 5 0 0 1-5 5H6m0 0 3 3m-3-3 3-3")}</>,
  bulb: <>{P("M9 18h6M10 21h4M12 3a6 6 0 0 1 4 10.5c-.8.7-1 1.5-1 2.5h-6c0-1-.2-1.8-1-2.5A6 6 0 0 1 12 3z")}</>,
  clock: (
    <>
      {C(12, 12, 9)}
      {P("M12 7v5l3.5 2")}
    </>
  ),
  doc: <>{P("M6 2h9l4 4v16H6zM14 2v5h5M9 12h7M9 16h7")}</>,
  check: <>{P("M4 12.5 9.5 18 20 6.5")}</>,
  link: <>{P("M9 15 15 9M8 12l-2.5 2.5a3.5 3.5 0 0 0 5 5L13 17M11 7l2.5-2.5a3.5 3.5 0 0 1 5 5L16 12")}</>,
  grid: <>{P("M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z")}</>,
  shield: <>{P("M12 3 5 6v6c0 4.5 3 7.5 7 9 4-1.5 7-4.5 7-9V6z")}</>,
  tree: <>{P("M12 3v18M12 8h6M12 13H6M12 18h6")}</>,
  video: <>{P("M3 7h12v10H3zM15 10l6-3v10l-6-3")}</>,
  user: (
    <>
      {C(12, 8, 3.5)}
      {P("M5 20a7 7 0 0 1 14 0")}
    </>
  ),
  flow: <>{P("M5 5h5v5H5zM14 14h5v5h-5zM10 7.5h6.5V14")}</>,
  star: <>{P("M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.8-5.2-2.8-5.2 2.8 1-5.8L3.5 9.7l5.9-.8z")}</>,
  x: <>{P("M6 6l12 12M18 6 6 18")}</>,
  plus: <>{P("M12 5v14M5 12h14")}</>,
  arr: <>{P("M5 12h14m0 0-5-5m5 5-5 5")}</>,
  chev: <>{P("M9 6l6 6-6 6")}</>,
  send: <>{P("M4 12 20 4l-6 16-3-6z")}</>,
  code: <>{P("M8 7 3 12l5 5M16 7l5 5-5 5")}</>,
  refresh: <>{P("M20 11a8 8 0 1 0-2.3 6.3M20 5v6h-6")}</>,
  alert: <>{P("M12 3 2 20h20zM12 9v5M12 17h.01")}</>,
  menu: <>{P("M4 7h16M4 12h16M4 17h16")}</>,
  edit: <>{P("M4 20h4L19 9l-4-4L4 16zM14 6l4 4")}</>,
  sun: (
    <>
      {C(12, 12, 4)}
      {P("M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M19.1 4.9l-1.4 1.4M6.3 17.7l-1.4 1.4")}
    </>
  ),
  moon: <>{P("M20 14.5A8.5 8.5 0 0 1 9.5 4a8.5 8.5 0 1 0 10.5 10.5z")}</>,
  scan: <>{P("M4 8V5.5A1.5 1.5 0 0 1 5.5 4H8M16 4h2.5A1.5 1.5 0 0 1 20 5.5V8M20 16v2.5a1.5 1.5 0 0 1-1.5 1.5H16M8 20H5.5A1.5 1.5 0 0 1 4 18.5V16M4 12h16")}</>,
  key: (
    <>
      {C(8, 14, 4)}
      {P("M11 11 20 3M16 6l3 3")}
    </>
  ),
  palette: <>{P("M12 3a9 9 0 1 0 0 18c1.5 0 2-1 2-2s-.7-1.8-2-1.8h-1A2 2 0 0 1 9 15a9 9 0 0 0 3-12zM7.5 10.5h.01M11 7.5h.01M15.5 9h.01")}</>,
  db: <>{P("M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3zM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3")}</>,
  cpu: <>{P("M7 7h10v10H7zM9.5 4v3M14.5 4v3M9.5 17v3M14.5 17v3M4 9.5h3M4 14.5h3M17 9.5h3M17 14.5h3")}</>,
  trash: <>{P("M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13M10 11v6M14 11v6")}</>,
  eye: (
    <>
      {P("M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z")}
      {C(12, 12, 3)}
    </>
  ),
  eyeoff: <>{P("M4 4l16 16M9.9 5.9A9.6 9.6 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a16 16 0 0 1-3.3 4M6.5 8A15.6 15.6 0 0 0 2.5 12S6 18.5 12 18.5c1.3 0 2.4-.3 3.5-.7M9.9 9.9a3 3 0 0 0 4.2 4.2")}</>,
} as const;

/** 图标名（受限于实际存在的键，拼错会编译失败）。 */
export type IconName = keyof typeof ICON_PATHS;

export interface IconProps extends Omit<SVGProps<SVGSVGElement>, "name"> {
  name: IconName;
  /** 额外 class（原型约定 `.ico` 控制尺寸，这里保留兼容） */
  className?: string;
}

/**
 * 图标组件。
 *
 * `aria-hidden` 默认开启：图标是装饰，屏幕阅读器应读旁边的文字。
 * 需要独立语义时传 `aria-label` + `role="img"`。
 */
export function Icon({ name, className, ...rest }: IconProps) {
  return (
    <svg
      className={["ico", className].filter(Boolean).join(" ")}
      viewBox="0 0 24 24"
      aria-hidden={rest["aria-label"] ? undefined : true}
      {...rest}
    >
      {ICON_PATHS[name]}
    </svg>
  );
}
