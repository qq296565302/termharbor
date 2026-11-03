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

/**
 * 重命名前的本地校验。返回错误文案；`null` 表示可以提交。
 *
 * 这几条都是"提交上去必然失败、但服务器只会回一个笼统的 Failure"的情况，
 * 让用户看到「文件传输失败」等于什么都没说 —— 所以挡在本地，把原因说清楚。
 */
export function validateRename(
  draft: string,
  entry: { name: string; path: string },
  siblings: { name: string; path: string }[],
): string | null {
  const name = draft.trim();
  if (!name) return "请输入新名称";
  if (name === "." || name === "..") return "这个名称不能用";
  if (name.includes("/")) return "名称里不能包含 /";
  if (name === entry.name) return null; // 没改，等同"可以提交"，调用方据此跳过请求

  const clash = siblings.some((item) => item.name === name && item.path !== entry.path);
  if (clash) return `当前目录已有「${name}」`;

  return null;
}
