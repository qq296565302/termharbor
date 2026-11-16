import { describe, expect, it } from "vitest";

import { areaPath, niceMax } from "./chart";

describe("areaPath", () => {
  it("点不足两个时返回 null（单点画不出线）", () => {
    expect(areaPath([], 100, 30, 100)).toBeNull();
    expect(areaPath([42], 100, 30, 100)).toBeNull();
  });

  it("起点在左下、终点在右下", () => {
    const geometry = areaPath([0, 100], 100, 40, 100);
    expect(geometry).not.toBeNull();
    expect(geometry!.line).toBe("M0,40L100,0");
    // 面积要闭合回基线，否则填充会悬在半空
    expect(geometry!.area).toBe("M0,40L100,0L100,40L0,40Z");
  });

  it("越界值夹到 0..max，画不出框外", () => {
    const geometry = areaPath([-20, 400], 100, 40, 100);
    expect(geometry!.line).toBe("M0,40L100,0");
  });

  it("NaN 当作 0，不让整条线消失", () => {
    const geometry = areaPath([Number.NaN, 50], 100, 40, 100);
    expect(geometry!.line).toBe("M0,40L100,20");
  });
});

describe("niceMax", () => {
  it("按 1/2/5 阶梯上浮，给出好读的刻度", () => {
    // 0.4 → 上浮 15% 得 0.46，按 5×10⁻¹ 取刻度
    expect(niceMax([0.4])).toBe(0.5);
    expect(niceMax([2.6])).toBe(3);
    expect(niceMax([7])).toBe(10);
    expect(niceMax([130])).toBe(150);
    // 只有 1/2/5 三档时这里会被抬到 5000，曲线压在下三分之一
    expect(niceMax([1800])).toBe(2500);
  });

  it("全零或空序列退回 fallback", () => {
    expect(niceMax([], 100)).toBe(100);
    expect(niceMax([0, 0], 100)).toBe(100);
  });

  it("上界必须大于峰值（否则顶到天花板看不出余量）", () => {
    for (const peak of [3, 9, 11, 99, 1234]) {
      expect(niceMax([peak])).toBeGreaterThan(peak);
    }
  });
});
