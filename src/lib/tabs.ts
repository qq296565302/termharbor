/**
 * 标签顺序的纯函数，单独放出来是为了能直接测（组件里不好断言）。
 */

/**
 * 在标签列表里按 step 前后移动，两端循环。
 *
 * @param ids 当前所有标签 id，顺序即显示顺序
 * @param current 当前活动标签，null 表示没有
 * @param step 1 向后，-1 向前
 */
export function nextTabId(
  ids: readonly string[],
  current: string | null,
  step: 1 | -1,
): string | null {
  if (ids.length === 0) {
    return null;
  }
  const index = current === null ? -1 : ids.indexOf(current);
  if (index < 0) {
    // 当前标签不在列表里（刚被关掉），退到第一个
    return step === 1 ? (ids[0] ?? null) : (ids[ids.length - 1] ?? null);
  }
  const next = (index + step + ids.length) % ids.length;
  return ids[next] ?? null;
}
