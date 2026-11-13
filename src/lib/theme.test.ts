import { describe, expect, it } from "vitest";

import { resolveTheme, systemTheme } from "./theme";

describe("resolveTheme", () => {
  it("选了固定值就原样返回，不再看系统", () => {
    expect(resolveTheme("dark")).toBe("dark");
    expect(resolveTheme("light")).toBe("light");
  });

  /** 「跟随系统」解析出的必须和系统一致 —— 这是这个函数存在的唯一理由。 */
  it("system 解析成当前系统值", () => {
    expect(resolveTheme("system")).toBe(systemTheme());
    expect(["light", "dark"]).toContain(resolveTheme("system"));
  });

  /** 没有 matchMedia 的环境（服务端渲染、老 WebView）不能抛错，退回浅色。 */
  it("拿不到 matchMedia 时退回浅色", () => {
    expect(systemTheme()).toBe("light");
  });
});
