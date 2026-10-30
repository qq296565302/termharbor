/**
 * 负载看板用到的纯格式化函数。
 *
 * 单独放一个文件是为了能测：这些函数决定用户看到的数字，
 * `NaN`、负值、超大值都在真实数据里出现过（计数器回绕、0 核的容器）。
 */

/** 0-100 之间的百分比，非法输入一律当 0。 */
export function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(100, Math.max(0, value));
}

/** 阈值配色：70% 起提醒，90% 起危险。与诊断面板口径一致。 */
export function levelOf(percent: number): "is-ok" | "is-warn" | "is-danger" {
  if (percent >= 90) return "is-danger";
  if (percent >= 70) return "is-warn";
  return "is-ok";
}

/**
 * KB → 人能读的单位。`/proc/meminfo` 全是 KB。
 *
 * 超过 100 之后不保留小数：`12480.0 MB` 的小数位是噪音。
 */
export function formatKb(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "0";
  const units = ["KB", "MB", "GB", "TB"];
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size >= 100 ? size.toFixed(0) : size.toFixed(1)} ${units[unit]}`;
}

/** 字节/秒 → 速率文本。看板只看量级，不追求小数。 */
export function formatRate(bps: number): string {
  if (!Number.isFinite(bps) || bps <= 0) return "0 B/s";
  const units = ["B/s", "KB/s", "MB/s", "GB/s"];
  let size = bps;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size >= 100 ? size.toFixed(0) : size.toFixed(1)} ${units[unit]}`;
}

/** 开机时长。只到天/小时/分钟，秒没有信息量。 */
export function formatUptime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "—";
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days} 天 ${hours} 小时`;
  if (hours > 0) return `${hours} 小时 ${minutes} 分`;
  return `${minutes} 分钟`;
}

/**
 * 负载均值该怎么解读。
 *
 * 绝对值没有意义：**要除以核心数**。4 核机器上 load 4 就是满载，
 * 32 核机器上 load 4 几乎空闲 —— 不解释这一点，看板只会让人误判。
 */
export function loadAdvice(load1: number, cores: number): string {
  if (!Number.isFinite(load1) || cores <= 0) return "";
  const ratio = load1 / cores;
  if (ratio >= 1) {
    return `已超过核心数（${cores} 核），进程在排队等 CPU`;
  }
  if (ratio >= 0.7) {
    return `接近核心数（${cores} 核），再高就要排队了`;
  }
  return `相对 ${cores} 核来说很空闲`;
}
