import { describe, expect, it } from "vitest";

import {
  filterConnections,
  filterProcesses,
  formatBytes,
  formatKb,
  formatUptime,
  parseInterfaces,
  sortProcesses,
  toggleInterface,
} from "./advancedFormat";
import type { NetConnection, ProcessInfo } from "./types";

function proc(over: Partial<ProcessInfo> = {}): ProcessInfo {
  return {
    pid: 1,
    ppid: 0,
    user: "root",
    stat: "Ss",
    cpuPercent: 0,
    memPercent: 0,
    memKb: 0,
    elapsed: "00:01",
    command: "init",
    ...over,
  };
}

function conn(over: Partial<NetConnection> = {}): NetConnection {
  return {
    proto: "tcp",
    state: "ESTABLISHED",
    localAddr: "127.0.0.1:80",
    remoteAddr: "10.0.0.1:5000",
    pid: 1,
    process: "nginx",
    kind: "established",
    ...over,
  };
}

describe("formatKb", () => {
  it("0、负数与非有限值都归零", () => {
    expect(formatKb(0)).toBe("0 B");
    expect(formatKb(-10)).toBe("0 B");
    expect(formatKb(Number.NaN)).toBe("0 B");
    expect(formatKb(Number.POSITIVE_INFINITY)).toBe("0 B");
  });

  it("B / KB / MB 逐级进位", () => {
    expect(formatKb(0.5)).toBe("512 B");
    expect(formatKb(1)).toBe("1.0 KB");
    expect(formatKb(1536)).toBe("1.5 MB");
  });

  it("GB / TB 大档", () => {
    expect(formatKb(1024 * 1024)).toBe("1.0 GB");
    expect(formatKb(1024 * 1024 * 1024)).toBe("1.0 TB");
  });

  it("上百不保留小数", () => {
    expect(formatKb(512)).toBe("512 KB");
    expect(formatKb(256 * 1024)).toBe("256 MB");
  });
});

describe("formatBytes", () => {
  it("字节直接按档位格式化", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(1024 * 1024 * 3)).toBe("3.0 MB");
  });
});

describe("formatUptime", () => {
  it("0 秒与不足一分钟都有输出", () => {
    expect(formatUptime(0)).toBe("0分");
    expect(formatUptime(59)).toBe("0分");
    expect(formatUptime(60)).toBe("1分");
  });

  it("跨天组合成「X天 Y小时 Z分」", () => {
    expect(formatUptime(3 * 86400 + 4 * 3600 + 12 * 60)).toBe("3天 4小时 12分");
    expect(formatUptime(86400 + 60)).toBe("1天 1分");
  });

  it("省略零段", () => {
    expect(formatUptime(3600)).toBe("1小时");
    expect(formatUptime(86400)).toBe("1天");
  });

  it("非法值退回占位符", () => {
    expect(formatUptime(-1)).toBe("—");
    expect(formatUptime(Number.NaN)).toBe("—");
  });
});

describe("sortProcesses", () => {
  const list = [
    proc({ pid: 3, cpuPercent: 5, memPercent: 50 }),
    proc({ pid: 1, cpuPercent: 99, memPercent: 1 }),
    proc({ pid: 2, cpuPercent: 50, memPercent: 99 }),
  ];

  it("CPU / 内存降序，pid 升序", () => {
    expect(sortProcesses(list, "cpu").map((p) => p.pid)).toEqual([1, 2, 3]);
    expect(sortProcesses(list, "mem").map((p) => p.pid)).toEqual([2, 3, 1]);
    expect(sortProcesses(list, "pid").map((p) => p.pid)).toEqual([1, 2, 3]);
  });

  it("不改原数组", () => {
    const before = list.map((p) => p.pid);
    const sorted = sortProcesses(list, "cpu");
    expect(list.map((p) => p.pid)).toEqual(before);
    expect(sorted).not.toBe(list);
    expect(list[0].pid).toBe(3);
  });
});

describe("filterProcesses", () => {
  const list = [
    proc({ pid: 1234, user: "root", command: "/usr/bin/nginx" }),
    proc({ pid: 9, user: "alice", command: "python3 app.py" }),
    proc({ pid: 77, user: "bob", command: "sshd" }),
  ];

  it("匹配命令名 / pid / user，且大小写无关", () => {
    expect(filterProcesses(list, "nginx")).toHaveLength(1);
    expect(filterProcesses(list, "1234")[0].user).toBe("root");
    expect(filterProcesses(list, "ALICE")[0].pid).toBe(9);
    expect(filterProcesses(list, "sshd")[0].pid).toBe(77);
  });

  it("空查询返回全部且是新数组", () => {
    const all = filterProcesses(list, "  ");
    expect(all).toHaveLength(3);
    expect(all).not.toBe(list);
  });
});

describe("filterConnections", () => {
  const list = [
    conn({ kind: "listen", localAddr: "0.0.0.0:22", process: "sshd" }),
    conn({ kind: "established", localAddr: "127.0.0.1:80", process: "nginx" }),
    conn({ kind: "other", localAddr: "10.0.0.1:53", process: "named" }),
  ];

  it("全部 / 仅监听 / 已建立 三种筛选", () => {
    expect(filterConnections(list, "", "all")).toHaveLength(3);
    expect(filterConnections(list, "", "listen")[0].process).toBe("sshd");
    expect(filterConnections(list, "", "established")[0].process).toBe("nginx");
  });

  it("筛选与搜索同时生效", () => {
    expect(filterConnections(list, "nginx", "all")).toHaveLength(1);
    expect(filterConnections(list, ":22", "all")).toHaveLength(1);
    expect(filterConnections(list, "nginx", "listen")).toHaveLength(0);
  });
});

describe("parseInterfaces", () => {
  it("去空、trim、去重且保序", () => {
    expect(parseInterfaces("eth0, eth1")).toEqual(["eth0", "eth1"]);
    expect(parseInterfaces("eth0,, eth0 ,lo,")).toEqual(["eth0", "lo"]);
    expect(parseInterfaces("")).toEqual([]);
    expect(parseInterfaces("  ,  ")).toEqual([]);
  });
});

describe("toggleInterface", () => {
  it("逐个加入", () => {
    expect(toggleInterface("", "eth0")).toBe("eth0");
    expect(toggleInterface("eth0", "eth1")).toBe("eth0,eth1");
  });

  it("取消与最后一个取消后变空串", () => {
    expect(toggleInterface("eth0,eth1", "eth0")).toBe("eth1");
    expect(toggleInterface("eth0", "eth0")).toBe("");
  });

  it("对已有设置去重后再切换", () => {
    expect(toggleInterface("eth0, eth1", "eth1")).toBe("eth0");
    expect(toggleInterface("eth0,eth0", "eth0")).toBe("");
  });
});
