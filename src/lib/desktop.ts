/**
 * 桌面应用不该保留的网页行为。
 *
 * 起因：文件页右键弹出的是 WebView 的默认菜单，里面有「重新加载」。
 * 点一下整页重载，前端状态全丢，用户被甩回初始页 —— 看起来像"点刷新跳到了自检页"。
 *
 * 这里只拦两件有明确答案的事：
 * 1. 默认右键菜单（我们自己画）；
 * 2. F5 / Ctrl+Shift+R 这类强制刷新。
 *
 * **故意不拦 Ctrl+R**：它在 shell 里是反向历史搜索，拦了会天天咬到终端用户。
 * 代价是用户仍可在非终端页按 Ctrl+R 触发重载，这个由 `uiStore` 的页面持久化兜底。
 */

interface KeyLike {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
}

/**
 * 是否属于「强制刷新」。
 *
 * 单独抽出来是为了能测：这条判断里最容易被后人"顺手优化"掉的，
 * 就是 `Ctrl+R` 必须返回 false 这一点。
 */
export function isHardReload(event: KeyLike): boolean {
  if (event.key === "F5") return true;

  const withModifier = event.ctrlKey || event.metaKey;
  return withModifier && event.shiftKey && event.key.toLowerCase() === "r";
}

export function installDesktopBehavior(): void {
  // 捕获阶段拦：任何组件内的预防都排在它之后，避免各写一遍
  document.addEventListener(
    "contextmenu",
    (event) => {
      event.preventDefault();
    },
    { capture: true },
  );

  document.addEventListener(
    "keydown",
    (event) => {
      if (!event.key) return;

      if (isHardReload(event)) {
        event.preventDefault();
        event.stopPropagation();
      }
    },
    { capture: true },
  );
}
