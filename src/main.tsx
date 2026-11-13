import React from "react";
import ReactDOM from "react-dom/client";

import App from "./App";
import { installDesktopBehavior } from "./lib/desktop";
import { applyCachedTheme } from "./lib/theme";
import "./styles.css";

// 在挂载之前先把界面外观定下来：用上次的缓存值兜住首帧，设置读回来之后会再校正。
// 顺序在 React 之前、在 installDesktopBehavior 之前都无所谓，但必须早于首次绘制。
applyCachedTheme();

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
