import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";

import { parseInterfaces } from "../lib/advancedFormat";
import { areaPath, niceMax } from "../lib/chart";
import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import {
  clampPercent,
  formatKb,
  formatRate,
  formatUptime,
  levelOf,
  loadAdvice,
} from "../lib/metrics";
import type { LoadSnapshot } from "../lib/types";
import { useSettingsStore } from "../stores/settingsStore";

/** 自动刷新间隔。3 秒足够看出趋势，又不至于把服务器刷烦。 */
const REFRESH_MS = 3000;
/** 保留多少个采样点（3 秒一个 → 3 分钟）。 */
const HISTORY_MAX = 60;

interface Props {
  /** 当前标签的终端会话。为空时不采样，只显示提示。 */
  sessionId: string | null;
  /**
   * 页面是否在前台。
   *
   * 会话页是常驻挂载的（切到设置页只是隐藏），面板不跟着停就会一直每 3 秒
   * 往生产服务器打一次 exec —— 用户看不见的地方不该偷偷跑命令。
   */
  active: boolean;
  onClose: () => void;
}

/**
 * 一个采样点。
 *
 * 整份快照不进历史（进程列表、挂载点每次都变，存 60 份纯属浪费），
 * 只留画曲线要用的标量 —— 60 个点也就几 KB。
 */
interface Sample {
  at: number;
  cpu: number;
  mem: number;
  swap: number;
  load1: number;
  /** 全部网卡合计（B/s） */
  rx: number;
  tx: number;
  tcp: number;
}

interface State {
  snapshot: LoadSnapshot | null;
  error: string | null;
  loading: boolean;
  /** 上次成功采样的时刻（本地时间） */
  at: number | null;
}

/**
 * 服务器负载看板。
 *
 * 数据全部来自 `/proc`（只读），一次 exec 采全部指标：
 * - CPU 使用率与网络速率靠**两次采样求差**（这些是累计计数器），
 *   命令里 sleep 0.5 秒，间隔用 `/proc/uptime` 的真实差值算，不假设 sleep 精度
 * - 解析在 Rust 侧完成（`monitor::load`），前端只负责画
 *
 * 视觉上按"大数据看板"来做：KPI 条带迷你趋势、CPU 大图带阈值线、
 * 分核热力条、内存分段条、网络双曲线。图自己画 SVG（`lib/chart`），不引图表库。
 *
 * 刷新失败的策略：**保留上一次的数据**，只在顶部挂一条错误。
 * 网络抖一下就把整块看板抹白，比看到旧数据更糟。
 */
export default function LoadPanel({ sessionId, active, onClose }: Props) {
  const [state, setState] = useState<State>({
    snapshot: null,
    error: null,
    loading: false,
    at: null,
  });
  const [auto, setAuto] = useState(true);
  /** 滚动采样序列，画曲线用 */
  const [samples, setSamples] = useState<Sample[]>([]);

  // 「高级监控 → 网络接口」里勾选的网卡。空 = 全部显示；
  // 两边共用 settings 里的同一个字段，不另存一份，免得改了一边另一边不认。
  const monitorInterfaces = useSettingsStore((s) => s.settings.monitorInterfaces);
  const selectedNets = useMemo(
    () => new Set(parseInterfaces(monitorInterfaces)),
    [monitorInterfaces],
  );
  const visibleNets = useMemo(
    () =>
      (state.snapshot?.nets ?? []).filter(
        (net) => selectedNets.size === 0 || selectedNets.has(net.name),
      ),
    [state.snapshot, selectedNets],
  );

  /** 防止慢请求叠起来：3 秒一次，但一次采样要 0.5 秒 + 往返 */
  const inFlight = useRef(false);

  const refresh = useCallback(async () => {
    if (!sessionId || inFlight.current) return;

    inFlight.current = true;
    setState((prev) => ({ ...prev, loading: true }));
    try {
      const snapshot = await call<LoadSnapshot>("load_snapshot", { sessionId });
      setState({ snapshot, error: null, loading: false, at: Date.now() });
      setSamples((prev) =>
        [
          ...prev,
          {
            at: Date.now(),
            cpu: snapshot.cpu.percent,
            mem: snapshot.memory.percent,
            swap: snapshot.memory.swapPercent,
            load1: snapshot.load1,
            rx: snapshot.nets.reduce((sum, net) => sum + net.rxBps, 0),
            tx: snapshot.nets.reduce((sum, net) => sum + net.txBps, 0),
            tcp: snapshot.tcpEstablished,
          },
        ].slice(-HISTORY_MAX),
      );
    } catch (err) {
      setState((prev) => ({
        ...prev,
        loading: false,
        // 保留旧的 snapshot：抖一下就清空比看到旧数据更难受
        error: fromUnknown(err).message,
      }));
    } finally {
      inFlight.current = false;
    }
  }, [sessionId]);

  // 切标签（sessionId 变化）立刻采一次，并把历史清掉 —— 那是另一台机器的曲线
  useEffect(() => {
    setSamples([]);
    setState({ snapshot: null, error: null, loading: false, at: null });
    if (sessionId) void refresh();
  }, [sessionId, refresh]);

  // 窗口最小化/被切到后台时也要停：WebView 里定时器不会被节流到停
  const [windowVisible, setWindowVisible] = useState(() => !document.hidden);
  useEffect(() => {
    const handler = () => setWindowVisible(!document.hidden);
    document.addEventListener("visibilitychange", handler);
    return () => document.removeEventListener("visibilitychange", handler);
  }, []);

  useEffect(() => {
    if (!auto || !sessionId || !active || !windowVisible) return;
    const timer = window.setInterval(() => void refresh(), REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [auto, active, refresh, sessionId, windowVisible]);

  const snapshot = state.snapshot;

  // 序列拆出来喂给各个图。每帧新建数组，但这比让图表组件自己 slice 更直观
  const cpuSeries = useMemo(() => samples.map((s) => s.cpu), [samples]);
  const memSeries = useMemo(() => samples.map((s) => s.mem), [samples]);
  const loadSeries = useMemo(() => samples.map((s) => s.load1), [samples]);
  const tcpSeries = useMemo(() => samples.map((s) => s.tcp), [samples]);
  const rxSeries = useMemo(() => samples.map((s) => s.rx), [samples]);
  const txSeries = useMemo(() => samples.map((s) => s.tx), [samples]);

  const netMax = useMemo(() => niceMax([...rxSeries, ...txSeries], 1024), [rxSeries, txSeries]);
  const loadMax = useMemo(
    () => niceMax(loadSeries, Math.max(1, snapshot?.cpu.coresCount ?? 1)),
    [loadSeries, snapshot],
  );
  const cpuStats = useMemo(() => statsOf(cpuSeries), [cpuSeries]);
  const windowSecs =
    samples.length > 1 ? Math.round((samples[samples.length - 1].at - samples[0].at) / 1000) : 0;

  return (
    <aside className="term-load">
      <div className="term-load__head">
        <span className="term-load__title">负载</span>
        <span className="term-load__host" title={snapshot?.kernel ?? ""}>
          {snapshot?.os || "—"}
        </span>
        <button
          type="button"
          className={`term-load__auto ${auto ? "is-on" : ""}`}
          title={auto ? "每 3 秒自动刷新（点击暂停）" : "已暂停，点击恢复自动刷新"}
          onClick={() => setAuto((value) => !value)}
        >
          {/* 状态灯：只有「正在自动刷新」才亮，比读两个字快 */}
          <span className="term-load__pulse" />
          {auto ? "自动" : "已暂停"}
        </button>
        <button
          type="button"
          className="btn btn--ghost btn--icon btn--sm"
          title="立即刷新"
          aria-label="立即刷新"
          disabled={!sessionId || state.loading}
          onClick={() => void refresh()}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M13 8a5 5 0 1 1-1.6-3.7" />
            <path d="M13 2.6V5h-2.4" />
          </svg>
        </button>
        <button
          type="button"
          className="btn btn--ghost btn--icon btn--sm term-load__close"
          title="关闭看板"
          aria-label="关闭看板"
          onClick={onClose}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M4.2 4.2l7.6 7.6M11.8 4.2l-7.6 7.6" />
          </svg>
        </button>
      </div>

      {!sessionId ? (
        <p className="term-load__hint">先打开一个终端标签，看板显示当前标签那台服务器。</p>
      ) : null}

      {state.error ? <p className="term-load__error">{state.error}</p> : null}
      {snapshot?.warning ? <p className="term-load__warn">{snapshot.warning}</p> : null}

      {sessionId && !snapshot && state.loading ? (
        <p className="term-load__hint">正在采样…</p>
      ) : null}

      {snapshot ? (
        <div className="term-load__body">
          {/* KPI 条：四个关键数 + 迷你趋势。"现在的状态"最先要看到 */}
          <div className="viz-kpi">
            <Kpi
              label="CPU"
              value={snapshot.cpu.percent.toFixed(1)}
              unit="%"
              series={cpuSeries}
              max={100}
              tone={levelOf(snapshot.cpu.percent)}
            />
            <Kpi
              label="内存"
              value={snapshot.memory.percent.toFixed(1)}
              unit="%"
              series={memSeries}
              max={100}
              tone={levelOf(snapshot.memory.percent)}
            />
            <Kpi
              label="负载 1m"
              value={snapshot.load1.toFixed(2)}
              series={loadSeries}
              max={loadMax}
              tone={snapshot.load1 > snapshot.cpu.coresCount ? "is-warn" : "is-ok"}
            />
            <Kpi
              label="TCP"
              value={String(snapshot.tcpEstablished)}
              series={tcpSeries}
              max={niceMax(tcpSeries, 10)}
              tone="is-ok"
            />
          </div>

          {/* CPU：主图 + 阈值线 + 分核热力。这一块是"看不出波动"的重灾区，
              所以给足高度、按 0–100 固定刻度、再压两条参考线 */}
          <section className="viz-card">
            <header className="viz-card__head">
              <span className="viz-card__name">CPU 使用率</span>
              <span className="viz-card__meta">
                {windowSecs > 0 ? `近 ${windowSecs}s · ${samples.length} 点` : "等待第二个采样点"}
              </span>
            </header>

            <Chart values={cpuSeries} max={100} height={94} thresholds={[70, 90]} />

            <div className="viz-stats">
              <span>
                当前 <b>{snapshot.cpu.percent.toFixed(1)}%</b>
              </span>
              <span>
                峰值 <b>{cpuStats.peak.toFixed(0)}%</b>
              </span>
              <span>
                均值 <b>{cpuStats.avg.toFixed(0)}%</b>
              </span>
              <span className="viz-stats__dim">{snapshot.cpu.coresCount} 核</span>
            </div>

            <div className="viz-cores">
              {snapshot.cpu.cores.map((core, index) => (
                <span className="viz-core" key={index} title={`cpu${index} ${core.toFixed(0)}%`}>
                  <span className="viz-core__track">
                    <span
                      className={`viz-core__fill ${levelOf(core)}`}
                      style={{ height: `${clampPercent(core)}%` }}
                    />
                  </span>
                  <span className="viz-core__label">{index}</span>
                </span>
              ))}
              {snapshot.cpu.coresCount === 0 ? (
                <span className="viz-card__meta">读不到分核数据</span>
              ) : null}
            </div>
          </section>

          {/* 内存：分段条 + 明细 */}
          <section className="viz-card">
            <header className="viz-card__head">
              <span className="viz-card__name">内存</span>
              <span className="viz-card__meta">
                {formatKb(snapshot.memory.usedKb)} / {formatKb(snapshot.memory.totalKb)}
              </span>
            </header>

            <StackBar
              total={snapshot.memory.totalKb}
              segments={[
                { value: snapshot.memory.usedKb, className: "is-used", label: "已用" },
                {
                  value: snapshot.memory.cachedKb + snapshot.memory.buffersKb,
                  className: "is-cache",
                  label: "缓存/缓冲",
                },
              ]}
            />

            <div className="viz-legend">
              <span className="viz-legend__item">
                <i className="viz-legend__dot is-used" />
                已用 {formatKb(snapshot.memory.usedKb)}
              </span>
              <span className="viz-legend__item">
                <i className="viz-legend__dot is-cache" />
                缓存 {formatKb(snapshot.memory.cachedKb + snapshot.memory.buffersKb)}
              </span>
              <span className="viz-legend__item">
                <i className="viz-legend__dot is-free" />
                可用 {formatKb(snapshot.memory.availableKb)}
              </span>
            </div>

            {snapshot.memory.swapTotalKb > 0 ? (
              <div className="viz-sub">
                <div className="viz-sub__head">
                  <span>Swap</span>
                  <span>
                    {formatKb(snapshot.memory.swapUsedKb)} / {formatKb(snapshot.memory.swapTotalKb)}
                  </span>
                </div>
                <StackBar
                  total={snapshot.memory.swapTotalKb}
                  height={6}
                  segments={[
                    {
                      value: snapshot.memory.swapUsedKb,
                      className: "is-swap",
                      label: "Swap 已用",
                    },
                  ]}
                />
              </div>
            ) : (
              <p className="viz-card__meta">未启用 swap</p>
            )}
          </section>

          {/* 网络：双曲线 + 每张网卡的当前速率 */}
          <section className="viz-card">
            <header className="viz-card__head">
              <span className="viz-card__name">网络吞吐</span>
              <span className="viz-card__meta">
                {(snapshot.windowMs / 1000).toFixed(1)}s 窗口 · 全部网卡
              </span>
            </header>

            <Chart values={rxSeries} secondary={txSeries} max={netMax} height={64} />

            <div className="viz-legend">
              <span className="viz-legend__item">
                <i className="viz-legend__dot is-rx" />
                下行 {formatRate(rxSeries.length > 0 ? rxSeries[rxSeries.length - 1] : 0)}
              </span>
              <span className="viz-legend__item">
                <i className="viz-legend__dot is-tx" />
                上行 {formatRate(txSeries.length > 0 ? txSeries[txSeries.length - 1] : 0)}
              </span>
              <span className="viz-legend__item viz-legend__item--dim">
                上限 {formatRate(netMax)}
              </span>
            </div>

            {selectedNets.size > 0 ? (
              <p className="viz-card__meta">明细只看勾选的网卡，在「高级」面板里改</p>
            ) : null}
            {visibleNets.length === 0 ? (
              <p className="viz-card__meta">
                {snapshot.nets.length === 0
                  ? "读不到网卡（/proc/net/dev 为空）"
                  : "勾选的网卡当前没有数据"}
              </p>
            ) : (
              <ul className="viz-rows">
                {visibleNets.map((net) => (
                  <li className="viz-row" key={net.name}>
                    <span className="viz-row__name" title={net.name}>
                      {net.name}
                    </span>
                    <span className="viz-row__rate">↓ {formatRate(net.rxBps)}</span>
                    <span className="viz-row__rate">↑ {formatRate(net.txBps)}</span>
                  </li>
                ))}
              </ul>
            )}
          </section>

          {/* 磁盘：每个挂载点一条细条，百分比按阈值着色 */}
          <section className="viz-card">
            <header className="viz-card__head">
              <span className="viz-card__name">磁盘</span>
              <span className="viz-card__meta">{snapshot.disks.length} 个挂载点</span>
            </header>
            {snapshot.disks.length === 0 ? (
              <p className="viz-card__meta">读不到挂载点（可能不是 Linux）</p>
            ) : (
              <ul className="viz-rows">
                {snapshot.disks.map((disk) => (
                  <li className="viz-disk" key={`${disk.filesystem}-${disk.mount}`}>
                    <div className="viz-disk__head">
                      <span className="viz-row__name" title={disk.filesystem}>
                        {disk.mount}
                      </span>
                      <span className="viz-disk__meta">
                        {disk.used} / {disk.size}
                      </span>
                      <span className={`viz-disk__pct ${levelOf(disk.percent)}`}>
                        {disk.percent}%
                      </span>
                    </div>
                    <StackBar
                      total={100}
                      height={5}
                      segments={[
                        {
                          value: clampPercent(disk.percent),
                          className: `is-disk ${levelOf(disk.percent)}`,
                          label: `${disk.mount} 已用`,
                        },
                      ]}
                    />
                  </li>
                ))}
              </ul>
            )}
          </section>

          {/* 负载均值：三个数 + 1 分钟趋势 */}
          <section className="viz-card">
            <header className="viz-card__head">
              <span className="viz-card__name">负载均值</span>
              <span className="viz-card__meta">
                {snapshot.running} 个可运行 / 共 {snapshot.totalProcs} 进程
              </span>
            </header>
            <div className="viz-triple">
              {[
                ["1 分钟", snapshot.load1],
                ["5 分钟", snapshot.load5],
                ["15 分钟", snapshot.load15],
              ].map(([label, value]) => (
                <div className="viz-triple__item" key={String(label)}>
                  <span className="viz-triple__num">{Number(value).toFixed(2)}</span>
                  <span className="viz-triple__label">{label}</span>
                </div>
              ))}
            </div>
            <Chart values={loadSeries} max={loadMax} height={40} />
            <p className="viz-card__meta">{loadAdvice(snapshot.load1, snapshot.cpu.coresCount)}</p>
          </section>

          {/* 进程：CPU 条一眼看出谁在吃 */}
          <section className="viz-card">
            <header className="viz-card__head">
              <span className="viz-card__name">CPU 占用前 5</span>
              <span className="viz-card__meta">PID / 名称 / CPU / 内存</span>
            </header>
            {snapshot.processes.length === 0 ? (
              <p className="viz-card__meta">读不到进程列表</p>
            ) : (
              <ul className="viz-rows">
                {snapshot.processes.map((proc) => (
                  <li className="viz-proc" key={`${proc.pid}-${proc.name}`}>
                    <span className="viz-row__pid">{proc.pid}</span>
                    <span className="viz-row__name" title={proc.name}>
                      {proc.name}
                    </span>
                    <span className="viz-proc__bar">
                      <span
                        className={`viz-proc__fill ${levelOf(proc.cpu)}`}
                        style={{ width: `${clampPercent(proc.cpu)}%` }}
                      />
                    </span>
                    <span className="viz-row__num">{proc.cpu.toFixed(1)}%</span>
                    <span className="viz-row__num viz-row__num--dim">{proc.mem.toFixed(1)}%</span>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="viz-meta">
            <div>
              <span className="viz-meta__label">运行时长</span>
              <span className="viz-meta__value">{formatUptime(snapshot.uptimeSecs)}</span>
            </div>
            <div>
              <span className="viz-meta__label">登录会话</span>
              <span className="viz-meta__value">{snapshot.sessions}</span>
            </div>
            <div>
              <span className="viz-meta__label">TCP 已建立</span>
              <span className="viz-meta__value">{snapshot.tcpEstablished}</span>
            </div>
            <div>
              <span className="viz-meta__label">内核</span>
              <span className="viz-meta__value" title={snapshot.kernel}>
                {snapshot.kernel.replace(/^Linux\s+/, "") || "—"}
              </span>
            </div>
          </section>

          {state.at ? (
            <p className="term-load__foot">
              采样于 {new Date(state.at).toLocaleTimeString("zh-CN", { hour12: false })}
              {state.loading ? " · 刷新中…" : ""}
            </p>
          ) : null}
        </div>
      ) : null}
    </aside>
  );
}

/* ---------------------------------------------------------------- 图与小组件 */

/**
 * 面积图（可叠第二条曲线）。
 *
 * 自己画 SVG，不引图表库：几条时间序列不值得多 40KB。
 * `preserveAspectRatio="none"` 让图随面板宽度拉伸，所以描边必须
 * `vector-effect="non-scaling-stroke"` —— 否则窄面板里线变粗、宽面板里细如发丝。
 */
function Chart({
  values,
  secondary,
  max,
  height,
  thresholds = [],
}: {
  values: number[];
  /** 第二条曲线（网络上下行） */
  secondary?: number[];
  max: number;
  height: number;
  /** 参考线（CPU 的 70% / 90%） */
  thresholds?: number[];
}) {
  const uid = useId().replace(/:/g, "");
  const width = 300;

  const primary = areaPath(values, width, height, max);
  const second = secondary ? areaPath(secondary, width, height, max) : null;

  return (
    <div className="viz-chart" style={{ height }}>
      <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" aria-hidden="true">
        {[0.25, 0.5, 0.75].map((ratio) => (
          <line
            key={ratio}
            className="viz-chart__grid"
            x1={0}
            x2={width}
            y1={height * ratio}
            y2={height * ratio}
            vectorEffect="non-scaling-stroke"
          />
        ))}

        {thresholds.map((value) => (
          <line
            key={value}
            className="viz-chart__threshold"
            x1={0}
            x2={width}
            y1={height - (value / max) * height}
            y2={height - (value / max) * height}
            vectorEffect="non-scaling-stroke"
          />
        ))}

        {second ? (
          <g className="viz-chart__series viz-chart__series--secondary">
            <defs>
              <linearGradient id={`${uid}-fill2`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="currentColor" stopOpacity="0.18" />
                <stop offset="100%" stopColor="currentColor" stopOpacity="0.01" />
              </linearGradient>
            </defs>
            <path d={second.area} fill={`url(#${uid}-fill2)`} />
            <path d={second.line} className="viz-chart__line" vectorEffect="non-scaling-stroke" />
          </g>
        ) : null}

        {primary ? (
          <g className="viz-chart__series viz-chart__series--primary">
            <defs>
              <linearGradient id={`${uid}-fill`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="currentColor" stopOpacity="0.28" />
                <stop offset="100%" stopColor="currentColor" stopOpacity="0.01" />
              </linearGradient>
            </defs>
            <path d={primary.area} fill={`url(#${uid}-fill)`} />
            <path d={primary.line} className="viz-chart__line" vectorEffect="non-scaling-stroke" />
          </g>
        ) : (
          <text className="viz-chart__empty" x={width / 2} y={height / 2} textAnchor="middle">
            等待第二个采样点
          </text>
        )}
      </svg>
    </div>
  );
}

/** KPI 小卡：大数字 + 迷你趋势。 */
function Kpi({
  label,
  value,
  unit,
  series,
  max,
  tone,
}: {
  label: string;
  value: string;
  unit?: string;
  series: number[];
  max: number;
  tone: string;
}) {
  const geometry = areaPath(series, 60, 20, max);
  return (
    <div className={`viz-kpi__cell ${tone}`}>
      <span className="viz-kpi__label">{label}</span>
      <span className="viz-kpi__value">
        {value}
        {unit ? <em>{unit}</em> : null}
      </span>
      <svg
        className="viz-kpi__spark"
        viewBox="0 0 60 20"
        preserveAspectRatio="none"
        aria-hidden="true"
      >
        {geometry ? (
          <>
            <path d={geometry.area} className="viz-kpi__area" />
            <path d={geometry.line} className="viz-kpi__line" vectorEffect="non-scaling-stroke" />
          </>
        ) : null}
      </svg>
    </div>
  );
}

/** 分段条：内存、磁盘这类"占了多少"的指标用它，比分数字直观。 */
function StackBar({
  total,
  segments,
  height = 8,
}: {
  total: number;
  segments: { value: number; className: string; label: string }[];
  height?: number;
}) {
  const safeTotal = total > 0 ? total : 1;
  return (
    <div className="viz-stack" style={{ height }}>
      {segments.map((segment) => (
        <span
          key={segment.label}
          className={`viz-stack__fill ${segment.className}`}
          style={{ width: `${Math.min(100, (segment.value / safeTotal) * 100)}%` }}
          title={`${segment.label} ${((segment.value / safeTotal) * 100).toFixed(1)}%`}
        />
      ))}
    </div>
  );
}

function statsOf(values: number[]): { peak: number; avg: number } {
  if (values.length === 0) return { peak: 0, avg: 0 };
  let peak = 0;
  let sum = 0;
  for (const value of values) {
    if (value > peak) peak = value;
    sum += value;
  }
  return { peak, avg: sum / values.length };
}
