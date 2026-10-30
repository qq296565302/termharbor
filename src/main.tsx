import React from "react";
import ReactDOM from "react-dom/client";

import App from "./App";
import { installDesktopBehavior } from "./lib/desktop";
import "./styles.css";

// 必须在 React 挂载前装：右键与强制刷新的拦截要在捕获阶段生效
installDesktopBehavior();

const root = document.getElementById("root");
if (!root) {
  throw new Error("找不到 #root 挂载点");
}

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
