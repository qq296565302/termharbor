import { create } from "zustand";

export type PageId =
  | "selfcheck"
  | "connections"
  | "terminal"
  | "settings"
  | "about"
  | "ime";

/** 生产构建里没有输入法测试页（S3 的取证页只在 dev 出现）。 */
const BASE_PAGES = ["connections", "terminal", "settings", "about"] as const;
const PAGE_KEY = "termharbor.activePage";

function allowedPages(): readonly string[] {
  // 自检页与输入法测试页只在开发构建里有入口（见 App.tsx 的说明）
  return import.meta.env.DEV ? [...BASE_PAGES, "selfcheck", "ime"] : [...BASE_PAGES];
}

/**
 * 记住当前页面。
 *
 * 起因：用户点到了 WebView 右键菜单里的「重新加载」，整页重载后 `activePage` 回到初始值，
 * 人就被甩回自检页。默认右键菜单已经禁掉（见 `lib/desktop.ts`），
 * 但 Ctrl+R 还得放行给 shell 做历史搜索，所以这层兜底不能省。
 *
 * 用 sessionStorage 而不是 localStorage：重开应用回到初始页是合理的，
 * 而同一次运行里的意外重载应该保住位置。
 */
/**
 * 存档里的页面名 → 合法页面名。不合法返回 null（由调用方给默认值）。
 *
 * 单独抽出来是为了能测：`loadPage` 在模块加载时就跑，测试没法替换 sessionStorage。
 */
export function resolveSavedPage(saved: string | null | undefined): PageId | null {
  if (!saved) return null;
  // 文件页已经并进会话页（终端右侧的文件面板）。老标签页里存着 "files"，
  // 直接落到终端页，别让用户看到一张空白页。
  if (saved === "files") return "terminal";
  return allowedPages().includes(saved) ? (saved as PageId) : null;
}

/**
 * 没有可用存档时的落点：**连接页**。
 *
 * 它是一台台服务器的家目录，也是新用户唯一该先看的地方；
 * 自检页已经降级成开发构建才有的排障工具（见 App.tsx），不能当首页。
 */
export const DEFAULT_PAGE: PageId = "connections";

function loadPage(): PageId {
  try {
    const resolved = resolveSavedPage(globalThis.sessionStorage?.getItem(PAGE_KEY));
    if (resolved) return resolved;
  } catch {
    // 无 sessionStorage（隐私模式、测试环境）时静默降级
  }
  return DEFAULT_PAGE;
}

function savePage(page: PageId): void {
  try {
    globalThis.sessionStorage?.setItem(PAGE_KEY, page);
  } catch {
    // 存不进去不影响功能
  }
}

interface UiState {
  activePage: PageId;
  setActivePage: (page: PageId) => void;
  /**
   * 连接配置的变更计数。
   *
   * 终端页是常驻挂载的（切页只切显隐，否则会销毁所有会话），
   * 所以它不能靠 `useEffect(..., [])` 拿一次列表就算完 ——
   * 之后在「连接」页新建/修改/删除的连接它根本不知道。
   * 这里放一个纯信号，连接页每次落库后 +1，终端页订阅它重新拉取。
   *
   * 只存计数不存列表，是因为列表的唯一真相在 SQLite，前端不能自己维护一份。
   */
  connectionsRevision: number;
  bumpConnections: () => void;
}

/** 纯 UI 状态。与业务会话状态分开，避免一次终端输出触发整页重渲染（需求 4.6）。 */
export const useUiStore = create<UiState>((set) => ({
  activePage: loadPage(),
  setActivePage: (activePage) => {
    savePage(activePage);
    set({ activePage });
  },
  connectionsRevision: 0,
  bumpConnections: () => set((state) => ({ connectionsRevision: state.connectionsRevision + 1 })),
}));
