import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import type { DiagKind, EnvStatus, SnippetDto } from "../lib/types";

interface Props {
  /** 拿到渲染好的命令文本。插入由父组件负责 —— 它才知道当前活跃会话是谁。 */
  onInsert: (command: string, snippet: SnippetDto) => void;
  /** 有诊断类型的片段可以用表格查看，父组件负责取数 */
  onView: (kind: DiagKind, title: string) => void;
  onClose: () => void;
  /** 当前活跃终端会话。有它才能探测服务器上装了哪些运行时。 */
  sessionId: string | null;
}

/**
 * 命令片段侧栏（需求 5.7.5）。
 *
 * 一条硬约束：**点击只插入，不执行**（A-12）。
 * 所以这里永远只调 `session_write` 写文本，绝不追加 `\r`。
 */
export default function SnippetPanel({ onInsert, onView, onClose, sessionId }: Props) {
  const [query, setQuery] = useState("");
  const [items, setItems] = useState<SnippetDto[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const searchRef = useRef<HTMLInputElement>(null);
  /** 服务器上已经装了哪些运行时。null 表示还没探测（或无会话）。 */
  const [env, setEnv] = useState<EnvStatus[] | null>(null);
  const [envError, setEnvError] = useState<string | null>(null);

  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  useEffect(() => {
    let cancelled = false;
    // 过滤在 Rust 侧做：名称/命令/描述/分组四类字段都在那里
    call<SnippetDto[]>("snippet_list", { query: query.trim() || null })
      .then((rows) => {
        if (!cancelled) {
          setItems(rows);
          setError(null);
        }
      })
      .catch((err) => {
        if (!cancelled) setError(fromUnknown(err).message);
      });
    return () => {
      cancelled = true;
    };
  }, [query]);

  const probeEnv = useCallback(async () => {
    if (!sessionId) {
      setEnv(null);
      return;
    }
    try {
      const rows = await call<EnvStatus[]>("env_probe", { sessionId });
      setEnv(rows);
      setEnvError(null);
    } catch (err) {
      setEnv(null);
      setEnvError(fromUnknown(err).message);
    }
  }, [sessionId]);

  // 打开面板就探一次：用户点「安装」之前该知道已经装了什么
  useEffect(() => {
    void probeEnv();
  }, [probeEnv]);

  const groups = useMemo(() => {
    const bucketed = new Map<string, SnippetDto[]>();
    for (const item of items) {
      const rows = bucketed.get(item.group) ?? [];
      rows.push(item);
      bucketed.set(item.group, rows);
    }
    return [...bucketed.entries()];
  }, [items]);

  const insert = async (snippet: SnippetDto) => {
    try {
      // 渲染与校验留在 Rust（A-11）：前端不拼命令字符串
      const command = await call<string>("snippet_render", { id: snippet.id });
      onInsert(command, snippet);
    } catch (err) {
      setError(fromUnknown(err).message);
    }
  };

  return (
    <aside className="snip">
      <div className="snip__head">
        <span>命令片段</span>
        <button
          type="button"
          className="snip__close"
          onClick={onClose}
          title="关闭（Ctrl+Shift+P）"
        >
          ×
        </button>
      </div>

      <input
        ref={searchRef}
        className="input snip__search"
        placeholder="搜索命令 / 名称 / 分组"
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") onClose();
        }}
      />

      {error ? <p className="snip__error">{error}</p> : null}

      <div className="snip__list">
        {groups.length === 0 && !error ? (
          <p className="snip__empty">没有匹配的片段</p>
        ) : null}

        {groups.map(([group, rows]) => {
          const isCollapsed = collapsed[group] ?? false;
          return (
            <section key={group} className="snip__group">
              <button
                type="button"
                className="snip__group-head"
                onClick={() =>
                  setCollapsed((prev) => ({ ...prev, [group]: !isCollapsed }))
                }
              >
                <span className="snip__caret">{isCollapsed ? "▸" : "▾"}</span>
                <span className="snip__group-name">{group}</span>
                <span className="snip__count">{rows.length}</span>
              </button>

              {isCollapsed ? null : (
                <>
                  {group === "环境准备" ? (
                    <div className="snip__env">
                      {!sessionId ? (
                        <span className="snip__env-hint">打开终端后可检测已装环境</span>
                      ) : envError ? (
                        <span className="snip__env-hint">{envError}</span>
                      ) : !env ? (
                        <span className="snip__env-hint">正在检测服务器上已装的环境…</span>
                      ) : (
                        <>
                          {env.map((item) => (
                            <span
                              key={item.name}
                              className={`snip__env-chip ${item.installed ? "is-on" : "is-off"}`}
                              title={item.installed ? "已安装" : "未安装"}
                            >
                              <span className="snip__env-dot" />
                              <span className="snip__env-name">{item.name}</span>
                              {item.version ? (
                                <span className="snip__env-ver">{item.version}</span>
                              ) : null}
                            </span>
                          ))}
                          <button
                            type="button"
                            className="snip__env-refresh"
                            onClick={() => void probeEnv()}
                          >
                            重新检测
                          </button>
                        </>
                      )}
                    </div>
                  ) : null}

                  <ul className="snip__items">
                  {rows.map((item) => (
                    <li key={item.id} className="snip__item">
                      <button
                        type="button"
                        className={`snip__insert is-${item.risk}`}
                        title={item.description}
                        onClick={() => void insert(item)}
                      >
                        <span className="snip__name">
                          {item.name}
                          {item.risk === "critical" ? (
                            <span className="snip__risk is-critical">高危</span>
                          ) : null}
                        </span>
                        <code className="snip__cmd">{item.command}</code>
                      </button>

                      {item.diag ? (
                        <button
                          type="button"
                          className="snip__view"
                          title="用表格查看（不占用终端）"
                          onClick={() => onView(item.diag as DiagKind, item.name)}
                        >
                          表格
                        </button>
                      ) : null}
                    </li>
                    ))}
                  </ul>
                </>
              )}
            </section>
          );
        })}
      </div>

      <p className="snip__foot">点击插入到光标处，不会自动执行</p>
    </aside>
  );
}
