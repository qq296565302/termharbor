/**
 * 高级监控面板用到的纯格式化 / 筛选函数。
 *
 * 为什么单独放一个文件：这些函数决定用户看到的数字与顺序，
 * 而面板本身要轮询、要发 IPC，混在一起没法测。抽成纯函数后
 * 单测不需要任何 DOM 或 Tauri 运行时。
 */

import type { NetConnection, ProcessInfo } from "./types";

/** 进程排序维度。与 sortProcesses 的 by 参数一一对应。 */
export type ProcessSort = "cpu" | "mem" | "pid";
/** 连接筛选维度。 */
export type ConnectionKind = "all" | "listen" | "established";

/**
 * 字节 -> 人读单位。
 *
 * 上百之后不保留小数：`12480.0 MB` 的小数位是噪音。
 * 字节档不保留小数，其余档一位小数。
 */
function humanize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let size = bytes;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const text = unit === 0 ? size.toFixed(0) : size >= 100 ? size.toFixed(0) : size.toFixed(1);
  return `${text} ${units[unit]}`;
}

/**
 * KB -> B/KB/MB/GB/TB。`/proc/meminfo`、`df` 的字段都是 KB。
 * 先把 KB 折算成字节再进位，这样小于 1 KB 的值会显示成 "512 B" 而不是 "0.0 KB"。
 */
export function formatKb(kb: number): string {
  return humanize(kb * 1024);
}

/** 字节 -> 人读单位。网卡累计收发（rxBytes/txBytes）给的是字节，不是 KB。 */
export function formatBytes(bytes: number): string {
  return humanize(bytes);
}

/**
 * 开机时长 -> "3天 4小时 12分"。只到天/小时/分，秒没有信息量。
 *
 * 零段省略（"1小时" 而不是 "1小时 0分"），但至少要留一个 "X分"：
 * 刚开机或非法输入时不能返回空串。
 */
export function formatUptime(sec: number): string {
  if (!Number.isFinite(sec) || sec < 0) return "—";
  const days = Math.floor(sec / 86400);
  const hours = Math.floor((sec % 86400) / 3600);
  const minutes = Math.floor((sec % 3600) / 60);
  const parts: string[] = [];
  if (days > 0) parts.push(`${days}天`);
  if (hours > 0) parts.push(`${hours}小时`);
  if (minutes > 0 || parts.length === 0) parts.push(`${minutes}分`);
  return parts.join(" ");
}

/**
 * 进程排序，返回新数组。
 *
 * 不能直接 `list.sort()`：数组来自 React state，原地排序会污染上一次渲染的数据。
 * CPU / 内存降序（谁最耗资源排前面），pid 升序（编号顺着读更自然）。
 * 同值时按 pid 升序兜底，保证顺序稳定、可预期。
 */
export function sortProcesses(list: ProcessInfo[], by: "cpu" | "mem" | "pid"): ProcessInfo[] {
  const key = (p: ProcessInfo): number => {
    if (by === "cpu") return p.cpuPercent;
    if (by === "mem") return p.memPercent;
    return p.pid;
  };
  const direction = by === "pid" ? 1 : -1;
  return [...list].sort((a, b) => {
    const diff = (key(a) - key(b)) * direction;
    return diff !== 0 ? diff : a.pid - b.pid;
  });
}

/** 按命令名 / pid / 用户过滤进程，大小写无关。空查询返回全部（副本）。 */
export function filterProcesses(list: ProcessInfo[], q: string): ProcessInfo[] {
  const needle = q.trim().toLowerCase();
  if (!needle) return [...list];
  return list.filter(
    (p) =>
      p.command.toLowerCase().includes(needle) ||
      String(p.pid).includes(needle) ||
      p.user.toLowerCase().includes(needle),
  );
}

/**
 * 连接筛选：先按 kind 收敛，再按搜索词匹配。
 * kind 直接取后端给的 `listen`/`established`/`other`，前端不自己猜状态。
 */
export function filterConnections(
  list: NetConnection[],
  q: string,
  kind: "all" | "listen" | "established",
): NetConnection[] {
  const needle = q.trim().toLowerCase();
  return list.filter((c) => {
    if (kind === "listen" && c.kind !== "listen") return false;
    if (kind === "established" && c.kind !== "established") return false;
    if (!needle) return true;
    return (
      c.proto.toLowerCase().includes(needle) ||
      c.state.toLowerCase().includes(needle) ||
      c.localAddr.toLowerCase().includes(needle) ||
      c.remoteAddr.toLowerCase().includes(needle) ||
      c.process.toLowerCase().includes(needle) ||
      String(c.pid).includes(needle)
    );
  });
}

/** "eth0, eth1" -> ["eth0","eth1"]：逗号拆分、去空、trim、去重且保序。 */
export function parseInterfaces(setting: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of setting.split(",")) {
    const name = raw.trim();
    if (!name || seen.has(name)) continue;
    seen.add(name);
    out.push(name);
  }
  return out;
}

/**
 * 勾选 / 取消一个网卡，返回新的逗号串。
 * 空串是「全部显示」的哨兵值，所以取消最后一个勾选后必须回到空串。
 */
export function toggleInterface(setting: string, name: string): string {
  const key = name.trim();
  const current = parseInterfaces(setting);
  if (!key) return current.join(",");
  const next = current.includes(key)
    ? current.filter((n) => n !== key)
    : [...current, key];
  return next.join(",");
}
