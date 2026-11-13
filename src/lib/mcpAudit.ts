/**
 * MCP 调用记录的展示逻辑。
 *
 * 单独放这里是为了能测：「这条记录的租约是不是本进程写的」有一条时间边界，
 * 写错了要么永远不报警，要么重启后满屏都是误报 —— 两种都比不做好不了多少。
 */

import type { McpAuditEntry } from "./types";

/** 拒绝原因翻成人话。列表里甩 `command_not_allowed` 没人看得懂，代码留在 title 里备查。 */
export const REASON_LABEL: Record<string, string> = {
  connection_disabled: "连接未开放",
  tool_not_allowed: "这个工具没开",
  path_not_allowed: "路径不在范围内",
  secret_path: "凭据类文件默认挡",
  command_not_allowed: "命令不在白名单",
  compound_command: "命令写法问题",
  empty_command: "空命令",
  no_live_session: "这台机器没连着",
  invalid_params: "参数不合法",
  unknown_tool: "不认识这个工具",
};

export interface LeaseLabel {
  text: string;
  /** 这份租约不是当前客户端进程写的（值得核对，但不定罪） */
  foreign: boolean;
  hint: string;
}

/**
 * 一条记录的「放行依据」。
 *
 * `foreign` 的边界是这里唯一的难点：租约里写着写它的进程 PID，正常情况就是当前
 * 这个客户端进程。但**本次启动之前**的记录属于上一次运行，PID 对不上完全正常，
 * 拿它报警只会让用户每次重启都看到一片黄。
 *
 * 反过来，同一用户下谁都能往那个文件里写，所以这一栏的作用是「值得核对」，
 * 不是「已发现入侵」—— 开两个客户端窗口同样会出现这种记录，文案里得说清楚。
 */
export function leaseLabel(
  entry: McpAuditEntry,
  selfPid: number,
  startedAtMs: number,
): LeaseLabel {
  const lease = entry.lease;
  if (!lease) {
    return {
      text: "无租约",
      foreign: false,
      hint:
        "这条记录没有租约指纹。只有真正连服务器的工具才会记 —— " +
        "list_connections 只读本地库，本来就不经过这道闸门；" +
        "其余情况可能是关掉了在线校验，或者日志出自 0.2.1 及更早的版本。",
    };
  }

  const age = Math.max(0, Math.round((entry.tsMs - lease.beatMs) / 1000));
  const foreign = lease.pid !== selfPid && entry.tsMs >= startedAtMs;
  return {
    text: `租约 #${lease.pid} · ${age}s`,
    foreign,
    hint: foreign
      ? `这次调用用的租约是进程 ${lease.pid} 写的，不是当前客户端（${selfPid}）。` +
        `同一用户下任何进程都能写那个文件，请核对那一时刻客户端是否开着；` +
        `开了两个客户端窗口也会出现这种记录。`
      : `由进程 ${lease.pid} 写的租约放行。租约每 5 秒续一次，${age} 秒前刚心跳过。` +
        `文件本身防不了伪造，这一栏是给你事后核对的。`,
  };
}
