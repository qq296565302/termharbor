/** 文件面板里与渲染无关的小工具。抽出来是为了能单测，边界条件比想象中多。 */

export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;

  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;

  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }

  // 上百的数值给小数位没意义，反而占宽度
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
}

export function formatSpeed(bps: number): string {
  if (!Number.isFinite(bps) || bps <= 0) return "—";
  return `${formatSize(bps)}/s`;
}

export function formatTime(ms: number | null): string {
  if (!ms) return "—";
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "—";

  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

export interface Crumb {
  label: string;
  path: string;
}

/**
 * `/var/log` → `[ / , var , log ]`，每段都带完整的累计路径。
 *
 * 不用 split 后直接拼，是因为末尾斜杠和前导斜杠的组合会让空的路径段混进来，
 * 点进去就变成「当前目录」而不是目标目录。
 */
export function crumbsOf(path: string): Crumb[] {
  const parts = path.split("/").filter(Boolean);
  const crumbs: Crumb[] = [{ label: "/", path: "/" }];

  let acc = "";
  for (const part of parts) {
    acc += `/${part}`;
    crumbs.push({ label: part, path: acc });
  }

  return crumbs;
}

/** 权限位显示：只取低 12 位，转成 `755` 这种三/四位八进制。 */
export function formatMode(mode: number | null): string {
  if (mode === null || !Number.isFinite(mode)) return "—";
  return (mode & 0o777).toString(8).padStart(3, "0");
}
