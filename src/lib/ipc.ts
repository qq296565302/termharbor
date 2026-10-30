import { invoke } from "@tauri-apps/api/core";

import { AppError, fromUnknown } from "./errors";

/** 是否运行在 Tauri 运行时里。直接用 `vite dev` 打开页面时为 false。 */
export function isDesktopRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * `invoke` 的统一入口。
 *
 * 所有 command 调用都必须经过这里，好处是错误一定被转成 `AppError`，
 * UI 层只需要 catch 一种东西。
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
    return await invoke<T>(command, args);
  } catch (err) {
    throw fromUnknown(err);
  }
}
