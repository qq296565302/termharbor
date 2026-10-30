import { useCallback, useEffect, useRef, useState } from "react";

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

/** 自动刷新间隔。3 秒足够看出趋势，又不至于把服务器刷烦。 */
const REFRESH_MS = 3000;
/** CPU 历史保留多少个点（3 秒一个 → 约 2 分钟）。 */
const HISTORY_MAX = 40;

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
  /** CPU 使用率历史，用来画右侧那条小折线 */
  const [history, setHistory] = useState<number[]>([]);

  /** 防止慢请求叠起来：3 秒一次，但一次采样要 0.5 秒 + 往返 */
  const inFlight = useRef(false);

  const refresh = useCallback(async () => {
    if (!sessionId || inFlight.current) return;

    inFlight.current = true;
    setState((prev) => ({ ...prev, loading: true }));
    try {
      const snapshot = await call<LoadSnapshot>("load_snapshot", { sessionId });
      setState({ snapshot, error: null, loading: false, at: Date.now() });
      setHistory((prev) => [...prev, snapshot.cpu.percent].slice(-HISTORY_MAX));
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
    setHistory([]);
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
          {auto ? "自动" : "暂停"}
        </button>
        <button
          type="button"
          className="term-load__refresh"
          title="立即刷新"
          disabled={!sessionId || state.loading}
          onClick={() => void refresh()}
        >
          ↻
        </button>
        <button type="button" className="term-load__close" title="关闭看板" onClick={onClose}>
          ×
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
          <section className="load-card">
            <header className="load-card__head">
              <span>CPU</span>
              <span className="load-card__value">{snapshot.cpu.percent.toFixed(1)}%</span>
            </header>
            <Sparkline points={history} />
            <div className="load-cores">
              {snapshot.cpu.cores.map((core, index) => (
                <div className="load-core" key={index} title={`cpu${index} ${core.toFixed(0)}%`}>
                  <span className="load-core__bar">
                    <span
                      className={`load-core__fill ${levelOf(core)}`}
                      style={{ width: `${clampPercent(core)}%` }}
                    />
                  </span>
                  <span className="load-core__label">c{index}</span>
                </div>
              ))}
              {snapshot.cpu.coresCount === 0 ? (
                <span className="load-card__hint">读不到分核数据</span>
              ) : null}
            </div>
          </section>

          <section className="load-card">
            <header className="load-card__head">
              <span>内存</span>
              <span className="load-card__value">
                {percentText(snapshot.memory.percent)}
              </span>
            </header>
            <Bar percent={snapshot.memory.percent} />
            <p className="load-card__line">
              已用 {formatKb(snapshot.memory.usedKb)} / 共 {formatKb(snapshot.memory.totalKb)}
            </p>
            <p className="load-card__hint">
              可用 {formatKb(snapshot.memory.availableKb)}（含缓存 {formatKb(snapshot.memory.cachedKb)}、
              缓冲 {formatKb(snapshot.memory.buffersKb)}）
            </p>

            {snapshot.memory.swapTotalKb > 0 ? (
              <>
                <header className="load-card__head load-card__head--sub">
                  <span>Swap</span>
                  <span className="load-card__value">
                    {percentText(snapshot.memory.swapPercent)}
                  </span>
                </header>
                <Bar percent={snapshot.memory.swapPercent} />
                <p className="load-card__line">
                  已用 {formatKb(snapshot.memory.swapUsedKb)} / 共 {formatKb(snapshot.memory.swapTotalKb)}
                </p>
              </>
            ) : (
              <p className="load-card__hint">没有启用 swap</p>
            )}
          </section>

          <section className="load-card">
            <header className="load-card__head">
              <span>负载均值</span>
              <span className="load-card__hint">
                {snapshot.running} 个可运行 / 共 {snapshot.totalProcs} 进程
              </span>
            </header>
            <div className="load-triple">
              {[
                ["1 分钟", snapshot.load1],
                ["5 分钟", snapshot.load5],
                ["15 分钟", snapshot.load15],
              ].map(([label, value]) => (
                <div className="load-triple__item" key={String(label)}>
                  <span className="load-triple__num">{Number(value).toFixed(2)}</span>
                  <span className="load-triple__label">{label}</span>
                </div>
              ))}
            </div>
            <p className="load-card__hint">
              {loadAdvice(snapshot.load1, snapshot.cpu.coresCount)}
            </p>
          </section>

          <section className="load-card">
            <header className="load-card__head">
              <span>磁盘</span>
              <span className="load-card__hint">{snapshot.disks.length} 个挂载点</span>
            </header>
            {snapshot.disks.length === 0 ? (
              <p className="load-card__hint">读不到挂载点（可能不是 Linux）</p>
            ) : (
              <ul className="load-list">
                {snapshot.disks.map((disk) => (
                  <li className="load-row" key={`${disk.filesystem}-${disk.mount}`}>
                    <span className="load-row__name" title={disk.filesystem}>
                      {disk.mount}
                    </span>
                    <span className="load-row__bar">
                      <span
                        className={`load-row__fill ${levelOf(disk.percent)}`}
                        style={{ width: `${clampPercent(disk.percent)}%` }}
                      />
                    </span>
                    <span className="load-row__meta">
                      {disk.used} / {disk.size}
                    </span>
                    <span className="load-row__num">{disk.percent}%</span>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="load-card">
            <header className="load-card__head">
              <span>网络</span>
              <span className="load-card__hint">{(snapshot.windowMs / 1000).toFixed(1)} 秒窗口</span>
            </header>
            {snapshot.nets.length === 0 ? (
              <p className="load-card__hint">读不到网卡（/proc/net/dev 为空）</p>
            ) : (
              <ul className="load-list">
                {snapshot.nets.map((net) => (
                  <li className="load-row load-row--net" key={net.name}>
                    <span className="load-row__name">{net.name}</span>
                    <span className="load-row__rate">↓ {formatRate(net.rxBps)}</span>
                    <span className="load-row__rate">↑ {formatRate(net.txBps)}</span>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="load-card">
            <header className="load-card__head">
              <span>CPU 占用前 5</span>
              <span className="load-card__hint">PID / 名称 / CPU / 内存</span>
            </header>
            {snapshot.processes.length === 0 ? (
              <p className="load-card__hint">读不到进程列表</p>
            ) : (
              <ul className="load-list">
                {snapshot.processes.map((proc) => (
                  <li className="load-row load-row--proc" key={`${proc.pid}-${proc.name}`}>
                    <span className="load-row__pid">{proc.pid}</span>
                    <span className="load-row__name" title={proc.name}>
                      {proc.name}
                    </span>
                    <span className="load-row__num">{proc.cpu.toFixed(1)}%</span>
                    <span className="load-row__num load-row__num--dim">
                      {proc.mem.toFixed(1)}%
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="load-card load-card--meta">
            <div>
              <span className="load-meta__label">运行时长</span>
              <span className="load-meta__value">{formatUptime(snapshot.uptimeSecs)}</span>
            </div>
            <div>
              <span className="load-meta__label">登录会话</span>
              <span className="load-meta__value">{snapshot.sessions}</span>
            </div>
            <div>
              <span className="load-meta__label">TCP 已建立</span>
              <span className="load-meta__value">{snapshot.tcpEstablished}</span>
            </div>
            <div>
              <span className="load-meta__label">内核</span>
              <span className="load-meta__value" title={snapshot.kernel}>
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

/** CPU 历史折线。自己画 SVG，不引图表库：一条 polyline 不值得多 40KB。 */
function Sparkline({ points }: { points: number[] }) {
  if (points.length < 2) {
    return <div className="load-spark load-spark--empty">等待更多采样…</div>;
  }

  // viewBox 固定 100x28，靠 preserveAspectRatio="none" 拉满宽度
  const step = 100 / (HISTORY_MAX - 1);
  const offset = 100 - (points.length - 1) * step;
  const coords = points
    .map((value, index) => {
      const x = offset + index * step;
      // y 反过来：0% 在底部，100% 在顶部，留 2px 边距
      const y = 26 - (clampPercent(value) / 100) * 24;
      return `${x.toFixed(2)},${y.toFixed(2)}`;
    })
    .join(" ");

  return (
    <svg className="load-spark" viewBox="0 0 100 28" preserveAspectRatio="none" aria-hidden="true">
      <polyline className="load-spark__line" points={coords} />
    </svg>
  );
}

function Bar({ percent }: { percent: number }) {
  return (
    <span className="load-bar">
      <span
        className={`load-bar__fill ${levelOf(percent)}`}
        style={{ width: `${clampPercent(percent)}%` }}
      />
    </span>
  );
}

/** 百分比统一一位小数：看板上不需要更细的精度，位数统一才好对齐。 */
function percentText(value: number): string {
  return `${value.toFixed(1)}%`;
}
