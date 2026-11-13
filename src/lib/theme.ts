/**
 * 界面外观的落地方式。
 *
 * 之前界面只跟系统走：`styles.css` 里一个 `@media (prefers-color-scheme: dark)`。
 * 要支持手动切换，最省事的写法是把那几十个暗色令牌再写一遍到
 * `:root[data-theme="dark"]` 下 —— 但两份令牌必然漂移（这个项目在「令牌只有一份」
 * 上已经吃过亏）。
 *
 * 所以改成：**JS 算出最终结果，永远给 `<html>` 挂 `data-theme="light|dark"`**，
 * CSS 里只保留一个 `[data-theme="dark"]` 块。「跟随系统」在 JS 里用 matchMedia 解析。
 *
 * 代价是首帧：JS 没跑之前 `data-theme` 不存在，会闪一下浅色。用两条路堵：
 * index.html 里一小段内联脚本（在样式表解析前）读 localStorage 先挂上，
 * 设置从库里读回来之后再校正一次。
 */

import type { ResolvedTheme, UiTheme } from "./types";

export const THEME_ATTR = "data-theme";
/** localStorage 只用来防首帧闪烁，真正的事实来源是数据库里的设置 */
export const THEME_CACHE_KEY = "termharbor.uiTheme";

/** 系统当前是深还是浅。拿不到 matchMedia（极端环境）就按浅色。 */
export function systemTheme(): ResolvedTheme {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return "light";
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

/** `system` 解析成具体值，其余原样返回 */
export function resolveTheme(uiTheme: UiTheme): ResolvedTheme {
  return uiTheme === "system" ? systemTheme() : uiTheme;
}

/** 立刻生效：挂属性 + 写缓存。返回解析后的结果，调用方一般不关心。 */
export function applyTheme(uiTheme: UiTheme): ResolvedTheme {
  const resolved = resolveTheme(uiTheme);
  if (typeof document !== "undefined") {
    document.documentElement.setAttribute(THEME_ATTR, resolved);
  }
  try {
    window.localStorage.setItem(THEME_CACHE_KEY, uiTheme);
  } catch {
    // 隐私模式 / 存储被禁用：只是首帧会闪，不影响功能
  }
  return resolved;
}

/**
 * 启动时用缓存值先挂一次。
 *
 * 只是**猜测**：设置从库里读回来之后会再校正。所以这里读失败、读到脏值都不要紧，
 * 退回 `system` 即可 —— 不能因为一个缓存把界面锁在错误的状态上。
 */
export function applyCachedTheme(): void {
  let cached: UiTheme = "system";
  try {
    const raw = window.localStorage.getItem(THEME_CACHE_KEY);
    if (raw === "system" || raw === "light" || raw === "dark") {
      cached = raw;
    }
  } catch {
    // 忽略：用 default
  }
  applyTheme(cached);
}

/**
 * 监听系统明暗变化。只在「跟随系统」时有意义，调用方自己判断。
 * 返回取消函数。
 */
export function watchSystemTheme(onChange: (theme: ResolvedTheme) => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => undefined;
  }
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  const handler = () => onChange(query.matches ? "dark" : "light");
  query.addEventListener("change", handler);
  return () => query.removeEventListener("change", handler);
}
