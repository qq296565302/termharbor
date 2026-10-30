import { beforeEach, describe, expect, it } from "vitest";

import { useSessionStore } from "./sessionStore";

function openOne(title = "阿里云", hostLabel = "root@1.2.3.4:22") {
  return useSessionStore.getState().openTab({
    connectionId: "c1",
    title,
    hostLabel,
  });
}

describe("renameTab", () => {
  beforeEach(() => {
    useSessionStore.setState({ tabs: [], activeTabId: null });
  });

  it("只改显示标题，主机标签保持不变", () => {
    const id = openOne();

    useSessionStore.getState().renameTab(id, "生产机");

    const tab = useSessionStore.getState().tabs[0];
    expect(tab.title).toBe("生产机");
    expect(tab.hostLabel).toBe("root@1.2.3.4:22");
  });

  it("空标题被忽略，不会把标签搞成空白", () => {
    const id = openOne();

    useSessionStore.getState().renameTab(id, "   ");

    expect(useSessionStore.getState().tabs[0].title).toBe("阿里云");
  });

  it("去掉首尾空格", () => {
    const id = openOne();

    useSessionStore.getState().renameTab(id, "  生产机  ");

    expect(useSessionStore.getState().tabs[0].title).toBe("生产机");
  });

  it("不影响其它标签", () => {
    const first = openOne("阿里云");
    const second = openOne("腾讯云");

    useSessionStore.getState().renameTab(first, "生产机");

    const tabs = useSessionStore.getState().tabs;
    expect(tabs.find((tab) => tab.tabId === first)?.title).toBe("生产机");
    expect(tabs.find((tab) => tab.tabId === second)?.title).toBe("腾讯云");
  });
});
