import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 开发时固定 1420 端口，与 tauri.conf.json 的 devUrl 保持一致。
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // src-tauri 由 cargo 自己 watch，避免 vite 重复触发
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    // WebView2 / WKWebView / WebKitGTK 的最低共同基线
    target: "chrome105",
    sourcemap: false,
    minify: "esbuild",
  },
});
