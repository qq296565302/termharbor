import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import type { DiagKind, EnvStatus, SnippetDto, SnippetRisk } from "../lib/types";

interface Props {
  /** 拿到渲染好的命令文本。插入由父组件负责 —— 它才知道当前活跃会话是谁。 */
  onInsert: (command: string, snippet: SnippetDto) => void;
  /** 有诊断类型的片段可以用表格查看，父组件负责取数 */
  onView: (kind: DiagKind, title: string) => void;
  onClose: () => void;
  /** 当前活跃终端会话。有它才能探测服务器上装了哪些运行时。 */
  sessionId: string | null;
}

/** 新建 / 编辑表单的草稿。 */
interface Draft {
  id: string | null;
  name: string;
  group: string;
  command: string;
  description: string;
}

const RISK_LABEL: Record<SnippetRisk, string> = {
  readonly: "只读",
  dangerous: "会改动系统",
  critical: "高危",
};

const EMPTY_DRAFT: Draft = {
  id: null,
  name: "",
  group: "",
  command: "",
  description: "",
};

/**
 * 命令片段侧栏（需求 5.7.5）。
 *
 * 两条硬约束：
 * 1. **点击只插入，不执行**（A-12）：这里永远只调 `session_write` 写文本，绝不追加 `\r`。
 * 2. **命令不允许换行**：终端把换行当回车，多行粘贴会逐行执行，
 *    等于点一下就把部署跑了一半。多步要用 `&&` 连接（校验在 Rust 侧）。
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
  /** 正在编辑的草稿；null 表示没打开表单。 */
  const [draft, setDraft] = useState<Draft | null>(null);
  const [draftRisk, setDraftRisk] = useState<SnippetRisk>("readonly");
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  /** 待确认删除的片段 id */
  const [deleting, setDeleting] = useState<string | null>(null);

  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  const reload = useCallback(async (keyword: string) => {
    try {
      // 过滤在 Rust 侧做：名称/命令/描述/分组四类字段都在那里
      const rows = await call<SnippetDto[]>("snippet_list", {
        query: keyword.trim() || null,
      });
      setItems(rows);
      setError(null);
    } catch (err) {
      setError(fromUnknown(err).message);
    }
  }, []);

  useEffect(() => {
    void reload(query);
  }, [query, reload]);

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

  // 输入命令时实时显示风险等级。判定走后端，前端不得自己算（SEC-SN-10）
  useEffect(() => {
    if (!draft) return;

    const command = draft.command;
    if (!command.trim()) {
      setDraftRisk("readonly");
      return;
    }

    const timer = window.setTimeout(() => {
      call<SnippetRisk>("snippet_classify", { command })
        .then(setDraftRisk)
        .catch(() => undefined);
    }, 250);

    return () => window.clearTimeout(timer);
  }, [draft]);

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

  const openCreate = () => {
    setFormError(null);
    setDeleting(null);
    setDraft({ ...EMPTY_DRAFT });
  };

  const openEdit = (item: SnippetDto) => {
    setFormError(null);
    setDeleting(null);
    setDraft({
      id: item.id,
      name: item.name,
      group: item.group,
      command: item.command,
      description: item.description,
    });
  };

  const save = async () => {
    if (!draft || saving) return;

    setSaving(true);
    setFormError(null);
    try {
      await call<SnippetDto>("snippet_save", {
        input: {
          id: draft.id,
          name: draft.name,
          command: draft.command,
          description: draft.description,
          group: draft.group,
        },
      });
      setDraft(null);
      await reload(query);
    } catch (err) {
      setFormError(fromUnknown(err).message);
    } finally {
      setSaving(false);
    }
  };

  const remove = async (id: string) => {
    try {
      await call<boolean>("snippet_delete", { id });
      setDeleting(null);
      await reload(query);
    } catch (err) {
      setError(fromUnknown(err).message);
    }
  };

  return (
    <aside className="snip">
      <div className="snip__head">
        <span className="snip__title">命令片段</span>
        <span className="snip__head-spacer" />
        <button
          type="button"
          className="btn btn--ghost btn--sm snip__new"
          onClick={openCreate}
          title="新建片段（部署命令可以存成一条）"
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M8 3.4v9.2M3.4 8h9.2" />
          </svg>
          新建
        </button>
        <button
          type="button"
          className="btn btn--ghost btn--icon btn--sm snip__close"
          onClick={onClose}
          title="关闭（Ctrl+Shift+P）"
          aria-label="关闭"
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M4.2 4.2l7.6 7.6M11.8 4.2l-7.6 7.6" />
          </svg>
        </button>
      </div>

      {/* 搜索框自带放大镜：侧栏里只有一个输入框，光靠 placeholder 说「搜索」，
          滚出视口后就看不出这是筛选器还是命令输入行 */}
      <div className="snip__searchbox">
        <svg className="snip__searchicon" viewBox="0 0 16 16" aria-hidden="true">
          <circle cx="7.1" cy="7.1" r="4.1" />
          <path d="m10.2 10.2 3 3" />
        </svg>
        <input
          ref={searchRef}
          className="input snip__search"
          placeholder="搜索命令 / 名称 / 分组"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              if (draft) setDraft(null);
              else onClose();
            }
          }}
        />
      </div>

      {draft ? (
        <div className="snip__form">
          <input
            className="input snip__form-input"
            placeholder="名称，如「部署 web」"
            value={draft.name}
            onChange={(event) => setDraft({ ...draft, name: event.target.value })}
            autoFocus
          />
          <input
            className="input snip__form-input"
            placeholder="分组，如「web 项目」（留空为「我的片段」）"
            value={draft.group}
            onChange={(event) => setDraft({ ...draft, group: event.target.value })}
          />
          <textarea
            className="input snip__form-input snip__form-cmd"
            placeholder="cd /opt/web && git pull && npm ci && npm run build"
            rows={3}
            value={draft.command}
            onChange={(event) => setDraft({ ...draft, command: event.target.value })}
          />
          <input
            className="input snip__form-input"
            placeholder="说明（可选）"
            value={draft.description}
            onChange={(event) => setDraft({ ...draft, description: event.target.value })}
          />

          <p className="snip__form-hint">
            多步命令用 <code>&&</code> 连接成一行。不能换行 —— 终端会把换行当回车逐行执行。
          </p>

          {formError ? <p className="snip__error">{formError}</p> : null}

          <div className="snip__form-foot">
            <span className={`snip__form-risk is-${draftRisk}`}>
              {RISK_LABEL[draftRisk]}
            </span>
            <span className="snip__head-spacer" />
            <button
              type="button"
              className="btn btn--sm"
              onClick={() => setDraft(null)}
              disabled={saving}
            >
              取消
            </button>
            <button
              type="button"
              className="btn btn--sm btn--primary"
              onClick={() => void save()}
              disabled={saving}
            >
              {saving ? "保存中…" : "保存"}
            </button>
          </div>
        </div>
      ) : null}

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
                <span className={`snip__caret${isCollapsed ? " is-collapsed" : ""}`}>
                  <svg viewBox="0 0 16 16" aria-hidden="true">
                    <path d="m6.2 4.2 4 3.8-4 3.8" />
                  </svg>
                </span>
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
                      <li
                        key={item.id}
                        className={`snip__item${deleting === item.id ? " is-deleting" : ""}`}
                      >
                        <button
                          type="button"
                          className={`snip__insert is-${item.risk}`}
                          title={item.description || item.command}
                          onClick={() => void insert(item)}
                        >
                          <span className="snip__name">
                            {item.name}
                            {/* 只读片段不挂标签：一列全是「只读」等于没有标签 */}
                            {item.risk !== "readonly" ? (
                              <span className={`snip__risk is-${item.risk}`}>
                                {item.risk === "critical" ? "高危" : "改动"}
                              </span>
                            ) : null}
                          </span>
                          <code className="snip__cmd">{item.command}</code>
                        </button>

                        <span className="snip__rowacts">
                          {item.diag ? (
                            <button
                              type="button"
                              className="snip__icon-btn"
                              title="用表格查看（不占用终端）"
                              aria-label="用表格查看"
                              onClick={() => onView(item.diag as DiagKind, item.name)}
                            >
                              <svg viewBox="0 0 16 16" aria-hidden="true">
                                <rect x="2" y="3" width="12" height="10" rx="1.6" />
                                <path d="M2 6.6h12M6.4 6.6V13" />
                              </svg>
                            </button>
                          ) : null}

                          {!item.builtin ? (
                            deleting === item.id ? (
                              <>
                                <button
                                  type="button"
                                  className="snip__icon-btn is-danger is-wide"
                                  onClick={() => void remove(item.id)}
                                >
                                  删除
                                </button>
                                <button
                                  type="button"
                                  className="snip__icon-btn is-wide"
                                  onClick={() => setDeleting(null)}
                                >
                                  取消
                                </button>
                              </>
                            ) : (
                              <>
                                <button
                                  type="button"
                                  className="snip__icon-btn"
                                  title="编辑"
                                  aria-label="编辑"
                                  onClick={() => openEdit(item)}
                                >
                                  <svg viewBox="0 0 16 16" aria-hidden="true">
                                    <path d="M11.1 2.9a1.4 1.4 0 0 1 2 2l-7 7-2.7.7.7-2.7z" />
                                    <path d="M9.9 4.1 11.9 6.1" />
                                  </svg>
                                </button>
                                <button
                                  type="button"
                                  className="snip__icon-btn"
                                  title="删除"
                                  aria-label="删除"
                                  onClick={() => setDeleting(item.id)}
                                >
                                  <svg viewBox="0 0 16 16" aria-hidden="true">
                                    <path d="M3 4.6h10" />
                                    <path d="M6.4 4.6V3.2h3.2v1.4" />
                                    <path d="M4.4 4.6 5 12.3a1 1 0 0 0 1 .9h4a1 1 0 0 0 1-.9l.6-7.7" />
                                  </svg>
                                </button>
                              </>
                            )
                          ) : null}
                        </span>
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
