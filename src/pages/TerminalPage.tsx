import { Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useCallback, useEffect, useRef, useState } from "react";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import { EMPTY_LINE, feedLine, parseCd } from "../lib/shell";
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
import FilePanel from "./FilePanel";
import LoadPanel from "./LoadPanel";
import { TERM_THEMES } from "./SettingsPage";
import { useSessionStore, type TerminalTab } from "../stores/sessionStore";
import { useSettingsStore } from "../stores/settingsStore";
import { useUiStore } from "../stores/uiStore";

/**
 * 一次性提示的存活时长。
 *
 * 主出口是「按回车」—— 那时命令真的发出去了，提示的使命结束；
 * 定时器只是兜底，防止用户不回车（改主意、直接切走）时那行字一直挂在窗口底部。
 */
const NOTICE_TTL_MS = 20_000;


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
  onEnter,
  onCommandLine,
}: {
  tab: TerminalTab;
  active: boolean;
  /** 父组件插入片段后 +1，用来把焦点还给终端 */
  focusTick: number;
  /** 用户在终端里按了回车（真正提交了这行命令） */
  onEnter?: () => void;
  /** 用户敲完并回车的一整行命令（用来识别 cd，让文件面板跟着走） */
  onCommandLine?: (line: string) => void;
}) {
  const fontSize = useSettingsStore((state) => state.settings.fontSize);
  const themeName = useSettingsStore((state) => state.settings.theme);
  const scrollback = useSettingsStore((state) => state.settings.scrollback);

  const hostRef = useRef<HTMLDivElement>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const sessionRef = useRef<string | null>(null);
  const reconnectRef = useRef<(() => void) | null>(null);
  /**
   * 回车回调走 ref 而不是进 effect 依赖：
   * 主 effect 的依赖一旦变化就会重建终端，而那会丢掉回滚缓冲。
   */
  const onEnterRef = useRef(onEnter);
  const onCommandLineRef = useRef(onCommandLine);
  /**
   * 正在输入的这一行。
   *
   * 只用来识别 `cd` —— SSH 里拿不到对端 shell 的当前目录（拿不到 pid，
   * OSC 7 又要用户自己配 rc），所以从"用户敲了什么"这条唯一可见的线索反推。
   */
  const lineRef = useRef(EMPTY_LINE);

  useEffect(() => {
    onEnterRef.current = onEnter;
    onCommandLineRef.current = onCommandLine;
  }, [onEnter, onCommandLine]);

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
      // 回车 = 用户真的把这行发出去了：插入片段时留的「还没运行」提示到这里就该收，
      // 同时把这一行交给上层识别 cd
      const fed = feedLine(lineRef.current, data);
      lineRef.current = fed.state;
      if (fed.submitted.length > 0) {
        onEnterRef.current?.();
        for (const line of fed.submitted) {
          if (line) onCommandLineRef.current?.(line);
        }
      }

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
        // 顺手挡掉浏览器默认的复制：否则系统剪贴板会被原生路径再写一次
        event.preventDefault();
        const selection = term.getSelection();
        if (selection) void navigator.clipboard.writeText(selection).catch(() => undefined);
        return false;
      }

      // Ctrl+Shift+V **故意不接管**，交给浏览器原生粘贴。
      //
      // 之前这里是自己 readText() 再 session_write。问题是只 return false 拦不住
      // WebView 的原生粘贴：内容照样被粘进 xterm 的隐藏 textarea，xterm 再通过
      // onData 发一遍 —— 加上我们自己那一路，同一段内容发两次（"粘一次变两次"）。
      //
      // 交给原生还有一个好处：xterm 会在 bracketed paste 模式下自动包 ESC[200~ … ESC[201~，
      // 多行粘贴不会在 shell 里被逐行立即执行。
      return true;
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
  /**
   * 右侧辅助面板：命令片段 / 文件 / 负载。
   *
   * 用联合状态而不是三个布尔，是为了让「同时只能开一个」成为**类型上的事实**，
   * 而不是靠每个按钮里手写 `setOther(false)` 维持 —— 那种写法迟早漏一处。
   */
  const [sidePanel, setSidePanel] = useState<"snippet" | "files" | "load" | null>(null);
  /** 哪些标签打开过文件面板。保持挂载，切走再回来目录和传输进度还在。 */
  const [filePanels, setFilePanels] = useState<string[]>([]);
  /**
   * 每个标签最近一次 `cd` 的参数，喂给对应的文件面板。
   *
   * 只有真的认出 cd 才更新 state —— 否则每敲一条命令整个终端页都要重渲染一遍。
   */
  const [cdTargets, setCdTargets] = useState<
    Record<string, { seq: number; target: string }>
  >({});
  const cdSeq = useRef(0);
  /**
   * 文件面板宽度。
   *
   * 左右拖动分隔条改，值存在 localStorage —— 这是纯 UI 偏好，
   * 没必要进 settings 表（那边是跨设备语义的配置）。
   */
  const [filesWidth, setFilesWidth] = useState(() => {
    try {
      const saved = Number(globalThis.localStorage?.getItem("termharbor.filesWidth"));
      if (Number.isFinite(saved) && saved >= 280) return saved;
    } catch {
      // 无 localStorage 时用默认值
    }
    return 440;
  });
  const dragRef = useRef<{ startX: number; startWidth: number } | null>(null);
  /** 插入片段后 +1，让终端重新拿回焦点 */
  const [focusTick, setFocusTick] = useState(0);
  /**
   * 一次性提示（插入片段、没连会话之类）。
   *
   * 带 `seq` 是因为「同一句话再说一遍」也要重新计时：只比字符串的话，
   * 连着插两次同一个高危片段，第二次不会重置定时器。
   */
  const [notice, setNotice] = useState<{ text: string; seq: number } | null>(null);
  const noticeSeq = useRef(0);
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
   * 换一句提示 / 收掉提示。
   *
   * 每句提示都带一个自增序号，所以「同一句话再来一次」也会重置自动收起的计时器。
   */
  const showNotice = useCallback((text: string | null) => {
    if (text === null) {
      setNotice(null);
      return;
    }
    noticeSeq.current += 1;
    setNotice({ text, seq: noticeSeq.current });
  }, []);

  const clearNotice = useCallback(() => setNotice(null), []);

  /** 终端里敲完一行：认出 cd 就转给对应标签的文件面板 */
  const handleCommandLine = useCallback((tabId: string, line: string) => {
    const target = parseCd(line);
    if (!target) return;

    cdSeq.current += 1;
    setCdTargets((prev) => ({ ...prev, [tabId]: { seq: cdSeq.current, target } }));
  }, []);

  // 到点自动收起。seq 变化会重开计时器，所以连续插入两次不会被前一次的计时器提前清掉。
  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), NOTICE_TTL_MS);
    return () => window.clearTimeout(timer);
  }, [notice]);

  // 换标签就把提示收起来：那条提示说的是上一个标签里插入的命令
  useEffect(() => {
    setNotice(null);
  }, [activeTabId]);

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
    // 文件面板内嵌进本页后，SFTP 的孤儿清理也归这里：
    // 整页重载后前端 sessionId 全丢，但后端连接还活着。
    void call<number>("sftp_close_all").catch(() => undefined);
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
      showNotice("先打开一个终端标签，片段会插入到当前会话");
      return;
    }

    // 有副作用的命令要明确告诉用户「还没执行」—— 标签上的颜色容易被忽略
    if (snippet.risk === "critical") {
      showNotice(`已插入高危命令「${snippet.name}」。它还没运行，请先核对内容再按回车。`);
    } else if (snippet.risk === "dangerous") {
      showNotice(`已插入「${snippet.name}」，按回车执行。`);
    } else {
      showNotice(null);
    }

    void call("session_write", { sessionId: tab.sessionId, data: command }).catch((err) => {
      showNotice(fromUnknown(err).message);
    });
    setFocusTick((tick) => tick + 1);
  };

  /**
   * 跑一条诊断并展示表格。
   *
   * 刷新时保留上一次结果（同一种诊断），否则每点一次「刷新」表格都会闪一下消失。
   */
  /**
   * 文件面板按标签挂载：当前标签第一次打开面板时才挂，
   * 之后切走保留挂载（`hidden`），否则每切一次标签都要重连 SFTP。
   */
  useEffect(() => {
    if (sidePanel !== "files" || !activeTabId) return;
    setFilePanels((prev) => (prev.includes(activeTabId) ? prev : [...prev, activeTabId]));
  }, [sidePanel, activeTabId]);

  // 标签关掉后把它对应的面板一起清掉，否则会留一个没人管的 SFTP 会话
  useEffect(() => {
    setFilePanels((prev) => {
      const alive = prev.filter((id) => tabs.some((tab) => tab.tabId === id));
      return alive.length === prev.length ? prev : alive;
    });
  }, [tabs]);

  /**
   * 拖动分隔条调宽度。
   *
   * 上限取「窗口宽度 - 420」：至少要给终端留 420px，否则拖到最宽终端就废了；
   * 下限 280 保证文件名还看得见。双击分隔条复位。
   */
  const startResize = (event: React.PointerEvent<HTMLDivElement>) => {
    dragRef.current = { startX: event.clientX, startWidth: filesWidth };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const resizeFiles = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const upper = Math.max(280, Math.min(760, window.innerWidth - 420));
    const next = Math.min(
      Math.max(drag.startWidth - (event.clientX - drag.startX), 280),
      upper,
    );
    setFilesWidth(next);
  };

  const endResize = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    dragRef.current = null;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    try {
      globalThis.localStorage?.setItem("termharbor.filesWidth", String(Math.round(filesWidth)));
    } catch {
      // 存不进去只是下次回到默认宽度，不影响使用
    }
  };

  /** 再点一次同一个按钮就收起。 */
  const togglePanel = (panel: "snippet" | "files" | "load") => {
    setSidePanel((current) => (current === panel ? null : panel));
  };

  const openDiag = async (kind: DiagKind, label: string) => {
    const store = useSessionStore.getState();
    const tab = store.tabs.find((item) => item.tabId === store.activeTabId);

    if (!tab?.sessionId) {
      showNotice("先打开一个终端标签，诊断需要一条已连接的会话");
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
        setSidePanel((current) => (current === "snippet" ? null : "snippet"));
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
        {/* 标签区可横向滚动；右侧这组按钮固定在右边，不跟着标签滚走 */}
        <div className="term-tabs__list">
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
        </div>

        <div className="term-tabs__actions">
          <button
            type="button"
            className="term-tabs__new"
            title="新建连接（Ctrl+Shift+T）"
            onClick={() => setPickerOpen((open) => !open)}
          >
            + 新连接
          </button>

          {/* 分组：左边是「新建」，右边是三个侧栏开关，中间用一条竖线隔开 */}
          <span className="term-tabs__divider" />

          {/* 光靠快捷键没人会发现侧栏，给个常驻入口 */}
          <button
            type="button"
            className={`term-tabs__new ${sidePanel === "snippet" ? "is-active" : ""}`}
            title="命令片段（Ctrl+Shift+P）"
            onClick={() => togglePanel("snippet")}
          >
            片段
          </button>

          <button
            type="button"
            className={`term-tabs__new ${sidePanel === "files" ? "is-active" : ""}`}
            title="文件（SFTP，跟着当前标签的服务器走）"
            disabled={!activeTabId}
            onClick={() => togglePanel("files")}
          >
            文件
          </button>

          <button
            type="button"
            className={`term-tabs__new ${sidePanel === "load" ? "is-active" : ""}`}
            title="负载看板（CPU / 内存 / 磁盘 / 网络，每 3 秒刷新）"
            disabled={!activeTabId}
            onClick={() => togglePanel("load")}
          >
            负载
          </button>
        </div>
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
              onEnter={clearNotice}
              onCommandLine={(line) => handleCommandLine(tab.tabId, line)}
            />
          ))}
        </div>

        {sidePanel === "snippet" ? (
          <SnippetPanel
            onInsert={insertSnippet}
            onView={(kind, label) => void openDiag(kind, label)}
            onClose={() => setSidePanel(null)}
            sessionId={
              tabs.find((tab) => tab.tabId === activeTabId)?.sessionId ?? null
            }
          />
        ) : null}

        {sidePanel === "load" ? (
          <LoadPanel
            sessionId={tabs.find((tab) => tab.tabId === activeTabId)?.sessionId ?? null}
            active={pageActive}
            onClose={() => setSidePanel(null)}
          />
        ) : null}

        {/* 文件面板一旦为某个标签打开过就**常驻挂载**，切到片段/负载时只隐藏。
            原来这里是 `sidePanel === "files" ? ... : null`，切走即卸载 ——
            而 FilePanel 卸载时会关掉自己的 SFTP 会话，于是每次点回来都要重连一遍。 */}
        {filePanels.length > 0 ? (
          <aside
            className="term-files"
            style={{ width: filesWidth, display: sidePanel === "files" ? undefined : "none" }}
          >
            <div
              className="term-files__grip"
              title="拖动调整宽度，双击复位"
              onPointerDown={startResize}
              onPointerMove={resizeFiles}
              onPointerUp={endResize}
              onPointerCancel={endResize}
              onDoubleClick={() => setFilesWidth(440)}
            />
            <div className="term-files__head">
              <span className="term-files__title">文件</span>
              <span className="term-files__host">
                {tabs.find((tab) => tab.tabId === activeTabId)?.hostLabel ?? ""}
              </span>
              <button
                type="button"
                className="term-files__close"
                title="收起文件面板（连接保留，下次打开不用重连；要断开请点面板里的「断开」）"
                onClick={() => setSidePanel(null)}
              >
                ×
              </button>
            </div>

            <div className="term-files__body">
              {filePanels.map((tabId) => {
                const tab = tabs.find((item) => item.tabId === tabId);
                if (!tab) return null;
                return (
                  <div
                    key={tabId}
                    className="term-files__slot"
                    hidden={tabId !== activeTabId}
                  >
                    <FilePanel
                      connectionId={tab.connectionId}
                      compact
                      cdTarget={cdTargets[tabId] ?? null}
                    />
                  </div>
                );
              })}
            </div>
          </aside>
        ) : null}
      </div>

      {notice ? (
        <p className="term-notice" role="status">
          <span className="term-notice__text">{notice.text}</span>
          <button
            type="button"
            className="term-notice__close"
            title="收起提示"
            aria-label="收起提示"
            onClick={clearNotice}
          >
            ×
          </button>
        </p>
      ) : null}

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
