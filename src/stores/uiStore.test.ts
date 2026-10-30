import { beforeEach, describe, expect, it } from "vitest";

import { useUiStore } from "./uiStore";

describe("connectionsRevision", () => {
  beforeEach(() => {
    // 每次归零，避免用例之间互相影响
    useUiStore.setState({ connectionsRevision: 0 });
  });

  it("每次落库都递增，终端页才有机会被唤醒", () => {
    const { bumpConnections } = useUiStore.getState();

    expect(useUiStore.getState().connectionsRevision).toBe(0);
    bumpConnections();
    expect(useUiStore.getState().connectionsRevision).toBe(1);
    bumpConnections();
    expect(useUiStore.getState().connectionsRevision).toBe(2);
  });

  it("切页不改变计数，只有数据变更才改变", () => {
    const { setActivePage, bumpConnections } = useUiStore.getState();

    bumpConnections();
    const before = useUiStore.getState().connectionsRevision;

    setActivePage("terminal");
    setActivePage("connections");

    expect(useUiStore.getState().connectionsRevision).toBe(before);
  });
});
