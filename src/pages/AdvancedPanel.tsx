import "./advanced-panel.css";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import {
  filterConnections,
  filterProcesses,
  formatBytes,
  formatKb,
  formatUptime,
  parseInterfaces,
  sortProcesses,
  toggleInterface,
} from "../lib/advancedFormat";
import type { ConnectionKind, ProcessSort } from "../lib/advancedFormat";
import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import type {
  AdvancedSnapshot,
  NetConnection,
  NetInterfaceInfo,
  ProcessInfo,
  SystemInfo,
} from "../lib/types";
import { useSettingsStore } from "../stores/settingsStore";

/** 对外契约，保持与集成方约定的形状一致。 */
export interface AdvancedPanelProps {
  sessionId: string;
  onClose: () => void;
}

/** 自动刷新间隔。5 秒一次，比负载看板慢：一次要采齐四类数据。 */
const REFRESH_MS = 5000;

type TabKey = "system" | "interfaces" | "processes" | "connections";

const TABS: { key: TabKey; label: string }[] = [
  { key: "system", label: "系统信息" },
  { key: "interfaces", label: "网络接口" },
  { key: "processes", label: "进程" },
  { key: "connections", label: "端口与连接" },
];

interface State {
  snapshot: AdvancedSnapshot | null;
  error: string | null;
  loading: boolean;
  /** 上次成功采样的时刻（本地时间） */
  at: number | null;
}

/**
 * 高级监控侧栏。
 *
 * 数据由一次 `advanced_snapshot` 全部采回，四个 tab 都只是同一份快照的
 * 不同视图 —— 所以切 tab 只是改本地状态，不重新打 IPC。
 * 刷新失败时保留上一次成功的快照，只在顶部挂一条错误（抖一下就把整块
 * 面板抹白，比看到旧数据更难受）。
 */
export default function AdvancedPanel({ sessionId, onClose }: AdvancedPanelProps): JSX.Element {
  const [state, setState] = useState<State>({
    snapshot: null,
    error: null,
    loading: false,
    at: null,
  });
  const [auto, setAuto] = useState(true);
  const [tab, setTab] = useState<TabKey>("system");

  /** 防止慢请求叠起来：一次采样要往返，5 秒里可能没回来。 */
  const inFlight = useRef(false);

  const settings = useSettingsStore((s) => s.settings);
  const preview = useSettingsStore((s) => s.preview);
  const commit = useSettingsStore((s) => s.commit);

  const refresh = useCallback(async () => {
    if (!sessionId || inFlight.current) return;

    inFlight.current = true;
    setState((prev) => ({ ...prev, loading: true }));
    try {
      const snapshot = await call<AdvancedSnapshot>("advanced_snapshot", { sessionId });
      setState({ snapshot, error: null, loading: false, at: Date.now() });
    } catch (err) {
      // 保留旧的 snapshot：失败不清空，界面继续显示上一次的数据
      setState((prev) => ({
        ...prev,
        loading: false,
        error: fromUnknown(err).message,
      }));
    } finally {
      inFlight.current = false;
    }
  }, [sessionId]);

  // 换标签（sessionId 变化）立刻重采一次，那是另一台机器的数据
  useEffect(() => {
    setState({ snapshot: null, error: null, loading: false, at: null });
    if (sessionId) void refresh();
  }, [sessionId, refresh]);

  // 页面被切到后台时停采：用户看不见的地方不该偷偷往服务器打 exec
  const [windowVisible, setWindowVisible] = useState(() => !document.hidden);
  useEffect(() => {
    const handler = () => setWindowVisible(!document.hidden);
    document.addEventListener("visibilitychange", handler);
    return () => document.removeEventListener("visibilitychange", handler);
  }, []);

  useEffect(() => {
    if (!auto || !sessionId || !windowVisible) return;
    const timer = window.setInterval(() => void refresh(), REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [auto, refresh, sessionId, windowVisible]);

  const snapshot = state.snapshot;
  const timeText = state.at
    ? new Date(state.at).toLocaleTimeString("zh-CN", { hour12: false })
    : "--:--:--";

  return (
    <aside className="adv">
      <header className="adv__head">
        <span className="adv__title">高级监控</span>
        <span className="adv__spacer" />
        <span className="adv__updated">最近更新 {timeText}</span>
        <button
          type="button"
          className={`btn btn--ghost btn--small adv__auto ${auto ? "is-on" : ""}`}
          title={auto ? "每 5 秒自动刷新（点击暂停）" : "已暂停，点击恢复自动刷新"}
          onClick={() => setAuto((value) => !value)}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M5 3.4v9.2M11 3.4v9.2" />
          </svg>
          {auto ? "自动" : "已暂停"}
        </button>
        <button
          type="button"
          className="btn btn--ghost btn--small adv__icon-btn"
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
          className="btn btn--ghost btn--small adv__icon-btn"
          title="关闭"
          aria-label="关闭"
          onClick={onClose}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M4.2 4.2l7.6 7.6M11.8 4.2l-7.6 7.6" />
          </svg>
        </button>
      </header>

      <nav className="adv__tabs" role="tablist" aria-label="高级监控分类">
        {TABS.map((item) => (
          <button
            key={item.key}
            type="button"
            role="tab"
            aria-selected={tab === item.key}
            className={`adv__tab ${tab === item.key ? "is-active" : ""}`}
            onClick={() => setTab(item.key)}
          >
            {item.label}
          </button>
        ))}
      </nav>

      <div className="adv__body">
        {state.error ? (
          <div className="adv__error">
            <span>{state.error}</span>
            <button type="button" className="btn btn--ghost btn--small" onClick={() => void refresh()}>
              重试
            </button>
          </div>
        ) : null}

        {/* warnings 走淡色提示条，不当错误弹窗 */}
        {snapshot && snapshot.warnings.length > 0 ? (
          <div className="adv__warn" role="status">
            {snapshot.warnings.map((warning, index) => (
              <span key={`${index}-${warning}`}>{warning}</span>
            ))}
          </div>
        ) : null}

        {!snapshot && state.loading ? (
          <div className="adv__skeleton" aria-busy="true">
            <span className="adv__skel-line" />
            <span className="adv__skel-line" />
            <span className="adv__skel-line" />
            <p className="adv__hint">正在采集…</p>
          </div>
        ) : null}

        {!snapshot && !state.loading && !state.error ? (
          <p className="adv__hint">等待数据…</p>
        ) : null}

        {snapshot ? (
          <>
            {tab === "system" ? <SystemTab info={snapshot.info} /> : null}

            {tab === "interfaces" ? (
              <InterfacesTab
                interfaces={snapshot.interfaces}
                monitorInterfaces={settings.monitorInterfaces}
                onToggle={(name) => {
                  // 读 store 的实时值，避免连点两次时第二次还基于上一次 render 的旧快照
                  const current = useSettingsStore.getState().settings.monitorInterfaces;
                  const next = toggleInterface(current, name);
                  preview({ monitorInterfaces: next });
                  void commit();
                }}
              />
            ) : null}

            {tab === "processes" ? (
              <ProcessesTab
                processes={snapshot.processes}
                sessionId={sessionId}
                onKilled={() => void refresh()}
              />
            ) : null}

            {tab === "connections" ? <ConnectionsTab connections={snapshot.connections} /> : null}
          </>
        ) : null}
      </div>
    </aside>
  );
}

/* ------------------------------------------------------------------ 表格骨架 */

interface Column {
  key: string;
  label: string;
  /** colgroup 用的固定列宽；所有列都写死，表头才不会错位 */
  width: string;
}

function DataTable({
  columns,
  minWidth,
  children,
}: {
  columns: Column[];
  minWidth?: number;
  children: ReactNode;
}): JSX.Element {
  return (
    <div className="adv__table-wrap">
      <table
        className="adv__table"
        style={minWidth ? { minWidth: `${minWidth}px` } : undefined}
      >
        <colgroup>
          {columns.map((c) => (
            <col key={c.key} style={{ width: c.width }} />
          ))}
        </colgroup>
        <thead>
          <tr>
            {columns.map((c) => (
              <th key={c.key} scope="col">{c.label}</th>
            ))}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

const DISK_COLUMNS: Column[] = [
  { key: "mount", label: "挂载点", width: "26%" },
  { key: "size", label: "容量", width: "20%" },
  { key: "used", label: "已用", width: "18%" },
  { key: "avail", label: "可用", width: "18%" },
  { key: "use", label: "使用率", width: "18%" },
];

const IFACE_COLUMNS: Column[] = [
  { key: "check", label: "", width: "4%" },
  { key: "name", label: "名称", width: "11%" },
  { key: "ipv4", label: "IPv4", width: "15%" },
  { key: "ipv6", label: "IPv6", width: "19%" },
  { key: "mac", label: "MAC", width: "16%" },
  { key: "state", label: "状态", width: "8%" },
  { key: "bytes", label: "累计收发", width: "19%" },
  { key: "errors", label: "错误包", width: "8%" },
];

const PROC_COLUMNS: Column[] = [
  { key: "pid", label: "pid", width: "8%" },
  { key: "ppid", label: "ppid", width: "8%" },
  { key: "user", label: "user", width: "10%" },
  { key: "stat", label: "stat", width: "8%" },
  { key: "cpu", label: "CPU%", width: "8%" },
  { key: "mem", label: "内存", width: "9%" },
  { key: "elapsed", label: "运行时长", width: "11%" },
  { key: "command", label: "命令", width: "30%" },
  { key: "action", label: "", width: "8%" },
];

const CONN_COLUMNS: Column[] = [
  { key: "proto", label: "协议", width: "9%" },
  { key: "state", label: "状态", width: "13%" },
  { key: "local", label: "本地地址", width: "27%" },
  { key: "remote", label: "远端地址", width: "27%" },
  { key: "pid", label: "pid", width: "9%" },
  { key: "process", label: "进程名", width: "15%" },
];

/* ------------------------------------------------------------------ tab 1 */

function SystemTab({ info }: { info: SystemInfo }): JSX.Element {
  const rows: [string, string][] = [
    ["主机名", info.hostname],
    ["发行版", info.osName],
    ["内核", info.kernel],
    ["架构", info.arch],
    ["CPU 型号", info.cpuModel],
    ["CPU 核数", String(info.cpuCores)],
    ["运行时长", formatUptime(info.uptimeSec)],
    ["内存总量", formatKb(info.totalMemKb)],
    ["Swap 总量", formatKb(info.totalSwapKb)],
    ["负载均值", info.loadAvg.map((v) => v.toFixed(2)).join(" / ")],
    ["进程数", String(info.processCount)],
    ["登录用户数", String(info.loggedUsers)],
    ["虚拟化", info.virtualization],
  ];

  return (
    <section className="adv__section">
      <h3 className="adv__section-title">系统信息</h3>
      <table className="adv__table adv__kv">
        <colgroup>
          <col style={{ width: "38%" }} />
          <col style={{ width: "62%" }} />
        </colgroup>
        <tbody>
          {rows.map(([label, value]) => (
            <tr key={label}>
              <th scope="row" className="adv__kv-key">{label}</th>
              <td className="adv__kv-val" title={value}>{value || "—"}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <h3 className="adv__section-title">磁盘</h3>
      {info.disks.length === 0 ? (
        <p className="adv__hint">读不到挂载点</p>
      ) : (
        <DataTable columns={DISK_COLUMNS} minWidth={420}>
          {info.disks.map((disk) => (
            <tr key={`${disk.filesystem}-${disk.mount}`}>
              <td className="adv__cell--key" title={disk.mount}>{disk.mount}</td>
              <td>{formatKb(disk.sizeKb)}</td>
              <td>{formatKb(disk.usedKb)}</td>
              <td>{formatKb(disk.availKb)}</td>
              <td className={disk.usePercent >= 80 ? "adv__danger" : undefined}>
                {disk.usePercent}%
              </td>
            </tr>
          ))}
        </DataTable>
      )}
    </section>
  );
}

/* ------------------------------------------------------------------ tab 2 */

/** UP 用语义绿、DOWN 用红，其余状态不着色。 */
function stateClass(state: string): string {
  const value = state.toUpperCase();
  if (value === "UP") return "adv__ok";
  if (value === "DOWN") return "adv__danger";
  return "";
}

function InterfacesTab({
  interfaces,
  monitorInterfaces,
  onToggle,
}: {
  interfaces: NetInterfaceInfo[];
  monitorInterfaces: string;
  onToggle: (name: string) => void;
}): JSX.Element {
  const checked = useMemo(
    () => new Set(parseInterfaces(monitorInterfaces)),
    [monitorInterfaces],
  );

  return (
    <section className="adv__section">
      <h3 className="adv__section-title">网络接口</h3>
      {interfaces.length === 0 ? (
        <p className="adv__hint">读不到网卡</p>
      ) : (
        <DataTable columns={IFACE_COLUMNS} minWidth={760}>
          {interfaces.map((iface) => (
            <tr key={iface.name}>
              <td className="adv__cell--check">
                <input
                  type="checkbox"
                  checked={checked.has(iface.name)}
                  aria-label={`监控 ${iface.name}`}
                  onChange={() => onToggle(iface.name)}
                />
              </td>
              <td className="adv__cell--key" title={iface.name}>{iface.name}</td>
              <td title={iface.ipv4}>{iface.ipv4 || "—"}</td>
              <td title={iface.ipv6}>{iface.ipv6 || "—"}</td>
              <td title={iface.mac}>{iface.mac || "—"}</td>
              <td className={stateClass(iface.state)}>{iface.state || "—"}</td>
              <td>↓ {formatBytes(iface.rxBytes)} ↑ {formatBytes(iface.txBytes)}</td>
              <td>↓ {iface.rxErrors} ↑ {iface.txErrors}</td>
            </tr>
          ))}
        </DataTable>
      )}
      <p className="adv__hint">
        勾选的网卡会在负载看板里单独显示速率。空选 = 全部显示。
      </p>
    </section>
  );
}

/* ------------------------------------------------------------------ tab 3 */

function ProcessesTab({
  processes,
  sessionId,
  onKilled,
}: {
  processes: ProcessInfo[];
  sessionId: string;
  onKilled: () => void;
}): JSX.Element {
  const [query, setQuery] = useState("");
  const [sortBy, setSortBy] = useState<ProcessSort>("cpu");
  /** 非 null 时展开内联确认条；结束进程必须先二次确认 */
  const [confirm, setConfirm] = useState<ProcessInfo | null>(null);
  const [killing, setKilling] = useState(false);
  const [killError, setKillError] = useState<string | null>(null);

  // 完整列表，不分页、不加前端条数上限；滚动交给 .adv__table-wrap 的 max-height
  const shown = useMemo(
    () => sortProcesses(filterProcesses(processes, query), sortBy),
    [processes, query, sortBy],
  );

  const doKill = useCallback(async () => {
    if (!confirm || killing) return;
    setKilling(true);
    setKillError(null);
    try {
      await call<string>("advanced_kill", { sessionId, pid: confirm.pid });
      setConfirm(null);
      // 成功后立刻重拉一次快照，让列表马上反映进程已退出
      onKilled();
    } catch (err) {
      setKillError(fromUnknown(err).message);
    } finally {
      setKilling(false);
    }
  }, [confirm, killing, onKilled, sessionId]);

  return (
    <section className="adv__section adv__section--fill">
      <div className="adv__toolbar">
        <input
          className="adv__search"
          type="search"
          placeholder="搜索命令名 / pid / 用户"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          aria-label="搜索进程"
        />
        <label className="adv__sort">
          <span>排序</span>
          <select
            value={sortBy}
            onChange={(event) => setSortBy(event.target.value as ProcessSort)}
          >
            <option value="cpu">CPU</option>
            <option value="mem">内存</option>
            <option value="pid">pid</option>
          </select>
        </label>
      </div>

      {confirm ? (
        <div className="adv__confirm" role="alertdialog" aria-label="确认结束进程">
          <span className="adv__confirm-text">
            结束进程 <b>{confirm.pid}</b>（{confirm.command || "无命令"}）？将发送 TERM 信号。
          </span>
          <button
            type="button"
            className="btn btn--danger btn--small"
            disabled={killing}
            onClick={() => void doKill()}
          >
            {killing ? "处理中" : "确认结束"}
          </button>
          <button
            type="button"
            className="btn btn--ghost btn--small"
            disabled={killing}
            onClick={() => {
              setConfirm(null);
              setKillError(null);
            }}
          >
            取消
          </button>
        </div>
      ) : null}

      {killError ? <p className="adv__error-inline">{killError}</p> : null}
      {!confirm && !killError ? (
        <p className="adv__hint adv__hint--count">共 {shown.length} 个进程</p>
      ) : null}

      <DataTable columns={PROC_COLUMNS} minWidth={720}>
        {shown.map((proc) => (
          <tr key={proc.pid}>
            <td className="adv__cell--num">{proc.pid}</td>
            <td className="adv__cell--num">{proc.ppid}</td>
            <td title={proc.user}>{proc.user}</td>
            <td title={proc.stat}>{proc.stat}</td>
            <td className="adv__cell--num">{proc.cpuPercent.toFixed(1)}</td>
            <td className="adv__cell--num" title={formatKb(proc.memKb)}>
              {proc.memPercent.toFixed(1)}%
            </td>
            <td className="adv__cell--nowrap">{proc.elapsed}</td>
            <td className="adv__cell--command" title={proc.command}>{proc.command}</td>
            <td className="adv__cell--action">
              <button
                type="button"
                className="btn btn--ghost btn--small adv__icon-btn"
                title="结束进程"
                aria-label={`结束进程 ${proc.pid}`}
                onClick={() => {
                  setConfirm(proc);
                  setKillError(null);
                }}
              >
                <svg viewBox="0 0 16 16" aria-hidden="true">
                  <path d="M4 4l8 8M12 4l-8 8" />
                </svg>
              </button>
            </td>
          </tr>
        ))}
      </DataTable>
    </section>
  );
}

/* ------------------------------------------------------------------ tab 4 */

const KIND_OPTIONS: { key: ConnectionKind; label: string }[] = [
  { key: "all", label: "全部" },
  { key: "listen", label: "仅监听" },
  { key: "established", label: "已建立" },
];

function ConnectionsTab({ connections }: { connections: NetConnection[] }): JSX.Element {
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<ConnectionKind>("all");

  const shown = useMemo(
    () => filterConnections(connections, query, kind),
    [connections, query, kind],
  );

  return (
    <section className="adv__section adv__section--fill">
      <div className="adv__toolbar">
        <div className="adv__segment" role="group" aria-label="连接筛选">
          {KIND_OPTIONS.map((option) => (
            <button
              key={option.key}
              type="button"
              className={`adv__segment-btn ${kind === option.key ? "is-active" : ""}`}
              onClick={() => setKind(option.key)}
            >
              {option.label}
            </button>
          ))}
        </div>
        <input
          className="adv__search"
          type="search"
          placeholder="搜索地址 / 进程 / pid"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          aria-label="搜索连接"
        />
      </div>
      <p className="adv__hint adv__hint--count">共 {shown.length} 条连接</p>

      <DataTable columns={CONN_COLUMNS} minWidth={620}>
        {shown.map((cxn, index) => (
          <tr
            key={`${cxn.proto}-${cxn.localAddr}-${cxn.remoteAddr}-${index}`}
            className={cxn.kind === "listen" ? "adv__row--listen" : undefined}
          >
            <td>{cxn.proto}</td>
            <td title={cxn.state}>{cxn.state || "—"}</td>
            <td title={cxn.localAddr}>{cxn.localAddr}</td>
            <td title={cxn.remoteAddr}>{cxn.remoteAddr || "—"}</td>
            <td className="adv__cell--num">{cxn.pid || "—"}</td>
            <td title={cxn.process}>{cxn.process || "—"}</td>
          </tr>
        ))}
      </DataTable>
    </section>
  );
}
