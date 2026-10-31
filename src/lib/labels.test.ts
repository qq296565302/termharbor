import { describe, expect, it } from "vitest";

import { ipcLabel, keySourceLabel } from "./labels";

describe("keySourceLabel", () => {
  it("把后端枚举翻成人话，同时保留可解释的 hint", () => {
    expect(keySourceLabel("keychain").text).toBe("系统凭据管理器");
    expect(keySourceLabel("dev_file").text).toBe("开发密钥文件");
    expect(keySourceLabel("master_password").text).toBe("主密码");
  });

  it("还不知道来源时不显示 undefined", () => {
    expect(keySourceLabel(null).text).toBe("—");
    expect(keySourceLabel(undefined).text).toBe("—");
  });

  it("没见过的来源原样显示，而不是假装认识", () => {
    expect(keySourceLabel("tpm").text).toBe("tpm");
    expect(keySourceLabel("tpm").hint).toContain("tpm");
  });
});

describe("ipcLabel", () => {
  it("没有错误时是就绪", () => {
    expect(ipcLabel(null).text).toBe("IPC 就绪");
    expect(ipcLabel(null, null).text).toBe("IPC 就绪");
  });

  it("启动探活失败时显示错误码，不是一句笼统的异常", () => {
    expect(ipcLabel("DB_ERROR").text).toBe("DB_ERROR");
  });

  it("运行期调不通时带上命令名，方便定位是哪一条", () => {
    const label = ipcLabel(null, {
      command: "sftp_list",
      message: "command sftp_list not found",
      at: 1,
    });
    expect(label.text).toBe("IPC 异常：sftp_list");
    expect(label.hint).toContain("sftp_list");
  });

  it("启动失败优先于运行期失败（说明更根本）", () => {
    expect(
      ipcLabel("STORAGE_ERROR", { command: "x", message: "y", at: 1 }).text,
    ).toBe("STORAGE_ERROR");
  });
});
