import { describe, expect, it } from "vitest";

import {
  crumbsOf,
  formatMode,
  formatSize,
  formatSpeed,
  formatTime,
  validateRename,
} from "./files";

describe("formatSize", () => {
  it("小于 1KB 直接给字节数", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(1)).toBe("1 B");
    expect(formatSize(1023)).toBe("1023 B");
  });

  it("按 1024 进位", () => {
    expect(formatSize(1024)).toBe("1.0 KB");
    expect(formatSize(1536)).toBe("1.5 KB");
    expect(formatSize(1024 * 1024)).toBe("1.0 MB");
  });

  it("上百之后不再保留小数", () => {
    expect(formatSize(150 * 1024)).toBe("150 KB");
  });

  it("异常输入不崩，也不显示 NaN", () => {
    expect(formatSize(-1)).toBe("0 B");
    expect(formatSize(Number.NaN)).toBe("0 B");
  });
});

describe("formatSpeed", () => {
  it("没有速度时显示占位符", () => {
    expect(formatSpeed(0)).toBe("—");
  });

  it("带上每秒后缀", () => {
    expect(formatSpeed(2048)).toBe("2.0 KB/s");
  });
});

describe("formatTime", () => {
  it("空值显示占位符", () => {
    expect(formatTime(null)).toBe("—");
    expect(formatTime(0)).toBe("—");
  });

  it("非法时间戳不显示 Invalid Date", () => {
    expect(formatTime(Number.NaN)).toBe("—");
  });
});

describe("crumbsOf", () => {
  it("根目录只有一段", () => {
    expect(crumbsOf("/")).toEqual([{ label: "/", path: "/" }]);
  });

  it("逐级累计路径", () => {
    expect(crumbsOf("/var/log")).toEqual([
      { label: "/", path: "/" },
      { label: "var", path: "/var" },
      { label: "log", path: "/var/log" },
    ]);
  });

  it("末尾斜杠不会造出空的一级", () => {
    expect(crumbsOf("/var/log/")).toEqual(crumbsOf("/var/log"));
  });

  it("连续斜杠被折叠", () => {
    expect(crumbsOf("//var//log")).toEqual(crumbsOf("/var/log"));
  });
});

describe("formatMode", () => {
  it("只取权限位，丢掉类型位", () => {
    // 0o40755 = 目录 + rwxr-xr-x
    expect(formatMode(0o40755)).toBe("755");
    expect(formatMode(0o100644)).toBe("644");
  });

  it("缺失时给占位符", () => {
    expect(formatMode(null)).toBe("—");
  });
});

describe("validateRename", () => {
  const entry = { name: "a.log", path: "/var/log/a.log" };
  const siblings = [entry, { name: "b.log", path: "/var/log/b.log" }];

  it("空名字要拦住", () => {
    expect(validateRename("   ", entry, siblings)).toBe("请输入新名称");
  });

  it("不允许 . / .. / 带斜杠的名字（那是路径，不是名字）", () => {
    expect(validateRename(".", entry, siblings)).toBe("这个名称不能用");
    expect(validateRename("..", entry, siblings)).toBe("这个名称不能用");
    expect(validateRename("../etc/passwd", entry, siblings)).toBe("名称里不能包含 /");
  });

  it("撞名时给一句能看懂的话，而不是让服务器回笼统的失败", () => {
    expect(validateRename("b.log", entry, siblings)).toBe("当前目录已有「b.log」");
  });

  it("名字没改算通过（调用方会跳过请求）", () => {
    expect(validateRename("a.log", entry, siblings)).toBeNull();
  });

  it("正常改名通过，前后的空格会被去掉", () => {
    expect(validateRename("  c.log  ", entry, siblings)).toBeNull();
  });
});
