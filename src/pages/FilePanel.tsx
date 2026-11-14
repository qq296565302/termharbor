import { open, save } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { fromUnknown } from "../lib/errors";
import {
  crumbsOf,
  formatMode,
  formatSize,
  formatSpeed,
  formatTime,
  validateRename,
} from "../lib/files";
import { call } from "../lib/ipc";
import { useSettingsStore } from "../stores/settingsStore";
import { resolveCdPath } from "../lib/shell";
import type { RemoteEntry, SftpOpened, TransferProgress } from "../lib/types";

/** 行内操作：同一时刻只允许一个，避免多个输入框同时出现。 */
type RowAction =
  | { kind: "mkdir" }
  | { kind: "rename"; entry: RemoteEntry }
  | { kind: "delete"; entry: RemoteEntry }
  | null;

/**
 * 面板顶部的提示。
 *
 * 分成 info / error 两类不是为了好看：**info 必须会自己消失**。
 * 早先只有一根 string，下载开始时写了「已开始下载 x」，传完了它还在那儿，
 * 用户会以为还在传 —— 提示不消失比没有提示更糟。
 */
interface Notice {
  kind: "info" | "error";
  text: string;
  /** 服务器/后端给的具体原因，只有 error 才可能有 */
  detail?: string | null;
}

/** 远端文件系统错误：`detail` 是服务器自己说的原因（Failure / Permission denied），值得给用户看。 */
function remoteErrorNotice(err: unknown): Notice {
  const error = fromUnknown(err);
  const showDetail = error.detail && error.code.startsWith("SFTP_");
  return {
    kind: "error",
    text: error.message,
    detail: showDetail ? error.detail : null,
  };
}

/** 提示行。info 用中性色，error 用警示色，服务器给的具体原因缩在后面。 */
function NoticeLine({ notice }: { notice: Notice | null }) {
  if (!notice) return null;
  return (
    <p className={`files-error${notice.kind === "info" ? " files-error--info" : ""}`}>
      {notice.text}
      {notice.detail ? <span className="files-error__detail">（{notice.detail}）</span> : null}
    </p>
  );
}

/**
 * 面板打开时先落在这里。
 *
 * 原来进 `$HOME`（多数机器是 `/root`），但用户要在 `/etc`、`/var` 之间来回翻，
 * 每次都要从家目录往上爬 —— 直接给根目录，要看家目录有工具栏的 `~`。
 */
const ROOT_PATH = "/";

interface FilePanelProps {
  /**
   * 浏览哪台机器。**跟着会话标签走**：终端在哪个标签，文件面板就属于那台机器。
   *
   * 合并前是「文件页自己再选一次服务器」，同一个主机要选两次、连两次（前端意义上的），
   * 这是用户反馈里最别扭的地方。
   */
  connectionId: string;
  /** 面板里的紧凑布局（窄一些，少显示一列） */
  compact?: boolean;
  /**
   * 终端里刚敲下的 `cd` 参数（原样，可能是 `~` / 相对路径）。
   *
   * `seq` 是父组件的自增序号：连着 `cd` 两次同一个目录也要各处理一次，
   * 只比字符串的话第二次不会触发。
   */
  cdTarget?: { seq: number; target: string } | null;
}

export default function FilePanel({ connectionId, compact = false, cdTarget }: FilePanelProps) {
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [home, setHome] = useState("");
  const [path, setPath] = useState("/");
  const [entries, setEntries] = useState<RemoteEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [draft, setDraft] = useState("");
  const [action, setAction] = useState<RowAction>(null);
  const [transfers, setTransfers] = useState<TransferProgress[]>([]);
  const autoPack = useSettingsStore((state) => state.settings.autoPack);
  const packThreshold = useSettingsStore((state) => state.settings.packThreshold);

  // info 提示自己收掉（"已开始下载 x" 这类）；错误一直留着，直到下一次操作覆盖它。
  useEffect(() => {
    if (notice?.kind !== "info") return;
    const timer = window.setTimeout(() => setNotice(null), 5000);
    return () => window.clearTimeout(timer);
  }, [notice]);
  /** 拖拽本地文件悬停在面板上时的高亮 */
  const [dragOver, setDragOver] = useState(false);
  /** 右键菜单。`entry` 为 null 表示点在空白处（只给目录级操作）。 */
  const [menu, setMenu] = useState<{ x: number; y: number; entry: RemoteEntry | null } | null>(null);

  const sessionRef = useRef<string | null>(null);
  /**
   * 我们**以为**的 shell 当前目录（不是面板正在看的目录）。
   *
   * 两者必须分开：用户可以点着面板逛到别处，而 shell 还站在原地；
   * 那时敲 `cd sub`，相对路径要相对 shell 算，不是相对面板。
   */
  const shellCwd = useRef<string | null>(null);
  /** 上一次 cd 前的 shell 目录，用来支持 `cd -` */
  const prevShellCwd = useRef<string | null>(null);
  /** 已经处理到哪个 seq，避免重复跟随 */
  const followedSeq = useRef(0);
  const panelRef = useRef<HTMLDivElement>(null);
  /** 当前目录的镜像，给"传输完成后刷新"这类异步回调读（闭包里拿不到最新 state） */
  const pathRef = useRef(path);
  /** transferId → 这次上传的目标目录，用来判断要不要刷新当前列表 */
  const uploadDirs = useRef(new Map<string, string>());
  const refreshTimer = useRef<number | null>(null);

  useEffect(() => {
    sessionRef.current = sessionId;
  }, [sessionId]);

  useEffect(() => {
    pathRef.current = path;
  }, [path]);

  /**
   * 合并刷新的定时器。
   *
   * 一次拖 10 个文件会有 10 条完成事件，逐条刷新就是 10 次 `sftp_list`；
   * 攒 250ms 只刷一次，反正用户看到的都是最后一次。
   */
  const scheduleRefresh = useCallback(() => {
    if (refreshTimer.current !== null) window.clearTimeout(refreshTimer.current);
    refreshTimer.current = window.setTimeout(() => {
      refreshTimer.current = null;
      const sid = sessionRef.current;
      if (!sid) return;
      // 失败不弹提示：这是"顺手刷新"，不打扰正在做的事
      void call<RemoteEntry[]>("sftp_list", { sessionId: sid, path: pathRef.current })
        .then((rows) => setEntries(rows))
        .catch(() => undefined);
    }, 250);
  }, []);

  // 传输进度
  useEffect(() => {
    const unlisten = listen<TransferProgress>("sftp:progress", (event) => {
      const payload = event.payload;
      // 会话页里每个标签一个面板，事件是全局广播的：
      // 不过滤的话 A 标签的传输会显示在 B 标签的面板里。
      if (payload.sessionId !== sessionRef.current) return;
      setTransfers((prev) => {
        const index = prev.findIndex((item) => item.transferId === payload.transferId);
        if (index < 0) return [...prev, payload];
        const next = [...prev];
        next[index] = payload;
        return next;
      });

      // 传完就刷一次列表，否则刚上传的文件要手动点「刷新」才看得见。
      // 只认上传：下载动的是本地磁盘，远端目录没变。
      if (payload.direction === "upload" && payload.state !== "running") {
        const dir = uploadDirs.current.get(payload.transferId);
        uploadDirs.current.delete(payload.transferId);
        if (payload.state === "done" && dir === pathRef.current) scheduleRefresh();
      }

      // 传完（成功、失败、取消都算）就把「已开始下载 x」收掉 ——
      // 留着它会让人以为还在传，比没有提示更糟
      if (payload.state !== "running") {
        setNotice((current) => (current?.kind === "info" ? null : current));
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
      if (refreshTimer.current !== null) {
        window.clearTimeout(refreshTimer.current);
        refreshTimer.current = null;
      }
    };
  }, [scheduleRefresh]);

  const load = useCallback(
    async (sid: string, target: string) => {
      const rows = await call<RemoteEntry[]>("sftp_list", { sessionId: sid, path: target });
      setEntries(rows);
      setPath(target);
      // 路径历史同样不设上限：命令栏的路径补全靠它给出「我去过哪些目录」。
      // 后端按 value 去重，反复进同一个目录只会刷新时间和使用次数。
      void call("history_push", { kind: "path", value: target, connectionId }).catch(
        () => undefined,
      );
    },
    [connectionId],
  );

  const openSession = async (connectionId: string) => {
    setBusy(true);
    setNotice(null);
    try {
      const opened = await call<SftpOpened>("sftp_open", {
        connectionId,
        trustFingerprint: null,
      });
      setSessionId(opened.sessionId);
      setHome(opened.home);
      // 登录 shell 默认就落在 $HOME，用它作为相对路径的基准
      shellCwd.current = opened.home;
      prevShellCwd.current = null;
      await load(opened.sessionId, ROOT_PATH).catch(() =>
        // 少数服务器（Windows OpenSSH）列不出根，退回家目录而不是留个空面板
        load(opened.sessionId, opened.home),
      );
    } catch (err) {
      const error = fromUnknown(err);
      // 首次连一台机器必须先在「连接」页确认指纹，这里给一句能照做的提示
      const hint =
        error.code === "SSH_HOST_KEY_UNKNOWN"
          ? "这台服务器还没确认过主机指纹。请先去「连接」页点「测试连接」，确认指纹后再回来。"
          : error.message;
      setNotice({ kind: "error", text: hint });
    } finally {
      setBusy(false);
    }
  };

  const closeSession = async () => {
    const sid = sessionRef.current;
    setSessionId(null);
    setEntries([]);
    setPath(ROOT_PATH);
    shellCwd.current = null;
    prevShellCwd.current = null;
    // 留一句提示：不然面板变成一个没有出口的空屏，用户不知道还能重连
    setNotice({ kind: "info", text: "已断开 SFTP 连接。" });
    if (sid) {
      await call<void>("sftp_close", { sessionId: sid }).catch(() => undefined);
    }
  };

  /**
   * 嵌入模式：进入面板就连上当前标签的机器。
   *
   * 用 ref 记住已经连过的 id，避免 StrictMode 下的重复执行和无关重渲染反复建连。
   * 切换标签时父组件换 `connectionId`，这里会为新机器再连一次；
   * 旧机器那条连接由父组件卸载旧面板时关掉。
   */
  const autoConnected = useRef<string | null>(null);
  useEffect(() => {
    if (autoConnected.current === connectionId) return;
    autoConnected.current = connectionId;
    void openSession(connectionId);
    // openSession 每次渲染都是新函数，放进依赖会导致无限重连
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionId]);

  // 面板卸载（关标签）时关掉自己的 SFTP 会话，避免连接泄漏。
  // 注意：**关面板不再走这里** —— 父组件现在只把它隐藏，会话留着下次秒开。
  useEffect(
    () => () => {
      const sid = sessionRef.current;
      if (sid) void call<void>("sftp_close", { sessionId: sid }).catch(() => undefined);
    },
    [],
  );

  /**
   * 跟着终端里敲的 `cd` 走。
   *
   * 宁可不动也不能乱跳：解析出来的路径要先在远端列一次，列不出来（目录不存在、
   * 是个文件、shell 那条 cd 其实失败了）就保持原地。
   */
  useEffect(() => {
    if (!cdTarget || !sessionId) return;
    if (followedSeq.current === cdTarget.seq) return;
    followedSeq.current = cdTarget.seq;

    const cwd = shellCwd.current ?? home;
    const target =
      cdTarget.target === "-" ? prevShellCwd.current : resolveCdPath(cdTarget.target, cwd, home);
    if (!target) return;

    void (async () => {
      try {
        await load(sessionId, target);
        prevShellCwd.current = cwd;
        shellCwd.current = target;
      } catch {
        // shell 那边大概也是 cd 失败，面板保持不动即可
      }
    })();
    // load / home 变化不该重放同一条 cd，靠 followedSeq 兜住
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cdTarget, sessionId]);

  const refresh = async () => {
    if (!sessionId) return;
    try {
      await load(sessionId, path);
      setNotice(null);
    } catch (err) {
      setNotice(remoteErrorNotice(err));
    }
  };

  const goUp = async () => {
    if (!sessionId) return;
    try {
      const parent = await call<string | null>("sftp_parent_of", { path });
      if (parent) await load(sessionId, parent);
    } catch (err) {
      setNotice(remoteErrorNotice(err));
    }
  };

  const run = async (task: () => Promise<void>) => {
    setBusy(true);
    setNotice(null);
    try {
      await task();
      setAction(null);
      setDraft("");
      if (sessionId) await load(sessionId, path);
    } catch (err) {
      setNotice(remoteErrorNotice(err));
    } finally {
      setBusy(false);
    }
  };

  const createFolder = () =>
    run(async () => {
      const name = draft.trim();
      if (!name) throw new Error("请输入文件夹名称");
      const target = path.endsWith("/") ? `${path}${name}` : `${path}/${name}`;
      await call<void>("sftp_mkdir", { sessionId, path: target });
    });

  const renameEntry = (entry: RemoteEntry) =>
    run(async () => {
      const name = draft.trim();
      const problem = validateRename(name, entry, entries);
      if (problem) throw new Error(problem);
      if (name === entry.name) return; // 没改，什么都不做

      const parent = entry.path.slice(0, entry.path.length - entry.name.length);
      const target = parent.endsWith("/") ? `${parent}${name}` : `${parent}/${name}`;
      await call<void>("sftp_rename", { sessionId, from: entry.path, to: target });
    });

  const removeEntry = (entry: RemoteEntry) =>
    run(async () => {
      await call<void>("sftp_remove", {
        sessionId,
        path: entry.path,
        recursive: entry.isDir,
      });
    });

  /**
   * 上传本地文件到当前目录。
   *
   * 两个入口共用：工具栏「上传…」（系统文件对话框）与**拖拽**（OS 直接把路径丢过来，
   * 不经过对话框，所以后端那道 `ensure_upload_source` 是唯一的安全边界）。
   */
  const upload = async (paths?: string[], asDirectory = false) => {
    if (!sessionId) return;
    const dir = path;
    try {
      let list = paths;
      if (!list) {
        const picked = await open({
          multiple: !asDirectory,
          directory: asDirectory,
          title: asDirectory ? "选择要上传的文件夹" : "选择要上传的文件",
        });
        list = Array.isArray(picked) ? picked : picked ? [picked] : [];
      }
      if (list.length === 0) return;

      // 多个文件（或整个文件夹）先在本机打包再传，到对端自动解开。
      // 目录只有这一条路可走：单个文件的 sftp_upload 明确拒绝目录。
      // 开关关掉时仍允许手动走「打包下载」，但目录上传会立刻报错提示打开开关。
      if (autoPack && (asDirectory || list.length >= packThreshold)) {
        try {
          const transferId = await call<string>("pack_upload", {
            sessionId,
            localPaths: list,
            remoteDir: dir,
          });
          uploadDirs.current.set(transferId, dir);
          setNotice({
            kind: "info",
            text: `已把 ${list.length} 项打包成 1 个传输任务，传完自动解开`,
          });
        } catch (err) {
          setNotice(remoteErrorNotice(err));
        }
        return;
      }
      if (asDirectory) {
        setNotice({
          kind: "error",
          text: "上传整个文件夹需要先在「设置 → 打包传输」里打开自动压缩传输",
        });
        return;
      }

      let queued = 0;
      const failed: string[] = [];
      for (const localPath of list) {
        try {
          const transferId = await call<string>("sftp_upload", {
            sessionId,
            localPath,
            remoteDir: dir,
            name: null,
          });
          // 记住这次传到哪个目录：完成事件里没有目录信息，只有靠这里对上
          uploadDirs.current.set(transferId, dir);
          queued += 1;
        } catch (err) {
          // 一次拖一堆时，逐条列出来比只显示最后一条有用
          const name = localPath.split(/[\\/]/).pop() || localPath;
          failed.push(`${name}：${fromUnknown(err).message}`);
        }
      }

      if (failed.length > 0) setNotice({ kind: "error", text: failed.join("；") });
      else setNotice({ kind: "info", text: `已加入 ${queued} 个上传任务` });
    } catch (err) {
      setNotice(remoteErrorNotice(err));
    }
  };

  /**
   * 打包下载：远端先 tar -czf，传回本机再解开。
   * 目录只能走这条路（单文件的 sftp_download 不支持目录），也可以一次打包多个路径。
   */
  const packDownload = async (remotePaths: string[]) => {
    if (!sessionId || remotePaths.length === 0) return;
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        title: "选择解包到哪个文件夹",
      });
      const localDir = Array.isArray(picked) ? picked[0] : picked;
      if (!localDir) return;
      await call<string>("pack_download", {
        sessionId,
        remotePaths,
        localDir,
      });
      setNotice({
        kind: "info",
        text: `已开始打包下载 ${remotePaths.length} 项，传完自动解到所选文件夹`,
      });
    } catch (err) {
      setNotice(remoteErrorNotice(err));
    }
  };

  const download = async (entry: RemoteEntry) => {
    if (!sessionId) return;
    if (entry.isDir) {
      await packDownload([entry.path]);
      return;
    }
    try {
      const target = await save({ defaultPath: entry.name, title: "保存到" });
      if (!target) return;
      await call<string>("sftp_download", {
        sessionId,
        remotePath: entry.path,
        localPath: target,
      });
      setNotice({ kind: "info", text: `已开始下载 ${entry.name}` });
    } catch (err) {
      setNotice(remoteErrorNotice(err));
    }
  };

  const cancelTransfer = async (transferId: string) => {
    await call<boolean>("sftp_cancel", { transferId }).catch(() => false);
  };

  /** 拖拽回调里要拿到最新的 sessionId / path，所以每次渲染后同步一份 */
  const dropRef = useRef<(paths: string[]) => void>(() => undefined);

  useEffect(() => {
    dropRef.current = (paths) => void upload(paths);
  });

  /**
   * 接收 OS 拖进来的文件。
   *
   * Tauri 里 `dragDropEnabled` 默认开着，HTML5 的 drop 事件是收不到的，
   * 只能监听 webview 级别的这个事件。它不给"落在哪个元素上"，只给物理像素坐标，
   * 所以自己拿面板的矩形判断 —— 隐藏的标签页矩形宽高是 0，天然不会误接。
   */
  useEffect(() => {
    let stop: (() => void) | null = null;
    let disposed = false;

    const inside = (position: { x: number; y: number }) => {
      const rect = panelRef.current?.getBoundingClientRect();
      if (!rect || rect.width === 0 || rect.height === 0) return false;
      const ratio = window.devicePixelRatio || 1;
      const x = position.x / ratio;
      const y = position.y / ratio;
      return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    };

    void getCurrentWebview()
      .onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === "leave") {
          setDragOver(false);
          return;
        }
        if (payload.type === "enter" || payload.type === "over") {
          setDragOver(inside(payload.position));
          return;
        }

        setDragOver(false);
        if (!inside(payload.position)) return;
        dropRef.current(payload.paths);
      })
      .then((fn) => {
        if (disposed) fn();
        else stop = fn;
      })
      .catch(() => undefined);

    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  const crumbs = useMemo(() => crumbsOf(path), [path]);
  const activeTransfers = transfers.filter((item) => item.state === "running");

  /**
   * 右键菜单定位。
   *
   * 菜单宽高是固定估值，贴边时向内收 —— 否则在最右/最下的行上右键，
   * 菜单会跑到窗口外面，用户只能看到半个。
   */
  const openMenu = (event: React.MouseEvent, entry: RemoteEntry | null) => {
    event.preventDefault();
    event.stopPropagation();

    const width = 176;
    const height = entry ? 130 : 116;
    setMenu({
      x: Math.max(8, Math.min(event.clientX, window.innerWidth - width - 8)),
      y: Math.max(8, Math.min(event.clientY, window.innerHeight - height - 8)),
      entry,
    });
  };

  const runFromMenu = (task: () => void) => {
    setMenu(null);
    task();
  };

  if (!sessionId) {
    return (
      <div className="file-panel__status">
        {busy ? <p className="files-hint">正在连接…</p> : null}
        <NoticeLine notice={notice} />
        {!busy ? (
          <button
            type="button"
            className="btn btn--sm"
            onClick={() => void openSession(connectionId)}
          >
            重新连接
          </button>
        ) : null}
      </div>
    );
  }

  return (
    <div
      className={`file-panel${compact ? " is-compact" : ""}${dragOver ? " is-drop" : ""}`}
      ref={panelRef}
    >
      <div className="files-bar">
        {/* 导航三件套装一组：它们作用于「当前目录」，和右边创建/上传不是一类操作 */}
        <div className="files-bar__group">
          <button
            type="button"
            className="btn btn--ghost btn--icon btn--sm"
            title="上一级"
            aria-label="上一级"
            onClick={goUp}
            disabled={path === "/"}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M8 12.6V3.6" />
              <path d="m4.4 7.2 3.6-3.6 3.6 3.6" />
            </svg>
          </button>
          <button
            type="button"
            className="btn btn--ghost btn--icon btn--sm"
            title="刷新"
            aria-label="刷新"
            onClick={refresh}
            disabled={busy}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M13 8a5 5 0 1 1-1.6-3.7" />
              <path d="M13 2.6V5h-2.4" />
            </svg>
          </button>
          <button
            type="button"
            className="btn btn--ghost btn--icon btn--sm"
            title={home ? `回家目录 ${home}` : "回家目录"}
            aria-label="回家目录"
            disabled={!home}
            onClick={() => home && void load(sessionId, home)}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M2.5 7.6 8 3l5.5 4.6" />
              <path d="M4.1 6.6v6.5h7.8V6.6" />
            </svg>
          </button>
        </div>

        <span className="files-bar__divider" />

        <button
          type="button"
          className="btn btn--ghost btn--sm"
          onClick={() => {
            setAction({ kind: "mkdir" });
            setDraft("");
          }}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M2 4.3a1 1 0 0 1 1-1h3.1l1.4 1.6H13a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" />
            <path d="M8 7.6v3.6M6.2 9.4h3.6" />
          </svg>
          新建文件夹
        </button>

        {/* 上传是这个面板唯一的「主动作」，给它实心底色 */}
        <button type="button" className="btn btn--primary btn--sm" onClick={() => void upload()}>
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M8 10.6V2.9" />
            <path d="m5 5.9 3-3 3 3" />
            <path d="M2.8 11.2v1.5a.9.9 0 0 0 .9.9h8.6a.9.9 0 0 0 .9-.9v-1.5" />
          </svg>
          上传
        </button>

        <button
          type="button"
          className="btn btn--ghost btn--sm"
          title="上传整个文件夹（打包传输）"
          onClick={() => void upload(undefined, true)}
        >
          文件夹
        </button>
        <button
          type="button"
          className="btn btn--ghost btn--sm"
          title="把当前目录打包下载到本机（自动解包）"
          onClick={() => void packDownload([path])}
        >
          打包下载
        </button>

        <span className="files-bar__spacer" />
        {compact ? null : (
          <span className="files-bar__home" title={`家目录 ${home}`}>
            {home}
          </span>
        )}
        <button type="button" className="btn btn--ghost btn--sm" onClick={closeSession}>
          断开
        </button>
      </div>

      <nav className="files-crumbs">
        {crumbs.map((crumb, index) => (
          <span key={crumb.path} className="files-crumbs__item">
            {index > 0 ? <span className="files-crumbs__sep">/</span> : null}
            <button
              type="button"
              className="files-crumbs__link"
              onClick={() => void load(sessionId, crumb.path)}
            >
              {crumb.label}
            </button>
          </span>
        ))}
      </nav>

      <NoticeLine notice={notice} />

      {action?.kind === "mkdir" ? (
        <div className="files-inline">
          <input
            className="input"
            autoFocus
            placeholder="新文件夹名称"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void createFolder();
              if (event.key === "Escape") setAction(null);
            }}
          />
          <button type="button" className="btn btn--primary btn--sm" onClick={createFolder}>
            创建
          </button>
          <button type="button" className="btn btn--sm" onClick={() => setAction(null)}>
            取消
          </button>
        </div>
      ) : null}

      <div className="files-table" onContextMenu={(event) => openMenu(event, null)}>
        <div className="files-row files-row--head">
          <span>名称</span>
          <span className="files-cell--num">大小</span>
          <span>修改时间</span>
          <span>权限</span>
          <span />
        </div>

        {entries.length === 0 && !busy ? (
          <p className="files-hint files-hint--pad">
            这个目录是空的。可以把本地文件直接拖进这个面板上传。
          </p>
        ) : null}

        {entries.map((entry) => {
          const renaming = action?.kind === "rename" && action.entry.path === entry.path;
          const deleting = action?.kind === "delete" && action.entry.path === entry.path;

          return (
            <div
              key={entry.path}
              className="files-row"
              onContextMenu={(event) => openMenu(event, entry)}
            >
              {renaming ? (
                <input
                  className="input"
                  autoFocus
                  value={draft}
                  onChange={(event) => setDraft(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") void renameEntry(entry);
                    if (event.key === "Escape") setAction(null);
                  }}
                />
              ) : (
                <button
                  type="button"
                  className={`files-name ${entry.isDir ? "is-dir" : ""}`}
                  onClick={() => {
                    if (entry.isDir) void load(sessionId, entry.path);
                  }}
                  title={entry.isDir ? "双击/点击进入目录" : entry.path}
                >
                  <span className="files-name__icon">
                    <EntryGlyph isDir={entry.isDir} isSymlink={entry.isSymlink} />
                  </span>
                  <span className="files-name__text">{entry.name}</span>
                </button>
              )}

              <span className="files-cell--num">
                {entry.isDir ? "—" : formatSize(entry.size)}
              </span>
              <span>{formatTime(entry.modifiedMs)}</span>
              <span className="files-mode">
                {formatMode(entry.mode)}
              </span>

              <span className="files-actions">
                {deleting ? (
                  <>
                    <span className="files-danger">确认删除？</span>
                    <button
                      type="button"
                      className="btn btn--sm btn--danger"
                      disabled={busy}
                      onClick={() => void removeEntry(entry)}
                    >
                      删除
                    </button>
                    <button
                      type="button"
                      className="btn btn--sm"
                      onClick={() => setAction(null)}
                    >
                      取消
                    </button>
                  </>
                ) : renaming ? (
                  <button
                    type="button"
                    className="btn btn--primary btn--sm"
                    onClick={() => void renameEntry(entry)}
                  >
                    保存
                  </button>
                ) : (
                  <>
                    {/* 行内操作改成图标：三个中文词常亮要吃掉 130px，而这一列
                        在 560px 宽的面板里就是压垮文件名的那根稻草。
                        图标有 title，和资源管理器/FileZilla 的习惯一致。 */}
                    {!entry.isDir ? (
                      <button
                        type="button"
                        className="btn btn--ghost btn--icon btn--sm files-act"
                        title="下载到本机"
                        aria-label="下载"
                        onClick={() => void download(entry)}
                      >
                        <svg viewBox="0 0 16 16" aria-hidden="true">
                          <path d="M8 2.9v7.7" />
                          <path d="m5 7.9 3 3 3-3" />
                          <path d="M2.8 11.6v1.3a.9.9 0 0 0 .9.9h8.6a.9.9 0 0 0 .9-.9v-1.3" />
                        </svg>
                      </button>
                    ) : null}
                    <button
                      type="button"
                      className="btn btn--ghost btn--icon btn--sm files-act"
                      title="重命名"
                      aria-label="重命名"
                      onClick={() => {
                        setAction({ kind: "rename", entry });
                        setDraft(entry.name);
                      }}
                    >
                      <svg viewBox="0 0 16 16" aria-hidden="true">
                        <path d="M11.1 2.9a1.4 1.4 0 0 1 2 2l-7 7-2.7.7.7-2.7z" />
                        <path d="M9.9 4.1 11.9 6.1" />
                      </svg>
                    </button>
                    <button
                      type="button"
                      className="btn btn--ghost btn--icon btn--sm files-act files-act--danger"
                      title="删除"
                      aria-label="删除"
                      onClick={() => setAction({ kind: "delete", entry })}
                    >
                      <svg viewBox="0 0 16 16" aria-hidden="true">
                        <path d="M3 4.6h10" />
                        <path d="M6.4 4.6V3.2h3.2v1.4" />
                        <path d="M4.4 4.6 5 12.3a1 1 0 0 0 1 .9h4a1 1 0 0 0 1-.9l.6-7.7" />
                      </svg>
                    </button>
                  </>
                )}
              </span>
            </div>
          );
        })}
      </div>

      {transfers.length > 0 ? (
        <div className="files-transfers">
          <div className="files-transfers__head">
            <span>传输</span>
            {activeTransfers.length === 0 ? (
              <button
                type="button"
                className="files-link"
                onClick={() => setTransfers([])}
              >
                清除记录
              </button>
            ) : null}
          </div>
          <ul className="files-transfers__list">
            {transfers.map((item) => {
              const percent =
                item.total > 0
                  ? Math.min(100, Math.round((item.transferred / item.total) * 100))
                  : 0;
              return (
                <li key={item.transferId} className="files-transfer">
                  <span className={`files-transfer__tag is-${item.state}`}>
                    {item.direction === "upload" ? "↑" : "↓"}
                  </span>
                  <span className="files-transfer__name" title={item.name}>
                    {item.name}
                  </span>
                  <span className="files-transfer__bar">
                    <span
                      className={`files-transfer__fill is-${item.state}`}
                      style={{ width: `${percent}%` }}
                    />
                  </span>
                  <span className="files-transfer__meta">
                    {item.state === "running"
                      ? `${formatSize(item.transferred)}${
                          item.total > 0 ? ` / ${formatSize(item.total)}` : ""
                        } · ${formatSpeed(item.bps)}`
                      : item.state === "done"
                        ? "完成"
                        : item.state === "canceled"
                          ? "已取消"
                          : (item.error ?? "失败")}
                  </span>
                  {item.state === "running" ? (
                    <button
                      type="button"
                      className="files-link"
                      onClick={() => void cancelTransfer(item.transferId)}
                    >
                      取消
                    </button>
                  ) : null}
                </li>
              );
            })}
          </ul>
        </div>
      ) : null}

      {dragOver ? (
        <div className="files-drop">
          <span className="files-drop__text">松手即上传到 {path}</span>
        </div>
      ) : null}

      {menu ? (
        <>
          {/* 全屏遮罩负责"点别处关闭"。比在 window 上挂监听可靠：
              lib/desktop.ts 在捕获阶段对 contextmenu 做了 preventDefault，
              靠 defaultPrevented 判断会失效。 */}
          <div
            className="ctx-backdrop"
            onClick={() => setMenu(null)}
            onContextMenu={(event) => {
              event.preventDefault();
              setMenu(null);
            }}
          />

          <div
            className="ctx-menu"
            style={{ left: menu.x, top: menu.y }}
            onContextMenu={(event) => event.preventDefault()}
          >
            {menu.entry ? renderEntryMenu(menu.entry) : null}

            <button
              type="button"
              className="ctx-menu__item"
              onClick={() => runFromMenu(() => void refresh())}
            >
              刷新
            </button>
            <button
              type="button"
              className="ctx-menu__item"
              onClick={() =>
                runFromMenu(() => {
                  setAction({ kind: "mkdir" });
                  setDraft("");
                })
              }
            >
              新建文件夹…
            </button>
            <button
              type="button"
              className="ctx-menu__item"
              onClick={() => runFromMenu(() => void upload())}
            >
              上传…
            </button>
          </div>
        </>
      ) : null}
    </div>
  );

  /** 行上的右键项：只有文件/目录各自可用。 */
  function renderEntryMenu(entry: RemoteEntry) {
    return (
      <>
        {entry.isDir ? (
          <>
            <button
              type="button"
              className="ctx-menu__item"
              onClick={() => runFromMenu(() => void load(sessionId!, entry.path))}
            >
              打开
            </button>
            <button
              type="button"
              className="ctx-menu__item"
              onClick={() => runFromMenu(() => void packDownload([entry.path]))}
            >
              打包下载…
            </button>
          </>
        ) : (
          <button
            type="button"
            className="ctx-menu__item"
            onClick={() => runFromMenu(() => void download(entry))}
          >
            下载…
          </button>
        )}

        <button
          type="button"
          className="ctx-menu__item"
          onClick={() =>
            runFromMenu(() => {
              setAction({ kind: "rename", entry });
              setDraft(entry.name);
            })
          }
        >
          重命名…
        </button>

        <button
          type="button"
          className="ctx-menu__item ctx-menu__item--danger"
          onClick={() => runFromMenu(() => setAction({ kind: "delete", entry }))}
        >
          删除…
        </button>

        <div className="ctx-menu__sep" />
      </>
    );
  }
}

/**
 * 条目类型图标。
 *
 * 原来这里是 📁 / 📄 / 🔗 三个 emoji：不同系统字体画出来的形状、大小、
 * 颜色都不一样，一列 emoji 排下来比正文还抢眼，而且暗色主题下有一半是
 * 彩色实心块。换成 16px 线性描边，颜色交给 CSS（目录用主色，其余灰阶）。
 */
function EntryGlyph({ isDir, isSymlink }: { isDir: boolean; isSymlink: boolean }) {
  // 目录优先于链接：能点进去的东西，形状应该先告诉人「这里能进」
  const shape = isDir ? (
    <path d="M2 4.4a1 1 0 0 1 1-1h3.1l1.4 1.6H13a1 1 0 0 1 1 1v6.4a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" />
  ) : isSymlink ? (
    <>
      <path d="M6.7 9.3a2.5 2.5 0 0 1 0-3.6l2-1.9a2.5 2.5 0 0 1 3.5 3.5l-.8.8" />
      <path d="M9.3 6.7a2.5 2.5 0 0 1 0 3.6l-2 1.9a2.5 2.5 0 0 1-3.5-3.5l.8-.8" />
    </>
  ) : (
    <>
      <path d="M3.4 2.4h5.1l3.5 3.5v7.1a.7.7 0 0 1-.7.7H3.4a.7.7 0 0 1-.7-.7V3.1a.7.7 0 0 1 .7-.7z" />
      <path d="M8.4 2.6v3.4h3.5" />
    </>
  );

  return (
    <svg className="files-name__svg" viewBox="0 0 16 16" aria-hidden="true">
      {shape}
    </svg>
  );
}
