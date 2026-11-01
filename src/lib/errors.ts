/**
 * 错误码契约。
 *
 * 这里的 code 必须与 Rust 侧 `src-tauri/src/error.rs` 的 `ErrCode` 一字不差。
 * 前端的职责是「把 code 映射成人话」，永远不直接展示 Rust 抛上来的 detail（需求 4.6）。
 */

export const ERR_CODES = [
  // SSH
  "SSH_TCP_TIMEOUT",
  "SSH_AUTH_FAILED",
  "SSH_HOST_KEY_MISMATCH",
  "SSH_HOST_KEY_UNKNOWN",
  "SSH_ALGO_NEGOTIATION",
  "SSH_DISCONNECTED",
  "SSH_SESSION_NOT_FOUND",
  // SFTP
  "SFTP_PERMISSION_DENIED",
  "SFTP_NOT_FOUND",
  "SFTP_IO_ERROR",
  "SFTP_CANCELLED",
  // 加密与存储
  "CRYPTO_KEY_UNAVAILABLE",
  "CRYPTO_DECRYPT_FAILED",
  "CONNECTION_NOT_FOUND",
  "STORAGE_ERROR",
  // 通用
  "VALIDATION_ERROR",
  "UPDATE_CHECK_FAILED",
  "INTERNAL",
  "UNKNOWN",
] as const;

export type ErrCode = (typeof ERR_CODES)[number];

export const ERROR_MESSAGES: Record<ErrCode, string> = {
  SSH_TCP_TIMEOUT: "无法连接到服务器，请检查地址与端口",
  SSH_AUTH_FAILED: "认证失败，请检查用户名和凭据",
  SSH_HOST_KEY_MISMATCH: "主机密钥已变更，可能存在安全风险，已阻止连接",
  SSH_HOST_KEY_UNKNOWN: "首次连接该主机，请确认指纹",
  SSH_ALGO_NEGOTIATION: "与服务器的加密算法协商失败",
  SSH_DISCONNECTED: "连接已断开",
  SSH_SESSION_NOT_FOUND: "会话不存在或已关闭",
  SFTP_PERMISSION_DENIED: "权限不足，无法执行该操作",
  SFTP_NOT_FOUND: "文件或目录不存在",
  SFTP_IO_ERROR: "文件传输失败",
  SFTP_CANCELLED: "传输已取消",
  CRYPTO_KEY_UNAVAILABLE: "无法获取本地加密密钥",
  CRYPTO_DECRYPT_FAILED: "本地配置解密失败",
  CONNECTION_NOT_FOUND: "连接配置不存在",
  STORAGE_ERROR: "本地数据读写失败",
  VALIDATION_ERROR: "输入内容不合法",
  UPDATE_CHECK_FAILED: "检查更新失败，请确认网络能访问 GitHub",
  INTERNAL: "应用内部错误",
  UNKNOWN: "发生未知错误",
};

export interface AppErrorPayload {
  code: string;
  message?: string;
  detail?: string | null;
}

export class AppError extends Error {
  readonly code: ErrCode;
  readonly detail: string | null;

  constructor(code: ErrCode, message: string, detail: string | null = null) {
    super(message);
    this.name = "AppError";
    this.code = code;
    this.detail = detail;
  }
}

function isKnownCode(code: string): code is ErrCode {
  return (ERR_CODES as readonly string[]).includes(code);
}

export function isErrorPayload(value: unknown): value is AppErrorPayload {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { code?: unknown }).code === "string"
  );
}

/**
 * 这次失败是不是「前端根本没拿到 Rust 的答复」。
 *
 * 区分很关键：业务错误（Rust 正常返回 `{ code, message }`）是日常，不该报警；
 * 而 `invoke` 自己抛的错（命令名不存在、参数对不上、通道断了）说明这条路本身坏了。
 * 后者现在到处都是 `.catch(() => undefined)`，出了事一点痕迹都不留。
 */
export function isTransportFailure(err: unknown): boolean {
  return !isErrorPayload(err);
}

/** code → 用户可读文案。未知 code 退回后端兜底文案，再退回通用文案。 */
export function messageFor(code: string, fallback?: string): string {
  if (isKnownCode(code)) {
    return ERROR_MESSAGES[code];
  }
  return fallback && fallback.length > 0 ? fallback : ERROR_MESSAGES.UNKNOWN;
}

/** 把 invoke 抛上来的任意东西统一成 AppError。 */
export function fromUnknown(err: unknown): AppError {
  if (err instanceof AppError) {
    return err;
  }
  if (isErrorPayload(err)) {
    const code = isKnownCode(err.code) ? err.code : "UNKNOWN";

    // 校验错误的 detail 就是给人看的具体原因（「命令不能换行」这种），
    // 而 message 是固定文案「输入内容不合法」。只显示 message 等于没说，
    // 所以这一类把 detail 顶到 message 上。
    // 其他错误仍然遵循「detail 不外露」（可能含主机名、路径等内部信息）。
    if (code === "VALIDATION_ERROR" && err.detail) {
      return new AppError(code, err.detail, err.detail);
    }

    return new AppError(code, messageFor(err.code, err.message), err.detail ?? null);
  }
  if (err instanceof Error) {
    return new AppError("UNKNOWN", err.message);
  }
  if (typeof err === "string") {
    return new AppError("UNKNOWN", err);
  }
  return new AppError("UNKNOWN", ERROR_MESSAGES.UNKNOWN);
}
