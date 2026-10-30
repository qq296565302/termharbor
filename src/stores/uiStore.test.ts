import { describe, expect, it } from "vitest";

import { DEFAULT_PAGE, resolveSavedPage } from "./uiStore";

describe("resolveSavedPage", () => {
  it("老存档里的 files 落到终端页", () => {
    // 文件页已并进会话页。不清这一条的话，升级后用户会被甩到一张不存在的空白页。
    expect(resolveSavedPage("files")).toBe("terminal");
  });

  it("正常页面原样返回", () => {
    expect(resolveSavedPage("connections")).toBe("connections");
    expect(resolveSavedPage("terminal")).toBe("terminal");
  });

  it("无法识别的页面名返回 null，由调用方给默认值", () => {
    expect(resolveSavedPage("nope")).toBeNull();
    expect(resolveSavedPage("")).toBeNull();
    expect(resolveSavedPage(null)).toBeNull();
  });
});

describe("DEFAULT_PAGE", () => {
  it("落在连接页，不是自检页", () => {
    // 自检页现在是开发构建才有的排障页，不能再当应用首页
    expect(DEFAULT_PAGE).toBe("connections");
  });
});
