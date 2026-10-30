import { describe, expect, it } from "vitest";

import { AppError, ERROR_MESSAGES, fromUnknown, messageFor } from "./errors";

describe("messageFor", () => {
  it("已知 code 走中文映射", () => {
    expect(messageFor("SSH_HOST_KEY_MISMATCH")).toBe(ERROR_MESSAGES.SSH_HOST_KEY_MISMATCH);
  });

  it("未知 code 优先用后端兜底文案", () => {
    expect(messageFor("SOMETHING_NEW", "后端说的")).toBe("后端说的");
  });

  it("未知 code 且无兜底时给通用文案", () => {
    expect(messageFor("SOMETHING_NEW")).toBe(ERROR_MESSAGES.UNKNOWN);
  });
});

describe("fromUnknown", () => {
  it("解析 Rust 的结构化错误并忽略原始 detail 作为主文案", () => {
    const err = fromUnknown({
      code: "SSH_AUTH_FAILED",
      message: "认证失败，请检查用户名和凭据",
      detail: "Permission denied (publickey)",
    });
    expect(err).toBeInstanceOf(AppError);
    expect(err.code).toBe("SSH_AUTH_FAILED");
    // 主文案来自前端映射，不是后端原文
    expect(err.message).toBe(ERROR_MESSAGES.SSH_AUTH_FAILED);
    expect(err.detail).toBe("Permission denied (publickey)");
  });

  it("未登记的 code 降级为 UNKNOWN", () => {
    const err = fromUnknown({ code: "WEIRD_CODE", message: "x" });
    expect(err.code).toBe("UNKNOWN");
    expect(err.message).toBe("x");
  });

  it("普通 Error 与字符串都能兜住", () => {
    expect(fromUnknown(new Error("boom")).message).toBe("boom");
    expect(fromUnknown("boom").message).toBe("boom");
    expect(fromUnknown(42).code).toBe("UNKNOWN");
  });

  it("幂等：已是 AppError 时原样返回", () => {
    const original = new AppError("STORAGE_ERROR", "x");
    expect(fromUnknown(original)).toBe(original);
  });
});
