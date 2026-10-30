import { open, save } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { fromUnknown } from "../lib/errors";
import { crumbsOf, formatMode, formatSize, formatSpeed, formatTime } from "../lib/files";
import { call } from "../lib/ipc";
import type { RemoteEntry, SftpOpened, TransferProgress } from "../lib/types";

/** 行内操作：同一时刻只允许一个，避免多个输入框同时出现。 */
type RowAction =
  | { kind: "mkdir" }
  | { kind: "rename"; entry: RemoteEntry }
  | { kind: "delete"; entry: RemoteEntry }
  | null;

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
}

export default function FilePanel({ connectionId, compact = false }: FilePanelProps) {
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [home, setHome] = useState("");
  const [path, setPath] = useState("/");
  const [entries, setEntries] = useState<RemoteEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [action, setAction] = useState<RowAction>(null);
  const [transfers, setTransfers] = useState<TransferProgress[]>([]);
  /** 右键菜单。`entry` 为 null 表示点在空白处（只给目录级操作）。 */
  const [menu, setMenu] = useState<{ x: number; y: number; entry: RemoteEntry | null } | null>(null);

  const sessionRef = useRef<string | null>(null);

  useEffect(() => {
    sessionRef.current = sessionId;
  }, [sessionId]);

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
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  const load = useCallback(async (sid: string, target: string) => {
    const rows = await call<RemoteEntry[]>("sftp_list", { sessionId: sid, path: target });
    setEntries(rows);
    setPath(target);
  }, []);

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
      await load(opened.sessionId, opened.home);
    } catch (err) {
      const error = fromUnknown(err);
      // 首次连一台机器必须先在「连接」页确认指纹，这里给一句能照做的提示
      const hint =
        error.code === "SSH_HOST_KEY_UNKNOWN"
          ? "这台服务器还没确认过主机指纹。请先去「连接」页点「测试连接」，确认指纹后再回来。"
          : error.message;
      setNotice(hint);
    } finally {
      setBusy(false);
    }
  };

  const closeSession = async () => {
    const sid = sessionRef.current;
    setSessionId(null);
    setEntries([]);
    setPath("/");
    // 留一句提示：不然面板变成一个没有出口的空屏，用户不知道还能重连
    setNotice("已断开 SFTP 连接。");
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

  // 面板卸载（关标签 / 关面板）时关掉自己的 SFTP 会话，避免连接泄漏
  useEffect(
    () => () => {
      const sid = sessionRef.current;
      if (sid) void call<void>("sftp_close", { sessionId: sid }).catch(() => undefined);
    },
    [],
  );

  const refresh = async () => {
    if (!sessionId) return;
    try {
      await load(sessionId, path);
      setNotice(null);
    } catch (err) {
      setNotice(fromUnknown(err).message);
    }
  };

  const goUp = async () => {
    if (!sessionId) return;
    try {
      const parent = await call<string | null>("sftp_parent_of", { path });
      if (parent) await load(sessionId, parent);
    } catch (err) {
      setNotice(fromUnknown(err).message);
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
      setNotice(fromUnknown(err).message);
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
      if (!name) throw new Error("请输入新名称");
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

  const upload = async () => {
    if (!sessionId) return;
    try {
      const picked = await open({
        multiple: true,
        directory: false,
        title: "选择要上传的文件",
      });
      const files = Array.isArray(picked) ? picked : picked ? [picked] : [];
      if (files.length === 0) return;

      for (const file of files) {
        await call<string>("sftp_upload", {
          sessionId,
          localPath: file,
          remoteDir: path,
          name: null,
        });
      }
      setNotice(`已加入 ${files.length} 个上传任务`);
    } catch (err) {
      setNotice(fromUnknown(err).message);
    }
  };

  const download = async (entry: RemoteEntry) => {
    if (!sessionId) return;
    try {
      const target = await save({ defaultPath: entry.name, title: "保存到" });
      if (!target) return;
      await call<string>("sftp_download", {
        sessionId,
        remotePath: entry.path,
        localPath: target,
      });
      setNotice(`已开始下载 ${entry.name}`);
    } catch (err) {
      setNotice(fromUnknown(err).message);
    }
  };

  const cancelTransfer = async (transferId: string) => {
    await call<boolean>("sftp_cancel", { transferId }).catch(() => false);
  };

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
        {notice ? <p className="files-error">{notice}</p> : null}
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
    <div className={`file-panel${compact ? " is-compact" : ""}`}>
      <div className="files-bar">
        <button type="button" className="btn btn--sm" onClick={goUp} disabled={path === "/"}>
          ↑ 上一级
        </button>
        <button type="button" className="btn btn--sm" onClick={refresh} disabled={busy}>
          刷新
        </button>
        <button
          type="button"
          className="btn btn--sm"
          onClick={() => {
            setAction({ kind: "mkdir" });
            setDraft("");
          }}
        >
          新建文件夹
        </button>
        <button type="button" className="btn btn--sm" onClick={upload}>
          上传…
        </button>
        <span className="files-bar__spacer" />
        {compact ? null : (
          <span className="files-bar__home" title={`家目录 ${home}`}>
            {home}
          </span>
        )}
        <button type="button" className="btn btn--sm" onClick={closeSession}>
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

      {notice ? <p className="files-error">{notice}</p> : null}

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
          <p className="files-hint files-hint--pad">这个目录是空的。</p>
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
                    {entry.isDir ? "📁" : entry.isSymlink ? "🔗" : "📄"}
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
                    className="btn btn--sm btn--primary"
                    onClick={() => void renameEntry(entry)}
                  >
                    保存
                  </button>
                ) : (
                  <>
                    {!entry.isDir ? (
                      <button
                        type="button"
                        className="files-link"
                        onClick={() => void download(entry)}
                      >
                        下载
                      </button>
                    ) : null}
                    <button
                      type="button"
                      className="files-link"
                      onClick={() => {
                        setAction({ kind: "rename", entry });
                        setDraft(entry.name);
                      }}
                    >
                      重命名
                    </button>
                    <button
                      type="button"
                      className="files-link files-link--danger"
                      onClick={() => setAction({ kind: "delete", entry })}
                    >
                      删除
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
          <button
            type="button"
            className="ctx-menu__item"
            onClick={() => runFromMenu(() => void load(sessionId!, entry.path))}
          >
            打开
          </button>
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
