/**
 * 应用入口。
 *
 * 🔴 样式导入顺序即优先级来源：tokens → base → app。
 * app.css 只放 base.css 里没有的类（见该文件头部说明），
 * 顺序颠倒会让补充样式被设计系统覆盖。
 */
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";

import "@/styles/tokens.css";
import "@/styles/base.css";
import "@/styles/app.css";

import { App } from "./App";
import { AppProvider } from "@/lib/AppContext";
import { ToastProvider } from "@/components/Toast";

const container = document.getElementById("root");
if (container === null) {
  // 🔴 直接抛错而不是静默退出：挂载点丢失通常意味着 index.html 被改坏，
  // 静默失败会表现为"白屏且控制台无任何提示"。
  throw new Error("找不到 #root 挂载点，请检查 index.html");
}

createRoot(container).render(
  <StrictMode>
    {/* 🔴 Provider 嵌套顺序有意义：
        AppProvider 在最外层（服务状态是一切的前提），
        Toast 在其内（toast 可能因服务状态变化而触发），
        Router 最内（页面组件同时需要上面两者）。 */}
    <AppProvider>
      <ToastProvider>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </ToastProvider>
    </AppProvider>
  </StrictMode>,
);
