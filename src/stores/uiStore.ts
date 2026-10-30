import { create } from "zustand";

export type PageId =
  | "selfcheck"
  | "connections"
  | "terminal"
  | "files"
  | "settings"
  | "about"
  | "ime";

/** 生产构建里没有输入法测试页（S3 的取证页只在 dev 出现）。 */
const BASE_PAGES = ["selfcheck", "connections", "terminal", "files", "settings", "about"] as const;
const PAGE_KEY = "termharbor.activePage";

function allowedPages(): readonly string[] {
  return import.meta.env.DEV ? [...BASE_PAGES, "ime"] : [...BASE_PAGES];
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
function loadPage(): PageId {
  try {
    const saved = globalThis.sessionStorage?.getItem(PAGE_KEY);
    if (saved && allowedPages().includes(saved)) {
      return saved as PageId;
    }
  } catch {
    // 无 sessionStorage（隐私模式、测试环境）时静默降级
  }
  return "selfcheck";
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
