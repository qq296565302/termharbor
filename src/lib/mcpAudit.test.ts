import { describe, expect, it } from "vitest";

import { leaseLabel } from "./mcpAudit";
import type { McpAuditEntry } from "./types";

function entry(over: Partial<McpAuditEntry> = {}): McpAuditEntry {
  return {
    tsMs: 10_000,
    client: "agent/1",
    tool: "exec",
    connection: "web-01",
    args: "df -h",
    decision: "allowed",
    reason: "-",
    durationMs: 10,
    bytes: 20,
    result: "ok",
    lease: { pid: 100, beatMs: 9_500, ageMs: 500 },
    ...over,
  };
}

describe("leaseLabel", () => {
  it("本进程写的租约不报警", () => {
    const out = leaseLabel(entry(), 100, 0);
    expect(out.foreign).toBe(false);
    expect(out.text).toBe("租约 #100 · 1s");
  });

  /** 重启之后 pid 必然对不上，那不是异常 —— 报成异常等于每次启动都刷一片黄。 */
  it("本次启动之前的记录不报警", () => {
    const out = leaseLabel(entry({ tsMs: 500 }), 200, 1_000);
    expect(out.foreign).toBe(false);
  });

  it("本次启动之后、却是别的进程写的，要标出来", () => {
    const out = leaseLabel(entry({ tsMs: 5_000 }), 200, 1_000);
    expect(out.foreign).toBe(true);
    expect(out.hint).toContain("不是当前客户端");
  });

  it("没有租约就是无租约，不参与判断", () => {
    const out = leaseLabel(entry({ lease: null }), 100, 0);
    expect(out.text).toBe("无租约");
    expect(out.foreign).toBe(false);
  });

  /** 心跳时间比记录还晚（时钟回拨）时不能算出负数秒。 */
  it("心跳晚于记录时按 0 秒算", () => {
    const out = leaseLabel(
      entry({ tsMs: 1_000, lease: { pid: 100, beatMs: 2_000, ageMs: 0 } }),
      100,
      0,
    );
    expect(out.text).toBe("租约 #100 · 0s");
  });
});
