import { describe, expect, it } from "vitest";

import { isHardReload } from "./desktop";

function key(
  name: string,
  modifiers: { ctrl?: boolean; meta?: boolean; shift?: boolean } = {},
) {
  return {
    key: name,
    ctrlKey: modifiers.ctrl ?? false,
    metaKey: modifiers.meta ?? false,
    shiftKey: modifiers.shift ?? false,
  };
}

describe("isHardReload", () => {
  it("拦 F5", () => {
    expect(isHardReload(key("F5"))).toBe(true);
  });

  it("拦 Ctrl+Shift+R 与 Cmd+Shift+R", () => {
    expect(isHardReload(key("R", { ctrl: true, shift: true }))).toBe(true);
    expect(isHardReload(key("r", { meta: true, shift: true }))).toBe(true);
  });

  it("绝不拦 Ctrl+R —— 它在 shell 里是反向历史搜索", () => {
    expect(isHardReload(key("r", { ctrl: true }))).toBe(false);
    expect(isHardReload(key("R", { ctrl: true }))).toBe(false);
  });

  it("其他组合键一律放行", () => {
    expect(isHardReload(key("t", { ctrl: true, shift: true }))).toBe(false);
    expect(isHardReload(key("r"))).toBe(false);
    expect(isHardReload(key("Tab", { ctrl: true }))).toBe(false);
  });
});
