import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";

// 🔴 后端地址只在这一处定义（单一真相源）。
// 用 `SPOLIA_API` 环境变量覆盖，便于连不同端口/远程实例调试。
const API_TARGET = process.env.SPOLIA_API ?? "http://127.0.0.1:8787";

// 🔴 默认避开 5173：本机另有无关项目的 dev server 长期占用它（Vibe Hardware）。
// 用 `SPOLIA_PORT` 覆盖。非法值直接抛错——与 strictPort 同一哲学：
// 宁可启动失败，也不要静默漂移到别的端口（那会让人打开并"验证"到别人家的页面）。
const DEV_PORT = Number(process.env.SPOLIA_PORT ?? 5174);
if (!Number.isInteger(DEV_PORT) || DEV_PORT < 1 || DEV_PORT > 65535) {
  throw new Error(`SPOLIA_PORT 不是合法端口号：${String(process.env.SPOLIA_PORT)}`);
}

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    // 只监听回环：与后端同样的安全约束（代码内容不得暴露到局域网）
    host: "127.0.0.1",
    port: DEV_PORT,
    strictPort: true,
    // 🔴 开发期用代理而非直连后端：
    // 1. 免 CORS 预检（后端不必为浏览器放宽同源策略）
    // 2. 前端代码里只写相对路径 `/api/...`，生产环境由 Tauri 壳或同源部署接管，
    //    无需按环境切换 baseURL——环境差异留在配置层，不渗进业务代码
    proxy: {
      "/api": {
        target: API_TARGET,
        changeOrigin: true,
        // SSE 需要关闭缓冲，否则进度事件会被代理攒批后一次性吐出
        configure: (proxy) => {
          proxy.on("proxyRes", (proxyRes) => {
            if (proxyRes.headers["content-type"]?.includes("text/event-stream")) {
              proxyRes.headers["cache-control"] = "no-cache, no-transform";
            }
          });
        },
      },
    },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
    // 图表/图谱渲染较重，单独分包避免首屏被拖慢
    rollupOptions: {
      output: {
        manualChunks: {
          react: ["react", "react-dom", "react-router-dom"],
        },
      },
    },
  },
});
