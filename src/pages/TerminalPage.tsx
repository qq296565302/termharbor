import { Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useCallback, useEffect, useRef, useState } from "react";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import { nextTabId } from "../lib/tabs";
import SnippetPanel from "./SnippetPanel";
import type { SnippetDto } from "../lib/types";

/** 插入时只关心名称和风险，不需要整条片段 */
type SnippetMeta = Pick<SnippetDto, "name" | "risk">;
import type {
  ConnectionDto,
  DiagKind,
  DiagResult,
  OutputBatch,
  SessionInfo,
  SshStateEvent,
} from "../lib/types";
import DiagPanel from "./DiagPanel";
import { TERM_THEMES } from "./SettingsPage";
import { useSessionStore, type TerminalTab } from "../stores/sessionStore";
import { useSettingsStore } from "../stores/settingsStore";
import { useUiStore } from "../stores/uiStore";


/**
 * 每个标签页一个终端实例。切换标签只切可见性，绝不销毁 —— 销毁会丢回滚缓冲。
 *
 * 重连语义（开发计划 §14-3）：**复用标签、新开会话、明确告知**。
 * 终端实例不销毁，旧的回滚内容保留，中间插一条提示，避免用户以为还是原来那个会话。
 */
function TerminalView({
  tab,
  active,
  focusTick,
}: {
  tab: TerminalTab;
  active: boolean;
  /** 父组件插入片段后 +1，用来把焦点还给终端 */
  focusTick: number;
}) {
  const fontSize = useSettingsStore((state) => state.settings.fontSize);
  const themeName = useSettingsStore((state) => state.settings.theme);
  const scrollback = useSettingsStore((state) => state.settings.scrollback);

  const hostRef = useRef<HTMLDivElement>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const sessionRef = useRef<string | null>(null);
  const reconnectRef = useRef<(() => void) | null>(null);
  /** 选中文字后浮出的复制按钮。坐标相对 `.term-host`。 */
  const [copyHint, setCopyHint] = useState<{
    x: number;
    y: number;
    below: boolean;
  } | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    let disposed = false;
    let pendingAck = 0;

    const term = new Terminal({
      fontFamily: 'ui-monospace, SFMono-Regular, Consolas, "Microsoft YaHei", monospace',
      fontSize,
      cursorBlink: true,
      scrollback,
      theme: TERM_THEMES[themeName],
    });
    termRef.current = term;
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    try {
      fit.fit();
    } catch {
      // 容器还没完成布局，切到该标签时会再 fit 一次
    }
    fitRef.current = fit;

    // 按真实字节数回 ack：JS 的 string.length 是 UTF-16 码元数，中文会少算
    const flushAck = () => {
      const id = sessionRef.current;
      if (!id || pendingAck <= 0) return;
      const bytes = pendingAck;
      pendingAck = 0;
      void call("session_ack", { sessionId: id, bytes }).catch(() => undefined);
    };
    const ackTimer = setInterval(flushAck, 50);

    const startSession = () => {
      const store = useSessionStore.getState();
      store.markStatus(tab.tabId, "connecting");

      const channel = new Channel<OutputBatch>();
      channel.onmessage = (batch) => {
        // 用 write 的回调统计"真的被终端吃掉了"，背压才是端到端的
        term.write(batch.data, () => {
          pendingAck += batch.bytes;
        });
      };

      void (async () => {
        try {
          const info = await call<SessionInfo>("session_open", {
            connectionId: tab.connectionId,
            cols: term.cols,
            rows: term.rows,
            trustFingerprint: null,
            channel,
          });
          if (disposed) {
            void call("session_close", { sessionId: info.sessionId }).catch(() => undefined);
            return;
          }
          sessionRef.current = info.sessionId;
          useSessionStore.getState().markOpened(tab.tabId, info.sessionId);
        } catch (err) {
          if (disposed) return;
          useSessionStore.getState().markStatus(tab.tabId, "failed", fromUnknown(err).message);
        }
      })();
    };

    reconnectRef.current = () => {
      const old = sessionRef.current;
      sessionRef.current = null;
      if (old) void call("session_close", { sessionId: old }).catch(() => undefined);

      term.writeln("");
      term.writeln("\x1b[33m── 正在重新连接（这会是一个新会话）──\x1b[0m");
      try {
        fit.fit();
      } catch {
        // 忽略
      }
      startSession();
    };

    startSession();

    const dataSub = term.onData((data) => {
      const id = sessionRef.current;
      if (id) void call("session_write", { sessionId: id, data }).catch(() => undefined);
    });

    const resizeSub = term.onResize(({ cols, rows }) => {
      const id = sessionRef.current;
      if (id) void call("session_resize", { sessionId: id, cols, rows }).catch(() => undefined);
    });

    // 选中文字后浮出「复制」按钮。
    //
    // 定位用 mouseup 的坐标而不是选区坐标：xterm 不公开选区像素位置
    // （getSelectionPosition 给的是 buffer 行列，换算要碰私有 API）。
    // 拖拽过程中不显示，避免按钮跟着移动、挡住正在选的内容。
    const el = hostRef.current;

    const hideHint = () => setCopyHint(null);

    const onMouseDown = () => hideHint();

    const onMouseUp = (event: MouseEvent) => {
      const { clientX, clientY } = event;
      // 等一帧再读选区：xterm 在 mouseup 处理完之后才把选区定下来
      requestAnimationFrame(() => {
        const text = term.getSelection();
        if (!text.trim() || !el) {
          setCopyHint(null);
          return;
        }

        const rect = el.getBoundingClientRect();
        const x = Math.min(Math.max(clientX - rect.left, 40), rect.width - 40);
        const y = clientY - rect.top;
        // 靠近顶部时改到下方，否则按钮会被裁掉
        setCopyHint({ x, y, below: y < 46 });
        setCopied(false);
      });
    };

    el?.addEventListener("mousedown", onMouseDown);
    el?.addEventListener("mouseup", onMouseUp);

    // 滚动会改变选区的位置，按钮跟着错位，直接收起来
    const scrollSub = term.onScroll(hideHint);

    // 容器尺寸变化（窗口缩放、面板折叠）时重新 fit
    const observer = new ResizeObserver(() => {
      try {
        fit.fit();
      } catch {
        // 隐藏状态下尺寸为 0，忽略
      }
    });
    observer.observe(host);

    // Ctrl/Cmd+Shift+C 复制选中内容、Ctrl/Cmd+Shift+V 粘贴；普通 Ctrl+C 仍然发 SIGINT
    term.attachCustomKeyEventHandler((event) => {
      if (event.type !== "keydown" || !event.shiftKey) return true;
      if (!event.ctrlKey && !event.metaKey) return true;

      if (event.code === "KeyC") {
        const selection = term.getSelection();
        if (selection) void navigator.clipboard.writeText(selection).catch(() => undefined);
        return false;
      }
      if (event.code === "KeyV") {
        void navigator.clipboard
          .readText()
          .then((text) => {
            const id = sessionRef.current;
            if (id && text) void call("session_write", { sessionId: id, data: text });
          })
          .catch(() => undefined);
        return false;
      }
      return true;
    });

    return () => {
      disposed = true;
      clearInterval(ackTimer);
      observer.disconnect();
      dataSub.dispose();
      resizeSub.dispose();
      scrollSub.dispose();
      el?.removeEventListener("mousedown", onMouseDown);
      el?.removeEventListener("mouseup", onMouseUp);
      reconnectRef.current = null;
      const id = sessionRef.current;
      if (id) void call("session_close", { sessionId: id }).catch(() => undefined);
      term.dispose();
      fitRef.current = null;
      termRef.current = null;
      sessionRef.current = null;
    };
  }, [tab.tabId, tab.connectionId]);

  // 切回这个标签时补一次 fit：隐藏期间容器尺寸一直是 0
  useEffect(() => {
    if (!active) return;
    const frame = requestAnimationFrame(() => {
      try {
        fitRef.current?.fit();
      } catch {
        // 忽略
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [active]);

  // 设置变化时就地更新，不重建终端 —— 重建会丢掉回滚缓冲
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;

    term.options.fontSize = fontSize;
    term.options.theme = TERM_THEMES[themeName];
    term.options.scrollback = scrollback;

    // 字号变了字符尺寸也跟着变，必须重新 fit，否则行列数还是旧值
    try {
      fitRef.current?.fit();
    } catch {
      // 隐藏状态下尺寸为 0，忽略
    }
  }, [fontSize, themeName, scrollback]);

  // 插入片段后把焦点还给终端，否则用户还得再点一下才能按回车执行
  useEffect(() => {
    if (focusTick > 0) termRef.current?.focus();
  }, [focusTick]);

  /** 复制当前选区，成功后短暂显示「已复制」再收起。 */
  const copySelection = async () => {
    const text = termRef.current?.getSelection() ?? "";
    if (!text) {
      setCopyHint(null);
      return;
    }

    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => {
        setCopyHint(null);
        setCopied(false);
      }, 700);
    } catch {
      // 剪贴板被拒（少见），保持按钮可见让用户改用 Ctrl+Shift+C
      setCopied(false);
    }
  };

  const reconnect = () => reconnectRef.current?.();
  const close = () => useSessionStore.getState().closeTab(tab.tabId);

  // 后端有时给的原因就是「连接已断开」，直接拼会变成「连接已断开 · 连接已断开」
  const bannerReason = tab.error && tab.error !== "连接已断开" ? ` · ${tab.error}` : "";

  return (
    <div className="term-host" style={{ display: active ? "block" : "none" }}>
      <div className="term-host__inner" ref={hostRef} />

      {copyHint ? (
        <button
          type="button"
          className="term-copybtn"
          style={{
            left: copyHint.x,
            top: copyHint.y,
            transform: copyHint.below
              ? "translate(-50%, 10px)"
              : "translate(-50%, calc(-100% - 10px))",
          }}
          // 按下时不让终端失焦/清选区，否则点一下选区就没了
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => void copySelection()}
        >
          {copied ? "已复制" : "复制"}
        </button>
      ) : null}

      {tab.status === "disconnected" ? (
        <div className="term-banner">
          <span className="term-banner__text">连接已断开{bannerReason}</span>
          <button type="button" className="btn btn--sm" onClick={reconnect}>
            重连
          </button>
          <button type="button" className="btn btn--sm" onClick={close}>
            关闭标签
          </button>
        </div>
      ) : null}

      {tab.status === "failed" ? (
        <div className="term-overlay term-overlay--danger">
          <strong>连接失败</strong>
          <p>{tab.error}</p>
          <div className="term-overlay__actions">
            <button type="button" className="btn btn--primary" onClick={reconnect}>
              重试
            </button>
            <button type="button" className="btn" onClick={close}>
              关闭标签
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

export default function TerminalPage() {
  const tabs = useSessionStore((state) => state.tabs);
  const activeTabId = useSessionStore((state) => state.activeTabId);
  const openTab = useSessionStore((state) => state.openTab);
  const closeTab = useSessionStore((state) => state.closeTab);
  const setActiveTab = useSessionStore((state) => state.setActiveTab);
  const markBySession = useSessionStore((state) => state.markBySession);
  const pageActive = useUiStore((state) => state.activePage === "terminal");
  const themeName = useSettingsStore((state) => state.settings.theme);
  const connectionsRevision = useUiStore((state) => state.connectionsRevision);

  const [connections, setConnections] = useState<ConnectionDto[]>([]);
  const [loading, setLoading] = useState(true);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [snippetOpen, setSnippetOpen] = useState(false);
  /** 插入片段后 +1，让终端重新拿回焦点 */
  const [focusTick, setFocusTick] = useState(0);
  const [snippetNotice, setSnippetNotice] = useState<string | null>(null);
  /** 正在重命名的标签（双击标题进入） */
  const [editingTab, setEditingTab] = useState<string | null>(null);
  const [tabDraft, setTabDraft] = useState("");
  /** 诊断浮层。null 表示没打开。 */
  const [diag, setDiag] = useState<{
    kind: DiagKind;
    title: string;
    loading: boolean;
    error: string | null;
    result: DiagResult | null;
  } | null>(null);

  /**
   * 拉取连接列表。
   *
   * 不能只在挂载时拉一次：终端页是常驻挂载的（切换页面只切显隐，否则会话全被销毁），
   * 所以启动时拉的那份快照之后永远不会更新 —— 在「连接」页新建的连接会一直看不见。
   *
   * 两个触发源：切到终端页（`pageActive`）、连接页落库（`connectionsRevision`）。
   * 这两个值变化时都会多拉一次，但那是本地 SQLite 查询，代价可以忽略。
   */
  useEffect(() => {
    let cancelled = false;
    call<ConnectionDto[]>("connection_list", { query: null })
      .then((rows) => {
        if (!cancelled) setConnections(rows);
      })
      .catch(() => undefined)
      .finally(() => {
        // 刷新时不动 loading，避免列表闪一下「加载中」
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [pageActive, connectionsRevision]);

  /**
   * 清理后端残留会话。
   *
   * 整页重载（右键「重新加载」/ HMR 全量刷新）后前端 sessionId 全丢，
   * 但 Rust 侧的 PTY 还开着 —— 不清就是泄漏。本页常驻挂载，
   * 这个 effect 只在真正重载时才会再跑一次。
   */
  useEffect(() => {
    void call<number>("session_close_all").catch(() => undefined);
  }, []);

  useEffect(() => {
    const unlisten = listen<SshStateEvent>("ssh:state", (event) => {
      markBySession(event.payload.sessionId, "disconnected", event.payload.reason);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [markBySession]);

  const open = useCallback(
    (connection: ConnectionDto) => {
      openTab({
        connectionId: connection.id,
        title: connection.name,
        hostLabel: `${connection.username}@${connection.host}:${connection.port}`,
      });
      setPickerOpen(false);
    },
    [openTab],
  );

  const commitRename = (tabId: string) => {
    useSessionStore.getState().renameTab(tabId, tabDraft);
    setEditingTab(null);
  };

  /**
   * 把片段插入当前活跃会话。
   *
   * 只写文本、不追加回车（A-12）。`sessionId` 从 store 里取，
   * 因为终端实例在子组件内部，父组件只通过 store 知道当前会话。
   */
  const insertSnippet = (command: string, snippet: SnippetMeta) => {
    const store = useSessionStore.getState();
    const tab = store.tabs.find((item) => item.tabId === store.activeTabId);

    if (!tab?.sessionId) {
      setSnippetNotice("先打开一个终端标签，片段会插入到当前会话");
      return;
    }

    // 有副作用的命令要明确告诉用户「还没执行」—— 标签上的颜色容易被忽略
    if (snippet.risk === "critical") {
      setSnippetNotice(
        `已插入高危命令「${snippet.name}」。它还没运行，请先核对内容再按回车。`,
      );
    } else if (snippet.risk === "dangerous") {
      setSnippetNotice(`已插入「${snippet.name}」，按回车执行。`);
    } else {
      setSnippetNotice(null);
    }

    void call("session_write", { sessionId: tab.sessionId, data: command }).catch((err) => {
      setSnippetNotice(fromUnknown(err).message);
    });
    setFocusTick((tick) => tick + 1);
  };

  /**
   * 跑一条诊断并展示表格。
   *
   * 刷新时保留上一次结果（同一种诊断），否则每点一次「刷新」表格都会闪一下消失。
   */
  const openDiag = async (kind: DiagKind, label: string) => {
    const store = useSessionStore.getState();
    const tab = store.tabs.find((item) => item.tabId === store.activeTabId);

    if (!tab?.sessionId) {
      setSnippetNotice("先打开一个终端标签，诊断需要一条已连接的会话");
      return;
    }

    const sessionId = tab.sessionId;
    setDiag((prev) => ({
      kind,
      title: label,
      loading: true,
      error: null,
      result: prev?.kind === kind ? prev.result : null,
    }));

    try {
      const result = await call<DiagResult>("diag_run", { sessionId, kind });
      setDiag({ kind, title: label, loading: false, error: null, result });
    } catch (err) {
      setDiag({
        kind,
        title: label,
        loading: false,
        error: fromUnknown(err).message,
        result: null,
      });
    }
  };

  /**
   * 应用级快捷键。
   *
   * 一律用 `Ctrl/Cmd+Shift+*`，因为 `Ctrl+T`（转置字符）、`Ctrl+W`（删除前一个词）
   * 在 shell 里是有含义的，抢走会天天咬到用户。这也是 GNOME Terminal 的惯例。
   *
   * 只在终端页处于激活状态时绑定，且只拦 T / W / Tab ——
   * 不能拦 C / V，否则 xterm 自己的复制粘贴处理器就收不到事件了。
   */
  useEffect(() => {
    if (!pageActive) return;

    const onKeyDown = (event: KeyboardEvent) => {
      if (!event.ctrlKey && !event.metaKey) return;

      const stop = () => {
        event.preventDefault();
        event.stopPropagation();
      };

      if (event.shiftKey && event.code === "KeyT") {
        stop();
        setPickerOpen(true);
        return;
      }

      if (event.shiftKey && event.code === "KeyP") {
        stop();
        setSnippetOpen((open) => !open);
        return;
      }

      if (event.shiftKey && event.code === "KeyW") {
        stop();
        const store = useSessionStore.getState();
        if (store.activeTabId) store.closeTab(store.activeTabId);
        return;
      }

      if (event.code === "Tab") {
        stop();
        const store = useSessionStore.getState();
        const ids = store.tabs.map((tab) => tab.tabId);
        const target = nextTabId(ids, store.activeTabId, event.shiftKey ? -1 : 1);
        if (target) store.setActiveTab(target);
      }
    };

    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [pageActive]);

  const showPicker = tabs.length === 0 || pickerOpen;

  return (
    <div className="term-page">
      <div className="term-tabs">
        {tabs.map((tab) => (
          <div
            key={tab.tabId}
            className={`term-tab ${tab.tabId === activeTabId ? "is-active" : ""}`}
          >
            {editingTab === tab.tabId ? (
              <input
                className="term-tab__input"
                autoFocus
                value={tabDraft}
                onChange={(event) => setTabDraft(event.target.value)}
                onBlur={() => commitRename(tab.tabId)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") commitRename(tab.tabId);
                  if (event.key === "Escape") setEditingTab(null);
                }}
              />
            ) : (
              <button
                type="button"
                className="term-tab__label"
                onClick={() => setActiveTab(tab.tabId)}
                onDoubleClick={() => {
                  setEditingTab(tab.tabId);
                  setTabDraft(tab.title);
                }}
                title={`${tab.hostLabel}（双击重命名）`}
              >
                <span
                  className={`dot dot--${
                    tab.status === "ready" ? "ok" : tab.status === "failed" ? "warn" : "idle"
                  }`}
                />
                <span className="term-tab__title">{tab.title}</span>
              </button>
            )}
            <button
              type="button"
              className="term-tab__close"
              title="关闭标签（Ctrl+Shift+W）"
              onClick={() => closeTab(tab.tabId)}
            >
              ×
            </button>
          </div>
        ))}
        <button
          type="button"
          className="term-tabs__new"
          title="新建连接（Ctrl+Shift+T）"
          onClick={() => setPickerOpen((open) => !open)}
        >
          + 新连接
        </button>
      </div>

      {showPicker ? (
        <div className="term-picker">
          {loading ? (
            <p className="term-picker__hint">加载连接列表…</p>
          ) : connections.length === 0 ? (
            <p className="term-picker__hint">
              还没有保存的连接。先去「连接」页添加一台服务器，再回来开终端。
            </p>
          ) : (
            <ul className="term-picker__list">
              {connections.map((connection) => (
                <li key={connection.id}>
                  <button
                    type="button"
                    className="term-picker__item"
                    onClick={() => open(connection)}
                  >
                    <span className="term-picker__name">{connection.name}</span>
                    <span className="term-picker__host">
                      {connection.username}@{connection.host}:{connection.port}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}

      <div className="term-body" style={{ background: TERM_THEMES[themeName].background }}>
        <div className="term-main">
          {tabs.map((tab) => (
            <TerminalView
              key={tab.tabId}
              tab={tab}
              active={tab.tabId === activeTabId}
              focusTick={focusTick}
            />
          ))}
        </div>

        {snippetOpen ? (
          <SnippetPanel
            onInsert={insertSnippet}
            onView={(kind, label) => void openDiag(kind, label)}
            onClose={() => setSnippetOpen(false)}
            sessionId={
              tabs.find((tab) => tab.tabId === activeTabId)?.sessionId ?? null
            }
          />
        ) : null}
      </div>

      {snippetNotice ? <p className="term-notice">{snippetNotice}</p> : null}

      <div className="term-hintbar">
        <span>Ctrl+Shift+T 新建</span>
        <span>Ctrl+Shift+W 关闭</span>
        <span>Ctrl+Shift+P 命令片段</span>
        <span>Ctrl+Tab 切换</span>
        <span>Ctrl+Shift+C/V 复制粘贴</span>
      </div>

      {diag ? (
        <DiagPanel
          title={diag.title}
          loading={diag.loading}
          error={diag.error}
          result={diag.result}
          onRefresh={() => void openDiag(diag.kind, diag.title)}
          onClose={() => setDiag(null)}
        />
      ) : null}
    </div>
  );
}
