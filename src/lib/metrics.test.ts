import { describe, expect, it } from "vitest";

import {
  clampPercent,
  formatKb,
  formatRate,
  formatUptime,
  levelOf,
  loadAdvice,
} from "./metrics";

describe("clampPercent", () => {
  it("把越界值收回 0-100", () => {
    expect(clampPercent(-5)).toBe(0);
    expect(clampPercent(120)).toBe(100);
    expect(clampPercent(42.5)).toBe(42.5);
  });

  it("NaN 与 Infinity 都当 0", () => {
    // 计数器回绕时后端会给出异常值，前端不能显示 NaN%
    expect(clampPercent(Number.NaN)).toBe(0);
    expect(clampPercent(Number.POSITIVE_INFINITY)).toBe(0);
  });
});

describe("levelOf", () => {
  it("70 提醒、90 危险，与诊断面板口径一致", () => {
    expect(levelOf(0)).toBe("is-ok");
    expect(levelOf(69.9)).toBe("is-ok");
    expect(levelOf(70)).toBe("is-warn");
    expect(levelOf(89.9)).toBe("is-warn");
    expect(levelOf(90)).toBe("is-danger");
    expect(levelOf(100)).toBe("is-danger");
  });
});

describe("formatKb", () => {
  it("按 1024 进位", () => {
    // 512 已经过百，按「上百不保留小数」这一档显示
    expect(formatKb(512)).toBe("512 KB");
    expect(formatKb(2048)).toBe("2.0 MB");
    expect(formatKb(1024 * 1024)).toBe("1.0 GB");
  });

  it("上百之后不保留小数", () => {
    expect(formatKb(200 * 1024)).toBe("200 MB");
  });

  it("0 与非法值给 0，不出现 NaN", () => {
    expect(formatKb(0)).toBe("0");
    expect(formatKb(-1)).toBe("0");
    expect(formatKb(Number.NaN)).toBe("0");
  });
});

describe("formatRate", () => {
  it("没有流量时显示 0 B/s", () => {
    // 网卡计数器回绕会把速率算成负数，不能显示 "-1.2 MB/s"
    expect(formatRate(0)).toBe("0 B/s");
    expect(formatRate(-100)).toBe("0 B/s");
  });

  it("按 1024 进位", () => {
    expect(formatRate(2048)).toBe("2.0 KB/s");
    expect(formatRate(3 * 1024 * 1024)).toBe("3.0 MB/s");
  });
});

describe("formatUptime", () => {
  it("按天/小时/分钟分段", () => {
    expect(formatUptime(90)).toBe("1 分钟");
    expect(formatUptime(3600 * 5 + 60)).toBe("5 小时 1 分");
    expect(formatUptime(86400 * 3 + 3600 * 4)).toBe("3 天 4 小时");
  });

  it("读不到时给占位符", () => {
    expect(formatUptime(0)).toBe("—");
    expect(formatUptime(Number.NaN)).toBe("—");
  });
});

describe("loadAdvice", () => {
  it("按核心数解读，不按绝对值", () => {
    // 同样 load 4：4 核是满载，32 核几乎空闲
    expect(loadAdvice(4, 4)).toContain("排队");
    expect(loadAdvice(4, 32)).toContain("空闲");
    expect(loadAdvice(3, 4)).toContain("接近");
  });

  it("核心数未知时不乱下结论", () => {
    expect(loadAdvice(2, 0)).toBe("");
  });
});
