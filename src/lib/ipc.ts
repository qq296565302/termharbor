import { invoke } from "@tauri-apps/api/core";

import { AppError, fromUnknown, isTransportFailure } from "./errors";

/** 是否运行在 Tauri 运行时里。直接用 `vite dev` 打开页面时为 false。 */
export function isDesktopRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * 最近一次「没调到后端」的痕迹。
 *
 * 为什么需要它：绝大多数后台调用都是 `.catch(() => undefined)`，
 * 命令名拼错、参数对不上、通道断了这类问题会被吞得干干净净，
 * 表现出来只是"某个功能没反应"，连日志都没有。这里留一条，状态栏能看到。
 */
export interface IpcFailure {
  command: string;
  message: string;
  at: number;
}

let failure: IpcFailure | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const fn of listeners) fn();
}

/** 给 `useSyncExternalStore` 用。 */
export function subscribeIpcFailure(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export function getIpcFailure(): IpcFailure | null {
  return failure;
}

export function clearIpcFailure(): void {
  if (!failure) return;
  failure = null;
  emit();
}

/**
 * `invoke` 的统一入口。
 *
 * 所有 command 调用都必须经过这里，好处是错误一定被转成 `AppError`，
 * UI 层只需要 catch 一种东西。
 *
 * 另外顺手记一条「传输层失败」：Rust 正常返回的 `{ code, message }` 是业务错误，
 * 不记；`invoke` 自己抛的（命令不存在、参数对不上、IPC 通道出问题）才记。
 */
export async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isDesktopRuntime()) {
    // 用 `pnpm dev` 在普通浏览器里打开时，invoke 会以很难懂的方式失败，这里提前讲清楚
    throw new AppError(
      "INTERNAL",
      "当前在普通浏览器中运行，请使用 pnpm tauri:dev 启动桌面应用",
    );
  }

  try {
    const result = await invoke<T>(command, args);
    // 同一条命令后来成功了，说明刚才那次是偶发，灯该回到绿色
    if (failure?.command === command) clearIpcFailure();
    return result;
  } catch (err) {
    if (isTransportFailure(err)) {
      failure = { command, message: fromUnknown(err).message, at: Date.now() };
      emit();
      // 不额外抛：下面统一转成 AppError，调用方行为不变
    }
    throw fromUnknown(err);
  }
}
