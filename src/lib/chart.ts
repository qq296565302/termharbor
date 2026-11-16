/**
 * 折线 / 面积图的几何计算。
 *
 * 为什么不引图表库：这块面板要的是"几条时间序列 + 分段条"，
 * 引一个 40KB+ 的库换来的是用不上的交互和主题适配成本。这里只算 path 字符串，
 * 画的部分交给 React —— 顺便还能单测（path 是纯字符串，断言起来很直接）。
 */

export interface AreaGeometry {
  /** 折线 path（不带填充） */
  line: string;
  /** 闭合到基线的填充 path */
  area: string;
}

/**
 * 把一串值转成 SVG path。
 *
 * - 值按 `max` 归一化并夹到 0..max，越界不会画出框外；
 * - 点不足 2 个返回 `null`：单点画不出线，硬画会得到一条看不见的竖线；
 * - 线的起点永远在 `x=0`、终点在 `x=width`，所以后写的点会"顶掉"先写的，
 *   这和看板的语义一致（最早的采样从左边流出去）。
 */
export function areaPath(
  values: number[],
  width: number,
  height: number,
  max: number,
): AreaGeometry | null {
  if (values.length < 2 || width <= 0 || height <= 0) return null;

  const safeMax = max > 0 ? max : 1;
  const step = width / (values.length - 1);
  const points = values.map((value, index) => {
    const clamped = Number.isFinite(value) ? Math.min(safeMax, Math.max(0, value)) : 0;
    const x = index * step;
    // y 轴朝下，所以使用率越高 y 越小
    const y = height - (clamped / safeMax) * height;
    return `${round(x)},${round(y)}`;
  });

  const line = `M${points.join("L")}`;
  const area = `${line}L${round(width)},${round(height)}L0,${round(height)}Z`;
  return { line, area };
}

/**
 * "好看"的纵轴上界：最大值上浮 15% 后取到 1/2/5 × 10ⁿ 的刻度。
 *
 * 网络速率这种没有天然上限的量必须用它，否则 300KB/s 和 320KB/s
 * 在 0..320 的坐标里就是两条几乎重合的直线 —— 那正是"看不出波动"的来源。
 */
export function niceMax(values: number[], fallback = 1): number {
  const peak = values.reduce((acc, value) => (Number.isFinite(value) ? Math.max(acc, value) : acc), 0);
  if (peak <= 0) return fallback;

  const target = peak * 1.15;
  const magnitude = 10 ** Math.floor(Math.log10(target));
  const normalized = target / magnitude;

  // 阶梯比 1/2/5 细一档：只有 1/2/5 时 1800 会被抬到 5000，
  // 曲线被压在下面三分之一，反而更难看出波动。
  const ladder = [1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10];
  const step = ladder.find((candidate) => candidate >= normalized) ?? 10;
  return step * magnitude;
}

function round(value: number): number {
  // 两位小数足够：viewBox 上的千分之一像素没人看得出来
  return Math.round(value * 100) / 100;
}
