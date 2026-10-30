import { describe, expect, it } from "vitest";

import { nextTabId } from "./tabs";

const TABS = ["t1", "t2", "t3"];

describe("nextTabId", () => {
  it("没有标签时返回 null", () => {
    expect(nextTabId([], null, 1)).toBeNull();
  });

  it("向后切换并在末尾回到开头", () => {
    expect(nextTabId(TABS, "t1", 1)).toBe("t2");
    expect(nextTabId(TABS, "t3", 1)).toBe("t1");
  });

  it("向前切换并在开头回到末尾", () => {
    expect(nextTabId(TABS, "t2", -1)).toBe("t1");
    expect(nextTabId(TABS, "t1", -1)).toBe("t3");
  });

  it("没有活动标签时给出合理落点", () => {
    expect(nextTabId(TABS, null, 1)).toBe("t1");
    expect(nextTabId(TABS, null, -1)).toBe("t3");
  });

  it("活动标签已失效时不会抛错", () => {
    expect(nextTabId(TABS, "gone", 1)).toBe("t1");
    expect(nextTabId(TABS, "gone", -1)).toBe("t3");
  });

  it("只有一个标签时原地循环", () => {
    expect(nextTabId(["only"], "only", 1)).toBe("only");
    expect(nextTabId(["only"], "only", -1)).toBe("only");
  });
});
