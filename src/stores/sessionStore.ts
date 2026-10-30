import { create } from "zustand";

/**
 * 会话 / 终端状态（需求 4.6：与 UI 状态分开存）。
 *
 * 终端高频输出只会动这里，不会牵连侧边栏与路由。
 *
 * `tabId` 由前端生成、`sessionId` 由 Rust 生成，两者分开是为了让 React 的 key 保持稳定：
 * 会话建立前后还是同一个 TerminalView 实例，不会因为 id 变化而销毁重建终端。
 */

export type SessionStatus = "connecting" | "ready" | "disconnected" | "failed";

export interface TerminalTab {
  tabId: string;
  connectionId: string;
  title: string;
  hostLabel: string;
  /** 会话建立前为 null */
  sessionId: string | null;
  status: SessionStatus;
  error?: string;
}

interface SessionState {
  tabs: TerminalTab[];
  activeTabId: string | null;
  openTab: (input: { connectionId: string; title: string; hostLabel: string }) => string;
  closeTab: (tabId: string) => void;
  setActiveTab: (tabId: string | null) => void;
  markOpened: (tabId: string, sessionId: string) => void;
  markStatus: (tabId: string, status: SessionStatus, error?: string) => void;
  /** 只改显示标题；`hostLabel` 保持不变，否则用户会以为连到了别的主机 */
  renameTab: (tabId: string, title: string) => void;
  markBySession: (sessionId: string, status: SessionStatus, error?: string) => void;
}

let tabSeq = 0;

export const useSessionStore = create<SessionState>((set) => ({
  tabs: [],
  activeTabId: null,

  openTab: ({ connectionId, title, hostLabel }) => {
    tabSeq += 1;
    const tabId = `t${tabSeq}`;
    set((state) => ({
      tabs: [
        ...state.tabs,
        { tabId, connectionId, title, hostLabel, sessionId: null, status: "connecting" },
      ],
      activeTabId: tabId,
    }));
    return tabId;
  },

  closeTab: (tabId) =>
    set((state) => {
      const tabs = state.tabs.filter((tab) => tab.tabId !== tabId);
      const activeTabId =
        state.activeTabId === tabId ? (tabs[tabs.length - 1]?.tabId ?? null) : state.activeTabId;
      return { tabs, activeTabId };
    }),

  setActiveTab: (activeTabId) => set({ activeTabId }),

  markOpened: (tabId, sessionId) =>
    set((state) => ({
      tabs: state.tabs.map((tab) =>
        tab.tabId === tabId ? { ...tab, sessionId, status: "ready", error: undefined } : tab,
      ),
    })),

  renameTab: (tabId, title) => {
    const trimmed = title.trim();
    // 空标题在这里也挡一道：别处调用忘了校验时不至于把标签搞成空白
    if (!trimmed) return;
    set((state) => ({
      tabs: state.tabs.map((tab) => (tab.tabId === tabId ? { ...tab, title: trimmed } : tab)),
    }));
  },

  markStatus: (tabId, status, error) =>
    set((state) => ({
      tabs: state.tabs.map((tab) => (tab.tabId === tabId ? { ...tab, status, error } : tab)),
    })),

  markBySession: (sessionId, status, error) =>
    set((state) => ({
      tabs: state.tabs.map((tab) =>
        tab.sessionId === sessionId ? { ...tab, status, error } : tab,
      ),
    })),
}));
