/**
 * 状态栏那点东西的文案。
 *
 * 后端给的是枚举名（`keychain` / `dev_file` / `master_password`），那是前后端契约，
 * 不能为了好看改 —— 所以翻译放在这一层，界面上给人话，`title` 里再补一句所以然。
 */

import type { IpcFailure } from "./ipc";

export interface Label {
  text: string;
  hint: string;
}

/**
 * 加密密钥存在哪儿。
 *
 * 密码和私钥口令不是明文存的，用的是 AES-256-GCM；这里说的是**那把数据密钥**的去处。
 */
export function keySourceLabel(source: string | null | undefined): Label {
  switch (source) {
    case "keychain":
      return {
        text: "系统凭据管理器",
        hint: "数据库里的密码用 AES-256-GCM 加密，这把数据密钥存在 Windows 凭据管理器里（不写进任何文件）",
      };
    case "dev_file":
      return {
        text: "开发密钥文件",
        hint: "开发构建用的本地密钥文件（TERMHARBOR_KEY_PROVIDER=devfile）。生产环境不该出现这个",
      };
    case "master_password":
      return {
        text: "主密码",
        hint: "数据密钥由你输入的主密码派生。功能未完成，见开发计划 B-2",
      };
    case null:
    case undefined:
      return { text: "—", hint: "还不知道密钥从哪来（后端还没回话）" };
    default:
      return { text: source, hint: `未识别的密钥来源：${source}` };
  }
}

/**
 * 状态栏右下角那个「IPC 就绪」。
 *
 * 它证明的是「启动时 `ping` 成功」—— 前端能调到 Rust 这边的 command。
 * 失败时优先显示启动探活的错误码（比如 `DB_ERROR`），比一句"异常"有用。
 *
 * 另外，运行期出现过「根本没调通」的调用（`invoke` 自己抛的错，不是 Rust 返回的
 * 业务错误）也会顶到这里，否则这类失败会被各处的 `.catch(() => undefined)` 吞掉。
 */
export function ipcLabel(
  errorCode: string | null | undefined,
  failure?: IpcFailure | null,
): Label {
  if (errorCode) {
    return { text: errorCode, hint: "启动自检失败：前端调 Rust 后端没通，错误码含义见「关于」页" };
  }
  if (failure) {
    return {
      text: `IPC 异常：${failure.command}`,
      hint: `调用 ${failure.command} 失败：${failure.message}（点击清除这条记录）`,
    };
  }
  return {
    text: "IPC 就绪",
    hint: "前端 ↔ Rust 后端通信正常（启动时 ping 成功，且之后没出现过调不通的情况）。这里不轮询",
  };
}
